//! Thin HTTP handlers for operator users + the whoami endpoint.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bos_contracts::operator_users::{
    OperatorUser, OperatorUserActionKind, OperatorUserActionRequest, OperatorUserCreateRequest,
    OperatorUserCreateResponse, OperatorUserDefaultCalendarRequest, OperatorUserRotateTokenRequest,
    OperatorUserRotateTokenResponse, OperatorUsersResponse, WhoAmIResponse,
};

use super::store::{self, UserActionContext};
use crate::http::{error_response, mutation_response, now_ms, AppState};
use crate::store_core::StoreError;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/me", get(whoami))
        .route("/api/users", get(users_list).post(user_create))
        .route("/api/users/{user_id}/action", post(user_action))
        .route("/api/users/{user_id}/rotate-token", post(rotate_token))
        .route(
            "/api/users/{user_id}/default-calendar",
            post(set_default_calendar),
        )
}

#[derive(serde::Deserialize)]
struct UsersListQuery {
    #[serde(default)]
    include_archived: bool,
}

async fn whoami(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let identity = match state.authenticate_operator(&headers) {
        Ok(identity) => identity,
        Err(denied) => return *denied,
    };
    Json(WhoAmIResponse {
        actor_id: identity.actor_id,
        display_name: identity.display_name,
    })
    .into_response()
}

async fn users_list(
    State(state): State<AppState>,
    Query(query): Query<UsersListQuery>,
    headers: HeaderMap,
) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    let persistence = state.persistence.lock();
    match store::list_users(
        persistence.connection_ref(),
        &state.client_id,
        query.include_archived,
    ) {
        Ok(users) => Json(OperatorUsersResponse { users }).into_response(),
        Err(err) => store_error_response(err),
    }
}

async fn user_create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<OperatorUserCreateRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let user_id = match super::service::user_id_from_display_name(&request.display_name) {
        Ok(user_id) => user_id,
        Err(code) => return error_response(StatusCode::UNPROCESSABLE_ENTITY, code),
    };
    let token = match super::service::generate_token() {
        Ok(token) => token,
        Err(err) => {
            tracing::error!(error = %err, "token generation failed");
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, "token_generation_failed");
        }
    };
    let now = now_ms();
    let user = OperatorUser {
        user_id,
        display_name: request.display_name.trim().to_string(),
        active: true,
        archived_at_ms: None,
        default_calendar_id: None,
        created_at_ms: now,
        updated_at_ms: now,
    };
    let mut persistence = state.persistence.lock();
    match store::create_user(
        persistence.connection(),
        &state.client_id,
        &actor_id,
        &user,
        &token,
        &request.idempotency_key,
    ) {
        Ok(_) => Json(OperatorUserCreateResponse { user, token }).into_response(),
        Err(err) => store_error_response(err),
    }
}

async fn user_action(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<OperatorUserActionRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let mut persistence = state.persistence.lock();
    let ctx = UserActionContext {
        client_id: &state.client_id,
        actor_id: &actor_id,
        expected_revision: request.expected_revision,
        idempotency_key: &request.idempotency_key,
        now_ms: now_ms(),
    };
    match request.action {
        OperatorUserActionKind::Enable => {
            match store::set_active(persistence.connection(), ctx, &user_id, true) {
                Ok(outcome) => mutation_response(outcome),
                Err(err) => store_error_response(err),
            }
        }
        OperatorUserActionKind::Disable => {
            match store::set_active(persistence.connection(), ctx, &user_id, false) {
                Ok(outcome) => mutation_response(outcome),
                Err(err) => store_error_response(err),
            }
        }
        OperatorUserActionKind::Archive => {
            if actor_id == user_id {
                return error_response(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "operator_user_self_archive",
                );
            }
            match store::archive_user(persistence.connection(), ctx, &user_id) {
                Ok(outcome) => mutation_response(outcome),
                Err(err) => store_error_response(err),
            }
        }
    }
}

async fn rotate_token(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<OperatorUserRotateTokenRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let token = match super::service::generate_token() {
        Ok(token) => token,
        Err(err) => {
            tracing::error!(error = %err, "token generation failed");
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, "token_generation_failed");
        }
    };
    let mut persistence = state.persistence.lock();
    let ctx = UserActionContext {
        client_id: &state.client_id,
        actor_id: &actor_id,
        expected_revision: None,
        idempotency_key: &request.idempotency_key,
        now_ms: now_ms(),
    };
    match store::rotate_token(persistence.connection(), ctx, &user_id, &token) {
        Ok(_) => Json(OperatorUserRotateTokenResponse { token }).into_response(),
        Err(err) => store_error_response(err),
    }
}

async fn set_default_calendar(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<OperatorUserDefaultCalendarRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    // Empty string = clear (the UI's "use the server default" option).
    let calendar_id = request
        .calendar_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty());
    let mut persistence = state.persistence.lock();
    let ctx = UserActionContext {
        client_id: &state.client_id,
        actor_id: &actor_id,
        expected_revision: request.expected_revision,
        idempotency_key: &request.idempotency_key,
        now_ms: now_ms(),
    };
    match store::set_default_calendar(persistence.connection(), ctx, &user_id, calendar_id) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

fn store_error_response(err: StoreError) -> Response {
    crate::http::store_error_response("operator_users", err)
}
