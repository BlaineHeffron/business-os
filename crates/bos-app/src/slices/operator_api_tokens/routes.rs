//! Thin HTTP handlers for scoped operator API tokens.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bos_contracts::operator_api_tokens::{
    OperatorApiToken, OperatorApiTokenActionKind, OperatorApiTokenActionRequest,
    OperatorApiTokenCreateRequest, OperatorApiTokenCreateResponse, OperatorApiTokenRotateRequest,
    OperatorApiTokenRotateResponse, OperatorApiTokensResponse,
};

use super::store::{self, TokenActionContext};
use crate::http::{error_response, mutation_response, now_ms, AppState};
use crate::store_core::{MutationOutcome, StoreError};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/operator-tokens", get(tokens_list).post(token_create))
        .route("/api/operator-tokens/{token_id}/action", post(token_action))
        .route(
            "/api/operator-tokens/{token_id}/rotate-token",
            post(rotate_token),
        )
}

async fn tokens_list(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(denied) = state.require_all_scope(&headers) {
        return *denied;
    }
    let persistence = state.persistence.lock();
    match store::list_tokens(persistence.connection_ref(), &state.client_id) {
        Ok(tokens) => Json(OperatorApiTokensResponse { tokens }).into_response(),
        Err(err) => store_error_response(err),
    }
}

async fn token_create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<OperatorApiTokenCreateRequest>,
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
    let label = match super::service::parse_label(&request.label) {
        Ok(label) => label,
        Err(code) => return error_response(StatusCode::UNPROCESSABLE_ENTITY, code),
    };
    let capabilities = match super::service::parse_capabilities(&request.capabilities) {
        Ok(capabilities) => capabilities,
        Err(code) => return error_response(StatusCode::UNPROCESSABLE_ENTITY, code),
    };
    let token_id = match super::service::generate_token_id() {
        Ok(token_id) => token_id,
        Err(err) => {
            tracing::error!(error = %err, "token id generation failed");
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, "token_generation_failed");
        }
    };
    let secret = match super::service::generate_secret() {
        Ok(secret) => secret,
        Err(err) => {
            tracing::error!(error = %err, "token generation failed");
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, "token_generation_failed");
        }
    };
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let now = now_ms();
    let api_token = OperatorApiToken {
        token_id,
        label,
        capabilities: capabilities
            .iter()
            .map(|cap| cap.as_str().to_string())
            .collect(),
        active: true,
        revoked_at_ms: None,
        created_by: actor_id.clone(),
        created_at_ms: now,
        updated_at_ms: now,
    };
    let mut persistence = state.persistence.lock();
    match store::create_token(
        persistence.connection(),
        &state.client_id,
        &actor_id,
        &api_token,
        &store::token_hash(&secret),
        &request.idempotency_key,
    ) {
        Ok(MutationOutcome::Applied { .. }) => Json(OperatorApiTokenCreateResponse {
            api_token,
            token: secret,
        })
        .into_response(),
        Ok(other) => mutation_response(other),
        Err(err) => store_error_response(err),
    }
}

async fn token_action(
    State(state): State<AppState>,
    Path(token_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<OperatorApiTokenActionRequest>,
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
    let ctx = TokenActionContext {
        client_id: &state.client_id,
        actor_id: &actor_id,
        expected_revision: request.expected_revision,
        idempotency_key: &request.idempotency_key,
        now_ms: now_ms(),
    };
    let result = match request.action {
        OperatorApiTokenActionKind::Enable => {
            store::set_active(persistence.connection(), ctx, &token_id, true)
        }
        OperatorApiTokenActionKind::Disable => {
            store::set_active(persistence.connection(), ctx, &token_id, false)
        }
        OperatorApiTokenActionKind::Revoke => {
            store::revoke_token(persistence.connection(), ctx, &token_id)
        }
    };
    match result {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

async fn rotate_token(
    State(state): State<AppState>,
    Path(token_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<OperatorApiTokenRotateRequest>,
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
    let secret = match super::service::generate_secret() {
        Ok(secret) => secret,
        Err(err) => {
            tracing::error!(error = %err, "token generation failed");
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, "token_generation_failed");
        }
    };
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let mut persistence = state.persistence.lock();
    let ctx = TokenActionContext {
        client_id: &state.client_id,
        actor_id: &actor_id,
        expected_revision: None,
        idempotency_key: &request.idempotency_key,
        now_ms: now_ms(),
    };
    match store::rotate_token(
        persistence.connection(),
        ctx,
        &token_id,
        &store::token_hash(&secret),
    ) {
        Ok(MutationOutcome::Applied { .. }) => {
            Json(OperatorApiTokenRotateResponse { token: secret }).into_response()
        }
        Ok(other) => mutation_response(other),
        Err(err) => store_error_response(err),
    }
}

fn store_error_response(err: StoreError) -> Response {
    crate::http::store_error_response("operator_api_tokens", err)
}
