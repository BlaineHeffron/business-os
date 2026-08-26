use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use bos_contracts::packet_proposals::{SmartDraftRequest, SmartDraftSourceStateRequest};

use super::service::{self, SmartDraftCandidateMode, SmartDraftInput, SmartDraftSourceStateInput};
use crate::http::{error_response, store_error_response, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/packet-proposals/smart-draft", post(smart_draft))
        .route(
            "/api/packet-proposals/smart-draft/source-state",
            post(smart_draft_source_state),
        )
}

async fn smart_draft_source_state(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<SmartDraftSourceStateRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    let input = SmartDraftSourceStateInput {
        source_kind: request.source_kind,
        source_ref: request.source_ref,
        run_id: request.run_id,
        scope: auth.scope,
    };
    let state_for_task = state.clone();
    match tokio::task::spawn_blocking(move || {
        service::smart_draft_source_state(state_for_task, input)
    })
    .await
    {
        Ok(Ok(response)) => Json(response).into_response(),
        Ok(Err(err)) => smart_draft_error_response(err),
        Err(_) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "packet_proposal_join_failed",
        ),
    }
}

async fn smart_draft(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<SmartDraftRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let input = SmartDraftInput {
        source_kind: request.source_kind,
        source_ref: request.source_ref,
        idempotency_key: request.idempotency_key,
        expected_revision: request.expected_revision,
        min_confidence: None,
        candidate_mode: SmartDraftCandidateMode::AllEnabled,
        actor_id,
        scope: auth.scope,
    };
    let state_for_task = state.clone();
    match tokio::task::spawn_blocking(move || service::kickoff_smart_draft(state_for_task, input))
        .await
    {
        Ok(Ok(response)) => Json(response).into_response(),
        Ok(Err(err)) => smart_draft_error_response(err),
        Err(_) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "packet_proposal_join_failed",
        ),
    }
}

fn smart_draft_error_response(err: service::SmartDraftError) -> Response {
    match err {
        service::SmartDraftError::BadRequest(code) => error_response(StatusCode::BAD_REQUEST, code),
        service::SmartDraftError::SourceNotFound => {
            error_response(StatusCode::NOT_FOUND, "packet_proposal_source_not_found")
        }
        service::SmartDraftError::SourceUnsupported => error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "packet_proposal_source_unsupported",
        ),
        service::SmartDraftError::NoProposalCandidates => error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "packet_proposal_no_candidates",
        ),
        service::SmartDraftError::Llm(code) => error_response(StatusCode::BAD_GATEWAY, &code),
        service::SmartDraftError::RevisionConflict { .. } => {
            error_response(StatusCode::CONFLICT, "expected_revision_conflict")
        }
        service::SmartDraftError::Store(err) => store_error_response("packet_proposals", err),
    }
}
