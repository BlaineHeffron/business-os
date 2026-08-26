//! Google Search Console cached traffic contracts. The browser reads only
//! BusinessOS snapshots; provider access is read-only and handled by the
//! search_console slice.

use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchConsoleMetricTotals {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub clicks: i64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub impressions: i64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub ctr_micros: i64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub position_micros: i64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchConsoleBreakdownRow {
    pub value: String,
    pub metrics: SearchConsoleMetricTotals,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalyticsMetricTotals {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub sessions: i64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub total_users: i64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub event_count: i64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub conversions: i64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalyticsBreakdownRow {
    pub value: String,
    pub metrics: AnalyticsMetricTotals,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchConsoleProperty {
    pub site_url: String,
    pub permission_level: String,
    #[serde(default)]
    pub selected: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovered_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_at_ms: Option<u64>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchConsoleTrafficOverview {
    pub configured: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub property_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub property_source: Option<String>,
    #[serde(default)]
    pub properties: Vec<SearchConsoleProperty>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection_revision: Option<u64>,
    pub credential_connected: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_granted: Option<bool>,
    pub in_flight: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_synced_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub next_sync_allowed_at_ms: u64,
    pub week: SearchConsoleMetricTotals,
    pub month_to_date: SearchConsoleMetricTotals,
    pub branded_week: SearchConsoleMetricTotals,
    pub nonbranded_week: SearchConsoleMetricTotals,
    pub top_queries_week: Vec<SearchConsoleBreakdownRow>,
    pub top_pages_week: Vec<SearchConsoleBreakdownRow>,
    pub analytics_configured: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analytics_property_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analytics_last_synced_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analytics_last_error: Option<String>,
    pub analytics_week: AnalyticsMetricTotals,
    pub analytics_month_to_date: AnalyticsMetricTotals,
    pub analytics_excluded_referrer_spam_week: AnalyticsMetricTotals,
    pub analytics_excluded_referrer_spam_month_to_date: AnalyticsMetricTotals,
    pub top_landing_pages_week: Vec<AnalyticsBreakdownRow>,
    pub top_sources_week: Vec<AnalyticsBreakdownRow>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchConsoleSyncNowResponse {
    pub accepted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub next_allowed_at_ms: u64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchConsolePropertySelectRequest {
    pub site_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
}
