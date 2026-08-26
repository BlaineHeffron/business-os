//! Thin HTTP handlers for invoice drafts (produce delegates to the shared
//! produce flow). Approval enforces what every provider needs (customer
//! email, non-zero total) in the store gate and branches the outbox job on
//! BOS_ACCOUNTING_PROVIDER (Invoice Ninja | Stripe); each arm dry-runs
//! until its write gate, and even live the invoice stays a provider
//! DRAFT — reviewing and sending it is a human action in the provider's UI.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bos_contracts::enrichment::{
    EnrichmentKickoffRequest, EnrichmentKickoffResponse, EnrichmentMode,
};
use bos_contracts::invoice_drafts::{
    InvoiceDraftActionKind, InvoiceDraftActionRequest, InvoiceDraftProduceRequest,
    InvoiceDraftUpdateRequest, InvoiceDraftsResponse, InvoiceSettingsUpdateRequest,
};
use serde::Deserialize;

use super::service;
use super::store::{self, DraftActionContext};
use crate::http::{error_response, mutation_response, now_ms, AppState};
use crate::store_core::StoreError;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/invoice-drafts", get(drafts_list))
        .route(
            "/api/invoice-drafts/settings",
            get(invoice_settings).post(update_invoice_settings),
        )
        .route("/api/invoice-drafts/produce", post(produce))
        .route("/api/invoice-drafts/{draft_id}/action", post(draft_action))
        .route("/api/invoice-drafts/{draft_id}/update", post(draft_update))
        .route("/api/invoice-drafts/{draft_id}/enrich", post(enrich_draft))
}

async fn invoice_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    let persistence = state.persistence.lock();
    match service::settings_response(persistence.connection_ref(), &state.client_id) {
        Ok(response) => Json(response).into_response(),
        Err(err) => store_error_response(err),
    }
}

async fn update_invoice_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<InvoiceSettingsUpdateRequest>,
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
    match service::replace_invoice_settings(
        persistence.connection(),
        &state.client_id,
        &actor_id,
        &request,
        now_ms(),
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
        Ok(drafts) => Json(InvoiceDraftsResponse { drafts }).into_response(),
        Err(err) => store_error_response(err),
    }
}

async fn produce(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<InvoiceDraftProduceRequest>,
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
    match request.mode.unwrap_or(EnrichmentMode::Standard) {
        EnrichmentMode::Standard => {}
        EnrichmentMode::Research => {
            return error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "research_mode_unavailable",
            );
        }
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
    Json(request): Json<InvoiceDraftUpdateRequest>,
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
        &request.customer_name,
        request.customer_email.as_deref(),
        request.due_date.as_deref(),
        &request.memo,
        &request.line_items,
    ) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

async fn draft_action(
    State(state): State<AppState>,
    Path(draft_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<InvoiceDraftActionRequest>,
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
        InvoiceDraftActionKind::Approve => {
            // Provider seam: the draft invoice is created in whichever
            // invoicing system BOS_ACCOUNTING_PROVIDER names. QBO has no
            // invoice-draft write here — refuse loudly rather than stage an
            // undeliverable job.
            //
            // INVARIANT for every arm (current and future, incl. a QBO one):
            // the client-facing invoice NUMBER is the PROVIDER's to assign
            // from its own counter (IN: no `number` sent, Generated Numbers
            // pattern; Stripe: numbered at finalization; QBO would omit
            // DocNumber). BOS never generates numbers — redelivery dedupe
            // must ride a provider-side marker/lookup instead, so BOS
            // invoices are indistinguishable from hand-made ones.
            let provider = match crate::slices::accounting::service::configured_accounting_provider(
            ) {
                Ok(provider)
                    if provider == service::PROVIDER_STRIPE
                        || provider
                            == crate::slices::ledger_drafts::service::PROVIDER_INVOICE_NINJA =>
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
                        "invoice_draft_not_found",
                    )
                }
                Err(err) => return store_error_response(err),
            };
            let built = if provider == service::PROVIDER_STRIPE {
                service::build_approval_job(&draft, &actor_id, ctx.now_ms)
            } else {
                service::build_invoice_ninja_approval_job(&draft, &actor_id, ctx.now_ms)
            };
            let job = match built {
                Ok(job) => job,
                // The email gate surfaces as a domain code the operator
                // fixes by editing the draft.
                Err(code) if code == "invoice_draft_email_required" => {
                    return error_response(StatusCode::UNPROCESSABLE_ENTITY, &code)
                }
                Err(message) => {
                    tracing::error!(error = %message, "invoice approval job build failed");
                    return error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "approval_job_build_failed",
                    );
                }
            };
            store::approve_draft(conn, ctx, &draft_id, &job)
        }
        InvoiceDraftActionKind::Reject => store::reject_draft(conn, ctx, &draft_id),
    };
    match outcome {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

fn store_error_response(err: StoreError) -> Response {
    crate::http::store_error_response("invoice_drafts", err)
}

fn enrichment_error_response(err: service::OnDemandEnrichmentError) -> Response {
    match err {
        service::OnDemandEnrichmentError::DraftNotFound => {
            error_response(StatusCode::NOT_FOUND, "invoice_draft_not_found")
        }
        service::OnDemandEnrichmentError::DraftNotStaged => {
            error_response(StatusCode::CONFLICT, "invoice_draft_not_staged")
        }
        service::OnDemandEnrichmentError::SourceMissing => error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "enrichment_source_missing",
        ),
        service::OnDemandEnrichmentError::DomainSeedInvalid => {
            error_response(StatusCode::UNPROCESSABLE_ENTITY, "domain_seed_invalid")
        }
        service::OnDemandEnrichmentError::Store(err) => store_error_response(err),
    }
}
