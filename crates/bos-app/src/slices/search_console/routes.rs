use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bos_contracts::search_console::{
    SearchConsolePropertySelectRequest, SearchConsoleSyncNowResponse,
};

use super::{service, store, worker};
use crate::http::{now_ms, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/search-console/status", get(status))
        .route("/api/search-console/sync", post(sync_now))
        .route("/api/google-analytics/sync", post(sync_analytics_now))
        .route("/api/search-console/property", post(select_property))
}

async fn status(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    let sync_status = state
        .sync_guards
        .guard(crate::http::Pump::SearchConsole)
        .lock()
        .clone();
    let today = service::today_utc();
    let persistence = state.persistence.lock();
    match service::overview(&state, persistence.connection_ref(), &sync_status, &today) {
        Ok(status) => Json(status).into_response(),
        Err(err) => crate::http::store_error_response("search_console", err),
    }
}

async fn sync_now(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    let now = now_ms();
    let max_requests = {
        let persistence = state.persistence.lock();
        match worker::max_requests_from_settings(persistence.connection_ref(), &state.client_id) {
            Ok(max_requests) => max_requests,
            Err(err) => return crate::http::store_error_response("search_console", err),
        }
    };
    if let Err(reason) = worker::try_begin_sync(&state, now) {
        let next_allowed_at_ms = state
            .sync_guards
            .guard(crate::http::Pump::SearchConsole)
            .lock()
            .next_allowed_at_ms;
        return (
            StatusCode::CONFLICT,
            Json(SearchConsoleSyncNowResponse {
                accepted: false,
                reason: Some(reason.to_string()),
                next_allowed_at_ms,
            }),
        )
            .into_response();
    }
    let worker_state = state.clone();
    match std::thread::Builder::new()
        .name("search-console-sync-now".to_string())
        .spawn(move || {
            if let Err(err) = worker::run_guarded_cycle(&worker_state, max_requests) {
                tracing::warn!(error = %err, "manual search console sync failed");
            }
        }) {
        Ok(_) => {}
        Err(err) => {
            let result = Err(format!("spawn_failed: {err}"));
            worker::finish_guarded_cycle(&state, &result);
            return crate::http::error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "search_console_sync_spawn_failed",
            );
        }
    }
    (
        StatusCode::ACCEPTED,
        Json(SearchConsoleSyncNowResponse {
            accepted: true,
            reason: None,
            next_allowed_at_ms: now + worker::SEARCH_CONSOLE_SYNC_COOLDOWN_MS,
        }),
    )
        .into_response()
}

async fn sync_analytics_now(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    let config = service::config(state.search_console_overlay.as_ref().as_ref());
    if !config.analytics_configured() {
        let next_allowed_at_ms = state
            .sync_guards
            .guard(crate::http::Pump::SearchConsole)
            .lock()
            .next_allowed_at_ms;
        return (
            StatusCode::CONFLICT,
            Json(SearchConsoleSyncNowResponse {
                accepted: false,
                reason: Some("google_analytics_not_configured".to_string()),
                next_allowed_at_ms,
            }),
        )
            .into_response();
    }
    let now = now_ms();
    let max_requests = {
        let persistence = state.persistence.lock();
        match worker::max_requests_from_settings(persistence.connection_ref(), &state.client_id) {
            Ok(max_requests) => max_requests,
            Err(err) => return crate::http::store_error_response("google_analytics", err),
        }
    };
    if let Err(reason) = worker::try_begin_sync(&state, now) {
        let next_allowed_at_ms = state
            .sync_guards
            .guard(crate::http::Pump::SearchConsole)
            .lock()
            .next_allowed_at_ms;
        return (
            StatusCode::CONFLICT,
            Json(SearchConsoleSyncNowResponse {
                accepted: false,
                reason: Some(reason.to_string()),
                next_allowed_at_ms,
            }),
        )
            .into_response();
    }
    let worker_state = state.clone();
    match std::thread::Builder::new()
        .name("google-analytics-sync-now".to_string())
        .spawn(move || {
            if let Err(err) = worker::run_guarded_analytics_cycle(&worker_state, max_requests) {
                tracing::warn!(error = %err, "manual google analytics sync failed");
            }
        }) {
        Ok(_) => {}
        Err(err) => {
            let result = Err(format!("spawn_failed: {err}"));
            worker::finish_guarded_cycle(&state, &result);
            return crate::http::error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "google_analytics_sync_spawn_failed",
            );
        }
    }
    (
        StatusCode::ACCEPTED,
        Json(SearchConsoleSyncNowResponse {
            accepted: true,
            reason: None,
            next_allowed_at_ms: now + worker::SEARCH_CONSOLE_SYNC_COOLDOWN_MS,
        }),
    )
        .into_response()
}

async fn select_property(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<SearchConsolePropertySelectRequest>,
) -> Response {
    let identity = match state.authenticate_operator(&headers) {
        Ok(identity) => identity,
        Err(denied) => return *denied,
    };
    let config = service::config(state.search_console_overlay.as_ref().as_ref());
    if config.configured() {
        return crate::http::error_response(
            StatusCode::CONFLICT,
            "search_console_config_overrides_selection",
        );
    }
    let mut persistence = state.persistence.lock();
    match store::select_property(
        persistence.connection(),
        store::PropertySelectionContext {
            client_id: &state.client_id,
            actor_id: &identity.actor_id,
            expected_revision: request.expected_revision,
            idempotency_key: &request.idempotency_key,
            now_ms: now_ms(),
        },
        &request.site_url,
    ) {
        Ok(outcome) => crate::http::mutation_response(outcome),
        Err(err) => crate::http::store_error_response("search_console", err),
    }
}
