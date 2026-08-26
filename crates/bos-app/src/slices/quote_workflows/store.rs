use bos_contracts::email_triage::EmailAttachmentRecord;
use bos_contracts::quote_workflows::{
    QuoteDraft, QuoteDraftStatus, QuoteDraftWithRevision, QuoteGuardrailEvaluation,
    QuoteGuardrailStatus, QuoteLineItem, WorkflowRun, WorkflowRunStatus, WorkflowStep,
    WorkflowTraceValue,
};
use bos_contracts::receipt::ActorKindDto;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json::Value;

use crate::outbox::{self, NewOutboxJob};
use crate::store_core::{self, MutationOutcome, MutationRequest, StoreError};

pub const RUN_ENTITY_KIND: &str = "workflow_run";
pub const DRAFT_ENTITY_KIND: &str = "quote_draft";
pub const WORKFLOW: &str = "quote_builder";
pub const VERSION: &str = "v1";
pub const PROVIDER_QUOTE_WORKFLOW: &str = "quote_workflow";
pub const CAPABILITY_STAGE_QUOTE_DRAFT: &str = "stage_quote_draft";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct QuoteWorkflowInput {
    pub source_kind: String,
    pub source_ref: String,
    #[serde(default)]
    pub source_attachments: Vec<EmailAttachmentRecord>,
    pub customer_name: String,
    pub customer_tier: Option<String>,
    pub request_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepRecord {
    pub node: String,
    pub node_kind: String,
    pub input_hash: Option<String>,
    pub output_hash: Option<String>,
    pub decision: Option<String>,
    pub inputs: Vec<WorkflowTraceValue>,
    pub outputs: Vec<WorkflowTraceValue>,
    pub llm_usage_json: Option<String>,
    pub latency_ms: u64,
    pub status: String,
    pub error_code: Option<String>,
}

pub struct Trace<'a> {
    conn: &'a mut Connection,
    client_id: String,
    actor_id: String,
    run_id: String,
    next_step_index: u32,
    last_receipt_id: Option<String>,
}

pub struct TraceStartContext<'a> {
    pub client_id: &'a str,
    pub actor_id: &'a str,
    pub run_id: &'a str,
    pub profile_id: &'a str,
    pub idempotency_key: &'a str,
    pub now_ms: u64,
}

pub struct TraceResumeContext<'a> {
    pub client_id: &'a str,
    pub actor_id: &'a str,
    pub run_id: &'a str,
    pub last_receipt_id: &'a str,
}

pub struct RunFinishContext<'a> {
    pub client_id: &'a str,
    pub actor_id: &'a str,
    pub run_id: &'a str,
    pub causation_id: Option<&'a str>,
    pub now_ms: u64,
}

impl<'a> Trace<'a> {
    pub fn start(
        conn: &'a mut Connection,
        ctx: TraceStartContext<'_>,
        input: &QuoteWorkflowInput,
    ) -> Result<Self, StoreError> {
        let input_snapshot_json = serde_json::to_string(input)
            .map_err(|err| StoreError::Domain(format!("serialize workflow input: {err}")))?;
        let after = serde_json::json!({
            "run_id": ctx.run_id,
            "workflow": WORKFLOW,
            "version": VERSION,
            "profile_id": ctx.profile_id,
            "status": "running",
        })
        .to_string();
        let owned_client = ctx.client_id.to_string();
        let owned_run = ctx.run_id.to_string();
        let owned_profile = ctx.profile_id.to_string();
        let owned_input = input_snapshot_json;
        let build_sha = option_env!("VERGEN_GIT_SHA").map(str::to_string);
        let outcome = store_core::mutate(
            conn,
            MutationRequest {
                client_id: ctx.client_id,
                entity_kind: RUN_ENTITY_KIND,
                entity_id: ctx.run_id,
                change_kind: "start",
                actor_id: ctx.actor_id,
                actor_kind: ActorKindDto::Agent,
                expected_revision: None,
                idempotency_key: ctx.idempotency_key,
                correlation_id: Some(ctx.run_id),
                causation_id: None,
                before_json: None,
                after_json: Some(after),
                now_ms: ctx.now_ms,
            },
            move |tx| {
                tx.execute(
                    "INSERT INTO workflow_runs \
                     (client_id, run_id, workflow, version, profile_id, build_sha, status, \
                      input_snapshot_json, started_at_ms, updated_at_ms) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'running', ?7, ?8, ?8) \
                     ON CONFLICT(client_id, run_id) DO NOTHING",
                    params![
                        owned_client,
                        owned_run,
                        WORKFLOW,
                        VERSION,
                        owned_profile,
                        build_sha,
                        owned_input,
                        ctx.now_ms as i64,
                    ],
                )?;
                Ok(())
            },
        )?;
        Ok(Self {
            conn,
            client_id: ctx.client_id.to_string(),
            actor_id: ctx.actor_id.to_string(),
            run_id: ctx.run_id.to_string(),
            next_step_index: 0,
            last_receipt_id: Some(receipt_id(&outcome)),
        })
    }

    pub fn resume(conn: &'a mut Connection, ctx: TraceResumeContext<'_>) -> Self {
        Self {
            conn,
            client_id: ctx.client_id.to_string(),
            actor_id: ctx.actor_id.to_string(),
            run_id: ctx.run_id.to_string(),
            next_step_index: 0,
            last_receipt_id: Some(ctx.last_receipt_id.to_string()),
        }
    }

    pub fn last_receipt_id(&self) -> Option<&str> {
        self.last_receipt_id.as_deref()
    }

    pub fn step(&mut self, record: StepRecord, now_ms: u64) -> Result<String, StoreError> {
        let index = self.next_step_index;
        self.next_step_index += 1;
        let receipt = insert_step_mutation(
            self.conn,
            StepMutationContext {
                client_id: &self.client_id,
                actor_id: &self.actor_id,
                run_id: &self.run_id,
                step_index: index,
                causation_id: self.last_receipt_id.as_deref(),
                idempotency_key: &format!("{}:{index}", self.run_id),
                now_ms,
            },
            &record,
        )?;
        self.last_receipt_id = Some(receipt.clone());
        Ok(receipt)
    }

    pub fn stage_draft(
        &mut self,
        draft: &QuoteDraft,
        record: StepRecord,
        now_ms: u64,
    ) -> Result<String, StoreError> {
        let index = self.next_step_index;
        self.next_step_index += 1;
        let line_items_json = serde_json::to_string(&draft.line_items)
            .map_err(|err| StoreError::Domain(format!("serialize quote lines: {err}")))?;
        let policy_notes_json = serde_json::to_string(&draft.policy_notes)
            .map_err(|err| StoreError::Domain(format!("serialize quote policy: {err}")))?;
        let guardrails_json = serde_json::to_string(&draft.guardrails)
            .map_err(|err| StoreError::Domain(format!("serialize quote guardrails: {err}")))?;
        let guardrail_config_json = serde_json::to_string(&draft.guardrails.config_snapshot_json)
            .map_err(|err| {
            StoreError::Domain(format!("serialize quote guardrail config: {err}"))
        })?;
        let after = serde_json::to_string(draft)
            .map_err(|err| StoreError::Domain(format!("serialize quote draft: {err}")))?;
        let owned_client = self.client_id.clone();
        let owned_run = self.run_id.clone();
        let owned_draft = draft.clone();
        let owned_record = record.clone();
        let causation_id = self.last_receipt_id.clone();
        let outcome = store_core::mutate_with_receipt_id(
            self.conn,
            MutationRequest {
                client_id: &self.client_id,
                entity_kind: DRAFT_ENTITY_KIND,
                entity_id: &draft.draft_id,
                change_kind: "stage",
                actor_id: &self.actor_id,
                actor_kind: ActorKindDto::Agent,
                expected_revision: None,
                idempotency_key: &format!("{}:{index}", self.run_id),
                correlation_id: Some(&self.run_id),
                causation_id: causation_id.as_deref(),
                before_json: None,
                after_json: Some(after),
                now_ms,
            },
            move |tx, receipt_id| {
                tx.execute(
                    "INSERT INTO quote_drafts \
                     (client_id, draft_id, run_id, source_kind, source_ref, status, \
                      customer_name, summary, line_items_json, subtotal_cents, \
                      policy_notes_json, guardrails_json, guardrail_config_json, \
                      created_at_ms, updated_at_ms) \
                     VALUES (?1, ?2, ?3, ?4, ?5, 'staged', ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13)",
                    params![
                        owned_client,
                        owned_draft.draft_id,
                        owned_draft.run_id,
                        owned_draft.source_kind,
                        owned_draft.source_ref,
                        owned_draft.customer_name,
                        owned_draft.summary,
                        line_items_json,
                        owned_draft.subtotal_cents,
                        policy_notes_json,
                        guardrails_json,
                        guardrail_config_json,
                        now_ms as i64,
                    ],
                )?;
                insert_step_row(
                    tx,
                    &owned_client,
                    &owned_run,
                    index,
                    &owned_record,
                    receipt_id,
                    now_ms,
                )?;
                tx.execute(
                    "UPDATE workflow_runs SET status = 'staged', updated_at_ms = ?3 \
                     WHERE client_id = ?1 AND run_id = ?2",
                    params![owned_client, owned_run, now_ms as i64],
                )?;
                Ok(())
            },
        )?;
        let receipt = receipt_id(&outcome);
        self.last_receipt_id = Some(receipt.clone());
        Ok(receipt)
    }

    pub fn finish(
        &mut self,
        status: WorkflowRunStatus,
        terminal_json: Value,
        now_ms: u64,
    ) -> Result<String, StoreError> {
        let causation_id = self.last_receipt_id.clone();
        let receipt = finish_run(
            self.conn,
            RunFinishContext {
                client_id: &self.client_id,
                actor_id: &self.actor_id,
                run_id: &self.run_id,
                causation_id: causation_id.as_deref(),
                now_ms,
            },
            status,
            terminal_json,
        )?;
        self.last_receipt_id = Some(receipt.clone());
        Ok(receipt)
    }
}

pub fn finish_run(
    conn: &mut Connection,
    ctx: RunFinishContext<'_>,
    status: WorkflowRunStatus,
    terminal_json: Value,
) -> Result<String, StoreError> {
    let status_str = run_status_str(status);
    let terminal = terminal_json.to_string();
    let after = serde_json::json!({
        "run_id": ctx.run_id,
        "status": status_str,
        "terminal": terminal_json,
    })
    .to_string();
    let owned_client = ctx.client_id.to_string();
    let owned_run = ctx.run_id.to_string();
    let outcome = store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: RUN_ENTITY_KIND,
            entity_id: ctx.run_id,
            change_kind: "finish",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Agent,
            expected_revision: None,
            idempotency_key: &format!("{}:finish", ctx.run_id),
            correlation_id: Some(ctx.run_id),
            causation_id: ctx.causation_id,
            before_json: None,
            after_json: Some(after),
            now_ms: ctx.now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE workflow_runs \
                 SET status = ?3, terminal_json = ?4, finished_at_ms = ?5, updated_at_ms = ?5 \
                 WHERE client_id = ?1 AND run_id = ?2",
                params![
                    owned_client,
                    owned_run,
                    status_str,
                    terminal,
                    ctx.now_ms as i64
                ],
            )?;
            Ok(())
        },
    )?;
    Ok(receipt_id(&outcome))
}

struct StepMutationContext<'a> {
    client_id: &'a str,
    actor_id: &'a str,
    run_id: &'a str,
    step_index: u32,
    causation_id: Option<&'a str>,
    idempotency_key: &'a str,
    now_ms: u64,
}

fn insert_step_mutation(
    conn: &mut Connection,
    ctx: StepMutationContext<'_>,
    record: &StepRecord,
) -> Result<String, StoreError> {
    let after = serde_json::json!({
        "run_id": ctx.run_id,
        "step_index": ctx.step_index,
        "node": record.node,
        "status": record.status,
    })
    .to_string();
    let owned_client = ctx.client_id.to_string();
    let owned_run = ctx.run_id.to_string();
    let owned_record = record.clone();
    let outcome = store_core::mutate_with_receipt_id(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: RUN_ENTITY_KIND,
            entity_id: ctx.run_id,
            change_kind: "step",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Agent,
            expected_revision: None,
            idempotency_key: ctx.idempotency_key,
            correlation_id: Some(ctx.run_id),
            causation_id: ctx.causation_id,
            before_json: None,
            after_json: Some(after),
            now_ms: ctx.now_ms,
        },
        move |tx, receipt_id| {
            insert_step_row(
                tx,
                &owned_client,
                &owned_run,
                ctx.step_index,
                &owned_record,
                receipt_id,
                ctx.now_ms,
            )?;
            Ok(())
        },
    )?;
    Ok(receipt_id(&outcome))
}

fn insert_step_row(
    tx: &rusqlite::Transaction<'_>,
    client_id: &str,
    run_id: &str,
    step_index: u32,
    record: &StepRecord,
    receipt_id: &str,
    now_ms: u64,
) -> Result<(), StoreError> {
    let inputs_json = serde_json::to_string(&record.inputs)
        .map_err(|err| StoreError::Domain(format!("serialize workflow step inputs: {err}")))?;
    let outputs_json = serde_json::to_string(&record.outputs)
        .map_err(|err| StoreError::Domain(format!("serialize workflow step outputs: {err}")))?;
    tx.execute(
        "INSERT INTO workflow_steps \
         (client_id, run_id, step_index, node, node_kind, input_hash, output_hash, \
          decision, input_values_json, output_values_json, llm_usage_json, latency_ms, \
          status, error_code, receipt_id, created_at_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        params![
            client_id,
            run_id,
            step_index as i64,
            record.node,
            record.node_kind,
            record.input_hash,
            record.output_hash,
            record.decision,
            inputs_json,
            outputs_json,
            record.llm_usage_json,
            record.latency_ms as i64,
            record.status,
            record.error_code,
            receipt_id,
            now_ms as i64,
        ],
    )?;
    Ok(())
}

pub use crate::slices::mutation_context::MutationContext as DraftActionContext;

pub fn approve_draft(
    conn: &mut Connection,
    ctx: DraftActionContext<'_>,
    draft_id: &str,
    job: &NewOutboxJob,
) -> Result<MutationOutcome, StoreError> {
    let draft = require_draft(conn, ctx.client_id, draft_id)?;
    if draft.draft.status != QuoteDraftStatus::Staged {
        return Err(StoreError::Domain("quote_draft_not_staged".to_string()));
    }
    let owned_client = ctx.client_id.to_string();
    let owned_draft = draft_id.to_string();
    let owned_run = draft.draft.run_id.clone();
    let owned_job = job.clone();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: DRAFT_ENTITY_KIND,
            entity_id: draft_id,
            change_kind: "approve",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: Some(&draft.draft.run_id),
            causation_id: None,
            before_json: None,
            after_json: Some(
                serde_json::json!({
                    "status": "approved",
                    "outbox_job_id": job.job_id,
                })
                .to_string(),
            ),
            now_ms: ctx.now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE quote_drafts \
                 SET status = 'approved', outbox_job_id = ?3, updated_at_ms = ?4 \
                 WHERE client_id = ?1 AND draft_id = ?2 AND status = 'staged'",
                params![
                    owned_client,
                    owned_draft,
                    owned_job.job_id,
                    ctx.now_ms as i64
                ],
            )?;
            tx.execute(
                "UPDATE workflow_runs SET status = 'approved', updated_at_ms = ?3 \
                 WHERE client_id = ?1 AND run_id = ?2",
                params![owned_client, owned_run, ctx.now_ms as i64],
            )?;
            outbox::enqueue_within(tx, &owned_client, &owned_job, ctx.now_ms)?;
            Ok(())
        },
    )
}

pub fn reject_draft(
    conn: &mut Connection,
    ctx: DraftActionContext<'_>,
    draft_id: &str,
) -> Result<MutationOutcome, StoreError> {
    let draft = require_draft(conn, ctx.client_id, draft_id)?;
    if draft.draft.status != QuoteDraftStatus::Staged {
        return Err(StoreError::Domain("quote_draft_not_staged".to_string()));
    }
    let owned_client = ctx.client_id.to_string();
    let owned_draft = draft_id.to_string();
    let owned_run = draft.draft.run_id.clone();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: DRAFT_ENTITY_KIND,
            entity_id: draft_id,
            change_kind: "reject",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: Some(&draft.draft.run_id),
            causation_id: None,
            before_json: None,
            after_json: Some(serde_json::json!({ "status": "rejected" }).to_string()),
            now_ms: ctx.now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE quote_drafts SET status = 'rejected', updated_at_ms = ?3 \
                 WHERE client_id = ?1 AND draft_id = ?2 AND status = 'staged'",
                params![owned_client, owned_draft, ctx.now_ms as i64],
            )?;
            tx.execute(
                "UPDATE workflow_runs SET status = 'rejected', updated_at_ms = ?3 \
                 WHERE client_id = ?1 AND run_id = ?2",
                params![owned_client, owned_run, ctx.now_ms as i64],
            )?;
            Ok(())
        },
    )
}

pub fn get_run(
    conn: &Connection,
    client_id: &str,
    run_id: &str,
) -> Result<Option<WorkflowRun>, StoreError> {
    conn.query_row(
        "SELECT run_id, workflow, version, build_sha, status, input_snapshot_json, \
         terminal_json, started_at_ms, finished_at_ms, updated_at_ms, profile_id \
         FROM workflow_runs WHERE client_id = ?1 AND run_id = ?2",
        params![client_id, run_id],
        run_from_row,
    )
    .optional()
    .map_err(Into::into)
}

pub fn steps_for_run(
    conn: &Connection,
    client_id: &str,
    run_id: &str,
) -> Result<Vec<WorkflowStep>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT run_id, step_index, node, node_kind, input_hash, output_hash, decision, \
         input_values_json, output_values_json, llm_usage_json, latency_ms, status, error_code, \
         receipt_id, created_at_ms \
         FROM workflow_steps WHERE client_id = ?1 AND run_id = ?2 ORDER BY step_index ASC",
    )?;
    let rows = stmt.query_map(params![client_id, run_id], step_from_row)?;
    let mut steps = Vec::new();
    for row in rows {
        steps.push(row?);
    }
    Ok(steps)
}

pub fn draft_for_run(
    conn: &Connection,
    client_id: &str,
    run_id: &str,
) -> Result<Option<QuoteDraftWithRevision>, StoreError> {
    let row = conn
        .query_row(
            &format!(
                "SELECT {} FROM quote_drafts d \
                 LEFT JOIN entity_revisions er \
                   ON er.client_id = d.client_id AND er.entity_kind = ?2 AND er.entity_id = d.draft_id \
                 WHERE d.client_id = ?1 AND d.run_id = ?3",
                draft_columns()
            ),
            params![client_id, DRAFT_ENTITY_KIND, run_id],
            draft_from_row,
        )
        .optional()?;
    row.map(|draft| attach_job_summary(conn, client_id, draft))
        .transpose()
}

pub fn get_draft(
    conn: &Connection,
    client_id: &str,
    draft_id: &str,
) -> Result<Option<QuoteDraftWithRevision>, StoreError> {
    let row = conn
        .query_row(
            &format!(
                "SELECT {} FROM quote_drafts d \
                 LEFT JOIN entity_revisions er \
                   ON er.client_id = d.client_id AND er.entity_kind = ?2 AND er.entity_id = d.draft_id \
                 WHERE d.client_id = ?1 AND d.draft_id = ?3",
                draft_columns()
            ),
            params![client_id, DRAFT_ENTITY_KIND, draft_id],
            draft_from_row,
        )
        .optional()?;
    row.map(|draft| attach_job_summary(conn, client_id, draft))
        .transpose()
}

fn require_draft(
    conn: &Connection,
    client_id: &str,
    draft_id: &str,
) -> Result<QuoteDraftWithRevision, StoreError> {
    get_draft(conn, client_id, draft_id)?
        .ok_or_else(|| StoreError::Domain("quote_draft_not_found".to_string()))
}

fn attach_job_summary(
    conn: &Connection,
    client_id: &str,
    mut draft: QuoteDraftWithRevision,
) -> Result<QuoteDraftWithRevision, StoreError> {
    if let Some(job_id) = draft.draft.outbox_job_id.as_deref() {
        draft.outbox_job = outbox::job_summary(conn, client_id, job_id)?;
    }
    Ok(draft)
}

fn draft_columns() -> &'static str {
    "d.draft_id, d.run_id, d.source_kind, d.source_ref, d.status, d.customer_name, \
     d.summary, d.line_items_json, d.subtotal_cents, d.policy_notes_json, d.outbox_job_id, \
     d.created_at_ms, d.updated_at_ms, COALESCE(er.revision, 0), d.guardrails_json, \
     d.guardrail_config_json"
}

fn draft_from_row(row: &Row<'_>) -> rusqlite::Result<QuoteDraftWithRevision> {
    let guardrails_raw: Option<String> = row.get(14)?;
    let config_raw: Option<String> = row.get(15)?;
    let guardrails = guardrails_from_json(guardrails_raw.as_deref(), config_raw.as_deref());
    Ok(QuoteDraftWithRevision {
        draft: QuoteDraft {
            draft_id: row.get(0)?,
            run_id: row.get(1)?,
            source_kind: row.get(2)?,
            source_ref: row.get(3)?,
            status: draft_status_from_str(&row.get::<_, String>(4)?),
            customer_name: row.get(5)?,
            summary: row.get(6)?,
            line_items: serde_json::from_str(&row.get::<_, String>(7)?).unwrap_or_default(),
            subtotal_cents: row.get(8)?,
            policy_notes: serde_json::from_str(&row.get::<_, String>(9)?).unwrap_or_default(),
            guardrails,
            outbox_job_id: row.get(10)?,
            created_at_ms: row.get::<_, i64>(11)? as u64,
            updated_at_ms: row.get::<_, i64>(12)? as u64,
        },
        revision: row.get::<_, i64>(13)? as u64,
        outbox_job: None,
    })
}

fn run_from_row(row: &Row<'_>) -> rusqlite::Result<WorkflowRun> {
    let input_raw: String = row.get(5)?;
    let terminal_raw: Option<String> = row.get(6)?;
    Ok(WorkflowRun {
        run_id: row.get(0)?,
        workflow: row.get(1)?,
        version: row.get(2)?,
        profile_id: row.get(10)?,
        build_sha: row.get(3)?,
        status: run_status_from_str(&row.get::<_, String>(4)?),
        input_snapshot_json: serde_json::from_str(&input_raw).unwrap_or(Value::Null),
        terminal_json: terminal_raw.and_then(|raw| serde_json::from_str(&raw).ok()),
        started_at_ms: row.get::<_, i64>(7)? as u64,
        finished_at_ms: row.get::<_, Option<i64>>(8)?.map(|v| v as u64),
        updated_at_ms: row.get::<_, i64>(9)? as u64,
    })
}

fn step_from_row(row: &Row<'_>) -> rusqlite::Result<WorkflowStep> {
    let inputs_raw: Option<String> = row.get(7)?;
    let outputs_raw: Option<String> = row.get(8)?;
    let llm_usage_raw: Option<String> = row.get(9)?;
    Ok(WorkflowStep {
        run_id: row.get(0)?,
        step_index: row.get::<_, i64>(1)? as u32,
        node: row.get(2)?,
        node_kind: row.get(3)?,
        input_hash: row.get(4)?,
        output_hash: row.get(5)?,
        decision: row.get(6)?,
        inputs: inputs_raw
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default(),
        outputs: outputs_raw
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default(),
        llm_usage: llm_usage_raw.and_then(|raw| serde_json::from_str(&raw).ok()),
        latency_ms: row.get::<_, i64>(10)? as u64,
        status: row.get(11)?,
        error_code: row.get(12)?,
        receipt_id: row.get(13)?,
        created_at_ms: row.get::<_, i64>(14)? as u64,
    })
}

pub fn receipt_id(outcome: &MutationOutcome) -> String {
    match outcome {
        MutationOutcome::Applied { receipt_id, .. }
        | MutationOutcome::ReplayedIdempotent { receipt_id, .. }
        | MutationOutcome::RevisionConflict { receipt_id, .. } => receipt_id.clone(),
    }
}

fn draft_status_from_str(raw: &str) -> QuoteDraftStatus {
    match raw {
        "approved" => QuoteDraftStatus::Approved,
        "rejected" => QuoteDraftStatus::Rejected,
        _ => QuoteDraftStatus::Staged,
    }
}

fn run_status_from_str(raw: &str) -> WorkflowRunStatus {
    match raw {
        "staged" => WorkflowRunStatus::Staged,
        "needs_operator_input" => WorkflowRunStatus::NeedsOperatorInput,
        "failed" => WorkflowRunStatus::Failed,
        "approved" => WorkflowRunStatus::Approved,
        "rejected" => WorkflowRunStatus::Rejected,
        _ => WorkflowRunStatus::Running,
    }
}

fn run_status_str(status: WorkflowRunStatus) -> &'static str {
    match status {
        WorkflowRunStatus::Running => "running",
        WorkflowRunStatus::Staged => "staged",
        WorkflowRunStatus::NeedsOperatorInput => "needs_operator_input",
        WorkflowRunStatus::Failed => "failed",
        WorkflowRunStatus::Approved => "approved",
        WorkflowRunStatus::Rejected => "rejected",
    }
}

pub fn quote_payload(draft: &QuoteDraft) -> Result<String, StoreError> {
    serde_json::to_string(draft)
        .map_err(|err| StoreError::Domain(format!("serialize quote payload: {err}")))
}

fn guardrails_from_json(
    guardrails_raw: Option<&str>,
    config_raw: Option<&str>,
) -> QuoteGuardrailEvaluation {
    guardrails_raw
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_else(|| QuoteGuardrailEvaluation {
            status: QuoteGuardrailStatus::WithinGuardrails,
            config_hash: "unconfigured".to_string(),
            findings: Vec::new(),
            approval_routes: Vec::new(),
            config_snapshot_json: config_raw
                .and_then(|raw| serde_json::from_str(raw).ok())
                .unwrap_or(serde_json::Value::Null),
        })
}

pub fn draft_from_interpretation(
    run_id: &str,
    input: &QuoteWorkflowInput,
    line_items: Vec<QuoteLineItem>,
    guardrails: QuoteGuardrailEvaluation,
    policy_notes: Vec<String>,
    summary: String,
    now_ms: u64,
) -> QuoteDraft {
    let subtotal_cents = line_items.iter().map(|line| line.total_cents).sum();
    QuoteDraft {
        draft_id: format!("qd_{run_id}"),
        run_id: run_id.to_string(),
        source_kind: input.source_kind.clone(),
        source_ref: input.source_ref.clone(),
        status: QuoteDraftStatus::Staged,
        customer_name: input.customer_name.clone(),
        summary,
        line_items,
        subtotal_cents,
        guardrails,
        policy_notes,
        outbox_job_id: None,
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
    }
}
