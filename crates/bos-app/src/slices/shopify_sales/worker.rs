//! Shopify sales sync pump: request-budgeted, env-gated, and cache-only.

use std::sync::Arc;
use std::time::Duration;

use bos_integrations::shopify_sales_read::{
    LiveShopifySalesReadClient, ReqwestShopifyHttpClient, ShopifySalesReadClient,
    ShopifySalesReadError, SHOPIFY_MAX_PAGE_SIZE,
};

use super::service;
use super::store::{self, ShopifySalesSyncState};
use crate::env_registry;
use crate::http::{now_ms, AppState};
use crate::store_core::StoreError;

pub const SHOPIFY_SALES_SYNC_COOLDOWN_MS: u64 = 120_000;

pub struct ShopifySalesPumpConfig {
    pub enabled: bool,
    pub interval: Duration,
    pub max_orders_per_cycle: u32,
}

pub fn config_from_settings(
    conn: &rusqlite::Connection,
    client_id: &str,
) -> Result<ShopifySalesPumpConfig, StoreError> {
    Ok(ShopifySalesPumpConfig {
        enabled: crate::slices::admin_settings::service::flag(
            conn,
            client_id,
            &env_registry::BOS_SHOPIFY_READ_SYNC_ENABLED,
        )?,
        interval: Duration::from_secs(
            crate::slices::admin_settings::service::usize_or(
                conn,
                client_id,
                &env_registry::BOS_SHOPIFY_READ_SYNC_INTERVAL_SECS,
                1800,
            )?
            .max(300) as u64,
        ),
        max_orders_per_cycle: max_orders_from_settings(conn, client_id)?,
    })
}

pub fn max_orders_from_settings(
    conn: &rusqlite::Connection,
    client_id: &str,
) -> Result<u32, StoreError> {
    Ok(crate::slices::admin_settings::service::usize_or(
        conn,
        client_id,
        &env_registry::BOS_SHOPIFY_READ_SYNC_MAX_ORDERS_PER_CYCLE,
        SHOPIFY_MAX_PAGE_SIZE as usize,
    )?
    .clamp(1, SHOPIFY_MAX_PAGE_SIZE as usize) as u32)
}

pub fn spawn(state: AppState) {
    if !state.slice_enabled(super::SLICE.id) {
        tracing::info!(
            "shopify sales sync pump not started (shopify_sales disabled by client overlay)"
        );
        return;
    }
    std::thread::Builder::new()
        .name("shopify-sales-sync-pump".to_string())
        .spawn(move || {
            tracing::info!("shopify sales sync pump started");
            loop {
                let config = {
                    let persistence = state.persistence.lock();
                    match config_from_settings(persistence.connection_ref(), &state.client_id) {
                        Ok(config) => config,
                        Err(err) => {
                            tracing::warn!(error = %err, "shopify sales sync config read failed");
                            ShopifySalesPumpConfig {
                                enabled: false,
                                interval: Duration::from_secs(1800),
                                max_orders_per_cycle: SHOPIFY_MAX_PAGE_SIZE,
                            }
                        }
                    }
                };
                if config.enabled && try_begin_sync(&state, now_ms()).is_ok() {
                    let summary = run_guarded_cycle(&state, config.max_orders_per_cycle);
                    match summary {
                        Ok(summary) if summary.requests_used > 0 => tracing::info!(
                            requests_used = summary.requests_used,
                            written = summary.written,
                            unchanged = summary.unchanged,
                            rate_limited = summary.rate_limited,
                            "shopify sales sync cycle complete"
                        ),
                        Ok(_) => {}
                        Err(err) => tracing::warn!(error = %err, "shopify sales sync cycle failed"),
                    }
                }
                std::thread::sleep(config.interval);
            }
        })
        .expect("spawn shopify-sales-sync-pump thread");
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CycleSummary {
    pub requests_used: u32,
    pub written: usize,
    pub unchanged: usize,
    pub rate_limited: bool,
}

pub fn try_begin_sync(state: &AppState, now: u64) -> Result<(), &'static str> {
    let mut status = state
        .sync_guards
        .guard(crate::http::Pump::ShopifySales)
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

pub fn run_guarded_cycle(state: &AppState, max_orders: u32) -> Result<CycleSummary, String> {
    let result = run_live_cycle(state, max_orders);
    let mut status = state
        .sync_guards
        .guard(crate::http::Pump::ShopifySales)
        .lock();
    status.in_flight = false;
    status.next_allowed_at_ms = now_ms() + SHOPIFY_SALES_SYNC_COOLDOWN_MS;
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

fn run_live_cycle(state: &AppState, max_orders: u32) -> Result<CycleSummary, String> {
    let Some(config) = service::connector_config_from_env() else {
        return Err("shopify connector unconfigured".to_string());
    };
    let client = LiveShopifySalesReadClient::new(
        Arc::new(ReqwestShopifyHttpClient::default()),
        config.to_read_config(),
    )
    .map_err(|err| err.to_string())?;
    run_sync_cycle(state, &client, &config.shop_domain, max_orders, now_ms())
}

pub fn run_sync_cycle(
    state: &AppState,
    read_client: &dyn ShopifySalesReadClient,
    shop_domain: &str,
    max_orders: u32,
    now: u64,
) -> Result<CycleSummary, String> {
    let mut summary = CycleSummary::default();
    let fingerprint = service::shop_domain_fingerprint(shop_domain);
    let sync_state = {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        store::reset_if_shop_changed(conn, &state.client_id, &fingerprint, now)
            .map_err(|err| err.to_string())?;
        let sync_state =
            store::get_sync_state(conn, &state.client_id).map_err(|err| err.to_string())?;
        if sync_state.rate_limited_until_ms > now {
            return Ok(summary);
        }
        sync_state
    };
    let order_cursor = (!sync_state.order_backfill_complete)
        .then_some(sync_state.order_backfill_cursor.as_deref())
        .flatten();
    let customer_cursor = (!sync_state.customer_backfill_complete)
        .then_some(sync_state.customer_backfill_cursor.as_deref())
        .flatten();

    let orders_page = match read_client.fetch_recent_orders_page(max_orders, order_cursor) {
        Ok(page) => page,
        Err(err) => {
            record_error(state, &fingerprint, &err, now)?;
            return match err {
                ShopifySalesReadError::RateLimited { .. } => {
                    summary.rate_limited = true;
                    Ok(summary)
                }
                _ => Err(err.to_string()),
            };
        }
    };
    summary.requests_used += 1;
    let customers_page = match read_client.fetch_customers_page(max_orders, customer_cursor) {
        Ok(page) => page,
        Err(err) => {
            record_error(state, &fingerprint, &err, now)?;
            return match err {
                ShopifySalesReadError::RateLimited { .. } => {
                    summary.rate_limited = true;
                    Ok(summary)
                }
                _ => Err(err.to_string()),
            };
        }
    };
    summary.requests_used += 1;

    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let order_summary =
        store::upsert_order_snapshots(conn, &state.client_id, &orders_page.records, now)
            .map_err(|err| err.to_string())?;
    let customer_summary =
        store::upsert_customer_snapshots(conn, &state.client_id, &customers_page.records, now)
            .map_err(|err| err.to_string())?;
    summary.written = order_summary.written + customer_summary.written;
    summary.unchanged = order_summary.unchanged + customer_summary.unchanged;
    let current = store::get_sync_state(conn, &state.client_id).map_err(|err| err.to_string())?;
    let order_backfill_complete = current.order_backfill_complete || !orders_page.has_next_page;
    let customer_backfill_complete =
        current.customer_backfill_complete || !customers_page.has_next_page;
    let order_backfill_cursor = if order_backfill_complete {
        None
    } else if orders_page.has_next_page {
        orders_page.end_cursor
    } else {
        None
    };
    let customer_backfill_cursor = if customer_backfill_complete {
        None
    } else if customers_page.has_next_page {
        customers_page.end_cursor
    } else {
        None
    };
    store::put_sync_state(
        conn,
        &state.client_id,
        &ShopifySalesSyncState {
            shop_domain_fingerprint: Some(fingerprint),
            backfill_complete: order_backfill_complete && customer_backfill_complete,
            order_backfill_complete,
            customer_backfill_complete,
            order_backfill_cursor,
            customer_backfill_cursor,
            rate_limited_until_ms: 0,
            last_error: None,
            last_advanced_at_ms: Some(now),
            last_order_count: orders_page.records.len() as u64,
            last_customer_count: customers_page.records.len() as u64,
        },
        now,
    )
    .map_err(|err| err.to_string())?;
    Ok(summary)
}

fn record_error(
    state: &AppState,
    fingerprint: &str,
    err: &ShopifySalesReadError,
    now: u64,
) -> Result<(), String> {
    let rate_limited_until_ms = match err {
        ShopifySalesReadError::RateLimited {
            retry_after_ms: Some(delay),
            ..
        } => now.saturating_add(*delay),
        ShopifySalesReadError::RateLimited { .. } => now.saturating_add(300_000),
        _ => 0,
    };
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let current = store::get_sync_state(conn, &state.client_id).map_err(|err| err.to_string())?;
    store::put_sync_state(
        conn,
        &state.client_id,
        &ShopifySalesSyncState {
            shop_domain_fingerprint: Some(fingerprint.to_string()),
            backfill_complete: current.backfill_complete,
            order_backfill_complete: current.order_backfill_complete,
            customer_backfill_complete: current.customer_backfill_complete,
            order_backfill_cursor: current.order_backfill_cursor,
            customer_backfill_cursor: current.customer_backfill_cursor,
            rate_limited_until_ms,
            last_error: Some(err.to_string()),
            last_advanced_at_ms: current.last_advanced_at_ms,
            last_order_count: current.last_order_count,
            last_customer_count: current.last_customer_count,
        },
        now,
    )
    .map_err(|err| err.to_string())?;
    Ok(())
}
