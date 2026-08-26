//! CRM note draft persistence through store_core. Approval enqueues the
//! provider-write outbox job inside the SAME mutation transaction.

use bos_contracts::crm_drafts::{CrmDraftStatus, CrmDraftWithRevision, CrmNoteDraft};
use bos_contracts::receipt::ActorKindDto;
use rusqlite::{params, Connection, Row};

use crate::http::OperatorScope;
use crate::outbox::{self, NewOutboxJob};
use crate::slices::draft_store::{
    self, DraftStore, DraftTableSpec, ScopedDraftStore, ScopedStatusDraftStore,
};
use crate::store_core::{self, MutationOutcome, MutationRequest, StoreError};

pub const DRAFT_ENTITY_KIND: &str = "crm_note_draft";
const APPROVE_SQL: &str = "UPDATE crm_note_drafts SET status = 'approved', outbox_job_id = ?3, \
     updated_at_ms = ?4 WHERE client_id = ?1 AND draft_id = ?2";
const REJECT_SQL: &str = "UPDATE crm_note_drafts SET status = 'rejected', updated_at_ms = ?3 \
     WHERE client_id = ?1 AND draft_id = ?2";
const DRAFT_TABLE: DraftTableSpec = DraftTableSpec {
    table: CrmDraftStore::TABLE,
    entity_kind: DRAFT_ENTITY_KIND,
    not_found_code: CrmDraftStore::NOT_FOUND,
    not_staged_code: "crm_draft_not_staged",
    approve_sql: APPROVE_SQL,
    reject_sql: REJECT_SQL,
};

const DRAFT_COLUMNS: &str = "d.draft_id, d.item_id, d.source_kind, d.source_ref, d.status, \
     d.source_user_id, d.note_body, d.contact_email, d.occurred_at, d.provenance_json, d.model, \
     d.confidence, d.outbox_job_id, d.created_at_ms, d.updated_at_ms, COALESCE(er.revision, 0) AS revision";

fn draft_from_row(row: &Row<'_>) -> rusqlite::Result<CrmDraftWithRevision> {
    Ok(CrmDraftWithRevision {
        draft: CrmNoteDraft {
            draft_id: row.get("draft_id")?,
            item_id: row.get("item_id")?,
            source_kind: row.get("source_kind")?,
            source_ref: row.get("source_ref")?,
            status: status_from_str(&row.get::<_, String>("status")?),
            source_user_id: row.get("source_user_id")?,
            note_body: row.get("note_body")?,
            contact_email: row.get("contact_email")?,
            occurred_at: row.get("occurred_at")?,
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
    mut entry: CrmDraftWithRevision,
) -> Result<CrmDraftWithRevision, StoreError> {
    if let Some(job_id) = entry.draft.outbox_job_id.as_deref() {
        entry.outbox_job = outbox::job_summary(conn, client_id, job_id)?;
    }
    Ok(entry)
}

struct CrmDraftStore;

impl DraftStore for CrmDraftStore {
    type WithRevision = CrmDraftWithRevision;

    const TABLE: &'static str = "crm_note_drafts";
    const COLUMNS: &'static str = DRAFT_COLUMNS;
    const ENTITY_KIND: &'static str = DRAFT_ENTITY_KIND;
    const NOT_FOUND: &'static str = "crm_draft_not_found";

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

impl ScopedDraftStore for CrmDraftStore {
    fn source_user_id(entry: &Self::WithRevision) -> Option<&str> {
        entry.draft.source_user_id.as_deref()
    }
}

impl ScopedStatusDraftStore for CrmDraftStore {
    fn map_status(row: &Row<'_>) -> rusqlite::Result<(String, Option<String>)> {
        Ok((row.get(0)?, row.get(1)?))
    }
}

pub fn active_draft_for_item(
    conn: &Connection,
    client_id: &str,
    item_id: &str,
) -> Result<Option<CrmDraftWithRevision>, StoreError> {
    draft_store::active_draft_for_item::<CrmDraftStore>(conn, client_id, item_id)
}

pub fn get_draft(
    conn: &Connection,
    client_id: &str,
    draft_id: &str,
    scope: &OperatorScope,
) -> Result<Option<CrmDraftWithRevision>, StoreError> {
    draft_store::get_draft_scoped::<CrmDraftStore>(conn, client_id, draft_id, scope)
}

pub fn list_drafts(
    conn: &Connection,
    client_id: &str,
    item_id: Option<&str>,
    limit: usize,
    scope: &OperatorScope,
) -> Result<Vec<CrmDraftWithRevision>, StoreError> {
    draft_store::list_drafts_scoped::<CrmDraftStore>(conn, client_id, item_id, limit, scope)
}

pub fn count_drafts_for_item(
    conn: &Connection,
    client_id: &str,
    item_id: &str,
) -> Result<u64, StoreError> {
    draft_store::count_drafts_for_item::<CrmDraftStore>(conn, client_id, item_id)
}

/// Item ids with a STAGED draft (operator decision pending). Feeds the
/// queue's "needs you" decoration via the produce spine.
pub fn staged_item_ids(conn: &Connection, client_id: &str) -> Result<Vec<String>, StoreError> {
    draft_store::staged_item_ids::<CrmDraftStore>(conn, client_id)
}

pub fn insert_draft(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    draft: &CrmNoteDraft,
    idempotency_key: &str,
) -> Result<MutationOutcome, StoreError> {
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
                "INSERT INTO crm_note_drafts \
                 (client_id, draft_id, item_id, source_kind, source_ref, source_user_id, \
                  status, note_body, contact_email, occurred_at, provenance_json, model, \
                  confidence, created_at_ms, updated_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'staged', ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13)",
                params![
                    owned_client,
                    row.draft_id,
                    row.item_id,
                    row.source_kind,
                    row.source_ref,
                    row.source_user_id,
                    row.note_body,
                    row.contact_email,
                    row.occurred_at,
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
                    StoreError::Domain("crm_draft_already_active".to_string())
                }
                other => other.into(),
            })?;
            Ok(())
        },
    )
}

pub use crate::slices::mutation_context::ScopedMutationContext as DraftActionContext;

/// Approve a staged draft: status flip + outbox enqueue, one transaction.
pub fn approve_draft(
    conn: &mut Connection,
    ctx: DraftActionContext<'_>,
    draft_id: &str,
    job: &NewOutboxJob,
) -> Result<MutationOutcome, StoreError> {
    draft_store::approve(conn, ctx.into(), &DRAFT_TABLE, draft_id, Some(job))
}

pub fn reject_draft(
    conn: &mut Connection,
    ctx: DraftActionContext<'_>,
    draft_id: &str,
) -> Result<MutationOutcome, StoreError> {
    draft_store::reject(conn, ctx.into(), &DRAFT_TABLE, draft_id)
}

/// Edit a STAGED draft's AI-filled fields ("AI-produced fields remain
/// editable until accepted"; full replacement, receipted). occurred_at stays
/// grounded from the source email — not editable. Approval builds the
/// provider payload from the stored row, so edits flow into the write.
pub fn update_draft(
    conn: &mut Connection,
    ctx: DraftActionContext<'_>,
    draft_id: &str,
    note_body_raw: &str,
    contact_email_raw: Option<&str>,
) -> Result<MutationOutcome, StoreError> {
    let (current, source_user_id) = require_status(conn, ctx.client_id, draft_id)?;
    ctx.scope.require_source_user(source_user_id.as_deref())?;
    if current != "staged" {
        return Err(StoreError::Domain(format!(
            "crm_draft_not_staged:{current}"
        )));
    }
    let note_body: String = note_body_raw.trim().chars().take(2_000).collect();
    if note_body.is_empty() {
        return Err(StoreError::Domain("crm_draft_body_required".to_string()));
    }
    let contact_email = contact_email_raw.map(str::trim).filter(|s| !s.is_empty());
    if let Some(email) = contact_email {
        if !email.contains('@') {
            return Err(StoreError::Domain(
                "crm_draft_contact_email_invalid".to_string(),
            ));
        }
    }
    let before: serde_json::Value = conn.query_row(
        "SELECT note_body, contact_email FROM crm_note_drafts \
         WHERE client_id = ?1 AND draft_id = ?2",
        params![ctx.client_id, draft_id],
        |row| {
            Ok(serde_json::json!({
                "note_body": row.get::<_, String>(0)?,
                "contact_email": row.get::<_, Option<String>>(1)?,
            }))
        },
    )?;
    let after = serde_json::json!({
        "note_body": note_body, "contact_email": contact_email,
    });
    let owned_client = ctx.client_id.to_string();
    let owned_draft = draft_id.to_string();
    let owned_email = contact_email.map(str::to_string);
    let owned_body = note_body;
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
            before_json: Some(before.to_string()),
            after_json: Some(after.to_string()),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE crm_note_drafts SET note_body = ?3, contact_email = ?4, \
                 updated_at_ms = ?5 WHERE client_id = ?1 AND draft_id = ?2",
                params![
                    owned_client,
                    owned_draft,
                    owned_body,
                    owned_email,
                    now_ms as i64
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
    draft_store::require_status_scoped::<CrmDraftStore>(conn, client_id, draft_id)
}

fn status_from_str(raw: &str) -> CrmDraftStatus {
    match raw {
        "approved" => CrmDraftStatus::Approved,
        "rejected" => CrmDraftStatus::Rejected,
        _ => CrmDraftStatus::Staged,
    }
}
