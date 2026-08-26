//! Drive corpus persistence: document snapshots, chunk rows + the FTS5
//! lexical index (maintained together in ONE transaction), and the sync
//! cursor. Content-hash skips happen BEFORE store_core::mutate — a
//! steady-state sync cycle writes zero rows anywhere (no receipts, no
//! timestamp churn), matching the accounting snapshot posture.

use std::collections::HashMap;

use bos_contracts::drive_corpus::{
    DriveCorpusDocCounts, DriveCorpusSettingsUpdateRequest, DriveSearchHit,
};
use bos_contracts::receipt::ActorKindDto;
use bos_integrations::google_drive_read::DriveFileMeta;
use rusqlite::{params, Connection, OptionalExtension, Row};
use sha2::{Digest, Sha256};

use crate::store_core::{self, MutationOutcome, MutationRequest, StoreError};

pub const DOC_ENTITY_KIND: &str = "drive_doc";
pub const CURSOR_ENTITY_KIND: &str = "drive_sync_cursor";
pub const SETTINGS_ENTITY_KIND: &str = "drive_corpus_settings";
pub const SETTINGS_ENTITY_ID: &str = DEFAULT_CORPUS_ID;
pub const SYNC_ACTOR: &str = "drive_sync_pump";
pub const DEFAULT_CORPUS_ID: &str = "default";

pub const STATUS_STALE: &str = "stale";
pub const STATUS_INDEXED: &str = "indexed";
pub const STATUS_SKIPPED: &str = "skipped";
pub const STATUS_ERROR: &str = "error";
pub const STATUS_REMOVED: &str = "removed";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredCorpusSettings {
    pub credential_user_id: Option<String>,
    pub folder_ids: Vec<String>,
    pub folder_names: HashMap<String, String>,
    pub revision: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveSyncCursor {
    pub config_hash: String,
    pub start_page_token: Option<String>,
    /// Mid-walk continuation inside the changes feed.
    pub pending_page_token: Option<String>,
    /// Position in the backfill target list (folders, then include files).
    pub backfill_folder_index: u32,
    pub backfill_page_token: Option<String>,
    pub backfill_complete: bool,
    pub rate_limited_until_ms: u64,
    pub last_error: Option<String>,
    pub last_advanced_at_ms: Option<u64>,
}

impl DriveSyncCursor {
    pub fn initial() -> Self {
        Self {
            config_hash: String::new(),
            start_page_token: None,
            pending_page_token: None,
            backfill_folder_index: 0,
            backfill_page_token: None,
            backfill_complete: false,
            rate_limited_until_ms: 0,
            last_error: None,
            last_advanced_at_ms: None,
        }
    }
}

pub fn get_corpus_settings(
    conn: &Connection,
    client_id: &str,
) -> Result<Option<StoredCorpusSettings>, StoreError> {
    let row = conn
        .query_row(
            "SELECT credential_user_id, folder_ids_json, folder_names_json \
             FROM drive_corpus_settings \
             WHERE client_id = ?1 AND corpus_id = ?2",
            params![client_id, DEFAULT_CORPUS_ID],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((credential_user_id, folder_ids_json, folder_names_json)) = row else {
        return Ok(None);
    };
    let folder_ids = serde_json::from_str(&folder_ids_json).unwrap_or_default();
    let folder_names = serde_json::from_str(&folder_names_json).unwrap_or_default();
    let revision =
        store_core::current_revision(conn, client_id, SETTINGS_ENTITY_KIND, SETTINGS_ENTITY_ID)?;
    Ok(Some(StoredCorpusSettings {
        credential_user_id,
        folder_ids,
        folder_names,
        revision,
    }))
}

pub fn replace_corpus_settings(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    credential_user_id: Option<&str>,
    request: &DriveCorpusSettingsUpdateRequest,
    now_ms: u64,
) -> Result<MutationOutcome, StoreError> {
    let folder_id = normalized_optional(&request.drive_folder_id, 256);
    let folder_name = normalized_optional(&request.drive_folder_name, 500);
    if folder_id.is_none() && folder_name.is_some() {
        return Err(StoreError::Domain(
            "drive_corpus_folder_id_required".to_string(),
        ));
    }
    let folder_ids = folder_id.iter().cloned().collect::<Vec<_>>();
    let mut folder_names = HashMap::new();
    if let (Some(id), Some(name)) = (folder_id.as_ref(), folder_name.as_ref()) {
        folder_names.insert(id.clone(), name.clone());
    }
    let write_credential_user_id = folder_id
        .as_ref()
        .and_then(|_| credential_user_id.map(str::to_string));
    let before_json = get_corpus_settings(conn, client_id)?.and_then(|settings| {
        serde_json::to_string(&serde_json::json!({
            "credential_user_id": settings.credential_user_id,
            "folder_ids": settings.folder_ids,
            "folder_names": settings.folder_names,
        }))
        .ok()
    });
    let after_json = serde_json::to_string(&serde_json::json!({
        "credential_user_id": write_credential_user_id,
        "folder_ids": folder_ids,
        "folder_names": folder_names,
    }))
    .map_err(|err| StoreError::Domain(format!("serialize drive corpus settings: {err}")))?;
    let folder_ids_json = serde_json::to_string(&folder_ids)
        .map_err(|err| StoreError::Domain(format!("serialize corpus folder ids: {err}")))?;
    let folder_names_json = serde_json::to_string(&folder_names)
        .map_err(|err| StoreError::Domain(format!("serialize corpus folder names: {err}")))?;
    let owned_client = client_id.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: SETTINGS_ENTITY_KIND,
            entity_id: SETTINGS_ENTITY_ID,
            change_kind: "replace",
            actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: request.expected_revision,
            idempotency_key: &request.idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json,
            after_json: Some(after_json),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO drive_corpus_settings \
                 (client_id, corpus_id, credential_user_id, folder_ids_json, folder_names_json, updated_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                 ON CONFLICT(client_id, corpus_id) DO UPDATE SET \
                   credential_user_id = excluded.credential_user_id, \
                   folder_ids_json = excluded.folder_ids_json, \
                   folder_names_json = excluded.folder_names_json, \
                   updated_at_ms = excluded.updated_at_ms",
                params![
                    owned_client,
                    DEFAULT_CORPUS_ID,
                    write_credential_user_id,
                    folder_ids_json,
                    folder_names_json,
                    now_ms as i64,
                ],
            )?;
            Ok(())
        },
    )
}

pub fn get_cursor(conn: &Connection, client_id: &str) -> Result<DriveSyncCursor, StoreError> {
    let row = conn
        .query_row(
            "SELECT config_hash, start_page_token, pending_page_token, \
             backfill_folder_index, backfill_page_token, backfill_complete, \
             rate_limited_until_ms, last_error, last_advanced_at_ms \
             FROM drive_sync_cursors WHERE client_id = ?1 AND corpus_id = ?2",
            params![client_id, DEFAULT_CORPUS_ID],
            |row| {
                Ok(DriveSyncCursor {
                    config_hash: row.get(0)?,
                    start_page_token: row.get(1)?,
                    pending_page_token: row.get(2)?,
                    backfill_folder_index: row.get::<_, i64>(3)? as u32,
                    backfill_page_token: row.get(4)?,
                    backfill_complete: row.get(5)?,
                    rate_limited_until_ms: row.get::<_, i64>(6)? as u64,
                    last_error: row.get(7)?,
                    last_advanced_at_ms: row.get::<_, Option<i64>>(8)?.map(|ms| ms as u64),
                })
            },
        )
        .optional()?;
    Ok(row.unwrap_or_else(DriveSyncCursor::initial))
}

fn normalized_optional(value: &Option<String>, max_len: usize) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(max_len).collect())
}

/// Compare-before-write cursor persistence: returns false (and writes
/// nothing, not even a receipt) when the cursor is unchanged.
pub fn put_cursor(
    conn: &mut Connection,
    client_id: &str,
    cursor: &DriveSyncCursor,
    now_ms: u64,
) -> Result<bool, StoreError> {
    let current = get_cursor(conn, client_id)?;
    if current == *cursor {
        return Ok(false);
    }
    let content = snapshot_hash(&[
        &cursor.config_hash,
        cursor.start_page_token.as_deref().unwrap_or(""),
        cursor.pending_page_token.as_deref().unwrap_or(""),
        &cursor.backfill_folder_index.to_string(),
        cursor.backfill_page_token.as_deref().unwrap_or(""),
        &(cursor.backfill_complete as u8).to_string(),
        &cursor.rate_limited_until_ms.to_string(),
        cursor.last_error.as_deref().unwrap_or(""),
    ]);
    let idempotency_key = format!("drive_cursor:{content}");
    let after = serde_json::json!({
        "start_page_token": cursor.start_page_token,
        "pending_page_token": cursor.pending_page_token,
        "backfill_folder_index": cursor.backfill_folder_index,
        "backfill_complete": cursor.backfill_complete,
        "rate_limited_until_ms": cursor.rate_limited_until_ms,
        "last_error": cursor.last_error,
    })
    .to_string();
    let owned_client = client_id.to_string();
    let owned_cursor = cursor.clone();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: CURSOR_ENTITY_KIND,
            entity_id: DEFAULT_CORPUS_ID,
            change_kind: "advance",
            actor_id: SYNC_ACTOR,
            actor_kind: ActorKindDto::System,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO drive_sync_cursors \
                 (client_id, corpus_id, config_hash, start_page_token, pending_page_token, \
                  backfill_folder_index, backfill_page_token, backfill_complete, \
                  rate_limited_until_ms, last_error, last_advanced_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
                 ON CONFLICT (client_id, corpus_id) DO UPDATE SET \
                   config_hash = excluded.config_hash, \
                   start_page_token = excluded.start_page_token, \
                   pending_page_token = excluded.pending_page_token, \
                   backfill_folder_index = excluded.backfill_folder_index, \
                   backfill_page_token = excluded.backfill_page_token, \
                   backfill_complete = excluded.backfill_complete, \
                   rate_limited_until_ms = excluded.rate_limited_until_ms, \
                   last_error = excluded.last_error, \
                   last_advanced_at_ms = excluded.last_advanced_at_ms",
                params![
                    owned_client,
                    DEFAULT_CORPUS_ID,
                    owned_cursor.config_hash,
                    owned_cursor.start_page_token,
                    owned_cursor.pending_page_token,
                    owned_cursor.backfill_folder_index as i64,
                    owned_cursor.backfill_page_token,
                    owned_cursor.backfill_complete,
                    owned_cursor.rate_limited_until_ms as i64,
                    owned_cursor.last_error,
                    now_ms as i64,
                ],
            )?;
            Ok(())
        },
    )?;
    Ok(true)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocSnapshot {
    pub meta: DriveFileMeta,
    pub status: String,
    pub content_hash: String,
    pub chunk_count: u32,
    pub last_error: Option<String>,
    pub first_seen_at_ms: u64,
    pub last_synced_at_ms: u64,
}

fn snapshot_from_row(row: &Row<'_>) -> rusqlite::Result<DocSnapshot> {
    Ok(DocSnapshot {
        meta: DriveFileMeta {
            file_id: row.get(0)?,
            name: row.get(1)?,
            mime_type: row.get(2)?,
            modified_time: row.get(3)?,
            version: row.get(4)?,
            parent_folder_ids: serde_json::from_str(&row.get::<_, String>(5)?).unwrap_or_default(),
            web_view_link: row.get(6)?,
            trashed: false,
        },
        status: row.get(7)?,
        content_hash: row.get(8)?,
        chunk_count: row.get::<_, i64>(9)? as u32,
        last_error: row.get(10)?,
        first_seen_at_ms: row.get::<_, i64>(11)? as u64,
        last_synced_at_ms: row.get::<_, i64>(12)? as u64,
    })
}

const SNAPSHOT_COLUMNS: &str = "file_id, name, mime_type, modified_time, version, \
     parent_folder_ids_json, web_view_link, status, content_hash, chunk_count, last_error, \
     first_seen_at_ms, last_synced_at_ms";

pub fn get_snapshot(
    conn: &Connection,
    client_id: &str,
    file_id: &str,
) -> Result<Option<DocSnapshot>, StoreError> {
    let row = conn
        .query_row(
            &format!(
                "SELECT {SNAPSHOT_COLUMNS} FROM drive_doc_snapshots \
                 WHERE client_id = ?1 AND file_id = ?2"
            ),
            params![client_id, file_id],
            snapshot_from_row,
        )
        .optional()?;
    Ok(row)
}

pub fn list_snapshots(conn: &Connection, client_id: &str) -> Result<Vec<DocSnapshot>, StoreError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {SNAPSHOT_COLUMNS} FROM drive_doc_snapshots WHERE client_id = ?1"
    ))?;
    let rows = stmt.query_map(params![client_id], snapshot_from_row)?;
    let mut snapshots = Vec::new();
    for row in rows {
        snapshots.push(row?);
    }
    Ok(snapshots)
}

/// Oldest-synced first so a budget-starved cycle rotates fairly.
pub fn stale_snapshots(
    conn: &Connection,
    client_id: &str,
    limit: usize,
) -> Result<Vec<DocSnapshot>, StoreError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {SNAPSHOT_COLUMNS} FROM drive_doc_snapshots \
         WHERE client_id = ?1 AND status = '{STATUS_STALE}' \
         ORDER BY last_synced_at_ms ASC, file_id ASC LIMIT ?2"
    ))?;
    let rows = stmt.query_map(params![client_id, limit as i64], snapshot_from_row)?;
    let mut snapshots = Vec::new();
    for row in rows {
        snapshots.push(row?);
    }
    Ok(snapshots)
}

pub fn doc_counts(conn: &Connection, client_id: &str) -> Result<DriveCorpusDocCounts, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT status, COUNT(*) FROM drive_doc_snapshots WHERE client_id = ?1 GROUP BY status",
    )?;
    let rows = stmt.query_map(params![client_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
    })?;
    let mut counts = DriveCorpusDocCounts::default();
    for row in rows {
        let (status, count) = row?;
        match status.as_str() {
            STATUS_INDEXED => counts.indexed = count,
            STATUS_STALE => counts.stale = count,
            STATUS_SKIPPED => counts.skipped = count,
            STATUS_ERROR => counts.error = count,
            STATUS_REMOVED => counts.removed = count,
            _ => {}
        }
    }
    Ok(counts)
}

pub fn chunk_count(conn: &Connection, client_id: &str) -> Result<u64, StoreError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM drive_chunks WHERE client_id = ?1",
        params![client_id],
        |row| row.get(0),
    )?;
    Ok(count as u64)
}

/// Record fresh Drive metadata for a corpus file. Skipped entirely (no write,
/// no receipt) when the stored row already carries this modified_time/version
/// and is not in error — the content-hash-skip's cheaper cousin, applied
/// before the text is ever fetched. New/changed files land as `stale` for the
/// indexing phase. Returns true when a write happened.
pub fn mark_stale_from_meta(
    conn: &mut Connection,
    client_id: &str,
    meta: &DriveFileMeta,
    now_ms: u64,
) -> Result<bool, StoreError> {
    let existing = get_snapshot(conn, client_id, &meta.file_id)?;
    if let Some(existing) = &existing {
        let same_revision = existing.meta.modified_time == meta.modified_time
            && existing.meta.version == meta.version;
        let metadata_only_change = existing.meta.name != meta.name
            || existing.meta.parent_folder_ids != meta.parent_folder_ids
            || existing.meta.web_view_link != meta.web_view_link;
        if same_revision
            && !metadata_only_change
            && matches!(
                existing.status.as_str(),
                STATUS_INDEXED | STATUS_STALE | STATUS_SKIPPED
            )
        {
            return Ok(false);
        }
        // Same content revision but moved/renamed: refresh metadata without
        // re-reading the text (keep indexed status; title changes re-index on
        // the next content revision).
        if same_revision && existing.status == STATUS_INDEXED {
            return update_snapshot_meta(conn, client_id, meta, existing, now_ms).map(|()| true);
        }
    }
    let first_seen = existing
        .as_ref()
        .map(|snapshot| snapshot.first_seen_at_ms)
        .unwrap_or(now_ms);
    let parents_json = serde_json::to_string(&meta.parent_folder_ids)
        .map_err(|err| StoreError::Domain(format!("serialize parents: {err}")))?;
    let idempotency_key = format!(
        "drive_doc_stale:{}:{}:{}",
        meta.file_id,
        meta.modified_time,
        meta.version.as_deref().unwrap_or("")
    );
    let after = serde_json::json!({
        "name": meta.name,
        "mime_type": meta.mime_type,
        "modified_time": meta.modified_time,
        "version": meta.version,
        "status": STATUS_STALE,
    })
    .to_string();
    let owned_client = client_id.to_string();
    let owned_meta = meta.clone();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: DOC_ENTITY_KIND,
            entity_id: &meta.file_id,
            change_kind: "sync_stale",
            actor_id: SYNC_ACTOR,
            actor_kind: ActorKindDto::System,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO drive_doc_snapshots \
                 (client_id, file_id, name, mime_type, modified_time, version, \
                  parent_folder_ids_json, web_view_link, status, content_hash, chunk_count, \
                  last_error, first_seen_at_ms, last_synced_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, \
                         COALESCE((SELECT content_hash FROM drive_doc_snapshots \
                                   WHERE client_id = ?1 AND file_id = ?2), ''), \
                         COALESCE((SELECT chunk_count FROM drive_doc_snapshots \
                                   WHERE client_id = ?1 AND file_id = ?2), 0), \
                         NULL, ?10, ?11) \
                 ON CONFLICT (client_id, file_id) DO UPDATE SET \
                   name = excluded.name, \
                   mime_type = excluded.mime_type, \
                   modified_time = excluded.modified_time, \
                   version = excluded.version, \
                   parent_folder_ids_json = excluded.parent_folder_ids_json, \
                   web_view_link = excluded.web_view_link, \
                   status = excluded.status, \
                   last_error = NULL, \
                   last_synced_at_ms = excluded.last_synced_at_ms",
                params![
                    owned_client,
                    owned_meta.file_id,
                    owned_meta.name,
                    owned_meta.mime_type,
                    owned_meta.modified_time,
                    owned_meta.version,
                    parents_json,
                    owned_meta.web_view_link,
                    STATUS_STALE,
                    first_seen as i64,
                    now_ms as i64,
                ],
            )?;
            Ok(())
        },
    )?;
    Ok(true)
}

/// Metadata-only refresh (rename/move with the same content revision).
fn update_snapshot_meta(
    conn: &mut Connection,
    client_id: &str,
    meta: &DriveFileMeta,
    existing: &DocSnapshot,
    now_ms: u64,
) -> Result<(), StoreError> {
    let parents_json = serde_json::to_string(&meta.parent_folder_ids)
        .map_err(|err| StoreError::Domain(format!("serialize parents: {err}")))?;
    let idempotency_key = format!(
        "drive_doc_meta:{}:{}",
        meta.file_id,
        snapshot_hash(&[&meta.name, &parents_json])
    );
    let owned_client = client_id.to_string();
    let owned_meta = meta.clone();
    let owned_title = meta.name.clone();
    let title_changed = existing.meta.name != meta.name;
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: DOC_ENTITY_KIND,
            entity_id: &meta.file_id,
            change_kind: "sync_meta",
            actor_id: SYNC_ACTOR,
            actor_kind: ActorKindDto::System,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(
                serde_json::json!({"name": meta.name, "parents": meta.parent_folder_ids})
                    .to_string(),
            ),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE drive_doc_snapshots SET name = ?3, parent_folder_ids_json = ?4, \
                 web_view_link = ?5, last_synced_at_ms = ?6 \
                 WHERE client_id = ?1 AND file_id = ?2",
                params![
                    owned_client,
                    owned_meta.file_id,
                    owned_meta.name,
                    parents_json,
                    owned_meta.web_view_link,
                    now_ms as i64,
                ],
            )?;
            if title_changed {
                // Keep the index's title column honest for renamed docs.
                tx.execute(
                    "UPDATE drive_chunks_fts SET doc_title = ?3 \
                     WHERE client_id = ?1 AND file_id = ?2",
                    params![owned_client, owned_meta.file_id, owned_title],
                )?;
            }
            Ok(())
        },
    )?;
    Ok(())
}

/// One indexed chunk, produced by the deterministic chunker (service.rs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkRow {
    pub seq: u32,
    pub heading_path: Vec<String>,
    pub start_offset: u32,
    pub end_offset: u32,
    pub text: String,
}

/// Replace a document's chunks + FTS rows and flip the snapshot to indexed —
/// ONE receipted transaction. The chunk text is stored raw; the document
/// title and heading path ride separate FTS columns (weighted at query time),
/// which is how "title + heading path prepended to each chunk" lands without
/// duplicating the prefix into every row.
pub fn index_document(
    conn: &mut Connection,
    client_id: &str,
    file_id: &str,
    doc_title: &str,
    content_hash_hex: &str,
    chunks: &[ChunkRow],
    now_ms: u64,
) -> Result<(), StoreError> {
    let idempotency_key = format!("drive_doc_index:{file_id}:{content_hash_hex}");
    let after = serde_json::json!({
        "status": STATUS_INDEXED,
        "content_hash": content_hash_hex,
        "chunk_count": chunks.len(),
    })
    .to_string();
    let owned_client = client_id.to_string();
    let owned_file = file_id.to_string();
    let owned_title = doc_title.to_string();
    let owned_hash = content_hash_hex.to_string();
    let owned_chunks = chunks.to_vec();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: DOC_ENTITY_KIND,
            entity_id: file_id,
            change_kind: "index",
            actor_id: SYNC_ACTOR,
            actor_kind: ActorKindDto::System,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "DELETE FROM drive_chunks_fts WHERE client_id = ?1 AND file_id = ?2",
                params![owned_client, owned_file],
            )?;
            tx.execute(
                "DELETE FROM drive_chunks WHERE client_id = ?1 AND file_id = ?2",
                params![owned_client, owned_file],
            )?;
            for chunk in &owned_chunks {
                let chunk_id = format!("{}:{}", owned_file, chunk.seq);
                let heading_json = serde_json::to_string(&chunk.heading_path)
                    .map_err(|err| StoreError::Domain(format!("serialize heading: {err}")))?;
                tx.execute(
                    "INSERT INTO drive_chunks \
                     (client_id, chunk_id, file_id, seq, heading_path_json, start_offset, \
                      end_offset, text, created_at_ms) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        owned_client,
                        chunk_id,
                        owned_file,
                        chunk.seq as i64,
                        heading_json,
                        chunk.start_offset as i64,
                        chunk.end_offset as i64,
                        chunk.text,
                        now_ms as i64,
                    ],
                )?;
                tx.execute(
                    "INSERT INTO drive_chunks_fts \
                     (client_id, chunk_id, file_id, doc_title, heading_path, text) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        owned_client,
                        chunk_id,
                        owned_file,
                        owned_title,
                        chunk.heading_path.join(" > "),
                        chunk.text,
                    ],
                )?;
            }
            tx.execute(
                "UPDATE drive_doc_snapshots SET status = ?3, content_hash = ?4, \
                 chunk_count = ?5, last_error = NULL, last_synced_at_ms = ?6 \
                 WHERE client_id = ?1 AND file_id = ?2",
                params![
                    owned_client,
                    owned_file,
                    STATUS_INDEXED,
                    owned_hash,
                    owned_chunks.len() as i64,
                    now_ms as i64,
                ],
            )?;
            Ok(())
        },
    )?;
    Ok(())
}

/// A stale doc whose freshly read text hashed identically: flip back to
/// indexed without touching chunks.
pub fn touch_indexed(
    conn: &mut Connection,
    client_id: &str,
    file_id: &str,
    now_ms: u64,
) -> Result<(), StoreError> {
    set_doc_status(conn, client_id, file_id, STATUS_INDEXED, None, now_ms)
}

pub fn mark_skipped(
    conn: &mut Connection,
    client_id: &str,
    file_id: &str,
    now_ms: u64,
) -> Result<(), StoreError> {
    set_doc_status(conn, client_id, file_id, STATUS_SKIPPED, None, now_ms)
}

pub fn mark_error(
    conn: &mut Connection,
    client_id: &str,
    file_id: &str,
    error: &str,
    now_ms: u64,
) -> Result<(), StoreError> {
    let trimmed: String = error.chars().take(300).collect();
    set_doc_status(
        conn,
        client_id,
        file_id,
        STATUS_ERROR,
        Some(&trimmed),
        now_ms,
    )
}

fn set_doc_status(
    conn: &mut Connection,
    client_id: &str,
    file_id: &str,
    status: &'static str,
    error: Option<&str>,
    now_ms: u64,
) -> Result<(), StoreError> {
    let idempotency_key = format!(
        "drive_doc_status:{file_id}:{status}:{}",
        snapshot_hash(&[error.unwrap_or(""), &now_ms.to_string()])
    );
    let owned_client = client_id.to_string();
    let owned_file = file_id.to_string();
    let owned_error = error.map(str::to_string);
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: DOC_ENTITY_KIND,
            entity_id: file_id,
            change_kind: "sync_status",
            actor_id: SYNC_ACTOR,
            actor_kind: ActorKindDto::System,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(
                serde_json::json!({"status": status, "last_error": error}).to_string(),
            ),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE drive_doc_snapshots SET status = ?3, last_error = ?4, \
                 last_synced_at_ms = ?5 WHERE client_id = ?1 AND file_id = ?2",
                params![owned_client, owned_file, status, owned_error, now_ms as i64],
            )?;
            Ok(())
        },
    )?;
    Ok(())
}

/// File removed/trashed/out-of-corpus: drop its chunks + FTS rows and keep a
/// tombstone snapshot. No-op (no receipt) when nothing is stored or the row
/// is already removed. Returns true when a write happened.
pub fn mark_removed(
    conn: &mut Connection,
    client_id: &str,
    file_id: &str,
    now_ms: u64,
) -> Result<bool, StoreError> {
    let existing = get_snapshot(conn, client_id, file_id)?;
    let Some(existing) = existing else {
        return Ok(false);
    };
    if existing.status == STATUS_REMOVED {
        return Ok(false);
    }
    let idempotency_key = format!("drive_doc_remove:{file_id}:{}", existing.content_hash);
    let owned_client = client_id.to_string();
    let owned_file = file_id.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: DOC_ENTITY_KIND,
            entity_id: file_id,
            change_kind: "remove",
            actor_id: SYNC_ACTOR,
            actor_kind: ActorKindDto::System,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: Some(serde_json::json!({"status": existing.status}).to_string()),
            after_json: Some(serde_json::json!({"status": STATUS_REMOVED}).to_string()),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "DELETE FROM drive_chunks_fts WHERE client_id = ?1 AND file_id = ?2",
                params![owned_client, owned_file],
            )?;
            tx.execute(
                "DELETE FROM drive_chunks WHERE client_id = ?1 AND file_id = ?2",
                params![owned_client, owned_file],
            )?;
            tx.execute(
                "UPDATE drive_doc_snapshots SET status = ?3, chunk_count = 0, \
                 content_hash = '', last_synced_at_ms = ?4 \
                 WHERE client_id = ?1 AND file_id = ?2",
                params![owned_client, owned_file, STATUS_REMOVED, now_ms as i64],
            )?;
            Ok(())
        },
    )?;
    Ok(true)
}

/// BM25 search over the chunk index. `match_expr` is a pre-built FTS5 MATCH
/// expression (service::fts_match_expression — never raw operator input).
/// Column weights: title 5, heading path 3, body 1 (the deterministic
/// contextual-retrieval prior: where a term appears matters).
pub fn search_chunks(
    conn: &Connection,
    client_id: &str,
    match_expr: &str,
    limit: usize,
) -> Result<Vec<DriveSearchHit>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT drive_chunks_fts.chunk_id, drive_chunks_fts.file_id, \
                drive_chunks_fts.doc_title, c.heading_path_json, c.text, s.web_view_link, \
                bm25(drive_chunks_fts, 0.0, 0.0, 0.0, 5.0, 3.0, 1.0) AS score \
         FROM drive_chunks_fts \
         JOIN drive_chunks c \
           ON c.client_id = drive_chunks_fts.client_id \
          AND c.chunk_id = drive_chunks_fts.chunk_id \
         JOIN drive_doc_snapshots s \
           ON s.client_id = drive_chunks_fts.client_id \
          AND s.file_id = drive_chunks_fts.file_id \
         WHERE drive_chunks_fts MATCH ?1 AND drive_chunks_fts.client_id = ?2 \
         ORDER BY score ASC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![match_expr, client_id, limit as i64], |row| {
        Ok(DriveSearchHit {
            chunk_id: row.get(0)?,
            file_id: row.get(1)?,
            doc_title: row.get(2)?,
            heading_path: serde_json::from_str(&row.get::<_, String>(3)?).unwrap_or_default(),
            text: row.get(4)?,
            web_view_link: row.get(5)?,
            score: row.get(6)?,
        })
    })?;
    let mut hits = Vec::new();
    for row in rows {
        hits.push(row?);
    }
    Ok(hits)
}

fn snapshot_hash(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0u8]);
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(16);
    for byte in &digest[..8] {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}
