//! Thin HTTP handlers for CRM note drafts (produce delegates to the shared
//! produce flow).

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bos_contracts::crm_drafts::{
    CrmDraftActionKind, CrmDraftActionRequest, CrmDraftProduceRequest, CrmDraftUpdateRequest,
    CrmDraftsResponse,
};
use serde::Deserialize;

use super::service;
use super::store::{self, DraftActionContext};
use crate::http::{error_response, mutation_response, now_ms, AppState};
use crate::store_core::StoreError;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/crm-drafts", get(drafts_list))
        .route("/api/crm-drafts/produce", post(produce))
        .route("/api/crm-drafts/{draft_id}/action", post(draft_action))
        .route("/api/crm-drafts/{draft_id}/update", post(draft_update))
}

async fn draft_update(
    State(state): State<AppState>,
    Path(draft_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<CrmDraftUpdateRequest>,
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
    let mut persistence = state.persistence.lock();
    let ctx = DraftActionContext {
        client_id: &state.client_id,
        actor_id: &actor_id,
        scope: &scope,
        expected_revision: request.expected_revision,
        idempotency_key: &request.idempotency_key,
        now_ms: now_ms(),
    };
    match store::update_draft(
        persistence.connection(),
        ctx,
        &draft_id,
        &request.note_body,
        request.contact_email.as_deref(),
    ) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
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
    let scope = match state.require_scope(&headers) {
        Ok(scope) => scope,
        Err(denied) => return *denied,
    };
    let persistence = state.persistence.lock();
    match store::list_drafts(
        persistence.connection_ref(),
        &state.client_id,
        query.item_id.as_deref(),
        100,
        &scope,
    ) {
        Ok(drafts) => Json(CrmDraftsResponse { drafts }).into_response(),
        Err(err) => store_error_response(err),
    }
}

async fn produce(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CrmDraftProduceRequest>,
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

async fn draft_action(
    State(state): State<AppState>,
    Path(draft_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<CrmDraftActionRequest>,
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
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let ctx = DraftActionContext {
        client_id: &state.client_id,
        actor_id: &actor_id,
        scope: &scope,
        expected_revision: request.expected_revision,
        idempotency_key: &request.idempotency_key,
        now_ms: now_ms(),
    };
    let outcome = match request.action {
        CrmDraftActionKind::Approve => {
            let draft = match store::get_draft(conn, &state.client_id, &draft_id, &scope) {
                Ok(Some(found)) => found.draft,
                Ok(None) => {
                    return error_response(StatusCode::UNPROCESSABLE_ENTITY, "crm_draft_not_found")
                }
                Err(err) => return store_error_response(err),
            };
            let provider = match service::configured_crm_provider() {
                Ok(provider) => provider,
                Err(message) => {
                    tracing::error!(error = %message, "crm provider misconfigured");
                    return error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "crm_provider_invalid",
                    );
                }
            };
            // CRM-aware gate (EspoCRM, D3): a note that names a contact attaches
            // to that contact's record at delivery — so the contact must exist
            // first. When it does not, refuse and point the operator at the
            // records draft (the produce path auto-added it on the miss).
            if provider == service::PROVIDER_ESPOCRM {
                if let Some(email) = draft.contact_email.as_deref() {
                    let matches =
                        crate::slices::crm_record_drafts::service::search_existing_records(
                            None,
                            Some(email),
                            None,
                        );
                    if matches.contact_id.is_none() {
                        return error_response(
                            StatusCode::UNPROCESSABLE_ENTITY,
                            "crm_note_records_first",
                        );
                    }
                }
            }
            let job = match service::build_approval_job(&draft, &actor_id, ctx.now_ms, provider) {
                Ok(job) => job,
                Err(message) => {
                    tracing::error!(error = %message, "approval job build failed");
                    return error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "approval_job_build_failed",
                    );
                }
            };
            store::approve_draft(conn, ctx, &draft_id, &job)
        }
        CrmDraftActionKind::Reject => store::reject_draft(conn, ctx, &draft_id),
    };
    match outcome {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

fn store_error_response(err: StoreError) -> Response {
    crate::http::store_error_response("crm_drafts", err)
}
