//! Thin HTTP handlers for the owner digest. Generate-now mirrors the
//! Sync-now guard shape (202; 409 while a generation runs or cools down);
//! the email action requires configured owner-report recipients and stages a
//! Gmail draft through the existing gated gmail delivery (dry-run until the
//! BOS_GMAIL_WRITE_ENABLED gate opens).

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bos_contracts::owner_reports::{
    OwnerReportEmailRequest, OwnerReportGenerateResponse, OwnerReportsResponse,
};
use bos_contracts::receipt::ActorKindDto;
use serde::Deserialize;

use super::service;
use super::store::{self, EmailActionContext};
use crate::http::{error_response, mutation_response, now_ms, AppState, SHARED_OPERATOR_ACTOR};
use crate::store_core::StoreError;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/owner-reports", get(reports_list))
        .route("/api/owner-reports/generate", post(generate_now))
        .route("/api/owner-reports/{report_id}/email", post(email_report))
}

#[derive(Debug, Deserialize)]
struct ReportsQuery {
    /// weekly | mtd (default both).
    #[serde(default)]
    period: Option<String>,
}

async fn reports_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ReportsQuery>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    let report_config = service::config_from_sources(state.owner_reports_overlay.as_ref().as_ref());
    if let Err(denied) = require_owner_report_access(&report_config, &auth.actor_id) {
        return *denied;
    }
    let period = match query.period.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(kind @ ("weekly" | "mtd")) => Some(kind.to_string()),
        Some(_) => return error_response(StatusCode::BAD_REQUEST, "owner_report_period_invalid"),
    };
    let persistence = state.persistence.lock();
    let conn = persistence.connection_ref();
    match store::list_reports(conn, &state.client_id, period.as_deref(), 24) {
        Ok(mut reports) => {
            for entry in &mut reports {
                match service::report_financials_visible(
                    conn,
                    &state.client_id,
                    &auth.scope,
                    &entry.report,
                    state.accounting_visibility_policy,
                ) {
                    Ok(true) => {}
                    Ok(false) => service::redact_financials(&mut entry.report),
                    Err(err) => return store_error_response(err),
                }
            }
            Json(OwnerReportsResponse { reports }).into_response()
        }
        Err(err) => store_error_response(err),
    }
}

/// Regenerate the current weekly + MTD digests now (always force-fresh).
/// 202 + background generation; the view polls GET /api/owner-reports.
async fn generate_now(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    let report_config = service::config_from_sources(state.owner_reports_overlay.as_ref().as_ref());
    if let Err(denied) = require_owner_report_access(&report_config, &auth.actor_id) {
        return *denied;
    }
    {
        let persistence = state.persistence.lock();
        if let Err(denied) = require_cached_financial_visibility(
            persistence.connection_ref(),
            &state.client_id,
            &auth.scope,
            state.accounting_visibility_policy,
        ) {
            return *denied;
        }
    }
    if let Err(reason) = super::worker::try_begin_generate(&state, now_ms()) {
        return (
            StatusCode::CONFLICT,
            Json(OwnerReportGenerateResponse {
                accepted: false,
                reason: Some(reason.to_string()),
            }),
        )
            .into_response();
    }
    let worker_state = state.clone();
    std::thread::Builder::new()
        .name("owner-digest-now".to_string())
        .spawn(move || {
            if let Err(err) = super::worker::run_guarded_generate(&worker_state, true) {
                tracing::warn!(error = %err, "manual owner-digest generation failed");
            }
        })
        .ok();
    (
        StatusCode::ACCEPTED,
        Json(OwnerReportGenerateResponse {
            accepted: true,
            reason: None,
        }),
    )
        .into_response()
}

async fn email_report(
    State(state): State<AppState>,
    Path(report_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<OwnerReportEmailRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let report_config = service::config_from_sources(state.owner_reports_overlay.as_ref().as_ref());
    if let Err(denied) = require_owner_report_access(&report_config, &auth.actor_id) {
        return *denied;
    }
    let Some(to_addr) = service::recipients_line(&report_config) else {
        return error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "owner_report_to_addr_unset",
        );
    };
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let report = match store::get_report(conn, &state.client_id, &report_id) {
        Ok(Some(found)) => found.report,
        Ok(None) => {
            return error_response(StatusCode::UNPROCESSABLE_ENTITY, "owner_report_not_found")
        }
        Err(err) => return store_error_response(err),
    };
    let financials_visible = match service::report_financials_visible(
        conn,
        &state.client_id,
        &auth.scope,
        &report,
        state.accounting_visibility_policy,
    ) {
        Ok(visible) => visible,
        Err(err) => return store_error_response(err),
    };
    let effective_report_config = if financials_visible {
        report_config.clone()
    } else {
        service::without_financial_sections(&report_config)
    };
    let ctx = EmailActionContext {
        client_id: &state.client_id,
        actor_id: &actor_id,
        actor_kind: ActorKindDto::Operator,
        expected_revision: request.expected_revision,
        idempotency_key: &request.idempotency_key,
        now_ms: now_ms(),
    };
    // The Gmail draft lands in the sender's mailbox when they have a
    // personal credential; shared identity uses the fallback chain.
    let credential_user = (actor_id != SHARED_OPERATOR_ACTOR).then_some(actor_id.as_str());
    let job = match service::build_email_job_with_config(
        &report,
        &to_addr,
        credential_user,
        &actor_id,
        ctx.now_ms,
        &effective_report_config,
    ) {
        Ok(job) => job,
        Err(message) => {
            tracing::error!(error = %message, "owner-report email job build failed");
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, "email_job_build_failed");
        }
    };
    match store::stage_email(conn, ctx, &report_id, &job) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

fn require_cached_financial_visibility(
    conn: &rusqlite::Connection,
    client_id: &str,
    scope: &crate::http::OperatorScope,
    policy: crate::overlay::AccountingVisibilityPolicy,
) -> Result<(), Box<Response>> {
    match crate::slices::accounting::service::cached_financial_visibility_allowed(
        conn, client_id, scope, policy,
    ) {
        Ok(true) => Ok(()),
        Ok(false) => Err(Box::new(error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "qbo_financial_scope_forbidden",
        ))),
        Err(err) => Err(Box::new(store_error_response(err))),
    }
}

fn require_owner_report_access(
    config: &service::OwnerReportConfig,
    actor_id: &str,
) -> Result<(), Box<Response>> {
    if service::operator_allowed(config, actor_id) {
        return Ok(());
    }
    Err(Box::new(error_response(
        StatusCode::UNPROCESSABLE_ENTITY,
        "owner_report_scope_forbidden",
    )))
}

fn store_error_response(err: StoreError) -> Response {
    crate::http::store_error_response("owner_reports", err)
}
