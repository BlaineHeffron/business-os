//! Thin HTTP handlers for CRM sales-intent drafts.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bos_contracts::crm_sales_intent::{
    CrmSalesIntentActionKind, CrmSalesIntentActionRequest, CrmSalesIntentDraftsResponse,
    CrmSalesIntentProduceRequest, CrmSalesIntentUpdateRequest,
};
use serde::Deserialize;

use super::service;
use super::store::{self, DraftActionContext};
use crate::http::{error_response, mutation_response, now_ms, AppState};
use crate::store_core::StoreError;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/crm-sales-intent", get(drafts_list))
        .route("/api/crm-sales-intent/produce", post(produce))
        .route(
            "/api/crm-sales-intent/{draft_id}/action",
            post(draft_action),
        )
        .route(
            "/api/crm-sales-intent/{draft_id}/update",
            post(draft_update),
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
        Ok(drafts) => Json(CrmSalesIntentDraftsResponse { drafts }).into_response(),
        Err(err) => store_error_response(err),
    }
}

async fn produce(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CrmSalesIntentProduceRequest>,
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
    Json(request): Json<CrmSalesIntentActionRequest>,
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
        CrmSalesIntentActionKind::Approve => {
            let draft = match store::get_draft(conn, &state.client_id, &draft_id, &scope) {
                Ok(Some(found)) => found.draft,
                Ok(None) => {
                    return error_response(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "crm_sales_intent_not_found",
                    )
                }
                Err(err) => return store_error_response(err),
            };
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
            let job = match service::build_approval_job(&draft, &actor_id, ctx.now_ms, provider) {
                Ok(job) => job,
                Err(code) => return error_response(StatusCode::UNPROCESSABLE_ENTITY, &code),
            };
            let task = draft
                .create_businessos_task
                .then(|| service::task_from_draft(&draft, ctx.now_ms));
            store::approve_draft(conn, ctx, &draft_id, &job, task.as_ref())
        }
        CrmSalesIntentActionKind::Reject => store::reject_draft(conn, ctx, &draft_id),
    };
    match outcome {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

async fn draft_update(
    State(state): State<AppState>,
    Path(draft_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<CrmSalesIntentUpdateRequest>,
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
    let current = match store::get_draft(conn, &state.client_id, &draft_id, &scope) {
        Ok(Some(found)) => found.draft,
        Ok(None) => {
            return error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "crm_sales_intent_not_found",
            )
        }
        Err(err) => return store_error_response(err),
    };
    let updated_at_ms = now_ms();
    let date_context = crate::slices::datetime_input::context_from_now_ms(updated_at_ms);
    let follow_up_due_date = match request
        .follow_up_due_date
        .as_deref()
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
        .map(|raw| crate::slices::datetime_input::normalize_civil_date(raw, Some(&date_context)))
        .transpose()
    {
        Ok(date) => date,
        Err(_) => {
            return error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "crm_sales_intent_follow_up_due_date_invalid",
            )
        }
    };
    let replacement = bos_contracts::crm_sales_intent::CrmSalesIntentDraft {
        company_name: request.company_name,
        contact_name: request.contact_name,
        contact_email: request.contact_email,
        lead_title: request.lead_title,
        intent_summary: request.intent_summary,
        rationale: request.rationale,
        qualification_status: request.qualification_status,
        next_step_text: request.next_step_text,
        follow_up_due_date,
        provider_target: request.provider_target,
        create_businessos_task: request.create_businessos_task,
        updated_at_ms,
        ..current
    };
    let ctx = DraftActionContext {
        client_id: &state.client_id,
        actor_id: &actor_id,
        scope: &scope,
        expected_revision: request.expected_revision,
        idempotency_key: &request.idempotency_key,
        now_ms: replacement.updated_at_ms,
    };
    match store::update_draft(conn, ctx, &draft_id, &replacement) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

fn store_error_response(err: StoreError) -> Response {
    crate::http::store_error_response("crm_sales_intent", err)
}
