//! Digest assembly + narration. Everything numeric is DETERMINISTIC — the
//! metrics come from the local caches via the owning slices' own reads (the
//! sales/margin figures are the accounting slice's financials_from_store,
//! so the digest can never drift from the Accounting tab). The ONE LLM call
//! narrates the metrics JSON into headline/narrative/callouts; a dollar
//! amount in that prose that does not literally appear in the input's
//! formatted amounts is refused (the ledger amount-grounding doctrine).
//! Site traffic reads the search_console slice's local snapshots and remains
//! an honest pending-data state until a property and credential are configured.

use bos_contracts::client_profile::ClientProfile;
use bos_contracts::follow_up_tasks::{TaskDueLane, TaskEscalationLevel, TaskStatus};
use bos_contracts::owner_reports::{
    DigestCallMetrics, DigestClaimMetrics, DigestDamageTypeCount, DigestDealMetrics,
    DigestDealMetricsStatus, DigestFollowUpMetrics, DigestInventoryMetrics, DigestOrderMetrics,
    DigestSalesMetrics, DigestSeverityCount, DigestStatusCount, DigestTrafficMetrics,
    OwnerDigestMetrics, OwnerReport, OwnerReportPeriodKind, OwnerReportStatus,
};
use bos_contracts::search_console::{AnalyticsMetricTotals, SearchConsoleMetricTotals};
use bos_integrations::gmail_draft_write::{
    GmailDraftApprovalMetadata, GmailDraftCreateOutboxPayload,
};
use bos_integrations::llm_typed_tasks::{
    TypedLlmAuthority, TypedLlmExecutionPolicy, TypedLlmExecutionRoute, TypedLlmFallbackPolicy,
    TypedLlmProviderPolicy, TypedLlmRawOutputRetention, TypedLlmRedactionPolicy,
    TypedLlmResponseFormat, TypedLlmRetryPolicy, TypedLlmSafetyPolicy, TypedLlmSourceEntity,
    TypedLlmTaskCapabilities, TypedLlmTaskClass, TypedLlmTaskInput, TypedLlmTaskRequest,
    TypedLlmTaskSpec,
};
use rusqlite::Connection;
use serde_json::json;

use crate::env_registry;
use crate::http::SyncGuard;
use crate::outbox::NewOutboxJob;
use crate::overlay::{OwnerReportsOverlay, SearchConsoleOverlay};
use crate::slices::accounting;
use crate::store_core::StoreError;

pub const NARRATION_SCHEMA_REF: &str = "bos.owner_reports.digest_narration.v1";
pub const NARRATION_PURPOSE: &str = "owner_digest_narration";
const BEHAVIOR_ANALYTICS_PENDING_REASON: &str =
    "GA4 behavior/acquisition data is not configured in BusinessOS yet.";
const CONVERSION_TRACKING_PENDING_REASON: &str =
    "GA4 conversion events are not configured in BusinessOS yet.";
const RETARGETING_PENDING_REASON: &str =
    "Retargeting pixel/audience setup is outside BusinessOS writes until separately designed.";

/// One digest period: the current week (Monday through today) or the
/// current month to date. Ids are deterministic per period start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestPeriod {
    pub kind: OwnerReportPeriodKind,
    /// Inclusive YYYY-MM-DD bounds; `end` is the as-of date.
    pub start: String,
    pub end: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportWeekday {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportMetricSection {
    Sales,
    Calls,
    FollowUps,
    Inventory,
    Orders,
    DamageClaims,
    SiteTraffic,
    CloseRate,
}

impl ReportMetricSection {
    fn id(self) -> &'static str {
        match self {
            ReportMetricSection::Sales => "sales",
            ReportMetricSection::Calls => "calls",
            ReportMetricSection::FollowUps => "follow_ups",
            ReportMetricSection::Inventory => "inventory",
            ReportMetricSection::Orders => "orders",
            ReportMetricSection::DamageClaims => "damage_claims",
            ReportMetricSection::SiteTraffic => "site_traffic",
            ReportMetricSection::CloseRate => "close_rate",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerReportConfig {
    pub allowed_operator_user_ids: Vec<String>,
    pub delivery_enabled: bool,
    pub recipients: Vec<String>,
    pub recipient_profiles: Vec<OwnerReportRecipientProfile>,
    pub financial_redaction_recipients: Vec<String>,
    pub weekly_weekday: Option<ReportWeekday>,
    pub mtd_day: Option<u8>,
    pub metrics: Vec<ReportMetricSection>,
    pub subject_prefix: String,
    /// Client report-assembly profile id (e.g. call reason-code bucketing).
    /// None = generic assembly only.
    pub report_profile: Option<String>,
}

impl Default for OwnerReportConfig {
    fn default() -> Self {
        Self {
            allowed_operator_user_ids: Vec::new(),
            delivery_enabled: false,
            recipients: Vec::new(),
            recipient_profiles: Vec::new(),
            financial_redaction_recipients: Vec::new(),
            weekly_weekday: None,
            mtd_day: None,
            metrics: default_metric_sections(),
            subject_prefix: "Owner digest".to_string(),
            report_profile: None,
        }
    }
}

/// True when `id` names a report profile compiled into this build.
pub fn report_profile_exists(id: &str) -> bool {
    #[cfg(test)]
    {
        if id.trim() == "test_profile" {
            return true;
        }
    }
    let _ = id;
    false
}

fn select_report_profile(id: &str) -> Option<&'static dyn bos_profile_api::ClientReportProfile> {
    let _ = id;
    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerReportRecipientProfile {
    pub recipients: Vec<String>,
    pub metrics: Vec<ReportMetricSection>,
}

fn default_metric_sections() -> Vec<ReportMetricSection> {
    vec![
        ReportMetricSection::Sales,
        ReportMetricSection::Calls,
        ReportMetricSection::FollowUps,
        ReportMetricSection::Inventory,
        ReportMetricSection::Orders,
        ReportMetricSection::DamageClaims,
        ReportMetricSection::SiteTraffic,
        ReportMetricSection::CloseRate,
    ]
}

pub fn config_from_sources(overlay: Option<&OwnerReportsOverlay>) -> OwnerReportConfig {
    let mut config = OwnerReportConfig::default();
    if let Some(overlay) = overlay {
        config.allowed_operator_user_ids = normalize_user_ids(&overlay.allowed_operator_user_ids);
        config.delivery_enabled = overlay.delivery_enabled;
        config.recipients = normalize_recipients(&overlay.recipients);
        config.recipient_profiles = overlay
            .recipient_profiles
            .iter()
            .filter_map(|profile| {
                let recipients = normalize_recipients(&profile.recipients);
                if recipients.is_empty() {
                    return None;
                }
                Some(OwnerReportRecipientProfile {
                    recipients,
                    metrics: parse_metric_sections(&profile.metrics),
                })
            })
            .collect();
        config.weekly_weekday = overlay.weekly_weekday.as_deref().and_then(parse_weekday);
        config.mtd_day = overlay.mtd_day.filter(|day| (1..=31).contains(day));
        if !overlay.metrics.is_empty() {
            let metrics = parse_metric_sections(&overlay.metrics);
            if !metrics.is_empty() {
                config.metrics = metrics;
            }
        }
        if let Some(prefix) = overlay
            .subject_prefix
            .as_deref()
            .map(str::trim)
            .filter(|raw| !raw.is_empty())
        {
            config.subject_prefix = prefix.to_string();
        }
        config.report_profile = overlay
            .report_profile
            .as_deref()
            .map(str::trim)
            .filter(|raw| !raw.is_empty())
            .map(str::to_string);
    }
    if crate::env_registry::flag(&crate::env_registry::BOS_REPORT_DIGEST_DELIVERY_ENABLED) {
        config.delivery_enabled = true;
    }
    if let Some(raw) = crate::env_registry::string(
        &crate::env_registry::BOS_OWNER_REPORT_ALLOWED_OPERATOR_USER_IDS,
    ) {
        config.allowed_operator_user_ids = split_user_ids(&raw);
    }
    if let Some(raw) = crate::env_registry::string(&crate::env_registry::BOS_REPORT_DIGEST_TO_ADDR)
    {
        let recipients = split_recipients(&raw);
        if !recipients.is_empty() {
            config.recipients = recipients;
        }
    }
    if let Some(raw) =
        crate::env_registry::string(&crate::env_registry::BOS_REPORT_DIGEST_WEEKLY_WEEKDAY)
    {
        config.weekly_weekday = parse_weekday(&raw);
    }
    if let Some(raw) = crate::env_registry::string(&crate::env_registry::BOS_REPORT_DIGEST_MTD_DAY)
    {
        config.mtd_day = raw
            .trim()
            .parse::<u8>()
            .ok()
            .filter(|day| (1..=31).contains(day));
    }
    if let Some(raw) = crate::env_registry::string(&crate::env_registry::BOS_REPORT_DIGEST_METRICS)
    {
        let parts = raw
            .split(|ch: char| ch == ',' || ch == ';' || ch.is_whitespace())
            .map(str::to_string)
            .collect::<Vec<_>>();
        let metrics = parse_metric_sections(&parts);
        if !metrics.is_empty() {
            config.metrics = metrics;
        }
    }
    if let Some(raw) =
        crate::env_registry::string(&crate::env_registry::BOS_REPORT_DIGEST_REDACT_FINANCIALS_FOR)
    {
        config.financial_redaction_recipients = split_recipients(&raw)
            .into_iter()
            .map(|recipient| recipient.to_ascii_lowercase())
            .collect();
    }
    if let Some(prefix) =
        crate::env_registry::string(&crate::env_registry::BOS_REPORT_DIGEST_SUBJECT_PREFIX)
            .map(|raw| raw.trim().to_string())
            .filter(|raw| !raw.is_empty())
    {
        config.subject_prefix = prefix;
    }
    config
}

pub fn recipients_line(config: &OwnerReportConfig) -> Option<String> {
    (!config.recipients.is_empty()).then(|| config.recipients.join(", "))
}

pub fn operator_allowed(config: &OwnerReportConfig, actor_id: &str) -> bool {
    config.allowed_operator_user_ids.is_empty()
        || config
            .allowed_operator_user_ids
            .iter()
            .any(|allowed| allowed == actor_id)
}

pub fn metric_section_ids(config: &OwnerReportConfig) -> Vec<String> {
    config
        .metrics
        .iter()
        .map(|section| section.id().to_string())
        .collect()
}

fn normalize_recipients(values: &[String]) -> Vec<String> {
    values
        .iter()
        .flat_map(|value| split_recipients(value))
        .collect()
}

fn normalize_user_ids(values: &[String]) -> Vec<String> {
    let mut ids = Vec::new();
    for value in values {
        for id in split_user_ids(value) {
            if !ids.iter().any(|existing| existing == &id) {
                ids.push(id);
            }
        }
    }
    ids
}

fn split_user_ids(raw: &str) -> Vec<String> {
    raw.split(|ch: char| ch == ',' || ch == ';' || ch.is_whitespace())
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect()
}

fn split_recipients(raw: &str) -> Vec<String> {
    let mut recipients = Vec::new();
    for entry in raw.split(|ch: char| ch == ',' || ch == ';' || ch.is_whitespace()) {
        let entry = entry.trim();
        if !entry.is_empty() && !recipients.iter().any(|existing| existing == entry) {
            recipients.push(entry.to_string());
        }
    }
    recipients
}

fn recipient_matches(profile_recipient: &str, actual_recipient: &str) -> bool {
    profile_recipient.eq_ignore_ascii_case(actual_recipient)
}

fn effective_metric_sections(
    config: &OwnerReportConfig,
    recipients: &[String],
) -> Vec<ReportMetricSection> {
    let mut sections = config.metrics.clone();
    for profile in &config.recipient_profiles {
        if profile.metrics.is_empty() {
            continue;
        }
        if recipients.iter().any(|recipient| {
            profile
                .recipients
                .iter()
                .any(|profile_recipient| recipient_matches(profile_recipient, recipient))
        }) {
            sections = profile.metrics.clone();
            break;
        }
    }
    if recipients.iter().any(|recipient| {
        config
            .financial_redaction_recipients
            .iter()
            .any(|profile_recipient| recipient_matches(profile_recipient, recipient))
    }) {
        sections.retain(|section| *section != ReportMetricSection::Sales);
    }
    sections
}

pub fn without_financial_sections(config: &OwnerReportConfig) -> OwnerReportConfig {
    let mut redacted = config.clone();
    redacted
        .metrics
        .retain(|section| *section != ReportMetricSection::Sales);
    for profile in &mut redacted.recipient_profiles {
        profile
            .metrics
            .retain(|section| *section != ReportMetricSection::Sales);
    }
    redacted
}

pub fn redact_financials(report: &mut OwnerReport) {
    report.metrics.sales = DigestSalesMetrics {
        basis: "redacted".to_string(),
        metric_basis: "redacted".to_string(),
        metric_basis_label: "Financial metric".to_string(),
        period_sales_cents: 0,
        prior_period_sales_cents: None,
        mtd_gross_profit_cents: None,
        baseline_monthly_margin_cents: None,
        margin_above_baseline_cents: None,
        metric_value_cents: None,
        metric_baseline_cents: None,
        metric_above_baseline_cents: None,
        metric_pending_reason: Some("Financial metrics redacted for this recipient.".to_string()),
        baseline_months_cached: 0,
        last_synced_at_ms: None,
    };
    report.headline = None;
    report.narrative = None;
    report.callouts.clear();
}

pub fn report_financials_visible(
    conn: &rusqlite::Connection,
    client_id: &str,
    scope: &crate::http::OperatorScope,
    report: &OwnerReport,
    policy: crate::overlay::AccountingVisibilityPolicy,
) -> Result<bool, StoreError> {
    if report.metrics.sales.basis == "redacted" {
        return Ok(true);
    }
    let policy =
        crate::slices::accounting::service::accounting_visibility_policy(conn, client_id, policy)?;
    if policy == crate::overlay::AccountingVisibilityPolicy::AuthorizerOnly
        && report.metrics.sales.basis == "quickbooks_pnl"
    {
        return crate::slices::accounting::service::qbo_authorizer_allows(conn, client_id, scope);
    }
    crate::slices::accounting::service::cached_financial_visibility_allowed(
        conn, client_id, scope, policy,
    )
}

fn parse_weekday(raw: &str) -> Option<ReportWeekday> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "mon" | "monday" => Some(ReportWeekday::Monday),
        "tue" | "tues" | "tuesday" => Some(ReportWeekday::Tuesday),
        "wed" | "wednesday" => Some(ReportWeekday::Wednesday),
        "thu" | "thur" | "thurs" | "thursday" => Some(ReportWeekday::Thursday),
        "fri" | "friday" => Some(ReportWeekday::Friday),
        "sat" | "saturday" => Some(ReportWeekday::Saturday),
        "sun" | "sunday" => Some(ReportWeekday::Sunday),
        _ => None,
    }
}

fn parse_metric_sections(values: &[String]) -> Vec<ReportMetricSection> {
    let mut sections = Vec::new();
    for value in values {
        let section = match value.trim().to_ascii_lowercase().as_str() {
            "sales" | "margin" | "financials" => ReportMetricSection::Sales,
            "calls" | "incoming_calls" | "call_volume" => ReportMetricSection::Calls,
            "follow_ups" | "follow-ups" | "tasks" => ReportMetricSection::FollowUps,
            "inventory" | "stock" | "stock_health" => ReportMetricSection::Inventory,
            "orders" | "order_control" | "missed_order_prevention" | "order_completeness" => {
                ReportMetricSection::Orders
            }
            "damage_claims" | "damage" | "claims" | "shipping_damage" => {
                ReportMetricSection::DamageClaims
            }
            "site_traffic" | "traffic" => ReportMetricSection::SiteTraffic,
            "close_rate" | "contact_to_close" | "deals" => ReportMetricSection::CloseRate,
            _ => continue,
        };
        if !sections.contains(&section) {
            sections.push(section);
        }
    }
    sections
}

pub fn report_id_for(kind: OwnerReportPeriodKind, period_start: &str) -> String {
    format!("owr_{}_{period_start}", super::store::period_kind_str(kind))
}

/// The two periods every digest cycle covers, anchored on `today`.
pub fn current_periods(today: &str) -> Vec<DigestPeriod> {
    let mut periods = Vec::new();
    if let Some(week_start) = accounting::service::week_start_date(today) {
        periods.push(DigestPeriod {
            kind: OwnerReportPeriodKind::Weekly,
            start: week_start,
            end: today.to_string(),
        });
    }
    if let Some(month_start) = accounting::service::month_start_date(today) {
        periods.push(DigestPeriod {
            kind: OwnerReportPeriodKind::Mtd,
            start: month_start,
            end: today.to_string(),
        });
    }
    periods
}

pub fn due_for_scheduled_delivery(
    period: &DigestPeriod,
    today: &str,
    config: &OwnerReportConfig,
) -> bool {
    if !config.delivery_enabled {
        return false;
    }
    match period.kind {
        OwnerReportPeriodKind::Weekly => config
            .weekly_weekday
            .is_some_and(|weekday| weekday_for_date(today) == Some(weekday)),
        OwnerReportPeriodKind::Mtd => config
            .mtd_day
            .is_some_and(|day| day_of_month(today) == Some(day)),
    }
}

fn weekday_for_date(date: &str) -> Option<ReportWeekday> {
    let days = accounting::service::date_to_epoch_ms(date)? / 86_400_000;
    match ((days as i64 + 3).rem_euclid(7)) as u8 {
        0 => Some(ReportWeekday::Monday),
        1 => Some(ReportWeekday::Tuesday),
        2 => Some(ReportWeekday::Wednesday),
        3 => Some(ReportWeekday::Thursday),
        4 => Some(ReportWeekday::Friday),
        5 => Some(ReportWeekday::Saturday),
        _ => Some(ReportWeekday::Sunday),
    }
}

fn day_of_month(date: &str) -> Option<u8> {
    date.get(8..10)?.parse().ok()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallVolumeMetricConfig {
    pub category_id: String,
    pub label: String,
    pub source_label: String,
    pub gmail_label: String,
    pub gmail_query: String,
}

impl CallVolumeMetricConfig {
    pub fn from_overlay(overlay: Option<&OwnerReportsOverlay>) -> Self {
        let call_volume = overlay.map(|overlay| &overlay.call_volume);
        Self {
            category_id: env_or_overlay(
                &env_registry::BOS_OWNER_REPORT_CALL_VOLUME_CATEGORY_ID,
                call_volume.map(|config| config.category_id.as_str()),
            ),
            label: env_or_overlay(
                &env_registry::BOS_OWNER_REPORT_CALL_VOLUME_LABEL,
                call_volume.map(|config| config.label.as_str()),
            )
            .trim()
            .to_string(),
            source_label: env_or_overlay(
                &env_registry::BOS_OWNER_REPORT_CALL_VOLUME_SOURCE_LABEL,
                call_volume.map(|config| config.source_label.as_str()),
            )
            .trim()
            .to_string(),
            gmail_label: env_or_overlay(
                &env_registry::BOS_OWNER_REPORT_CALL_VOLUME_GMAIL_LABEL,
                call_volume.map(|config| config.gmail_label.as_str()),
            ),
            gmail_query: env_or_overlay(
                &env_registry::BOS_OWNER_REPORT_CALL_VOLUME_GMAIL_QUERY,
                call_volume.map(|config| config.gmail_query.as_str()),
            ),
        }
    }

    fn metric_label(&self) -> String {
        if self.label.trim().is_empty() {
            "Incoming calls".to_string()
        } else {
            self.label.trim().to_string()
        }
    }

    fn metric_source_label(&self) -> String {
        if self.source_label.trim().is_empty() {
            "Email-derived call summaries".to_string()
        } else {
            self.source_label.trim().to_string()
        }
    }

    fn pending_reason(&self) -> Option<String> {
        if self.category_id.trim().is_empty() {
            return Some("Missing call-summary email category".to_string());
        }
        None
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MetricAssemblyConfig<'a> {
    pub report: &'a OwnerReportConfig,
    pub call_volume: &'a CallVolumeMetricConfig,
    pub accounting_metric: &'a accounting::service::AccountingMetricBasisConfig,
    pub search_console_overlay: Option<&'a SearchConsoleOverlay>,
}

fn env_or_overlay(var: &env_registry::EnvVar, overlay_value: Option<&str>) -> String {
    env_registry::string(var)
        .or_else(|| {
            overlay_value
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_default()
}

/// Assemble every digest metric for one period. Local cache reads happen first;
/// provider reporting reads are appended after so worker callers can keep the
/// provider call outside the persistence lock.
pub fn assemble_metrics(
    conn: &Connection,
    client_id: &str,
    accounting_status: &SyncGuard,
    config: MetricAssemblyConfig<'_>,
    period: &DigestPeriod,
    today: &str,
) -> Result<OwnerDigestMetrics, StoreError> {
    let mut metrics =
        assemble_local_metrics(conn, client_id, accounting_status, config, period, today)?;
    let (start_ms, end_ms) = period_bounds_ms(period)?;
    if config
        .report
        .metrics
        .contains(&ReportMetricSection::CloseRate)
    {
        metrics.deals = assemble_hubspot_deal_metrics(conn, client_id, start_ms, end_ms);
    }
    Ok(metrics)
}

/// Assemble digest metrics that are backed by local caches only. Reads only;
/// each number comes through the owning slice's store/service (the accounting
/// figures via the same assembly the Accounting tab serves).
pub(crate) fn assemble_local_metrics(
    conn: &Connection,
    client_id: &str,
    accounting_status: &SyncGuard,
    config: MetricAssemblyConfig<'_>,
    period: &DigestPeriod,
    today: &str,
) -> Result<OwnerDigestMetrics, StoreError> {
    let sync = accounting::service::sync_info(conn, client_id, accounting_status)?;
    let last_synced_at_ms = sync.last_synced_at_ms;
    let financials = accounting::service::financials_from_store(
        conn,
        client_id,
        today,
        sync,
        config.accounting_metric,
    )?;
    let (period_sales_cents, prior_period_sales_cents) = match period.kind {
        OwnerReportPeriodKind::Weekly => (
            financials.week_to_date_cents,
            financials.prior_week_to_date_cents,
        ),
        OwnerReportPeriodKind::Mtd => (
            financials.month_to_date_cents,
            financials.prior_month_to_date_cents,
        ),
    };
    let sales = DigestSalesMetrics {
        basis: financials.basis,
        metric_basis: financials.metric_basis,
        metric_basis_label: financials.metric_basis_label,
        period_sales_cents,
        prior_period_sales_cents,
        mtd_gross_profit_cents: financials.mtd_gross_profit_cents,
        baseline_monthly_margin_cents: financials.baseline_monthly_margin_cents,
        margin_above_baseline_cents: financials.margin_above_baseline_cents,
        metric_value_cents: financials.metric_value_cents,
        metric_baseline_cents: financials.metric_baseline_cents,
        metric_above_baseline_cents: financials.metric_above_baseline_cents,
        metric_pending_reason: financials.metric_pending_reason,
        baseline_months_cached: financials.baseline_months_cached,
        last_synced_at_ms,
    };

    let (start_ms, end_ms) = period_bounds_ms(period)?;

    let calls_in_profile = config.report.metrics.contains(&ReportMetricSection::Calls);
    let pending_reason = if calls_in_profile {
        config.call_volume.pending_reason()
    } else {
        Some("Call-volume reporting is not part of this report profile.".to_string())
    };
    let calls_configured = pending_reason.is_none();
    let (
        call_log_messages,
        transfer_successful,
        callback_needed,
        no_callback_needed,
        unknown_outcome,
    ) = if calls_configured {
        assemble_call_outcome_counts(
            conn,
            client_id,
            config.call_volume.category_id.trim(),
            config.report.report_profile.as_deref(),
            start_ms,
            end_ms,
        )?
    } else {
        (0, 0, 0, 0, 0)
    };
    let calls = DigestCallMetrics {
        call_log_messages,
        transfer_successful,
        callback_needed,
        no_callback_needed,
        unknown_outcome,
        label: config.call_volume.metric_label(),
        source_label: config.call_volume.metric_source_label(),
        configured: calls_configured,
        pending_reason,
    };

    let open_tasks = crate::slices::follow_up_tasks::store::list_tasks(
        conn,
        client_id,
        Some(TaskStatus::Open),
        10_000,
        &crate::http::OperatorScope::All,
    )?;
    let policy = crate::slices::follow_up_tasks::service::WatchdogPolicy::default();
    let mut follow_ups = DigestFollowUpMetrics {
        open: open_tasks.len() as u64,
        done_in_period: crate::slices::follow_up_tasks::store::count_done_between(
            conn, client_id, start_ms, end_ms,
        )?,
        due_today: 0,
        overdue: 0,
        escalated: 0,
        critical: 0,
    };
    for entry in &open_tasks {
        let escalation = crate::slices::follow_up_tasks::service::classify_task_due(
            entry.task.due_date.as_deref(),
            today,
            &policy,
        );
        match escalation.lane {
            TaskDueLane::DueToday => follow_ups.due_today += 1,
            TaskDueLane::Overdue => {
                follow_ups.overdue += 1;
                match escalation.level {
                    TaskEscalationLevel::Escalated => follow_ups.escalated += 1,
                    TaskEscalationLevel::Critical => follow_ups.critical += 1,
                    _ => {}
                }
            }
            _ => {}
        }
    }

    let orders = if config.report.metrics.contains(&ReportMetricSection::Orders) {
        let order_counts = crate::slices::inventory::store::order_control_counts(
            conn,
            client_id,
            &period.start,
            &period.end,
        )?;
        DigestOrderMetrics {
            configured: true,
            pending_reason: None,
            orders_in_period: order_counts.orders_in_window,
            exceptions: order_counts.exceptions,
            deduction_failed: order_counts.deduction_failed,
            needs_mapping: order_counts.needs_mapping,
            packed_missing_photo: order_counts.packed_missing_photo,
            blocked: order_counts.blocked,
        }
    } else {
        pending_order_metrics("Inventory/order reporting is not part of this report profile.")
    };

    let inventory = if config
        .report
        .metrics
        .contains(&ReportMetricSection::Inventory)
    {
        let materials = crate::slices::inventory::store::list_materials(conn, client_id)?;
        let alerts = crate::slices::inventory::store::list_alerts(conn, client_id)?;
        let (stock_kpis, _) = crate::slices::inventory::service::compute_stock(&materials, &alerts);
        let purchase_orders =
            crate::slices::inventory::store::list_purchase_orders(conn, client_id)?;
        let (_, inbound_open_po_cents) =
            crate::slices::inventory::service::open_purchase_orders(&purchase_orders);
        DigestInventoryMetrics {
            configured: true,
            pending_reason: None,
            stocked_sku_count: u64::from(stock_kpis.monitored_materials),
            out_of_stock_count: u64::from(stock_kpis.out_of_stock_count),
            critical_count: u64::from(stock_kpis.critical_count),
            stock_value_cents: stock_kpis.stock_value_cents,
            inbound_open_po_cents,
        }
    } else {
        DigestInventoryMetrics {
            pending_reason: Some(
                "Inventory reporting is not part of this report profile.".to_string(),
            ),
            ..DigestInventoryMetrics::default()
        }
    };

    let claims = if config
        .report
        .metrics
        .contains(&ReportMetricSection::DamageClaims)
    {
        let damage_status_metrics =
            crate::slices::claim_drafts::store::damage_status_metrics_between(
                conn,
                client_id,
                &period.start,
                &period.end,
            )?;
        let damage_queue_status_counts =
            crate::slices::work_queue::store::status_counts_for_sources(
                conn,
                client_id,
                crate::slices::work_queue::SOURCE_KIND_STOCKFORGE_DAMAGE,
                &damage_status_metrics.damage_event_ids,
            )?;
        let (claims_drafted_in_period, claims_approved_in_period) =
            crate::slices::claim_drafts::store::claim_draft_counts_between(
                conn, client_id, start_ms, end_ms,
            )?;
        let claim_drafts_by_status =
            crate::slices::claim_drafts::store::claim_draft_status_counts_between(
                conn, client_id, start_ms, end_ms,
            )?
            .into_iter()
            .map(|(status, count)| DigestStatusCount { status, count })
            .collect();
        DigestClaimMetrics {
            configured: true,
            pending_reason: None,
            damage_events_in_period: damage_status_metrics.damage_events_in_period,
            damage_open: damage_status_metrics.damage_open,
            damage_resolved: damage_status_metrics.damage_resolved,
            damage_by_severity: damage_status_metrics
                .damage_by_severity
                .into_iter()
                .map(|(severity, count)| DigestSeverityCount { severity, count })
                .collect(),
            damage_by_status: damage_status_metrics
                .damage_by_status
                .into_iter()
                .map(|(status, count)| DigestStatusCount { status, count })
                .collect(),
            damage_by_type: damage_status_metrics
                .damage_by_type
                .into_iter()
                .map(|(damage_type, count)| DigestDamageTypeCount { damage_type, count })
                .collect(),
            queue_open: count_status(&damage_queue_status_counts, "open"),
            queue_accepted: count_status(&damage_queue_status_counts, "accepted"),
            queue_dismissed: count_status(&damage_queue_status_counts, "dismissed"),
            claims_drafted_in_period,
            claims_approved_in_period,
            claim_drafts_by_status,
        }
    } else {
        pending_claim_metrics(
            "StockForge damage/claim reporting is not part of this report profile.",
        )
    };
    let traffic = traffic_metrics(conn, client_id, period, config.search_console_overlay)?;

    Ok(OwnerDigestMetrics {
        metric_sections: metric_section_ids(config.report),
        sales,
        calls,
        follow_ups,
        orders,
        inventory,
        claims,
        traffic,
        deals: DigestDealMetrics::default(),
    })
}

fn assemble_call_outcome_counts(
    conn: &Connection,
    client_id: &str,
    category_id: &str,
    report_profile: Option<&str>,
    start_ms: u64,
    end_ms: u64,
) -> Result<(u64, u64, u64, u64, u64), StoreError> {
    use bos_profile_api::CallOutcome;

    let messages = crate::slices::email_triage::store::list_inbound_in_category_between(
        conn,
        client_id,
        category_id,
        start_ms,
        end_ms,
    )?;
    // The client report profile owns the parser-specific reason-code vocabulary;
    // the host only reads enrichment rows and counts the neutral outcomes. With
    // no profile selected, outcomes are unclassified (Unknown).
    let profile = report_profile.and_then(select_report_profile);
    let mut transfer_successful = 0;
    let mut callback_needed = 0;
    let mut no_callback_needed = 0;
    let mut unknown_outcome = 0;
    for message in &messages {
        let enrichments = crate::slices::email_triage::store::list_inbound_enrichments(
            conn,
            client_id,
            &message.source_key,
        )?;
        let reason_code = enrichments
            .iter()
            .flat_map(|enrichment| enrichment.parsed.attention_signals.iter())
            .find_map(|signal| {
                let reason = signal.reason_code.trim();
                (!reason.is_empty()).then_some(reason)
            });
        let outcome = match profile {
            Some(profile) => profile.classify_call_reason(reason_code),
            None => CallOutcome::Unknown,
        };
        match outcome {
            CallOutcome::TransferSuccessful => transfer_successful += 1,
            CallOutcome::CallbackNeeded => callback_needed += 1,
            CallOutcome::NoCallbackNeeded => no_callback_needed += 1,
            CallOutcome::Unknown => unknown_outcome += 1,
        }
    }
    Ok((
        messages.len() as u64,
        transfer_successful,
        callback_needed,
        no_callback_needed,
        unknown_outcome,
    ))
}

fn pending_order_metrics(reason: &str) -> DigestOrderMetrics {
    DigestOrderMetrics {
        configured: false,
        pending_reason: Some(reason.to_string()),
        orders_in_period: 0,
        exceptions: 0,
        deduction_failed: 0,
        needs_mapping: 0,
        packed_missing_photo: 0,
        blocked: 0,
    }
}

fn pending_claim_metrics(reason: &str) -> DigestClaimMetrics {
    DigestClaimMetrics {
        configured: false,
        pending_reason: Some(reason.to_string()),
        damage_events_in_period: 0,
        damage_open: 0,
        damage_resolved: 0,
        damage_by_severity: Vec::new(),
        damage_by_status: Vec::new(),
        damage_by_type: Vec::new(),
        queue_open: 0,
        queue_accepted: 0,
        queue_dismissed: 0,
        claims_drafted_in_period: 0,
        claims_approved_in_period: 0,
        claim_drafts_by_status: Vec::new(),
    }
}

fn traffic_metrics(
    conn: &Connection,
    client_id: &str,
    period: &DigestPeriod,
    search_console_overlay: Option<&SearchConsoleOverlay>,
) -> Result<DigestTrafficMetrics, StoreError> {
    let config = crate::slices::search_console::service::config(search_console_overlay);
    let analytics = analytics_metrics(conn, client_id, period, &config)?;
    let Some(effective) =
        crate::slices::search_console::service::effective_property(conn, client_id, &config)?
    else {
        return Ok(DigestTrafficMetrics {
            configured: false,
            property_url: config.property_url,
            has_data: false,
            last_synced_at_ms: None,
            totals: SearchConsoleMetricTotals::default(),
            branded: SearchConsoleMetricTotals::default(),
            nonbranded: SearchConsoleMetricTotals::default(),
            behavior_configured: analytics.behavior_configured,
            behavior_pending_reason: analytics.behavior_pending_reason,
            conversion_tracking_configured: analytics.behavior_configured,
            conversion_tracking_pending_reason: (!analytics.behavior_configured)
                .then(|| CONVERSION_TRACKING_PENDING_REASON.to_string()),
            retargeting_configured: false,
            retargeting_pending_reason: Some(RETARGETING_PENDING_REASON.to_string()),
            behavior_has_data: analytics.behavior_has_data,
            behavior_week: analytics.behavior_week,
            behavior_month_to_date: analytics.behavior_month_to_date,
            top_landing_pages_week: analytics.top_landing_pages_week,
            top_sources_week: analytics.top_sources_week,
        });
    };
    let property_url = effective.property_url.as_str();
    let cursor = crate::slices::search_console::store::get_cursor(conn, client_id, property_url)?;
    let totals = crate::slices::search_console::store::sum_daily(
        conn,
        client_id,
        property_url,
        &period.start,
        &period.end,
    )?;
    let branded = crate::slices::search_console::store::sum_dimension(
        conn,
        client_id,
        property_url,
        "query",
        Some(true),
        &period.start,
        &period.end,
    )?;
    let nonbranded = crate::slices::search_console::store::sum_dimension(
        conn,
        client_id,
        property_url,
        "query",
        Some(false),
        &period.start,
        &period.end,
    )?;
    Ok(DigestTrafficMetrics {
        configured: true,
        property_url: Some(property_url.to_string()),
        has_data: totals.clicks > 0 || totals.impressions > 0,
        last_synced_at_ms: cursor.last_synced_at_ms,
        totals,
        branded,
        nonbranded,
        behavior_configured: analytics.behavior_configured,
        behavior_pending_reason: analytics.behavior_pending_reason,
        conversion_tracking_configured: analytics.behavior_configured,
        conversion_tracking_pending_reason: (!analytics.behavior_configured)
            .then(|| CONVERSION_TRACKING_PENDING_REASON.to_string()),
        retargeting_configured: false,
        retargeting_pending_reason: Some(RETARGETING_PENDING_REASON.to_string()),
        behavior_has_data: analytics.behavior_has_data,
        behavior_week: analytics.behavior_week,
        behavior_month_to_date: analytics.behavior_month_to_date,
        top_landing_pages_week: analytics.top_landing_pages_week,
        top_sources_week: analytics.top_sources_week,
    })
}

struct DigestAnalyticsMetrics {
    behavior_configured: bool,
    behavior_pending_reason: Option<String>,
    behavior_has_data: bool,
    behavior_week: AnalyticsMetricTotals,
    behavior_month_to_date: AnalyticsMetricTotals,
    top_landing_pages_week: Vec<bos_contracts::search_console::AnalyticsBreakdownRow>,
    top_sources_week: Vec<bos_contracts::search_console::AnalyticsBreakdownRow>,
}

fn analytics_metrics(
    conn: &Connection,
    client_id: &str,
    period: &DigestPeriod,
    config: &crate::slices::search_console::service::SearchConsoleConfig,
) -> Result<DigestAnalyticsMetrics, StoreError> {
    let Some(property_id) = config
        .ga4_property_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(DigestAnalyticsMetrics {
            behavior_configured: false,
            behavior_pending_reason: Some(BEHAVIOR_ANALYTICS_PENDING_REASON.to_string()),
            behavior_has_data: false,
            behavior_week: AnalyticsMetricTotals::default(),
            behavior_month_to_date: AnalyticsMetricTotals::default(),
            top_landing_pages_week: Vec::new(),
            top_sources_week: Vec::new(),
        });
    };
    let behavior_week = crate::slices::search_console::service::analytics_reporting_metrics(
        conn,
        client_id,
        property_id,
        &period.start,
        &period.end,
        config,
    )?;
    let behavior_month_to_date =
        crate::slices::search_console::service::analytics_reporting_metrics(
            conn,
            client_id,
            property_id,
            &crate::slices::accounting::service::month_start_date(&period.end)
                .unwrap_or_else(|| period.start.clone()),
            &period.end,
            config,
        )?;
    Ok(DigestAnalyticsMetrics {
        behavior_configured: true,
        behavior_pending_reason: None,
        behavior_has_data: behavior_week.included.sessions > 0
            || behavior_week.included.total_users > 0,
        behavior_week: behavior_week.included,
        behavior_month_to_date: behavior_month_to_date.included,
        top_landing_pages_week: crate::slices::search_console::store::top_analytics_dimensions(
            conn,
            client_id,
            property_id,
            "landing_page",
            &period.start,
            &period.end,
            5,
        )?,
        top_sources_week: behavior_week.top_sources,
    })
}

fn count_status(counts: &[(String, u64)], status: &str) -> u64 {
    counts
        .iter()
        .find(|(entry_status, _)| entry_status == status)
        .map(|(_, count)| *count)
        .unwrap_or(0)
}

pub(crate) fn period_bounds_ms(period: &DigestPeriod) -> Result<(u64, u64), StoreError> {
    let start_ms = accounting::service::date_to_epoch_ms(&period.start)
        .ok_or_else(|| StoreError::Domain("owner_report_bad_period_start".to_string()))?;
    let end_ms = accounting::service::date_to_epoch_ms(&period.end)
        .ok_or_else(|| StoreError::Domain("owner_report_bad_period_end".to_string()))?
        + 86_400_000; // end date inclusive -> exclusive next midnight
    Ok((start_ms, end_ms))
}

fn csv_env(var: &crate::env_registry::EnvVar) -> Vec<String> {
    crate::env_registry::string(var)
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn hubspot_deal_env_config() -> bos_integrations::hubspot::HubSpotDealReadConfig {
    bos_integrations::hubspot::HubSpotDealReadConfig {
        access_token: crate::env_registry::string(&crate::env_registry::BOS_HUBSPOT_ACCESS_TOKEN),
        pipeline_id: crate::env_registry::string(
            &crate::env_registry::BOS_HUBSPOT_DEALS_PIPELINE_ID,
        ),
        open_stage_ids: csv_env(&crate::env_registry::BOS_HUBSPOT_DEALS_OPEN_STAGE_IDS),
        won_stage_ids: csv_env(&crate::env_registry::BOS_HUBSPOT_DEALS_WON_STAGE_IDS),
        lost_stage_ids: csv_env(&crate::env_registry::BOS_HUBSPOT_DEALS_LOST_STAGE_IDS),
        started_date_property: crate::env_registry::string(
            &crate::env_registry::BOS_HUBSPOT_DEALS_STARTED_DATE_PROPERTY,
        ),
        closed_date_property: crate::env_registry::string(
            &crate::env_registry::BOS_HUBSPOT_DEALS_CLOSED_DATE_PROPERTY,
        ),
        segment_properties: csv_env(&crate::env_registry::BOS_HUBSPOT_DEALS_SEGMENT_PROPERTIES),
    }
}

pub(crate) fn hubspot_deal_config_for_client(
    conn: &Connection,
    client_id: &str,
) -> Result<bos_integrations::hubspot::HubSpotDealReadConfig, String> {
    let env_config = hubspot_deal_env_config();
    if env_config.missing_reason().is_none() {
        return Ok(env_config);
    }
    let saved = crate::slices::home_dashboard::store::load_hubspot_deal_mapping(conn, client_id)
        .map_err(|err| format!("hubspot deal mapping unavailable: {err}"))?;
    let Some(mapping) = saved.mapping else {
        return Ok(env_config);
    };
    let mut open_stage_ids = Vec::new();
    let mut won_stage_ids = Vec::new();
    let mut lost_stage_ids = Vec::new();
    for stage in mapping.stage_mappings {
        match stage.status {
            bos_contracts::home_dashboard::HubSpotDealMappedStatus::Open => {
                open_stage_ids.push(stage.stage_id)
            }
            bos_contracts::home_dashboard::HubSpotDealMappedStatus::Won => {
                won_stage_ids.push(stage.stage_id)
            }
            bos_contracts::home_dashboard::HubSpotDealMappedStatus::Lost => {
                lost_stage_ids.push(stage.stage_id)
            }
        }
    }
    Ok(bos_integrations::hubspot::HubSpotDealReadConfig {
        access_token: env_config.access_token,
        pipeline_id: Some(mapping.pipeline_id),
        open_stage_ids,
        won_stage_ids,
        lost_stage_ids,
        started_date_property: Some(mapping.started_date_property),
        closed_date_property: Some(mapping.closed_date_property),
        segment_properties: env_config.segment_properties,
    })
}

pub(crate) fn assemble_hubspot_deal_metrics(
    conn: &Connection,
    client_id: &str,
    start_ms: u64,
    end_ms: u64,
) -> DigestDealMetrics {
    let crm_provider = env_registry::string(&env_registry::BOS_CRM_PROVIDER)
        .unwrap_or_else(|| "hubspot".to_string())
        .trim()
        .to_ascii_lowercase();
    if crm_provider == "hubspot" {
        return assemble_hubspot_deal_metrics_with_config(
            hubspot_deal_config_for_client(conn, client_id),
            start_ms,
            end_ms,
        );
    }
    assemble_deal_metrics_for_provider(&crm_provider, start_ms, end_ms)
}

pub(crate) fn assemble_deal_metrics_for_provider(
    crm_provider: &str,
    start_ms: u64,
    end_ms: u64,
) -> DigestDealMetrics {
    if crm_provider == "espocrm" {
        return DigestDealMetrics {
            status: DigestDealMetricsStatus::PendingConfig,
            source: "espocrm_business_metrics".to_string(),
            message: "EspoCRM opportunity/profit mapping is not configured yet.".to_string(),
            ..DigestDealMetrics::default()
        };
    }
    if crm_provider != "hubspot" {
        return DigestDealMetrics {
            status: DigestDealMetricsStatus::PendingConfig,
            source: "crm_business_metrics".to_string(),
            message: format!(
                "{crm_provider} owner-report close-rate metrics are not configured yet."
            ),
            ..DigestDealMetrics::default()
        };
    }
    let config = hubspot_deal_env_config();
    assemble_hubspot_deal_metrics_with_config(Ok(config), start_ms, end_ms)
}

pub(crate) fn assemble_hubspot_deal_metrics_with_config(
    config: Result<bos_integrations::hubspot::HubSpotDealReadConfig, String>,
    start_ms: u64,
    end_ms: u64,
) -> DigestDealMetrics {
    let source = "hubspot_deals".to_string();
    let config = match config {
        Ok(config) => config,
        Err(message) => {
            return DigestDealMetrics {
                status: DigestDealMetricsStatus::PendingConfig,
                source,
                message,
                ..DigestDealMetrics::default()
            }
        }
    };
    if let Some(reason) = config.missing_reason() {
        return DigestDealMetrics {
            status: DigestDealMetricsStatus::PendingConfig,
            source,
            message: reason,
            ..DigestDealMetrics::default()
        };
    }
    let client = match bos_integrations::hubspot::hubspot_deal_read_client(&config) {
        Ok(client) => client,
        Err(reason) => {
            return DigestDealMetrics {
                status: DigestDealMetricsStatus::PendingConfig,
                source,
                message: reason,
                ..DigestDealMetrics::default()
            }
        }
    };
    match client.reporting_snapshot(&config, start_ms as i64, end_ms as i64) {
        Ok(snapshot) => DigestDealMetrics {
            status: DigestDealMetricsStatus::Available,
            source,
            message: if snapshot.closed_deals == 0 {
                "No won/lost deals closed in this period.".to_string()
            } else {
                "Computed from HubSpot deals closed in this period.".to_string()
            },
            closed_deals: Some(snapshot.closed_deals),
            won_deals: Some(snapshot.won_deals),
            lost_deals: Some(snapshot.lost_deals),
            close_rate_bps: snapshot.close_rate_bps,
            avg_contact_to_close_days: snapshot.avg_contact_to_close_days,
            contact_to_close_sample: Some(snapshot.contact_to_close_sample),
            segment_cuts: snapshot.segment_cuts,
        },
        Err(bos_integrations::hubspot::HubSpotReadError::Limited { code, message })
        | Err(bos_integrations::hubspot::HubSpotReadError::Retryable { code, message }) => {
            DigestDealMetrics {
                status: DigestDealMetricsStatus::LimitedData,
                source,
                message: format!("{code}: {message}"),
                segment_cuts: config.segment_properties,
                ..DigestDealMetrics::default()
            }
        }
    }
}

/// Grouped, sign-aware dollars: -123456 → "-$1,234.56".
pub fn format_dollars(cents: i64) -> String {
    let sign = if cents < 0 { "-" } else { "" };
    let abs = cents.unsigned_abs();
    let whole = abs / 100;
    let frac = abs % 100;
    let digits = whole.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    format!("{sign}${grouped}.{frac:02}")
}

/// Every formatted dollar string the narration may use — each money field in
/// the metrics, signed and absolute. The validator refuses any other amount.
pub fn grounded_amounts(metrics: &OwnerDigestMetrics) -> Vec<String> {
    let mut amounts = Vec::new();
    let mut push = |cents: i64| {
        let formatted = format_dollars(cents);
        if !amounts.contains(&formatted) {
            amounts.push(formatted);
        }
        let absolute = format_dollars(cents.abs());
        if !amounts.contains(&absolute) {
            amounts.push(absolute);
        }
    };
    push(metrics.sales.period_sales_cents);
    if let Some(cents) = metrics.sales.prior_period_sales_cents {
        push(cents);
    }
    if let Some(cents) = metrics.sales.mtd_gross_profit_cents {
        push(cents);
    }
    if let Some(cents) = metrics.sales.baseline_monthly_margin_cents {
        push(cents);
    }
    if let Some(cents) = metrics.sales.margin_above_baseline_cents {
        push(cents);
    }
    if let Some(cents) = metrics.sales.metric_value_cents {
        push(cents);
    }
    if let Some(cents) = metrics.sales.metric_baseline_cents {
        push(cents);
    }
    if let Some(cents) = metrics.sales.metric_above_baseline_cents {
        push(cents);
    }
    if metrics.inventory.configured {
        push(metrics.inventory.stock_value_cents);
        push(metrics.inventory.inbound_open_po_cents);
    }
    amounts
}

pub fn build_narration_request(
    client_id: &str,
    report_id: &str,
    period: &DigestPeriod,
    metrics: &OwnerDigestMetrics,
    client_profile: Option<&ClientProfile>,
    attempt: u64,
) -> TypedLlmTaskRequest {
    let task_id = format!("owner_digest_{report_id}_{attempt}");
    let allowed_amounts = grounded_amounts(metrics);
    TypedLlmTaskRequest {
        task_id: task_id.clone(),
        correlation_id: report_id.to_string(),
        idempotency_key: task_id,
        tenant_or_project_scope: client_id.to_string(),
        source_entity: Some(TypedLlmSourceEntity {
            entity_kind: super::store::REPORT_ENTITY_KIND.to_string(),
            entity_id: report_id.to_string(),
        }),
        spec: TypedLlmTaskSpec {
            task_class: TypedLlmTaskClass::Draft,
            prompt_template_id: "owner_digest_narration".to_string(),
            prompt_template_version: "2".to_string(),
            prompt_template_hash: String::new(),
            schema_ref: NARRATION_SCHEMA_REF.to_string(),
            response_format: TypedLlmResponseFormat::JsonObject,
            max_input_bytes: 32 * 1024,
            max_output_bytes: 8 * 1024,
            max_tokens: 0, // filled from runtime config
            timeout_ms: 0, // filled from runtime config
            capabilities: TypedLlmTaskCapabilities::pure_transformation(),
            authority: TypedLlmAuthority::no_side_effects(),
        },
        input: TypedLlmTaskInput {
            json: json!({
                "instructions": "Write the owner digest narration for the configured client profile from METRICS only. The client_profile object is deployment config seeded from the client overlay; use it only for business context and tone, never to invent results. Cover only active_metric_sections. Do not mention calls, inventory, orders, damage/claims, site traffic, or close-rate unless that section id is present in active_metric_sections. Respond with a single JSON object with EXACTLY these fields: headline (one plain-English sentence, the week/month in a glance), narrative (2-6 factual sentences over the active metrics: sales vs prior period, configured financial metric vs baseline when present, and any configured operational sections), callouts (array of 0-5 short strings, each one configured metric worth the owner's attention; empty array when nothing stands out), confidence (\"high\" | \"medium\" | \"low\"). Every dollar amount you write MUST be copied character-for-character from allowed_amounts — write no other dollar amounts. Counts must match the metrics exactly. When a configured metric has configured=false or pending/limited status, state that it is pending/limited instead of writing 0.",
                "client_profile": client_profile,
                "active_metric_sections": metrics.metric_sections,
                "period": {
                    "kind": super::store::period_kind_str(period.kind),
                    "start": period.start,
                    "end": period.end,
                },
                "metrics": metrics,
                "allowed_amounts": allowed_amounts,
                "hubspot_deal_reporting_note": "If metrics.deals.status is available, closed_deals/won_deals/lost_deals/close_rate_bps/avg_contact_to_close_days are read-only HubSpot deal reporting numbers for the configured pipeline. If pending_config or limited_data, mention that close-rate reporting is not fully available only when it is material; do not invent deal counts.",
            }),
            text_blocks: Vec::new(),
        },
        execution_policy: TypedLlmExecutionPolicy {
            default_route: TypedLlmExecutionRoute::Harness, // realigned by the router
            fallback_policy: TypedLlmFallbackPolicy::NoFallback,
            retry_policy: TypedLlmRetryPolicy {
                max_attempts: 2,
                backoff_ms: 1_000,
                max_elapsed_ms: 240_000,
            },
        },
        provider_policy: TypedLlmProviderPolicy {
            preferred_provider: String::new(),
            preferred_model: String::new(),
            fallback_provider: None,
            fallback_model: None,
        },
        safety_policy: TypedLlmSafetyPolicy {
            redaction_policy: TypedLlmRedactionPolicy::PreSubmit,
            raw_output_retention: TypedLlmRawOutputRetention::None,
        },
    }
}

/// A validated narration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestNarration {
    pub headline: String,
    pub narrative: String,
    pub callouts: Vec<String>,
    pub confidence: String,
}

/// Every `$amount` token in `text` ("$1,234.56" shapes; a leading '-' binds
/// to the amount). The grounding scanner.
fn dollar_tokens(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'$' {
            index += 1;
            continue;
        }
        let mut end = index + 1;
        while end < bytes.len()
            && (bytes[end].is_ascii_digit() || bytes[end] == b',' || bytes[end] == b'.')
        {
            end += 1;
        }
        // Trailing punctuation ("...$1,234." at sentence end) is not amount.
        let mut token_end = end;
        while token_end > index + 1 && matches!(bytes[token_end - 1], b'.' | b',') {
            token_end -= 1;
        }
        if token_end > index + 1 {
            let signed_start = if index > 0 && bytes[index - 1] == b'-' {
                index - 1
            } else {
                index
            };
            tokens.push(text[signed_start..token_end].to_string());
        }
        index = end.max(index + 1);
    }
    tokens
}

/// Parse + ground the narration. Any dollar amount in the prose that is not
/// literally one of the metrics' formatted amounts refuses the output — the
/// model may phrase, never price.
pub fn parse_narration_response(
    response: &serde_json::Value,
    metrics: &OwnerDigestMetrics,
) -> Result<DigestNarration, String> {
    let headline = string_field(response, "headline").ok_or("headline missing or empty")?;
    let narrative = string_field(response, "narrative").ok_or("narrative missing or empty")?;
    let confidence = string_field(response, "confidence")
        .filter(|raw| matches!(raw.as_str(), "high" | "medium" | "low"))
        .ok_or("confidence missing or invalid")?;
    let callouts: Vec<String> = response
        .get("callouts")
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(|entry| entry.chars().take(300).collect::<String>())
                .take(5)
                .collect()
        })
        .unwrap_or_default();
    let allowed = grounded_amounts(metrics);
    for text in std::iter::once(headline.as_str())
        .chain(std::iter::once(narrative.as_str()))
        .chain(callouts.iter().map(String::as_str))
    {
        for token in dollar_tokens(text) {
            if !allowed.contains(&token) {
                return Err(format!(
                    "narration contains a dollar amount not present in the metrics: {token}"
                ));
            }
        }
    }
    Ok(DigestNarration {
        headline: headline.chars().take(300).collect(),
        narrative: narrative.chars().take(2_000).collect(),
        callouts,
        confidence,
    })
}

/// Assemble the stored report row: metrics always, narration when the
/// transform succeeded, the failure code when it did not (the digest is
/// still useful as numbers).
pub fn report_from_parts(
    period: &DigestPeriod,
    metrics: OwnerDigestMetrics,
    narration: Result<(DigestNarration, String), String>,
    now_ms: u64,
) -> OwnerReport {
    let report_id = report_id_for(period.kind, &period.start);
    let (status, headline, narrative, callouts, confidence, model, narration_error) =
        match narration {
            Ok((narration, model)) => (
                OwnerReportStatus::Complete,
                non_empty(narration.headline),
                non_empty(narration.narrative),
                narration.callouts,
                non_empty(narration.confidence),
                non_empty(model),
                None,
            ),
            Err(error) => (
                OwnerReportStatus::NarrationFailed,
                None,
                None,
                Vec::new(),
                None,
                None,
                Some(error.chars().take(300).collect::<String>()),
            ),
        };
    OwnerReport {
        report_id,
        period_kind: period.kind,
        period_start: period.start.clone(),
        period_end: period.end.clone(),
        as_of_date: period.end.clone(),
        status,
        metrics,
        headline,
        narrative,
        callouts,
        confidence,
        model,
        narration_error,
        outbox_job_id: None,
        generated_at_ms: now_ms,
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
    }
}

fn non_empty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

fn period_title(report: &OwnerReport) -> String {
    match report.period_kind {
        OwnerReportPeriodKind::Weekly => format!(
            "Week of {} (through {})",
            report.period_start, report.period_end
        ),
        OwnerReportPeriodKind::Mtd => format!(
            "Month to date {} – {}",
            report.period_start, report.period_end
        ),
    }
}

/// Render the digest as the owner email — deterministic text over the
/// stored metrics + narration. Pending-decision metrics are named as
/// pending, never silently absent (an owner reading the digest should know
/// what it does not cover yet).
pub fn render_digest_email(report: &OwnerReport) -> (String, String) {
    render_digest_email_with_config(report, &OwnerReportConfig::default())
}

pub fn render_digest_email_with_config(
    report: &OwnerReport,
    config: &OwnerReportConfig,
) -> (String, String) {
    render_digest_email_for_recipients(report, config, &config.recipients)
}

pub fn render_digest_email_for_recipients(
    report: &OwnerReport,
    config: &OwnerReportConfig,
    recipients: &[String],
) -> (String, String) {
    let metrics = &report.metrics;
    let subject = format!("{} — {}", config.subject_prefix, period_title(report));
    let sections = effective_metric_sections(config, recipients);
    let include_narration = sections.contains(&ReportMetricSection::Sales);
    let mut body = String::new();
    body.push_str(&format!("OWNER DIGEST — {}\n", period_title(report)));
    body.push_str(&format!(
        "Assembled {} from local data.\n\n",
        report.as_of_date
    ));
    if include_narration {
        if let (Some(headline), Some(narrative)) = (&report.headline, &report.narrative) {
            body.push_str(&format!("{headline}\n\n{narrative}\n"));
            if !report.callouts.is_empty() {
                body.push_str("\nWorth your attention:\n");
                for callout in &report.callouts {
                    body.push_str(&format!("- {callout}\n"));
                }
            }
            body.push('\n');
        }
    }
    for section in &sections {
        match section {
            ReportMetricSection::Sales => render_sales_section(&mut body, metrics),
            ReportMetricSection::Calls => render_calls_section(&mut body, metrics),
            ReportMetricSection::FollowUps => render_follow_ups_section(&mut body, metrics),
            ReportMetricSection::Inventory => render_inventory_section(&mut body, metrics),
            ReportMetricSection::Orders => render_orders_section(&mut body, metrics),
            ReportMetricSection::DamageClaims => render_damage_claims_section(&mut body, metrics),
            ReportMetricSection::SiteTraffic => render_traffic_section(&mut body, metrics),
            ReportMetricSection::CloseRate => render_deals_section(&mut body, metrics),
        }
    }
    (subject, body)
}

fn render_sales_section(body: &mut String, metrics: &OwnerDigestMetrics) {
    body.push_str(&format!(
        "SALES ({})\n- Period sales: {}{}\n",
        if metrics.sales.basis == "invoice_totals" {
            "invoice totals — invoices only, no sales receipts/credit notes"
        } else {
            "QuickBooks P&L"
        },
        format_dollars(metrics.sales.period_sales_cents),
        metrics
            .sales
            .prior_period_sales_cents
            .map(|cents| format!(" (prior period {})", format_dollars(cents)))
            .unwrap_or_default(),
    ));
    if let Some(cents) = metrics.sales.mtd_gross_profit_cents {
        body.push_str(&format!("- MTD gross profit: {}\n", format_dollars(cents)));
    }
    if metrics.sales.metric_basis != "gross_margin" {
        match (
            metrics.sales.metric_above_baseline_cents,
            metrics.sales.metric_baseline_cents,
        ) {
            (Some(delta), Some(baseline)) => body.push_str(&format!(
                "- {} above baseline: {} (baseline {} / month)\n",
                metrics.sales.metric_basis_label,
                format_dollars(delta),
                format_dollars(baseline),
            )),
            _ => {
                let reason = metrics
                    .sales
                    .metric_pending_reason
                    .as_deref()
                    .unwrap_or("not yet computable");
                body.push_str(&format!(
                    "- {} above baseline: {}\n",
                    metrics.sales.metric_basis_label, reason
                ));
            }
        }
    }
}

fn render_calls_section(body: &mut String, metrics: &OwnerDigestMetrics) {
    body.push_str("\nCALLS\n");
    if metrics.calls.configured {
        body.push_str(&format!(
            "- {}: {} ({})\n",
            metrics.calls.label, metrics.calls.call_log_messages, metrics.calls.source_label
        ));
        body.push_str(&format!(
            "- Outcomes: {} transferred, {} need callback, {} no callback, {} unknown\n",
            metrics.calls.transfer_successful,
            metrics.calls.callback_needed,
            metrics.calls.no_callback_needed,
            metrics.calls.unknown_outcome
        ));
    } else {
        body.push_str(&format!(
            "- {}: pending data ({})\n",
            metrics.calls.label,
            metrics
                .calls
                .pending_reason
                .as_deref()
                .unwrap_or("call-volume source is not configured")
        ));
    }
}

fn render_follow_ups_section(body: &mut String, metrics: &OwnerDigestMetrics) {
    body.push_str(&format!(
        "\nFOLLOW-UPS\n- Completed this period: {}\n- Open now: {} ({} due today, {} overdue, {} escalated, {} critical)\n",
        metrics.follow_ups.done_in_period,
        metrics.follow_ups.open,
        metrics.follow_ups.due_today,
        metrics.follow_ups.overdue,
        metrics.follow_ups.escalated,
        metrics.follow_ups.critical,
    ));
}

fn render_orders_section(body: &mut String, metrics: &OwnerDigestMetrics) {
    body.push_str(&format!(
        "\nORDERS\n- Orders in period: {}\n- Current backlog: {} exceptions, {} deduction failed, {} need SKU mapping, {} packed missing photo, {} blocked\n",
        metrics.orders.orders_in_period,
        metrics.orders.exceptions,
        metrics.orders.deduction_failed,
        metrics.orders.needs_mapping,
        metrics.orders.packed_missing_photo,
        metrics.orders.blocked,
    ));
}

fn render_inventory_section(body: &mut String, metrics: &OwnerDigestMetrics) {
    if metrics.inventory.configured {
        body.push_str(&format!(
            "\nINVENTORY\n- Stocked SKUs: {} ({} out, {} critical)\n- Stocked valuation: {}\n- Inbound on open POs: {}\n",
            metrics.inventory.stocked_sku_count,
            metrics.inventory.out_of_stock_count,
            metrics.inventory.critical_count,
            format_dollars(metrics.inventory.stock_value_cents),
            format_dollars(metrics.inventory.inbound_open_po_cents),
        ));
    } else {
        body.push_str(&format!(
            "\nINVENTORY\n- Pending data ({})\n",
            metrics
                .inventory
                .pending_reason
                .as_deref()
                .unwrap_or("inventory reporting is not configured")
        ));
    }
}

fn render_damage_claims_section(body: &mut String, metrics: &OwnerDigestMetrics) {
    body.push_str(&format!(
        "\nDAMAGE / CLAIMS\n- Damage events in period: {} ({} open, {} resolved){}\n- Queue now: {} open · {} accepted · {} dismissed\n- Claim packets: {} drafted · {} approved{}\n",
        metrics.claims.damage_events_in_period,
        metrics.claims.damage_open,
        metrics.claims.damage_resolved,
        if metrics.claims.damage_by_severity.is_empty() {
            String::new()
        } else {
            format!(
                " ({})",
                metrics
                    .claims
                    .damage_by_severity
                    .iter()
                    .map(|entry| format!("{} {}", entry.count, entry.severity))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        },
        metrics.claims.queue_open,
        metrics.claims.queue_accepted,
        metrics.claims.queue_dismissed,
        metrics.claims.claims_drafted_in_period,
        metrics.claims.claims_approved_in_period,
        if metrics.claims.claim_drafts_by_status.is_empty() {
            String::new()
        } else {
            format!(
                " ({})",
                metrics
                    .claims
                    .claim_drafts_by_status
                    .iter()
                    .map(|entry| format!("{} {}", entry.count, entry.status))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        },
    ));
}

fn render_traffic_section(body: &mut String, metrics: &OwnerDigestMetrics) {
    if metrics.traffic.configured {
        body.push_str(&format!(
            "\nSITE TRAFFIC\n- Organic search clicks: {} ({} branded, {} non-branded)\n- Organic search impressions: {}\n",
            metrics.traffic.totals.clicks,
            metrics.traffic.branded.clicks,
            metrics.traffic.nonbranded.clicks,
            metrics.traffic.totals.impressions,
        ));
        if metrics.traffic.last_synced_at_ms.is_none() {
            body.push_str("- Pending data: Search Console has not synced yet.\n");
        }
    } else {
        body.push_str(
            "\nSITE TRAFFIC\n- Pending data: configure Search Console property/access.\n",
        );
    }
    if !metrics.traffic.behavior_configured {
        body.push_str(&format!(
            "- Behavior analytics pending: {}\n",
            metrics
                .traffic
                .behavior_pending_reason
                .as_deref()
                .unwrap_or("GA4 behavior/acquisition data is not configured.")
        ));
    } else if metrics.traffic.behavior_has_data {
        body.push_str(&format!(
            "- Website behavior: {} sessions, {} users, {} conversions this period.\n",
            metrics.traffic.behavior_week.sessions,
            metrics.traffic.behavior_week.total_users,
            metrics.traffic.behavior_week.conversions,
        ));
        if !metrics.traffic.top_landing_pages_week.is_empty() {
            body.push_str(&format!(
                "- Top landing pages: {}\n",
                metrics
                    .traffic
                    .top_landing_pages_week
                    .iter()
                    .take(3)
                    .map(|row| format!("{} ({} sessions)", row.value, row.metrics.sessions))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !metrics.traffic.top_sources_week.is_empty() {
            body.push_str(&format!(
                "- Top acquisition sources: {}\n",
                metrics
                    .traffic
                    .top_sources_week
                    .iter()
                    .take(3)
                    .map(|row| format!("{} ({} sessions)", row.value, row.metrics.sessions))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    } else {
        body.push_str("- Website behavior: GA4 is configured but no behavior snapshots are available for this period yet.\n");
    }
    if !metrics.traffic.conversion_tracking_configured {
        body.push_str(&format!(
            "- Conversion tracking pending: {}\n",
            metrics
                .traffic
                .conversion_tracking_pending_reason
                .as_deref()
                .unwrap_or("Conversion events are not configured.")
        ));
    }
    if !metrics.traffic.retargeting_configured {
        body.push_str(&format!(
            "- Retargeting pending: {}\n",
            metrics
                .traffic
                .retargeting_pending_reason
                .as_deref()
                .unwrap_or("Retargeting setup is outside BusinessOS writes.")
        ));
    }
}

fn render_deals_section(body: &mut String, metrics: &OwnerDigestMetrics) {
    body.push_str(&format!(
        "\n{}\n",
        if metrics.deals.source == "hubspot_deals" {
            "HUBSPOT DEALS"
        } else {
            "CRM BUSINESS METRICS"
        }
    ));
    match metrics.deals.status {
        DigestDealMetricsStatus::Available => {
            let rate = metrics
                .deals
                .close_rate_bps
                .map(|bps| format!("{:.1}%", f64::from(bps) / 100.0))
                .unwrap_or_else(|| "n/a".to_string());
            let days = metrics
                .deals
                .avg_contact_to_close_days
                .map(|days| format!("{days} days"))
                .unwrap_or_else(|| "n/a".to_string());
            body.push_str(&format!(
                "- Close rate: {rate} ({} won / {} lost / {} closed)\n- Avg contact-to-close: {days} ({} deals with both dates)\n",
                metrics.deals.won_deals.unwrap_or(0),
                metrics.deals.lost_deals.unwrap_or(0),
                metrics.deals.closed_deals.unwrap_or(0),
                metrics.deals.contact_to_close_sample.unwrap_or(0),
            ));
            if !metrics.deals.segment_cuts.is_empty() {
                body.push_str(&format!(
                    "- Segment cuts: {}\n",
                    metrics.deals.segment_cuts.join(", ")
                ));
            }
        }
        DigestDealMetricsStatus::PendingConfig => body.push_str(&format!(
            "- Pending configuration: {}\n",
            metrics.deals.message
        )),
        DigestDealMetricsStatus::LimitedData => body.push_str(&format!(
            "- Limited data: {} (HubSpot plan/API access may restrict deal reporting)\n",
            metrics.deals.message
        )),
    }
}

/// Build the gated Gmail-draft outbox job for the digest email. `to_addr`
/// comes from BOS_REPORT_DIGEST_TO_ADDR (the owners' mailbox).
pub fn build_email_job(
    report: &OwnerReport,
    to_addr: &str,
    credential_user_id: Option<&str>,
    actor_id: &str,
    now_ms: u64,
) -> Result<NewOutboxJob, String> {
    build_email_job_with_config(
        report,
        to_addr,
        credential_user_id,
        actor_id,
        now_ms,
        &OwnerReportConfig::default(),
    )
}

pub fn build_email_job_with_config(
    report: &OwnerReport,
    to_addr: &str,
    credential_user_id: Option<&str>,
    actor_id: &str,
    now_ms: u64,
    config: &OwnerReportConfig,
) -> Result<NewOutboxJob, String> {
    // One job per generation: a regenerated digest emails under a fresh key.
    let idempotency_key = format!(
        "ownerreport:{}:{}",
        report.report_id, report.generated_at_ms
    );
    let recipients = split_recipients(to_addr);
    let (subject, body_text) = render_digest_email_for_recipients(report, config, &recipients);
    let payload = GmailDraftCreateOutboxPayload {
        idempotency_key: idempotency_key.clone(),
        credential_user_id: credential_user_id.map(str::to_string),
        approval: GmailDraftApprovalMetadata {
            approval_id: format!("appr_{}_{}", report.report_id, report.generated_at_ms),
            approved_by: actor_id.to_string(),
            approved_at: crate::produce::epoch_ms_to_rfc3339_utc(now_ms),
        },
        to: to_addr.to_string(),
        cc: Vec::new(),
        subject,
        body_text,
        thread_id: None,
        reply_message_id: None,
        reference_message_ids: Vec::new(),
    };
    Ok(NewOutboxJob {
        job_id: format!("obj_{}_{}", report.report_id, report.generated_at_ms),
        provider: crate::slices::email_drafts::service::PROVIDER_GMAIL.to_string(),
        capability: crate::slices::email_drafts::service::CAPABILITY_CREATE_DRAFT.to_string(),
        payload_json: serde_json::to_string(&payload)
            .map_err(|err| format!("serialize outbox payload: {err}"))?,
        source_entity_kind: super::store::REPORT_ENTITY_KIND.to_string(),
        source_entity_id: report.report_id.clone(),
        correlation_id: Some(report.report_id.clone()),
        causation_id: None,
        idempotency_key,
    })
}

fn string_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|raw| !raw.is_empty() && *raw != "null")
        .map(str::to_string)
}
