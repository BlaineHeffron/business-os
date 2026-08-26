//! Thin HTTP handler for the AI usage log.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use bos_contracts::ai_usage::AiUsageResponse;
use bos_contracts::llm_settings::{
    ClaudeSubscriptionAuthCompleteRequest, ClaudeSubscriptionAuthStartRequest,
    LlmRouteSettingsUpdateRequest,
};

use super::store;
use crate::http::{now_ms, AppState};
use crate::store_core::{MutationOutcome, StoreError};

const DAY_MS: u64 = 24 * 60 * 60 * 1000;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/ai-usage", get(usage))
        .route(
            "/api/llm-settings",
            get(llm_settings).post(update_llm_settings),
        )
        .route(
            "/api/llm-settings/claude-subscription",
            get(claude_subscription_status),
        )
        .route(
            "/api/llm-settings/claude-subscription/start",
            axum::routing::post(start_claude_subscription_auth),
        )
        .route(
            "/api/llm-settings/claude-subscription/complete",
            axum::routing::post(complete_claude_subscription_auth),
        )
}

async fn usage(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    let persistence = state.persistence.lock();
    let conn = persistence.connection_ref();
    let result = (|| -> Result<AiUsageResponse, StoreError> {
        Ok(AiUsageResponse {
            rows: store::list_recent(conn, &state.client_id, 100)?,
            totals_all_time: store::totals_since(conn, &state.client_id, 0)?,
            totals_last_24h: store::totals_since(
                conn,
                &state.client_id,
                now_ms().saturating_sub(DAY_MS),
            )?,
        })
    })();
    match result {
        Ok(response) => Json(response).into_response(),
        Err(err) => crate::http::store_error_response("ai_usage", err),
    }
}

async fn llm_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    let persistence = state.persistence.lock();
    match super::service::settings_response(persistence.connection_ref(), &state.client_id) {
        Ok(response) => Json(response).into_response(),
        Err(err) => crate::http::store_error_response("ai_usage", err),
    }
}

async fn update_llm_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<LlmRouteSettingsUpdateRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return crate::http::error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let mut persistence = state.persistence.lock();
    match super::service::replace_llm_route_settings(
        persistence.connection(),
        &state.client_id,
        &actor_id,
        &request,
        now_ms(),
    ) {
        Ok(outcome) => crate::http::mutation_response(outcome),
        Err(err) => crate::http::store_error_response("ai_usage", err),
    }
}

async fn claude_subscription_status(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    let config = {
        let persistence = state.persistence.lock();
        match super::service::effective_config(persistence.connection_ref(), &state.client_id) {
            Ok(config) => config,
            Err(err) => return crate::http::store_error_response("ai_usage", err),
        }
    };
    match tokio::task::spawn_blocking(move || super::service::claude_subscription_status(&config))
        .await
    {
        Ok(status) => Json(status).into_response(),
        Err(_) => crate::http::error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "llm_subscription_status_failed",
        ),
    }
}

async fn start_claude_subscription_auth(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ClaudeSubscriptionAuthStartRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return crate::http::error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let config = {
        let persistence = state.persistence.lock();
        match super::service::effective_config(persistence.connection_ref(), &state.client_id) {
            Ok(config) => config,
            Err(err) => return crate::http::store_error_response("ai_usage", err),
        }
    };
    let start_actor = actor_id.clone();
    let started_at_ms = now_ms();
    let response = match tokio::task::spawn_blocking(move || {
        super::service::start_claude_subscription_auth(&config, &start_actor, started_at_ms)
    })
    .await
    {
        Ok(Ok(response)) => response,
        Ok(Err(code)) => return claude_auth_error_response(code),
        Err(_) => {
            return crate::http::error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "llm_subscription_auth_start_failed",
            );
        }
    };

    let mut persistence = state.persistence.lock();
    match store::record_claude_subscription_action(
        persistence.connection(),
        &state.client_id,
        &actor_id,
        &response.flow_id,
        "authorize_requested",
        &request.idempotency_key,
        started_at_ms,
    ) {
        Ok(MutationOutcome::Applied { .. } | MutationOutcome::ReplayedIdempotent { .. }) => {
            Json(response).into_response()
        }
        Ok(MutationOutcome::RevisionConflict { .. }) => {
            super::service::cancel_claude_subscription_auth(&response.flow_id);
            crate::http::error_response(StatusCode::CONFLICT, "revision_conflict")
        }
        Err(err) => {
            super::service::cancel_claude_subscription_auth(&response.flow_id);
            crate::http::store_error_response("ai_usage", err)
        }
    }
}

async fn complete_claude_subscription_auth(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ClaudeSubscriptionAuthCompleteRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return crate::http::error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    if request.flow_id.trim().is_empty()
        || super::service::validate_claude_authorization_code(&request.authorization_code).is_err()
    {
        return crate::http::error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "llm_subscription_authorization_code_invalid",
        );
    }
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let submitted_at_ms = now_ms();
    {
        let mut persistence = state.persistence.lock();
        match store::claude_subscription_action_was_applied(
            persistence.connection_ref(),
            &state.client_id,
            request.idempotency_key.trim(),
        ) {
            Ok(true) => {
                return match store::record_claude_subscription_action(
                    persistence.connection(),
                    &state.client_id,
                    &actor_id,
                    request.flow_id.trim(),
                    "authorization_code_submitted",
                    &request.idempotency_key,
                    submitted_at_ms,
                ) {
                    Ok(outcome) => crate::http::mutation_response(outcome),
                    Err(err) => crate::http::store_error_response("ai_usage", err),
                };
            }
            Ok(false) => {}
            Err(err) => return crate::http::store_error_response("ai_usage", err),
        }
    }
    if let Err(code) = super::service::submit_claude_subscription_code(
        &request.flow_id,
        &actor_id,
        &request.authorization_code,
        submitted_at_ms,
    ) {
        let failure_key = format!("{}:submit_failed", request.idempotency_key.trim());
        let mut persistence = state.persistence.lock();
        if let Err(err) = store::record_claude_subscription_failure(
            persistence.connection(),
            &state.client_id,
            &actor_id,
            request.flow_id.trim(),
            &failure_key,
            code,
            submitted_at_ms,
        ) {
            tracing::warn!(
                error = %err,
                flow_id = %request.flow_id.trim(),
                "failed to receipt Claude authorization-code submission failure"
            );
        }
        return claude_auth_error_response(code);
    }
    let mut persistence = state.persistence.lock();
    match store::record_claude_subscription_action(
        persistence.connection(),
        &state.client_id,
        &actor_id,
        request.flow_id.trim(),
        "authorization_code_submitted",
        &request.idempotency_key,
        submitted_at_ms,
    ) {
        Ok(outcome) => crate::http::mutation_response(outcome),
        Err(err) => crate::http::store_error_response("ai_usage", err),
    }
}

fn claude_auth_error_response(code: &'static str) -> Response {
    let status = match code {
        "llm_subscription_authorization_code_invalid" => StatusCode::UNPROCESSABLE_ENTITY,
        "llm_subscription_auth_in_progress"
        | "llm_subscription_auth_flow_not_found"
        | "llm_subscription_auth_code_already_submitted" => StatusCode::CONFLICT,
        "llm_harness_unavailable" | "llm_harness_program_not_found" => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        "llm_subscription_auth_start_timeout" => StatusCode::GATEWAY_TIMEOUT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    crate::http::error_response(status, code)
}
