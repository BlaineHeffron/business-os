//! Thin HTTP handlers for the diagnostics surface.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};

use super::service;
use crate::http::{now_ms, AppState};

pub fn router() -> Router<AppState> {
    Router::new().route("/api/diagnostics/health", get(health))
}

/// Unauthenticated structured liveness. Mounted in the CORE router (not the
/// slice router) so it answers even when the slice is disabled by overlay.
pub async fn readyz(State(state): State<AppState>) -> Response {
    match service::readyz(&state, now_ms()) {
        Ok(readyz) => Json(readyz).into_response(),
        Err(err) => crate::http::store_error_response("instance_diagnostics", err),
    }
}

async fn health(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    match service::health(&state, &auth.scope, &auth.actor_id, now_ms()) {
        Ok(health) => Json(health).into_response(),
        Err(err) => crate::http::store_error_response("instance_diagnostics", err),
    }
}
