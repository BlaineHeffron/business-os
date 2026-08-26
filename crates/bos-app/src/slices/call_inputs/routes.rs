//! Thin HTTP handlers for call inputs.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bos_contracts::call_inputs::{
    CallInputActionKind, CallInputActionRequest, CallInputStageRequest, CallInputStatus,
    CallInputsDriveSettingsUpdateRequest, CallInputsResponse,
};
use bos_contracts::receipt::ActorKindDto;
use serde::Deserialize;

use super::{service, store};
use crate::http::{error_response, mutation_response, now_ms, AppState};
use crate::slices::mutation_context::MutationContext;
use crate::store_core::StoreError;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/call-inputs/status", get(status))
        .route(
            "/api/call-inputs/drive-settings",
            get(drive_settings).post(update_drive_settings),
        )
        .route("/api/call-inputs", get(inputs_list).post(input_stage))
        .route(
            "/api/call-inputs/{call_input_id}/action",
            post(input_action),
        )
}

async fn drive_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    let persistence = state.persistence.lock();
    match service::drive_settings_response(
        persistence.connection_ref(),
        &state.client_id,
        &auth.actor_id,
    ) {
        Ok(response) => Json(response).into_response(),
        Err(err) => store_error_response(err),
    }
}

async fn update_drive_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CallInputsDriveSettingsUpdateRequest>,
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
    let credential_user_id = match crate::slices::google_connector::service::google_oauth_owner(
        persistence.connection_ref(),
        &state.client_id,
        &auth.actor_id,
    ) {
        Ok(owner) => owner,
        Err(err) => return store_error_response(err),
    };
    match service::replace_drive_settings(
        persistence.connection(),
        &state.client_id,
        &actor_id,
        credential_user_id.as_deref(),
        &request,
        now_ms(),
    ) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

async fn status(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    Json(service::status(&state.call_inputs_overlay)).into_response()
}

#[derive(Debug, Deserialize)]
struct InputsQuery {
    #[serde(default)]
    status: Option<String>,
}

async fn inputs_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<InputsQuery>,
) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    let status = match query.status.as_deref() {
        None => None,
        Some("staged") => Some(CallInputStatus::Staged),
        Some("accepted") => Some(CallInputStatus::Accepted),
        Some("rejected") => Some(CallInputStatus::Rejected),
        Some(_) => return error_response(StatusCode::BAD_REQUEST, "call_input_status_invalid"),
    };
    let persistence = state.persistence.lock();
    match store::list_inputs(persistence.connection_ref(), &state.client_id, status, 100) {
        Ok(inputs) => Json(CallInputsResponse { inputs }).into_response(),
        Err(err) => store_error_response(err),
    }
}

async fn input_stage(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CallInputStageRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    let source =
        match service::resolve_enabled_source(&state.call_inputs_overlay, &request.source_id) {
            Ok(source) => source,
            Err(err) => return store_error_response(err),
        };
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let now = now_ms();
    let input = match service::input_from_stage(&request, source, now) {
        Ok(input) => input,
        Err(err) => return store_error_response(err),
    };
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let mut persistence = state.persistence.lock();
    match store::insert_input(
        persistence.connection(),
        &state.client_id,
        &actor_id,
        ActorKindDto::Operator,
        &input,
        &request.idempotency_key,
    ) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

async fn input_action(
    State(state): State<AppState>,
    Path(call_input_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<CallInputActionRequest>,
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
    let ctx = MutationContext {
        client_id: &state.client_id,
        actor_id: &actor_id,
        expected_revision: request.expected_revision,
        idempotency_key: &request.idempotency_key,
        now_ms: now_ms(),
    };
    let outcome = match request.action {
        CallInputActionKind::Accept => match service::resolve_packet_kinds(
            &request.packet_kinds,
            &state.call_inputs_overlay.routing,
            |slice_id| state.slice_enabled(slice_id),
        ) {
            Ok(packet_kinds) => {
                store::accept_input(persistence.connection(), ctx, &call_input_id, &packet_kinds)
            }
            Err(err) => Err(err),
        },
        CallInputActionKind::Reject => {
            store::reject_input(persistence.connection(), ctx, &call_input_id)
        }
    };
    match outcome {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

fn store_error_response(err: StoreError) -> Response {
    crate::http::store_error_response("call_inputs", err)
}
