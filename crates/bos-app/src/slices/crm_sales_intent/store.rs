//! CRM sales-intent draft persistence through store_core.

use bos_contracts::crm_sales_intent::{
    CrmSalesIntentDraft, CrmSalesIntentDraftStatus, CrmSalesIntentDraftWithRevision,
    CrmSalesIntentProviderTarget,
};
use bos_contracts::follow_up_tasks::TaskRecord;
use bos_contracts::receipt::ActorKindDto;
use rusqlite::{params, Connection, Row};

use crate::http::OperatorScope;
use crate::outbox::{self, NewOutboxJob};
use crate::slices::draft_store::{
    self, DraftStore, DraftTableSpec, ScopedDraftStore, ScopedStatusDraftStore,
};
use crate::store_core::{self, MutationOutcome, MutationRequest, StoreError};

pub const DRAFT_ENTITY_KIND: &str = "crm_sales_intent_draft";
const APPROVE_SQL: &str =
    "UPDATE crm_sales_intent_drafts SET status = 'approved', outbox_job_id = ?3, \
     updated_at_ms = ?4 WHERE client_id = ?1 AND draft_id = ?2";
const REJECT_SQL: &str =
    "UPDATE crm_sales_intent_drafts SET status = 'rejected', updated_at_ms = ?3 \
     WHERE client_id = ?1 AND draft_id = ?2";
const DRAFT_TABLE: DraftTableSpec = DraftTableSpec {
    table: SalesIntentDraftStore::TABLE,
    entity_kind: DRAFT_ENTITY_KIND,
    not_found_code: SalesIntentDraftStore::NOT_FOUND,
    not_staged_code: "crm_sales_intent_not_staged",
    approve_sql: APPROVE_SQL,
    reject_sql: REJECT_SQL,
};

const DRAFT_COLUMNS: &str =
    "d.draft_id, d.item_id, d.source_kind, d.source_ref, d.source_user_id, \
     d.status, d.company_name, d.contact_name, d.contact_email, d.lead_title, d.intent_summary, \
     d.rationale, d.qualification_status, d.next_step_text, d.follow_up_due_date, \
     d.provider_target, d.create_businessos_task, d.provenance_json, d.model, d.confidence, \
     d.outbox_job_id, d.created_at_ms, d.updated_at_ms, COALESCE(er.revision, 0) AS revision";

fn draft_from_row(row: &Row<'_>) -> rusqlite::Result<CrmSalesIntentDraftWithRevision> {
    Ok(CrmSalesIntentDraftWithRevision {
        draft: CrmSalesIntentDraft {
            draft_id: row.get("draft_id")?,
            item_id: row.get("item_id")?,
            source_kind: row.get("source_kind")?,
            source_ref: row.get("source_ref")?,
            source_user_id: row.get("source_user_id")?,
            status: status_from_str(&row.get::<_, String>("status")?),
            company_name: row.get("company_name")?,
            contact_name: row.get("contact_name")?,
            contact_email: row.get("contact_email")?,
            lead_title: row.get("lead_title")?,
            intent_summary: row.get("intent_summary")?,
            rationale: row.get("rationale")?,
            qualification_status: row.get("qualification_status")?,
            next_step_text: row.get("next_step_text")?,
            follow_up_due_date: row.get("follow_up_due_date")?,
            provider_target: target_from_str(&row.get::<_, String>("provider_target")?),
            create_businessos_task: row.get::<_, i64>("create_businessos_task")? != 0,
            provenance: serde_json::from_str(&row.get::<_, String>("provenance_json")?)
                .unwrap_or_default(),
            model: row.get("model")?,
            confidence: row.get("confidence")?,
            outbox_job_id: row.get("outbox_job_id")?,
            created_at_ms: row.get::<_, i64>("created_at_ms")? as u64,
            updated_at_ms: row.get::<_, i64>("updated_at_ms")? as u64,
        },
        revision: row.get::<_, i64>("revision")? as u64,
        outbox_job: None,
    })
}

fn attach_job_summary(
    conn: &Connection,
    client_id: &str,
    mut entry: CrmSalesIntentDraftWithRevision,
) -> Result<CrmSalesIntentDraftWithRevision, StoreError> {
    if let Some(job_id) = entry.draft.outbox_job_id.as_deref() {
        entry.outbox_job = outbox::job_summary(conn, client_id, job_id)?;
    }
    Ok(entry)
}

struct SalesIntentDraftStore;

impl DraftStore for SalesIntentDraftStore {
    type WithRevision = CrmSalesIntentDraftWithRevision;

    const TABLE: &'static str = "crm_sales_intent_drafts";
    const COLUMNS: &'static str = DRAFT_COLUMNS;
    const ENTITY_KIND: &'static str = DRAFT_ENTITY_KIND;
    const NOT_FOUND: &'static str = "crm_sales_intent_not_found";

    fn map_row(row: &Row<'_>) -> rusqlite::Result<Self::WithRevision> {
        draft_from_row(row)
    }

    fn attach(
        conn: &Connection,
        client_id: &str,
        entry: Self::WithRevision,
    ) -> Result<Self::WithRevision, StoreError> {
        attach_job_summary(conn, client_id, entry)
    }
}

impl ScopedDraftStore for SalesIntentDraftStore {
    fn source_user_id(entry: &Self::WithRevision) -> Option<&str> {
        entry.draft.source_user_id.as_deref()
    }
}

impl ScopedStatusDraftStore for SalesIntentDraftStore {
    fn map_status(row: &Row<'_>) -> rusqlite::Result<(String, Option<String>)> {
        Ok((row.get(0)?, row.get(1)?))
    }
}

pub fn active_draft_for_item(
    conn: &Connection,
    client_id: &str,
    item_id: &str,
) -> Result<Option<CrmSalesIntentDraftWithRevision>, StoreError> {
    draft_store::active_draft_for_item::<SalesIntentDraftStore>(conn, client_id, item_id)
}

pub fn get_draft(
    conn: &Connection,
    client_id: &str,
    draft_id: &str,
    scope: &OperatorScope,
) -> Result<Option<CrmSalesIntentDraftWithRevision>, StoreError> {
    draft_store::get_draft_scoped::<SalesIntentDraftStore>(conn, client_id, draft_id, scope)
}

pub fn list_drafts(
    conn: &Connection,
    client_id: &str,
    item_id: Option<&str>,
    limit: usize,
    scope: &OperatorScope,
) -> Result<Vec<CrmSalesIntentDraftWithRevision>, StoreError> {
    draft_store::list_drafts_scoped::<SalesIntentDraftStore>(conn, client_id, item_id, limit, scope)
}

pub fn count_drafts_for_item(
    conn: &Connection,
    client_id: &str,
    item_id: &str,
) -> Result<u64, StoreError> {
    draft_store::count_drafts_for_item::<SalesIntentDraftStore>(conn, client_id, item_id)
}

pub fn staged_item_ids(conn: &Connection, client_id: &str) -> Result<Vec<String>, StoreError> {
    draft_store::staged_item_ids::<SalesIntentDraftStore>(conn, client_id)
}

pub fn insert_draft(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    draft: &CrmSalesIntentDraft,
    idempotency_key: &str,
) -> Result<MutationOutcome, StoreError> {
    validate_draft(draft)?;
    let after = serde_json::to_string(draft)
        .map_err(|err| StoreError::Domain(format!("serialize draft: {err}")))?;
    let provenance_json = serde_json::to_string(&draft.provenance)
        .map_err(|err| StoreError::Domain(format!("serialize provenance: {err}")))?;
    let row = draft.clone();
    let owned_client = client_id.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: DRAFT_ENTITY_KIND,
            entity_id: &draft.draft_id,
            change_kind: "stage",
            actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: None,
            idempotency_key,
            correlation_id: Some(&draft.item_id),
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms: draft.created_at_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO crm_sales_intent_drafts \
                 (client_id, draft_id, item_id, source_kind, source_ref, source_user_id, status, \
                  company_name, contact_name, contact_email, lead_title, intent_summary, rationale, \
                  qualification_status, next_step_text, follow_up_due_date, provider_target, \
                  create_businessos_task, provenance_json, model, confidence, created_at_ms, updated_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'staged', ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, \
                         ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?21)",
                params![
                    owned_client,
                    row.draft_id,
                    row.item_id,
                    row.source_kind,
                    row.source_ref,
                    row.source_user_id,
                    row.company_name,
                    row.contact_name,
                    row.contact_email,
                    row.lead_title,
                    row.intent_summary,
                    row.rationale,
                    row.qualification_status,
                    row.next_step_text,
                    row.follow_up_due_date,
                    target_to_str(row.provider_target),
                    if row.create_businessos_task { 1_i64 } else { 0_i64 },
                    provenance_json,
                    row.model,
                    row.confidence,
                    row.created_at_ms as i64,
                ],
            )
            .map_err(|err| match err {
                rusqlite::Error::SqliteFailure(code, _)
                    if code.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    StoreError::Domain("crm_sales_intent_already_active".to_string())
                }
                other => other.into(),
            })?;
            Ok(())
        },
    )
}

pub use crate::slices::mutation_context::ScopedMutationContext as DraftActionContext;

pub fn approve_draft(
    conn: &mut Connection,
    ctx: DraftActionContext<'_>,
    draft_id: &str,
    job: &NewOutboxJob,
    task: Option<&TaskRecord>,
) -> Result<MutationOutcome, StoreError> {
    let (current, source_user_id) = require_status(conn, ctx.client_id, draft_id)?;
    ctx.scope.require_source_user(source_user_id.as_deref())?;
    if current != "staged" {
        return Err(StoreError::Domain(format!(
            "crm_sales_intent_not_staged:{current}"
        )));
    }
    let owned_client = ctx.client_id.to_string();
    let owned_draft = draft_id.to_string();
    let owned_job = job.clone();
    let owned_task = task.cloned();
    let now_ms = ctx.now_ms;
    let after_json = match task {
        Some(task) => format!(
            "{{\"status\":\"approved\",\"outbox_job_id\":\"{}\",\"follow_up_task_id\":\"{}\"}}",
            job.job_id, task.task_id
        ),
        None => format!(
            "{{\"status\":\"approved\",\"outbox_job_id\":\"{}\"}}",
            job.job_id
        ),
    };
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
            correlation_id: Some(&job.job_id),
            causation_id: None,
            before_json: Some("{\"status\":\"staged\"}".to_string()),
            after_json: Some(after_json),
            now_ms,
        },
        move |tx| {
            tx.execute(
                APPROVE_SQL,
                params![owned_client, owned_draft, owned_job.job_id, now_ms as i64],
            )?;
            outbox::enqueue_within(tx, &owned_client, &owned_job, now_ms)?;
            if let Some(task) = owned_task {
                crate::slices::follow_up_tasks::store::insert_task_within(
                    tx,
                    &owned_client,
                    &task,
                    now_ms,
                )?;
            }
            Ok(())
        },
    )
}

pub fn reject_draft(
    conn: &mut Connection,
    ctx: DraftActionContext<'_>,
    draft_id: &str,
) -> Result<MutationOutcome, StoreError> {
    draft_store::reject(conn, ctx.into(), &DRAFT_TABLE, draft_id)
}

pub fn update_draft(
    conn: &mut Connection,
    ctx: DraftActionContext<'_>,
    draft_id: &str,
    draft: &CrmSalesIntentDraft,
) -> Result<MutationOutcome, StoreError> {
    let (current, source_user_id) = require_status(conn, ctx.client_id, draft_id)?;
    ctx.scope.require_source_user(source_user_id.as_deref())?;
    if current != "staged" {
        return Err(StoreError::Domain(format!(
            "crm_sales_intent_not_staged:{current}"
        )));
    }
    validate_draft(draft)?;
    let before: serde_json::Value = conn.query_row(
        "SELECT company_name, contact_name, contact_email, lead_title, intent_summary, rationale, \
         qualification_status, next_step_text, follow_up_due_date, provider_target, \
         create_businessos_task FROM crm_sales_intent_drafts WHERE client_id = ?1 AND draft_id = ?2",
        params![ctx.client_id, draft_id],
        |row| {
            Ok(serde_json::json!({
                "company_name": row.get::<_, Option<String>>(0)?,
                "contact_name": row.get::<_, Option<String>>(1)?,
                "contact_email": row.get::<_, Option<String>>(2)?,
                "lead_title": row.get::<_, String>(3)?,
                "intent_summary": row.get::<_, String>(4)?,
                "rationale": row.get::<_, String>(5)?,
                "qualification_status": row.get::<_, String>(6)?,
                "next_step_text": row.get::<_, String>(7)?,
                "follow_up_due_date": row.get::<_, Option<String>>(8)?,
                "provider_target": row.get::<_, String>(9)?,
                "create_businessos_task": row.get::<_, i64>(10)? != 0,
            }))
        },
    )?;
    let after = serde_json::to_string(draft)
        .map_err(|err| StoreError::Domain(format!("serialize draft: {err}")))?;
    let owned_client = ctx.client_id.to_string();
    let owned_draft_id = draft_id.to_string();
    let row = draft.clone();
    let now_ms = ctx.now_ms;
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: DRAFT_ENTITY_KIND,
            entity_id: draft_id,
            change_kind: "edit",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: Some(&draft.item_id),
            causation_id: None,
            before_json: Some(before.to_string()),
            after_json: Some(after),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE crm_sales_intent_drafts SET company_name = ?3, contact_name = ?4, \
                 contact_email = ?5, lead_title = ?6, intent_summary = ?7, rationale = ?8, \
                 qualification_status = ?9, next_step_text = ?10, follow_up_due_date = ?11, \
                 provider_target = ?12, create_businessos_task = ?13, updated_at_ms = ?14 \
                 WHERE client_id = ?1 AND draft_id = ?2",
                params![
                    owned_client,
                    owned_draft_id,
                    row.company_name,
                    row.contact_name,
                    row.contact_email,
                    row.lead_title,
                    row.intent_summary,
                    row.rationale,
                    row.qualification_status,
                    row.next_step_text,
                    row.follow_up_due_date,
                    target_to_str(row.provider_target),
                    if row.create_businessos_task {
                        1_i64
                    } else {
                        0_i64
                    },
                    now_ms as i64,
                ],
            )?;
            Ok(())
        },
    )
}

fn require_status(
    conn: &Connection,
    client_id: &str,
    draft_id: &str,
) -> Result<(String, Option<String>), StoreError> {
    draft_store::require_status_scoped::<SalesIntentDraftStore>(conn, client_id, draft_id)
}

pub(crate) fn validate_draft(draft: &CrmSalesIntentDraft) -> Result<(), StoreError> {
    if draft.lead_title.trim().is_empty() {
        return Err(StoreError::Domain(
            "crm_sales_intent_title_required".to_string(),
        ));
    }
    if draft.intent_summary.trim().is_empty() {
        return Err(StoreError::Domain(
            "crm_sales_intent_summary_required".to_string(),
        ));
    }
    if draft.next_step_text.trim().is_empty() {
        return Err(StoreError::Domain(
            "crm_sales_intent_next_step_required".to_string(),
        ));
    }
    if let Some(email) = draft.contact_email.as_deref() {
        if !email.contains('@') || email.contains(char::is_whitespace) {
            return Err(StoreError::Domain(
                "crm_sales_intent_contact_email_invalid".to_string(),
            ));
        }
    }
    if let Some(date) = draft.follow_up_due_date.as_deref() {
        if !super::service::is_iso_date(date) {
            return Err(StoreError::Domain(
                "crm_sales_intent_follow_up_due_date_invalid".to_string(),
            ));
        }
    }
    Ok(())
}

fn status_from_str(raw: &str) -> CrmSalesIntentDraftStatus {
    match raw {
        "approved" => CrmSalesIntentDraftStatus::Approved,
        "rejected" => CrmSalesIntentDraftStatus::Rejected,
        _ => CrmSalesIntentDraftStatus::Staged,
    }
}

pub(crate) fn target_from_str(raw: &str) -> CrmSalesIntentProviderTarget {
    match raw {
        "deal" => CrmSalesIntentProviderTarget::Deal,
        "task_only" => CrmSalesIntentProviderTarget::TaskOnly,
        _ => CrmSalesIntentProviderTarget::Lead,
    }
}

pub(crate) fn target_to_str(target: CrmSalesIntentProviderTarget) -> &'static str {
    match target {
        CrmSalesIntentProviderTarget::Lead => "lead",
        CrmSalesIntentProviderTarget::Deal => "deal",
        CrmSalesIntentProviderTarget::TaskOnly => "task_only",
    }
}
