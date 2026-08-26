use bos_contracts::search_console::{AnalyticsMetricTotals, SearchConsoleMetricTotals};
use bos_integrations::google_search_console::{
    SearchConsoleClient, SearchConsoleError, SearchConsoleRow, SearchConsoleSite,
};
use std::sync::Arc;

use super::{service, store, worker};
use crate::http::test_support::test_state;
use crate::overlay::SearchConsoleOverlay;
use crate::persistence::Persistence;

struct FailingDiscovery {
    error: SearchConsoleError,
}

impl SearchConsoleClient for FailingDiscovery {
    fn list_sites(
        &self,
        _access_token: &str,
    ) -> Result<Vec<SearchConsoleSite>, SearchConsoleError> {
        Err(self.error.clone())
    }

    fn query(
        &self,
        _access_token: &str,
        _property_url: &str,
        _start_date: &str,
        _end_date: &str,
        _dimensions: &[&str],
        _row_limit: u32,
    ) -> Result<Vec<SearchConsoleRow>, SearchConsoleError> {
        Ok(Vec::new())
    }
}

#[test]
fn branded_patterns_are_config_driven_and_wildcard_capable() {
    let patterns = service::split_patterns("Example Company, demo*, *floor coating");
    assert!(service::is_branded_query(
        "Example Company epoxy",
        &patterns
    ));
    assert!(service::is_branded_query("demo industrial", &patterns));
    assert!(service::is_branded_query("garage floor coating", &patterns));
    assert!(!service::is_branded_query(
        "epoxy contractors near me",
        &patterns
    ));
}

#[test]
fn sums_daily_and_branded_query_metrics() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let client = "test";
    let property = "sc-domain:example.com";
    store::replace_window(
        conn,
        client,
        property,
        store::SnapshotWindow {
            start_date: "2026-06-15",
            end_date: "2026-06-16",
            daily: &[
                store::DailyMetricRow {
                    date: "2026-06-15".to_string(),
                    metrics: totals(10, 100),
                },
                store::DailyMetricRow {
                    date: "2026-06-16".to_string(),
                    metrics: totals(20, 200),
                },
            ],
            dimensions: &[
                store::DimensionMetricRow {
                    date: "2026-06-15".to_string(),
                    dimension_type: "query".to_string(),
                    dimension_value: "brand".to_string(),
                    is_branded: true,
                    metrics: totals(7, 70),
                },
                store::DimensionMetricRow {
                    date: "2026-06-15".to_string(),
                    dimension_type: "query".to_string(),
                    dimension_value: "generic".to_string(),
                    is_branded: false,
                    metrics: totals(3, 30),
                },
            ],
        },
        1,
    )
    .expect("replace");
    let total = store::sum_daily(conn, client, property, "2026-06-15", "2026-06-16").expect("sum");
    assert_eq!(total.clicks, 30);
    assert_eq!(total.impressions, 300);
    let branded = store::sum_dimension(
        conn,
        client,
        property,
        "query",
        Some(true),
        "2026-06-15",
        "2026-06-16",
    )
    .expect("branded");
    assert_eq!(branded.clicks, 7);
}

#[test]
fn sums_analytics_daily_and_top_dimensions() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let client = "test";
    let property = "123456789";
    store::replace_analytics_window(
        conn,
        client,
        property,
        store::AnalyticsSnapshotWindow {
            start_date: "2026-06-15",
            end_date: "2026-06-16",
            daily: &[
                store::AnalyticsDailyMetricRow {
                    date: "2026-06-15".to_string(),
                    metrics: analytics_totals(10, 8, 50, 1),
                },
                store::AnalyticsDailyMetricRow {
                    date: "2026-06-16".to_string(),
                    metrics: analytics_totals(20, 15, 90, 2),
                },
            ],
            dimensions: &[
                store::AnalyticsDimensionMetricRow {
                    date: "2026-06-15".to_string(),
                    dimension_type: "landing_page".to_string(),
                    dimension_value: "/".to_string(),
                    metrics: analytics_totals(7, 6, 30, 1),
                },
                store::AnalyticsDimensionMetricRow {
                    date: "2026-06-15".to_string(),
                    dimension_type: "landing_page".to_string(),
                    dimension_value: "/products".to_string(),
                    metrics: analytics_totals(3, 2, 20, 0),
                },
            ],
        },
        1,
    )
    .expect("replace analytics");
    let total = store::sum_analytics_daily(conn, client, property, "2026-06-15", "2026-06-16")
        .expect("analytics sum");
    assert_eq!(total.sessions, 30);
    assert_eq!(total.total_users, 23);
    assert_eq!(total.conversions, 3);
    let top = store::top_analytics_dimensions(
        conn,
        client,
        property,
        "landing_page",
        "2026-06-15",
        "2026-06-16",
        2,
    )
    .expect("top landing pages");
    assert_eq!(top[0].value, "/");
    assert_eq!(top[0].metrics.sessions, 7);

    store::replace_analytics_window(
        conn,
        client,
        property,
        store::AnalyticsSnapshotWindow {
            start_date: "2026-06-15",
            end_date: "2026-06-16",
            daily: &[store::AnalyticsDailyMetricRow {
                date: "2026-06-15".to_string(),
                metrics: analytics_totals(99, 88, 77, 6),
            }],
            dimensions: &[store::AnalyticsDimensionMetricRow {
                date: "2026-06-15".to_string(),
                dimension_type: "landing_page".to_string(),
                dimension_value: "/".to_string(),
                metrics: analytics_totals(99, 88, 77, 6),
            }],
        },
        2,
    )
    .expect("replace changed analytics with same row counts");
    let changed = store::sum_analytics_daily(conn, client, property, "2026-06-15", "2026-06-16")
        .expect("changed analytics sum");
    assert_eq!(
        changed.sessions, 99,
        "same-shape GA4 refreshes must not replay stale snapshots"
    );
}

#[test]
fn discovered_properties_are_cached_and_selectable_with_revision() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let sites = vec![
        SearchConsoleSite {
            site_url: "sc-domain:example.com".to_string(),
            permission_level: "siteOwner".to_string(),
        },
        SearchConsoleSite {
            site_url: "https://www.example.com/".to_string(),
            permission_level: "siteFullUser".to_string(),
        },
    ];
    store::replace_discovered_properties(conn, "test", &sites, 1).expect("cache properties");

    let properties = store::list_properties(conn, "test").expect("properties");
    assert_eq!(properties.len(), 2);
    assert!(properties.iter().all(|property| !property.selected));

    let outcome = store::select_property(
        conn,
        store::PropertySelectionContext {
            client_id: "test",
            actor_id: "operator",
            expected_revision: None,
            idempotency_key: "select-1",
            now_ms: 2,
        },
        "sc-domain:example.com",
    )
    .expect("select");
    assert!(matches!(
        outcome,
        crate::store_core::MutationOutcome::Applied { revision: 1, .. }
    ));
    let selected = store::selected_property(conn, "test")
        .expect("selected")
        .expect("selected row");
    assert_eq!(selected.site_url, "sc-domain:example.com");
    assert_eq!(selected.revision, Some(1));
    let properties = store::list_properties(conn, "test").expect("properties");
    assert!(properties
        .iter()
        .any(|property| property.site_url == "sc-domain:example.com" && property.selected));
}

#[test]
fn selection_rejects_undiscovered_property() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let err = store::select_property(
        persistence.connection(),
        store::PropertySelectionContext {
            client_id: "test",
            actor_id: "operator",
            expected_revision: None,
            idempotency_key: "select-missing",
            now_ms: 1,
        },
        "sc-domain:missing.example",
    )
    .expect_err("missing property rejected");
    assert!(matches!(
        err,
        crate::store_core::StoreError::Domain(code)
            if code == "search_console_property_not_discovered"
    ));
}

#[test]
fn effective_property_keeps_config_override_before_selection_and_discovery() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    store::replace_discovered_properties(
        conn,
        "test",
        &[SearchConsoleSite {
            site_url: "sc-domain:discovered.example".to_string(),
            permission_level: "siteOwner".to_string(),
        }],
        1,
    )
    .expect("cache");
    store::select_property(
        conn,
        store::PropertySelectionContext {
            client_id: "test",
            actor_id: "operator",
            expected_revision: None,
            idempotency_key: "select",
            now_ms: 2,
        },
        "sc-domain:discovered.example",
    )
    .expect("select");

    let config = service::config(Some(&SearchConsoleOverlay {
        property_url: "sc-domain:configured.example".to_string(),
        branded_query_patterns: Vec::new(),
        user_id: String::new(),
        sync_days: None,
        ga4_property_id: String::new(),
        analytics_excluded_referrer_domains: Vec::new(),
    }));
    let effective = service::effective_property(conn, "test", &config)
        .expect("effective")
        .expect("property");
    assert_eq!(effective.property_url, "sc-domain:configured.example");
    assert_eq!(effective.source, "config");
}

#[test]
fn effective_property_uses_selected_then_single_discovered_property() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let empty_config = service::SearchConsoleConfig {
        property_url: None,
        branded_query_patterns: Vec::new(),
        user_id: None,
        sync_days: 90,
        ga4_property_id: None,
        analytics_excluded_referrer_domains: Vec::new(),
    };
    store::replace_discovered_properties(
        conn,
        "test",
        &[SearchConsoleSite {
            site_url: "sc-domain:only.example".to_string(),
            permission_level: "siteOwner".to_string(),
        }],
        1,
    )
    .expect("cache");
    let effective = service::effective_property(conn, "test", &empty_config)
        .expect("effective")
        .expect("single discovered");
    assert_eq!(effective.property_url, "sc-domain:only.example");
    assert_eq!(effective.source, "single_discovered");

    store::replace_discovered_properties(
        conn,
        "test",
        &[SearchConsoleSite {
            site_url: "https://www.example.com/".to_string(),
            permission_level: "siteFullUser".to_string(),
        }],
        2,
    )
    .expect("cache second");
    assert!(
        service::effective_property(conn, "test", &empty_config)
            .expect("effective")
            .is_none(),
        "multiple discovered properties require explicit selection"
    );

    store::select_property(
        conn,
        store::PropertySelectionContext {
            client_id: "test",
            actor_id: "operator",
            expected_revision: None,
            idempotency_key: "select",
            now_ms: 3,
        },
        "https://www.example.com/",
    )
    .expect("select");
    let effective = service::effective_property(conn, "test", &empty_config)
        .expect("effective")
        .expect("selected");
    assert_eq!(effective.property_url, "https://www.example.com/");
    assert_eq!(effective.source, "selected");
}

#[test]
fn analytics_sync_requires_full_three_request_budget_after_discovery() {
    assert!(worker::ensure_analytics_budget(2).is_err());
    assert!(worker::ensure_analytics_budget(3).is_ok());
}

#[test]
fn reporting_end_uses_latest_finalized_sync_date() {
    assert_eq!(
        service::reporting_end_date("2026-07-06", Some("2026-07-05")),
        "2026-07-05"
    );
    assert_eq!(
        service::reporting_end_date("2026-07-06", Some("2026-07-07")),
        "2026-07-06"
    );
    assert_eq!(
        service::reporting_end_date("2026-07-06", None),
        "2026-07-06"
    );
}

#[test]
fn analytics_overview_excludes_referrer_spam_from_reporting_totals() {
    let mut state = test_state();
    state.search_console_overlay = Arc::new(Some(SearchConsoleOverlay {
        ga4_property_id: "123456789".to_string(),
        analytics_excluded_referrer_domains: vec!["trafficheap.cc".to_string()],
        ..SearchConsoleOverlay::default()
    }));
    {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        store::replace_analytics_window(
            conn,
            &state.client_id,
            "123456789",
            store::AnalyticsSnapshotWindow {
                start_date: "2026-06-29",
                end_date: "2026-07-05",
                daily: &[store::AnalyticsDailyMetricRow {
                    date: "2026-07-05".to_string(),
                    metrics: analytics_totals(15, 12, 90, 3),
                }],
                dimensions: &[
                    store::AnalyticsDimensionMetricRow {
                        date: "2026-07-05".to_string(),
                        dimension_type: "source_medium".to_string(),
                        dimension_value: "google / organic".to_string(),
                        metrics: analytics_totals(10, 8, 50, 2),
                    },
                    store::AnalyticsDimensionMetricRow {
                        date: "2026-07-05".to_string(),
                        dimension_type: "source_medium".to_string(),
                        dimension_value: "trafficheap.cc / referral".to_string(),
                        metrics: analytics_totals(5, 4, 40, 1),
                    },
                ],
            },
            1,
        )
        .expect("replace analytics");
        let mut cursor =
            store::get_analytics_cursor(&*conn, &state.client_id, "123456789").expect("cursor");
        cursor.synced_end_date = Some("2026-07-05".to_string());
        cursor.last_synced_at_ms = Some(1);
        store::put_analytics_cursor(conn, &state.client_id, "123456789", &cursor, 1)
            .expect("put cursor");
    }

    let persistence = state.persistence.lock();
    let sync_guard = state
        .sync_guards
        .guard(crate::http::Pump::SearchConsole)
        .lock()
        .clone();
    let overview = service::overview(
        &state,
        persistence.connection_ref(),
        &sync_guard,
        "2026-07-06",
    )
    .expect("overview");

    assert_eq!(overview.analytics_week.sessions, 10);
    assert_eq!(overview.analytics_week.conversions, 2);
    assert_eq!(overview.analytics_excluded_referrer_spam_week.sessions, 5);
    assert_eq!(
        overview
            .top_sources_week
            .iter()
            .map(|row| row.value.as_str())
            .collect::<Vec<_>>(),
        vec!["google / organic"]
    );

    let raw = store::sum_analytics_daily(
        persistence.connection_ref(),
        &state.client_id,
        "123456789",
        "2026-07-05",
        "2026-07-05",
    )
    .expect("raw sum");
    assert_eq!(raw.sessions, 15, "raw GA4 cache stays unchanged");
}

#[test]
fn finish_guarded_cycle_releases_shared_guard_on_error() {
    let state = test_state();
    assert!(worker::try_begin_sync(&state, 1_000).is_ok());

    let result: Result<worker::CycleSummary, String> =
        Err("spawn_failed: thread unavailable".into());
    worker::finish_guarded_cycle(&state, &result);

    let status = state
        .sync_guards
        .guard(crate::http::Pump::SearchConsole)
        .lock()
        .clone();
    assert!(!status.in_flight);
    assert_eq!(
        status.last_outcome.as_deref(),
        Some("error: spawn_failed: thread unavailable")
    );
}

#[test]
fn discovery_rate_limit_is_persisted_to_cursor() {
    let state = test_state();
    let cursor = store::SearchConsoleCursor::default();
    let err = worker::discover_properties(
        &state,
        &FailingDiscovery {
            error: SearchConsoleError::RateLimited {
                retry_after_ms: Some(1_000),
                message: "search console returned 429".to_string(),
            },
        },
        "token",
        store::DISCOVERY_CURSOR_KEY,
        cursor,
        10,
    )
    .expect_err("rate limit recorded");
    assert_eq!(err, "search console returned 429");

    let persistence = state.persistence.lock();
    let cursor = store::get_cursor(
        persistence.connection_ref(),
        &state.client_id,
        store::DISCOVERY_CURSOR_KEY,
    )
    .expect("cursor");
    assert_eq!(
        cursor.last_error.as_deref(),
        Some("search console returned 429")
    );
    assert_eq!(cursor.rate_limited_until_ms, 1_010);
}

#[test]
fn discovery_auth_error_is_persisted_to_cursor() {
    let state = test_state();
    let cursor = store::SearchConsoleCursor::default();
    let err = worker::discover_properties(
        &state,
        &FailingDiscovery {
            error: SearchConsoleError::AuthRejected {
                message: "search console returned 403".to_string(),
            },
        },
        "token",
        store::DISCOVERY_CURSOR_KEY,
        cursor,
        10,
    )
    .expect_err("auth error recorded");
    assert_eq!(err, "auth_rejected: search console returned 403");

    let persistence = state.persistence.lock();
    let cursor = store::get_cursor(
        persistence.connection_ref(),
        &state.client_id,
        store::DISCOVERY_CURSOR_KEY,
    )
    .expect("cursor");
    assert_eq!(
        cursor.last_error.as_deref(),
        Some("auth_rejected: search console returned 403")
    );
}

fn totals(clicks: i64, impressions: i64) -> SearchConsoleMetricTotals {
    SearchConsoleMetricTotals {
        clicks,
        impressions,
        ctr_micros: 100_000,
        position_micros: 1_500_000,
    }
}

fn analytics_totals(
    sessions: i64,
    total_users: i64,
    event_count: i64,
    conversions: i64,
) -> AnalyticsMetricTotals {
    AnalyticsMetricTotals {
        sessions,
        total_users,
        event_count,
        conversions,
    }
}
