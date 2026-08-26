//! Thin HTTP boundary for operator social proposal review. Agent staging uses
//! the separately gated agent_mcp tool and never reaches the action route.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bos_contracts::receipt::ActorKindDto;
use bos_contracts::social_publishing::{
    SocialDraftPreviewGenerateRequest, SocialGenerationResponse, SocialProposalActionKind,
    SocialProposalActionRequest, SocialProposalGenerateRequest, SocialProposalStageRequest,
    SocialProposalUpdateRequest, SocialPublishingResponse,
};

use super::{service, store};
use crate::http::{mutation_response, now_ms, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/social-publishing/proposals",
            get(proposals_list).post(proposal_stage),
        )
        .route(
            "/api/social-publishing/proposals/{proposal_id}/update",
            post(proposal_update),
        )
        .route(
            "/api/social-publishing/proposals/{proposal_id}/action",
            post(proposal_action),
        )
        .route(
            "/api/social-publishing/sources/{source_id}/generate",
            post(source_generate),
        )
        .route(
            "/api/social-publishing/drafts/{draft_id}/generate-preview",
            post(draft_preview_generate),
        )
}

async fn proposals_list(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    let channels = match service::configured_channels() {
        Ok(channels) => channels,
        Err(crate::store_core::StoreError::Domain(code))
            if code == "social_channels_not_configured" =>
        {
            Vec::new()
        }
        Err(err) => return store_error_response(err),
    };
    let persistence = state.persistence.lock();
    let mut proposals =
        match store::list_proposals(persistence.connection_ref(), &state.client_id, 100) {
            Ok(proposals) => proposals,
            Err(err) => return store_error_response(err),
        };
    let published_sources =
        match service::published_sources(persistence.connection_ref(), &state.client_id) {
            Ok(sources) => sources,
            Err(err) => return store_error_response(err),
        };
    let visible_source_ids = published_sources
        .iter()
        .map(|source| source.source_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    proposals.retain(|proposal| {
        proposal
            .proposal
            .source_id
            .as_deref()
            .is_none_or(|source_id| visible_source_ids.contains(source_id))
    });
    Json(SocialPublishingResponse {
        proposals,
        buffer_configured: !channels.is_empty(),
        channels,
        published_sources,
        buffer_live_enabled: service::buffer_live_enabled(
            persistence.connection_ref(),
            &state.client_id,
        ),
    })
    .into_response()
}

async fn proposal_stage(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<SocialProposalStageRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let mut persistence = state.persistence.lock();
    match service::stage_request(
        persistence.connection(),
        &state.client_id,
        &actor_id,
        ActorKindDto::Operator,
        &request,
        now_ms(),
    ) {
        Ok((outcome, _)) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

async fn proposal_update(
    State(state): State<AppState>,
    Path(proposal_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<SocialProposalUpdateRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let mut persistence = state.persistence.lock();
    match service::update_request(
        persistence.connection(),
        &state.client_id,
        &actor_id,
        &proposal_id,
        &request,
        now_ms(),
    ) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

async fn proposal_action(
    State(state): State<AppState>,
    Path(proposal_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<SocialProposalActionRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let mut persistence = state.persistence.lock();
    let result = match request.action {
        SocialProposalActionKind::Approve => service::approve_request(
            persistence.connection(),
            &state.client_id,
            &actor_id,
            &proposal_id,
            request.expected_revision,
            &request.idempotency_key,
            now_ms(),
        ),
        SocialProposalActionKind::Reject => service::reject_request(
            persistence.connection(),
            &state.client_id,
            &actor_id,
            &proposal_id,
            request.expected_revision,
            &request.idempotency_key,
            now_ms(),
        ),
    };
    match result {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

async fn source_generate(
    State(state): State<AppState>,
    Path(source_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<SocialProposalGenerateRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    match service::kickoff_generation(
        state,
        &source_id,
        request.expected_revision,
        &request.idempotency_key,
        &actor_id,
        bos_contracts::receipt::ActorKindDto::Operator,
    ) {
        Ok(service::GenerationKickoffOutcome::Accepted(source)) => (
            StatusCode::ACCEPTED,
            Json(SocialGenerationResponse { source: *source }),
        )
            .into_response(),
        Ok(service::GenerationKickoffOutcome::Conflict(outcome)) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

async fn draft_preview_generate(
    State(state): State<AppState>,
    Path(draft_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<SocialDraftPreviewGenerateRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    match service::kickoff_draft_preview_generation(state, &draft_id, &request, &actor_id) {
        Ok(service::GenerationKickoffOutcome::Accepted(source)) => (
            StatusCode::ACCEPTED,
            Json(SocialGenerationResponse { source: *source }),
        )
            .into_response(),
        Ok(service::GenerationKickoffOutcome::Conflict(outcome)) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

fn store_error_response(err: crate::store_core::StoreError) -> Response {
    crate::http::store_error_response("social_publishing", err)
}
