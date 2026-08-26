//! Email reply draft persistence through store_core. Approval enqueues the
//! Gmail draft-create outbox job inside the SAME mutation transaction.

use bos_contracts::calendar_drafts::DraftFieldProvenance;
use bos_contracts::email_drafts::{
    EmailDraftFollowUpRequest, EmailDraftStatus, EmailDraftWithRevision,
    EmailOutboundFollowUpStatus, EmailOutboundFollowUpSummary, EmailReplyDraft,
    GmailThreadFollowUpState,
};
use bos_contracts::follow_up_tasks::{TaskRecord, TaskStatus, TaskWithRevision};
use bos_contracts::receipt::ActorKindDto;
use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::http::OperatorScope;
use crate::outbox::{self, NewOutboxJob};
use crate::slices::draft_store::{
    self, DraftStore, DraftTableSpec, ScopedDraftStore, ScopedStatusDraftStore,
};
use crate::store_core::{self, MutationOutcome, MutationRequest, StoreError};

pub const DRAFT_ENTITY_KIND: &str = "email_reply_draft";
const APPROVE_SQL: &str = "UPDATE email_reply_drafts SET status = 'approved', outbox_job_id = ?3, \
     updated_at_ms = ?4 WHERE client_id = ?1 AND draft_id = ?2";
const REJECT_SQL: &str = "UPDATE email_reply_drafts SET status = 'rejected', updated_at_ms = ?3 \
     WHERE client_id = ?1 AND draft_id = ?2";
const DRAFT_TABLE: DraftTableSpec = DraftTableSpec {
    table: EmailDraftStore::TABLE,
    entity_kind: DRAFT_ENTITY_KIND,
    not_found_code: EmailDraftStore::NOT_FOUND,
    not_staged_code: "email_draft_not_staged",
    approve_sql: APPROVE_SQL,
    reject_sql: REJECT_SQL,
};

const DRAFT_COLUMNS: &str = "d.draft_id, d.item_id, d.source_kind, d.source_ref, d.status, \
     d.source_user_id, d.to_addr, COALESCE(d.cc_addrs_json, '[]') AS cc_addrs_json, \
     d.subject, d.body_text, d.thread_id, d.reply_message_id, \
     COALESCE(d.reference_message_ids_json, '[]') AS reference_message_ids_json, d.provenance_json, \
     d.model, d.confidence, d.outbox_job_id, d.created_at_ms, d.updated_at_ms, \
     COALESCE(er.revision, 0) AS revision";

fn draft_from_row(row: &Row<'_>) -> rusqlite::Result<EmailDraftWithRevision> {
    Ok(EmailDraftWithRevision {
        draft: EmailReplyDraft {
            draft_id: row.get("draft_id")?,
            item_id: row.get("item_id")?,
            source_kind: row.get("source_kind")?,
            source_ref: row.get("source_ref")?,
            status: status_from_str(&row.get::<_, String>("status")?),
            source_user_id: row.get("source_user_id")?,
            to_addr: row.get("to_addr")?,
            cc_addrs: serde_json::from_str(&row.get::<_, String>("cc_addrs_json")?)
                .unwrap_or_default(),
            subject: row.get("subject")?,
            body_text: row.get("body_text")?,
            thread_id: row.get("thread_id")?,
            reply_message_id: row.get("reply_message_id")?,
            reference_message_ids: serde_json::from_str(
                &row.get::<_, String>("reference_message_ids_json")?,
            )
            .unwrap_or_default(),
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
        follow_up: None,
    })
}

fn attach_job_summary(
    conn: &Connection,
    client_id: &str,
    mut entry: EmailDraftWithRevision,
) -> Result<EmailDraftWithRevision, StoreError> {
    if let Some(job_id) = entry.draft.outbox_job_id.as_deref() {
        entry.outbox_job = outbox::job_summary(conn, client_id, job_id)?;
    }
    Ok(entry)
}

fn attach_follow_up_summary(
    conn: &Connection,
    client_id: &str,
    mut entry: EmailDraftWithRevision,
) -> Result<EmailDraftWithRevision, StoreError> {
    entry.follow_up = follow_up_for_draft(conn, client_id, &entry.draft.draft_id)?;
    Ok(entry)
}

struct EmailDraftStore;

impl DraftStore for EmailDraftStore {
    type WithRevision = EmailDraftWithRevision;

    const TABLE: &'static str = "email_reply_drafts";
    const COLUMNS: &'static str = DRAFT_COLUMNS;
    const ENTITY_KIND: &'static str = DRAFT_ENTITY_KIND;
    const NOT_FOUND: &'static str = "email_draft_not_found";

    fn map_row(row: &Row<'_>) -> rusqlite::Result<Self::WithRevision> {
        draft_from_row(row)
    }

    fn attach(
        conn: &Connection,
        client_id: &str,
        entry: Self::WithRevision,
    ) -> Result<Self::WithRevision, StoreError> {
        attach_follow_up_summary(conn, client_id, attach_job_summary(conn, client_id, entry)?)
    }
}

impl ScopedDraftStore for EmailDraftStore {
    fn source_user_id(entry: &Self::WithRevision) -> Option<&str> {
        entry.draft.source_user_id.as_deref()
    }
}

impl ScopedStatusDraftStore for EmailDraftStore {
    fn map_status(row: &Row<'_>) -> rusqlite::Result<(String, Option<String>)> {
        Ok((row.get(0)?, row.get(1)?))
    }
}

pub fn active_draft_for_item(
    conn: &Connection,
    client_id: &str,
    item_id: &str,
) -> Result<Option<EmailDraftWithRevision>, StoreError> {
    draft_store::active_draft_for_item::<EmailDraftStore>(conn, client_id, item_id)
}

pub fn get_draft(
    conn: &Connection,
    client_id: &str,
    draft_id: &str,
    scope: &OperatorScope,
) -> Result<Option<EmailDraftWithRevision>, StoreError> {
    draft_store::get_draft_scoped::<EmailDraftStore>(conn, client_id, draft_id, scope)
}

pub fn list_drafts(
    conn: &Connection,
    client_id: &str,
    item_id: Option<&str>,
    limit: usize,
    scope: &OperatorScope,
) -> Result<Vec<EmailDraftWithRevision>, StoreError> {
    draft_store::list_drafts_scoped::<EmailDraftStore>(conn, client_id, item_id, limit, scope)
}

pub fn count_drafts_for_item(
    conn: &Connection,
    client_id: &str,
    item_id: &str,
) -> Result<u64, StoreError> {
    draft_store::count_drafts_for_item::<EmailDraftStore>(conn, client_id, item_id)
}

/// Item ids with a STAGED draft (operator decision pending). Feeds the
/// queue's "needs you" decoration via the produce spine.
pub fn staged_item_ids(conn: &Connection, client_id: &str) -> Result<Vec<String>, StoreError> {
    draft_store::staged_item_ids::<EmailDraftStore>(conn, client_id)
}

pub fn insert_draft(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    draft: &EmailReplyDraft,
    idempotency_key: &str,
) -> Result<MutationOutcome, StoreError> {
    let after = serde_json::to_string(draft)
        .map_err(|err| StoreError::Domain(format!("serialize draft: {err}")))?;
    let provenance_json = serde_json::to_string(&draft.provenance)
        .map_err(|err| StoreError::Domain(format!("serialize provenance: {err}")))?;
    let cc_addrs_json = serde_json::to_string(&draft.cc_addrs)
        .map_err(|err| StoreError::Domain(format!("serialize cc addrs: {err}")))?;
    let reference_message_ids_json = serde_json::to_string(&draft.reference_message_ids)
        .map_err(|err| StoreError::Domain(format!("serialize references: {err}")))?;
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
                "INSERT INTO email_reply_drafts \
                 (client_id, draft_id, item_id, source_kind, source_ref, source_user_id, \
                  status, to_addr, cc_addrs_json, subject, body_text, thread_id, reply_message_id, \
                  reference_message_ids_json, provenance_json, model, confidence, created_at_ms, updated_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'staged', ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, \
                  ?15, ?16, ?17, ?17)",
                params![
                    owned_client,
                    row.draft_id,
                    row.item_id,
                    row.source_kind,
                    row.source_ref,
                    row.source_user_id,
                    row.to_addr,
                    cc_addrs_json,
                    row.subject,
                    row.body_text,
                    row.thread_id,
                    row.reply_message_id,
                    reference_message_ids_json,
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
                    StoreError::Domain("email_draft_already_active".to_string())
                }
                other => other.into(),
            })?;
            Ok(())
        },
    )
}

pub use crate::slices::mutation_context::ScopedMutationContext as DraftActionContext;

pub const FOLLOW_UP_ENTITY_KIND: &str = "email_outbound_follow_up";
pub const SOURCE_KIND_EMAIL_FOLLOW_UP: &str = "email_follow_up";
pub const RESOLUTION_THEY_REPLIED: &str = "they_replied";

#[derive(Debug, Clone)]
pub struct EmailFollowUpPlan {
    pub follow_up_id: String,
    pub task: TaskRecord,
    pub due_date: String,
    pub title: String,
    pub context: String,
    pub create_follow_up_draft: bool,
}

impl EmailFollowUpPlan {
    pub fn from_request(
        draft: &EmailReplyDraft,
        request: &EmailDraftFollowUpRequest,
        now_ms: u64,
    ) -> Result<Option<Self>, StoreError> {
        if !request.enabled {
            return Ok(None);
        }
        let due_date = request
            .due_date
            .as_deref()
            .map(str::trim)
            .filter(|raw| !raw.is_empty())
            .ok_or_else(|| StoreError::Domain("email_follow_up_due_date_required".to_string()))?;
        let date_context = crate::slices::datetime_input::context_from_now_ms(now_ms);
        let due_date =
            crate::slices::datetime_input::normalize_civil_date(due_date, Some(&date_context))
                .map_err(|_| StoreError::Domain("email_follow_up_due_date_invalid".to_string()))?;
        let title: String = request.title.trim().chars().take(200).collect();
        if title.is_empty() {
            return Err(StoreError::Domain(
                "email_follow_up_title_required".to_string(),
            ));
        }
        let context: String = request.context.trim().chars().take(1_000).collect();
        let follow_up_id = format!("efuw_{}", draft.draft_id);
        let task_id = format!("task_{follow_up_id}");
        Ok(Some(Self {
            follow_up_id,
            task: TaskRecord {
                task_id,
                title: title.clone(),
                due_date: Some(due_date.clone()),
                context: context.clone(),
                source_kind: SOURCE_KIND_EMAIL_FOLLOW_UP.to_string(),
                source_ref: draft.draft_id.clone(),
                source_user_id: draft.source_user_id.clone(),
                source_item_id: None,
                status: TaskStatus::Open,
                created_at_ms: now_ms,
                updated_at_ms: now_ms,
            },
            due_date,
            title,
            context,
            create_follow_up_draft: request.create_follow_up_draft,
        }))
    }
}

/// Approve a staged draft: status flip + outbox enqueue + optional local
/// follow-up task/workflow row, one transaction.
pub fn approve_draft(
    conn: &mut Connection,
    ctx: DraftActionContext<'_>,
    draft_id: &str,
    job: &NewOutboxJob,
    follow_up: Option<EmailFollowUpPlan>,
) -> Result<MutationOutcome, StoreError> {
    let source_user_id: Option<String> = conn
        .query_row(
            "SELECT source_user_id FROM email_reply_drafts WHERE client_id = ?1 AND draft_id = ?2",
            params![ctx.client_id, draft_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| StoreError::Domain("email_draft_not_found".to_string()))?;
    ctx.scope.require_source_user(source_user_id.as_deref())?;

    let draft_meta: (String, Option<String>) = conn.query_row(
        "SELECT item_id, thread_id FROM email_reply_drafts WHERE client_id = ?1 AND draft_id = ?2",
        params![ctx.client_id, draft_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;

    let owned_client = ctx.client_id.to_string();
    let owned_draft = draft_id.to_string();
    let owned_job = job.clone();
    let owned_follow_up = follow_up.clone();
    let now_ms = ctx.now_ms;
    let after_json = serde_json::json!({
        "status": "approved",
        "outbox_job_id": job.job_id,
        "follow_up_id": follow_up.as_ref().map(|plan| plan.follow_up_id.as_str()),
    })
    .to_string();
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
            let current: String = tx.query_row(
                "SELECT status FROM email_reply_drafts WHERE client_id = ?1 AND draft_id = ?2",
                params![owned_client, owned_draft],
                |row| row.get(0),
            )?;
            if current != "staged" {
                return Err(StoreError::Domain(format!(
                    "email_draft_not_staged:{current}"
                )));
            }
            tx.execute(
                APPROVE_SQL,
                params![owned_client, owned_draft, owned_job.job_id, now_ms as i64],
            )?;
            outbox::enqueue_within(tx, &owned_client, &owned_job, now_ms)?;
            if let Some(plan) = owned_follow_up {
                crate::slices::follow_up_tasks::store::insert_task_within(
                    tx,
                    &owned_client,
                    &plan.task,
                    now_ms,
                )?;
                tx.execute(
                    "INSERT INTO email_outbound_follow_ups \
                     (client_id, follow_up_id, email_draft_id, item_id, thread_id, \
                      source_user_id, gmail_draft_outbox_job_id, follow_up_task_id, status, \
                      thread_state, due_date, follow_up_title, follow_up_context, \
                      create_follow_up_draft, created_at_ms, updated_at_ms) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'active', ?9, ?10, ?11, ?12, \
                      ?13, ?14, ?14)",
                    params![
                        owned_client,
                        plan.follow_up_id,
                        owned_draft,
                        draft_meta.0,
                        draft_meta.1,
                        source_user_id,
                        owned_job.job_id,
                        plan.task.task_id,
                        if draft_meta.1.is_some() {
                            "draft_created"
                        } else {
                            "not_applicable"
                        },
                        plan.due_date,
                        plan.title,
                        plan.context,
                        if plan.create_follow_up_draft { 1 } else { 0 },
                        now_ms as i64,
                    ],
                )?;
                crate::store_core::initialize_revision_within(
                    tx,
                    &owned_client,
                    FOLLOW_UP_ENTITY_KIND,
                    &plan.follow_up_id,
                    1,
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

/// Edit a STAGED draft's operator-reviewable fields. Approval builds the
/// Gmail draft payload from the stored row, so edits flow into the write.
pub fn update_draft(
    conn: &mut Connection,
    ctx: DraftActionContext<'_>,
    draft_id: &str,
    to_addr_raw: &str,
    cc_addrs_raw: &[String],
    subject_raw: &str,
    body_text_raw: &str,
) -> Result<MutationOutcome, StoreError> {
    let (current, source_user_id) = require_status(conn, ctx.client_id, draft_id)?;
    ctx.scope.require_source_user(source_user_id.as_deref())?;
    if current != "staged" {
        return Err(StoreError::Domain(format!(
            "email_draft_not_staged:{current}"
        )));
    }
    let fields =
        normalize_editable_fields(to_addr_raw, cc_addrs_raw, subject_raw, body_text_raw, false)?;
    let before: (String, String, String, String) = conn.query_row(
        "SELECT to_addr, cc_addrs_json, subject, body_text FROM email_reply_drafts WHERE client_id = ?1 AND draft_id = ?2",
        params![ctx.client_id, draft_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    let owned_client = ctx.client_id.to_string();
    let owned_draft = draft_id.to_string();
    let owned_to = fields.to_addr.clone();
    let owned_cc = serde_json::to_string(&fields.cc_addrs)
        .map_err(|err| StoreError::Domain(format!("serialize cc addrs: {err}")))?;
    let owned_subject = fields.subject.clone();
    let owned_body = fields.body_text.clone();
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
            correlation_id: None,
            causation_id: None,
            before_json: Some(
                serde_json::json!({
                    "to_addr": before.0,
                    "cc_addrs": serde_json::from_str::<serde_json::Value>(&before.1).unwrap_or_default(),
                    "subject": before.2,
                    "body_text": before.3,
                })
                .to_string(),
            ),
            after_json: Some(
                serde_json::json!({
                    "to_addr": fields.to_addr,
                    "cc_addrs": fields.cc_addrs,
                    "subject": fields.subject,
                    "body_text": fields.body_text,
                })
                .to_string(),
            ),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE email_reply_drafts SET to_addr = ?3, cc_addrs_json = ?4, subject = ?5, \
                 body_text = ?6, updated_at_ms = ?7 \
                 WHERE client_id = ?1 AND draft_id = ?2",
                params![
                    owned_client,
                    owned_draft,
                    owned_to,
                    owned_cc,
                    owned_subject,
                    owned_body,
                    now_ms as i64
                ],
            )?;
            Ok(())
        },
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailEditableFields {
    pub to_addr: String,
    pub cc_addrs: Vec<String>,
    pub subject: String,
    pub body_text: String,
}

/// One validation chokepoint for manual staging and later operator edits.
pub fn normalize_editable_fields(
    to_addr_raw: &str,
    cc_addrs_raw: &[String],
    subject_raw: &str,
    body_text_raw: &str,
    allow_empty_body: bool,
) -> Result<EmailEditableFields, StoreError> {
    let to_addr = normalize_recipient_line(to_addr_raw)?;
    if cc_addrs_raw.len() > 100 {
        return Err(StoreError::Domain(
            "email_draft_cc_addrs_invalid".to_string(),
        ));
    }
    let mut cc_addrs = Vec::new();
    for raw in cc_addrs_raw {
        let recipient = normalize_recipient(raw.trim())
            .map_err(|_| StoreError::Domain("email_draft_cc_addrs_invalid".to_string()))?;
        if !cc_addrs.iter().any(|existing| existing == &recipient) {
            cc_addrs.push(recipient);
        }
    }
    if subject_raw.chars().any(char::is_control) {
        return Err(StoreError::Domain(
            "email_draft_subject_invalid".to_string(),
        ));
    }
    let subject: String = subject_raw.trim().chars().take(500).collect();
    if subject.is_empty() {
        return Err(StoreError::Domain(
            "email_draft_subject_required".to_string(),
        ));
    }
    let body_text: String = body_text_raw.trim().chars().take(10_000).collect();
    if !allow_empty_body && body_text.is_empty() {
        return Err(StoreError::Domain("email_draft_body_required".to_string()));
    }
    Ok(EmailEditableFields {
        to_addr,
        cc_addrs,
        subject,
        body_text,
    })
}

/// Apply a bounded model rewrite to the body of a still-staged exact revision.
/// Typed destination fields remain untouched and approval remains separate.
pub fn apply_ai_rewrite(
    conn: &mut Connection,
    ctx: DraftActionContext<'_>,
    draft_id: &str,
    body_text_raw: &str,
    provenance: &[DraftFieldProvenance],
    model_raw: &str,
    confidence_raw: &str,
) -> Result<MutationOutcome, StoreError> {
    let (current, source_user_id) = require_status(conn, ctx.client_id, draft_id)?;
    ctx.scope.require_source_user(source_user_id.as_deref())?;
    if current != "staged" {
        return Err(StoreError::Domain(format!(
            "email_draft_not_staged:{current}"
        )));
    }
    let body_text: String = body_text_raw.trim().chars().take(10_000).collect();
    if body_text.is_empty() {
        return Err(StoreError::Domain("email_draft_body_required".to_string()));
    }
    let confidence = confidence_raw.trim();
    if !matches!(confidence, "high" | "medium" | "low") {
        return Err(StoreError::Domain(
            "email_draft_confidence_invalid".to_string(),
        ));
    }
    let model: String = model_raw.trim().chars().take(200).collect();
    let provenance_json = serde_json::to_string(provenance)
        .map_err(|err| StoreError::Domain(format!("serialize provenance: {err}")))?;
    let before: (String, String, String, String) = conn.query_row(
        "SELECT body_text, provenance_json, model, confidence FROM email_reply_drafts \
         WHERE client_id = ?1 AND draft_id = ?2",
        params![ctx.client_id, draft_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    let owned_client = ctx.client_id.to_string();
    let owned_draft = draft_id.to_string();
    let owned_body = body_text.clone();
    let owned_provenance = provenance_json.clone();
    let owned_model = model.clone();
    let owned_confidence = confidence.to_string();
    let now_ms = ctx.now_ms;
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: DRAFT_ENTITY_KIND,
            entity_id: draft_id,
            change_kind: "rewrite",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: Some(
                serde_json::json!({
                    "body_text": before.0,
                    "provenance": serde_json::from_str::<serde_json::Value>(&before.1).unwrap_or_default(),
                    "model": before.2,
                    "confidence": before.3,
                })
                .to_string(),
            ),
            after_json: Some(
                serde_json::json!({
                    "body_text": body_text,
                    "provenance": provenance,
                    "model": model,
                    "confidence": confidence,
                })
                .to_string(),
            ),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE email_reply_drafts SET body_text = ?3, provenance_json = ?4, model = ?5, \
                 confidence = ?6, updated_at_ms = ?7 WHERE client_id = ?1 AND draft_id = ?2",
                params![
                    owned_client,
                    owned_draft,
                    owned_body,
                    owned_provenance,
                    owned_model,
                    owned_confidence,
                    now_ms as i64,
                ],
            )?;
            Ok(())
        },
    )
}

fn normalize_recipient_line(raw: &str) -> Result<String, StoreError> {
    if raw.chars().any(|ch| ch.is_control()) {
        return Err(StoreError::Domain(
            "email_draft_to_addr_invalid".to_string(),
        ));
    }
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(StoreError::Domain(
            "email_draft_to_addr_required".to_string(),
        ));
    }
    if trimmed.chars().count() > 2_000 {
        return Err(StoreError::Domain(
            "email_draft_to_addr_invalid".to_string(),
        ));
    }
    let mut recipients = Vec::new();
    for entry in trimmed.split(',').map(str::trim) {
        if entry.is_empty() {
            return Err(StoreError::Domain(
                "email_draft_to_addr_invalid".to_string(),
            ));
        }
        recipients.push(normalize_recipient(entry)?);
    }
    if recipients.is_empty() {
        return Err(StoreError::Domain(
            "email_draft_to_addr_required".to_string(),
        ));
    }
    Ok(recipients.join(", "))
}

fn normalize_recipient(entry: &str) -> Result<String, StoreError> {
    if let Some(open) = entry.find('<') {
        let close = entry
            .rfind('>')
            .ok_or_else(|| StoreError::Domain("email_draft_to_addr_invalid".to_string()))?;
        if close < open || !entry[close + 1..].trim().is_empty() {
            return Err(StoreError::Domain(
                "email_draft_to_addr_invalid".to_string(),
            ));
        }
        let display = entry[..open].trim();
        let address = entry[open + 1..close].trim();
        if display.is_empty() || display.contains(['<', '>', ';']) || !valid_email_address(address)
        {
            return Err(StoreError::Domain(
                "email_draft_to_addr_invalid".to_string(),
            ));
        }
        return Ok(format!("{display} <{}>", address.to_ascii_lowercase()));
    }
    if entry.contains(['>', '<', ';']) || !valid_email_address(entry) {
        return Err(StoreError::Domain(
            "email_draft_to_addr_invalid".to_string(),
        ));
    }
    Ok(entry.to_ascii_lowercase())
}

fn valid_email_address(address: &str) -> bool {
    if address.is_empty()
        || address.contains(char::is_whitespace)
        || address.contains(['<', '>', ',', ';'])
    {
        return false;
    }
    let Some((local, domain)) = address.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && !domain.is_empty()
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && domain.contains('.')
        && !domain.contains('@')
}

fn require_status(
    conn: &Connection,
    client_id: &str,
    draft_id: &str,
) -> Result<(String, Option<String>), StoreError> {
    draft_store::require_status_scoped::<EmailDraftStore>(conn, client_id, draft_id)
}

fn status_from_str(raw: &str) -> EmailDraftStatus {
    match raw {
        "approved" => EmailDraftStatus::Approved,
        "rejected" => EmailDraftStatus::Rejected,
        _ => EmailDraftStatus::Staged,
    }
}

fn follow_up_status_from_str(raw: &str) -> EmailOutboundFollowUpStatus {
    match raw {
        "resolved" => EmailOutboundFollowUpStatus::Resolved,
        "cancelled" => EmailOutboundFollowUpStatus::Cancelled,
        "stale" => EmailOutboundFollowUpStatus::Stale,
        _ => EmailOutboundFollowUpStatus::Active,
    }
}

fn follow_up_status_str(status: EmailOutboundFollowUpStatus) -> &'static str {
    match status {
        EmailOutboundFollowUpStatus::Active => "active",
        EmailOutboundFollowUpStatus::Resolved => "resolved",
        EmailOutboundFollowUpStatus::Cancelled => "cancelled",
        EmailOutboundFollowUpStatus::Stale => "stale",
    }
}

fn thread_state_from_str(raw: &str) -> GmailThreadFollowUpState {
    match raw {
        "sent_waiting_reply" => GmailThreadFollowUpState::SentWaitingReply,
        "replied_after_send" => GmailThreadFollowUpState::RepliedAfterSend,
        "stale_unknown" => GmailThreadFollowUpState::StaleUnknown,
        "not_applicable" => GmailThreadFollowUpState::NotApplicable,
        _ => GmailThreadFollowUpState::DraftCreated,
    }
}

pub fn thread_state_str(state: GmailThreadFollowUpState) -> &'static str {
    match state {
        GmailThreadFollowUpState::DraftCreated => "draft_created",
        GmailThreadFollowUpState::SentWaitingReply => "sent_waiting_reply",
        GmailThreadFollowUpState::RepliedAfterSend => "replied_after_send",
        GmailThreadFollowUpState::StaleUnknown => "stale_unknown",
        GmailThreadFollowUpState::NotApplicable => "not_applicable",
    }
}

fn follow_up_summary_from_row(row: &Row<'_>) -> rusqlite::Result<EmailOutboundFollowUpSummary> {
    Ok(EmailOutboundFollowUpSummary {
        follow_up_id: row.get("follow_up_id")?,
        email_draft_id: row.get("email_draft_id")?,
        follow_up_task_id: row.get("follow_up_task_id")?,
        item_id: row.get("item_id")?,
        thread_id: row.get("thread_id")?,
        status: follow_up_status_from_str(&row.get::<_, String>("status")?),
        thread_state: thread_state_from_str(&row.get::<_, String>("thread_state")?),
        due_date: row.get("due_date")?,
        follow_up_title: row.get("follow_up_title")?,
        create_follow_up_draft: row.get::<_, i64>("create_follow_up_draft")? != 0,
        sent_message_id: row.get("sent_message_id")?,
        sent_at_ms: row.get::<_, Option<i64>>("sent_at_ms")?.map(|v| v as u64),
        reply_message_id: row.get("reply_message_id")?,
        reply_at_ms: row.get::<_, Option<i64>>("reply_at_ms")?.map(|v| v as u64),
        resolution_reason: row.get("resolution_reason")?,
        last_checked_at_ms: row
            .get::<_, Option<i64>>("last_checked_at_ms")?
            .map(|v| v as u64),
        last_check_error: row.get("last_check_error")?,
    })
}

const FOLLOW_UP_COLUMNS: &str = "follow_up_id, email_draft_id, follow_up_task_id, item_id, \
     thread_id, status, thread_state, due_date, follow_up_title, create_follow_up_draft, \
     sent_message_id, sent_at_ms, reply_message_id, reply_at_ms, resolution_reason, \
     last_checked_at_ms, last_check_error";

pub fn get_follow_up(
    conn: &Connection,
    client_id: &str,
    follow_up_id: &str,
    scope: &OperatorScope,
) -> Result<Option<EmailOutboundFollowUpSummary>, StoreError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {FOLLOW_UP_COLUMNS}, source_user_id FROM email_outbound_follow_ups \
         WHERE client_id = ?1 AND follow_up_id = ?2"
    ))?;
    let row = stmt
        .query_row(params![client_id, follow_up_id], |row| {
            let source_user_id: Option<String> = row.get("source_user_id")?;
            Ok((follow_up_summary_from_row(row)?, source_user_id))
        })
        .optional()?;
    Ok(row
        .filter(|(_, source_user_id)| scope.matches_source_user(source_user_id.as_deref()))
        .map(|(summary, _)| summary))
}

#[derive(Debug, Clone)]
pub struct FollowUpCheckTarget {
    pub summary: EmailOutboundFollowUpSummary,
    pub source_user_id: Option<String>,
    pub approved_at_ms: u64,
}

pub fn get_follow_up_check_target(
    conn: &Connection,
    client_id: &str,
    follow_up_id: &str,
    scope: &OperatorScope,
) -> Result<Option<FollowUpCheckTarget>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT f.*, d.updated_at_ms AS approved_at_ms \
         FROM email_outbound_follow_ups f \
         JOIN email_reply_drafts d ON d.client_id = f.client_id AND d.draft_id = f.email_draft_id \
         WHERE f.client_id = ?1 AND f.follow_up_id = ?2",
    )?;
    let row = stmt
        .query_row(params![client_id, follow_up_id], |row| {
            Ok(FollowUpCheckTarget {
                summary: follow_up_summary_from_row(row)?,
                source_user_id: row.get("source_user_id")?,
                approved_at_ms: row.get::<_, i64>("approved_at_ms")? as u64,
            })
        })
        .optional()?;
    Ok(row.filter(|target| scope.matches_source_user(target.source_user_id.as_deref())))
}

pub fn source_view_for_follow_up(
    conn: &Connection,
    client_id: &str,
    follow_up_id: &str,
) -> Result<Option<bos_contracts::email_triage::InboundMessageRecord>, StoreError> {
    conn.query_row(
        "SELECT f.follow_up_id, f.follow_up_context, f.source_user_id, d.to_addr, d.subject, \
         d.body_text, d.thread_id, d.updated_at_ms \
         FROM email_outbound_follow_ups f \
         JOIN email_reply_drafts d ON d.client_id = f.client_id AND d.draft_id = f.email_draft_id \
         WHERE f.client_id = ?1 AND f.follow_up_id = ?2",
        params![client_id, follow_up_id],
        |row| {
            let follow_up_id: String = row.get("follow_up_id")?;
            let subject: String = row.get("subject")?;
            let body_text: String = row.get("body_text")?;
            let context: String = row.get("follow_up_context")?;
            Ok(bos_contracts::email_triage::InboundMessageRecord {
                source_key: follow_up_id.clone(),
                message_id: follow_up_id,
                thread_id: row.get("thread_id")?,
                internal_date_ms: Some(row.get("updated_at_ms")?),
                from_addr: Some(row.get("to_addr")?),
                to_addr: None,
                subject: Some(subject.clone()),
                body_excerpt: format!(
                    "Follow up on the prior outbound draft if no reply has arrived.\n\nPrior draft subject: {subject}\n\nOperator note: {context}\n\nPrior draft body:\n{body_text}"
                )
                .chars()
                .take(2_000)
                .collect(),
                body_full: format!(
                    "Follow up on the prior outbound draft if no reply has arrived.\n\nPrior draft subject: {subject}\n\nOperator note: {context}\n\nPrior draft body:\n{body_text}"
                ),
                headers: Vec::new(),
                labels: Vec::new(),
                resolved_category: "follow_up".to_string(),
                matched_rule_id: None,
                ingested_at_ms: row.get::<_, i64>("updated_at_ms")? as u64,
                ai_triage_status: None,
                ai_triage_rationale: None,
                attachments: Vec::new(),
                source_user_id: row.get("source_user_id")?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

pub fn follow_up_for_draft(
    conn: &Connection,
    client_id: &str,
    draft_id: &str,
) -> Result<Option<EmailOutboundFollowUpSummary>, StoreError> {
    conn.query_row(
        &format!(
            "SELECT {FOLLOW_UP_COLUMNS} FROM email_outbound_follow_ups \
             WHERE client_id = ?1 AND email_draft_id = ?2 \
             ORDER BY created_at_ms DESC, follow_up_id DESC LIMIT 1"
        ),
        params![client_id, draft_id],
        follow_up_summary_from_row,
    )
    .optional()
    .map_err(Into::into)
}

pub fn list_follow_ups(
    conn: &Connection,
    client_id: &str,
    status: FollowUpListStatus,
    scope: &OperatorScope,
) -> Result<Vec<EmailOutboundFollowUpSummary>, StoreError> {
    let status_filter = match status {
        FollowUpListStatus::Open => "AND f.status IN ('active', 'stale')",
        FollowUpListStatus::Resolved => "AND f.status = 'resolved'",
        FollowUpListStatus::All => "",
    };
    let (scope_pred, scope_all, scope_user) = scope.sql_filter("f.source_user_id", 3, 4);
    let mut stmt = conn.prepare(&format!(
        "SELECT {FOLLOW_UP_COLUMNS} FROM email_outbound_follow_ups f \
         WHERE f.client_id = ?1 {status_filter} AND {scope_pred} \
         ORDER BY f.due_date ASC, f.created_at_ms DESC LIMIT ?2"
    ))?;
    let rows = stmt.query_map(params![client_id, 200_i64, scope_all, scope_user], |row| {
        follow_up_summary_from_row(row)
    })?;
    let mut summaries = Vec::new();
    for row in rows {
        summaries.push(row?);
    }
    Ok(summaries)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FollowUpListStatus {
    Open,
    Resolved,
    All,
}

pub fn decorate_tasks_with_follow_ups(
    conn: &Connection,
    client_id: &str,
    scope: &OperatorScope,
    tasks: &mut Vec<TaskWithRevision>,
) -> Result<(), StoreError> {
    if tasks.is_empty() {
        return Ok(());
    }
    let mut stmt = conn.prepare(&format!(
        "SELECT {FOLLOW_UP_COLUMNS}, source_user_id FROM email_outbound_follow_ups \
         WHERE client_id = ?1 AND follow_up_task_id = ?2"
    ))?;
    let mut keep = Vec::with_capacity(tasks.len());
    for entry in &mut *tasks {
        let row = stmt
            .query_row(params![client_id, entry.task.task_id], |row| {
                let source_user_id: Option<String> = row.get("source_user_id")?;
                Ok((follow_up_summary_from_row(row)?, source_user_id))
            })
            .optional()?;
        match row {
            Some((summary, source_user_id))
                if scope.matches_source_user(source_user_id.as_deref()) =>
            {
                entry.follow_up = Some(summary);
                keep.push(true);
            }
            Some(_) => keep.push(false),
            None if entry.task.source_kind == SOURCE_KIND_EMAIL_FOLLOW_UP => {
                keep.push(scope.matches_source_user(None));
            }
            None => keep.push(true),
        }
    }
    let mut idx = 0usize;
    tasks.retain(|_| {
        let should_keep = keep.get(idx).copied().unwrap_or(true);
        idx += 1;
        should_keep
    });
    Ok(())
}

#[derive(Debug, Clone)]
pub struct ThreadReconciliation {
    pub thread_state: GmailThreadFollowUpState,
    pub status: EmailOutboundFollowUpStatus,
    pub sent_message_id: Option<String>,
    pub sent_at_ms: Option<u64>,
    pub reply_message_id: Option<String>,
    pub reply_at_ms: Option<u64>,
    pub resolution_reason: Option<String>,
    pub last_check_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ThreadReconciliationOutcome {
    pub linked_task_id: Option<String>,
    pub should_complete_linked_task: bool,
}

pub fn apply_thread_reconciliation(
    conn: &mut Connection,
    ctx: DraftActionContext<'_>,
    follow_up_id: &str,
    reconciliation: ThreadReconciliation,
) -> Result<ThreadReconciliationOutcome, StoreError> {
    let current: Option<(Option<String>, Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT status, thread_state, source_user_id FROM email_outbound_follow_ups \
             WHERE client_id = ?1 AND follow_up_id = ?2",
            params![ctx.client_id, follow_up_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((current_status, current_state, source_user_id)) = current else {
        return Err(StoreError::Domain("email_follow_up_not_found".to_string()));
    };
    ctx.scope.require_source_user(source_user_id.as_deref())?;

    let linked_task_id: Option<String> = conn.query_row(
        "SELECT follow_up_task_id FROM email_outbound_follow_ups WHERE client_id = ?1 AND follow_up_id = ?2",
        params![ctx.client_id, follow_up_id],
        |row| row.get(0),
    )?;
    let correlation_id = linked_task_id.clone();

    let owned_client = ctx.client_id.to_string();
    let owned_follow_up = follow_up_id.to_string();
    let now_ms = ctx.now_ms;
    let after_json = serde_json::json!({
        "status": follow_up_status_str(reconciliation.status),
        "thread_state": thread_state_str(reconciliation.thread_state),
        "sent_message_id": reconciliation.sent_message_id,
        "reply_message_id": reconciliation.reply_message_id,
        "resolution_reason": reconciliation.resolution_reason,
        "last_check_error": reconciliation.last_check_error,
    })
    .to_string();
    let should_complete_linked_task =
        reconciliation.status == EmailOutboundFollowUpStatus::Resolved;
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: FOLLOW_UP_ENTITY_KIND,
            entity_id: follow_up_id,
            change_kind: "check",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: correlation_id.as_deref(),
            causation_id: None,
            before_json: Some(
                serde_json::json!({
                    "status": current_status,
                    "thread_state": current_state,
                })
                .to_string(),
            ),
            after_json: Some(after_json),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE email_outbound_follow_ups SET status = ?3, thread_state = ?4, \
                 sent_message_id = ?5, sent_at_ms = ?6, reply_message_id = ?7, reply_at_ms = ?8, \
                 resolution_reason = ?9, last_checked_at_ms = ?10, last_check_error = ?11, \
                 updated_at_ms = ?10 WHERE client_id = ?1 AND follow_up_id = ?2",
                params![
                    owned_client,
                    owned_follow_up,
                    follow_up_status_str(reconciliation.status),
                    thread_state_str(reconciliation.thread_state),
                    reconciliation.sent_message_id,
                    reconciliation.sent_at_ms.map(|v| v as i64),
                    reconciliation.reply_message_id,
                    reconciliation.reply_at_ms.map(|v| v as i64),
                    reconciliation.resolution_reason,
                    now_ms as i64,
                    reconciliation.last_check_error,
                ],
            )?;
            Ok(())
        },
    )?;
    Ok(ThreadReconciliationOutcome {
        linked_task_id,
        should_complete_linked_task,
    })
}
