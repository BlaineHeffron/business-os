//! Thin HTTP handlers for content drafts (produce delegates to the shared
//! produce flow). Publishing is an explicit post-approval action.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bos_contracts::content_drafts::{
    ContentDraftActionKind, ContentDraftActionRequest, ContentDraftProduceRequest,
    ContentDraftPublishRequest, ContentDraftUpdateRequest, ContentDraftsResponse,
};
use serde::Deserialize;

use super::service;
use super::store::{self, DraftActionContext};
use crate::http::{error_response, mutation_response, now_ms, AppState};
use crate::store_core::StoreError;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/content-drafts", get(drafts_list))
        .route("/api/content-drafts/produce", post(produce))
        .route("/api/content-drafts/{draft_id}/action", post(draft_action))
        .route("/api/content-drafts/{draft_id}/update", post(draft_update))
        .route(
            "/api/content-drafts/{draft_id}/publish",
            post(draft_publish),
        )
}

#[derive(Debug, Deserialize)]
struct DraftsQuery {
    #[serde(default)]
    item_id: Option<String>,
}

async fn drafts_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<DraftsQuery>,
) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    let persistence = state.persistence.lock();
    match store::list_drafts(
        persistence.connection_ref(),
        &state.client_id,
        query.item_id.as_deref(),
        100,
    ) {
        Ok(drafts) => Json(ContentDraftsResponse {
            drafts,
            publishing_available: service::publishing_available(),
            publishing_live_enabled: service::publishing_live_enabled(
                persistence.connection_ref(),
                &state.client_id,
            ),
        })
        .into_response(),
        Err(err) => store_error_response(err),
    }
}

async fn draft_publish(
    State(state): State<AppState>,
    Path(draft_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<ContentDraftPublishRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    if !service::publishing_available() {
        return error_response(StatusCode::CONFLICT, "content_publish_adapter_unavailable");
    }
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let mut persistence = state.persistence.lock();
    let draft = match store::get_draft(persistence.connection_ref(), &state.client_id, &draft_id) {
        Ok(Some(entry)) => entry,
        Ok(None) => return error_response(StatusCode::NOT_FOUND, "content_draft_not_found"),
        Err(err) => return store_error_response(err),
    };
    match crate::slices::content_plans::store::content_draft_campaign_locked(
        persistence.connection_ref(),
        &state.client_id,
        &draft_id,
    ) {
        Ok(true) => {
            return error_response(StatusCode::CONFLICT, "content_publish_owned_by_campaign")
        }
        Ok(false) => {}
        Err(err) => return store_error_response(err),
    }
    let job = match service::build_publish_job(
        &state.client_id,
        &draft.draft,
        &request.slug,
        &request.published_at,
        &request.idempotency_key,
    ) {
        Ok(job) => job,
        Err(err) => return store_error_response(err),
    };
    let ctx = DraftActionContext {
        client_id: &state.client_id,
        actor_id: &actor_id,
        expected_revision: request.expected_revision,
        idempotency_key: &request.idempotency_key,
        now_ms: now_ms(),
    };
    match store::publish_draft(persistence.connection(), ctx, &draft_id, &job) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

async fn produce(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ContentDraftProduceRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    let scope = auth.scope.clone();
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    crate::produce::run(
        state,
        service::Produce,
        &request.item_id,
        &request.idempotency_key,
        &actor_id,
        scope,
    )
    .await
}

async fn draft_update(
    State(state): State<AppState>,
    Path(draft_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<ContentDraftUpdateRequest>,
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
    let ctx = DraftActionContext {
        client_id: &state.client_id,
        actor_id: &actor_id,
        expected_revision: request.expected_revision,
        idempotency_key: &request.idempotency_key,
        now_ms: now_ms(),
    };
    match store::update_draft(
        persistence.connection(),
        ctx,
        &draft_id,
        &request.title,
        &request.body_markdown,
        request.target_query.as_deref(),
        request.meta_description.as_deref(),
    ) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

async fn draft_action(
    State(state): State<AppState>,
    Path(draft_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<ContentDraftActionRequest>,
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
    let conn = persistence.connection();
    let ctx = DraftActionContext {
        client_id: &state.client_id,
        actor_id: &actor_id,
        expected_revision: request.expected_revision,
        idempotency_key: &request.idempotency_key,
        now_ms: now_ms(),
    };
    let outcome = match request.action {
        ContentDraftActionKind::Approve => store::approve_draft(conn, ctx, &draft_id),
        ContentDraftActionKind::Reject => store::reject_draft(conn, ctx, &draft_id),
    };
    match outcome {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

fn store_error_response(err: StoreError) -> Response {
    crate::http::store_error_response("content_drafts", err)
}
