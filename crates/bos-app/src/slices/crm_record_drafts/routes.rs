//! Thin HTTP handlers for CRM record-create drafts (produce delegates to the
//! shared produce flow; approval enqueues the EspoCRM ensure-chain write).

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bos_contracts::crm_record_drafts::{
    CrmRecordDraftActionKind, CrmRecordDraftActionRequest, CrmRecordDraftProduceRequest,
    CrmRecordDraftUpdateRequest, CrmRecordDraftsResponse,
};
use bos_contracts::enrichment::{EnrichmentKickoffRequest, EnrichmentKickoffResponse};
use serde::Deserialize;

use super::service;
use super::store::{self, DraftActionContext};
use crate::http::{error_response, mutation_response, now_ms, AppState};
use crate::store_core::StoreError;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/crm-record-drafts", get(drafts_list))
        .route("/api/crm-record-drafts/produce", post(produce))
        .route(
            "/api/crm-record-drafts/{draft_id}/action",
            post(draft_action),
        )
        .route(
            "/api/crm-record-drafts/{draft_id}/update",
            post(draft_update),
        )
        .route(
            "/api/crm-record-drafts/{draft_id}/enrich",
            post(enrich_draft),
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
        Ok(drafts) => Json(CrmRecordDraftsResponse { drafts }).into_response(),
        Err(err) => store_error_response(err),
    }
}

async fn produce(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CrmRecordDraftProduceRequest>,
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

async fn enrich_draft(
    State(state): State<AppState>,
    Path(draft_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<EnrichmentKickoffRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let actor_id = auth.actor_or(None);
    let domain_override =
        match service::normalize_enrichment_domain_seed(request.domain_seed.as_deref()) {
            Ok(domain) => domain,
            Err(service::OnDemandEnrichmentError::DomainSeedInvalid) => {
                return error_response(StatusCode::UNPROCESSABLE_ENTITY, "domain_seed_invalid")
            }
            Err(err) => return enrichment_error_response(err),
        };
    match service::kick_on_demand_enrichment(
        state,
        draft_id,
        actor_id,
        request.idempotency_key,
        domain_override,
        request.mode,
    ) {
        Ok(kickoff) => (
            StatusCode::ACCEPTED,
            Json(EnrichmentKickoffResponse {
                run_id: kickoff.run_id,
                already_running: kickoff.already_running,
            }),
        )
            .into_response(),
        Err(err) => enrichment_error_response(err),
    }
}

async fn draft_update(
    State(state): State<AppState>,
    Path(draft_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<CrmRecordDraftUpdateRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let edit = match service::sanitize_record_edit(&request) {
        Ok(edit) => edit,
        Err(code) => return error_response(StatusCode::UNPROCESSABLE_ENTITY, code),
    };
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let mut persistence = state.persistence.lock();
    let ctx = DraftActionContext {
        client_id: &state.client_id,
        actor_id: &actor_id,
        expected_revision: request.expected_revision,
        idempotency_key: &request.idempotency_key,
        now_ms: now_ms(),
    };
    match store::update_draft(persistence.connection(), ctx, &draft_id, &edit) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

async fn draft_action(
    State(state): State<AppState>,
    Path(draft_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<CrmRecordDraftActionRequest>,
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
        CrmRecordDraftActionKind::Approve => {
            // Both CRM providers run a records ensure-chain (EspoCRM and
            // HubSpot); the job is built for whichever is configured.
            let provider = match crate::slices::crm_drafts::service::configured_crm_provider() {
                Ok(provider) => provider,
                Err(message) => {
                    tracing::error!(error = %message, "crm provider misconfigured");
                    return error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "crm_provider_invalid",
                    );
                }
            };
            let draft = match store::get_draft(conn, &state.client_id, &draft_id) {
                Ok(Some(found)) => found.draft,
                Ok(None) => {
                    return error_response(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "crm_record_draft_not_found",
                    )
                }
                Err(err) => return store_error_response(err),
            };
            let job = match service::build_approval_job(&draft, &actor_id, ctx.now_ms, provider) {
                Ok(job) => job,
                Err(code) => {
                    // Gate failures are operator-actionable (no record proposed,
                    // missing name) — surface as 422 with the wire code.
                    return error_response(StatusCode::UNPROCESSABLE_ENTITY, &code);
                }
            };
            store::approve_draft(conn, ctx, &draft_id, &job)
        }
        CrmRecordDraftActionKind::Reject => store::reject_draft(conn, ctx, &draft_id),
    };
    match outcome {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

fn store_error_response(err: StoreError) -> Response {
    crate::http::store_error_response("crm_record_drafts", err)
}

fn enrichment_error_response(err: service::OnDemandEnrichmentError) -> Response {
    match err {
        service::OnDemandEnrichmentError::DraftNotFound => {
            error_response(StatusCode::NOT_FOUND, "crm_record_draft_not_found")
        }
        service::OnDemandEnrichmentError::DraftNotStaged => {
            error_response(StatusCode::CONFLICT, "crm_record_draft_not_staged")
        }
        service::OnDemandEnrichmentError::NothingToEnrich => {
            error_response(StatusCode::UNPROCESSABLE_ENTITY, "nothing_to_enrich")
        }
        service::OnDemandEnrichmentError::SourceMissing => error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "enrichment_source_missing",
        ),
        service::OnDemandEnrichmentError::DomainSeedInvalid => {
            error_response(StatusCode::UNPROCESSABLE_ENTITY, "domain_seed_invalid")
        }
        service::OnDemandEnrichmentError::ResearchModeDisabled => {
            error_response(StatusCode::UNPROCESSABLE_ENTITY, "research_mode_disabled")
        }
        service::OnDemandEnrichmentError::ResearchDomainMissing => {
            error_response(StatusCode::UNPROCESSABLE_ENTITY, "research_domain_missing")
        }
        service::OnDemandEnrichmentError::ResearchConcurrencyLimit => {
            error_response(StatusCode::CONFLICT, "research_concurrency_limit")
        }
        service::OnDemandEnrichmentError::Store(err) => store_error_response(err),
    }
}
