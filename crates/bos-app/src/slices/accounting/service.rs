//! QBO connect-flow assembly, connector status, and the pure view
//! computations (aging buckets, invoice-based sales summary). All view math
//! runs over the local snapshot cache — nothing here talks to QBO.

use bos_contracts::accounting::{
    AccountingAgingBucket, AccountingConnectorStatus, AccountingCustomerRow, AccountingInvoiceRow,
    AccountingSyncInfo,
};
use bos_integrations::qbo_oauth::{self, QboEnvironment, QboOAuthApp};
use rusqlite::{params, Connection};

use super::store::{self, CustomerSnapshotRow, InvoiceSnapshotRow};
use crate::env_registry;
use crate::http::OperatorScope;
use crate::overlay::{AccountingMetricBasisOverlay, AccountingVisibilityPolicy};
use crate::store_core::StoreError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountingMetricBasisKind {
    GrossMargin,
    AdjustedGrossSales,
    InvoiceTotals,
}

impl AccountingMetricBasisKind {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "gross_margin" | "quickbooks_pnl" => Some(Self::GrossMargin),
            "adjusted_gross_sales" => Some(Self::AdjustedGrossSales),
            "invoice_totals" => Some(Self::InvoiceTotals),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::GrossMargin => "gross_margin",
            Self::AdjustedGrossSales => "adjusted_gross_sales",
            Self::InvoiceTotals => "invoice_totals",
        }
    }

    pub fn default_label(self) -> &'static str {
        match self {
            Self::GrossMargin => "Gross margin",
            Self::AdjustedGrossSales => "Adjusted gross sales",
            Self::InvoiceTotals => "Invoice totals",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountingMetricBasisConfig {
    pub basis: AccountingMetricBasisKind,
    pub label: String,
    pub baseline_cents: Option<i64>,
    pub freight_cents: Option<i64>,
    pub taxes_cents: Option<i64>,
    pub insurance_cents: Option<i64>,
    pub basis_explicit: bool,
    pub label_explicit: bool,
    pub configured: bool,
}

impl Default for AccountingMetricBasisConfig {
    fn default() -> Self {
        Self {
            basis: AccountingMetricBasisKind::GrossMargin,
            label: AccountingMetricBasisKind::GrossMargin
                .default_label()
                .to_string(),
            baseline_cents: None,
            freight_cents: None,
            taxes_cents: None,
            insurance_cents: None,
            basis_explicit: false,
            label_explicit: false,
            configured: false,
        }
    }
}

fn env_i64(var: &env_registry::EnvVar) -> Result<Option<i64>, StoreError> {
    let Some(raw) = env_registry::string(var) else {
        return Ok(None);
    };
    raw.trim()
        .parse::<i64>()
        .map(Some)
        .map_err(|_| StoreError::Domain(format!("{} must be an integer number of cents", var.name)))
}

pub fn metric_basis_config_from_sources(
    overlay: Option<&AccountingMetricBasisOverlay>,
) -> Result<AccountingMetricBasisConfig, StoreError> {
    let overlay_basis = overlay
        .map(|config| config.basis.trim())
        .filter(|basis| !basis.is_empty());
    let raw_basis = env_registry::string(&env_registry::BOS_ACCOUNTING_METRIC_BASIS)
        .filter(|basis| !basis.trim().is_empty())
        .or_else(|| overlay_basis.map(str::to_string));
    let basis_explicit = raw_basis.is_some();
    let basis = match raw_basis {
        Some(raw) => AccountingMetricBasisKind::parse(&raw).ok_or_else(|| {
            StoreError::Domain(format!(
                "unknown BOS_ACCOUNTING_METRIC_BASIS/accounting.metric_basis.basis: {}",
                raw.trim()
            ))
        })?,
        None => AccountingMetricBasisKind::GrossMargin,
    };
    let raw_label =
        env_registry::string(&env_registry::BOS_ACCOUNTING_METRIC_LABEL).or_else(|| {
            overlay
                .map(|config| config.label.trim())
                .filter(|label| !label.is_empty())
                .map(str::to_string)
        });
    let label_explicit = raw_label.is_some();
    let label = raw_label.unwrap_or_else(|| basis.default_label().to_string());
    let baseline_cents = env_i64(&env_registry::BOS_ACCOUNTING_METRIC_BASELINE_CENTS)?
        .or_else(|| overlay.and_then(|config| config.baseline_cents));
    let freight_cents = env_i64(&env_registry::BOS_ACCOUNTING_METRIC_ADJUSTED_FREIGHT_CENTS)?
        .or_else(|| overlay.and_then(|config| config.freight_cents));
    let taxes_cents = env_i64(&env_registry::BOS_ACCOUNTING_METRIC_ADJUSTED_TAXES_CENTS)?
        .or_else(|| overlay.and_then(|config| config.taxes_cents));
    let insurance_cents = env_i64(&env_registry::BOS_ACCOUNTING_METRIC_ADJUSTED_INSURANCE_CENTS)?
        .or_else(|| overlay.and_then(|config| config.insurance_cents));
    let configured = basis_explicit
        || label_explicit
        || baseline_cents.is_some()
        || freight_cents.is_some()
        || taxes_cents.is_some()
        || insurance_cents.is_some();
    Ok(AccountingMetricBasisConfig {
        basis,
        label,
        baseline_cents,
        freight_cents,
        taxes_cents,
        insurance_cents,
        basis_explicit,
        label_explicit,
        configured,
    })
}

pub(crate) fn effective_metric_config_for_provider(
    provider: &str,
    config: &AccountingMetricBasisConfig,
) -> AccountingMetricBasisConfig {
    if provider_supports_pnl(provider)
        || config.basis_explicit
        || config.basis != AccountingMetricBasisKind::GrossMargin
    {
        return config.clone();
    }
    AccountingMetricBasisConfig {
        basis: AccountingMetricBasisKind::InvoiceTotals,
        label: if config.label_explicit {
            config.label.clone()
        } else {
            AccountingMetricBasisKind::InvoiceTotals
                .default_label()
                .to_string()
        },
        ..config.clone()
    }
}

/// The configured accounting provider. Unknown values are a loud error —
/// never a silent wrong-provider sync (the BOS_CRM_PROVIDER doctrine).
pub fn configured_accounting_provider() -> Result<String, String> {
    let provider = env_registry::string(&env_registry::BOS_ACCOUNTING_PROVIDER)
        .unwrap_or_else(|| "qbo".to_string());
    match provider.trim() {
        "qbo" | "invoice_ninja" | "stripe" => Ok(provider.trim().to_string()),
        other => Err(format!("unknown BOS_ACCOUNTING_PROVIDER: {other}")),
    }
}

/// Does the configured provider expose profit-and-loss data?
pub fn provider_supports_pnl(provider: &str) -> bool {
    provider == "qbo"
}

/// Stripe read credential from env (the secret/restricted key).
pub fn stripe_config_from_env() -> Option<String> {
    env_registry::string(&env_registry::BOS_STRIPE_SECRET_KEY)
}

/// Invoice Ninja config from env: (base_url, api_token) when both are set.
pub fn invoice_ninja_config_from_env() -> Option<(String, String)> {
    match (
        env_registry::string(&env_registry::BOS_INVOICE_NINJA_BASE_URL),
        env_registry::string(&env_registry::BOS_INVOICE_NINJA_API_TOKEN),
    ) {
        (Some(base_url), Some(api_token)) => Some((base_url, api_token)),
        _ => None,
    }
}

pub fn environment_from_env() -> Option<QboEnvironment> {
    env_registry::string(&env_registry::BOS_QBO_ENVIRONMENT)
        .as_deref()
        .and_then(QboEnvironment::parse)
}

/// OAuth app credentials (NOT the per-company tokens) — env-provided.
pub fn oauth_app_from_env() -> Option<QboOAuthApp> {
    match (
        env_registry::string(&env_registry::BOS_QBO_CLIENT_ID),
        env_registry::string(&env_registry::BOS_QBO_CLIENT_SECRET),
        environment_from_env(),
    ) {
        (Some(client_id), Some(client_secret), Some(environment)) => Some(QboOAuthApp {
            client_id,
            client_secret,
            environment,
            token_url: None,
        }),
        _ => None,
    }
}

pub fn redirect_uri() -> String {
    let base = env_registry::string(&env_registry::BOS_PUBLIC_BASE_URL)
        .unwrap_or_else(|| "http://127.0.0.1:4400".to_string());
    format!("{}/api/connectors/qbo/callback", base.trim_end_matches('/'))
}

pub fn consent_url(app: &QboOAuthApp, state: &str) -> String {
    qbo_oauth::authorization_consent_url(app, &redirect_uri(), state)
}

pub fn connector_status(
    conn: &Connection,
    client_id: &str,
) -> Result<AccountingConnectorStatus, StoreError> {
    let provider = configured_accounting_provider().unwrap_or_else(|err| err);
    if provider == "invoice_ninja" {
        // An environment-configured static token enables the provider.
        let configured = invoice_ninja_config_from_env().is_some();
        return Ok(AccountingConnectorStatus {
            provider,
            connected: configured,
            realm_id: None,
            environment: None,
            connected_by: None,
            refresh_token_expires_at_ms: None,
            connect_url: None,
            blocked_reason: (!configured).then(|| {
                "invoice_ninja_unconfigured: set BOS_INVOICE_NINJA_BASE_URL and                  BOS_INVOICE_NINJA_API_TOKEN"
                    .to_string()
            }),
        });
    }
    if provider == "stripe" {
        // Env-configured static secret key: "connected" = configured.
        let configured = stripe_config_from_env().is_some();
        return Ok(AccountingConnectorStatus {
            provider,
            connected: configured,
            realm_id: None,
            environment: None,
            connected_by: None,
            refresh_token_expires_at_ms: None,
            connect_url: None,
            blocked_reason: (!configured)
                .then(|| "stripe_unconfigured: set BOS_STRIPE_SECRET_KEY".to_string()),
        });
    }
    if let Some(credential) = store::get_credential(conn, client_id)? {
        return Ok(AccountingConnectorStatus {
            provider,
            connected: true,
            realm_id: Some(credential.realm_id),
            environment: Some(credential.environment),
            connected_by: Some(credential.connected_by_user_id),
            refresh_token_expires_at_ms: Some(credential.refresh_token_expires_at_ms),
            connect_url: None,
            blocked_reason: None,
        });
    }
    let (connect_url, blocked_reason) = if oauth_app_from_env().is_some() {
        (Some("/api/connectors/qbo/connect".to_string()), None)
    } else {
        (
            None,
            Some(
                "oauth_app_unconfigured: set BOS_QBO_CLIENT_ID, BOS_QBO_CLIENT_SECRET and \
                 BOS_QBO_ENVIRONMENT"
                    .to_string(),
            ),
        )
    };
    Ok(AccountingConnectorStatus {
        provider,
        connected: false,
        realm_id: None,
        environment: None,
        connected_by: None,
        refresh_token_expires_at_ms: None,
        connect_url,
        blocked_reason,
    })
}

pub fn accounting_visibility_policy(
    conn: &rusqlite::Connection,
    client_id: &str,
    overlay_policy: AccountingVisibilityPolicy,
) -> Result<AccountingVisibilityPolicy, StoreError> {
    match crate::slices::admin_settings::service::value(
        conn,
        client_id,
        &env_registry::BOS_ACCOUNTING_VISIBILITY_POLICY,
    )? {
        Some(raw) if !raw.trim().is_empty() => {
            AccountingVisibilityPolicy::parse(&raw).ok_or_else(|| {
                StoreError::Domain(format!(
                    "unknown BOS_ACCOUNTING_VISIBILITY_POLICY: {}",
                    raw.trim()
                ))
            })
        }
        _ => Ok(overlay_policy),
    }
}

/// QBO exposes a broad company-wide accounting grant; BusinessOS visibility is
/// an internal per-client policy, independent of Intuit OAuth scopes.
pub fn financial_visibility_allowed(
    conn: &Connection,
    client_id: &str,
    scope: &OperatorScope,
    overlay_policy: AccountingVisibilityPolicy,
) -> Result<bool, StoreError> {
    let policy = accounting_visibility_policy(conn, client_id, overlay_policy)?;
    if policy == AccountingVisibilityPolicy::Shared {
        return Ok(true);
    }
    if policy == AccountingVisibilityPolicy::AdminOnly {
        return Ok(matches!(scope, OperatorScope::All));
    }
    let Some(credential) = store::get_credential(conn, client_id)? else {
        return Ok(true);
    };
    Ok(match scope {
        OperatorScope::All => true,
        OperatorScope::User(user_id) => credential.connected_by_user_id == *user_id,
    })
}

pub fn qbo_authorizer_allows(
    conn: &Connection,
    client_id: &str,
    scope: &OperatorScope,
) -> Result<bool, StoreError> {
    match scope {
        OperatorScope::All => Ok(true),
        OperatorScope::User(user_id) => Ok(store::get_credential(conn, client_id)?
            .is_some_and(|credential| credential.connected_by_user_id == *user_id)),
    }
}

pub fn cached_financial_visibility_allowed(
    conn: &Connection,
    client_id: &str,
    scope: &OperatorScope,
    overlay_policy: AccountingVisibilityPolicy,
) -> Result<bool, StoreError> {
    let policy = accounting_visibility_policy(conn, client_id, overlay_policy)?;
    if policy == AccountingVisibilityPolicy::Shared {
        return Ok(true);
    }
    if policy == AccountingVisibilityPolicy::AdminOnly {
        return Ok(matches!(scope, OperatorScope::All));
    }
    if store::get_credential(conn, client_id)?.is_some() {
        return qbo_authorizer_allows(conn, client_id, scope);
    }
    if matches!(scope, OperatorScope::All) {
        return Ok(true);
    }
    Ok(!has_cached_financial_rows(conn, client_id)?)
}

fn has_cached_financial_rows(conn: &Connection, client_id: &str) -> Result<bool, StoreError> {
    let (invoice_count, customer_count) = store::snapshot_counts(conn, client_id)?;
    if invoice_count > 0 || customer_count > 0 {
        return Ok(true);
    }
    let pnl_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM accounting_pnl_snapshots WHERE client_id = ?1",
        params![client_id],
        |row| row.get(0),
    )?;
    Ok(pnl_count > 0)
}

/// Today's local civil date as YYYY-MM-DD (the views' "as of" date).
pub fn today_string(now_ms: u64) -> String {
    // Days math is over UTC; accounting views tolerate the offset (the data
    // itself carries QBO's company-timezone dates).
    let (year, month, day) = civil_from_days((now_ms / 86_400_000) as i64);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Howard Hinnant's days-from-civil: YYYY-MM-DD → days since 1970-01-01.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let adjusted_year = if month <= 2 { year - 1 } else { year };
    let era = if adjusted_year >= 0 {
        adjusted_year
    } else {
        adjusted_year - 399
    } / 400;
    let year_of_era = adjusted_year - era * 400;
    let month_shift = if month > 2 { month - 3 } else { month + 9 } as i64;
    let day_of_year = (153 * month_shift + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Inverse: days since epoch → (year, month, day).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_shift = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_shift + 2) / 5 + 1) as u32;
    let month = if month_shift < 10 {
        month_shift + 3
    } else {
        month_shift - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Parse YYYY-MM-DD → days since epoch. None for malformed dates.
fn parse_date_days(date: &str) -> Option<i64> {
    let bytes = date.as_bytes();
    if bytes.len() < 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let year: i64 = date.get(0..4)?.parse().ok()?;
    let month: u32 = date.get(5..7)?.parse().ok()?;
    let day: u32 = date.get(8..10)?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(days_from_civil(year, month, day))
}

/// Days `today` is past `due_date` (negative = not yet due; None = no/bad date).
fn days_overdue(due_date: Option<&str>, today_days: i64) -> Option<i64> {
    due_date
        .and_then(parse_date_days)
        .map(|due| today_days - due)
}

/// open | overdue | paid | voided, plus days_overdue for the row.
pub fn invoice_row(snapshot: &InvoiceSnapshotRow, today: &str) -> AccountingInvoiceRow {
    let today_days = parse_date_days(today).unwrap_or(0);
    let overdue_by = days_overdue(snapshot.due_date.as_deref(), today_days).unwrap_or(0);
    let status = if snapshot.voided {
        "voided"
    } else if snapshot.balance_cents <= 0 {
        "paid"
    } else if overdue_by > 0 {
        "overdue"
    } else {
        "open"
    };
    AccountingInvoiceRow {
        invoice_id: snapshot.invoice_id.clone(),
        doc_number: snapshot.doc_number.clone(),
        customer_name: snapshot.customer_name.clone(),
        txn_date: snapshot.txn_date.clone(),
        due_date: snapshot.due_date.clone(),
        total_cents: snapshot.total_amt_cents,
        balance_cents: snapshot.balance_cents,
        status: status.to_string(),
        days_overdue: overdue_by.max(0),
    }
}

const AGING_BUCKETS: &[(&str, &str)] = &[
    ("current", "Current"),
    ("days_1_30", "1–30 days"),
    ("days_31_60", "31–60 days"),
    ("days_61_90", "61–90 days"),
    ("days_90_plus", "90+ days"),
    ("no_due_date", "No due date"),
];

/// AR aging over OPEN invoices (balance > 0, not voided).
pub fn compute_aging(invoices: &[InvoiceSnapshotRow], today: &str) -> Vec<AccountingAgingBucket> {
    let today_days = parse_date_days(today).unwrap_or(0);
    let mut buckets: Vec<AccountingAgingBucket> = AGING_BUCKETS
        .iter()
        .map(|(bucket, label)| AccountingAgingBucket {
            bucket: bucket.to_string(),
            label: label.to_string(),
            invoice_count: 0,
            balance_cents: 0,
        })
        .collect();
    for invoice in invoices {
        if invoice.voided || invoice.balance_cents <= 0 {
            continue;
        }
        let index = match days_overdue(invoice.due_date.as_deref(), today_days) {
            None => 5,
            Some(days) if days <= 0 => 0,
            Some(days) if days <= 30 => 1,
            Some(days) if days <= 60 => 2,
            Some(days) if days <= 90 => 3,
            Some(_) => 4,
        };
        buckets[index].invoice_count += 1;
        buckets[index].balance_cents += invoice.balance_cents;
    }
    buckets
}

/// Financials for providers WITHOUT P&L data: sales sums over the cached
/// invoice snapshots (voided excluded), monthly trend included, every margin
/// field absent. Basis "invoice_totals" — the UI labels and hides
/// accordingly.
pub fn compute_financials_from_invoices(
    invoices: &[InvoiceSnapshotRow],
    today: &str,
    sync: AccountingSyncInfo,
) -> bos_contracts::accounting::AccountingFinancialsResponse {
    use bos_contracts::accounting::{AccountingFinancialsResponse, AccountingPnlMonth};
    let today_days = parse_date_days(today).unwrap_or(0);
    let week_start = week_start_days(today_days);
    let month_start = month_start_days(today_days);
    let prior_month_start = shift_month_start(month_start, -1);
    let prior_wtd = prior_week_to_date_window(today);
    let prior_mtd = prior_month_to_date_window(today);
    let mut week_to_date = 0i64;
    let mut prior_week = 0i64;
    let mut prior_week_to_date = 0i64;
    let mut month_to_date = 0i64;
    let mut prior_month = 0i64;
    let mut prior_month_to_date = 0i64;
    let mut monthly: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
    for invoice in invoices {
        if invoice.voided {
            continue;
        }
        let Some(txn_date) = invoice.txn_date.as_deref() else {
            continue;
        };
        let Some(txn_days) = parse_date_days(txn_date) else {
            continue;
        };
        if txn_days > today_days {
            continue; // future-dated stays out of "to date" sums
        }
        if txn_days >= week_start {
            week_to_date += invoice.total_amt_cents;
        } else if txn_days >= week_start - 7 {
            prior_week += invoice.total_amt_cents;
            if prior_wtd
                .as_ref()
                .is_some_and(|(start, end)| txn_date >= start.as_str() && txn_date <= end.as_str())
            {
                prior_week_to_date += invoice.total_amt_cents;
            }
        }
        if txn_days >= month_start {
            month_to_date += invoice.total_amt_cents;
        } else if txn_days >= prior_month_start {
            prior_month += invoice.total_amt_cents;
            if prior_mtd
                .as_ref()
                .is_some_and(|(start, end)| txn_date >= start.as_str() && txn_date <= end.as_str())
            {
                prior_month_to_date += invoice.total_amt_cents;
            }
        }
        *monthly
            .entry(format_days(month_start_days(txn_days)))
            .or_default() += invoice.total_amt_cents;
    }
    let current_month_key = format_days(month_start);
    apply_metric_basis(
        AccountingFinancialsResponse {
            basis: "invoice_totals".to_string(),
            metric_basis: String::new(),
            metric_basis_label: String::new(),
            week_to_date_cents: week_to_date,
            prior_week_cents: Some(prior_week),
            prior_week_to_date_cents: prior_wtd.map(|_| prior_week_to_date),
            month_to_date_cents: month_to_date,
            prior_month_cents: Some(prior_month),
            prior_month_to_date_cents: prior_mtd.map(|_| prior_month_to_date),
            mtd_gross_profit_cents: None,
            mtd_cogs_cents: None,
            baseline_monthly_margin_cents: None,
            baseline_months_cached: 0,
            baseline_window_start: None,
            baseline_window_end: None,
            margin_above_baseline_cents: None,
            metric_value_cents: None,
            metric_baseline_cents: None,
            metric_above_baseline_cents: None,
            metric_pending_reason: None,
            months: monthly
                .into_iter()
                .map(|(month_start, income)| AccountingPnlMonth {
                    is_complete: month_start != current_month_key,
                    month_start,
                    total_income_cents: income,
                    total_cogs_cents: None,
                    gross_profit_cents: None,
                })
                .collect(),
            sync,
        },
        &AccountingMetricBasisConfig {
            basis: AccountingMetricBasisKind::InvoiceTotals,
            label: AccountingMetricBasisKind::InvoiceTotals
                .default_label()
                .to_string(),
            ..AccountingMetricBasisConfig::default()
        },
    )
}

pub fn customer_row(snapshot: &CustomerSnapshotRow) -> AccountingCustomerRow {
    AccountingCustomerRow {
        customer_id: snapshot.customer_id.clone(),
        display_name: snapshot.display_name.clone(),
        company_name: snapshot.company_name.clone(),
        email: snapshot.email.clone(),
        tier: snapshot.tier.clone(),
        active: snapshot.active,
    }
}

/// One P&L period the sync should keep cached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PnlPeriod {
    /// "month" | "week".
    pub kind: &'static str,
    pub start: String,
    /// Inclusive end; for incomplete periods this is `today`.
    pub end: String,
    pub is_complete: bool,
}

fn format_days(days: i64) -> String {
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

pub fn last_n_days_window(today: &str, days: u32) -> Option<(String, String)> {
    let today_days = parse_date_days(today)?;
    let span = i64::from(days.max(1));
    Some((format_days(today_days - span + 1), format_days(today_days)))
}

pub(crate) fn prior_week_to_date_window(today: &str) -> Option<(String, String)> {
    let today_days = parse_date_days(today)?;
    let current_week_start = week_start_days(today_days);
    let prior_week_start = current_week_start - 7;
    let comparable_end = prior_week_start + (today_days - current_week_start);
    Some((format_days(prior_week_start), format_days(comparable_end)))
}

pub(crate) fn prior_month_to_date_window(today: &str) -> Option<(String, String)> {
    let today_days = parse_date_days(today)?;
    let current_month_start = month_start_days(today_days);
    let prior_month_start = shift_month_start(current_month_start, -1);
    let prior_month_end = current_month_start - 1;
    let (_, _, day_of_month) = civil_from_days(today_days);
    let comparable_end = (prior_month_start + i64::from(day_of_month) - 1).min(prior_month_end);
    Some((format_days(prior_month_start), format_days(comparable_end)))
}

/// First day of the month containing `days`.
fn month_start_days(days: i64) -> i64 {
    let (year, month, _) = civil_from_days(days);
    days_from_civil(year, month, 1)
}

/// Shift a month start by `delta` months (delta may be negative).
fn shift_month_start(days: i64, delta: i32) -> i64 {
    let (year, month, _) = civil_from_days(days);
    let zero_based = year * 12 + (month as i64 - 1) + delta as i64;
    let new_year = zero_based.div_euclid(12);
    let new_month = (zero_based.rem_euclid(12) + 1) as u32;
    days_from_civil(new_year, new_month, 1)
}

/// First day of the quarter containing `days`.
fn quarter_start_days(days: i64) -> i64 {
    let (year, month, _) = civil_from_days(days);
    let quarter_month = ((month - 1) / 3) * 3 + 1;
    days_from_civil(year, quarter_month, 1)
}

/// Monday of the week containing `days` (epoch day 0 was a Thursday).
fn week_start_days(days: i64) -> i64 {
    days - (days + 3).rem_euclid(7)
}

/// The baseline window per the pilot agreement: the previous FOUR COMPLETED
/// quarters before `today` — twelve full months, [start, end] inclusive.
pub fn baseline_window(today: &str) -> Option<(String, String)> {
    let today_days = parse_date_days(today)?;
    let current_quarter = quarter_start_days(today_days);
    let window_start = shift_month_start(current_quarter, -12);
    Some((format_days(window_start), format_days(current_quarter - 1)))
}

/// Every P&L period the cache should hold for `today`: the 12 baseline
/// months + months since the baseline window through the current month, plus
/// the prior (complete) week and the current week-to-date. Complete periods
/// are immutable — the sync fetches each exactly once.
pub fn needed_pnl_periods(today: &str) -> Vec<PnlPeriod> {
    let Some(today_days) = parse_date_days(today) else {
        return Vec::new();
    };
    let mut periods = Vec::new();
    let current_quarter = quarter_start_days(today_days);
    let current_month = month_start_days(today_days);
    let mut month = shift_month_start(current_quarter, -12);
    while month <= current_month {
        let next = shift_month_start(month, 1);
        let is_complete = next - 1 <= today_days && month != current_month;
        periods.push(PnlPeriod {
            kind: "month",
            start: format_days(month),
            end: format_days(if is_complete { next - 1 } else { today_days }),
            is_complete,
        });
        month = next;
    }
    let week = week_start_days(today_days);
    periods.push(PnlPeriod {
        kind: "week",
        start: format_days(week - 7),
        end: format_days(week - 1),
        is_complete: true,
    });
    periods.push(PnlPeriod {
        kind: "week",
        start: format_days(week),
        end: format_days(today_days),
        is_complete: false,
    });
    periods
}

/// Monday of the week containing `date` (YYYY-MM-DD). None for bad dates.
pub fn week_start_date(date: &str) -> Option<String> {
    parse_date_days(date).map(|days| format_days(week_start_days(days)))
}

/// First day of the month containing `date`. None for bad dates.
pub fn month_start_date(date: &str) -> Option<String> {
    parse_date_days(date).map(|days| format_days(month_start_days(days)))
}

/// UTC midnight of a civil date in epoch ms (window math over *_at_ms
/// columns; same UTC tolerance as [`today_string`]).
pub fn date_to_epoch_ms(date: &str) -> Option<u64> {
    parse_date_days(date).and_then(|days| u64::try_from(days).ok().map(|d| d * 86_400_000))
}

/// Sync freshness for the view envelopes: cursor rows + the in-memory guard
/// state (callers pass a clone of the AppState mutex contents).
pub fn sync_info(
    conn: &Connection,
    client_id: &str,
    status: &crate::http::SyncGuard,
) -> Result<AccountingSyncInfo, StoreError> {
    let (invoice_count, customer_count) = store::snapshot_counts(conn, client_id)?;
    let invoice_cursor = store::get_cursor(conn, client_id, store::ENTITY_INVOICE)?;
    let bill_cursor = store::get_cursor(conn, client_id, store::ENTITY_BILL)?;
    let customer_cursor = store::get_cursor(conn, client_id, store::ENTITY_CUSTOMER)?;
    let balance_cursor = store::get_cursor(conn, client_id, super::worker::ENTITY_BALANCE_SHEET)?;
    let qbo_reports_expected = configured_accounting_provider().as_deref() == Ok("qbo");
    let last_synced_at_ms = [
        invoice_cursor.last_advanced_at_ms,
        bill_cursor.last_advanced_at_ms,
        customer_cursor.last_advanced_at_ms,
        balance_cursor.last_advanced_at_ms,
        status.last_attempt_ms,
    ]
    .into_iter()
    .flatten()
    .max();
    Ok(AccountingSyncInfo {
        sync_enabled: crate::slices::admin_settings::service::flag(
            conn,
            client_id,
            &env_registry::BOS_ACCOUNTING_SYNC_ENABLED,
        )?,
        in_flight: status.in_flight,
        backfill_complete: invoice_cursor.backfill_complete
            && customer_cursor.backfill_complete
            && (!qbo_reports_expected || bill_cursor.backfill_complete)
            && (!qbo_reports_expected || balance_cursor.backfill_complete),
        last_synced_at_ms,
        invoice_count,
        customer_count,
        last_requests_used: status.units_used,
        next_sync_allowed_at_ms: status.next_allowed_at_ms,
        last_error: invoice_cursor
            .last_error
            .or(customer_cursor.last_error)
            .or(bill_cursor.last_error)
            .or(balance_cursor.last_error),
    })
}

pub fn daily_revenue_from_store(
    conn: &Connection,
    client_id: &str,
    today: &str,
) -> Result<Vec<super::store::PnlSnapshotRow>, StoreError> {
    let Some((start, end)) = last_n_days_window(today, 7) else {
        return Ok(Vec::new());
    };
    let mut rows = store::list_pnl_snapshots(conn, client_id, "day")?;
    rows.retain(|row| {
        row.period_start.as_str() >= start.as_str() && row.period_start.as_str() <= end.as_str()
    });
    Ok(rows)
}

/// The financials read model exactly as GET /api/accounting/financials
/// serves it: provider branch + snapshot reads + the pure compute. The owner
/// digest reuses this so its sales/margin figures can never drift from the
/// Accounting tab (same basis label, same baseline math).
pub fn financials_from_store(
    conn: &Connection,
    client_id: &str,
    today: &str,
    sync: AccountingSyncInfo,
    metric_config: &AccountingMetricBasisConfig,
) -> Result<bos_contracts::accounting::AccountingFinancialsResponse, StoreError> {
    let provider = configured_accounting_provider().unwrap_or_else(|err| err);
    let effective_metric_config = effective_metric_config_for_provider(&provider, metric_config);
    if !provider_supports_pnl(&provider) {
        // No P&L data: financials are invoice-total sums (basis labeled).
        let snapshots = store::list_invoices(conn, client_id, 10_000)?;
        return Ok(apply_metric_basis(
            compute_financials_from_invoices(&snapshots, today, sync),
            &effective_metric_config,
        ));
    }
    let months = store::list_pnl_snapshots(conn, client_id, "month")?;
    let weeks = store::list_pnl_snapshots(conn, client_id, "week")?;
    let days = store::list_pnl_snapshots(conn, client_id, "day")?;
    let mut response =
        compute_financials_with_basis(&months, &weeks, today, sync, &effective_metric_config);
    response.prior_week_to_date_cents = prior_week_to_date_window(today)
        .and_then(|window| sum_complete_daily_window(&days, window));
    response.prior_month_to_date_cents = prior_month_to_date_from_daily(&days, today);
    Ok(response)
}

/// Assemble the financials read model from cached P&L periods. Pure: rows in,
/// contract out — the baseline math (avg monthly gross margin of the previous
/// four completed quarters) is THE pilot payment metric, so it only reports
/// when every baseline month is cached.
pub fn compute_financials(
    months: &[super::store::PnlSnapshotRow],
    weeks: &[super::store::PnlSnapshotRow],
    today: &str,
    sync: AccountingSyncInfo,
) -> bos_contracts::accounting::AccountingFinancialsResponse {
    compute_financials_with_basis(
        months,
        weeks,
        today,
        sync,
        &AccountingMetricBasisConfig::default(),
    )
}

pub fn compute_financials_with_basis(
    months: &[super::store::PnlSnapshotRow],
    weeks: &[super::store::PnlSnapshotRow],
    today: &str,
    sync: AccountingSyncInfo,
    metric_config: &AccountingMetricBasisConfig,
) -> bos_contracts::accounting::AccountingFinancialsResponse {
    use bos_contracts::accounting::{AccountingFinancialsResponse, AccountingPnlMonth};
    let today_days = parse_date_days(today).unwrap_or(0);
    let current_month = format_days(month_start_days(today_days));
    let prior_month = format_days(shift_month_start(month_start_days(today_days), -1));
    let current_week = format_days(week_start_days(today_days));
    let prior_week = format_days(week_start_days(today_days) - 7);

    let month_row = |start: &str| months.iter().find(|row| row.period_start == start);
    let week_row = |start: &str| weeks.iter().find(|row| row.period_start == start);

    let (baseline_start, baseline_end) = baseline_window(today).unwrap_or_default();
    let baseline_rows: Vec<_> = months
        .iter()
        .filter(|row| {
            row.is_complete
                && row.period_start.as_str() >= baseline_start.as_str()
                && row.period_start.as_str() <= baseline_end.as_str()
        })
        .collect();
    let baseline_months_cached = baseline_rows.len() as u32;
    let baseline_monthly_margin_cents = (baseline_months_cached == 12).then(|| {
        baseline_rows
            .iter()
            .map(|row| row.gross_profit_cents)
            .sum::<i64>()
            / 12
    });
    let mtd = month_row(&current_month);
    let margin_above_baseline_cents = match (mtd, baseline_monthly_margin_cents) {
        (Some(mtd), Some(baseline)) => Some(mtd.gross_profit_cents - baseline),
        _ => None,
    };

    apply_metric_basis(
        AccountingFinancialsResponse {
            basis: "quickbooks_pnl".to_string(),
            metric_basis: String::new(),
            metric_basis_label: String::new(),
            week_to_date_cents: week_row(&current_week)
                .map(|row| row.total_income_cents)
                .unwrap_or(0),
            prior_week_cents: week_row(&prior_week).map(|row| row.total_income_cents),
            prior_week_to_date_cents: None,
            month_to_date_cents: mtd.map(|row| row.total_income_cents).unwrap_or(0),
            prior_month_cents: month_row(&prior_month).map(|row| row.total_income_cents),
            prior_month_to_date_cents: None,
            mtd_gross_profit_cents: Some(mtd.map(|row| row.gross_profit_cents).unwrap_or(0)),
            mtd_cogs_cents: Some(mtd.map(|row| row.total_cogs_cents).unwrap_or(0)),
            baseline_monthly_margin_cents,
            baseline_months_cached,
            baseline_window_start: Some(baseline_start),
            baseline_window_end: Some(baseline_end),
            margin_above_baseline_cents,
            metric_value_cents: None,
            metric_baseline_cents: None,
            metric_above_baseline_cents: None,
            metric_pending_reason: None,
            months: months
                .iter()
                .map(|row| AccountingPnlMonth {
                    month_start: row.period_start.clone(),
                    total_income_cents: row.total_income_cents,
                    total_cogs_cents: Some(row.total_cogs_cents),
                    gross_profit_cents: Some(row.gross_profit_cents),
                    is_complete: row.is_complete,
                })
                .collect(),
            sync,
        },
        metric_config,
    )
}

fn prior_month_to_date_from_daily(
    days: &[super::store::PnlSnapshotRow],
    today: &str,
) -> Option<i64> {
    sum_complete_daily_window(days, prior_month_to_date_window(today)?)
}

fn sum_complete_daily_window(
    days: &[super::store::PnlSnapshotRow],
    (start, end): (String, String),
) -> Option<i64> {
    let mut total = 0i64;
    let mut expected_day = parse_date_days(&start)?;
    let end_day = parse_date_days(&end)?;
    for row in days.iter().filter(|row| {
        row.period_start.as_str() >= start.as_str() && row.period_start.as_str() <= end.as_str()
    }) {
        let row_day = parse_date_days(&row.period_start)?;
        if row_day != expected_day || row.period_end != row.period_start {
            return None;
        }
        total += row.total_income_cents;
        expected_day += 1;
    }
    (expected_day == end_day + 1).then_some(total)
}

pub(crate) fn apply_metric_basis(
    mut response: bos_contracts::accounting::AccountingFinancialsResponse,
    config: &AccountingMetricBasisConfig,
) -> bos_contracts::accounting::AccountingFinancialsResponse {
    response.metric_basis = config.basis.as_str().to_string();
    response.metric_basis_label = config.label.clone();
    match config.basis {
        AccountingMetricBasisKind::GrossMargin => {
            response.metric_value_cents = response.mtd_gross_profit_cents;
            response.metric_baseline_cents = response
                .baseline_monthly_margin_cents
                .or(config.baseline_cents);
            response.metric_above_baseline_cents =
                match (response.metric_value_cents, response.metric_baseline_cents) {
                    (Some(value), Some(baseline)) => Some(value - baseline),
                    _ => None,
                };
            response.metric_pending_reason = if response.metric_value_cents.is_none() {
                Some("Gross margin needs provider P&L data.".to_string())
            } else if response.metric_baseline_cents.is_none() {
                Some(format!(
                    "Baseline not yet available ({} of 12 months cached).",
                    response.baseline_months_cached
                ))
            } else {
                None
            };
        }
        AccountingMetricBasisKind::AdjustedGrossSales => {
            let missing: Vec<&str> = [
                ("freight", config.freight_cents),
                ("taxes", config.taxes_cents),
                ("insurance", config.insurance_cents),
            ]
            .into_iter()
            .filter_map(|(label, value)| value.is_none().then_some(label))
            .collect();
            if missing.is_empty() {
                let deductions = config.freight_cents.unwrap_or(0)
                    + config.taxes_cents.unwrap_or(0)
                    + config.insurance_cents.unwrap_or(0);
                response.metric_value_cents = Some(response.month_to_date_cents - deductions);
            }
            response.metric_baseline_cents = config.baseline_cents;
            response.metric_above_baseline_cents =
                match (response.metric_value_cents, response.metric_baseline_cents) {
                    (Some(value), Some(baseline)) => Some(value - baseline),
                    _ => None,
                };
            response.metric_pending_reason = if !missing.is_empty() {
                Some(format!(
                    "Adjusted gross sales needs imported current-period {}.",
                    missing.join(", ")
                ))
            } else if response.metric_baseline_cents.is_none() {
                Some("Adjusted gross sales baseline is not configured.".to_string())
            } else {
                None
            };
        }
        AccountingMetricBasisKind::InvoiceTotals => {
            response.metric_value_cents = Some(response.month_to_date_cents);
            response.metric_baseline_cents = config.baseline_cents;
            response.metric_above_baseline_cents =
                match (response.metric_value_cents, response.metric_baseline_cents) {
                    (Some(value), Some(baseline)) => Some(value - baseline),
                    _ => None,
                };
            response.metric_pending_reason = (config.configured
                && response.metric_baseline_cents.is_none())
            .then(|| "Invoice-total baseline is not configured.".to_string());
        }
    }
    response
}
