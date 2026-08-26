//! Thin HTTP handlers for lead discovery.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bos_contracts::lead_discovery::{
    LeadFindingActionKind, LeadFindingActionRequest, LeadFindingStageRequest, LeadFindingStatus,
    LeadFindingsResponse,
};
use bos_contracts::receipt::ActorKindDto;
use serde::Deserialize;

use super::{service, store};
use crate::http::{error_response, mutation_response, now_ms, AppState, Pump};
use crate::slices::mutation_context::MutationContext;
use crate::store_core::StoreError;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/lead-discovery/status", get(status))
        .route(
            "/api/lead-discovery/findings",
            get(findings_list).post(finding_stage),
        )
        .route(
            "/api/lead-discovery/findings/{finding_id}/action",
            post(finding_action),
        )
}

async fn status(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    let last_checked = state
        .sync_guards
        .guard(Pump::LeadDiscoveryAutoscrape)
        .lock()
        .last_attempt_ms;
    Json(service::status_with_auto_poll_last_checked(
        &state.lead_discovery_overlay,
        last_checked,
    ))
    .into_response()
}

#[derive(Debug, Deserialize)]
struct FindingsQuery {
    #[serde(default)]
    status: Option<String>,
}

async fn findings_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<FindingsQuery>,
) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    let status = match query.status.as_deref() {
        None => None,
        Some("staged") => Some(LeadFindingStatus::Staged),
        Some("accepted") => Some(LeadFindingStatus::Accepted),
        Some("rejected") => Some(LeadFindingStatus::Rejected),
        Some(_) => return error_response(StatusCode::BAD_REQUEST, "lead_finding_status_invalid"),
    };
    let persistence = state.persistence.lock();
    match store::list_findings(persistence.connection_ref(), &state.client_id, status, 100) {
        Ok(findings) => Json(LeadFindingsResponse { findings }).into_response(),
        Err(err) => store_error_response(err),
    }
}

async fn finding_stage(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<LeadFindingStageRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let source =
        match service::resolve_enabled_source(&state.lead_discovery_overlay, &request.source_id) {
            Ok(source) => source,
            Err(err) => return store_error_response(err),
        };
    let now = now_ms();
    let finding = match service::finding_from_stage(&request, source, now) {
        Ok(finding) => finding,
        Err(err) => return store_error_response(err),
    };
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let mut persistence = state.persistence.lock();
    match store::insert_finding(
        persistence.connection(),
        &state.client_id,
        &actor_id,
        ActorKindDto::Operator,
        &finding,
        &request.idempotency_key,
    ) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

async fn finding_action(
    State(state): State<AppState>,
    Path(finding_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<LeadFindingActionRequest>,
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
        LeadFindingActionKind::Accept => store::accept_finding(
            persistence.connection(),
            ctx,
            &finding_id,
            &state.lead_discovery_overlay.criteria,
        ),
        LeadFindingActionKind::Reject => {
            store::reject_finding(persistence.connection(), ctx, &finding_id)
        }
    };
    match outcome {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

fn store_error_response(err: StoreError) -> Response {
    crate::http::store_error_response("lead_discovery", err)
}
