//! Owner reporting digest contracts (port #7, W16): a deterministic weekly +
//! month-to-date digest assembled from the LOCAL caches. Every money figure
//! is READ from the accounting snapshots (the digest mirrors the Accounting
//! view's basis labeling, including the invoice_totals caveat) — the ONE
//! LLM transform writes prose only (headline/narrative/callouts), and any
//! dollar amount in that prose must literally appear in the input metrics.

use serde::{Deserialize, Serialize};

use crate::calendar_drafts::OutboxJobSummary;
use crate::search_console::{
    AnalyticsBreakdownRow, AnalyticsMetricTotals, SearchConsoleMetricTotals,
};

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnerReportPeriodKind {
    Weekly,
    Mtd,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnerReportStatus {
    /// Metrics + narration both present.
    Complete,
    /// Metrics present; the narration transform failed (digest still usable).
    NarrationFailed,
}

/// Sales plus the configured management metric, read from the accounting
/// slice's cached snapshots — the same numbers the Accounting tab shows,
/// basis labeled the same way.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigestSalesMetrics {
    /// "quickbooks_pnl" | "invoice_totals" (invoice_totals counts invoices
    /// only — no sales receipts/credit notes; margin fields absent).
    pub basis: String,
    /// Configured management metric basis ("gross_margin" |
    /// "adjusted_gross_sales" | "invoice_totals").
    #[serde(default)]
    pub metric_basis: String,
    /// Operator-facing label for the configured management metric.
    #[serde(default)]
    pub metric_basis_label: String,
    /// Sales for THIS report's period (week-to-date or month-to-date).
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub period_sales_cents: i64,
    /// The prior comparable period (full prior week / prior month-to-date
    /// equivalent per the accounting view's comparison).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub prior_period_sales_cents: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub mtd_gross_profit_cents: Option<i64>,
    /// Avg monthly margin of the previous four completed quarters — the
    /// pilot baseline (§4). None until all twelve months are cached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub baseline_monthly_margin_cents: Option<i64>,
    /// THE pilot payment metric (read from the accounting slice, never
    /// recomputed here).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub margin_above_baseline_cents: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub metric_value_cents: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub metric_baseline_cents: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub metric_above_baseline_cents: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metric_pending_reason: Option<String>,
    pub baseline_months_cached: u32,
    /// Accounting cache freshness at assembly time (honesty: stale cache =
    /// stale digest).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub last_synced_at_ms: Option<u64>,
}

fn default_call_metric_label() -> String {
    "Incoming calls".to_string()
}

fn default_call_metric_source_label() -> String {
    "Email-derived call summaries".to_string()
}

fn default_call_metric_configured() -> bool {
    true
}

fn default_metric_configured() -> bool {
    true
}

/// Incoming call volume from a configured email-derived category.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigestCallMetrics {
    /// Inbound messages in the configured call-summary category within the period.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub call_log_messages: u64,
    /// Calls whose Ruby summary says the caller was successfully transferred.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub transfer_successful: u64,
    /// Calls whose Ruby summary says someone needs to call/contact the caller.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub callback_needed: u64,
    /// Calls whose Ruby summary says no callback is needed.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub no_callback_needed: u64,
    /// Calls whose Ruby summary did not expose a supported outcome signal.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub unknown_outcome: u64,
    /// Operator-facing metric label, configured per client.
    #[serde(default = "default_call_metric_label")]
    pub label: String,
    /// Source/coverage explanation, e.g. "Ruby-summary calls, not direct calls".
    #[serde(default = "default_call_metric_source_label")]
    pub source_label: String,
    /// False when the required email-triage category config is missing; the
    /// count must be rendered as pending data instead of as zero.
    #[serde(default = "default_call_metric_configured")]
    pub configured: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_reason: Option<String>,
}

/// Follow-up completion: stored open/done counts plus the watchdog's
/// read-time escalation lanes (follow_up_tasks slice classification).
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigestFollowUpMetrics {
    /// Open tasks right now (point-in-time, not windowed).
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub open: u64,
    /// Tasks marked done within the period.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub done_in_period: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub due_today: u64,
    /// All overdue open tasks (missed + escalated + critical).
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub overdue: u64,
    /// Overdue past the escalation threshold (excludes critical).
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub escalated: u64,
    /// Overdue past the critical threshold.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub critical: u64,
}

/// Order control from the cached Stockforge order board. Orders-in-period is
/// windowed by order date; the flag counts are the CURRENT backlog.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigestOrderMetrics {
    #[serde(default = "default_metric_configured")]
    pub configured: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_reason: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub orders_in_period: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub exceptions: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub deduction_failed: u64,
    /// SKU reconciliation backlog (orders needing mapping).
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub needs_mapping: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub packed_missing_photo: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub blocked: u64,
}

/// Point-in-time inventory health from the same cached computation as the
/// Inventory tab. This is a first-class owner-digest section, not a second
/// reporting stack.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigestInventoryMetrics {
    #[serde(default)]
    pub configured: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_reason: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub stocked_sku_count: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub out_of_stock_count: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub critical_count: u64,
    /// Stocked/monitored valuation, matching InventoryStockKpis.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub stock_value_cents: i64,
    /// Total estimated cost of POs that are not RECEIVED/CANCELLED.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub inbound_open_po_cents: i64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigestSeverityCount {
    pub severity: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub count: u64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigestStatusCount {
    pub status: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub count: u64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigestDamageTypeCount {
    pub damage_type: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub count: u64,
}

/// Damage/claims activity from the cached damage snapshots + claim drafts.
/// This is reporting only: it describes observed damage, queue lifecycle, and
/// local claim-packet status. It does not imply carrier/insurance submission.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigestClaimMetrics {
    #[serde(default = "default_metric_configured")]
    pub configured: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_reason: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub damage_events_in_period: u64,
    /// Damage rows whose source status is still open.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub damage_open: u64,
    /// Damage rows whose source status is present and no longer open.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub damage_resolved: u64,
    pub damage_by_severity: Vec<DigestSeverityCount>,
    #[serde(default)]
    pub damage_by_status: Vec<DigestStatusCount>,
    #[serde(default)]
    pub damage_by_type: Vec<DigestDamageTypeCount>,
    /// Current BusinessOS queue state for damage items in the period.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub queue_open: u64,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub queue_accepted: u64,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub queue_dismissed: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub claims_drafted_in_period: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub claims_approved_in_period: u64,
    #[serde(default)]
    pub claim_drafts_by_status: Vec<DigestStatusCount>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigestTrafficMetrics {
    /// Search Console property/access configured for organic-search reporting.
    pub configured: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub property_url: Option<String>,
    pub has_data: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_synced_at_ms: Option<u64>,
    pub totals: SearchConsoleMetricTotals,
    pub branded: SearchConsoleMetricTotals,
    pub nonbranded: SearchConsoleMetricTotals,
    /// GA4-style behavior/acquisition reporting is configured separately from
    /// Search Console. False means BOS should render a pending setup state,
    /// not zero sessions.
    #[serde(default)]
    pub behavior_configured: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior_pending_reason: Option<String>,
    /// Conversion events are a separate setup step from installing analytics.
    #[serde(default)]
    pub conversion_tracking_configured: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversion_tracking_pending_reason: Option<String>,
    /// Retargeting pixels/audiences are marketing implementation outside BOS
    /// provider writes unless separately designed and approved.
    #[serde(default)]
    pub retargeting_configured: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retargeting_pending_reason: Option<String>,
    #[serde(default)]
    pub behavior_has_data: bool,
    #[serde(default)]
    pub behavior_week: AnalyticsMetricTotals,
    #[serde(default)]
    pub behavior_month_to_date: AnalyticsMetricTotals,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub top_landing_pages_week: Vec<AnalyticsBreakdownRow>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub top_sources_week: Vec<AnalyticsBreakdownRow>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DigestDealMetricsStatus {
    Available,
    #[default]
    PendingConfig,
    LimitedData,
}

/// HubSpot deal close-rate reporting. The pipeline, stage ids, date fields,
/// and optional segment cuts are deployment config; BusinessOS only computes
/// from that mapping and reports when the data is unavailable or incomplete.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DigestDealMetrics {
    pub status: DigestDealMetricsStatus,
    pub source: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub closed_deals: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub won_deals: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub lost_deals: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub close_rate_bps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub avg_contact_to_close_days: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub contact_to_close_sample: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub segment_cuts: Vec<String>,
}

/// The full deterministic metric set for one digest period — the narration
/// transform's ONLY input, and the grounding set for its dollar amounts.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerDigestMetrics {
    /// Ordered metric ids active for this report profile. Empty means a
    /// historical report generated before profile scoping existed.
    #[serde(default)]
    pub metric_sections: Vec<String>,
    pub sales: DigestSalesMetrics,
    pub calls: DigestCallMetrics,
    pub follow_ups: DigestFollowUpMetrics,
    pub orders: DigestOrderMetrics,
    #[serde(default)]
    pub inventory: DigestInventoryMetrics,
    pub claims: DigestClaimMetrics,
    pub traffic: DigestTrafficMetrics,
    #[serde(default)]
    pub deals: DigestDealMetrics,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerReport {
    /// "owr_<kind>_<period_start>" — deterministic; regeneration upserts.
    pub report_id: String,
    pub period_kind: OwnerReportPeriodKind,
    pub period_start: String,
    /// Inclusive end of the data window (the as-of date for current periods).
    pub period_end: String,
    /// The civil date the metrics were assembled (stale when != today).
    pub as_of_date: String,
    pub status: OwnerReportStatus,
    pub metrics: OwnerDigestMetrics,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headline: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub narrative: Option<String>,
    pub callouts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub narration_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outbox_job_id: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub generated_at_ms: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub created_at_ms: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub updated_at_ms: u64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerReportWithRevision {
    pub report: OwnerReport,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub revision: u64,
    /// Delivery state of the digest email's Gmail-draft job, when staged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outbox_job: Option<OutboxJobSummary>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerReportsResponse {
    pub reports: Vec<OwnerReportWithRevision>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerReportGenerateResponse {
    pub accepted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerReportEmailRequest {
    pub idempotency_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_id: Option<String>,
}
