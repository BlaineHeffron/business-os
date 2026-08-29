//! Accounting view contracts (provider-agnostic: QuickBooks, Invoice Ninja, …): connector status, sync state, and the
//! cached invoice/aging/sales/customer read models. Every response that
//! renders accounting data carries [`AccountingSyncInfo`] so the UI can say how fresh the
//! numbers are — the browser never talks to QBO, only to the local cache.

use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountingConnectorStatus {
    /// Which accounting provider this instance talks to ("qbo" | "invoice_ninja").
    pub provider: String,
    pub connected: bool,
    /// True when the stored OAuth grant cannot refresh and the operator must
    /// complete the provider consent flow again.
    #[serde(default)]
    pub reconnect_required: bool,
    /// Safe machine-readable cause. This field never contains provider tokens
    /// or raw provider responses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub realm_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connected_by: Option<String>,
    /// When the rotating refresh token dies if never used again (~100 days
    /// from the last refresh) — the UI warns as it approaches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub refresh_token_expires_at_ms: Option<u64>,
    /// Present when connection or reconnection is available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connect_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
}

/// Sync freshness attached to every QBO view response.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountingSyncInfo {
    /// The background pump's env gate (manual sync works regardless).
    pub sync_enabled: bool,
    pub in_flight: bool,
    /// False until the initial full walk of both entities completes.
    pub backfill_complete: bool,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub last_synced_at_ms: Option<u64>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub invoice_count: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub customer_count: u64,
    /// QBO API requests spent by the most recent cycle (budget visibility).
    pub last_requests_used: u32,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub next_sync_allowed_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountingInvoiceRow {
    pub invoice_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc_number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub txn_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_date: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub total_cents: i64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub balance_cents: i64,
    /// open | overdue | paid | voided.
    pub status: String,
    /// Days past due (0 when not overdue or no due date).
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub days_overdue: i64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountingInvoicesResponse {
    pub invoices: Vec<AccountingInvoiceRow>,
    pub sync: AccountingSyncInfo,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountingAgingBucket {
    /// current | days_1_30 | days_31_60 | days_61_90 | days_90_plus | no_due_date.
    pub bucket: String,
    pub label: String,
    pub invoice_count: u32,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub balance_cents: i64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountingAgingResponse {
    pub buckets: Vec<AccountingAgingBucket>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub total_open_cents: i64,
    pub sync: AccountingSyncInfo,
}

/// One cached month of ProfitAndLoss totals (the margin trend's bars).
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountingPnlMonth {
    /// First day of the month (YYYY-MM-DD).
    pub month_start: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub total_income_cents: i64,
    /// None for providers without cost data (basis "invoice_totals").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub total_cogs_cents: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub gross_profit_cents: Option<i64>,
    /// False = the month is still in progress (partial totals).
    pub is_complete: bool,
}

/// Owner financials — sales pace plus the configured management metric. Gross
/// margin remains the default metric when P&L data exists; deployments may
/// configure another basis such as adjusted gross sales.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountingFinancialsResponse {
    /// What the sales numbers come from ("quickbooks_pnl" | "invoice_totals").
    pub basis: String,
    /// Configured management metric basis ("gross_margin" |
    /// "adjusted_gross_sales" | "invoice_totals").
    #[serde(default)]
    pub metric_basis: String,
    /// Operator-facing label for the configured metric.
    #[serde(default)]
    pub metric_basis_label: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub week_to_date_cents: i64,
    /// The last FULL week's income (None until cached).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub prior_week_cents: Option<i64>,
    /// Prior week through the same weekday as `week_to_date_cents` where the
    /// provider cache has enough daily rows to compute it exactly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub prior_week_to_date_cents: Option<i64>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub month_to_date_cents: i64,
    /// Full prior completed month's income. Used for completed-month context,
    /// not for in-progress MTD deltas.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub prior_month_cents: Option<i64>,
    /// Prior month through the same day-of-month as `month_to_date_cents`
    /// where the provider cache has enough daily rows to compute it exactly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub prior_month_to_date_cents: Option<i64>,
    /// None for providers without cost data (basis "invoice_totals").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub mtd_gross_profit_cents: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub mtd_cogs_cents: Option<i64>,
    /// Average monthly gross margin of the previous four completed quarters
    /// — None until all twelve baseline months are cached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub baseline_monthly_margin_cents: Option<i64>,
    /// How many of the twelve baseline months are cached so far.
    pub baseline_months_cached: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_window_start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_window_end: Option<String>,
    /// THE pilot payment metric: this month's gross margin minus the
    /// baseline. None until the baseline is computable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub margin_above_baseline_cents: Option<i64>,
    /// Current month-to-date value for the configured management metric. None
    /// means the metric is pending/limited and `metric_pending_reason` explains why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub metric_value_cents: Option<i64>,
    /// Baseline value for the configured management metric. For gross margin
    /// this is the computed trailing four-quarter average; for imported bases
    /// this can come from deployment config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub metric_baseline_cents: Option<i64>,
    /// Configured metric minus its baseline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub metric_above_baseline_cents: Option<i64>,
    /// Present when missing inputs or baseline make the configured metric
    /// limited rather than computable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metric_pending_reason: Option<String>,
    /// All cached months oldest-first (trend bars; includes the partial
    /// current month flagged is_complete=false).
    pub months: Vec<AccountingPnlMonth>,
    pub sync: AccountingSyncInfo,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountingCustomerRow {
    pub customer_id: String,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    pub active: bool,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountingCustomersResponse {
    pub customers: Vec<AccountingCustomerRow>,
    pub sync: AccountingSyncInfo,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountingSyncNowResponse {
    pub accepted: bool,
    /// Refusal reason when not accepted: sync_in_flight | sync_cooldown |
    /// qbo_not_connected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub next_allowed_at_ms: u64,
}
