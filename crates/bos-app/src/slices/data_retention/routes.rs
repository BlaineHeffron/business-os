//! Thin authenticated retention status and manual-run handlers.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bos_contracts::data_retention::{DataRetentionRunRequest, DataRetentionRunStatus};

use super::{service, worker};
use crate::http::{error_response, now_ms, AppState, Pump};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/data-retention/status", get(status))
        .route("/api/data-retention/run", post(run_now))
}

async fn status(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    let guard = state.sync_guards.guard(Pump::DataRetention).lock().clone();
    match service::status(&state, &guard, now_ms()) {
        Ok(status) => Json(status).into_response(),
        Err(err) => crate::http::store_error_response("data_retention", err),
    }
}

async fn run_now(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<DataRetentionRunRequest>,
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
    match worker::start_manual_run(&state, actor_id, &request.idempotency_key, now_ms()) {
        Ok(response) => {
            let status = match response.status {
                DataRetentionRunStatus::Spawned => StatusCode::ACCEPTED,
                DataRetentionRunStatus::Replayed => StatusCode::OK,
                DataRetentionRunStatus::AlreadyRunning => StatusCode::CONFLICT,
            };
            (status, Json(response)).into_response()
        }
        Err(err) => crate::http::store_error_response("data_retention", err),
    }
}
