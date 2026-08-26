//! Read-only rollup queries over existing tables (receipts, outbox_jobs,
//! ai_usage_log). This slice owns no tables and writes nothing.

use bos_contracts::instance_diagnostics::{ErrorRollupDto, LlmErrorRollupDto, OutboxBacklogDto};
use rusqlite::{params, Connection};

use crate::store_core::StoreError;

/// Migration level of the open database (rusqlite_migration tracks it in
/// PRAGMA user_version).
pub fn schema_version(conn: &Connection) -> Result<u32, StoreError> {
    conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
        .map(|version| version as u32)
        .map_err(Into::into)
}

/// Failed/conflict receipts and LLM failures within the trailing window.
pub fn error_rollup(
    conn: &Connection,
    client_id: &str,
    now_ms: u64,
    window_ms: u64,
) -> Result<ErrorRollupDto, StoreError> {
    let since_ms = now_ms.saturating_sub(window_ms);
    let (failed_receipts, conflict_receipts) = conn.query_row(
        "SELECT \
         COALESCE(SUM(CASE WHEN outcome = 'failed' THEN 1 ELSE 0 END), 0), \
         COALESCE(SUM(CASE WHEN outcome = 'revision_conflict' THEN 1 ELSE 0 END), 0) \
         FROM receipts WHERE client_id = ?1 AND created_at_ms >= ?2",
        params![client_id, since_ms as i64],
        |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? as u64)),
    )?;
    let mut stmt = conn.prepare(
        "SELECT purpose, error_code, COUNT(*) FROM ai_usage_log \
         WHERE client_id = ?1 AND success = 0 AND recorded_at_ms >= ?2 \
         GROUP BY purpose, error_code \
         ORDER BY COUNT(*) DESC, purpose ASC",
    )?;
    let rows = stmt.query_map(params![client_id, since_ms as i64], |row| {
        Ok(LlmErrorRollupDto {
            purpose: row.get(0)?,
            error_code: row.get(1)?,
            count: row.get::<_, i64>(2)? as u64,
        })
    })?;
    let mut llm_errors = Vec::new();
    for row in rows {
        llm_errors.push(row?);
    }
    let llm_failures = llm_errors.iter().map(|rollup| rollup.count).sum();
    Ok(ErrorRollupDto {
        window_ms,
        failed_receipts,
        conflict_receipts,
        llm_failures,
        llm_errors,
    })
}

/// Point-in-time outbox backlog with a sample of the latest terminal error.
pub fn outbox_backlog(conn: &Connection, client_id: &str) -> Result<OutboxBacklogDto, StoreError> {
    let (pending_jobs, terminal_jobs) = conn.query_row(
        "SELECT \
         COALESCE(SUM(CASE WHEN status = 'pending' THEN 1 ELSE 0 END), 0), \
         COALESCE(SUM(CASE WHEN status IN ('failed_terminal', 'delivery_outcome_unknown') THEN 1 ELSE 0 END), 0) \
         FROM outbox_jobs WHERE client_id = ?1",
        params![client_id],
        |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? as u64)),
    )?;
    let last_terminal_error = conn
        .query_row(
            "SELECT last_error FROM outbox_jobs \
             WHERE client_id = ?1 AND status IN ('failed_terminal', 'delivery_outcome_unknown') \
             ORDER BY updated_at_ms DESC, job_id DESC LIMIT 1",
            params![client_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .unwrap_or(None);
    Ok(OutboxBacklogDto {
        pending_jobs,
        terminal_jobs,
        last_terminal_error,
    })
}
