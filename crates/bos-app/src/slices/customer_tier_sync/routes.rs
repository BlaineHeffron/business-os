//! Thin HTTP handlers for customer tier sync. Preview reads local QBO
//! snapshots only; approval enqueues the Shopify write through the outbox.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bos_contracts::customer_tier_sync::{
    CustomerTierSyncApproveRequest, CustomerTierSyncListResponse, CustomerTierSyncPreviewRequest,
};

use super::{service, store};
use crate::http::{error_response, mutation_response, now_ms, AppState};
use crate::store_core::StoreError;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/customer-tier-sync/runs", get(list_runs))
        .route("/api/customer-tier-sync/preview", post(preview))
        .route(
            "/api/customer-tier-sync/runs/{run_id}/approve",
            post(approve),
        )
        .route("/api/customer-tier-sync/runs/{run_id}/reject", post(reject))
}

async fn list_runs(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    let persistence = state.persistence.lock();
    match store::list_runs(persistence.connection_ref(), &state.client_id, 50) {
        Ok(runs) => Json(CustomerTierSyncListResponse { runs }).into_response(),
        Err(err) => store_error_response(err),
    }
}

async fn preview(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CustomerTierSyncPreviewRequest>,
) -> Response {
    let auth = match state.authenticate_operator(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let resolver = match service::target_resolver_from_sources(&state.customer_tier_sync_overlay) {
        Ok(resolver) => resolver,
        Err(code) => return error_response(StatusCode::UNPROCESSABLE_ENTITY, &code),
    };
    let now = now_ms();
    let run_id = service::run_id_for_idempotency_key(&request.idempotency_key);
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let customers = match crate::slices::accounting::store::list_customers(conn, &state.client_id) {
        Ok(customers) => customers,
        Err(err) => return store_error_response(err),
    };
    let plan = service::build_plan_with_resolver(&customers, &resolver);
    match store::stage_run(
        conn,
        &state.client_id,
        &auth.actor_id,
        &run_id,
        &plan,
        &request.idempotency_key,
        now,
    ) {
        Ok(_outcome) => match store::get_run(conn, &state.client_id, &run_id) {
            Ok(Some(run)) => Json(run).into_response(),
            Ok(None) => {
                error_response(StatusCode::INTERNAL_SERVER_ERROR, "tier_sync_stage_missing")
            }
            Err(err) => store_error_response(err),
        },
        Err(err) => store_error_response(err),
    }
}

async fn approve(
    State(state): State<AppState>,
    Path(run_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<CustomerTierSyncApproveRequest>,
) -> Response {
    let auth = match state.authenticate_operator(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let run = match store::get_run(conn, &state.client_id, &run_id) {
        Ok(Some(run)) => run,
        Ok(None) => {
            return error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "customer_tier_sync_run_not_found",
            )
        }
        Err(err) => return store_error_response(err),
    };
    let now = now_ms();
    let job = match service::build_approval_job(&run, &auth.actor_id, now) {
        Ok(job) => job,
        Err(code) => return error_response(StatusCode::UNPROCESSABLE_ENTITY, &code),
    };
    match store::approve_run(
        conn,
        crate::slices::mutation_context::MutationContext {
            client_id: &state.client_id,
            actor_id: &auth.actor_id,
            expected_revision: Some(request.expected_revision),
            idempotency_key: &request.idempotency_key,
            now_ms: now,
        },
        &run_id,
        &job,
    ) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

async fn reject(
    State(state): State<AppState>,
    Path(run_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<CustomerTierSyncApproveRequest>,
) -> Response {
    let auth = match state.authenticate_operator(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let mut persistence = state.persistence.lock();
    match store::reject_run(
        persistence.connection(),
        crate::slices::mutation_context::MutationContext {
            client_id: &state.client_id,
            actor_id: &auth.actor_id,
            expected_revision: Some(request.expected_revision),
            idempotency_key: &request.idempotency_key,
            now_ms: now_ms(),
        },
        &run_id,
    ) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

fn store_error_response(err: StoreError) -> Response {
    crate::http::store_error_response("customer_tier_sync", err)
}
