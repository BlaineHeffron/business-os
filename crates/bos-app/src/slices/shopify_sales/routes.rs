//! Thin HTTP handlers for Shopify sales cached views.

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bos_contracts::shopify_sales::{
    ShopifyCustomersResponse, ShopifyOrdersResponse, ShopifySalesSyncInfo,
    ShopifySalesSyncNowResponse,
};

use super::{service, store, worker};
use crate::http::{now_ms, AppState, SyncGuard};
use crate::store_core::StoreError;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/shopify-sales/status", get(status))
        .route("/api/shopify-sales/sync", post(sync_now))
        .route("/api/shopify-sales/orders", get(orders))
        .route("/api/shopify-sales/customers", get(customers))
}

#[derive(Debug, serde::Deserialize)]
struct OrdersQuery {
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize)]
struct CustomersQuery {
    email: String,
    #[serde(default)]
    limit: Option<usize>,
}

async fn status(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    let has_synced = {
        let persistence = state.persistence.lock();
        let conn = persistence.connection_ref();
        store::get_sync_state(conn, &state.client_id)
            .map(|cursor| cursor.last_advanced_at_ms.is_some())
            .unwrap_or(false)
    };
    Json(service::connector_status(has_synced)).into_response()
}

async fn sync_now(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    if !service::connector_config_present_from_env() {
        return (
            StatusCode::CONFLICT,
            Json(ShopifySalesSyncNowResponse {
                accepted: false,
                reason: Some("shopify_not_configured".to_string()),
                next_allowed_at_ms: 0,
            }),
        )
            .into_response();
    }
    let now = now_ms();
    if let Err(reason) = worker::try_begin_sync(&state, now) {
        let next_allowed_at_ms = state
            .sync_guards
            .guard(crate::http::Pump::ShopifySales)
            .lock()
            .next_allowed_at_ms;
        return (
            StatusCode::CONFLICT,
            Json(ShopifySalesSyncNowResponse {
                accepted: false,
                reason: Some(reason.to_string()),
                next_allowed_at_ms,
            }),
        )
            .into_response();
    }
    let max_orders = {
        let persistence = state.persistence.lock();
        match worker::max_orders_from_settings(persistence.connection_ref(), &state.client_id) {
            Ok(max_orders) => max_orders,
            Err(err) => return crate::http::store_error_response("shopify_sales", err),
        }
    };
    let task_state = state.clone();
    if let Err(err) = std::thread::Builder::new()
        .name("shopify-sales-sync-now".to_string())
        .spawn(move || {
            if let Err(err) = worker::run_guarded_cycle(&task_state, max_orders) {
                tracing::warn!(error = %err, "manual shopify sales sync failed");
            }
        })
    {
        let mut status = state
            .sync_guards
            .guard(crate::http::Pump::ShopifySales)
            .lock();
        status.in_flight = false;
        status.last_outcome = Some(format!("error: spawn_failed: {err}"));
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ShopifySalesSyncNowResponse {
                accepted: false,
                reason: Some("spawn_failed".to_string()),
                next_allowed_at_ms: status.next_allowed_at_ms,
            }),
        )
            .into_response();
    }
    (
        StatusCode::ACCEPTED,
        Json(ShopifySalesSyncNowResponse {
            accepted: true,
            reason: None,
            next_allowed_at_ms: now + worker::SHOPIFY_SALES_SYNC_COOLDOWN_MS,
        }),
    )
        .into_response()
}

async fn orders(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<OrdersQuery>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    let sync_status = state
        .sync_guards
        .guard(crate::http::Pump::ShopifySales)
        .lock()
        .clone();
    let persistence = state.persistence.lock();
    let conn = persistence.connection_ref();
    let policy = match service::visibility_policy_from_settings(conn, &state.client_id) {
        Ok(policy) => policy,
        Err(err) => return store_error_response(err),
    };
    let financial_visible = service::financial_visible(&auth.scope, policy);
    let limit = query.limit.unwrap_or(50).clamp(1, 250);
    let listed = match query
        .email
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        Some(email) => store::orders_by_customer(
            conn,
            &state.client_id,
            &auth.scope,
            financial_visible,
            email,
            limit,
        ),
        None => store::list_recent_orders(
            conn,
            &state.client_id,
            &auth.scope,
            financial_visible,
            limit,
        ),
    }
    .and_then(|orders| Ok((orders, sync_info(conn, &state.client_id, &sync_status)?)));
    match listed {
        Ok((orders, sync)) => Json(ShopifyOrdersResponse {
            orders: orders.iter().map(service::order_row).collect(),
            sync,
        })
        .into_response(),
        Err(err) => store_error_response(err),
    }
}

async fn customers(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<CustomersQuery>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    let sync_status = state
        .sync_guards
        .guard(crate::http::Pump::ShopifySales)
        .lock()
        .clone();
    let persistence = state.persistence.lock();
    let conn = persistence.connection_ref();
    let policy = match service::visibility_policy_from_settings(conn, &state.client_id) {
        Ok(policy) => policy,
        Err(err) => return store_error_response(err),
    };
    let financial_visible = service::financial_visible(&auth.scope, policy);
    let listed = store::customers_by_email(
        conn,
        &state.client_id,
        &auth.scope,
        financial_visible,
        &query.email,
        query.limit.unwrap_or(20).clamp(1, 100),
    )
    .and_then(|customers| Ok((customers, sync_info(conn, &state.client_id, &sync_status)?)));
    match listed {
        Ok((customers, sync)) => Json(ShopifyCustomersResponse {
            customers: customers.iter().map(service::customer_row).collect(),
            sync,
        })
        .into_response(),
        Err(err) => store_error_response(err),
    }
}

fn sync_info(
    conn: &rusqlite::Connection,
    client_id: &str,
    status: &SyncGuard,
) -> Result<ShopifySalesSyncInfo, StoreError> {
    let (order_count, customer_count) = store::snapshot_counts(conn, client_id)?;
    let sync_state = store::get_sync_state(conn, client_id)?;
    Ok(ShopifySalesSyncInfo {
        sync_enabled: crate::slices::admin_settings::service::flag(
            conn,
            client_id,
            &crate::env_registry::BOS_SHOPIFY_READ_SYNC_ENABLED,
        )?,
        in_flight: status.in_flight,
        backfill_complete: sync_state.backfill_complete,
        last_synced_at_ms: sync_state.last_advanced_at_ms.max(status.last_attempt_ms),
        order_count,
        customer_count,
        last_requests_used: status.units_used,
        next_sync_allowed_at_ms: status.next_allowed_at_ms,
        last_error: sync_state.last_error,
    })
}

fn store_error_response(err: StoreError) -> Response {
    crate::http::store_error_response("shopify_sales", err)
}
