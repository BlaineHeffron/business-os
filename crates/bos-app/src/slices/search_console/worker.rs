use std::time::Duration;

use bos_contracts::search_console::{AnalyticsMetricTotals, SearchConsoleMetricTotals};
use bos_integrations::google_analytics_data::{
    AnalyticsDataClient, AnalyticsDataError, AnalyticsMetrics, LiveAnalyticsDataClient,
    GOOGLE_ANALYTICS_READONLY_SCOPE,
};
use bos_integrations::google_oauth;
use bos_integrations::google_search_console::{
    LiveSearchConsoleClient, SearchConsoleClient, SearchConsoleError, SearchConsoleMetrics,
    SearchConsoleSite, GOOGLE_SEARCH_CONSOLE_READONLY_SCOPE,
};

use super::{service, store};
use crate::env_registry;
use crate::http::{now_ms, AppState};
use crate::store_core::StoreError;

pub const SEARCH_CONSOLE_SYNC_COOLDOWN_MS: u64 = 120_000;
const ANALYTICS_REQUESTS_PER_CYCLE: u32 = 3;

pub struct SearchConsolePumpConfig {
    pub enabled: bool,
    pub interval: Duration,
    pub max_requests_per_cycle: u32,
}

pub fn config_from_settings(
    conn: &rusqlite::Connection,
    client_id: &str,
) -> Result<SearchConsolePumpConfig, StoreError> {
    Ok(SearchConsolePumpConfig {
        enabled: crate::slices::admin_settings::service::flag(
            conn,
            client_id,
            &env_registry::BOS_SEARCH_CONSOLE_SYNC_ENABLED,
        )?,
        interval: Duration::from_secs(
            crate::slices::admin_settings::service::usize_or(
                conn,
                client_id,
                &env_registry::BOS_SEARCH_CONSOLE_SYNC_INTERVAL_SECS,
                1800,
            )?
            .max(300) as u64,
        ),
        max_requests_per_cycle: max_requests_from_settings(conn, client_id)?,
    })
}

pub fn max_requests_from_settings(
    conn: &rusqlite::Connection,
    client_id: &str,
) -> Result<u32, StoreError> {
    Ok(crate::slices::admin_settings::service::usize_or(
        conn,
        client_id,
        &env_registry::BOS_SEARCH_CONSOLE_MAX_REQUESTS_PER_CYCLE,
        8,
    )?
    .clamp(5, 20) as u32)
}

pub fn spawn(state: AppState) {
    if !state.slice_enabled(super::SLICE.id) {
        tracing::info!("search console sync pump not started (slice disabled by client overlay)");
        return;
    }
    std::thread::Builder::new()
        .name("search-console-sync-pump".to_string())
        .spawn(move || {
            tracing::info!("search console sync pump started");
            loop {
                let config = {
                    let persistence = state.persistence.lock();
                    match config_from_settings(persistence.connection_ref(), &state.client_id) {
                        Ok(config) => config,
                        Err(err) => {
                            tracing::warn!(error = %err, "search console sync config read failed");
                            SearchConsolePumpConfig {
                                enabled: false,
                                interval: Duration::from_secs(1800),
                                max_requests_per_cycle: 8,
                            }
                        }
                    }
                };
                if config.enabled && try_begin_sync(&state, now_ms()).is_ok() {
                    let result = run_guarded_cycle(&state, config.max_requests_per_cycle);
                    match result {
                        Ok(summary) if summary.requests_used > 0 => tracing::info!(
                            requests_used = summary.requests_used,
                            daily_rows = summary.daily_rows,
                            dimension_rows = summary.dimension_rows,
                            "search console sync complete"
                        ),
                        Ok(_) => {}
                        Err(err) => tracing::warn!(error = %err, "search console sync failed"),
                    }
                }
                std::thread::sleep(config.interval);
            }
        })
        .expect("spawn search-console-sync-pump thread");
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CycleSummary {
    pub requests_used: u32,
    pub daily_rows: usize,
    pub dimension_rows: usize,
    pub analytics_daily_rows: usize,
    pub analytics_dimension_rows: usize,
    pub rate_limited: bool,
}

pub fn try_begin_sync(state: &AppState, now: u64) -> Result<(), &'static str> {
    let mut status = state
        .sync_guards
        .guard(crate::http::Pump::SearchConsole)
        .lock();
    if status.in_flight {
        return Err("sync_in_flight");
    }
    if now < status.next_allowed_at_ms {
        return Err("sync_cooldown");
    }
    status.in_flight = true;
    status.last_attempt_ms = Some(now);
    Ok(())
}

pub fn run_guarded_cycle(state: &AppState, max_requests: u32) -> Result<CycleSummary, String> {
    let result = run_live_cycle(state, max_requests);
    finish_guarded_cycle(state, &result);
    result
}

pub fn run_guarded_analytics_cycle(
    state: &AppState,
    max_requests: u32,
) -> Result<CycleSummary, String> {
    let result = run_live_analytics_cycle(state, max_requests);
    finish_guarded_cycle(state, &result);
    result
}

pub(crate) fn finish_guarded_cycle(state: &AppState, result: &Result<CycleSummary, String>) {
    let mut status = state
        .sync_guards
        .guard(crate::http::Pump::SearchConsole)
        .lock();
    status.in_flight = false;
    status.next_allowed_at_ms = now_ms() + SEARCH_CONSOLE_SYNC_COOLDOWN_MS;
    match &result {
        Ok(summary) => {
            status.units_used = summary.requests_used;
            status.last_outcome = Some(if summary.rate_limited {
                "rate_limited".to_string()
            } else {
                "ok".to_string()
            });
        }
        Err(err) => status.last_outcome = Some(format!("error: {err}")),
    }
}

fn run_live_cycle(state: &AppState, max_requests: u32) -> Result<CycleSummary, String> {
    let config = service::config(state.search_console_overlay.as_ref().as_ref());
    let oauth = {
        let persistence = state.persistence.lock();
        crate::slices::google_connector::service::resolve_google_oauth(
            persistence.connection_ref(),
            &state.client_id,
            config.user_id.as_deref(),
        )
        .map_err(|err| err.to_string())?
    };
    let Some(oauth) = oauth else {
        return Ok(CycleSummary::default());
    };
    let now = now_ms();
    let discovery_cursor_key = discovery_cursor_key(state, &config)?;
    let search_console_scope_ok = oauth.scopes.is_empty()
        || google_oauth::has_scope(&oauth, GOOGLE_SEARCH_CONSOLE_READONLY_SCOPE);
    if !search_console_scope_ok {
        record_cursor_error(
            state,
            &discovery_cursor_key,
            "search_console_scope_missing_reconnect_google",
            now,
        )?;
    }
    let analytics_scope_ok = !config.analytics_configured()
        || oauth.scopes.is_empty()
        || google_oauth::has_scope(&oauth, GOOGLE_ANALYTICS_READONLY_SCOPE);
    if !analytics_scope_ok {
        if let Some(property_id) = config.ga4_property_id.as_deref().map(str::trim) {
            record_analytics_cursor_error(
                state,
                property_id,
                "google_analytics_scope_missing_reconnect_google",
                now,
            )?;
        }
    }
    if !search_console_scope_ok && !analytics_scope_ok {
        return Ok(CycleSummary::default());
    }
    let mut summary = CycleSummary::default();
    let mut remaining = max_requests;
    spend_request(&mut remaining)?;
    let access_token = google_oauth::fetch_access_token(&oauth).map_err(|err| err.to_string())?;
    summary.requests_used += 1;
    if search_console_scope_ok {
        let client = LiveSearchConsoleClient::default();
        let discovery_cursor = {
            let persistence = state.persistence.lock();
            store::get_cursor(
                persistence.connection_ref(),
                &state.client_id,
                &discovery_cursor_key,
            )
            .map_err(|err| err.to_string())?
        };
        if discovery_cursor.rate_limited_until_ms > now {
            summary.rate_limited = true;
        } else {
            spend_request(&mut remaining)?;
            discover_properties(
                state,
                &client,
                &access_token,
                &discovery_cursor_key,
                discovery_cursor,
                now,
            )?;
            summary.requests_used += 1;
            let effective = {
                let persistence = state.persistence.lock();
                service::effective_property(persistence.connection_ref(), &state.client_id, &config)
                    .map_err(|err| err.to_string())?
            };
            if let Some(effective) = effective {
                ensure_analytics_budget(remaining)?;
                let cycle = run_sync_cycle_for_property(
                    state,
                    &client,
                    &access_token,
                    &config,
                    &effective.property_url,
                    remaining,
                    now,
                )?;
                summary.requests_used += cycle.requests_used;
                summary.daily_rows = cycle.daily_rows;
                summary.dimension_rows = cycle.dimension_rows;
                summary.rate_limited |= cycle.rate_limited;
                remaining = remaining.saturating_sub(cycle.requests_used);
            }
        }
    }
    if remaining > 0 && config.analytics_configured() && analytics_scope_ok {
        let analytics_client = LiveAnalyticsDataClient::default();
        let analytics = run_analytics_sync_cycle(
            state,
            &analytics_client,
            &access_token,
            &config,
            remaining,
            now,
        )?;
        summary.requests_used += analytics.requests_used;
        summary.analytics_daily_rows = analytics.analytics_daily_rows;
        summary.analytics_dimension_rows = analytics.analytics_dimension_rows;
        summary.rate_limited |= analytics.rate_limited;
    }
    Ok(summary)
}

fn run_live_analytics_cycle(state: &AppState, max_requests: u32) -> Result<CycleSummary, String> {
    let config = service::config(state.search_console_overlay.as_ref().as_ref());
    let Some(property_id) = config.ga4_property_id.as_deref().map(str::trim) else {
        return Ok(CycleSummary::default());
    };
    if property_id.is_empty() {
        return Ok(CycleSummary::default());
    }
    let oauth = {
        let persistence = state.persistence.lock();
        crate::slices::google_connector::service::resolve_google_oauth(
            persistence.connection_ref(),
            &state.client_id,
            config.user_id.as_deref(),
        )
        .map_err(|err| err.to_string())?
    };
    let Some(oauth) = oauth else {
        return Ok(CycleSummary::default());
    };
    let now = now_ms();
    let analytics_scope_ok =
        oauth.scopes.is_empty() || google_oauth::has_scope(&oauth, GOOGLE_ANALYTICS_READONLY_SCOPE);
    if !analytics_scope_ok {
        record_analytics_cursor_error(
            state,
            property_id,
            "google_analytics_scope_missing_reconnect_google",
            now,
        )?;
        return Ok(CycleSummary::default());
    }
    let mut remaining = max_requests;
    spend_request(&mut remaining)?;
    let access_token = google_oauth::fetch_access_token(&oauth).map_err(|err| err.to_string())?;
    let analytics_client = LiveAnalyticsDataClient::default();
    let mut summary = run_analytics_sync_cycle(
        state,
        &analytics_client,
        &access_token,
        &config,
        remaining,
        now,
    )?;
    summary.requests_used += 1;
    Ok(summary)
}

fn spend_request(remaining: &mut u32) -> Result<(), String> {
    if *remaining == 0 {
        return Err("request_budget_exhausted".to_string());
    }
    *remaining -= 1;
    Ok(())
}

pub(crate) fn ensure_analytics_budget(remaining: u32) -> Result<(), String> {
    if remaining < ANALYTICS_REQUESTS_PER_CYCLE {
        return Err("request_budget_exhausted".to_string());
    }
    Ok(())
}

fn discovery_cursor_key(
    state: &AppState,
    config: &service::SearchConsoleConfig,
) -> Result<String, String> {
    if let Some(property_url) = config.property_url() {
        return Ok(property_url.to_string());
    }
    let persistence = state.persistence.lock();
    Ok(
        match service::effective_property(persistence.connection_ref(), &state.client_id, config)
            .map_err(|err| err.to_string())?
        {
            Some(effective) => effective.property_url,
            None => store::DISCOVERY_CURSOR_KEY.to_string(),
        },
    )
}

pub fn run_sync_cycle(
    state: &AppState,
    client: &dyn SearchConsoleClient,
    access_token: &str,
    config: &service::SearchConsoleConfig,
    max_requests: u32,
    now: u64,
) -> Result<CycleSummary, String> {
    let Some(property_url) = config.property_url() else {
        let persistence = state.persistence.lock();
        let Some(effective) =
            service::effective_property(persistence.connection_ref(), &state.client_id, config)
                .map_err(|err| err.to_string())?
        else {
            return Ok(CycleSummary::default());
        };
        return run_sync_cycle_for_property(
            state,
            client,
            access_token,
            config,
            &effective.property_url,
            max_requests,
            now,
        );
    };
    run_sync_cycle_for_property(
        state,
        client,
        access_token,
        config,
        property_url,
        max_requests,
        now,
    )
}

fn run_sync_cycle_for_property(
    state: &AppState,
    client: &dyn SearchConsoleClient,
    access_token: &str,
    config: &service::SearchConsoleConfig,
    property_url: &str,
    max_requests: u32,
    now: u64,
) -> Result<CycleSummary, String> {
    let today = service::today_utc();
    let Some((start_date, end_date)) = service::sync_window(&today, config.sync_days) else {
        return Ok(CycleSummary::default());
    };
    let mut cursor = {
        let persistence = state.persistence.lock();
        store::get_cursor(persistence.connection_ref(), &state.client_id, property_url)
            .map_err(|err| err.to_string())?
    };
    if cursor.rate_limited_until_ms > now {
        return Ok(CycleSummary::default());
    }
    let config_hash = service::config_hash(config);
    let mut budget = max_requests;
    let mut spend = || -> Result<(), String> {
        if budget == 0 {
            return Err("request_budget_exhausted".to_string());
        }
        budget -= 1;
        Ok(())
    };

    spend()?;
    let daily_rows = client
        .query(
            access_token,
            property_url,
            &start_date,
            &end_date,
            &["date"],
            500,
        )
        .map_err(|err| handle_error(state, property_url, cursor.clone(), err, now))?;
    spend()?;
    let query_rows = client
        .query(
            access_token,
            property_url,
            &start_date,
            &end_date,
            &["date", "query"],
            25_000,
        )
        .map_err(|err| handle_error(state, property_url, cursor.clone(), err, now))?;
    spend()?;
    let page_rows = client
        .query(
            access_token,
            property_url,
            &start_date,
            &end_date,
            &["date", "page"],
            25_000,
        )
        .map_err(|err| handle_error(state, property_url, cursor.clone(), err, now))?;

    let daily: Vec<store::DailyMetricRow> = daily_rows
        .into_iter()
        .filter_map(|row| {
            let date = row.keys.first()?.to_string();
            Some(store::DailyMetricRow {
                date,
                metrics: to_totals(&row.metrics),
            })
        })
        .collect();
    let mut dimensions = Vec::new();
    for row in query_rows {
        if row.keys.len() < 2 {
            continue;
        }
        let query = row.keys[1].clone();
        dimensions.push(store::DimensionMetricRow {
            date: row.keys[0].clone(),
            dimension_type: "query".to_string(),
            dimension_value: query.clone(),
            is_branded: service::is_branded_query(&query, &config.branded_query_patterns),
            metrics: to_totals(&row.metrics),
        });
    }
    for row in page_rows {
        if row.keys.len() < 2 {
            continue;
        }
        dimensions.push(store::DimensionMetricRow {
            date: row.keys[0].clone(),
            dimension_type: "page".to_string(),
            dimension_value: row.keys[1].clone(),
            is_branded: false,
            metrics: to_totals(&row.metrics),
        });
    }

    {
        let mut persistence = state.persistence.lock();
        store::replace_window(
            persistence.connection(),
            &state.client_id,
            property_url,
            store::SnapshotWindow {
                start_date: &start_date,
                end_date: &end_date,
                daily: &daily,
                dimensions: &dimensions,
            },
            now,
        )
        .map_err(|err| err.to_string())?;
    }
    cursor.config_hash = config_hash;
    cursor.synced_start_date = Some(start_date);
    cursor.synced_end_date = Some(end_date);
    cursor.rate_limited_until_ms = 0;
    cursor.last_error = None;
    cursor.last_synced_at_ms = Some(now);
    {
        let mut persistence = state.persistence.lock();
        store::put_cursor(
            persistence.connection(),
            &state.client_id,
            property_url,
            &cursor,
            now,
        )
        .map_err(|err| err.to_string())?;
    }
    Ok(CycleSummary {
        requests_used: max_requests - budget,
        daily_rows: daily.len(),
        dimension_rows: dimensions.len(),
        analytics_daily_rows: 0,
        analytics_dimension_rows: 0,
        rate_limited: false,
    })
}

pub fn run_analytics_sync_cycle(
    state: &AppState,
    client: &dyn AnalyticsDataClient,
    access_token: &str,
    config: &service::SearchConsoleConfig,
    max_requests: u32,
    now: u64,
) -> Result<CycleSummary, String> {
    let Some(property_id) = config.ga4_property_id.as_deref() else {
        return Ok(CycleSummary::default());
    };
    let today = service::today_utc();
    let Some((start_date, end_date)) = service::sync_window(&today, config.sync_days) else {
        return Ok(CycleSummary::default());
    };
    let mut cursor = {
        let persistence = state.persistence.lock();
        store::get_analytics_cursor(persistence.connection_ref(), &state.client_id, property_id)
            .map_err(|err| err.to_string())?
    };
    if cursor.rate_limited_until_ms > now {
        return Ok(CycleSummary::default());
    }
    let config_hash = service::config_hash(config);
    let mut budget = max_requests;
    let mut spend = || -> Result<(), String> {
        if budget == 0 {
            return Err("request_budget_exhausted".to_string());
        }
        budget -= 1;
        Ok(())
    };

    spend()?;
    let daily_rows = client
        .run_report(
            access_token,
            property_id,
            &start_date,
            &end_date,
            &["date"],
            500,
        )
        .map_err(|err| handle_analytics_error(state, property_id, cursor.clone(), err, now))?;
    spend()?;
    let landing_rows = client
        .run_report(
            access_token,
            property_id,
            &start_date,
            &end_date,
            &["date", "landingPagePlusQueryString"],
            25_000,
        )
        .map_err(|err| handle_analytics_error(state, property_id, cursor.clone(), err, now))?;
    spend()?;
    let source_rows = client
        .run_report(
            access_token,
            property_id,
            &start_date,
            &end_date,
            &["date", "sessionSourceMedium"],
            25_000,
        )
        .map_err(|err| handle_analytics_error(state, property_id, cursor.clone(), err, now))?;

    let daily: Vec<store::AnalyticsDailyMetricRow> = daily_rows
        .into_iter()
        .filter_map(|row| {
            Some(store::AnalyticsDailyMetricRow {
                date: normalize_ga_date(row.keys.first()?)?,
                metrics: to_analytics_totals(&row.metrics),
            })
        })
        .collect();
    let mut dimensions = Vec::new();
    for row in landing_rows {
        if row.keys.len() < 2 {
            continue;
        }
        let Some(date) = normalize_ga_date(&row.keys[0]) else {
            continue;
        };
        dimensions.push(store::AnalyticsDimensionMetricRow {
            date,
            dimension_type: "landing_page".to_string(),
            dimension_value: row.keys[1].clone(),
            metrics: to_analytics_totals(&row.metrics),
        });
    }
    for row in source_rows {
        if row.keys.len() < 2 {
            continue;
        }
        let Some(date) = normalize_ga_date(&row.keys[0]) else {
            continue;
        };
        dimensions.push(store::AnalyticsDimensionMetricRow {
            date,
            dimension_type: "source_medium".to_string(),
            dimension_value: row.keys[1].clone(),
            metrics: to_analytics_totals(&row.metrics),
        });
    }

    {
        let mut persistence = state.persistence.lock();
        store::replace_analytics_window(
            persistence.connection(),
            &state.client_id,
            property_id,
            store::AnalyticsSnapshotWindow {
                start_date: &start_date,
                end_date: &end_date,
                daily: &daily,
                dimensions: &dimensions,
            },
            now,
        )
        .map_err(|err| err.to_string())?;
    }
    cursor.config_hash = config_hash;
    cursor.synced_start_date = Some(start_date);
    cursor.synced_end_date = Some(end_date);
    cursor.rate_limited_until_ms = 0;
    cursor.last_error = None;
    cursor.last_synced_at_ms = Some(now);
    {
        let mut persistence = state.persistence.lock();
        store::put_analytics_cursor(
            persistence.connection(),
            &state.client_id,
            property_id,
            &cursor,
            now,
        )
        .map_err(|err| err.to_string())?;
    }
    Ok(CycleSummary {
        requests_used: max_requests - budget,
        daily_rows: 0,
        dimension_rows: 0,
        analytics_daily_rows: daily.len(),
        analytics_dimension_rows: dimensions.len(),
        rate_limited: false,
    })
}

pub(crate) fn discover_properties(
    state: &AppState,
    client: &dyn SearchConsoleClient,
    access_token: &str,
    cursor_key: &str,
    cursor: store::SearchConsoleCursor,
    now: u64,
) -> Result<Vec<SearchConsoleSite>, String> {
    let sites = client
        .list_sites(access_token)
        .map_err(|err| handle_error(state, cursor_key, cursor, err, now))?;
    let mut persistence = state.persistence.lock();
    store::replace_discovered_properties(persistence.connection(), &state.client_id, &sites, now)
        .map_err(|err| err.to_string())?;
    Ok(sites)
}

fn to_totals(metrics: &SearchConsoleMetrics) -> SearchConsoleMetricTotals {
    SearchConsoleMetricTotals {
        clicks: metrics.clicks,
        impressions: metrics.impressions,
        ctr_micros: (metrics.ctr * 1_000_000.0).round() as i64,
        position_micros: (metrics.position * 1_000_000.0).round() as i64,
    }
}

fn to_analytics_totals(metrics: &AnalyticsMetrics) -> AnalyticsMetricTotals {
    AnalyticsMetricTotals {
        sessions: metrics.sessions,
        total_users: metrics.total_users,
        event_count: metrics.event_count,
        conversions: metrics.conversions,
    }
}

fn normalize_ga_date(raw: &str) -> Option<String> {
    if raw.len() == 8 && raw.chars().all(|ch| ch.is_ascii_digit()) {
        return Some(format!("{}-{}-{}", &raw[0..4], &raw[4..6], &raw[6..8]));
    }
    if raw.len() >= 10
        && raw.as_bytes().get(4) == Some(&b'-')
        && raw.as_bytes().get(7) == Some(&b'-')
    {
        return Some(raw[0..10].to_string());
    }
    None
}

fn record_cursor_error(
    state: &AppState,
    property_url: &str,
    error: &str,
    now: u64,
) -> Result<(), String> {
    let mut persistence = state.persistence.lock();
    let mut cursor =
        store::get_cursor(persistence.connection_ref(), &state.client_id, property_url)
            .map_err(|err| err.to_string())?;
    cursor.last_error = Some(error.to_string());
    store::put_cursor(
        persistence.connection(),
        &state.client_id,
        property_url,
        &cursor,
        now,
    )
    .map_err(|err| err.to_string())
}

fn record_analytics_cursor_error(
    state: &AppState,
    property_id: &str,
    error: &str,
    now: u64,
) -> Result<(), String> {
    let mut persistence = state.persistence.lock();
    let mut cursor =
        store::get_analytics_cursor(persistence.connection_ref(), &state.client_id, property_id)
            .map_err(|err| err.to_string())?;
    cursor.last_error = Some(error.to_string());
    store::put_analytics_cursor(
        persistence.connection(),
        &state.client_id,
        property_id,
        &cursor,
        now,
    )
    .map_err(|err| err.to_string())
}

fn handle_error(
    state: &AppState,
    property_url: &str,
    mut cursor: store::SearchConsoleCursor,
    err: SearchConsoleError,
    now: u64,
) -> String {
    match err {
        SearchConsoleError::RateLimited {
            retry_after_ms,
            message,
        } => {
            cursor.rate_limited_until_ms = now + retry_after_ms.unwrap_or(300_000);
            cursor.last_error = Some(message);
        }
        SearchConsoleError::AuthRejected { message } => {
            cursor.last_error = Some(format!("auth_rejected: {message}"));
        }
        SearchConsoleError::Permanent { code, message } => {
            cursor.last_error = Some(format!("{code}: {message}"));
        }
    }
    let message = cursor
        .last_error
        .clone()
        .unwrap_or_else(|| "search_console_error".to_string());
    let mut persistence = state.persistence.lock();
    store::put_cursor(
        persistence.connection(),
        &state.client_id,
        property_url,
        &cursor,
        now,
    )
    .ok();
    message
}

fn handle_analytics_error(
    state: &AppState,
    property_id: &str,
    mut cursor: store::AnalyticsCursor,
    err: AnalyticsDataError,
    now: u64,
) -> String {
    match err {
        AnalyticsDataError::RateLimited {
            retry_after_ms,
            message,
        } => {
            cursor.rate_limited_until_ms = now + retry_after_ms.unwrap_or(300_000);
            cursor.last_error = Some(message);
        }
        AnalyticsDataError::AuthRejected { message } => {
            cursor.last_error = Some(format!("auth_rejected: {message}"));
        }
        AnalyticsDataError::Permanent { code, message } => {
            cursor.last_error = Some(format!("{code}: {message}"));
        }
    }
    let message = cursor
        .last_error
        .clone()
        .unwrap_or_else(|| "google_analytics_error".to_string());
    let mut persistence = state.persistence.lock();
    store::put_analytics_cursor(
        persistence.connection(),
        &state.client_id,
        property_id,
        &cursor,
        now,
    )
    .ok();
    message
}
