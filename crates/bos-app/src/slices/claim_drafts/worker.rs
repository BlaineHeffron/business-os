//! Claims pump: polls Stockforge damage events into the local cache.
//! Bounded, request-budgeted, backoff-respecting; off unless
//! BOS_CLAIMS_SYNC_ENABLED. Cycle shape (each request costs one budget
//! unit): OPEN damage list (one page covers Demo's volume) → upsert snapshots
//! (content-hash, receipt-quiet) → emit ONE work item per OPEN damage event
//! (one-item-per-source; re-reported events do not duplicate) → RESOLVED
//! damage list for report status refresh only → fetch
//! pack-station photos for snapshots that still need them (one request per
//! shipment with a pack-station container).
//!
//! Reuses the inventory slice's Stockforge connector config (same base URL
//! and VIEWER api key). Rate-limit standdown is tracked on this slice's own
//! cursor; the pump runs at a long interval with a small budget so the two
//! Stockforge pumps stay trivially inside the instance limiter.

use std::sync::Arc;
use std::time::Duration;

use bos_integrations::stockforge_read::{
    LiveStockforgeReadClient, ReqwestStockforgeHttpClient, StockforgeError, StockforgeReadClient,
};

use super::store::{self, ClaimsSyncCursor};
use crate::env_registry;
use crate::http::{now_ms, AppState};
use crate::store_core::StoreError;

/// Minimum gap between claim sync cycles (manual or pump).
pub const CLAIMS_SYNC_COOLDOWN_MS: u64 = 60_000;
/// One damage-list page covers Demo's claim volume.
const DAMAGE_PAGE_SIZE: u32 = 100;
const DAMAGE_CLAIM_STATUS_FETCHES: &[(&str, bool)] = &[
    ("OPEN", true),
    // Reporting-only refresh: do not create new queue items for damage events
    // first seen after Stockforge already marked them resolved.
    ("RESOLVED", false),
];

pub struct ClaimsPumpConfig {
    pub enabled: bool,
    pub interval: Duration,
    pub max_requests_per_cycle: u32,
}

pub fn config_from_settings(
    conn: &rusqlite::Connection,
    client_id: &str,
) -> Result<ClaimsPumpConfig, StoreError> {
    Ok(ClaimsPumpConfig {
        enabled: crate::slices::admin_settings::service::flag(
            conn,
            client_id,
            &env_registry::BOS_CLAIMS_SYNC_ENABLED,
        )?,
        interval: Duration::from_secs(
            crate::slices::admin_settings::service::usize_or(
                conn,
                client_id,
                &env_registry::BOS_CLAIMS_SYNC_INTERVAL_SECS,
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
        &env_registry::BOS_CLAIMS_MAX_REQUESTS_PER_CYCLE,
        5,
    )?
    .clamp(1, 20) as u32)
}

pub fn spawn(state: AppState) {
    if !state.slice_enabled(super::SLICE.id) {
        tracing::info!("claims pump not started (claim_drafts disabled by client overlay)");
        return;
    }
    std::thread::Builder::new()
        .name("claims-sync-pump".to_string())
        .spawn(move || {
            tracing::info!("claims pump started");
            loop {
                let config = {
                    let persistence = state.persistence.lock();
                    match config_from_settings(persistence.connection_ref(), &state.client_id) {
                        Ok(config) => config,
                        Err(err) => {
                            tracing::warn!(error = %err, "claims sync config read failed");
                            ClaimsPumpConfig {
                                enabled: false,
                                interval: Duration::from_secs(1800),
                                max_requests_per_cycle: 5,
                            }
                        }
                    }
                };
                if config.enabled && try_begin_sync(&state, now_ms()).is_ok() {
                    let summary = run_guarded_cycle(&state, config.max_requests_per_cycle);
                    match summary {
                        Ok(summary) if summary.requests_used > 0 => tracing::info!(
                            requests_used = summary.requests_used,
                            upserted = summary.upserted,
                            items_emitted = summary.items_emitted,
                            photos_fetched = summary.photos_fetched,
                            rate_limited = summary.rate_limited,
                            "claims sync cycle complete"
                        ),
                        Ok(_) => {}
                        Err(err) => tracing::warn!(error = %err, "claims sync cycle failed"),
                    }
                }
                std::thread::sleep(config.interval);
            }
        })
        .expect("spawn claims-sync-pump thread");
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CycleSummary {
    pub requests_used: u32,
    pub upserted: usize,
    pub items_emitted: usize,
    pub photos_fetched: usize,
    pub rate_limited: bool,
}

/// Claim the sync slot. Err = someone else is syncing or cooldown active.
pub fn try_begin_sync(state: &AppState, now: u64) -> Result<(), &'static str> {
    let mut status = state.sync_guards.guard(crate::http::Pump::Claims).lock();
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

/// Run one cycle with the LIVE client and release the slot. Caller must
/// hold the slot via [`try_begin_sync`].
pub fn run_guarded_cycle(state: &AppState, max_requests: u32) -> Result<CycleSummary, String> {
    let result = run_live_cycle(state, max_requests);
    let mut status = state.sync_guards.guard(crate::http::Pump::Claims).lock();
    status.in_flight = false;
    status.next_allowed_at_ms = now_ms() + CLAIMS_SYNC_COOLDOWN_MS;
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
    result
}

fn run_live_cycle(state: &AppState, max_requests: u32) -> Result<CycleSummary, String> {
    let Some(config) = crate::slices::inventory::service::connector_config_from_env() else {
        // Stockforge unconfigured — wait quietly; the inventory status
        // surface already says so.
        return Ok(CycleSummary::default());
    };
    let http = Arc::new(ReqwestStockforgeHttpClient::default());
    let read_client = LiveStockforgeReadClient::new(http, config.base_url.clone());
    run_sync_cycle(state, &read_client, &config.api_key, max_requests, now_ms())
}

/// The testable cycle core: every external seam is injected.
pub fn run_sync_cycle(
    state: &AppState,
    client: &dyn StockforgeReadClient,
    api_key: &str,
    max_requests: u32,
    now: u64,
) -> Result<CycleSummary, String> {
    let mut summary = CycleSummary::default();
    let cursor = {
        let persistence = state.persistence.lock();
        store::get_cursor(persistence.connection_ref(), &state.client_id)
            .map_err(|err| err.to_string())?
    };
    if cursor.rate_limited_until_ms > now {
        return Ok(summary);
    }
    let mut budget = max_requests;

    // Phase 1: damage lists. OPEN rows create queue items; RESOLVED rows only
    // refresh the local reporting cache.
    for (claim_status, emit_items) in DAMAGE_CLAIM_STATUS_FETCHES {
        if budget == 0 {
            return Ok(summary);
        }
        budget -= 1;
        summary.requests_used += 1;
        let page = match client.fetch_damage_events(api_key, claim_status, 0, DAMAGE_PAGE_SIZE) {
            Ok(page) => page,
            Err(err) => return handle_stockforge_error(state, &mut summary, cursor, err, now),
        };
        for record in &page.records {
            let mut persistence = state.persistence.lock();
            let conn = persistence.connection();
            if store::upsert_damage_snapshot(conn, &state.client_id, record, now)
                .map_err(|err| err.to_string())?
            {
                summary.upserted += 1;
            }
            if !emit_items {
                continue;
            }
            // One work item per damage event, ever (one-item-per-source).
            let snapshot =
                store::get_damage_snapshot(conn, &state.client_id, &record.damage_event_id)
                    .map_err(|err| err.to_string())?
                    .expect("snapshot just upserted");
            if crate::slices::work_queue::service::emit_unconditional(
                conn,
                &state.client_id,
                crate::slices::work_queue::service::UnconditionalEmit {
                    source_kind: crate::slices::work_queue::SOURCE_KIND_STOCKFORGE_DAMAGE,
                    source_ref: &record.damage_event_id,
                    category_id: super::DAMAGE_CATEGORY,
                    title: &super::service::damage_item_title(&snapshot),
                    summary: &super::service::produce_source_view(&snapshot).body_excerpt,
                    default_kinds: vec![super::service::PACKET_KIND.to_string()],
                    allow_policy_kinds: true,
                    source_user_id: None,
                    status: bos_contracts::work_queue::WorkItemStatus::Open,
                },
                now,
            )
            .map_err(|err| err.to_string())?
            {
                summary.items_emitted += 1;
            }
        }
    }

    // Phase 2: pack-station photos for snapshots that still need them. A
    // shipment without a matched order/pack container resolves to "nothing
    // to fetch" locally (zero requests) so it never starves the queue.
    loop {
        let pending = {
            let persistence = state.persistence.lock();
            store::damage_snapshots_needing_photos(
                persistence.connection_ref(),
                &state.client_id,
                (budget.max(1)) as usize,
            )
            .map_err(|err| err.to_string())?
        };
        if pending.is_empty() {
            break;
        }
        let mut progressed = false;
        for snapshot in pending {
            let container = {
                let persistence = state.persistence.lock();
                crate::slices::inventory::store::get_order_by_shipment(
                    persistence.connection_ref(),
                    &state.client_id,
                    &snapshot.shipment_id,
                )
                .map_err(|err| err.to_string())?
                .and_then(|order| {
                    (order.photo_count > 0)
                        .then_some(order.pack_station_container_id)
                        .flatten()
                })
            };
            let Some(container_id) = container else {
                // No order match / no container / no photos — done locally.
                let mut persistence = state.persistence.lock();
                store::set_pack_photos(
                    persistence.connection(),
                    &state.client_id,
                    &snapshot.damage_event_id,
                    &[],
                    now,
                )
                .map_err(|err| err.to_string())?;
                progressed = true;
                continue;
            };
            if budget == 0 {
                return Ok(summary);
            }
            budget -= 1;
            summary.requests_used += 1;
            match client.fetch_container_photos(api_key, &container_id) {
                Ok(photos) => {
                    let urls: Vec<String> = photos
                        .unwrap_or_default()
                        .into_iter()
                        .map(|photo| photo.url)
                        .collect();
                    let mut persistence = state.persistence.lock();
                    store::set_pack_photos(
                        persistence.connection(),
                        &state.client_id,
                        &snapshot.damage_event_id,
                        &urls,
                        now,
                    )
                    .map_err(|err| err.to_string())?;
                    summary.photos_fetched += 1;
                    progressed = true;
                }
                Err(err) => return handle_stockforge_error(state, &mut summary, cursor, err, now),
            }
        }
        if !progressed {
            break;
        }
    }

    // Healthy cycle: clear any stale error.
    if cursor.last_error.is_some() || cursor.rate_limited_until_ms != 0 {
        let cleared = ClaimsSyncCursor {
            rate_limited_until_ms: 0,
            last_error: None,
            last_advanced_at_ms: cursor.last_advanced_at_ms,
        };
        put_cursor(state, &cleared, now)?;
    }
    Ok(summary)
}

fn handle_stockforge_error(
    state: &AppState,
    summary: &mut CycleSummary,
    mut cursor: ClaimsSyncCursor,
    err: StockforgeError,
    now: u64,
) -> Result<CycleSummary, String> {
    match err {
        StockforgeError::RateLimited {
            retry_after_ms,
            message,
        } => {
            summary.rate_limited = true;
            cursor.rate_limited_until_ms = now + retry_after_ms.unwrap_or(60_000);
            cursor.last_error = Some(message);
            put_cursor(state, &cursor, now)?;
            Ok(*summary)
        }
        StockforgeError::AuthRejected { message } => {
            cursor.last_error = Some(format!("auth: {message}"));
            put_cursor(state, &cursor, now)?;
            Err(format!("stockforge api key rejected: {message}"))
        }
        other => {
            cursor.last_error = Some(other.to_string().chars().take(300).collect());
            put_cursor(state, &cursor, now)?;
            Err(other.to_string())
        }
    }
}

fn put_cursor(state: &AppState, cursor: &ClaimsSyncCursor, now: u64) -> Result<(), String> {
    let mut persistence = state.persistence.lock();
    store::put_cursor(persistence.connection(), &state.client_id, cursor, now)
        .map_err(|err| err.to_string())?;
    Ok(())
}
