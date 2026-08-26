//! Invoice draft persistence through store_core. Approval enqueues the
//! provider write outbox job inside the SAME mutation transaction; the
//! approve gate enforces what every invoicing arm needs (customer email,
//! non-zero total) so an undeliverable job is never staged.

use bos_contracts::invoice_drafts::{
    InvoiceDraft, InvoiceDraftLineItem, InvoiceDraftStatus, InvoiceDraftWithRevision,
    InvoiceSettingsUpdateRequest,
};
use bos_contracts::receipt::ActorKindDto;
use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::outbox::{self, NewOutboxJob};
use crate::slices::draft_store::{self, DraftStore, DraftTableSpec};
use crate::store_core::{self, MutationOutcome, MutationRequest, StoreError};

pub const DRAFT_ENTITY_KIND: &str = "invoice_draft";
const APPROVE_SQL: &str = "UPDATE invoice_drafts SET status = 'approved', outbox_job_id = ?3, \
     updated_at_ms = ?4 WHERE client_id = ?1 AND draft_id = ?2";
const REJECT_SQL: &str = "UPDATE invoice_drafts SET status = 'rejected', updated_at_ms = ?3 \
     WHERE client_id = ?1 AND draft_id = ?2";
const DRAFT_TABLE: DraftTableSpec = DraftTableSpec {
    table: InvoiceDraftStore::TABLE,
    entity_kind: DRAFT_ENTITY_KIND,
    not_found_code: InvoiceDraftStore::NOT_FOUND,
    not_staged_code: "invoice_draft_not_staged",
    approve_sql: APPROVE_SQL,
    reject_sql: REJECT_SQL,
};

const DRAFT_COLUMNS: &str = "d.draft_id, d.item_id, d.source_kind, d.source_ref, d.status, \
     d.customer_name, d.customer_email, d.currency, d.line_items_json, d.subtotal_cents, \
     d.total_cents, d.due_date, d.memo, d.provenance_json, d.model, d.confidence, \
     d.outbox_job_id, d.created_at_ms, d.updated_at_ms, COALESCE(er.revision, 0) AS revision";

fn draft_from_row(row: &Row<'_>) -> rusqlite::Result<InvoiceDraftWithRevision> {
    Ok(InvoiceDraftWithRevision {
        draft: InvoiceDraft {
            draft_id: row.get("draft_id")?,
            item_id: row.get("item_id")?,
            source_kind: row.get("source_kind")?,
            source_ref: row.get("source_ref")?,
            status: status_from_str(&row.get::<_, String>("status")?),
            customer_name: row.get("customer_name")?,
            customer_email: row.get("customer_email")?,
            currency: row.get("currency")?,
            line_items: serde_json::from_str(&row.get::<_, String>("line_items_json")?)
                .unwrap_or_default(),
            subtotal_cents: row.get("subtotal_cents")?,
            total_cents: row.get("total_cents")?,
            due_date: row.get("due_date")?,
            memo: row.get("memo")?,
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
    mut entry: InvoiceDraftWithRevision,
) -> Result<InvoiceDraftWithRevision, StoreError> {
    if let Some(job_id) = entry.draft.outbox_job_id.as_deref() {
        entry.outbox_job = outbox::job_summary(conn, client_id, job_id)?;
    }
    Ok(entry)
}

struct InvoiceDraftStore;

impl DraftStore for InvoiceDraftStore {
    type WithRevision = InvoiceDraftWithRevision;

    const TABLE: &'static str = "invoice_drafts";
    const COLUMNS: &'static str = DRAFT_COLUMNS;
    const ENTITY_KIND: &'static str = DRAFT_ENTITY_KIND;
    const NOT_FOUND: &'static str = "invoice_draft_not_found";

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
) -> Result<Option<InvoiceDraftWithRevision>, StoreError> {
    draft_store::active_draft_for_item::<InvoiceDraftStore>(conn, client_id, item_id)
}

pub fn get_draft(
    conn: &Connection,
    client_id: &str,
    draft_id: &str,
) -> Result<Option<InvoiceDraftWithRevision>, StoreError> {
    draft_store::get_draft_unscoped::<InvoiceDraftStore>(conn, client_id, draft_id)
}

pub fn list_drafts(
    conn: &Connection,
    client_id: &str,
    item_id: Option<&str>,
    limit: usize,
) -> Result<Vec<InvoiceDraftWithRevision>, StoreError> {
    draft_store::list_drafts_unscoped::<InvoiceDraftStore>(conn, client_id, item_id, limit)
}

pub fn count_drafts_for_item(
    conn: &Connection,
    client_id: &str,
    item_id: &str,
) -> Result<u64, StoreError> {
    draft_store::count_drafts_for_item::<InvoiceDraftStore>(conn, client_id, item_id)
}

/// Item ids with a STAGED draft (operator decision pending). Feeds the
/// queue's "needs you" decoration via the produce spine.
pub fn staged_item_ids(conn: &Connection, client_id: &str) -> Result<Vec<String>, StoreError> {
    draft_store::staged_item_ids::<InvoiceDraftStore>(conn, client_id)
}

pub fn insert_draft(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    draft: &InvoiceDraft,
    idempotency_key: &str,
) -> Result<MutationOutcome, StoreError> {
    let after = serde_json::to_string(draft)
        .map_err(|err| StoreError::Domain(format!("serialize draft: {err}")))?;
    let line_items_json = serde_json::to_string(&draft.line_items)
        .map_err(|err| StoreError::Domain(format!("serialize line items: {err}")))?;
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
                "INSERT INTO invoice_drafts \
                 (client_id, draft_id, item_id, source_kind, source_ref, status, customer_name, \
                  customer_email, currency, line_items_json, subtotal_cents, total_cents, \
                  due_date, memo, provenance_json, model, confidence, created_at_ms, \
                  updated_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 'staged', ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, \
                         ?14, ?15, ?16, ?17, ?17)",
                params![
                    owned_client,
                    row.draft_id,
                    row.item_id,
                    row.source_kind,
                    row.source_ref,
                    row.customer_name,
                    row.customer_email,
                    row.currency,
                    line_items_json,
                    row.subtotal_cents,
                    row.total_cents,
                    row.due_date,
                    row.memo,
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
                    StoreError::Domain("invoice_draft_already_active".to_string())
                }
                other => other.into(),
            })?;
            Ok(())
        },
    )
}

pub use crate::slices::mutation_context::MutationContext as DraftActionContext;

/// One enrichment-sourced invoice customer value plus the provenance quote
/// backing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomerEnrichedValue {
    pub value: String,
    pub provenance_quote: String,
}

/// Customer fields the invoice enrichment subject may graft onto a staged
/// invoice draft. Money/line fields are intentionally out of scope.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CustomerEnrichmentApply {
    pub customer_name: Option<CustomerEnrichedValue>,
    pub customer_email: Option<CustomerEnrichedValue>,
}

impl CustomerEnrichmentApply {
    pub fn is_empty(&self) -> bool {
        self.customer_name.is_none() && self.customer_email.is_none()
    }
}

/// Graft customer enrichment onto a STAGED invoice draft. Existing fill,
/// operator edits, and CRM billing context win: customer_email fills only NULL;
/// customer_name only replaces a weak domain-like AI prefill.
pub fn apply_customer_enrichment(
    conn: &mut Connection,
    ctx: DraftActionContext<'_>,
    draft_id: &str,
    apply: &CustomerEnrichmentApply,
) -> Result<Option<MutationOutcome>, StoreError> {
    struct Current {
        status: String,
        name: String,
        email: Option<String>,
        provenance_json: String,
    }
    let Current {
        status,
        name,
        email,
        provenance_json,
    } = conn
        .query_row(
            "SELECT status, customer_name, customer_email, provenance_json \
             FROM invoice_drafts WHERE client_id = ?1 AND draft_id = ?2",
            params![ctx.client_id, draft_id],
            |row| {
                Ok(Current {
                    status: row.get(0)?,
                    name: row.get(1)?,
                    email: row.get(2)?,
                    provenance_json: row.get(3)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| StoreError::Domain("invoice_draft_not_found".to_string()))?;
    if status != "staged" {
        return Err(StoreError::Domain(format!(
            "invoice_draft_not_staged:{status}"
        )));
    }
    let mut provenance: Vec<bos_contracts::calendar_drafts::DraftFieldProvenance> =
        serde_json::from_str(&provenance_json).unwrap_or_default();

    let mut changed = false;
    let name = if let Some(incoming) = &apply.customer_name {
        let current = name.trim();
        let replacement = incoming.value.trim();
        let may_replace = !replacement.is_empty()
            && !replacement.eq_ignore_ascii_case(current)
            && !crate::produce::draft_field_policy::is_domain_like_display_name(replacement)
            && crate::produce::draft_field_policy::is_domain_like_display_name(current)
            && crate::produce::draft_field_policy::still_ai_prefill(
                &provenance,
                "customer_name",
                current,
            );
        if may_replace {
            changed = true;
            provenance.retain(|p| p.field != "customer_name");
            provenance.push(bos_contracts::calendar_drafts::DraftFieldProvenance {
                field: "customer_name".to_string(),
                quote: incoming.provenance_quote.clone(),
            });
            replacement.chars().take(200).collect()
        } else {
            name
        }
    } else {
        name
    };

    let email = match (&email, &apply.customer_email) {
        (None, Some(incoming)) => {
            let value = incoming.value.trim();
            if value.contains('@') && !value.contains(char::is_whitespace) {
                changed = true;
                if !provenance.iter().any(|p| p.field == "customer_email") {
                    provenance.push(bos_contracts::calendar_drafts::DraftFieldProvenance {
                        field: "customer_email".to_string(),
                        quote: incoming.provenance_quote.clone(),
                    });
                }
                Some(value.chars().take(300).collect())
            } else {
                None
            }
        }
        _ => email,
    };
    if !changed {
        return Ok(None);
    }

    let provenance_json =
        serde_json::to_string(&provenance).unwrap_or_else(|_| provenance_json.clone());
    let owned_client = ctx.client_id.to_string();
    let owned_draft = draft_id.to_string();
    let now_ms = ctx.now_ms;
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: DRAFT_ENTITY_KIND,
            entity_id: draft_id,
            change_kind: "enrich",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::System,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some("{\"customer_enrichment\":true}".to_string()),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE invoice_drafts SET customer_name = ?3, customer_email = ?4, \
                 provenance_json = ?5, updated_at_ms = ?6 \
                 WHERE client_id = ?1 AND draft_id = ?2",
                params![
                    owned_client,
                    owned_draft,
                    name,
                    email,
                    provenance_json,
                    now_ms as i64
                ],
            )?;
            Ok(())
        },
    )
    .map(Some)
}

/// Approve a staged draft: status flip + Stripe outbox enqueue, one
/// transaction. What Stripe needs is enforced HERE — a customer email and a
/// non-zero total — so an undeliverable job is never staged.
pub fn approve_draft(
    conn: &mut Connection,
    ctx: DraftActionContext<'_>,
    draft_id: &str,
    job: &NewOutboxJob,
) -> Result<MutationOutcome, StoreError> {
    let (status, customer_email, total_cents) = require_draft(conn, ctx.client_id, draft_id)?;
    if status != "staged" {
        return Err(StoreError::Domain(format!(
            "invoice_draft_not_staged:{status}"
        )));
    }
    if customer_email
        .as_deref()
        .map(str::trim)
        .filter(|email| !email.is_empty())
        .is_none()
    {
        return Err(StoreError::Domain(
            "invoice_draft_email_required".to_string(),
        ));
    }
    if total_cents <= 0 {
        return Err(StoreError::Domain(
            "invoice_draft_total_required".to_string(),
        ));
    }
    draft_store::approve(conn, ctx.into(), &DRAFT_TABLE, draft_id, Some(job))
}

pub fn reject_draft(
    conn: &mut Connection,
    ctx: DraftActionContext<'_>,
    draft_id: &str,
) -> Result<MutationOutcome, StoreError> {
    draft_store::reject(conn, ctx.into(), &DRAFT_TABLE, draft_id)
}

/// Edit a STAGED draft. Line items are replaced wholesale; totals are
/// recomputed here (the model's — or the browser's — arithmetic is never
/// trusted). The human IS the grounding for edited amounts.
#[allow(clippy::too_many_arguments)]
pub fn update_draft(
    conn: &mut Connection,
    ctx: DraftActionContext<'_>,
    draft_id: &str,
    customer_name_raw: &str,
    customer_email_raw: Option<&str>,
    due_date_raw: Option<&str>,
    memo_raw: &str,
    line_items_raw: &[InvoiceDraftLineItem],
) -> Result<MutationOutcome, StoreError> {
    let (status, _, _) = require_draft(conn, ctx.client_id, draft_id)?;
    if status != "staged" {
        return Err(StoreError::Domain(format!(
            "invoice_draft_not_staged:{status}"
        )));
    }
    let customer_name: String = customer_name_raw.trim().chars().take(200).collect();
    if customer_name.is_empty() {
        return Err(StoreError::Domain(
            "invoice_draft_customer_required".to_string(),
        ));
    }
    let customer_email = customer_email_raw
        .map(str::trim)
        .filter(|raw| !raw.is_empty());
    if let Some(email) = customer_email {
        if !email.contains('@') || email.contains(char::is_whitespace) {
            return Err(StoreError::Domain(
                "invoice_draft_email_invalid".to_string(),
            ));
        }
    }
    let date_context = crate::slices::datetime_input::context_from_now_ms(ctx.now_ms);
    let due_date = due_date_raw
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
        .map(|date| {
            crate::slices::datetime_input::normalize_civil_date(date, Some(&date_context))
                .map_err(|_| StoreError::Domain("invoice_draft_date_invalid".to_string()))
        })
        .transpose()?;
    if line_items_raw.is_empty() || line_items_raw.len() > 20 {
        return Err(StoreError::Domain(
            "invoice_draft_line_items_invalid".to_string(),
        ));
    }
    let mut line_items: Vec<InvoiceDraftLineItem> = line_items_raw
        .iter()
        .enumerate()
        .map(|(index, line)| InvoiceDraftLineItem {
            line_number: index as u32 + 1,
            label: line.label.trim().chars().take(200).collect(),
            description: line
                .description
                .as_deref()
                .map(str::trim)
                .filter(|raw| !raw.is_empty())
                .map(|raw| raw.chars().take(500).collect()),
            quantity: line.quantity,
            unit_amount_cents: line.unit_amount_cents,
            line_total_cents: 0, // recomputed below
        })
        .collect();
    let subtotal = super::service::recompute_totals(&mut line_items)
        .map_err(|_| StoreError::Domain("invoice_draft_line_items_invalid".to_string()))?;
    let memo: String = memo_raw.trim().chars().take(500).collect();
    let line_items_json = serde_json::to_string(&line_items)
        .map_err(|err| StoreError::Domain(format!("serialize line items: {err}")))?;
    let before: serde_json::Value = conn.query_row(
        "SELECT customer_name, customer_email, due_date, memo, line_items_json, total_cents \
         FROM invoice_drafts WHERE client_id = ?1 AND draft_id = ?2",
        params![ctx.client_id, draft_id],
        |row| {
            Ok(serde_json::json!({
                "customer_name": row.get::<_, String>(0)?,
                "customer_email": row.get::<_, Option<String>>(1)?,
                "due_date": row.get::<_, Option<String>>(2)?,
                "memo": row.get::<_, String>(3)?,
                "line_items": row.get::<_, String>(4)?,
                "total_cents": row.get::<_, i64>(5)?,
            }))
        },
    )?;
    let after = serde_json::json!({
        "customer_name": customer_name, "customer_email": customer_email,
        "due_date": due_date, "memo": memo,
        "line_items": line_items_json, "total_cents": subtotal,
    });
    let owned_client = ctx.client_id.to_string();
    let owned_draft = draft_id.to_string();
    let owned_email = customer_email.map(str::to_string);
    let owned_date = due_date.clone();
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
                "UPDATE invoice_drafts SET customer_name = ?3, customer_email = ?4, \
                 due_date = ?5, memo = ?6, line_items_json = ?7, subtotal_cents = ?8, \
                 total_cents = ?8, updated_at_ms = ?9 \
                 WHERE client_id = ?1 AND draft_id = ?2",
                params![
                    owned_client,
                    owned_draft,
                    customer_name,
                    owned_email,
                    owned_date,
                    memo,
                    line_items_json,
                    subtotal,
                    now_ms as i64
                ],
            )?;
            Ok(())
        },
    )
}

fn require_draft(
    conn: &Connection,
    client_id: &str,
    draft_id: &str,
) -> Result<(String, Option<String>, i64), StoreError> {
    conn.query_row(
        "SELECT status, customer_email, total_cents FROM invoice_drafts \
         WHERE client_id = ?1 AND draft_id = ?2",
        params![client_id, draft_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .optional()?
    .ok_or_else(|| StoreError::Domain("invoice_draft_not_found".to_string()))
}

fn status_from_str(raw: &str) -> InvoiceDraftStatus {
    match raw {
        "approved" => InvoiceDraftStatus::Approved,
        "rejected" => InvoiceDraftStatus::Rejected,
        _ => InvoiceDraftStatus::Staged,
    }
}

pub const INVOICE_SETTINGS_ENTITY_KIND: &str = "invoice_settings";
pub const INVOICE_SETTINGS_ENTITY_ID: &str = "invoice_settings";

#[derive(Debug, Clone)]
pub struct StoredInvoiceSettings {
    pub default_due_days: Option<u32>,
    pub revision: Option<u64>,
}

pub fn get_invoice_settings(
    conn: &Connection,
    client_id: &str,
) -> Result<Option<StoredInvoiceSettings>, StoreError> {
    let row: Option<Option<i64>> = conn
        .query_row(
            "SELECT default_due_days FROM invoice_settings WHERE client_id = ?1",
            params![client_id],
            |r| r.get::<_, Option<i64>>(0),
        )
        .optional()?;
    let Some(default_due_days) = row else {
        return Ok(None);
    };
    let revision = store_core::current_revision(
        conn,
        client_id,
        INVOICE_SETTINGS_ENTITY_KIND,
        INVOICE_SETTINGS_ENTITY_ID,
    )?;
    Ok(Some(StoredInvoiceSettings {
        default_due_days: default_due_days.map(|days| days as u32),
        revision,
    }))
}

pub fn replace_invoice_settings(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    request: &InvoiceSettingsUpdateRequest,
    now_ms: u64,
) -> Result<MutationOutcome, StoreError> {
    let before_json = get_invoice_settings(conn, client_id)?
        .and_then(|settings| serde_json::to_string(&settings.default_due_days).ok());
    let after_json = serde_json::to_string(&request.default_due_days).ok();
    let due_days = request.default_due_days;
    let owned_client = client_id.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: INVOICE_SETTINGS_ENTITY_KIND,
            entity_id: INVOICE_SETTINGS_ENTITY_ID,
            change_kind: "replace",
            actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: request.expected_revision,
            idempotency_key: &request.idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json,
            after_json,
            now_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO invoice_settings (client_id, default_due_days, updated_at_ms) \
                 VALUES (?1, ?2, ?3) \
                 ON CONFLICT(client_id) DO UPDATE SET \
                   default_due_days = excluded.default_due_days, \
                   updated_at_ms = excluded.updated_at_ms",
                params![
                    &owned_client,
                    due_days.map(|days| days as i64),
                    now_ms as i64
                ],
            )?;
            Ok(())
        },
    )
}
