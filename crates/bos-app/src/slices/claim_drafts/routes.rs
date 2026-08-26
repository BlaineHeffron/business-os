//! Thin HTTP handlers for claim drafts (produce delegates to the shared
//! produce flow). Approval requires BOS_CLAIM_DRAFT_TO_ADDR (the filing
//! mailbox) — the Gmail draft-create executes through the existing gated
//! gmail delivery; carrier/platform filing stays human (HUMAN-CLAIM).

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bos_contracts::claim_drafts::{
    ClaimDraftActionKind, ClaimDraftActionRequest, ClaimDraftProduceRequest,
    ClaimDraftUpdateRequest, ClaimDraftsResponse,
};
use serde::Deserialize;

use super::service;
use super::store::{self, DraftActionContext};
use crate::env_registry;
use crate::http::{error_response, mutation_response, now_ms, AppState, SHARED_OPERATOR_ACTOR};
use crate::store_core::StoreError;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/claim-drafts", get(drafts_list))
        .route("/api/claim-drafts/produce", post(produce))
        .route("/api/claim-drafts/{draft_id}/action", post(draft_action))
        .route("/api/claim-drafts/{draft_id}/update", post(draft_update))
        .route("/api/claim-drafts/sync", post(sync_now))
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
    let persistence = match state.persistence_or_busy() {
        Ok(persistence) => persistence,
        Err(response) => return *response,
    };
    match store::list_drafts(
        persistence.connection_ref(),
        &state.client_id,
        query.item_id.as_deref(),
        100,
    ) {
        Ok(drafts) => Json(ClaimDraftsResponse { drafts }).into_response(),
        Err(err) => store_error_response(err),
    }
}

async fn produce(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ClaimDraftProduceRequest>,
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
    Json(request): Json<ClaimDraftUpdateRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let mut persistence = match state.persistence_or_busy() {
        Ok(persistence) => persistence,
        Err(response) => return *response,
    };
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
        &request.damage_narrative,
        &request.item_description,
        request.claim_amount_cents,
    ) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

async fn draft_action(
    State(state): State<AppState>,
    Path(draft_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<ClaimDraftActionRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let mut persistence = match state.persistence_or_busy() {
        Ok(persistence) => persistence,
        Err(response) => return *response,
    };
    let conn = persistence.connection();
    let ctx = DraftActionContext {
        client_id: &state.client_id,
        actor_id: &actor_id,
        expected_revision: request.expected_revision,
        idempotency_key: &request.idempotency_key,
        now_ms: now_ms(),
    };
    let outcome = match request.action {
        ClaimDraftActionKind::Approve => {
            let Some(to_addr) = env_registry::string(&env_registry::BOS_CLAIM_DRAFT_TO_ADDR) else {
                return error_response(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "claim_draft_to_addr_unset",
                );
            };
            let draft = match store::get_draft(conn, &state.client_id, &draft_id) {
                Ok(Some(found)) => found.draft,
                Ok(None) => {
                    return error_response(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "claim_draft_not_found",
                    )
                }
                Err(err) => return store_error_response(err),
            };
            // The Gmail draft lands in the approver's mailbox when they have
            // a personal credential; shared identity uses the fallback chain.
            let credential_user = (actor_id != SHARED_OPERATOR_ACTOR).then_some(actor_id.as_str());
            let job = match service::build_approval_job(
                &draft,
                &to_addr,
                credential_user,
                &actor_id,
                ctx.now_ms,
            ) {
                Ok(job) => job,
                Err(message) => {
                    tracing::error!(error = %message, "claim approval job build failed");
                    return error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "approval_job_build_failed",
                    );
                }
            };
            let task = service::tracking_task(&draft, ctx.now_ms);
            store::approve_draft(conn, ctx, &draft_id, &job, &task)
        }
        ClaimDraftActionKind::Reject => store::reject_draft(conn, ctx, &draft_id),
    };
    match outcome {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

/// Kick one claims sync cycle (202; 409 while syncing/cooling down or when
/// Stockforge is unconfigured). Mirrors the inventory Sync-now shape.
async fn sync_now(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    if crate::slices::inventory::service::connector_config_from_env().is_none() {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "accepted": false,
                "reason": "stockforge_not_configured",
            })),
        )
            .into_response();
    }
    let now = now_ms();
    if let Err(reason) = super::worker::try_begin_sync(&state, now) {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "accepted": false, "reason": reason })),
        )
            .into_response();
    }
    let max_requests = {
        let persistence = state.persistence.lock();
        match super::worker::max_requests_from_settings(
            persistence.connection_ref(),
            &state.client_id,
        ) {
            Ok(max_requests) => max_requests,
            Err(err) => return crate::http::store_error_response("claim_drafts", err),
        }
    };
    let worker_state = state.clone();
    std::thread::Builder::new()
        .name("claims-sync-now".to_string())
        .spawn(move || {
            if let Err(err) = super::worker::run_guarded_cycle(&worker_state, max_requests) {
                tracing::warn!(error = %err, "manual claims sync failed");
            }
        })
        .ok();
    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "accepted": true })),
    )
        .into_response()
}

fn store_error_response(err: StoreError) -> Response {
    crate::http::store_error_response("claim_drafts", err)
}
