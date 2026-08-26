//! Google Search Console read integration. The slice syncs read-only Search
//! Analytics data into local snapshots and serves owner-facing traffic views
//! from sqlite only. Property id, brand-query rules, user binding, and metric
//! preferences are runtime config (overlay/env), never Demo hardcoding.

pub mod routes;
pub mod service;
pub mod store;
pub mod worker;

#[cfg(test)]
mod tests;

use crate::env_registry;
use crate::slices::{RouteSpec, SliceSpec};

pub const SLICE: SliceSpec = SliceSpec {
    id: "search_console",
    title: "Search Console traffic",
    summary: "Read-only Google Search Console and GA4 sync for the configured properties. Stores local traffic snapshots and serves status, sync-now, and cached traffic overview with branded/non-branded, top query, top landing-page, and source cuts.",
    routes: &[
        RouteSpec {
            method: "GET",
            path: "/api/search-console/status",
            summary: "Configured property, credential/scope state, sync freshness, and cached weekly/MTD traffic",
        },
        RouteSpec {
            method: "POST",
            path: "/api/search-console/sync",
            summary: "Kick one budgeted Search Console discovery/sync cycle (202; 409 while syncing/cooling down)",
        },
        RouteSpec {
            method: "POST",
            path: "/api/google-analytics/sync",
            summary: "Kick one budgeted GA4 sync cycle (202; 409 while syncing/cooling down/unconfigured)",
        },
        RouteSpec {
            method: "POST",
            path: "/api/search-console/property",
            summary: "Select one discovered Search Console property for cached reporting when no env/overlay property overrides it",
        },
    ],
    tables: &[
        "search_console_sync_cursors",
        "search_console_properties",
        "search_console_property_selection",
        "search_console_daily_metrics",
        "search_console_dimension_metrics",
        "google_analytics_sync_cursors",
        "google_analytics_daily_metrics",
        "google_analytics_dimension_metrics",
    ],
    env_vars: &[
        &env_registry::BOS_SEARCH_CONSOLE_ANALYTICS_EXCLUDED_REFERRER_DOMAINS,
        &env_registry::BOS_SEARCH_CONSOLE_BRANDED_QUERY_PATTERNS,
        &env_registry::BOS_SEARCH_CONSOLE_GA4_PROPERTY_ID,
        &env_registry::BOS_SEARCH_CONSOLE_MAX_REQUESTS_PER_CYCLE,
        &env_registry::BOS_SEARCH_CONSOLE_PROPERTY_URL,
        &env_registry::BOS_SEARCH_CONSOLE_SYNC_DAYS,
        &env_registry::BOS_SEARCH_CONSOLE_SYNC_ENABLED,
        &env_registry::BOS_SEARCH_CONSOLE_SYNC_INTERVAL_SECS,
        &env_registry::BOS_SEARCH_CONSOLE_USER_ID,
    ],
    read_models: &["search_console_status"],
};
