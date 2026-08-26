//! Packet proposal run persistence through store_core.

use bos_contracts::packet_proposals::{
    PacketProposalDecisionMode, PacketProposalExecutionMode, PacketProposalKindOutcome,
    PacketProposalRun, PacketProposalRunStatus,
};
use bos_contracts::receipt::ActorKindDto;
use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::store_core::{self, MutationOutcome, MutationRequest, StoreError};

pub const RUN_ENTITY_KIND: &str = "packet_proposal_run";
pub const EVIDENCE_ENTITY_KIND: &str = "packet_proposal_run_evidence";

const RUN_COLUMNS: &str = "run_id, source_kind, source_ref, item_id, \
    resolved_decision_mode, execution_mode, status, candidate_packet_kinds_json, \
    outcomes_json, model, confidence, error_code, created_at_ms, updated_at_ms";
const EVIDENCE_COLUMNS: &str = "evidence_id, run_id, turn_index, tool_name, \
    tool_args_json, result_ref, result_excerpt, created_at_ms";

pub struct NewRun<'a> {
    pub run_id: &'a str,
    pub source_kind: &'a str,
    pub source_ref: &'a str,
    pub item_id: Option<&'a str>,
    pub resolved_decision_mode: PacketProposalDecisionMode,
    pub execution_mode: PacketProposalExecutionMode,
    pub candidate_packet_kinds: &'a [String],
    pub idempotency_key: &'a str,
    pub actor_id: &'a str,
    pub actor_kind: ActorKindDto,
    pub now_ms: u64,
}

pub struct RunUpdate<'a> {
    pub run_id: &'a str,
    pub item_id: Option<&'a str>,
    pub status: PacketProposalRunStatus,
    pub outcomes: &'a [PacketProposalKindOutcome],
    pub model: Option<&'a str>,
    pub confidence: Option<&'a str>,
    pub error_code: Option<&'a str>,
    pub idempotency_key: &'a str,
    pub actor_id: &'a str,
    pub actor_kind: ActorKindDto,
    pub now_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PacketProposalEvidence {
    pub evidence_id: String,
    pub run_id: String,
    pub turn_index: u32,
    pub tool_name: String,
    pub tool_args_json: String,
    pub result_ref: String,
    pub result_excerpt: String,
    pub created_at_ms: u64,
}

pub struct NewEvidence<'a> {
    pub evidence_id: &'a str,
    pub run_id: &'a str,
    pub turn_index: u32,
    pub tool_name: &'a str,
    pub tool_args_json: &'a str,
    pub result_ref: &'a str,
    pub result_excerpt: &'a str,
    pub idempotency_key: &'a str,
    pub actor_id: &'a str,
    pub actor_kind: ActorKindDto,
    pub now_ms: u64,
}

pub fn get_run(
    conn: &Connection,
    client_id: &str,
    run_id: &str,
) -> Result<Option<PacketProposalRun>, StoreError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {RUN_COLUMNS} FROM packet_proposal_runs \
         WHERE client_id = ?1 AND run_id = ?2"
    ))?;
    Ok(stmt
        .query_row(params![client_id, run_id], run_from_row)
        .optional()?)
}

pub fn latest_run_for_source(
    conn: &Connection,
    client_id: &str,
    source_kind: &str,
    source_ref: &str,
) -> Result<Option<PacketProposalRun>, StoreError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {RUN_COLUMNS} FROM packet_proposal_runs \
         WHERE client_id = ?1 AND source_kind = ?2 AND source_ref = ?3 \
         ORDER BY updated_at_ms DESC, run_id DESC LIMIT 1"
    ))?;
    Ok(stmt
        .query_row(params![client_id, source_kind, source_ref], run_from_row)
        .optional()?)
}

pub fn running_runs_for_source(
    conn: &Connection,
    client_id: &str,
    source_kind: &str,
    source_ref: &str,
) -> Result<Vec<PacketProposalRun>, StoreError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {RUN_COLUMNS} FROM packet_proposal_runs \
         WHERE client_id = ?1 AND source_kind = ?2 AND source_ref = ?3 \
           AND status = 'running' \
         ORDER BY updated_at_ms DESC, run_id DESC"
    ))?;
    let rows = stmt.query_map(params![client_id, source_kind, source_ref], run_from_row)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

pub fn has_running_or_terminal_run_for_item(
    conn: &Connection,
    client_id: &str,
    item_id: &str,
) -> Result<bool, StoreError> {
    let found: i64 = conn.query_row(
        "SELECT COUNT(*) FROM packet_proposal_runs \
         WHERE client_id = ?1 AND item_id = ?2 \
           AND status IN ('running', 'completed', 'failed')",
        params![client_id, item_id],
        |row| row.get(0),
    )?;
    Ok(found > 0)
}

pub fn has_running_or_terminal_run_covering_kind(
    conn: &Connection,
    client_id: &str,
    item_id: &str,
    packet_kind: &str,
) -> Result<bool, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT status, candidate_packet_kinds_json, outcomes_json FROM packet_proposal_runs \
         WHERE client_id = ?1 AND item_id = ?2 \
           AND status IN ('running', 'completed')",
    )?;
    let rows = stmt.query_map(params![client_id, item_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (status, candidate_json, outcomes_json) = row?;
        if status == "running" {
            let candidates: Vec<String> = serde_json::from_str(&candidate_json).unwrap_or_default();
            if candidates.iter().any(|kind| kind == packet_kind) {
                return Ok(true);
            }
        } else if status == "completed" {
            let outcomes: Vec<PacketProposalKindOutcome> =
                serde_json::from_str(&outcomes_json).unwrap_or_default();
            if outcomes
                .iter()
                .any(|outcome| outcome.packet_kind == packet_kind)
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

pub fn insert_run(
    conn: &mut Connection,
    client_id: &str,
    run: NewRun<'_>,
) -> Result<MutationOutcome, StoreError> {
    let candidate_packet_kinds_json = serde_json::to_string(run.candidate_packet_kinds)
        .map_err(|err| StoreError::Domain(format!("serialize candidate kinds: {err}")))?;
    let row = PacketProposalRun {
        run_id: run.run_id.to_string(),
        source_kind: run.source_kind.to_string(),
        source_ref: run.source_ref.to_string(),
        item_id: run.item_id.map(str::to_string),
        resolved_decision_mode: run.resolved_decision_mode,
        execution_mode: run.execution_mode,
        status: PacketProposalRunStatus::Running,
        candidate_packet_kinds: run.candidate_packet_kinds.to_vec(),
        outcomes: Vec::new(),
        model: None,
        confidence: None,
        error_code: None,
        created_at_ms: run.now_ms,
        updated_at_ms: run.now_ms,
    };
    let after = serde_json::to_string(&row)
        .map_err(|err| StoreError::Domain(format!("serialize proposal run: {err}")))?;
    let owned_client = client_id.to_string();
    let owned_run_id = run.run_id.to_string();
    let owned_source_kind = run.source_kind.to_string();
    let owned_source_ref = run.source_ref.to_string();
    let owned_item_id = run.item_id.map(str::to_string);
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: RUN_ENTITY_KIND,
            entity_id: run.run_id,
            change_kind: "start",
            actor_id: run.actor_id,
            actor_kind: run.actor_kind,
            expected_revision: None,
            idempotency_key: run.idempotency_key,
            correlation_id: Some(run.source_ref),
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms: run.now_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO packet_proposal_runs \
                 (client_id, run_id, source_kind, source_ref, item_id, \
                  resolved_decision_mode, execution_mode, status, \
                  candidate_packet_kinds_json, outcomes_json, model, confidence, error_code, \
                  created_at_ms, updated_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'running', ?8, '[]', NULL, NULL, NULL, ?9, ?9)",
                params![
                    owned_client,
                    owned_run_id,
                    owned_source_kind,
                    owned_source_ref,
                    owned_item_id,
                    decision_mode_str(run.resolved_decision_mode),
                    execution_mode_str(run.execution_mode),
                    candidate_packet_kinds_json,
                    run.now_ms as i64,
                ],
            )?;
            Ok(())
        },
    )
}

pub fn append_evidence(
    conn: &mut Connection,
    client_id: &str,
    evidence: NewEvidence<'_>,
) -> Result<MutationOutcome, StoreError> {
    let row = PacketProposalEvidence {
        evidence_id: evidence.evidence_id.to_string(),
        run_id: evidence.run_id.to_string(),
        turn_index: evidence.turn_index,
        tool_name: evidence.tool_name.to_string(),
        tool_args_json: evidence.tool_args_json.to_string(),
        result_ref: evidence.result_ref.to_string(),
        result_excerpt: evidence.result_excerpt.to_string(),
        created_at_ms: evidence.now_ms,
    };
    let after = serde_json::to_string(&row)
        .map_err(|err| StoreError::Domain(format!("serialize proposal evidence: {err}")))?;
    let owned_client = client_id.to_string();
    let owned_evidence_id = evidence.evidence_id.to_string();
    let owned_run_id = evidence.run_id.to_string();
    let owned_tool_name = evidence.tool_name.to_string();
    let owned_tool_args = evidence.tool_args_json.to_string();
    let owned_result_ref = evidence.result_ref.to_string();
    let owned_result_excerpt = evidence.result_excerpt.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: EVIDENCE_ENTITY_KIND,
            entity_id: evidence.evidence_id,
            change_kind: "append",
            actor_id: evidence.actor_id,
            actor_kind: evidence.actor_kind,
            expected_revision: None,
            idempotency_key: evidence.idempotency_key,
            correlation_id: Some(evidence.run_id),
            causation_id: Some(evidence.run_id),
            before_json: None,
            after_json: Some(after),
            now_ms: evidence.now_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO packet_proposal_run_evidence \
                 (client_id, evidence_id, run_id, turn_index, tool_name, tool_args_json, \
                  result_ref, result_excerpt, created_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    owned_client,
                    owned_evidence_id,
                    owned_run_id,
                    evidence.turn_index as i64,
                    owned_tool_name,
                    owned_tool_args,
                    owned_result_ref,
                    owned_result_excerpt,
                    evidence.now_ms as i64,
                ],
            )?;
            Ok(())
        },
    )
}

pub fn evidence_for_run(
    conn: &Connection,
    client_id: &str,
    run_id: &str,
) -> Result<Vec<PacketProposalEvidence>, StoreError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {EVIDENCE_COLUMNS} FROM packet_proposal_run_evidence \
         WHERE client_id = ?1 AND run_id = ?2 \
         ORDER BY turn_index ASC, evidence_id ASC",
    ))?;
    let rows = stmt.query_map(params![client_id, run_id], evidence_from_row)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

pub fn update_run(
    conn: &mut Connection,
    client_id: &str,
    update: RunUpdate<'_>,
) -> Result<MutationOutcome, StoreError> {
    let current = get_run(conn, client_id, update.run_id)?
        .ok_or_else(|| StoreError::Domain("packet_proposal_run_not_found".to_string()))?;
    let outcomes_json = serde_json::to_string(update.outcomes)
        .map_err(|err| StoreError::Domain(format!("serialize proposal outcomes: {err}")))?;
    let mut after = current.clone();
    after.item_id = update
        .item_id
        .map(str::to_string)
        .or(current.item_id.clone());
    after.status = update.status;
    after.outcomes = update.outcomes.to_vec();
    after.model = update.model.map(str::to_string).or(current.model.clone());
    after.confidence = update
        .confidence
        .map(str::to_string)
        .or(current.confidence.clone());
    after.error_code = update.error_code.map(str::to_string);
    after.updated_at_ms = update.now_ms;
    let before = serde_json::to_string(&current)
        .map_err(|err| StoreError::Domain(format!("serialize before run: {err}")))?;
    let after_json = serde_json::to_string(&after)
        .map_err(|err| StoreError::Domain(format!("serialize after run: {err}")))?;
    let owned_client = client_id.to_string();
    let owned_run_id = update.run_id.to_string();
    let owned_item_id = after.item_id.clone();
    let owned_model = after.model.clone();
    let owned_confidence = after.confidence.clone();
    let owned_error = after.error_code.clone();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: RUN_ENTITY_KIND,
            entity_id: update.run_id,
            change_kind: "finish",
            actor_id: update.actor_id,
            actor_kind: update.actor_kind,
            expected_revision: None,
            idempotency_key: update.idempotency_key,
            correlation_id: Some(&current.source_ref),
            causation_id: after.item_id.as_deref(),
            before_json: Some(before),
            after_json: Some(after_json),
            now_ms: update.now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE packet_proposal_runs \
                 SET item_id = ?3, status = ?4, outcomes_json = ?5, model = ?6, \
                     confidence = ?7, error_code = ?8, updated_at_ms = ?9 \
                 WHERE client_id = ?1 AND run_id = ?2",
                params![
                    owned_client,
                    owned_run_id,
                    owned_item_id,
                    run_status_str(update.status),
                    outcomes_json,
                    owned_model,
                    owned_confidence,
                    owned_error,
                    update.now_ms as i64,
                ],
            )?;
            Ok(())
        },
    )
}

fn run_from_row(row: &Row<'_>) -> rusqlite::Result<PacketProposalRun> {
    let candidate_packet_kinds_json: String = row.get("candidate_packet_kinds_json")?;
    let outcomes_json: String = row.get("outcomes_json")?;
    Ok(PacketProposalRun {
        run_id: row.get("run_id")?,
        source_kind: row.get("source_kind")?,
        source_ref: row.get("source_ref")?,
        item_id: row.get("item_id")?,
        resolved_decision_mode: decision_mode_from_str(
            &row.get::<_, String>("resolved_decision_mode")?,
        ),
        execution_mode: execution_mode_from_str(&row.get::<_, String>("execution_mode")?),
        status: run_status_from_str(&row.get::<_, String>("status")?),
        candidate_packet_kinds: serde_json::from_str(&candidate_packet_kinds_json)
            .unwrap_or_default(),
        outcomes: serde_json::from_str(&outcomes_json).unwrap_or_default(),
        model: row.get("model")?,
        confidence: row.get("confidence")?,
        error_code: row.get("error_code")?,
        created_at_ms: row.get::<_, i64>("created_at_ms")? as u64,
        updated_at_ms: row.get::<_, i64>("updated_at_ms")? as u64,
    })
}

pub fn decision_mode_str(mode: PacketProposalDecisionMode) -> &'static str {
    match mode {
        PacketProposalDecisionMode::AiDecides => "ai_decides",
        PacketProposalDecisionMode::FillFixed => "fill_fixed",
    }
}

fn decision_mode_from_str(raw: &str) -> PacketProposalDecisionMode {
    match raw {
        "fill_fixed" => PacketProposalDecisionMode::FillFixed,
        _ => PacketProposalDecisionMode::AiDecides,
    }
}

pub fn execution_mode_str(mode: PacketProposalExecutionMode) -> &'static str {
    match mode {
        PacketProposalExecutionMode::BoundedTyped => "bounded_typed",
        PacketProposalExecutionMode::ToolLoopAgentic => "tool_loop_agentic",
    }
}

fn execution_mode_from_str(raw: &str) -> PacketProposalExecutionMode {
    match raw {
        "tool_loop_agentic" => PacketProposalExecutionMode::ToolLoopAgentic,
        _ => PacketProposalExecutionMode::BoundedTyped,
    }
}

pub fn run_status_str(status: PacketProposalRunStatus) -> &'static str {
    match status {
        PacketProposalRunStatus::Running => "running",
        PacketProposalRunStatus::Completed => "completed",
        PacketProposalRunStatus::Failed => "failed",
    }
}

fn run_status_from_str(raw: &str) -> PacketProposalRunStatus {
    match raw {
        "completed" => PacketProposalRunStatus::Completed,
        "failed" => PacketProposalRunStatus::Failed,
        _ => PacketProposalRunStatus::Running,
    }
}

fn evidence_from_row(row: &Row<'_>) -> rusqlite::Result<PacketProposalEvidence> {
    Ok(PacketProposalEvidence {
        evidence_id: row.get("evidence_id")?,
        run_id: row.get("run_id")?,
        turn_index: row.get::<_, i64>("turn_index")? as u32,
        tool_name: row.get("tool_name")?,
        tool_args_json: row.get("tool_args_json")?,
        result_ref: row.get("result_ref")?,
        result_excerpt: row.get("result_excerpt")?,
        created_at_ms: row.get::<_, i64>("created_at_ms")? as u64,
    })
}
