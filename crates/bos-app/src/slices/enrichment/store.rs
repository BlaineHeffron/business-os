//! Store-backed enrichment run diagnostics. Rows are mutable only through
//! store_core so skipped, failed, partial, and completed runs are auditable.

use bos_contracts::enrichment::{
    EnrichmentFieldProposal, EnrichmentPlan, EnrichmentRun, EnrichmentRunStatus,
    EnrichmentTierEvent,
};
use bos_contracts::receipt::ActorKindDto;
use rusqlite::{params, Connection, OptionalExtension, ToSql};

use crate::store_core::{self, MutationOutcome, MutationRequest, StoreError};

pub const RUN_ENTITY_KIND: &str = "enrichment_run";

#[derive(Debug, Clone)]
pub struct StartRun<'a> {
    pub run_id: &'a str,
    pub slice_id: &'a str,
    pub draft_id: &'a str,
    pub item_id: &'a str,
    pub plan: &'a EnrichmentPlan,
    pub created_by: &'a str,
    pub now_ms: u64,
}

#[derive(Debug, Clone)]
pub struct OnDemandKickoff<'a> {
    pub run_id: &'a str,
    pub slice_id: &'a str,
    pub draft_id: &'a str,
    pub item_id: &'a str,
    pub idempotency_key: &'a str,
    pub now_ms: u64,
}

#[derive(Debug, Clone)]
pub struct OnDemandKickoffOutcome {
    pub mutation: MutationOutcome,
    pub run_id: String,
}

#[derive(Debug, Clone)]
pub struct FinishRun<'a> {
    pub run_id: &'a str,
    pub status: EnrichmentRunStatus,
    pub diagnostics: &'a [EnrichmentTierEvent],
    pub proposals: &'a [EnrichmentFieldProposal],
    pub cost_micros: u64,
    pub now_ms: u64,
    pub reason: &'a str,
}

#[derive(Debug, Clone)]
pub struct AppendRunDiagnostics<'a> {
    pub run_id: &'a str,
    pub event_seq: &'a str,
    pub diagnostics: &'a [EnrichmentTierEvent],
    pub proposals: &'a [EnrichmentFieldProposal],
    pub cost_micros: u64,
    pub now_ms: u64,
}

#[derive(Debug, Clone)]
pub struct TransitionRunStatus<'a> {
    pub run_id: &'a str,
    pub status: EnrichmentRunStatus,
    pub now_ms: u64,
    pub reason: &'a str,
}

pub fn start_run(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    input: StartRun<'_>,
) -> Result<MutationOutcome, StoreError> {
    let plan_json = serde_json::to_string(input.plan)
        .map_err(|err| StoreError::Domain(format!("enrichment_plan_invalid:{err}")))?;
    let empty_json = "[]".to_string();
    let idempotency_key = format!("enrichment:{}:start", input.run_id);
    let run_id = input.run_id.to_string();
    let slice_id = input.slice_id.to_string();
    let draft_id = input.draft_id.to_string();
    let item_id = input.item_id.to_string();
    let subject = input.plan.subject.clone();
    let created_by = input.created_by.to_string();
    let now_ms = input.now_ms;
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: RUN_ENTITY_KIND,
            entity_id: input.run_id,
            change_kind: "start",
            actor_id,
            actor_kind: ActorKindDto::System,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            correlation_id: Some(input.item_id),
            causation_id: Some(input.draft_id),
            before_json: None,
            after_json: Some(plan_json.clone()),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT OR IGNORE INTO enrichment_runs \
                 (client_id, run_id, slice_id, draft_id, item_id, subject, status, \
                  started_at_ms, finished_at_ms, plan_json, diagnostics_json, proposals_json, \
                  cost_micros, created_by) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'started', ?7, NULL, ?8, ?9, ?10, 0, ?11)",
                params![
                    client_id,
                    run_id,
                    slice_id,
                    draft_id,
                    item_id,
                    subject,
                    now_ms as i64,
                    plan_json,
                    empty_json,
                    empty_json,
                    created_by,
                ],
            )?;
            Ok(())
        },
    )
}

pub fn record_on_demand_kickoff(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    input: OnDemandKickoff<'_>,
) -> Result<OnDemandKickoffOutcome, StoreError> {
    let after_json = serde_json::json!({
        "run_id": input.run_id,
        "slice_id": input.slice_id,
        "draft_id": input.draft_id,
        "item_id": input.item_id,
    })
    .to_string();
    let mutation = store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: RUN_ENTITY_KIND,
            entity_id: input.run_id,
            change_kind: "on_demand_kickoff",
            actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: None,
            idempotency_key: input.idempotency_key,
            correlation_id: Some(input.item_id),
            causation_id: Some(input.draft_id),
            before_json: None,
            after_json: Some(after_json),
            now_ms: input.now_ms,
        },
        |_tx| Ok(()),
    )?;
    let run_id = match mutation {
        MutationOutcome::ReplayedIdempotent { .. } => {
            kickoff_run_id_for_idempotency_key(conn, client_id, input.idempotency_key)?
                .unwrap_or_else(|| input.run_id.to_string())
        }
        _ => input.run_id.to_string(),
    };
    Ok(OnDemandKickoffOutcome { mutation, run_id })
}

pub fn finish_run(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    input: FinishRun<'_>,
) -> Result<MutationOutcome, StoreError> {
    append_run_diagnostics(
        conn,
        client_id,
        actor_id,
        AppendRunDiagnostics {
            run_id: input.run_id,
            event_seq: input.reason,
            diagnostics: input.diagnostics,
            proposals: input.proposals,
            cost_micros: input.cost_micros,
            now_ms: input.now_ms,
        },
    )?;
    transition_run_status(
        conn,
        client_id,
        actor_id,
        TransitionRunStatus {
            run_id: input.run_id,
            status: input.status,
            now_ms: input.now_ms,
            reason: input.reason,
        },
    )
}

pub fn append_run_diagnostics(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    input: AppendRunDiagnostics<'_>,
) -> Result<MutationOutcome, StoreError> {
    let incoming_diagnostics = input.diagnostics.to_vec();
    let incoming_proposals = input.proposals.to_vec();
    let idempotency_key = format!("enrichment:{}:event:{}", input.run_id, input.event_seq);
    let run_id = input.run_id.to_string();
    let now_ms = input.now_ms;
    let cost_delta = input.cost_micros;
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: RUN_ENTITY_KIND,
            entity_id: input.run_id,
            change_kind: "append_diagnostics",
            actor_id,
            actor_kind: ActorKindDto::System,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            correlation_id: None,
            causation_id: Some(input.run_id),
            before_json: None,
            after_json: Some(
                serde_json::json!({
                    "event_seq": input.event_seq,
                    "event_count": input.diagnostics.len(),
                })
                .to_string(),
            ),
            now_ms,
        },
        move |tx| {
            let Some((diagnostics_json, proposals_json, cost_micros)) = tx
                .query_row(
                    "SELECT diagnostics_json, proposals_json, cost_micros \
                     FROM enrichment_runs WHERE client_id = ?1 AND run_id = ?2",
                    params![client_id, run_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    },
                )
                .optional()?
            else {
                return Err(StoreError::Domain("enrichment_run_not_found".to_string()));
            };
            let mut diagnostics: Vec<EnrichmentTierEvent> =
                serde_json::from_str(&diagnostics_json).unwrap_or_default();
            let mut proposals: Vec<EnrichmentFieldProposal> =
                serde_json::from_str(&proposals_json).unwrap_or_default();
            diagnostics.extend(incoming_diagnostics);
            proposals.extend(incoming_proposals);
            let diagnostics_json = serde_json::to_string(&diagnostics).map_err(|err| {
                StoreError::Domain(format!("enrichment_diagnostics_invalid:{err}"))
            })?;
            let proposals_json = serde_json::to_string(&proposals)
                .map_err(|err| StoreError::Domain(format!("enrichment_proposals_invalid:{err}")))?;
            tx.execute(
                "UPDATE enrichment_runs SET diagnostics_json = ?3, proposals_json = ?4, \
                 cost_micros = ?5 WHERE client_id = ?1 AND run_id = ?2",
                params![
                    client_id,
                    run_id,
                    diagnostics_json,
                    proposals_json,
                    cost_micros + cost_delta as i64,
                ],
            )?;
            Ok(())
        },
    )
}

pub fn transition_run_status(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    input: TransitionRunStatus<'_>,
) -> Result<MutationOutcome, StoreError> {
    let status = status_to_str(input.status).to_string();
    let idempotency_key = format!(
        "enrichment:{}:status:{}:{}",
        input.run_id, status, input.reason
    );
    let run_id = input.run_id.to_string();
    let now_ms = input.now_ms;
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: RUN_ENTITY_KIND,
            entity_id: input.run_id,
            change_kind: "transition_status",
            actor_id,
            actor_kind: ActorKindDto::System,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            correlation_id: None,
            causation_id: Some(input.run_id),
            before_json: None,
            after_json: Some(serde_json::json!({ "status": status }).to_string()),
            now_ms,
        },
        move |tx| {
            let updated = tx.execute(
                "UPDATE enrichment_runs SET status = ?3, finished_at_ms = ?4 \
                 WHERE client_id = ?1 AND run_id = ?2",
                params![client_id, run_id, status, now_ms as i64,],
            )?;
            if updated == 0 {
                return Err(StoreError::Domain("enrichment_run_not_found".to_string()));
            }
            Ok(())
        },
    )
}

pub fn list_runs(
    conn: &Connection,
    client_id: &str,
    slice_id: Option<&str>,
    draft_id: Option<&str>,
    item_id: Option<&str>,
    limit: usize,
) -> Result<Vec<EnrichmentRun>, StoreError> {
    let limit = limit.clamp(1, 200);
    let mut sql = "SELECT run_id, slice_id, draft_id, item_id, subject, status, \
                   started_at_ms, finished_at_ms, plan_json, diagnostics_json, proposals_json, \
                   cost_micros, created_by \
                   FROM enrichment_runs WHERE client_id = ?1"
        .to_string();
    let mut next_param = 2;
    if slice_id.is_some() {
        sql.push_str(&format!(" AND slice_id = ?{next_param}"));
        next_param += 1;
    }
    if draft_id.is_some() {
        sql.push_str(&format!(" AND draft_id = ?{next_param}"));
        next_param += 1;
    }
    if item_id.is_some() {
        sql.push_str(&format!(" AND item_id = ?{next_param}"));
        next_param += 1;
    }
    sql.push_str(&format!(
        " ORDER BY started_at_ms DESC, run_id DESC LIMIT ?{next_param}"
    ));

    let mut values: Vec<Box<dyn ToSql>> = vec![Box::new(client_id.to_string())];
    if let Some(value) = slice_id {
        values.push(Box::new(value.to_string()));
    }
    if let Some(value) = draft_id {
        values.push(Box::new(value.to_string()));
    }
    if let Some(value) = item_id {
        values.push(Box::new(value.to_string()));
    }
    values.push(Box::new(limit as i64));
    let params = values
        .iter()
        .map(|value| value.as_ref() as &dyn ToSql)
        .collect::<Vec<_>>();

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params.as_slice(), run_from_row)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn run_exists(conn: &Connection, client_id: &str, run_id: &str) -> Result<bool, StoreError> {
    let exists = conn
        .query_row(
            "SELECT 1 FROM enrichment_runs WHERE client_id = ?1 AND run_id = ?2 LIMIT 1",
            params![client_id, run_id],
            |_row| Ok(()),
        )
        .optional()?
        .is_some();
    Ok(exists)
}

pub fn last_accepted_proposal_at_ms(
    conn: &Connection,
    client_id: &str,
    slice_id: &str,
    draft_id: &str,
    subject: &str,
    field_ids: &[&str],
) -> Result<Option<u64>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT started_at_ms, finished_at_ms, proposals_json \
         FROM enrichment_runs \
         WHERE client_id = ?1 AND slice_id = ?2 AND draft_id = ?3 AND subject = ?4 \
           AND status IN ('completed', 'partial') \
         ORDER BY COALESCE(finished_at_ms, started_at_ms) DESC, run_id DESC",
    )?;
    let rows = stmt.query_map(params![client_id, slice_id, draft_id, subject], |row| {
        Ok((
            row.get::<_, i64>(0)? as u64,
            row.get::<_, Option<i64>>(1)?.map(|value| value as u64),
            row.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (started_at_ms, finished_at_ms, proposals_json) = row?;
        let proposals: Vec<EnrichmentFieldProposal> =
            serde_json::from_str(&proposals_json).unwrap_or_default();
        if proposals
            .iter()
            .any(|proposal| proposal.accepted && field_ids.contains(&proposal.field_id.as_str()))
        {
            return Ok(Some(finished_at_ms.unwrap_or(started_at_ms)));
        }
    }
    Ok(None)
}

fn kickoff_run_id_for_idempotency_key(
    conn: &Connection,
    client_id: &str,
    idempotency_key: &str,
) -> Result<Option<String>, StoreError> {
    let row = conn
        .query_row(
            "SELECT after_json FROM receipts \
             WHERE client_id = ?1 AND idempotency_key = ?2 \
               AND entity_kind = ?3 AND change_kind = 'on_demand_kickoff' \
               AND outcome = 'applied' \
             ORDER BY created_at_ms ASC, receipt_id ASC LIMIT 1",
            params![client_id, idempotency_key, RUN_ENTITY_KIND],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    let Some(after_json) = row else {
        return Ok(None);
    };
    let value: serde_json::Value = serde_json::from_str(&after_json)
        .map_err(|err| StoreError::Domain(format!("enrichment_kickoff_invalid:{err}")))?;
    Ok(value
        .get("run_id")
        .and_then(|run_id| run_id.as_str())
        .map(str::to_string))
}

fn run_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EnrichmentRun> {
    let plan_json: String = row.get(8)?;
    let diagnostics_json: String = row.get(9)?;
    let proposals_json: String = row.get(10)?;
    let plan = serde_json::from_str(&plan_json).unwrap_or_else(|_| EnrichmentPlan {
        subject: row.get::<_, String>(4).unwrap_or_default(),
        fields: Vec::new(),
        seed_evidence: Vec::new(),
        enabled_tiers: Vec::new(),
        stop_policy: Vec::new(),
    });
    let diagnostics = serde_json::from_str(&diagnostics_json).unwrap_or_default();
    let proposals = serde_json::from_str(&proposals_json).unwrap_or_default();
    Ok(EnrichmentRun {
        run_id: row.get(0)?,
        slice_id: row.get(1)?,
        draft_id: row.get(2)?,
        item_id: row.get(3)?,
        subject: row.get(4)?,
        status: status_from_str(&row.get::<_, String>(5)?),
        started_at_ms: row.get::<_, i64>(6)? as u64,
        finished_at_ms: row.get::<_, Option<i64>>(7)?.map(|v| v as u64),
        plan,
        diagnostics,
        proposals,
        cost_micros: row.get::<_, i64>(11)? as u64,
        created_by: row.get(12)?,
    })
}

fn status_to_str(status: EnrichmentRunStatus) -> &'static str {
    match status {
        EnrichmentRunStatus::Started => "started",
        EnrichmentRunStatus::Completed => "completed",
        EnrichmentRunStatus::Partial => "partial",
        EnrichmentRunStatus::Skipped => "skipped",
        EnrichmentRunStatus::Failed => "failed",
    }
}

fn status_from_str(raw: &str) -> EnrichmentRunStatus {
    match raw {
        "completed" => EnrichmentRunStatus::Completed,
        "partial" => EnrichmentRunStatus::Partial,
        "skipped" => EnrichmentRunStatus::Skipped,
        "failed" => EnrichmentRunStatus::Failed,
        _ => EnrichmentRunStatus::Started,
    }
}
