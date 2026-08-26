//! Stockforge sync pump: bounded, request-budgeted, backoff-respecting. Off
//! unless BOS_STOCKFORGE_SYNC_ENABLED; the manual Sync-now route runs the
//! same cycle core through the same serialization guard, so there is NEVER
//! more than one Stockforge request in flight from this process.
//!
//! Auth posture: a static org API key (VIEWER role) from env rides every
//! request as the bearer token — no login flow, no session state. A 401
//! means the key is invalid/revoked/expired and only an operator can fix
//! it, so the cycle records the error and stops; nothing retries.
//!
//! Cycle shape (budget default 10, one request each unless noted):
//! materials (paginated walk, resumes across cycles) → alerts → reorder
//! suggestions → order board (30-day window) → purchase orders. The
//! full-set entities prune rows that left the fetched set; steady state
//! writes zero rows anywhere.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use bos_integrations::stockforge_read::{
    LiveStockforgeReadClient, ReqwestStockforgeHttpClient, StockforgeError, StockforgeReadClient,
    STOCKFORGE_MAX_PAGE_SIZE,
};

use super::service::{self, StockforgeConnectorConfig};
use super::store::{self, SfSyncCursor};
use crate::env_registry;
use crate::http::{now_ms, AppState};
use crate::store_core::StoreError;

/// Minimum gap between manual Sync-now requests (also applies after pump
/// cycles). Stockforge is our own service — shorter than QBO's cooldown.
pub const STOCKFORGE_SYNC_COOLDOWN_MS: u64 = 60_000;

pub struct StockforgePumpConfig {
    pub enabled: bool,
    pub interval: Duration,
    pub max_requests_per_cycle: u32,
}

pub fn config_from_settings(
    conn: &rusqlite::Connection,
    client_id: &str,
) -> Result<StockforgePumpConfig, StoreError> {
    Ok(StockforgePumpConfig {
        enabled: crate::slices::admin_settings::service::flag(
            conn,
            client_id,
            &env_registry::BOS_STOCKFORGE_SYNC_ENABLED,
        )?,
        interval: Duration::from_secs(
            crate::slices::admin_settings::service::usize_or(
                conn,
                client_id,
                &env_registry::BOS_STOCKFORGE_SYNC_INTERVAL_SECS,
                900,
            )?
            .max(120) as u64,
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
        &env_registry::BOS_STOCKFORGE_MAX_REQUESTS_PER_CYCLE,
        10,
    )?
    .clamp(2, 30) as u32)
}

pub fn spawn(state: AppState) {
    if !state.slice_enabled(super::SLICE.id) {
        tracing::info!("stockforge sync pump not started (inventory disabled by client overlay)");
        return;
    }
    std::thread::Builder::new()
        .name("stockforge-sync-pump".to_string())
        .spawn(move || {
            tracing::info!("stockforge sync pump started");
            loop {
                let config = {
                    let persistence = state.persistence.lock();
                    match config_from_settings(persistence.connection_ref(), &state.client_id) {
                        Ok(config) => config,
                        Err(err) => {
                            tracing::warn!(error = %err, "stockforge sync config read failed");
                            StockforgePumpConfig {
                                enabled: false,
                                interval: Duration::from_secs(900),
                                max_requests_per_cycle: 10,
                            }
                        }
                    }
                };
                if config.enabled && try_begin_sync(&state, now_ms()).is_ok() {
                    let summary = run_guarded_cycle(&state, config.max_requests_per_cycle);
                    match summary {
                        Ok(summary) if summary.requests_used > 0 => tracing::info!(
                            requests_used = summary.requests_used,
                            written = summary.written,
                            unchanged = summary.unchanged,
                            pruned = summary.pruned,
                            rate_limited = summary.rate_limited,
                            "stockforge sync cycle complete"
                        ),
                        Ok(_) => {}
                        Err(err) => tracing::warn!(error = %err, "stockforge sync cycle failed"),
                    }
                }
                std::thread::sleep(config.interval);
            }
        })
        .expect("spawn stockforge-sync-pump thread");
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CycleSummary {
    pub requests_used: u32,
    pub written: usize,
    pub unchanged: usize,
    pub pruned: usize,
    pub rate_limited: bool,
}

/// Claim the sync slot. Err = someone else is syncing or cooldown active.
pub fn try_begin_sync(state: &AppState, now: u64) -> Result<(), &'static str> {
    let mut status = state
        .sync_guards
        .guard(crate::http::Pump::Stockforge)
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

/// Run one cycle with the LIVE client and release the slot. Caller must hold
/// the slot via [`try_begin_sync`].
pub fn run_guarded_cycle(state: &AppState, max_requests: u32) -> Result<CycleSummary, String> {
    let result = run_live_cycle(state, max_requests);
    let mut status = state
        .sync_guards
        .guard(crate::http::Pump::Stockforge)
        .lock();
    status.in_flight = false;
    status.next_allowed_at_ms = now_ms() + STOCKFORGE_SYNC_COOLDOWN_MS;
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

/// Webhook-triggered "sync as soon as the guard allows". Claims the slot and
/// runs immediately when free; otherwise parks ONE deferred waiter (bursts of
/// webhook events collapse into a single pending sync) that claims the slot
/// when the in-flight cycle / cooldown ends. Gives up quietly after ~5
/// minutes — the periodic pump is the fallback cadence.
pub fn kick_sync_soon(state: AppState) {
    let now = now_ms();
    if try_begin_sync(&state, now).is_ok() {
        spawn_cycle_thread(state, "stockforge-webhook-sync");
        return;
    }
    {
        let mut status = state
            .sync_guards
            .guard(crate::http::Pump::Stockforge)
            .lock();
        if status.kick_pending {
            return; // a waiter already exists; this event rides along
        }
        status.kick_pending = true;
    }
    std::thread::Builder::new()
        .name("stockforge-webhook-wait".to_string())
        .spawn(move || {
            for _ in 0..60 {
                std::thread::sleep(Duration::from_secs(5));
                if try_begin_sync(&state, now_ms()).is_ok() {
                    clear_kick_pending(&state);
                    let max_requests = {
                        let persistence = state.persistence.lock();
                        match max_requests_from_settings(
                            persistence.connection_ref(),
                            &state.client_id,
                        ) {
                            Ok(max_requests) => max_requests,
                            Err(err) => {
                                tracing::warn!(
                                    error = %err,
                                    "webhook-deferred stockforge sync config read failed"
                                );
                                state
                                    .sync_guards
                                    .guard(crate::http::Pump::Stockforge)
                                    .lock()
                                    .in_flight = false;
                                return;
                            }
                        }
                    };
                    if let Err(err) = run_guarded_cycle(&state, max_requests) {
                        tracing::warn!(error = %err, "webhook-deferred stockforge sync failed");
                    }
                    return;
                }
            }
            clear_kick_pending(&state);
        })
        .ok();
}

fn clear_kick_pending(state: &AppState) {
    state
        .sync_guards
        .guard(crate::http::Pump::Stockforge)
        .lock()
        .kick_pending = false;
}

fn spawn_cycle_thread(state: AppState, thread_name: &str) {
    let max_requests = {
        let persistence = state.persistence.lock();
        match max_requests_from_settings(persistence.connection_ref(), &state.client_id) {
            Ok(max_requests) => max_requests,
            Err(err) => {
                tracing::warn!(error = %err, "kicked stockforge sync config read failed");
                return;
            }
        }
    };
    std::thread::Builder::new()
        .name(thread_name.to_string())
        .spawn(move || {
            if let Err(err) = run_guarded_cycle(&state, max_requests) {
                tracing::warn!(error = %err, "kicked stockforge sync failed");
            }
        })
        .ok();
}

fn run_live_cycle(state: &AppState, max_requests: u32) -> Result<CycleSummary, String> {
    // Config resolves per cycle so env changes apply without a restart.
    let Some(config) = service::connector_config_from_env() else {
        return Err("stockforge connector unconfigured".to_string());
    };
    let http = Arc::new(ReqwestStockforgeHttpClient::default());
    let read_client = LiveStockforgeReadClient::new(http, config.base_url.clone());
    run_sync_cycle(state, &read_client, &config, max_requests, now_ms())
}

/// The testable cycle core: every external seam is injected.
pub fn run_sync_cycle(
    state: &AppState,
    read_client: &dyn StockforgeReadClient,
    config: &StockforgeConnectorConfig,
    max_requests: u32,
    now: u64,
) -> Result<CycleSummary, String> {
    let mut summary = CycleSummary::default();

    // A 429 deadline is instance-wide (one rate limiter in front of all the
    // routes): if any cursor carries a future backoff deadline, the whole
    // cycle stands down.
    let backoff_until = {
        let persistence = state.persistence.lock();
        let conn = persistence.connection_ref();
        store::ALL_ENTITIES
            .iter()
            .map(|entity| {
                store::get_cursor(conn, &state.client_id, entity)
                    .map(|cursor| cursor.rate_limited_until_ms)
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| err.to_string())?
            .into_iter()
            .max()
            .unwrap_or(0)
    };
    if backoff_until > now {
        return Ok(summary);
    }

    let mut budget = max_requests;
    let mut applied_any = false;
    let today = service::today_string(now);
    let (window_start, window_end) = service::order_window(&today);

    for entity in store::ALL_ENTITIES {
        loop {
            if budget == 0 {
                return Ok(summary);
            }
            let cursor = {
                let persistence = state.persistence.lock();
                store::get_cursor(persistence.connection_ref(), &state.client_id, entity)
                    .map_err(|err| err.to_string())?
            };
            budget -= 1;
            summary.requests_used += 1;
            let fetched = fetch_entity(
                read_client,
                entity,
                &config.api_key,
                &cursor,
                &window_start,
                &window_end,
            );
            match fetched {
                Ok(payload) => {
                    let applied = apply_payload(state, entity, &payload, cursor, now)
                        .map_err(|err| err.to_string())?;
                    applied_any = true;
                    summary.written += applied.written;
                    summary.unchanged += applied.unchanged;
                    summary.pruned += applied.pruned;
                    if applied.walk_complete {
                        break; // entity caught up this cycle
                    }
                }
                Err(StockforgeError::RateLimited {
                    retry_after_ms,
                    message,
                }) => {
                    // Stop the WHOLE cycle; stamp the deadline so even the
                    // next cycle skips until it passes.
                    summary.rate_limited = true;
                    let mut stamped = cursor;
                    stamped.rate_limited_until_ms = now + retry_after_ms.unwrap_or(60_000);
                    stamped.last_error = Some(message);
                    stamped.last_error_class = Some("rate_limited".to_string());
                    stamped.last_error_at_ms = Some(now);
                    let mut persistence = state.persistence.lock();
                    store::put_cursor(
                        persistence.connection(),
                        &state.client_id,
                        entity,
                        &stamped,
                        now,
                    )
                    .map_err(|err| err.to_string())?;
                    return Ok(summary);
                }
                Err(StockforgeError::AuthRejected { message }) => {
                    // The static key is invalid/revoked/expired — nothing a
                    // retry can fix. Record it where the status view reads
                    // and end the cycle.
                    record_entity_error(state, entity, &message, "auth", now)?;
                    return Err(format!("stockforge api key rejected: {message}"));
                }
                Err(err) => {
                    record_entity_error(
                        state,
                        entity,
                        &err.to_string(),
                        service::classify_stockforge_error(&err),
                        now,
                    )?;
                    break; // cursor untouched; next cycle resumes here
                }
            }
        }
    }
    if applied_any {
        let mut status = state
            .sync_guards
            .guard(crate::http::Pump::Stockforge)
            .lock();
        status.last_success_ms = Some(now);
    }
    Ok(summary)
}

enum Payload {
    Materials(
        bos_integrations::stockforge_read::SfPage<
            bos_integrations::stockforge_read::SfMaterialRecord,
        >,
    ),
    Alerts(Vec<bos_integrations::stockforge_read::SfAlertRecord>),
    Reorders(Vec<bos_integrations::stockforge_read::SfReorderSuggestionRecord>),
    Orders(Vec<bos_integrations::stockforge_read::SfOrderCardRecord>),
    PurchaseOrders(
        bos_integrations::stockforge_read::SfPage<
            bos_integrations::stockforge_read::SfPurchaseOrderRecord,
        >,
    ),
}

fn fetch_entity(
    client: &dyn StockforgeReadClient,
    entity: &str,
    access_token: &str,
    cursor: &SfSyncCursor,
    window_start: &str,
    window_end: &str,
) -> Result<Payload, StockforgeError> {
    match entity {
        store::ENTITY_MATERIAL => client
            .fetch_materials(access_token, cursor.next_skip, STOCKFORGE_MAX_PAGE_SIZE)
            .map(Payload::Materials),
        store::ENTITY_ALERT => client
            .fetch_active_alerts(access_token)
            .map(Payload::Alerts),
        store::ENTITY_REORDER => client
            .fetch_reorder_suggestions(access_token)
            .map(Payload::Reorders),
        store::ENTITY_ORDER => client
            .fetch_order_board(access_token, window_start, window_end)
            .map(Payload::Orders),
        _ => client
            // Newest 100 POs cover Demo's volume; older ones age out of the
            // open-PO view by status anyway.
            .fetch_purchase_orders(access_token, 0, STOCKFORGE_MAX_PAGE_SIZE)
            .map(Payload::PurchaseOrders),
    }
}

struct AppliedPayload {
    written: usize,
    unchanged: usize,
    pruned: usize,
    walk_complete: bool,
}

/// Persist one fetched payload + the advanced cursor under a single lock
/// hold. Full-set entities (alerts/reorders/orders) prune rows missing from
/// the fetch; the material walk only advances its offset.
fn apply_payload(
    state: &AppState,
    entity: &str,
    payload: &Payload,
    mut cursor: SfSyncCursor,
    now: u64,
) -> Result<AppliedPayload, crate::store_core::StoreError> {
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let client_id: &str = &state.client_id;
    let mut pruned = 0;
    let (summary, walk_complete) = match payload {
        Payload::Materials(page) => {
            let summary = store::upsert_material_snapshots(conn, client_id, &page.records, now)?;
            let complete = page.records.len() < page.requested_take as usize;
            cursor.next_skip = if complete {
                0
            } else {
                cursor.next_skip + page.requested_take
            };
            if complete {
                cursor.backfill_complete = true;
            }
            (summary, complete)
        }
        Payload::Alerts(records) => {
            let summary = store::upsert_alert_snapshots(conn, client_id, records, now)?;
            let keep: HashSet<String> = records
                .iter()
                .map(|record| record.alert_id.clone())
                .collect();
            pruned = store::prune_missing(
                conn,
                client_id,
                "stockforge_alert_snapshots",
                "alert_id",
                store::ALERT_ENTITY_KIND,
                &keep,
                now,
            )?;
            cursor.backfill_complete = true;
            (summary, true)
        }
        Payload::Reorders(records) => {
            let summary = store::upsert_reorder_snapshots(conn, client_id, records, now)?;
            let keep: HashSet<String> = records
                .iter()
                .map(|record| record.suggestion_id.clone())
                .collect();
            pruned = store::prune_missing(
                conn,
                client_id,
                "stockforge_reorder_snapshots",
                "suggestion_id",
                store::REORDER_ENTITY_KIND,
                &keep,
                now,
            )?;
            cursor.backfill_complete = true;
            (summary, true)
        }
        Payload::Orders(records) => {
            let summary = store::upsert_order_snapshots(conn, client_id, records, now)?;
            let keep: HashSet<String> = records
                .iter()
                .map(|record| record.order_id.clone())
                .collect();
            pruned = store::prune_missing(
                conn,
                client_id,
                "stockforge_order_snapshots",
                "order_id",
                store::ORDER_ENTITY_KIND,
                &keep,
                now,
            )?;
            cursor.backfill_complete = true;
            (summary, true)
        }
        Payload::PurchaseOrders(page) => {
            let summary = store::upsert_po_snapshots(conn, client_id, &page.records, now)?;
            cursor.backfill_complete = true;
            (summary, true)
        }
    };
    cursor.last_error = None;
    cursor.last_error_class = None;
    cursor.last_error_at_ms = None;
    cursor.rate_limited_until_ms = 0;
    store::put_cursor(conn, client_id, entity, &cursor, now)?;
    Ok(AppliedPayload {
        written: summary.written,
        unchanged: summary.unchanged,
        pruned,
        walk_complete,
    })
}

fn record_entity_error(
    state: &AppState,
    entity: &str,
    error: &str,
    class: &str,
    now: u64,
) -> Result<(), String> {
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let mut cursor =
        store::get_cursor(conn, &state.client_id, entity).map_err(|err| err.to_string())?;
    cursor.last_error = Some(error.chars().take(300).collect());
    cursor.last_error_class = Some(class.to_string());
    cursor.last_error_at_ms = Some(now);
    store::put_cursor(conn, &state.client_id, entity, &cursor, now)
        .map_err(|err| err.to_string())?;
    Ok(())
}
