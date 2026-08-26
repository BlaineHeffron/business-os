//! Release note persistence through store_core.

use bos_contracts::receipt::ActorKindDto;
use bos_contracts::release_notes::ReleaseNote;
use rusqlite::{params, Connection, OptionalExtension};

use crate::store_core::{self, MutationOutcome, MutationRequest, StoreError};

pub const NOTE_ENTITY_KIND: &str = "release_note";
pub const DISMISSAL_ENTITY_KIND: &str = "release_note_dismissal";

pub fn insert_note(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    note: &ReleaseNote,
    idempotency_key: &str,
) -> Result<MutationOutcome, StoreError> {
    if note.release_note_id.trim().is_empty() {
        return Err(StoreError::Domain("release_note_id_required".to_string()));
    }
    if note.summary.trim().is_empty() {
        return Err(StoreError::Domain("release_note_summary_empty".to_string()));
    }
    let after = serde_json::to_string(note)
        .map_err(|err| StoreError::Domain(format!("serialize release note: {err}")))?;
    let row = note.clone();
    let owned_client = client_id.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: NOTE_ENTITY_KIND,
            entity_id: &note.release_note_id,
            change_kind: "create",
            actor_id,
            actor_kind: ActorKindDto::System,
            expected_revision: None,
            idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms: note.created_at_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO release_notes \
                 (client_id, release_note_id, title, summary, body, build_sha, \
                  created_at_ms, updated_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7) \
                 ON CONFLICT (client_id, release_note_id) DO UPDATE SET \
                   title = excluded.title, \
                   summary = excluded.summary, \
                   body = excluded.body, \
                   build_sha = excluded.build_sha, \
                   created_at_ms = excluded.created_at_ms, \
                   updated_at_ms = excluded.updated_at_ms",
                params![
                    owned_client,
                    row.release_note_id,
                    row.title,
                    row.summary,
                    row.body,
                    row.build_sha,
                    row.created_at_ms as i64,
                ],
            )?;
            Ok(())
        },
    )
}

pub fn dismiss_note(
    conn: &mut Connection,
    client_id: &str,
    user_id: &str,
    actor_id: &str,
    release_note_id: &str,
    idempotency_key: &str,
    now_ms: u64,
) -> Result<MutationOutcome, StoreError> {
    let release_note_id = release_note_id.trim();
    if release_note_id.is_empty() {
        return Err(StoreError::Domain("release_note_id_required".to_string()));
    }
    if !note_exists(conn, client_id, release_note_id)? {
        return Err(StoreError::Domain("release_note_not_found".to_string()));
    }
    let entity_id = dismissal_entity_id(user_id, release_note_id);
    let after = serde_json::json!({
        "user_id": user_id,
        "release_note_id": release_note_id,
        "dismissed_at_ms": now_ms,
    })
    .to_string();
    let owned_client = client_id.to_string();
    let owned_user = user_id.to_string();
    let owned_note = release_note_id.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: DISMISSAL_ENTITY_KIND,
            entity_id: &entity_id,
            change_kind: "dismiss",
            actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: None,
            idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO release_note_dismissals \
                 (client_id, user_id, release_note_id, dismissed_at_ms) \
                 VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT (client_id, user_id, release_note_id) DO UPDATE SET \
                   dismissed_at_ms = excluded.dismissed_at_ms",
                params![owned_client, owned_user, owned_note, now_ms as i64],
            )?;
            Ok(())
        },
    )
}

pub fn latest_visible(
    conn: &Connection,
    client_id: &str,
    user_id: &str,
) -> Result<Option<ReleaseNote>, StoreError> {
    conn.query_row(
        "SELECT rn.release_note_id, rn.title, rn.summary, rn.body, rn.build_sha, \
                rn.created_at_ms \
         FROM release_notes rn \
         WHERE rn.client_id = ?1 \
           AND NOT EXISTS ( \
             SELECT 1 FROM release_note_dismissals d \
             WHERE d.client_id = rn.client_id \
               AND d.release_note_id = rn.release_note_id \
               AND d.user_id = ?2 \
           ) \
         ORDER BY rn.created_at_ms DESC, rn.release_note_id DESC \
         LIMIT 1",
        params![client_id, user_id],
        note_from_row,
    )
    .optional()
    .map_err(Into::into)
}

pub fn list_recent(
    conn: &Connection,
    client_id: &str,
    limit: usize,
) -> Result<Vec<ReleaseNote>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT release_note_id, title, summary, body, build_sha, created_at_ms \
         FROM release_notes WHERE client_id = ?1 \
         ORDER BY created_at_ms DESC, release_note_id DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![client_id, limit as i64], note_from_row)?;
    let mut notes = Vec::new();
    for row in rows {
        notes.push(row?);
    }
    Ok(notes)
}

fn note_exists(
    conn: &Connection,
    client_id: &str,
    release_note_id: &str,
) -> Result<bool, StoreError> {
    let found: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM release_notes WHERE client_id = ?1 AND release_note_id = ?2",
            params![client_id, release_note_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(found.is_some())
}

fn dismissal_entity_id(user_id: &str, release_note_id: &str) -> String {
    format!("{user_id}:{release_note_id}")
}

fn note_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ReleaseNote> {
    Ok(ReleaseNote {
        release_note_id: row.get(0)?,
        title: row.get(1)?,
        summary: row.get(2)?,
        body: row.get(3)?,
        build_sha: row.get(4)?,
        created_at_ms: row.get::<_, i64>(5)? as u64,
    })
}
