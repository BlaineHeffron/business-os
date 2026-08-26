//! Thin HTTP handlers for release notes.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bos_contracts::release_notes::{
    ReleaseNoteCreateRequest, ReleaseNoteDismissRequest, ReleaseNotesResponse,
};

use super::{service, store};
use crate::http::{error_response, mutation_response, now_ms, AppState};
use crate::store_core::{MutationOutcome, StoreError};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/webhooks/release-notes", post(webhook_create))
        .route("/api/release-notes/latest", get(latest))
        .route("/api/release-notes", get(list))
        .route("/api/release-notes/{id}/dismiss", post(dismiss))
}

async fn webhook_create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ReleaseNoteCreateRequest>,
) -> Response {
    let Some(secret) = service::webhook_secret_from_env() else {
        return error_response(StatusCode::NOT_FOUND, "route_not_found");
    };
    let authorization = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    if let Err(code) = service::verify_webhook_bearer(authorization, &secret) {
        return error_response(StatusCode::UNAUTHORIZED, code);
    };
    let note = match service::note_from_request(&request, now_ms()) {
        Ok(note) => note,
        Err(code) => return error_response(StatusCode::UNPROCESSABLE_ENTITY, code),
    };
    let mut persistence = state.persistence();
    let outcome = match store::insert_note(
        persistence.connection(),
        &state.client_id,
        "fleet",
        &note,
        &request.idempotency_key,
    ) {
        Ok(outcome) => outcome,
        Err(err) => return store_error_response(err),
    };
    let status = match outcome {
        MutationOutcome::Applied { .. } => StatusCode::ACCEPTED,
        MutationOutcome::ReplayedIdempotent { .. } => StatusCode::OK,
        MutationOutcome::RevisionConflict { .. } => StatusCode::CONFLICT,
    };
    (status, Json(serde_json::json!({ "accepted": true }))).into_response()
}

async fn latest(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    let persistence = state.persistence();
    match store::latest_visible(
        persistence.connection_ref(),
        &state.client_id,
        &auth.identity.actor_id,
    ) {
        Ok(Some(note)) => Json(ReleaseNotesResponse { notes: vec![note] }).into_response(),
        Ok(None) => Json(ReleaseNotesResponse { notes: Vec::new() }).into_response(),
        Err(err) => store_error_response(err),
    }
}

async fn list(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    let persistence = state.persistence();
    match store::list_recent(persistence.connection_ref(), &state.client_id, 20) {
        Ok(notes) => Json(ReleaseNotesResponse { notes }).into_response(),
        Err(err) => store_error_response(err),
    }
}

async fn dismiss(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(release_note_id): Path<String>,
    Json(request): Json<ReleaseNoteDismissRequest>,
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
    match store::dismiss_note(
        persistence.connection(),
        &state.client_id,
        &user_id,
        &actor_id,
        &release_note_id,
        &request.idempotency_key,
        now_ms(),
    ) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

fn store_error_response(err: StoreError) -> Response {
    crate::http::store_error_response("release_notes", err)
}
