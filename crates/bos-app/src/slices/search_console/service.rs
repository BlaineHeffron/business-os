use bos_contracts::search_console::{
    AnalyticsBreakdownRow, AnalyticsMetricTotals, SearchConsoleMetricTotals, SearchConsoleProperty,
    SearchConsoleTrafficOverview,
};
use bos_integrations::google_oauth;
use bos_integrations::google_search_console::GOOGLE_SEARCH_CONSOLE_READONLY_SCOPE;
use rusqlite::Connection;
use sha2::{Digest, Sha256};

use super::store;
use crate::env_registry;
use crate::http::{AppState, SyncGuard};
use crate::overlay::SearchConsoleOverlay;
use crate::store_core::StoreError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchConsoleConfig {
    pub property_url: Option<String>,
    pub branded_query_patterns: Vec<String>,
    pub user_id: Option<String>,
    pub sync_days: u32,
    pub ga4_property_id: Option<String>,
    pub analytics_excluded_referrer_domains: Vec<String>,
}

impl SearchConsoleConfig {
    pub fn configured(&self) -> bool {
        self.property_url
            .as_deref()
            .is_some_and(|property| !property.trim().is_empty())
    }

    pub fn property_url(&self) -> Option<&str> {
        self.property_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    pub fn analytics_configured(&self) -> bool {
        self.ga4_property_id
            .as_deref()
            .is_some_and(|property| !property.trim().is_empty())
    }

    pub fn any_source_configured(&self) -> bool {
        self.configured() || self.analytics_configured()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveProperty {
    pub property_url: String,
    pub source: &'static str,
    pub selection_revision: Option<u64>,
}

pub fn config(overlay: Option<&SearchConsoleOverlay>) -> SearchConsoleConfig {
    let property_url = env_registry::string(&env_registry::BOS_SEARCH_CONSOLE_PROPERTY_URL)
        .or_else(|| overlay.and_then(|o| non_empty(o.property_url.clone())));
    let branded_query_patterns =
        env_registry::string(&env_registry::BOS_SEARCH_CONSOLE_BRANDED_QUERY_PATTERNS)
            .map(|raw| split_patterns(&raw))
            .unwrap_or_else(|| {
                overlay
                    .map(|o| o.branded_query_patterns.clone())
                    .unwrap_or_default()
            });
    let user_id = env_registry::string(&env_registry::BOS_SEARCH_CONSOLE_USER_ID)
        .or_else(|| overlay.and_then(|o| non_empty(o.user_id.clone())));
    let ga4_property_id = env_registry::string(&env_registry::BOS_SEARCH_CONSOLE_GA4_PROPERTY_ID)
        .or_else(|| overlay.and_then(|o| non_empty(o.ga4_property_id.clone())));
    let analytics_excluded_referrer_domains =
        env_registry::string(&env_registry::BOS_SEARCH_CONSOLE_ANALYTICS_EXCLUDED_REFERRER_DOMAINS)
            .map(|raw| split_list(&raw))
            .unwrap_or_else(|| {
                overlay
                    .map(|o| normalize_referrer_domains(&o.analytics_excluded_referrer_domains))
                    .unwrap_or_default()
            });
    let sync_days = env_registry::string(&env_registry::BOS_SEARCH_CONSOLE_SYNC_DAYS)
        .and_then(|raw| raw.trim().parse::<u32>().ok())
        .unwrap_or_else(|| overlay.and_then(|o| o.sync_days).unwrap_or(90))
        .clamp(7, 180);
    SearchConsoleConfig {
        property_url,
        branded_query_patterns,
        user_id,
        sync_days,
        ga4_property_id,
        analytics_excluded_referrer_domains,
    }
}

fn non_empty(value: String) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

pub fn split_patterns(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|pattern| !pattern.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

fn split_list(raw: &str) -> Vec<String> {
    normalize_referrer_domains(
        &raw.split([',', ';', '\n', '\t', ' '])
            .map(str::to_string)
            .collect::<Vec<_>>(),
    )
}

fn normalize_referrer_domains(values: &[String]) -> Vec<String> {
    values
        .iter()
        .filter_map(|value| normalize_referrer_domain(value))
        .collect()
}

fn normalize_referrer_domain(value: &str) -> Option<String> {
    let trimmed = value.trim().to_ascii_lowercase();
    let without_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(&trimmed);
    let host = without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .trim_matches('.');
    (!host.is_empty()).then(|| host.to_string())
}

pub fn is_branded_query(query: &str, patterns: &[String]) -> bool {
    let query = query.to_ascii_lowercase();
    patterns
        .iter()
        .any(|pattern| wildcard_match(pattern, &query))
}

fn wildcard_match(pattern: &str, value: &str) -> bool {
    let segments: Vec<&str> = pattern.split('*').collect();
    if segments.len() == 1 {
        return value.contains(pattern);
    }
    let mut pos = 0usize;
    for (index, segment) in segments.iter().enumerate() {
        if segment.is_empty() {
            continue;
        }
        let Some(found) = value[pos..].find(segment) else {
            return false;
        };
        if index == 0 && found != 0 {
            return false;
        }
        pos += found + segment.len();
    }
    segments
        .last()
        .is_none_or(|last| last.is_empty() || value.ends_with(last))
}

pub fn config_hash(config: &SearchConsoleConfig) -> String {
    let mut hasher = Sha256::new();
    hasher.update(config.property_url.as_deref().unwrap_or("").as_bytes());
    hasher.update([0]);
    hasher.update(config.ga4_property_id.as_deref().unwrap_or("").as_bytes());
    hasher.update([0]);
    for pattern in &config.branded_query_patterns {
        hasher.update(pattern.as_bytes());
        hasher.update([0]);
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(16);
    for byte in &digest[..8] {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

pub fn effective_property(
    conn: &Connection,
    client_id: &str,
    config: &SearchConsoleConfig,
) -> Result<Option<EffectiveProperty>, StoreError> {
    if let Some(property_url) = config.property_url() {
        return Ok(Some(EffectiveProperty {
            property_url: property_url.to_string(),
            source: "config",
            selection_revision: None,
        }));
    }
    if let Some(selected) = store::selected_property(conn, client_id)? {
        return Ok(Some(EffectiveProperty {
            property_url: selected.site_url,
            source: "selected",
            selection_revision: selected.revision,
        }));
    }
    let properties = store::list_properties(conn, client_id)?;
    if properties.len() == 1 {
        return Ok(Some(EffectiveProperty {
            property_url: properties[0].site_url.clone(),
            source: "single_discovered",
            selection_revision: store::selection_revision(conn, client_id)?,
        }));
    }
    Ok(None)
}

pub fn today_utc() -> String {
    let days = crate::http::now_ms() as i64 / 86_400_000;
    format_days(days)
}

pub fn days_before(date: &str, days: i64) -> Option<String> {
    parse_date_days(date).map(|d| format_days(d - days))
}

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

pub fn sync_window(today: &str, sync_days: u32) -> Option<(String, String)> {
    let today_days = parse_date_days(today)?;
    // Search Console finalized data usually lags; end yesterday to avoid
    // partial current-day rows.
    let end = today_days - 1;
    let start = end - i64::from(sync_days.saturating_sub(1));
    Some((format_days(start), format_days(end)))
}

pub(crate) fn reporting_end_date(today: &str, synced_end_date: Option<&str>) -> String {
    let Some(today_days) = parse_date_days(today) else {
        return today.to_string();
    };
    let Some(synced_end_days) = synced_end_date.and_then(parse_date_days) else {
        return today.to_string();
    };
    format_days(synced_end_days.min(today_days))
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = year - (month <= 2) as i64;
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let month = month as i64;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let doe = days - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096).div_euclid(365);
    let year = yoe + era * 400;
    let day_of_year = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let month_shift = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_shift + 2) / 5 + 1) as u32;
    let month = if month_shift < 10 {
        month_shift + 3
    } else {
        month_shift - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

fn format_days(days: i64) -> String {
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

pub fn overview(
    state: &AppState,
    conn: &Connection,
    sync_status: &SyncGuard,
    today: &str,
) -> Result<SearchConsoleTrafficOverview, StoreError> {
    let config = config(state.search_console_overlay.as_ref().as_ref());
    let properties = store::list_properties(conn, &state.client_id)?;
    let oauth = crate::slices::google_connector::service::resolve_google_oauth(
        conn,
        &state.client_id,
        config.user_id.as_deref(),
    )?;
    let scope_granted = oauth.as_ref().map(|oauth| {
        oauth.scopes.is_empty()
            || google_oauth::has_scope(oauth, GOOGLE_SEARCH_CONSOLE_READONLY_SCOPE)
    });
    let analytics = analytics_overview(conn, &state.client_id, &config, today)?;
    let Some(effective) = effective_property(conn, &state.client_id, &config)? else {
        let discovery_cursor =
            store::get_cursor(conn, &state.client_id, store::DISCOVERY_CURSOR_KEY)?;
        let mut overview = empty_overview(
            config,
            properties,
            store::selection_revision(conn, &state.client_id)?,
            sync_status,
            Some(oauth.is_some()),
            scope_granted,
            discovery_cursor.last_error,
        );
        apply_analytics_overview(&mut overview, analytics);
        return Ok(overview);
    };
    let property_url = effective.property_url.as_str();
    let cursor = store::get_cursor(conn, &state.client_id, property_url)?;
    let reporting_end = reporting_end_date(today, cursor.synced_end_date.as_deref());
    let week_start = crate::slices::accounting::service::week_start_date(&reporting_end)
        .unwrap_or_else(|| reporting_end.clone());
    let month_start = crate::slices::accounting::service::month_start_date(&reporting_end)
        .unwrap_or_else(|| reporting_end.clone());
    let week = store::sum_daily(
        conn,
        &state.client_id,
        property_url,
        &week_start,
        &reporting_end,
    )?;
    let month_to_date = store::sum_daily(
        conn,
        &state.client_id,
        property_url,
        &month_start,
        &reporting_end,
    )?;
    let branded_week = store::sum_dimension(
        conn,
        &state.client_id,
        property_url,
        "query",
        Some(true),
        &week_start,
        &reporting_end,
    )?;
    let nonbranded_week = store::sum_dimension(
        conn,
        &state.client_id,
        property_url,
        "query",
        Some(false),
        &week_start,
        &reporting_end,
    )?;
    let mut overview = SearchConsoleTrafficOverview {
        configured: true,
        property_url: Some(property_url.to_string()),
        property_source: Some(effective.source.to_string()),
        properties,
        selection_revision: effective.selection_revision,
        credential_connected: oauth.is_some(),
        scope_granted,
        in_flight: sync_status.in_flight,
        last_synced_at_ms: cursor.last_synced_at_ms,
        last_error: cursor.last_error,
        next_sync_allowed_at_ms: sync_status.next_allowed_at_ms,
        week,
        month_to_date,
        branded_week,
        nonbranded_week,
        top_queries_week: store::top_dimensions(
            conn,
            &state.client_id,
            property_url,
            "query",
            &week_start,
            &reporting_end,
            5,
        )?,
        top_pages_week: store::top_dimensions(
            conn,
            &state.client_id,
            property_url,
            "page",
            &week_start,
            &reporting_end,
            5,
        )?,
        analytics_configured: false,
        analytics_property_id: None,
        analytics_last_synced_at_ms: None,
        analytics_last_error: None,
        analytics_week: AnalyticsMetricTotals::default(),
        analytics_month_to_date: AnalyticsMetricTotals::default(),
        analytics_excluded_referrer_spam_week: AnalyticsMetricTotals::default(),
        analytics_excluded_referrer_spam_month_to_date: AnalyticsMetricTotals::default(),
        top_landing_pages_week: Vec::new(),
        top_sources_week: Vec::new(),
    };
    apply_analytics_overview(&mut overview, analytics);
    Ok(overview)
}

struct AnalyticsOverview {
    configured: bool,
    property_id: Option<String>,
    last_synced_at_ms: Option<u64>,
    last_error: Option<String>,
    week: AnalyticsMetricTotals,
    month_to_date: AnalyticsMetricTotals,
    excluded_referrer_spam_week: AnalyticsMetricTotals,
    excluded_referrer_spam_month_to_date: AnalyticsMetricTotals,
    top_landing_pages_week: Vec<AnalyticsBreakdownRow>,
    top_sources_week: Vec<AnalyticsBreakdownRow>,
}

fn apply_analytics_overview(
    overview: &mut SearchConsoleTrafficOverview,
    analytics: AnalyticsOverview,
) {
    overview.analytics_configured = analytics.configured;
    overview.analytics_property_id = analytics.property_id;
    overview.analytics_last_synced_at_ms = analytics.last_synced_at_ms;
    overview.analytics_last_error = analytics.last_error;
    overview.analytics_week = analytics.week;
    overview.analytics_month_to_date = analytics.month_to_date;
    overview.analytics_excluded_referrer_spam_week = analytics.excluded_referrer_spam_week;
    overview.analytics_excluded_referrer_spam_month_to_date =
        analytics.excluded_referrer_spam_month_to_date;
    overview.top_landing_pages_week = analytics.top_landing_pages_week;
    overview.top_sources_week = analytics.top_sources_week;
}

fn analytics_overview(
    conn: &Connection,
    client_id: &str,
    config: &SearchConsoleConfig,
    today: &str,
) -> Result<AnalyticsOverview, StoreError> {
    let Some(property_id) = config
        .ga4_property_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(AnalyticsOverview {
            configured: false,
            property_id: config.ga4_property_id.clone(),
            last_synced_at_ms: None,
            last_error: None,
            week: AnalyticsMetricTotals::default(),
            month_to_date: AnalyticsMetricTotals::default(),
            excluded_referrer_spam_week: AnalyticsMetricTotals::default(),
            excluded_referrer_spam_month_to_date: AnalyticsMetricTotals::default(),
            top_landing_pages_week: Vec::new(),
            top_sources_week: Vec::new(),
        });
    };
    let cursor = store::get_analytics_cursor(conn, client_id, property_id)?;
    let reporting_end = reporting_end_date(today, cursor.synced_end_date.as_deref());
    let week_start = crate::slices::accounting::service::week_start_date(&reporting_end)
        .unwrap_or_else(|| reporting_end.clone());
    let month_start = crate::slices::accounting::service::month_start_date(&reporting_end)
        .unwrap_or_else(|| reporting_end.clone());
    let week_metrics = analytics_reporting_metrics(
        conn,
        client_id,
        property_id,
        &week_start,
        &reporting_end,
        config,
    )?;
    let month_metrics = analytics_reporting_metrics(
        conn,
        client_id,
        property_id,
        &month_start,
        &reporting_end,
        config,
    )?;
    Ok(AnalyticsOverview {
        configured: true,
        property_id: Some(property_id.to_string()),
        last_synced_at_ms: cursor.last_synced_at_ms,
        last_error: cursor.last_error,
        week: week_metrics.included,
        month_to_date: month_metrics.included,
        excluded_referrer_spam_week: week_metrics.excluded_referrer_spam,
        excluded_referrer_spam_month_to_date: month_metrics.excluded_referrer_spam,
        top_landing_pages_week: store::top_analytics_dimensions(
            conn,
            client_id,
            property_id,
            "landing_page",
            &week_start,
            &reporting_end,
            5,
        )?,
        top_sources_week: week_metrics.top_sources,
    })
}

#[derive(Debug, Default)]
struct SourceFilterResult {
    source_rows: usize,
    included: AnalyticsMetricTotals,
    excluded: AnalyticsMetricTotals,
    top_sources: Vec<AnalyticsBreakdownRow>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AnalyticsReportingMetrics {
    pub(crate) included: AnalyticsMetricTotals,
    pub(crate) excluded_referrer_spam: AnalyticsMetricTotals,
    pub(crate) top_sources: Vec<AnalyticsBreakdownRow>,
}

pub(crate) fn analytics_reporting_metrics(
    conn: &Connection,
    client_id: &str,
    property_id: &str,
    start_date: &str,
    end_date: &str,
    config: &SearchConsoleConfig,
) -> Result<AnalyticsReportingMetrics, StoreError> {
    let spam_domains = referrer_spam_domains(config);
    let sources = store::analytics_source_rows(conn, client_id, property_id, start_date, end_date)?;
    let filter = filter_source_rows(sources, &spam_domains);
    let daily = store::sum_analytics_daily(conn, client_id, property_id, start_date, end_date)?;
    Ok(AnalyticsReportingMetrics {
        included: if filter.source_rows == 0 {
            daily
        } else {
            filter.included
        },
        excluded_referrer_spam: filter.excluded,
        top_sources: filter.top_sources,
    })
}

fn referrer_spam_domains(config: &SearchConsoleConfig) -> Vec<String> {
    let mut domains = REFERRER_SPAM_DOMAINS
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .filter_map(normalize_referrer_domain)
        .collect::<Vec<_>>();
    domains.extend(config.analytics_excluded_referrer_domains.iter().cloned());
    domains.sort();
    domains.dedup();
    domains
}

fn filter_source_rows(
    mut rows: Vec<AnalyticsBreakdownRow>,
    spam_domains: &[String],
) -> SourceFilterResult {
    let mut result = SourceFilterResult {
        source_rows: rows.len(),
        ..SourceFilterResult::default()
    };
    rows.retain(|row| {
        if source_is_referrer_spam(&row.value, spam_domains) {
            add_analytics_totals(&mut result.excluded, &row.metrics);
            false
        } else {
            add_analytics_totals(&mut result.included, &row.metrics);
            true
        }
    });
    rows.truncate(5);
    result.top_sources = rows;
    result
}

fn source_is_referrer_spam(source_medium: &str, spam_domains: &[String]) -> bool {
    let source = source_medium
        .split_once(" / ")
        .map(|(source, _)| source)
        .unwrap_or(source_medium);
    let Some(source_domain) = normalize_referrer_domain(source) else {
        return false;
    };
    spam_domains
        .iter()
        .any(|domain| source_domain == *domain || source_domain.ends_with(&format!(".{domain}")))
}

fn add_analytics_totals(total: &mut AnalyticsMetricTotals, add: &AnalyticsMetricTotals) {
    total.sessions += add.sessions;
    total.total_users += add.total_users;
    total.event_count += add.event_count;
    total.conversions += add.conversions;
}

const REFERRER_SPAM_DOMAINS: &str = include_str!("../../../../../data/referrer-spam-domains.txt");

pub fn empty_overview(
    config: SearchConsoleConfig,
    properties: Vec<SearchConsoleProperty>,
    selection_revision: Option<u64>,
    sync_status: &SyncGuard,
    credential_connected: Option<bool>,
    scope_granted: Option<bool>,
    last_error: Option<String>,
) -> SearchConsoleTrafficOverview {
    SearchConsoleTrafficOverview {
        configured: config.configured(),
        property_url: config.property_url,
        property_source: None,
        properties,
        selection_revision,
        credential_connected: credential_connected.unwrap_or(false),
        scope_granted,
        in_flight: sync_status.in_flight,
        last_synced_at_ms: None,
        last_error,
        next_sync_allowed_at_ms: sync_status.next_allowed_at_ms,
        week: SearchConsoleMetricTotals::default(),
        month_to_date: SearchConsoleMetricTotals::default(),
        branded_week: SearchConsoleMetricTotals::default(),
        nonbranded_week: SearchConsoleMetricTotals::default(),
        top_queries_week: Vec::new(),
        top_pages_week: Vec::new(),
        analytics_configured: config
            .ga4_property_id
            .as_deref()
            .is_some_and(|property| !property.trim().is_empty()),
        analytics_property_id: config.ga4_property_id,
        analytics_last_synced_at_ms: None,
        analytics_last_error: None,
        analytics_week: AnalyticsMetricTotals::default(),
        analytics_month_to_date: AnalyticsMetricTotals::default(),
        analytics_excluded_referrer_spam_week: AnalyticsMetricTotals::default(),
        analytics_excluded_referrer_spam_month_to_date: AnalyticsMetricTotals::default(),
        top_landing_pages_week: Vec::new(),
        top_sources_week: Vec::new(),
    }
}
