//! Debug diagnostics projection plus the receipted panic diagnostic write path.

use bos_contracts::debug::DebugDiagnosticRow;
use bos_contracts::packet_proposals::{
    PacketProposalKindOutcome, PacketProposalKindOutcomeStatus, PacketProposalReasonCode,
};
use bos_contracts::receipt::ActorKindDto;
use rusqlite::{params, Connection, Transaction};

use crate::outbox;
use crate::store_core::{self, StoreError};

pub const AGENT_LAUNCH_ENTITY_KIND: &str = "debug_agent_launch";

pub fn list_recent(
    conn: &Connection,
    client_id: &str,
    limit: usize,
) -> Result<Vec<DebugDiagnosticRow>, StoreError> {
    let mut rows = Vec::new();
    append_panic_diagnostics(conn, client_id, limit, &mut rows)?;
    append_receipt_diagnostics(conn, client_id, limit, &mut rows)?;
    append_outbox_diagnostics(conn, client_id, limit, &mut rows)?;
    append_sync_cursor_diagnostics(conn, client_id, limit, &mut rows)?;
    append_drive_doc_diagnostics(conn, client_id, limit, &mut rows)?;
    append_packet_proposal_diagnostics(conn, client_id, limit, &mut rows)?;
    append_llm_diagnostics(conn, client_id, limit, &mut rows)?;
    rows.sort_by(|left, right| {
        right
            .occurred_at_ms
            .cmp(&left.occurred_at_ms)
            .then_with(|| right.diagnostic_id.cmp(&left.diagnostic_id))
    });
    rows.truncate(limit);
    Ok(rows)
}

pub struct PanicDiagnosticInsert<'a> {
    pub diagnostic_id: &'a str,
    pub client_id: &'a str,
    pub message: &'a str,
    pub location: Option<&'a str>,
    pub backtrace: &'a str,
    pub thread_name: Option<&'a str>,
    pub occurred_at_ms: u64,
}

pub fn insert_panic_diagnostic(
    conn: &mut Connection,
    input: &PanicDiagnosticInsert<'_>,
) -> Result<store_core::MutationOutcome, StoreError> {
    let after_json = serde_json::json!({
        "diagnostic_id": input.diagnostic_id,
        "message": input.message,
        "location": input.location,
        "backtrace": input.backtrace,
        "thread_name": input.thread_name,
        "occurred_at_ms": input.occurred_at_ms,
    })
    .to_string();
    store_core::mutate(
        conn,
        store_core::MutationRequest {
            client_id: input.client_id,
            entity_kind: "panic_diagnostic",
            entity_id: input.diagnostic_id,
            change_kind: "record",
            actor_id: "system",
            actor_kind: ActorKindDto::System,
            expected_revision: None,
            idempotency_key: input.diagnostic_id,
            correlation_id: Some(input.diagnostic_id),
            causation_id: None,
            before_json: None,
            after_json: Some(after_json),
            now_ms: input.occurred_at_ms,
        },
        |tx| insert_panic_diagnostic_row(tx, input),
    )
}

pub struct AgentLaunchRequestContext<'a> {
    pub client_id: &'a str,
    pub diagnostic_id: &'a str,
    pub actor_id: &'a str,
    pub job: &'a outbox::NewOutboxJob,
    pub idempotency_key: &'a str,
    pub now_ms: u64,
}

pub fn record_agent_launch_request(
    conn: &mut Connection,
    ctx: AgentLaunchRequestContext<'_>,
) -> Result<store_core::MutationOutcome, StoreError> {
    let after_json = serde_json::json!({
        "diagnostic_id": ctx.diagnostic_id,
        "outbox_job_id": ctx.job.job_id,
    })
    .to_string();
    let owned_client = ctx.client_id.to_string();
    let owned_job = ctx.job.clone();
    store_core::mutate(
        conn,
        store_core::MutationRequest {
            client_id: ctx.client_id,
            entity_kind: AGENT_LAUNCH_ENTITY_KIND,
            entity_id: ctx.diagnostic_id,
            change_kind: "spawn_agent",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: None,
            idempotency_key: ctx.idempotency_key,
            correlation_id: Some(ctx.diagnostic_id),
            causation_id: None,
            before_json: None,
            after_json: Some(after_json),
            now_ms: ctx.now_ms,
        },
        move |tx| outbox::enqueue_within(tx, &owned_client, &owned_job, ctx.now_ms),
    )
}

fn insert_panic_diagnostic_row(
    tx: &Transaction<'_>,
    input: &PanicDiagnosticInsert<'_>,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO panic_diagnostics (
            diagnostic_id, client_id, message, location, backtrace, thread_name, occurred_at_ms
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            input.diagnostic_id,
            input.client_id,
            input.message,
            input.location,
            input.backtrace,
            input.thread_name,
            input.occurred_at_ms as i64,
        ],
    )?;
    Ok(())
}

fn append_panic_diagnostics(
    conn: &Connection,
    client_id: &str,
    limit: usize,
    out: &mut Vec<DebugDiagnosticRow>,
) -> Result<(), StoreError> {
    let mut stmt = conn.prepare(
        "SELECT diagnostic_id, message, location, backtrace, thread_name, occurred_at_ms \
         FROM panic_diagnostics \
         WHERE client_id = ?1 \
         ORDER BY occurred_at_ms DESC, diagnostic_id DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![client_id, limit as i64], |row| {
        let diagnostic_id: String = row.get(0)?;
        let message: String = row.get(1)?;
        let location: Option<String> = row.get(2)?;
        let backtrace: String = row.get(3)?;
        let thread_name: Option<String> = row.get(4)?;
        let mut error_message = message;
        if let Some(location) = location {
            error_message.push_str("\nlocation: ");
            error_message.push_str(&location);
        }
        if let Some(thread_name) = thread_name {
            error_message.push_str("\nthread: ");
            error_message.push_str(&thread_name);
        }
        error_message.push_str("\nbacktrace:\n");
        error_message.push_str(&backtrace);
        Ok(DebugDiagnosticRow {
            diagnostic_id: format!("panic:{diagnostic_id}"),
            source: "panic".to_string(),
            severity: "error".to_string(),
            category: "panic".to_string(),
            entity_kind: Some("process".to_string()),
            entity_id: None,
            operation: Some("panic_unwind".to_string()),
            error_code: "panic".to_string(),
            error_message: Some(error_message),
            correlation_id: None,
            reference_id: Some(diagnostic_id),
            occurred_at_ms: row.get::<_, i64>(5)?.max(0) as u64,
        })
    })?;
    for row in rows {
        out.push(row?);
    }
    Ok(())
}

fn append_receipt_diagnostics(
    conn: &Connection,
    client_id: &str,
    limit: usize,
    out: &mut Vec<DebugDiagnosticRow>,
) -> Result<(), StoreError> {
    let mut stmt = conn.prepare(
        "SELECT receipt_id, entity_kind, entity_id, change_kind, outcome, error_class, \
         correlation_id, created_at_ms, after_json \
         FROM receipts \
         WHERE client_id = ?1 AND outcome IN ('failed', 'revision_conflict') \
         ORDER BY created_at_ms DESC, receipt_id DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![client_id, limit as i64], |row| {
        let receipt_id: String = row.get(0)?;
        let outcome: String = row.get(4)?;
        let error_class: Option<String> = row.get(5)?;
        Ok(DebugDiagnosticRow {
            diagnostic_id: format!("receipt:{receipt_id}"),
            source: "receipt".to_string(),
            severity: if outcome == "revision_conflict" {
                "warning".to_string()
            } else {
                "error".to_string()
            },
            category: "mutation".to_string(),
            entity_kind: row.get(1)?,
            entity_id: row.get(2)?,
            operation: row.get(3)?,
            error_code: error_class.unwrap_or(outcome),
            error_message: receipt_error_message(row.get::<_, Option<String>>(8)?.as_deref()),
            correlation_id: row.get(6)?,
            reference_id: Some(receipt_id),
            occurred_at_ms: row.get::<_, i64>(7)? as u64,
        })
    })?;
    for row in rows {
        out.push(row?);
    }
    Ok(())
}

fn receipt_error_message(after_json: Option<&str>) -> Option<String> {
    let value = after_json.and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())?;
    value
        .get("message")
        .or_else(|| value.get("reason"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .map(|message| message.chars().take(1_000).collect())
}

fn append_outbox_diagnostics(
    conn: &Connection,
    client_id: &str,
    limit: usize,
    out: &mut Vec<DebugDiagnosticRow>,
) -> Result<(), StoreError> {
    let mut stmt = conn.prepare(
        "SELECT job_id, provider, capability, status, attempts, last_error, \
         source_entity_kind, source_entity_id, correlation_id, updated_at_ms \
         FROM outbox_jobs \
         WHERE client_id = ?1 AND last_error IS NOT NULL \
         ORDER BY updated_at_ms DESC, job_id DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![client_id, limit as i64], |row| {
        let job_id: String = row.get(0)?;
        let provider: String = row.get(1)?;
        let capability: String = row.get(2)?;
        let status: String = row.get(3)?;
        let attempts: i64 = row.get(4)?;
        Ok(DebugDiagnosticRow {
            diagnostic_id: format!("outbox:{job_id}"),
            source: "outbox".to_string(),
            severity: if matches!(
                status.as_str(),
                crate::outbox::STATUS_FAILED_TERMINAL
                    | crate::outbox::STATUS_DELIVERY_OUTCOME_UNKNOWN
            ) {
                "error".to_string()
            } else {
                "warning".to_string()
            },
            category: "provider_delivery".to_string(),
            entity_kind: row.get(6)?,
            entity_id: row.get(7)?,
            operation: Some(format!("{provider}:{capability}")),
            error_code: status,
            error_message: row
                .get::<_, Option<String>>(5)?
                .map(|message| format!("attempt {attempts}: {message}")),
            correlation_id: row.get(8)?,
            reference_id: Some(job_id),
            occurred_at_ms: row.get::<_, i64>(9)? as u64,
        })
    })?;
    for row in rows {
        out.push(row?);
    }
    Ok(())
}

fn append_sync_cursor_diagnostics(
    conn: &Connection,
    client_id: &str,
    limit: usize,
    out: &mut Vec<DebugDiagnosticRow>,
) -> Result<(), StoreError> {
    append_entity_cursor_diagnostics(
        conn,
        client_id,
        limit,
        out,
        CursorDiagnosticSpec {
            table: "accounting_sync_cursors",
            category: "accounting_sync",
            entity_kind: "accounting_sync_cursor",
            id_column: "entity",
        },
    )?;
    append_entity_cursor_diagnostics(
        conn,
        client_id,
        limit,
        out,
        CursorDiagnosticSpec {
            table: "stockforge_sync_cursors",
            category: "inventory_sync",
            entity_kind: "stockforge_sync_cursor",
            id_column: "entity",
        },
    )?;
    append_entity_cursor_diagnostics(
        conn,
        client_id,
        limit,
        out,
        CursorDiagnosticSpec {
            table: "drive_sync_cursors",
            category: "drive_sync",
            entity_kind: "drive_sync_cursor",
            id_column: "corpus_id",
        },
    )?;
    let mut stmt = conn.prepare(
        "SELECT last_error, last_advanced_at_ms FROM claims_sync_cursors \
         WHERE client_id = ?1 AND last_error IS NOT NULL \
         ORDER BY COALESCE(last_advanced_at_ms, 0) DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![client_id, limit as i64], |row| {
        let occurred_at_ms = row.get::<_, Option<i64>>(1)?.unwrap_or(0).max(0) as u64;
        Ok(DebugDiagnosticRow {
            diagnostic_id: "sync:claims_sync_cursors:claims".to_string(),
            source: "sync".to_string(),
            severity: "error".to_string(),
            category: "claim_sync".to_string(),
            entity_kind: Some("claims_sync_cursor".to_string()),
            entity_id: Some("claims".to_string()),
            operation: Some("sync".to_string()),
            error_code: "last_error".to_string(),
            error_message: row.get(0)?,
            correlation_id: None,
            reference_id: Some("claims".to_string()),
            occurred_at_ms,
        })
    })?;
    for row in rows {
        out.push(row?);
    }
    Ok(())
}

struct CursorDiagnosticSpec {
    table: &'static str,
    category: &'static str,
    entity_kind: &'static str,
    id_column: &'static str,
}

fn append_entity_cursor_diagnostics(
    conn: &Connection,
    client_id: &str,
    limit: usize,
    out: &mut Vec<DebugDiagnosticRow>,
    spec: CursorDiagnosticSpec,
) -> Result<(), StoreError> {
    let sql = format!(
        "SELECT {id_column}, last_error, last_advanced_at_ms FROM {table} \
         WHERE client_id = ?1 AND last_error IS NOT NULL \
         ORDER BY COALESCE(last_advanced_at_ms, 0) DESC, {id_column} DESC LIMIT ?2",
        id_column = spec.id_column,
        table = spec.table,
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![client_id, limit as i64], |row| {
        let entity_id: String = row.get(0)?;
        let occurred_at_ms = row.get::<_, Option<i64>>(2)?.unwrap_or(0).max(0) as u64;
        Ok(DebugDiagnosticRow {
            diagnostic_id: format!("sync:{}:{entity_id}", spec.table),
            source: "sync".to_string(),
            severity: "error".to_string(),
            category: spec.category.to_string(),
            entity_kind: Some(spec.entity_kind.to_string()),
            entity_id: Some(entity_id.clone()),
            operation: Some("sync".to_string()),
            error_code: "last_error".to_string(),
            error_message: row.get(1)?,
            correlation_id: None,
            reference_id: Some(entity_id),
            occurred_at_ms,
        })
    })?;
    for row in rows {
        out.push(row?);
    }
    Ok(())
}

fn append_drive_doc_diagnostics(
    conn: &Connection,
    client_id: &str,
    limit: usize,
    out: &mut Vec<DebugDiagnosticRow>,
) -> Result<(), StoreError> {
    let mut stmt = conn.prepare(
        "SELECT file_id, name, last_error, last_synced_at_ms FROM drive_doc_snapshots \
         WHERE client_id = ?1 AND status = 'error' AND last_error IS NOT NULL \
         ORDER BY last_synced_at_ms DESC, file_id DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![client_id, limit as i64], |row| {
        let file_id: String = row.get(0)?;
        let name: String = row.get(1)?;
        Ok(DebugDiagnosticRow {
            diagnostic_id: format!("drive_doc:{file_id}"),
            source: "drive".to_string(),
            severity: "error".to_string(),
            category: "document_index".to_string(),
            entity_kind: Some("drive_doc_snapshot".to_string()),
            entity_id: Some(file_id.clone()),
            operation: Some(name),
            error_code: "drive_doc_error".to_string(),
            error_message: row.get(2)?,
            correlation_id: None,
            reference_id: Some(file_id),
            occurred_at_ms: row.get::<_, i64>(3)?.max(0) as u64,
        })
    })?;
    for row in rows {
        out.push(row?);
    }
    Ok(())
}

fn append_packet_proposal_diagnostics(
    conn: &Connection,
    client_id: &str,
    limit: usize,
    out: &mut Vec<DebugDiagnosticRow>,
) -> Result<(), StoreError> {
    if limit == 0 {
        return Ok(());
    }
    // Most completed Smart Draft runs are successful and therefore not debug
    // diagnostics. Scan past the display limit so successful runs do not crowd
    // out nearby failed/no-draft runs before Rust parses the outcome JSON.
    let scan_limit = limit.saturating_mul(10).max(limit);
    let mut stmt = conn.prepare(
        "SELECT run_id, source_kind, source_ref, item_id, status, outcomes_json, \
                COALESCE(error_code, ''), updated_at_ms \
         FROM packet_proposal_runs \
         WHERE client_id = ?1 AND status IN ('completed', 'failed') \
         ORDER BY updated_at_ms DESC, run_id DESC LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![client_id, scan_limit as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, i64>(7)?.max(0) as u64,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (
        run_id,
        source_kind,
        source_ref,
        item_id,
        status,
        outcomes_json,
        error_code,
        occurred_at_ms,
    ) in rows
    {
        let outcomes: Vec<PacketProposalKindOutcome> =
            serde_json::from_str(&outcomes_json).unwrap_or_default();
        let has_draft = outcomes
            .iter()
            .any(|outcome| outcome.status == PacketProposalKindOutcomeStatus::Drafted);
        if status == "completed" && has_draft {
            continue;
        }
        if status == "completed" && packet_proposal_no_draft_is_expected(&outcomes) {
            continue;
        }
        let evidence = packet_proposal_evidence_excerpt(conn, client_id, &run_id)?;
        let error_code = if error_code.trim().is_empty() {
            if status == "failed" {
                "smart_draft_failed".to_string()
            } else {
                "smart_draft_no_reviewable_drafts".to_string()
            }
        } else {
            error_code
        };
        out.push(DebugDiagnosticRow {
            diagnostic_id: format!("packet_proposal:{run_id}"),
            source: "packet_proposal".to_string(),
            severity: if status == "failed" {
                "error".to_string()
            } else {
                "warning".to_string()
            },
            category: "smart_draft".to_string(),
            entity_kind: Some("packet_proposal_run".to_string()),
            entity_id: Some(run_id.clone()),
            operation: Some(status),
            error_code,
            error_message: packet_proposal_error_message(&outcomes, evidence.as_deref()),
            correlation_id: Some(format!("{source_kind}:{source_ref}")),
            reference_id: item_id.or(Some(run_id)),
            occurred_at_ms,
        });
    }
    Ok(())
}

fn packet_proposal_no_draft_is_expected(outcomes: &[PacketProposalKindOutcome]) -> bool {
    !outcomes.is_empty()
        && outcomes.iter().all(|outcome| {
            outcome.status == PacketProposalKindOutcomeStatus::Unavailable
                && matches!(
                    outcome.reason_code,
                    Some(
                        PacketProposalReasonCode::ContextUnavailable
                            | PacketProposalReasonCode::KindNotRequested
                            | PacketProposalReasonCode::LowConfidence
                    )
                )
        })
}

fn packet_proposal_evidence_excerpt(
    conn: &Connection,
    client_id: &str,
    run_id: &str,
) -> Result<Option<String>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT result_ref, result_excerpt FROM packet_proposal_run_evidence \
         WHERE client_id = ?1 AND run_id = ?2 \
         ORDER BY turn_index ASC, evidence_id ASC LIMIT 3",
    )?;
    let rows = stmt.query_map(params![client_id, run_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut parts = Vec::new();
    for row in rows {
        let (result_ref, excerpt) = row?;
        let excerpt: String = excerpt.chars().take(800).collect();
        parts.push(format!("{result_ref}: {excerpt}"));
    }
    Ok((!parts.is_empty()).then(|| parts.join("\n\n")))
}

fn packet_proposal_error_message(
    outcomes: &[PacketProposalKindOutcome],
    evidence: Option<&str>,
) -> Option<String> {
    let mut parts: Vec<String> = outcomes
        .iter()
        .filter(|outcome| outcome.status != PacketProposalKindOutcomeStatus::Drafted)
        .map(|outcome| {
            let reason = outcome
                .message
                .as_deref()
                .filter(|message| !message.trim().is_empty())
                .map(str::to_string)
                .or_else(|| outcome.reason_code.map(|reason| format!("{reason:?}")))
                .unwrap_or_else(|| "not available".to_string());
            format!("{}: {reason}", outcome.packet_kind)
        })
        .take(5)
        .collect();
    if let Some(evidence) = evidence {
        parts.push(format!("evidence:\n{evidence}"));
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn append_llm_diagnostics(
    conn: &Connection,
    client_id: &str,
    limit: usize,
    out: &mut Vec<DebugDiagnosticRow>,
) -> Result<(), StoreError> {
    let mut stmt = conn.prepare(
        "SELECT usage_id, purpose, task_kind, route, provider, model, thinking_level, \
         COALESCE(error_code, 'llm_failed'), error_message, correlation_id, \
         provider_request_id, latency_ms, recorded_at_ms \
         FROM ai_usage_log WHERE client_id = ?1 AND success = 0 \
         ORDER BY recorded_at_ms DESC, usage_id DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![client_id, limit as i64], |row| {
        let usage_id: String = row.get(0)?;
        let purpose: String = row.get(1)?;
        let route: String = row.get(3)?;
        let provider: String = row.get(4)?;
        let model: String = row.get(5)?;
        let thinking_level: Option<String> = row.get(6)?;
        let provider_request_id: Option<String> = row.get(10)?;
        let latency_ms: i64 = row.get(11)?;
        let mut message = row.get::<_, Option<String>>(8)?;
        if message.is_none() {
            message = Some(format!(
                "{provider}/{model} via {route}; latency {latency_ms}ms"
            ));
        }
        if let Some(level) = thinking_level {
            message = Some(match message {
                Some(existing) => format!("{existing}; thinking {level}"),
                None => format!("thinking {level}"),
            });
        }
        Ok(DebugDiagnosticRow {
            diagnostic_id: format!("llm:{usage_id}"),
            source: "llm".to_string(),
            severity: "error".to_string(),
            category: "llm".to_string(),
            entity_kind: Some("ai_usage".to_string()),
            entity_id: Some(usage_id.clone()),
            operation: Some(match row.get::<_, Option<String>>(2)? {
                Some(task_kind) => format!("{purpose}:{task_kind}"),
                None => purpose,
            }),
            error_code: row.get(7)?,
            error_message: message,
            correlation_id: row.get(9)?,
            reference_id: provider_request_id.or(Some(usage_id)),
            occurred_at_ms: row.get::<_, i64>(12)? as u64,
        })
    })?;
    for row in rows {
        out.push(row?);
    }
    Ok(())
}
