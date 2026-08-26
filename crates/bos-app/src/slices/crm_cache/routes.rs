use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bos_contracts::crm_cache::{
    CrmCacheSyncNowResponse, CrmContactSnapshotsResponse, CrmDealSnapshotsResponse,
};
use serde::Deserialize;

use super::{service, worker};
use crate::http::{error_response, now_ms, AppState, OperatorScope};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/crm-cache/status", get(status))
        .route("/api/crm-cache/contacts", get(contacts))
        .route("/api/crm-cache/deals", get(deals))
        .route("/api/crm-cache/context", get(context))
        .route("/api/crm-cache/sync", post(sync_now))
}

async fn status(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(denied) = state.require_scope(&headers) {
        return *denied;
    }
    let sync_status = state
        .sync_guards
        .guard(crate::http::Pump::CrmCache)
        .lock()
        .clone();
    let persistence = state.persistence.lock();
    match service::sync_info(persistence.connection_ref(), &state.client_id, &sync_status) {
        Ok(info) => Json(info).into_response(),
        Err(err) => crate::http::store_error_response("crm_cache", err),
    }
}

#[derive(Debug, Default, Deserialize)]
struct ContactsQuery {
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    company: Option<String>,
}

async fn contacts(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ContactsQuery>,
) -> Response {
    let scope = match state.require_scope(&headers) {
        Ok(scope) => scope,
        Err(denied) => return *denied,
    };
    let email = query
        .email
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let company = query
        .company
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let persistence = state.persistence.lock();
    let conn = persistence.connection_ref();
    let result = match (email, company) {
        (Some(email), None) => service::contacts_by_email(conn, &state.client_id, &scope, email),
        (None, Some(company)) => {
            service::contact_by_company(conn, &state.client_id, &scope, company)
        }
        _ => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "crm_cache_contact_lookup_requires_one_key",
            )
        }
    };
    match result {
        Ok(contacts) => Json(CrmContactSnapshotsResponse { contacts }).into_response(),
        Err(err) => crate::http::store_error_response("crm_cache", err),
    }
}

#[derive(Debug, Default, Deserialize)]
struct DealsQuery {
    #[serde(default)]
    contact_email: Option<String>,
}

async fn deals(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<DealsQuery>,
) -> Response {
    let scope = match state.require_scope(&headers) {
        Ok(scope) => scope,
        Err(denied) => return *denied,
    };
    let Some(contact_email) = query
        .contact_email
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "crm_cache_deal_lookup_requires_contact_email",
        );
    };
    let persistence = state.persistence.lock();
    match service::deals_by_contact(
        persistence.connection_ref(),
        &state.client_id,
        &scope,
        contact_email,
    ) {
        Ok(deals) => Json(CrmDealSnapshotsResponse { deals }).into_response(),
        Err(err) => crate::http::store_error_response("crm_cache", err),
    }
}

#[derive(Debug, Default, Deserialize)]
struct ContextQuery {
    #[serde(default)]
    source_key: Option<String>,
}

async fn context(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ContextQuery>,
) -> Response {
    let scope = match state.require_scope(&headers) {
        Ok(scope) => scope,
        Err(denied) => return *denied,
    };
    let Some(source_key) = query
        .source_key
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "crm_cache_context_requires_source_key",
        );
    };
    let persistence = state.persistence.lock();
    match service::context_for_source(
        persistence.connection_ref(),
        &state.client_id,
        &scope,
        source_key,
    ) {
        Ok(context) => Json(context).into_response(),
        Err(err) => crate::http::store_error_response("crm_cache", err),
    }
}

async fn sync_now(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let scope = match state.require_scope(&headers) {
        Ok(scope) => scope,
        Err(denied) => return *denied,
    };
    if !matches!(scope, OperatorScope::All) {
        return error_response(StatusCode::UNPROCESSABLE_ENTITY, "crm_cache_admin_only");
    }
    let (max_requests, refresh_interval) = {
        let persistence = state.persistence.lock();
        match worker::config_from_settings(persistence.connection_ref(), &state.client_id) {
            Ok(config) => (config.max_requests_per_cycle, config.interval),
            Err(err) => return crate::http::store_error_response("crm_cache", err),
        }
    };
    let now = now_ms();
    if let Err(reason) = worker::try_begin_sync(&state, now) {
        let status = state
            .sync_guards
            .guard(crate::http::Pump::CrmCache)
            .lock()
            .clone();
        return (
            StatusCode::CONFLICT,
            Json(CrmCacheSyncNowResponse {
                accepted: false,
                reason: Some(reason.to_string()),
                next_allowed_at_ms: status.next_allowed_at_ms,
            }),
        )
            .into_response();
    }
    let task_state = state.clone();
    std::thread::Builder::new()
        .name("crm-cache-sync-now".to_string())
        .spawn(move || {
            if let Err(err) =
                worker::run_guarded_cycle(&task_state, max_requests, true, refresh_interval)
            {
                tracing::warn!(error = %err, "manual CRM cache sync failed");
            }
        })
        .ok();
    (
        StatusCode::ACCEPTED,
        Json(CrmCacheSyncNowResponse {
            accepted: true,
            reason: None,
            next_allowed_at_ms: now + worker::CRM_CACHE_SYNC_COOLDOWN_MS,
        }),
    )
        .into_response()
}
