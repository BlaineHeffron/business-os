//! Thin HTTP handlers for the configurable Home dashboard.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bos_contracts::home_dashboard::{
    HomeDashboardPreferencesUpdateRequest, HubSpotDealPipelineMappingSaveRequest,
};

use crate::http::{error_response, mutation_response, now_ms, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/home-dashboard", get(dashboard))
        .route("/api/home-dashboard/preferences", post(update_preferences))
        .route(
            "/api/home-dashboard/hubspot-deals/discovery",
            get(hubspot_deal_discovery),
        )
        .route(
            "/api/home-dashboard/hubspot-deals/mapping",
            get(hubspot_deal_mapping).post(save_hubspot_deal_mapping),
        )
}

async fn dashboard(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let identity = match state.authenticate_operator(&headers) {
        Ok(identity) => identity,
        Err(denied) => return *denied,
    };
    match super::service::dashboard_response(&state, &identity.scope(), &identity.actor_id) {
        Ok(response) => Json(response).into_response(),
        Err(err) => crate::http::store_error_response("home_dashboard", err),
    }
}

async fn hubspot_deal_discovery(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(denied) = state.authenticate_operator(&headers) {
        return *denied;
    }
    Json(super::service::discover_hubspot_deals()).into_response()
}

async fn hubspot_deal_mapping(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(denied) = state.authenticate_operator(&headers) {
        return *denied;
    }
    let persistence = state.persistence();
    match super::store::load_hubspot_deal_mapping(persistence.connection_ref(), &state.client_id) {
        Ok(response) => Json(response).into_response(),
        Err(err) => crate::http::store_error_response("home_dashboard", err),
    }
}

async fn save_hubspot_deal_mapping(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<HubSpotDealPipelineMappingSaveRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let mut persistence = state.persistence();
    match super::store::save_hubspot_deal_mapping(
        persistence.connection(),
        &state.client_id,
        &actor_id,
        &request,
        now_ms(),
    ) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => crate::http::store_error_response("home_dashboard", err),
    }
}

async fn update_preferences(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<HomeDashboardPreferencesUpdateRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let user_id = auth.identity.actor_id;
    let mut persistence = state.persistence();
    match super::store::replace_preference(
        persistence.connection(),
        &state.client_id,
        &user_id,
        &actor_id,
        &request,
        now_ms(),
    ) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => crate::http::store_error_response("home_dashboard", err),
    }
}
