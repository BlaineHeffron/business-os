//! Invoice draft contracts: the produce → approve → provider-write vertical
//! for the `invoice_draft` packet kind (drafting a Stripe invoice from a
//! note/email that describes billable work — Avery's own invoicing, not a
//! Demo workflow). Money is grounded: every line item's amount must carry a
//! literal provenance quote from the source, and line/total math is
//! recomputed deterministically — the model never does arithmetic. Approval
//! enqueues a Stripe create-invoice-draft outbox job; the client dry-runs
//! until BOS_STRIPE_WRITE_ENABLED opens, and even live the invoice stays a
//! Stripe DRAFT (finalize/send is a human action in the Stripe dashboard).

use serde::{Deserialize, Serialize};

use crate::calendar_drafts::{DraftFieldProvenance, OutboxJobSummary};

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvoiceDraftStatus {
    Staged,
    Approved,
    Rejected,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvoiceDraftLineItem {
    /// 1-based; stable across edits (feeds the per-line idempotency key).
    pub line_number: u32,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub quantity: u32,
    /// Integer cents — REQUIRES a literal provenance quote from the source
    /// (field "line_{n}_amount") when AI-filled; the human is the grounding
    /// after an edit.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub unit_amount_cents: i64,
    /// Always recomputed server-side as quantity × unit_amount_cents.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub line_total_cents: i64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvoiceDraft {
    /// "inv_<item_id>_<attempt>" — one active (non-rejected) draft per item.
    pub draft_id: String,
    pub item_id: String,
    pub source_kind: String,
    pub source_ref: String,
    pub status: InvoiceDraftStatus,
    /// Who gets billed (Stripe customer name).
    pub customer_name: String,
    /// Required for approval — Stripe's find-or-create key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer_email: Option<String>,
    /// ISO 4217 (v1 stages "usd").
    pub currency: String,
    pub line_items: Vec<InvoiceDraftLineItem>,
    /// Recomputed server-side from the line items; equals total in v1
    /// (no tax/discount lines yet).
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub subtotal_cents: i64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub total_cents: i64,
    /// YYYY-MM-DD, only when the source states one (never invented).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_date: Option<String>,
    /// Lands on the Stripe invoice's description/memo field.
    pub memo: String,
    pub provenance: Vec<DraftFieldProvenance>,
    pub model: String,
    pub confidence: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outbox_job_id: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub created_at_ms: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub updated_at_ms: u64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvoiceDraftWithRevision {
    pub draft: InvoiceDraft,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outbox_job: Option<OutboxJobSummary>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvoiceDraftsResponse {
    pub drafts: Vec<InvoiceDraftWithRevision>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvoiceDraftProduceRequest {
    pub item_id: String,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvoiceDraftProduceResponse {
    pub draft: InvoiceDraftWithRevision,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvoiceDraftActionKind {
    Approve,
    Reject,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvoiceDraftActionRequest {
    pub action: InvoiceDraftActionKind,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

/// Operator edit of a STAGED draft. Line items are replaced wholesale; the
/// server recomputes totals and re-validates the math — the human IS the
/// grounding when they change an amount.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvoiceDraftUpdateRequest {
    pub customer_name: String,
    #[serde(default)]
    pub customer_email: Option<String>,
    #[serde(default)]
    pub due_date: Option<String>,
    pub memo: String,
    pub line_items: Vec<InvoiceDraftLineItem>,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

/// Operator-configurable invoicing defaults (Settings → Invoicing). The
/// default payment term in days (Net N), when set, is applied to a produced
/// invoice when the source states no explicit due date and no "Net N" term.
/// None = no default (due date stays blank).
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvoiceSettingsResponse {
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub revision: Option<u64>,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub default_due_days: Option<u32>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvoiceSettingsUpdateRequest {
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub default_due_days: Option<u32>,
}
