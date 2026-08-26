use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bos_contracts::admin_settings::{AdminSettingClearRequest, AdminSettingUpdateRequest};

use super::service;
use crate::http::{error_response, mutation_response, now_ms, store_error_response, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/settings", get(settings))
        .route(
            "/api/admin/settings/{var_name}",
            post(update_setting).delete(clear_setting),
        )
}

async fn settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(denied) = state.require_all_scope(&headers) {
        return *denied;
    }
    let persistence = state.persistence.lock();
    let mut overlay_values = vec![service::OverlayRuntimeValue {
        var_name: crate::env_registry::BOS_ACCOUNTING_VISIBILITY_POLICY.name,
        value: state.accounting_visibility_policy.as_str().into(),
    }];
    if let Some(mapping_json) = crate::slices::customer_tier_sync::service::overlay_mapping_json(
        &state.customer_tier_sync_overlay,
    ) {
        overlay_values.push(service::OverlayRuntimeValue {
            var_name: crate::env_registry::BOS_SHOPIFY_TIER_MAPPING_JSON.name,
            value: mapping_json.into(),
        });
    }
    match service::settings_response_with_overlay(
        persistence.connection_ref(),
        &state.client_id,
        overlay_values.as_slice(),
    ) {
        Ok(response) => Json(response).into_response(),
        Err(err) => store_error_response("admin_settings", err),
    }
}

async fn update_setting(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(var_name): Path<String>,
    Json(request): Json<AdminSettingUpdateRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if let Err(denied) = auth.require_all_scope() {
        return *denied;
    }
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let mut persistence = state.persistence.lock();
    match service::upsert_setting(
        persistence.connection(),
        &state.client_id,
        &actor_id,
        &var_name,
        &request,
        now_ms(),
    ) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response("admin_settings", err),
    }
}

async fn clear_setting(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(var_name): Path<String>,
    Json(request): Json<AdminSettingClearRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if let Err(denied) = auth.require_all_scope() {
        return *denied;
    }
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let mut persistence = state.persistence.lock();
    match service::clear_setting(
        persistence.connection(),
        &state.client_id,
        &actor_id,
        &var_name,
        &request,
        now_ms(),
    ) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response("admin_settings", err),
    }
}
