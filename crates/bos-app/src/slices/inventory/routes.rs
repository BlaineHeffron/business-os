//! Thin HTTP handlers for the Stockforge connector + cached inventory views.
//! Every GET view serves from the local snapshot cache — these routes NEVER
//! call Stockforge. Only the (guarded) Sync-now kickoff touches it, and there
//! is no connect/callback pair: the credential is the env-provided service
//! account, so status just reports configured-or-not.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bos_contracts::inventory::{
    InventoryAlertsResponse, InventoryOrdersResponse, InventoryPurchaseOrdersResponse,
    InventoryStockResponse, InventorySyncInfo, InventorySyncNowResponse,
};

use super::service;
use super::store;
use super::worker;
use crate::http::{now_ms, AppState, SyncGuard};
use crate::store_core::StoreError;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/connectors/stockforge/status", get(status))
        .route("/api/inventory/sync", post(sync_now))
        .route("/api/inventory/stock", get(stock))
        .route("/api/inventory/alerts", get(alerts))
        .route("/api/inventory/orders", get(orders))
        .route("/api/inventory/purchase-orders", get(purchase_orders))
        .route("/api/webhooks/stockforge", post(webhook))
}

/// Inbound Stockforge webhook (stock.warning/critical/out, order events, …).
/// NOT operator-authenticated — callers prove themselves with the HMAC
/// signature over `{timestamp}.{body}` (replay-bounded). A verified event
/// just kicks the normal guarded sync cycle: the payload itself is never
/// trusted as data, so a forged-but-somehow-verified body still can't write
/// anything — the cycle re-reads everything from the Stockforge API.
async fn webhook(State(state): State<AppState>, headers: HeaderMap, body: String) -> Response {
    let Some(secret) = service::webhook_secret_from_env() else {
        // Unconfigured = the route effectively doesn't exist.
        return crate::http::error_response(StatusCode::NOT_FOUND, "route_not_found");
    };
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string()
    };
    let verified = service::verify_webhook_signature(
        &secret,
        &header("x-webhook-timestamp"),
        &body,
        &header("x-webhook-signature"),
        now_ms(),
    );
    if let Err(code) = verified {
        return crate::http::error_response(StatusCode::UNAUTHORIZED, code);
    }
    let event = header("x-webhook-event");
    tracing::info!(event = %event, "stockforge webhook received; kicking sync");
    worker::kick_sync_soon(state);
    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "accepted": true })),
    )
        .into_response()
}

async fn status(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    let has_synced = {
        let persistence = state.persistence.lock();
        let conn = persistence.connection_ref();
        store::ALL_ENTITIES.iter().any(|entity| {
            store::get_cursor(conn, &state.client_id, entity)
                .map(|cursor| cursor.last_advanced_at_ms.is_some())
                .unwrap_or(false)
        })
    };
    Json(service::connector_status(has_synced)).into_response()
}

/// Kick one sync cycle on a background thread. 202 when claimed; 409 with the
/// reason when a sync is running or the cooldown hasn't passed.
async fn sync_now(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    if service::connector_config_from_env().is_none() {
        return (
            StatusCode::CONFLICT,
            Json(InventorySyncNowResponse {
                accepted: false,
                reason: Some("stockforge_not_configured".to_string()),
                next_allowed_at_ms: 0,
            }),
        )
            .into_response();
    }
    let now = now_ms();
    if let Err(reason) = worker::try_begin_sync(&state, now) {
        let next_allowed_at_ms = state
            .sync_guards
            .guard(crate::http::Pump::Stockforge)
            .lock()
            .next_allowed_at_ms;
        return (
            StatusCode::CONFLICT,
            Json(InventorySyncNowResponse {
                accepted: false,
                reason: Some(reason.to_string()),
                next_allowed_at_ms,
            }),
        )
            .into_response();
    }
    let max_requests = {
        let persistence = state.persistence.lock();
        match worker::max_requests_from_settings(persistence.connection_ref(), &state.client_id) {
            Ok(max_requests) => max_requests,
            Err(err) => return crate::http::store_error_response("inventory", err),
        }
    };
    let task_state = state.clone();
    std::thread::Builder::new()
        .name("stockforge-sync-now".to_string())
        .spawn(move || {
            if let Err(err) = worker::run_guarded_cycle(&task_state, max_requests) {
                tracing::warn!(error = %err, "manual stockforge sync failed");
            }
        })
        .ok();
    (
        StatusCode::ACCEPTED,
        Json(InventorySyncNowResponse {
            accepted: true,
            reason: None,
            next_allowed_at_ms: now + worker::STOCKFORGE_SYNC_COOLDOWN_MS,
        }),
    )
        .into_response()
}

fn sync_info(
    conn: &rusqlite::Connection,
    client_id: &str,
    status: &SyncGuard,
) -> Result<InventorySyncInfo, StoreError> {
    let (material_count, order_count) = store::snapshot_counts(conn, client_id)?;
    let mut backfill_complete = true;
    let mut last_synced_at_ms = None;
    let mut last_error = None;
    let mut last_error_class = None;
    let mut last_error_at_ms = None;
    for entity in store::ALL_ENTITIES {
        let cursor = store::get_cursor(conn, client_id, entity)?;
        backfill_complete &= cursor.backfill_complete;
        last_synced_at_ms = last_synced_at_ms.max(cursor.last_advanced_at_ms);
        if cursor.last_error.is_some()
            && cursor.last_error_at_ms.unwrap_or(0) >= last_error_at_ms.unwrap_or(0)
        {
            last_error = cursor.last_error;
            last_error_class = cursor.last_error_class;
            last_error_at_ms = cursor.last_error_at_ms;
        }
    }
    Ok(InventorySyncInfo {
        sync_enabled: crate::slices::admin_settings::service::flag(
            conn,
            client_id,
            &crate::env_registry::BOS_STOCKFORGE_SYNC_ENABLED,
        )?,
        in_flight: status.in_flight,
        backfill_complete,
        last_synced_at_ms: last_synced_at_ms.max(status.last_success_ms),
        material_count,
        order_count,
        last_requests_used: status.units_used,
        next_sync_allowed_at_ms: status.next_allowed_at_ms,
        last_error,
        last_error_class,
        last_error_at_ms,
    })
}

async fn stock(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    let sync_status = state
        .sync_guards
        .guard(crate::http::Pump::Stockforge)
        .lock()
        .clone();
    let persistence = state.persistence.lock();
    let conn = persistence.connection_ref();
    let listed = store::list_materials(conn, &state.client_id).and_then(|materials| {
        Ok((
            materials,
            store::list_alerts(conn, &state.client_id)?,
            store::list_reorder_suggestions(conn, &state.client_id)?,
            store::list_orders(conn, &state.client_id)?,
            store::list_purchase_orders(conn, &state.client_id)?,
            store::get_cursor(conn, &state.client_id, store::ENTITY_ORDER)?,
            store::get_cursor(conn, &state.client_id, store::ENTITY_PO)?,
            sync_info(conn, &state.client_id, &sync_status)?,
        ))
    });
    match listed {
        Ok((materials, alert_rows, reorders, orders, pos, order_cursor, po_cursor, sync)) => {
            let (kpis, mut rows) = service::compute_stock(&materials, &alert_rows);
            let history = service::stock_history_evidence(
                &orders,
                &pos,
                order_cursor.backfill_complete
                    && order_cursor.last_advanced_at_ms.is_some()
                    && order_cursor.last_error.is_none(),
                po_cursor.backfill_complete
                    && po_cursor.last_advanced_at_ms.is_some()
                    && po_cursor.last_error.is_none(),
            );
            service::enrich_stock_rows(&mut rows, &reorders, &history);
            Json(InventoryStockResponse {
                kpis,
                materials: rows,
                sync,
            })
            .into_response()
        }
        Err(err) => store_error_response(err),
    }
}

async fn alerts(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    let sync_status = state
        .sync_guards
        .guard(crate::http::Pump::Stockforge)
        .lock()
        .clone();
    let persistence = state.persistence.lock();
    let conn = persistence.connection_ref();
    let listed = store::list_alerts(conn, &state.client_id).and_then(|alert_rows| {
        Ok((
            alert_rows,
            store::list_reorder_suggestions(conn, &state.client_id)?,
            store::list_materials(conn, &state.client_id)?,
            sync_info(conn, &state.client_id, &sync_status)?,
        ))
    });
    match listed {
        Ok((alert_rows, reorder_rows, materials, sync)) => Json(InventoryAlertsResponse {
            alerts: service::alert_rows(&alert_rows, &materials),
            reorder_suggestions: service::reorder_rows(&reorder_rows, &materials),
            sync,
        })
        .into_response(),
        Err(err) => store_error_response(err),
    }
}

async fn orders(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    let sync_status = state
        .sync_guards
        .guard(crate::http::Pump::Stockforge)
        .lock()
        .clone();
    let persistence = state.persistence.lock();
    let conn = persistence.connection_ref();
    let today = service::today_string(now_ms());
    let listed = store::list_orders(conn, &state.client_id)
        .and_then(|snapshots| Ok((snapshots, sync_info(conn, &state.client_id, &sync_status)?)));
    match listed {
        Ok((snapshots, sync)) => {
            let (pipeline, controls, rows) = service::compute_orders(&snapshots, &today);
            Json(InventoryOrdersResponse {
                pipeline,
                controls,
                orders: rows,
                window_days: service::ORDER_WINDOW_DAYS,
                sync,
            })
            .into_response()
        }
        Err(err) => store_error_response(err),
    }
}

async fn purchase_orders(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    let sync_status = state
        .sync_guards
        .guard(crate::http::Pump::Stockforge)
        .lock()
        .clone();
    let persistence = state.persistence.lock();
    let conn = persistence.connection_ref();
    let listed = store::list_purchase_orders(conn, &state.client_id)
        .and_then(|snapshots| Ok((snapshots, sync_info(conn, &state.client_id, &sync_status)?)));
    match listed {
        Ok((snapshots, sync)) => {
            let (rows, open_total_cents) = service::open_purchase_orders(&snapshots);
            Json(InventoryPurchaseOrdersResponse {
                purchase_orders: rows,
                open_total_cents,
                sync,
            })
            .into_response()
        }
        Err(err) => store_error_response(err),
    }
}

fn store_error_response(err: StoreError) -> Response {
    crate::http::store_error_response("inventory", err)
}
