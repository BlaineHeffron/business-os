//! Thin HTTP handlers for ledger entry drafts (produce delegates to the
//! shared produce flow). Approval requires a WRITABLE accounting provider
//! (Invoice Ninja) — QBO is read-only by construction, so approving against
//! a QBO instance refuses loudly instead of staging an undeliverable job.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bos_contracts::ledger_drafts::{
    LedgerDraftActionKind, LedgerDraftActionRequest, LedgerDraftProduceRequest,
    LedgerDraftUpdateRequest, LedgerDraftsResponse,
};
use serde::Deserialize;

use super::service;
use super::store::{self, DraftActionContext};
use crate::http::{error_response, mutation_response, now_ms, AppState};
use crate::store_core::StoreError;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/ledger-drafts", get(drafts_list))
        .route("/api/ledger-drafts/produce", post(produce))
        .route("/api/ledger-drafts/{draft_id}/action", post(draft_action))
        .route("/api/ledger-drafts/{draft_id}/update", post(draft_update))
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
        Ok(drafts) => Json(LedgerDraftsResponse { drafts }).into_response(),
        Err(err) => store_error_response(err),
    }
}

async fn produce(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<LedgerDraftProduceRequest>,
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
    Json(request): Json<LedgerDraftUpdateRequest>,
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
        &request.payer_name,
        request.payer_email.as_deref(),
        request.amount_cents,
        &request.paid_date,
        &request.description,
    ) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

async fn draft_action(
    State(state): State<AppState>,
    Path(draft_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<LedgerDraftActionRequest>,
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
        LedgerDraftActionKind::Approve => {
            // Provider seam: Invoice Ninja records a receipt (ensure-chain);
            // QBO records a payment against the snapshot-matched invoice.
            let provider =
                match crate::slices::accounting::service::configured_accounting_provider() {
                    Ok(provider)
                        if provider == service::PROVIDER_INVOICE_NINJA
                            || provider == service::PROVIDER_QBO =>
                    {
                        provider
                    }
                    Ok(_) | Err(_) => {
                        return error_response(
                            StatusCode::UNPROCESSABLE_ENTITY,
                            "accounting_provider_not_writable",
                        )
                    }
                };
            let draft = match store::get_draft(conn, &state.client_id, &draft_id) {
                Ok(Some(found)) => found.draft,
                Ok(None) => {
                    return error_response(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "ledger_draft_not_found",
                    )
                }
                Err(err) => return store_error_response(err),
            };
            let job = if provider == service::PROVIDER_QBO {
                match service::build_qbo_approval_job(
                    conn,
                    &state.client_id,
                    &draft,
                    &actor_id,
                    ctx.now_ms,
                ) {
                    Ok(job) => job,
                    // Domain codes (no/ambiguous invoice match) are the
                    // operator's to act on — surfaced verbatim as 422.
                    Err(code) if code.starts_with("qbo_payment_") => {
                        return error_response(StatusCode::UNPROCESSABLE_ENTITY, &code)
                    }
                    Err(message) => {
                        tracing::error!(error = %message, "qbo approval job build failed");
                        return error_response(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "approval_job_build_failed",
                        );
                    }
                }
            } else {
                match service::build_approval_job(&draft, &actor_id, ctx.now_ms) {
                    Ok(job) => job,
                    Err(message) => {
                        tracing::error!(error = %message, "approval job build failed");
                        return error_response(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "approval_job_build_failed",
                        );
                    }
                }
            };
            store::approve_draft(conn, ctx, &draft_id, &job)
        }
        LedgerDraftActionKind::Reject => store::reject_draft(conn, ctx, &draft_id),
    };
    match outcome {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

fn store_error_response(err: StoreError) -> Response {
    crate::http::store_error_response("ledger_drafts", err)
}
