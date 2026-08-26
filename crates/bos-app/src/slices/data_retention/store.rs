//! Read-only SQLite allocation inspection plus receipted manual-run kickoff.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use bos_contracts::receipt::ActorKindDto;
use rusqlite::{params, Connection, OptionalExtension};

use crate::store_core::{self, MutationOutcome, MutationRequest, StoreError};

pub const RETENTION_ENTITY_KIND: &str = "data_retention";
pub const MANUAL_KICKOFF_CHANGE_KIND: &str = "manual_kickoff";
pub const EMAIL_BODY_COMPACTION_CHANGE_KIND: &str = "compact_email_bodies";
pub const RECEIPT_PAYLOAD_COMPACTION_CHANGE_KIND: &str = "compact_receipt_payloads";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteStorageStats {
    pub database_bytes: u64,
    pub page_size_bytes: u64,
    pub page_count: u64,
    pub freelist_pages: u64,
    pub wal_bytes: u64,
    pub auto_vacuum_mode: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalCheckpointStats {
    pub busy: u64,
    pub log_pages: u64,
    pub checkpointed_pages: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatestRetentionReceipt {
    pub created_at_ms: u64,
    pub outcome: String,
}

#[derive(Debug, Clone)]
pub struct ManualKickoff<'a> {
    pub run_id: &'a str,
    pub actor_id: &'a str,
    pub idempotency_key: &'a str,
    pub now_ms: u64,
}

#[derive(Debug, Clone)]
pub struct ManualKickoffOutcome {
    pub mutation: MutationOutcome,
    pub run_id: String,
}

pub fn sqlite_storage_stats(conn: &Connection) -> Result<SqliteStorageStats, StoreError> {
    let page_size_bytes = pragma_u64(conn, "PRAGMA page_size")?;
    let page_count = pragma_u64(conn, "PRAGMA page_count")?;
    let freelist_pages = pragma_u64(conn, "PRAGMA freelist_count")?;
    let auto_vacuum_mode = conn.query_row("PRAGMA auto_vacuum", [], |row| row.get::<_, i64>(0))?;
    let database_path = main_database_path(conn)?;
    let database_bytes = database_path
        .as_deref()
        .and_then(file_size)
        .unwrap_or_else(|| page_count.saturating_mul(page_size_bytes));
    let wal_bytes = database_path
        .as_deref()
        .map(wal_path)
        .as_deref()
        .and_then(file_size)
        .unwrap_or(0);
    Ok(SqliteStorageStats {
        database_bytes,
        page_size_bytes,
        page_count,
        freelist_pages,
        wal_bytes,
        auto_vacuum_mode,
    })
}

pub fn checkpoint_passive(conn: &Connection) -> Result<WalCheckpointStats, StoreError> {
    wal_checkpoint(conn, "PASSIVE")
}

pub fn checkpoint_truncate(conn: &Connection) -> Result<WalCheckpointStats, StoreError> {
    wal_checkpoint(conn, "TRUNCATE")
}

pub fn optimize(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch("PRAGMA optimize")?;
    Ok(())
}

pub fn incremental_vacuum(conn: &Connection, pages: usize) -> Result<(), StoreError> {
    if pages == 0 {
        return Ok(());
    }
    let mode = conn.query_row("PRAGMA auto_vacuum", [], |row| row.get::<_, i64>(0))?;
    if mode == 2 {
        conn.execute_batch(&format!("PRAGMA incremental_vacuum({pages})"))?;
    }
    Ok(())
}

pub fn latest_retention_receipt(
    conn: &Connection,
    client_id: &str,
) -> Result<Option<LatestRetentionReceipt>, StoreError> {
    conn.query_row(
        "SELECT created_at_ms, outcome FROM receipts \
         WHERE client_id = ?1 AND entity_kind = ?2 \
         ORDER BY created_at_ms DESC, receipt_id DESC LIMIT 1",
        params![client_id, RETENTION_ENTITY_KIND],
        |row| {
            Ok(LatestRetentionReceipt {
                created_at_ms: row.get::<_, i64>(0)? as u64,
                outcome: row.get(1)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

pub fn record_manual_kickoff(
    conn: &mut Connection,
    client_id: &str,
    input: ManualKickoff<'_>,
) -> Result<ManualKickoffOutcome, StoreError> {
    let after_json = serde_json::json!({ "run_id": input.run_id }).to_string();
    let mutation = store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: RETENTION_ENTITY_KIND,
            entity_id: input.run_id,
            change_kind: MANUAL_KICKOFF_CHANGE_KIND,
            actor_id: input.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: None,
            idempotency_key: input.idempotency_key,
            correlation_id: Some(input.run_id),
            causation_id: None,
            before_json: None,
            after_json: Some(after_json),
            now_ms: input.now_ms,
        },
        |_tx| Ok(()),
    )?;
    let run_id = match mutation {
        MutationOutcome::ReplayedIdempotent { .. } => {
            manual_run_id_for_idempotency_key(conn, client_id, input.idempotency_key)?
                .unwrap_or_else(|| input.run_id.to_string())
        }
        _ => input.run_id.to_string(),
    };
    Ok(ManualKickoffOutcome { mutation, run_id })
}

fn manual_run_id_for_idempotency_key(
    conn: &Connection,
    client_id: &str,
    idempotency_key: &str,
) -> Result<Option<String>, StoreError> {
    let after_json = conn
        .query_row(
            "SELECT after_json FROM receipts \
             WHERE client_id = ?1 AND idempotency_key = ?2 \
               AND entity_kind = ?3 AND change_kind = ?4 AND outcome = 'applied' \
             ORDER BY created_at_ms ASC, receipt_id ASC LIMIT 1",
            params![
                client_id,
                idempotency_key,
                RETENTION_ENTITY_KIND,
                MANUAL_KICKOFF_CHANGE_KIND
            ],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    let Some(after_json) = after_json else {
        return Ok(None);
    };
    let value: serde_json::Value = serde_json::from_str(&after_json)
        .map_err(|err| StoreError::Domain(format!("retention_kickoff_invalid:{err}")))?;
    Ok(value
        .get("run_id")
        .and_then(|run_id| run_id.as_str())
        .map(str::to_string))
}

fn pragma_u64(conn: &Connection, sql: &str) -> Result<u64, StoreError> {
    conn.query_row(sql, [], |row| row.get::<_, i64>(0))
        .map(|value| value.max(0) as u64)
        .map_err(Into::into)
}

fn wal_checkpoint(conn: &Connection, mode: &str) -> Result<WalCheckpointStats, StoreError> {
    let sql = format!("PRAGMA wal_checkpoint({mode})");
    conn.query_row(&sql, [], |row| {
        Ok(WalCheckpointStats {
            busy: row.get::<_, i64>(0)?.max(0) as u64,
            log_pages: row.get::<_, i64>(1)?.max(0) as u64,
            checkpointed_pages: row.get::<_, i64>(2)?.max(0) as u64,
        })
    })
    .map_err(Into::into)
}

fn main_database_path(conn: &Connection) -> Result<Option<PathBuf>, StoreError> {
    let mut stmt = conn.prepare("PRAGMA database_list")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(1)?, row.get::<_, String>(2)?))
    })?;
    for row in rows {
        let (name, path) = row?;
        if name == "main" && !path.is_empty() {
            return Ok(Some(PathBuf::from(path)));
        }
    }
    Ok(None)
}

fn wal_path(database_path: &Path) -> PathBuf {
    let mut path: OsString = database_path.as_os_str().to_owned();
    path.push("-wal");
    PathBuf::from(path)
}

fn file_size(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok().map(|metadata| metadata.len())
}
