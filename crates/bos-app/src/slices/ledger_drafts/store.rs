//! Ledger entry draft persistence through store_core. Approval enqueues the
//! provider-write outbox job inside the SAME mutation transaction.

use bos_contracts::ledger_drafts::{LedgerDraftStatus, LedgerDraftWithRevision, LedgerEntryDraft};
use bos_contracts::receipt::ActorKindDto;
use rusqlite::{params, Connection, Row};

use crate::outbox::{self, NewOutboxJob};
use crate::slices::draft_store::{self, DraftStore, DraftTableSpec};
use crate::store_core::{self, MutationOutcome, MutationRequest, StoreError};

pub const DRAFT_ENTITY_KIND: &str = "ledger_entry_draft";
const APPROVE_SQL: &str =
    "UPDATE ledger_entry_drafts SET status = 'approved', outbox_job_id = ?3, \
     updated_at_ms = ?4 WHERE client_id = ?1 AND draft_id = ?2";
const REJECT_SQL: &str = "UPDATE ledger_entry_drafts SET status = 'rejected', updated_at_ms = ?3 \
     WHERE client_id = ?1 AND draft_id = ?2";
const DRAFT_TABLE: DraftTableSpec = DraftTableSpec {
    table: LedgerDraftStore::TABLE,
    entity_kind: DRAFT_ENTITY_KIND,
    not_found_code: LedgerDraftStore::NOT_FOUND,
    not_staged_code: "ledger_draft_not_staged",
    approve_sql: APPROVE_SQL,
    reject_sql: REJECT_SQL,
};

const DRAFT_COLUMNS: &str = "d.draft_id, d.item_id, d.source_kind, d.source_ref, d.status, \
     d.payer_name, d.payer_email, d.amount_cents, d.paid_date, d.description, \
     d.provenance_json, d.model, d.confidence, d.outbox_job_id, d.created_at_ms, \
     d.updated_at_ms, COALESCE(er.revision, 0) AS revision";

fn draft_from_row(row: &Row<'_>) -> rusqlite::Result<LedgerDraftWithRevision> {
    Ok(LedgerDraftWithRevision {
        draft: LedgerEntryDraft {
            draft_id: row.get("draft_id")?,
            item_id: row.get("item_id")?,
            source_kind: row.get("source_kind")?,
            source_ref: row.get("source_ref")?,
            status: status_from_str(&row.get::<_, String>("status")?),
            payer_name: row.get("payer_name")?,
            payer_email: row.get("payer_email")?,
            amount_cents: row.get("amount_cents")?,
            paid_date: row.get("paid_date")?,
            description: row.get("description")?,
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
    mut entry: LedgerDraftWithRevision,
) -> Result<LedgerDraftWithRevision, StoreError> {
    if let Some(job_id) = entry.draft.outbox_job_id.as_deref() {
        entry.outbox_job = outbox::job_summary(conn, client_id, job_id)?;
    }
    Ok(entry)
}

struct LedgerDraftStore;

impl DraftStore for LedgerDraftStore {
    type WithRevision = LedgerDraftWithRevision;

    const TABLE: &'static str = "ledger_entry_drafts";
    const COLUMNS: &'static str = DRAFT_COLUMNS;
    const ENTITY_KIND: &'static str = DRAFT_ENTITY_KIND;
    const NOT_FOUND: &'static str = "ledger_draft_not_found";

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

pub fn active_draft_for_item(
    conn: &Connection,
    client_id: &str,
    item_id: &str,
) -> Result<Option<LedgerDraftWithRevision>, StoreError> {
    draft_store::active_draft_for_item::<LedgerDraftStore>(conn, client_id, item_id)
}

pub fn get_draft(
    conn: &Connection,
    client_id: &str,
    draft_id: &str,
) -> Result<Option<LedgerDraftWithRevision>, StoreError> {
    draft_store::get_draft_unscoped::<LedgerDraftStore>(conn, client_id, draft_id)
}

pub fn list_drafts(
    conn: &Connection,
    client_id: &str,
    item_id: Option<&str>,
    limit: usize,
) -> Result<Vec<LedgerDraftWithRevision>, StoreError> {
    draft_store::list_drafts_unscoped::<LedgerDraftStore>(conn, client_id, item_id, limit)
}

pub fn count_drafts_for_item(
    conn: &Connection,
    client_id: &str,
    item_id: &str,
) -> Result<u64, StoreError> {
    draft_store::count_drafts_for_item::<LedgerDraftStore>(conn, client_id, item_id)
}

/// Item ids with a STAGED draft (operator decision pending). Feeds the
/// queue's "needs you" decoration via the produce spine.
pub fn staged_item_ids(conn: &Connection, client_id: &str) -> Result<Vec<String>, StoreError> {
    draft_store::staged_item_ids::<LedgerDraftStore>(conn, client_id)
}

pub fn insert_draft(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    draft: &LedgerEntryDraft,
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
                "INSERT INTO ledger_entry_drafts \
                 (client_id, draft_id, item_id, source_kind, source_ref, status, payer_name, \
                  payer_email, amount_cents, paid_date, description, provenance_json, model, \
                  confidence, created_at_ms, updated_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 'staged', ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14)",
                params![
                    owned_client,
                    row.draft_id,
                    row.item_id,
                    row.source_kind,
                    row.source_ref,
                    row.payer_name,
                    row.payer_email,
                    row.amount_cents,
                    row.paid_date,
                    row.description,
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
                    StoreError::Domain("ledger_draft_already_active".to_string())
                }
                other => other.into(),
            })?;
            Ok(())
        },
    )
}

pub use crate::slices::mutation_context::MutationContext as DraftActionContext;

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

/// Edit a STAGED draft's AI-filled fields (full replacement, receipted).
/// Every field is editable — when the operator changes a value, the human IS
/// the grounding. Approval builds the provider payload from the stored row,
/// so edits flow into the write.
#[allow(clippy::too_many_arguments)]
pub fn update_draft(
    conn: &mut Connection,
    ctx: DraftActionContext<'_>,
    draft_id: &str,
    payer_name_raw: &str,
    payer_email_raw: Option<&str>,
    amount_cents: i64,
    paid_date_raw: &str,
    description_raw: &str,
) -> Result<MutationOutcome, StoreError> {
    let current = require_status(conn, ctx.client_id, draft_id)?;
    if current != "staged" {
        return Err(StoreError::Domain(format!(
            "ledger_draft_not_staged:{current}"
        )));
    }
    let payer_name: String = payer_name_raw.trim().chars().take(200).collect();
    if payer_name.is_empty() {
        return Err(StoreError::Domain(
            "ledger_draft_payer_required".to_string(),
        ));
    }
    if amount_cents <= 0 {
        return Err(StoreError::Domain(
            "ledger_draft_amount_invalid".to_string(),
        ));
    }
    let date_context = crate::slices::datetime_input::context_from_now_ms(ctx.now_ms);
    let paid_date =
        crate::slices::datetime_input::normalize_civil_date(paid_date_raw, Some(&date_context))
            .map_err(|_| StoreError::Domain("ledger_draft_date_invalid".to_string()))?;
    let payer_email = payer_email_raw.map(str::trim).filter(|raw| !raw.is_empty());
    if let Some(email) = payer_email {
        if !email.contains('@') {
            return Err(StoreError::Domain(
                "ledger_draft_payer_email_invalid".to_string(),
            ));
        }
    }
    let description: String = description_raw.trim().chars().take(500).collect();
    let before: serde_json::Value = conn.query_row(
        "SELECT payer_name, payer_email, amount_cents, paid_date, description \
         FROM ledger_entry_drafts WHERE client_id = ?1 AND draft_id = ?2",
        params![ctx.client_id, draft_id],
        |row| {
            Ok(serde_json::json!({
                "payer_name": row.get::<_, String>(0)?,
                "payer_email": row.get::<_, Option<String>>(1)?,
                "amount_cents": row.get::<_, i64>(2)?,
                "paid_date": row.get::<_, String>(3)?,
                "description": row.get::<_, String>(4)?,
            }))
        },
    )?;
    let after = serde_json::json!({
        "payer_name": payer_name, "payer_email": payer_email,
        "amount_cents": amount_cents, "paid_date": paid_date,
        "description": description,
    });
    let owned_client = ctx.client_id.to_string();
    let owned_draft = draft_id.to_string();
    let owned_email = payer_email.map(str::to_string);
    let owned_date = paid_date.clone();
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
                "UPDATE ledger_entry_drafts SET payer_name = ?3, payer_email = ?4, \
                 amount_cents = ?5, paid_date = ?6, description = ?7, updated_at_ms = ?8 \
                 WHERE client_id = ?1 AND draft_id = ?2",
                params![
                    owned_client,
                    owned_draft,
                    payer_name,
                    owned_email,
                    amount_cents,
                    owned_date,
                    description,
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
) -> Result<String, StoreError> {
    draft_store::require_status_unscoped::<LedgerDraftStore>(conn, client_id, draft_id)
}

fn status_from_str(raw: &str) -> LedgerDraftStatus {
    match raw {
        "approved" => LedgerDraftStatus::Approved,
        "rejected" => LedgerDraftStatus::Rejected,
        _ => LedgerDraftStatus::Staged,
    }
}
