//! Thin HTTP handlers for the QBO connector + cached views. Every GET view
//! serves from the local snapshot cache — these routes NEVER call QBO. Only
//! the connect/callback pair and the (guarded) Sync-now kickoff touch Intuit.

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bos_contracts::accounting::{
    AccountingAgingResponse, AccountingCustomersResponse, AccountingInvoicesResponse,
    AccountingSyncInfo, AccountingSyncNowResponse,
};
use serde::Deserialize;

use super::service;
use super::store;
use super::worker;
use crate::http::{error_response, now_ms, AppState, OperatorScope, SyncGuard};
use crate::store_core::StoreError;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/accounting/status", get(status))
        .route("/api/connectors/qbo/connect", get(connect))
        .route("/api/connectors/qbo/callback", get(callback))
        .route("/api/connectors/qbo/disconnect", post(disconnect))
        .route("/api/accounting/sync", post(sync_now))
        .route("/api/accounting/invoices", get(invoices))
        .route("/api/accounting/aging", get(aging))
        .route("/api/accounting/financials", get(financials))
        .route("/api/accounting/customers", get(customers))
}

async fn status(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let scope = match state.require_scope(&headers) {
        Ok(scope) => scope,
        Err(denied) => return *denied,
    };
    let persistence = state.persistence.lock();
    let conn = persistence.connection_ref();
    if let Err(denied) = require_financial_visibility(
        conn,
        &state.client_id,
        &scope,
        state.accounting_visibility_policy,
    ) {
        return *denied;
    }
    match service::connector_status(conn, &state.client_id) {
        Ok(status) => Json(status).into_response(),
        Err(err) => store_error_response(err),
    }
}

#[derive(Debug, Deserialize)]
struct ConnectQuery {
    /// Operator token may arrive as a query param because this URL is opened
    /// in a browser tab, where setting an Authorization header is impractical.
    #[serde(default)]
    token: Option<String>,
}

async fn connect(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ConnectQuery>,
) -> Response {
    // The credential is client-wide, but the CSRF state still binds WHO is
    // connecting — they become the receipt actor.
    let identity =
        match state.authenticate_operator_or_query_token(&headers, query.token.as_deref()) {
            Ok(identity) => identity,
            Err(denied) => return *denied,
        };
    {
        let persistence = state.persistence.lock();
        if let Err(denied) = require_financial_visibility(
            persistence.connection_ref(),
            &state.client_id,
            &identity.scope(),
            state.accounting_visibility_policy,
        ) {
            return *denied;
        }
    }
    let Some(app) = service::oauth_app_from_env() else {
        return error_response(StatusCode::CONFLICT, "oauth_app_unconfigured");
    };
    let csrf_state = crate::slices::google_connector::service::generate_state();
    if let Err(err) = state.register_oauth_state("qbo", &csrf_state, &identity.actor_id) {
        return store_error_response(err);
    }
    Redirect::temporary(&service::consent_url(&app, &csrf_state)).into_response()
}

#[derive(Debug, Deserialize)]
struct CallbackQuery {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    state: Option<String>,
    /// Intuit appends the connected company id to the redirect.
    #[serde(default, rename = "realmId")]
    realm_id: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

async fn callback(State(state): State<AppState>, Query(query): Query<CallbackQuery>) -> Response {
    if let Some(err) = query.error {
        return Html(format!(
            "<h2>QuickBooks connect failed</h2><p>{}</p>",
            err.replace('<', "&lt;")
        ))
        .into_response();
    }
    let (Some(code), Some(csrf_state), Some(realm_id)) = (query.code, query.state, query.realm_id)
    else {
        return error_response(StatusCode::BAD_REQUEST, "oauth_callback_missing_params");
    };
    // Single-use: consume validates AND removes; carries who connected.
    let user_id = match state.consume_oauth_state("qbo", &csrf_state) {
        Ok(Some(user_id)) => user_id,
        Ok(None) => return error_response(StatusCode::BAD_REQUEST, "oauth_state_invalid"),
        Err(err) => return store_error_response(err),
    };
    let Some(app) = service::oauth_app_from_env() else {
        return error_response(StatusCode::CONFLICT, "oauth_app_unconfigured");
    };
    let environment = app.environment;
    let redirect_uri = service::redirect_uri();
    let exchange = tokio::task::spawn_blocking(move || {
        bos_integrations::qbo_oauth::exchange_authorization_code(
            &app,
            &redirect_uri,
            &code,
            now_ms(),
        )
    })
    .await;
    let grant = match exchange {
        Ok(Ok(grant)) => grant,
        Ok(Err(err)) => {
            tracing::warn!(error = %err, "qbo oauth code exchange failed");
            return error_response(StatusCode::BAD_GATEWAY, "oauth_code_exchange_failed");
        }
        Err(_join) => {
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, "oauth_task_failed")
        }
    };
    let mut persistence = state.persistence.lock();
    let stored = store::store_credential(
        persistence.connection(),
        &state.client_id,
        &realm_id,
        environment.as_str(),
        &grant,
        &user_id,
        now_ms(),
    );
    match stored {
        Ok(_) => crate::http::connector_connected_page(
            "QuickBooks",
            "Your accounting is linked. Use \"Sync now\" in the Accounting tab to pull the first snapshot.",
        ),
        Err(err) => store_error_response(err),
    }
}

#[derive(Debug, Default, Deserialize)]
struct DisconnectRequest {
    /// Also delete every cached snapshot/cursor row (e.g. leaving a test
    /// company behind). Same receipted transaction as the credential delete.
    #[serde(default)]
    purge: bool,
}

async fn disconnect(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Option<Json<DisconnectRequest>>,
) -> Response {
    let identity = match state.authenticate_operator(&headers) {
        Ok(identity) => identity,
        Err(denied) => return *denied,
    };
    let purge = body.map(|Json(request)| request.purge).unwrap_or(false);
    let mut persistence = state.persistence.lock();
    if let Err(denied) = require_financial_visibility(
        persistence.connection_ref(),
        &state.client_id,
        &identity.scope(),
        state.accounting_visibility_policy,
    ) {
        return *denied;
    }
    match store::delete_credential(
        persistence.connection(),
        &state.client_id,
        &identity.actor_id,
        purge,
        now_ms(),
    ) {
        Ok(_) => Json(serde_json::json!({"disconnected": true, "purged": purge})).into_response(),
        Err(err) => store_error_response(err),
    }
}

/// Kick one sync cycle on a background thread. 202 when claimed; 409 with the
/// reason when a sync is running or the cooldown hasn't passed.
async fn sync_now(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let scope = match state.require_scope(&headers) {
        Ok(scope) => scope,
        Err(denied) => return *denied,
    };
    let connected = {
        let persistence = state.persistence.lock();
        let conn = persistence.connection_ref();
        if let Err(denied) = require_financial_visibility(
            conn,
            &state.client_id,
            &scope,
            state.accounting_visibility_policy,
        ) {
            return *denied;
        }
        service::connector_status(conn, &state.client_id)
    };
    match connected {
        Ok(status) if status.reconnect_required => {
            return (
                StatusCode::CONFLICT,
                Json(AccountingSyncNowResponse {
                    accepted: false,
                    reason: Some(
                        status
                            .connection_error_code
                            .unwrap_or_else(|| "qbo_token_rejected".to_string()),
                    ),
                    next_allowed_at_ms: 0,
                }),
            )
                .into_response()
        }
        Ok(status) if status.connected => {}
        Ok(_) => {
            return (
                StatusCode::CONFLICT,
                Json(AccountingSyncNowResponse {
                    accepted: false,
                    reason: Some("accounting_not_connected".to_string()),
                    next_allowed_at_ms: 0,
                }),
            )
                .into_response()
        }
        Err(err) => return store_error_response(err),
    }
    let max_requests = {
        let persistence = state.persistence.lock();
        match worker::max_requests_from_settings(persistence.connection_ref(), &state.client_id) {
            Ok(max_requests) => max_requests,
            Err(err) => return store_error_response(err),
        }
    };
    let now = now_ms();
    if let Err(reason) = worker::try_begin_sync(&state, now) {
        let next_allowed_at_ms = state
            .sync_guards
            .guard(crate::http::Pump::Accounting)
            .lock()
            .next_allowed_at_ms;
        return (
            StatusCode::CONFLICT,
            Json(AccountingSyncNowResponse {
                accepted: false,
                reason: Some(reason.to_string()),
                next_allowed_at_ms,
            }),
        )
            .into_response();
    }
    let task_state = state.clone();
    std::thread::Builder::new()
        .name("qbo-sync-now".to_string())
        .spawn(move || {
            if let Err(err) = worker::run_guarded_cycle(&task_state, max_requests) {
                tracing::warn!(error = %err, "manual qbo sync failed");
            }
        })
        .ok();
    (
        StatusCode::ACCEPTED,
        Json(AccountingSyncNowResponse {
            accepted: true,
            reason: None,
            next_allowed_at_ms: now + worker::ACCOUNTING_SYNC_COOLDOWN_MS,
        }),
    )
        .into_response()
}

fn sync_info(
    conn: &rusqlite::Connection,
    client_id: &str,
    status: &SyncGuard,
) -> Result<AccountingSyncInfo, StoreError> {
    service::sync_info(conn, client_id, status)
}

#[derive(Debug, Deserialize)]
struct InvoicesQuery {
    /// open | overdue | all (default all).
    #[serde(default)]
    filter: Option<String>,
}

async fn invoices(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<InvoicesQuery>,
) -> Response {
    let scope = match state.require_scope(&headers) {
        Ok(scope) => scope,
        Err(denied) => return *denied,
    };
    let sync_status = state
        .sync_guards
        .guard(crate::http::Pump::Accounting)
        .lock()
        .clone();
    let persistence = state.persistence.lock();
    let conn = persistence.connection_ref();
    if let Err(denied) = require_cached_financial_visibility(
        conn,
        &state.client_id,
        &scope,
        state.accounting_visibility_policy,
    ) {
        return *denied;
    }
    let today = service::today_string(now_ms());
    let listed = store::list_invoices(conn, &state.client_id, 500)
        .and_then(|snapshots| Ok((snapshots, sync_info(conn, &state.client_id, &sync_status)?)));
    match listed {
        Ok((snapshots, sync)) => {
            let filter = query.filter.as_deref().unwrap_or("all");
            let invoices = snapshots
                .iter()
                .map(|snapshot| service::invoice_row(snapshot, &today))
                .filter(|row| match filter {
                    "open" => row.status == "open" || row.status == "overdue",
                    "overdue" => row.status == "overdue",
                    _ => true,
                })
                .collect();
            Json(AccountingInvoicesResponse { invoices, sync }).into_response()
        }
        Err(err) => store_error_response(err),
    }
}

async fn aging(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let scope = match state.require_scope(&headers) {
        Ok(scope) => scope,
        Err(denied) => return *denied,
    };
    let sync_status = state
        .sync_guards
        .guard(crate::http::Pump::Accounting)
        .lock()
        .clone();
    let persistence = state.persistence.lock();
    let conn = persistence.connection_ref();
    if let Err(denied) = require_cached_financial_visibility(
        conn,
        &state.client_id,
        &scope,
        state.accounting_visibility_policy,
    ) {
        return *denied;
    }
    let today = service::today_string(now_ms());
    let listed = store::list_invoices(conn, &state.client_id, 10_000)
        .and_then(|snapshots| Ok((snapshots, sync_info(conn, &state.client_id, &sync_status)?)));
    match listed {
        Ok((snapshots, sync)) => {
            let buckets = service::compute_aging(&snapshots, &today);
            let total_open_cents = buckets.iter().map(|bucket| bucket.balance_cents).sum();
            Json(AccountingAgingResponse {
                buckets,
                total_open_cents,
                sync,
            })
            .into_response()
        }
        Err(err) => store_error_response(err),
    }
}

async fn financials(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let scope = match state.require_scope(&headers) {
        Ok(scope) => scope,
        Err(denied) => return *denied,
    };
    let sync_status = state
        .sync_guards
        .guard(crate::http::Pump::Accounting)
        .lock()
        .clone();
    let persistence = state.persistence.lock();
    let conn = persistence.connection_ref();
    if let Err(denied) = require_cached_financial_visibility(
        conn,
        &state.client_id,
        &scope,
        state.accounting_visibility_policy,
    ) {
        return *denied;
    }
    let today = service::today_string(now_ms());
    let metric_config = match service::metric_basis_config_from_sources(Some(
        &state.accounting_overlay.metric_basis,
    )) {
        Ok(config) => config,
        Err(err) => return store_error_response(err),
    };
    let assembled = sync_info(conn, &state.client_id, &sync_status).and_then(|sync| {
        service::financials_from_store(conn, &state.client_id, &today, sync, &metric_config)
    });
    match assembled {
        Ok(response) => Json(response).into_response(),
        Err(err) => store_error_response(err),
    }
}

async fn customers(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let scope = match state.require_scope(&headers) {
        Ok(scope) => scope,
        Err(denied) => return *denied,
    };
    let sync_status = state
        .sync_guards
        .guard(crate::http::Pump::Accounting)
        .lock()
        .clone();
    let persistence = state.persistence.lock();
    let conn = persistence.connection_ref();
    if let Err(denied) = require_financial_visibility(
        conn,
        &state.client_id,
        &scope,
        state.accounting_visibility_policy,
    ) {
        return *denied;
    }
    let listed = store::list_customers(conn, &state.client_id)
        .and_then(|snapshots| Ok((snapshots, sync_info(conn, &state.client_id, &sync_status)?)));
    match listed {
        Ok((snapshots, sync)) => Json(AccountingCustomersResponse {
            customers: snapshots.iter().map(service::customer_row).collect(),
            sync,
        })
        .into_response(),
        Err(err) => store_error_response(err),
    }
}

fn store_error_response(err: StoreError) -> Response {
    crate::http::store_error_response("qbo_views", err)
}

fn require_financial_visibility(
    conn: &rusqlite::Connection,
    client_id: &str,
    scope: &OperatorScope,
    policy: crate::overlay::AccountingVisibilityPolicy,
) -> Result<(), Box<Response>> {
    match service::financial_visibility_allowed(conn, client_id, scope, policy) {
        Ok(true) => Ok(()),
        Ok(false) => Err(Box::new(error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "qbo_financial_scope_forbidden",
        ))),
        Err(err) => Err(Box::new(store_error_response(err))),
    }
}

fn require_cached_financial_visibility(
    conn: &rusqlite::Connection,
    client_id: &str,
    scope: &OperatorScope,
    policy: crate::overlay::AccountingVisibilityPolicy,
) -> Result<(), Box<Response>> {
    match service::cached_financial_visibility_allowed(conn, client_id, scope, policy) {
        Ok(true) => Ok(()),
        Ok(false) => Err(Box::new(error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "qbo_financial_scope_forbidden",
        ))),
        Err(err) => Err(Box::new(store_error_response(err))),
    }
}
