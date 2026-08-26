use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bos_contracts::quote_workflows::{
    QuoteDraftActionRequest, QuoteWorkflowRunRequest, QuoteWorkflowRunResponse,
};

use super::service::{self, QuoteRunContext};
use super::store::{DraftActionContext, QuoteWorkflowInput};
use crate::http::{error_response, mutation_response, now_ms, AppState};
use crate::store_core::StoreError;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/quote-workflows/run", post(run_workflow))
        .route("/api/quote-workflows/{run_id}", get(inspect_workflow))
        .route("/api/quote-drafts/{draft_id}/action", post(draft_action))
}

async fn run_workflow(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<QuoteWorkflowRunRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let source_attachments = match source_attachments_for_request(&state, &auth.scope, &request) {
        Ok(attachments) => attachments,
        Err(err) => return store_error_response(err),
    };
    let input = QuoteWorkflowInput {
        source_kind: request.source_kind,
        source_ref: request.source_ref,
        source_attachments,
        customer_name: request.customer_name,
        customer_tier: request.customer_tier,
        request_text: request.request_text,
    };
    let profile_config_json =
        crate::overlay::quote_profile_config_json(&state.quote_workflows_overlay);
    let guardrail_config_json =
        crate::overlay::quote_guardrail_config_json(&state.quote_workflows_overlay);
    let run_id = service::quote_run_id(&request.idempotency_key);
    {
        let persistence = state.persistence.lock();
        match service::existing_run_response(
            persistence.connection_ref(),
            &state.client_id,
            &run_id,
        ) {
            Ok(Some(response)) => {
                return Json::<QuoteWorkflowRunResponse>(response).into_response();
            }
            Ok(None) => {}
            Err(err) => return store_error_response(err),
        }
    }
    let permit = match service::try_acquire_quote_run() {
        Ok(permit) => permit,
        Err(err) => return store_error_response(err),
    };
    let started = {
        let mut persistence = state.persistence.lock();
        match service::existing_run_response(
            persistence.connection_ref(),
            &state.client_id,
            &run_id,
        ) {
            Ok(Some(response)) => {
                return Json::<QuoteWorkflowRunResponse>(response).into_response();
            }
            Ok(None) => {}
            Err(err) => return store_error_response(err),
        }
        match service::start_quote_builder(
            persistence.connection(),
            input,
            QuoteRunContext {
                client_id: &state.client_id,
                actor_id: &actor_id,
                profile_id: state.quote_workflows_overlay.profile.trim(),
                profile_config_json,
                guardrail_config_json,
                request_idempotency_key: &request.idempotency_key,
                now_ms: now_ms(),
            },
        ) {
            Ok(started) => started,
            Err(err) => return store_error_response(err),
        }
    };
    let prepared = match service::prepare_quote_builder(started, permit) {
        Ok(prepared) => prepared,
        Err(failure) => {
            let (started, err) = *failure;
            let mut persistence = state.persistence.lock();
            if let Err(finish_err) = service::fail_started_quote_builder(
                persistence.connection(),
                &started,
                &err,
                now_ms(),
            ) {
                return store_error_response(finish_err);
            }
            return store_error_response(err);
        }
    };
    let mut persistence = state.persistence.lock();
    match service::persist_prepared_quote_builder(persistence.connection(), prepared) {
        Ok(response) => Json::<QuoteWorkflowRunResponse>(response).into_response(),
        Err(err) => store_error_response(err),
    }
}

fn source_attachments_for_request(
    state: &AppState,
    scope: &crate::http::OperatorScope,
    request: &QuoteWorkflowRunRequest,
) -> Result<Vec<bos_contracts::email_triage::EmailAttachmentRecord>, crate::store_core::StoreError>
{
    if request.source_kind != "email" {
        return Ok(Vec::new());
    }
    let persistence = state.persistence.lock();
    let attachments = crate::slices::email_triage::store::inbound_by_source_keys(
        persistence.connection_ref(),
        &state.client_id,
        std::slice::from_ref(&request.source_ref),
        scope,
    )?
    .into_iter()
    .next()
    .map(|message| message.attachments)
    .unwrap_or_default();
    Ok(attachments)
}

async fn inspect_workflow(
    State(state): State<AppState>,
    Path(run_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    let persistence = state.persistence.lock();
    match service::inspect_run(persistence.connection_ref(), &state.client_id, &run_id) {
        Ok(Some(inspection)) => Json(inspection).into_response(),
        Ok(None) => error_response(StatusCode::NOT_FOUND, "quote_workflow_not_found"),
        Err(err) => store_error_response(err),
    }
}

async fn draft_action(
    State(state): State<AppState>,
    Path(draft_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<QuoteDraftActionRequest>,
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
    match service::apply_draft_action(persistence.connection(), ctx, &draft_id, request.action) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

fn store_error_response(err: StoreError) -> Response {
    match err {
        StoreError::Domain(msg) => error_response(StatusCode::UNPROCESSABLE_ENTITY, &msg),
        StoreError::Sqlite(msg) => {
            tracing::error!(error = %msg, "quote workflow sqlite error");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "store_sqlite_error")
        }
    }
}
