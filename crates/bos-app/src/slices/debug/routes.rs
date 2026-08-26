//! Thin HTTP handler for the operator Debug diagnostics surface.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bos_contracts::debug::{
    DebugDiagnosticRow, DebugDiagnosticsResponse, DebugSpawnAgentRequest, DebugSpawnAgentResponse,
};

use super::store;
use crate::http::AppState;
use crate::outbox::{AttemptOutcome, STATUS_DELIVERED};
use crate::store_core::{MutationOutcome, StoreError};

#[derive(Debug)]
pub(super) enum DebugAgentSpawnError {
    PayloadBuild,
    AlreadyRequested,
    InProgress,
    JobNotClaimable,
    DeliveryFailed,
    ResultInvalid,
    JoinFailed,
    Store(StoreError),
}

impl DebugAgentSpawnError {
    fn code(&self) -> &'static str {
        match self {
            Self::PayloadBuild => "debug_agent_payload_build_failed",
            Self::AlreadyRequested => "debug_agent_launch_already_requested",
            Self::InProgress => "debug_agent_launch_in_progress",
            Self::JobNotClaimable => "debug_agent_job_not_claimable",
            Self::DeliveryFailed => "debug_agent_monitor_delivery_failed",
            Self::ResultInvalid => "debug_agent_monitor_response_invalid",
            Self::JoinFailed => "debug_agent_spawn_join_failed",
            Self::Store(_) => "debug_agent_store_failed",
        }
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/debug", get(debug))
        .route("/api/debug/spawn-agent", post(spawn_agent))
}

async fn debug(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    if !crate::env_registry::flag(&crate::env_registry::BOS_DEBUG_ENABLED) {
        return crate::http::error_response(StatusCode::NOT_FOUND, "route_not_found");
    }
    let persistence = state.persistence.lock();
    match store::list_recent(persistence.connection_ref(), &state.client_id, 200)
        .map(|rows| DebugDiagnosticsResponse { rows })
    {
        Ok(response) => Json(response).into_response(),
        Err(err) => crate::http::store_error_response("debug", err),
    }
}

async fn spawn_agent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<DebugSpawnAgentRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if !crate::env_registry::flag(&crate::env_registry::BOS_DEBUG_ENABLED) {
        return crate::http::error_response(StatusCode::NOT_FOUND, "route_not_found");
    }
    if request.idempotency_key.trim().is_empty() {
        return crate::http::error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let monitor_url =
        match crate::env_registry::string(&crate::env_registry::BOS_DEBUG_AGENT_MONITOR_URL)
            .map(|value| value.trim().trim_end_matches('/').to_string())
            .filter(|value| !value.is_empty())
        {
            Some(value) => value,
            None => {
                return crate::http::error_response(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "debug_agent_monitor_unconfigured",
                )
            }
        };
    let row = {
        let persistence = state.persistence.lock();
        match store::list_recent(persistence.connection_ref(), &state.client_id, 200) {
            Ok(rows) => rows
                .into_iter()
                .find(|row| row.diagnostic_id == request.diagnostic_id),
            Err(err) => return crate::http::store_error_response("debug", err),
        }
    };
    let Some(row) = row else {
        return crate::http::error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "debug_row_not_found",
        );
    };

    let actor_id = auth.actor_or(None);
    let prompt = debug_agent_prompt(&state.client_id, &row);
    let display_name = format!("BusinessOS debug {}", row.error_code);
    let job = match crate::slices::work_queue::agent_launch::build_outbox_job_for_source(
        crate::slices::work_queue::agent_launch::AgentLaunchOutboxJobInput {
            source_id: &row.diagnostic_id,
            idempotency_key: &request.idempotency_key,
            monitor_url: &monitor_url,
            display_name: &display_name,
            initial_prompt: &prompt,
            work_dir: crate::slices::work_queue::agent_launch::DEFAULT_AGENT_WORK_DIR,
            source_entity_kind: store::AGENT_LAUNCH_ENTITY_KIND,
            source_entity_id: &row.diagnostic_id,
            correlation_id: Some(&row.diagnostic_id),
        },
    ) {
        Ok(job) => job,
        Err(err) => {
            tracing::warn!(error = %err, "debug agent launch payload build failed");
            return debug_agent_error_response(DebugAgentSpawnError::PayloadBuild);
        }
    };
    let job_id = job.job_id.clone();
    {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        if let Some(result_json) =
            match crate::outbox::job_result_json(conn, &state.client_id, &job_id) {
                Ok(result_json) => result_json,
                Err(err) => return crate::http::store_error_response("debug", err),
            }
        {
            return match debug_response_from_result_json(&result_json) {
                Ok(response) => Json(response).into_response(),
                Err(err) => debug_agent_error_response(err),
            };
        }
        match crate::outbox::job_exists(conn, &state.client_id, &job_id) {
            Ok(true) => return debug_agent_error_response(DebugAgentSpawnError::AlreadyRequested),
            Ok(false) => {}
            Err(err) => return crate::http::store_error_response("debug", err),
        }
        match store::record_agent_launch_request(
            conn,
            store::AgentLaunchRequestContext {
                client_id: &state.client_id,
                diagnostic_id: &row.diagnostic_id,
                actor_id: &actor_id,
                job: &job,
                idempotency_key: &request.idempotency_key,
                now_ms: crate::http::now_ms(),
            },
        ) {
            Ok(MutationOutcome::Applied { .. }) => {}
            Ok(MutationOutcome::ReplayedIdempotent { .. }) => {
                let result_json =
                    match crate::outbox::job_result_json(conn, &state.client_id, &job_id) {
                        Ok(Some(result_json)) => result_json,
                        Ok(None) => {
                            return debug_agent_error_response(
                                DebugAgentSpawnError::AlreadyRequested,
                            )
                        }
                        Err(err) => return crate::http::store_error_response("debug", err),
                    };
                return match debug_response_from_result_json(&result_json) {
                    Ok(response) => Json(response).into_response(),
                    Err(err) => debug_agent_error_response(err),
                };
            }
            Ok(MutationOutcome::RevisionConflict { .. }) => {
                return debug_agent_error_response(DebugAgentSpawnError::AlreadyRequested)
            }
            Err(err) => return crate::http::store_error_response("debug", err),
        }
    }

    let task =
        tokio::task::spawn_blocking(move || launch_claimed_debug_agent_job(state, job_id)).await;
    match task {
        Ok(Ok(response)) => Json(response).into_response(),
        Ok(Err(err)) => debug_agent_error_response(err),
        Err(err) => {
            tracing::error!(error = %err, "debug spawn agent task failed");
            debug_agent_error_response(DebugAgentSpawnError::JoinFailed)
        }
    }
}

pub(super) fn launch_claimed_debug_agent_job(
    state: AppState,
    job_id: String,
) -> Result<DebugSpawnAgentResponse, DebugAgentSpawnError> {
    let now = crate::http::now_ms();
    let claimed = {
        let mut persistence = state.persistence.lock();
        crate::outbox::claim_due_job_by_id(
            persistence.connection(),
            &state.client_id,
            &job_id,
            120_000,
            now,
        )
        .map_err(DebugAgentSpawnError::Store)?
    };
    let Some(job) = claimed else {
        return debug_response_for_unclaimable_job(&state, &job_id);
    };
    let outcome = crate::slices::work_queue::agent_launch::deliver(&job, crate::http::now_ms());
    let result_json = match &outcome {
        AttemptOutcome::Delivered { result_json } => Some(result_json.clone()),
        AttemptOutcome::Retry { error, .. }
        | AttemptOutcome::Terminal { error, .. }
        | AttemptOutcome::OutcomeUnknown { error, .. } => {
            tracing::warn!(error, "debug agent launch outbox delivery failed");
            None
        }
    };
    let status = {
        let mut persistence = state.persistence.lock();
        crate::outbox::record_attempt(
            persistence.connection(),
            &state.client_id,
            &job,
            &outcome,
            crate::http::now_ms(),
        )
        .map_err(DebugAgentSpawnError::Store)?
    };
    match (status, result_json) {
        (STATUS_DELIVERED, Some(result_json)) => debug_response_from_result_json(&result_json),
        _ => Err(DebugAgentSpawnError::DeliveryFailed),
    }
}

fn debug_response_for_unclaimable_job(
    state: &AppState,
    job_id: &str,
) -> Result<DebugSpawnAgentResponse, DebugAgentSpawnError> {
    let persistence = state.persistence.lock();
    let conn = persistence.connection_ref();
    if let Some(result_json) = crate::outbox::job_result_json(conn, &state.client_id, job_id)
        .map_err(DebugAgentSpawnError::Store)?
    {
        return debug_response_from_result_json(&result_json);
    }
    let Some(summary) = crate::outbox::job_summary(conn, &state.client_id, job_id)
        .map_err(DebugAgentSpawnError::Store)?
    else {
        return Err(DebugAgentSpawnError::JobNotClaimable);
    };
    if summary.status == crate::outbox::STATUS_PENDING {
        Err(DebugAgentSpawnError::InProgress)
    } else {
        Err(DebugAgentSpawnError::DeliveryFailed)
    }
}

fn debug_response_from_result_json(
    result_json: &str,
) -> Result<DebugSpawnAgentResponse, DebugAgentSpawnError> {
    let response: bos_contracts::work_queue::LaunchAgentResponse =
        serde_json::from_str(result_json).map_err(|_| DebugAgentSpawnError::ResultInvalid)?;
    Ok(DebugSpawnAgentResponse {
        session_id: response.session_id,
        thread_id: response.thread_id,
        monitor_url: response.monitor_url,
    })
}

fn debug_agent_error_response(err: DebugAgentSpawnError) -> Response {
    let code = err.code();
    match err {
        DebugAgentSpawnError::PayloadBuild => {
            crate::http::error_response(StatusCode::INTERNAL_SERVER_ERROR, code)
        }
        DebugAgentSpawnError::AlreadyRequested => {
            crate::http::error_response(StatusCode::CONFLICT, code)
        }
        DebugAgentSpawnError::InProgress => crate::http::error_response(StatusCode::CONFLICT, code),
        DebugAgentSpawnError::JobNotClaimable
        | DebugAgentSpawnError::DeliveryFailed
        | DebugAgentSpawnError::ResultInvalid => {
            crate::http::error_response(StatusCode::BAD_GATEWAY, code)
        }
        DebugAgentSpawnError::JoinFailed => {
            crate::http::error_response(StatusCode::INTERNAL_SERVER_ERROR, code)
        }
        DebugAgentSpawnError::Store(err) => crate::http::store_error_response("debug", err),
    }
}

fn debug_agent_prompt(client_id: &str, row: &DebugDiagnosticRow) -> String {
    format!(
        "Agent session: BusinessOS debug diagnostic\n\
         Workdir: /home/example/projects/BusinessOS\n\
         Client: {client_id}\n\
         Diagnostic: {diagnostic_id}\n\
         Source: {source}\n\
         Severity: {severity}\n\
         Category: {category}\n\
         Entity: {entity_kind} {entity_id}\n\
         Operation: {operation}\n\
         Error: {error_code}\n\
         Message: {error_message}\n\
         Correlation: {correlation_id}\n\
         Reference: {reference_id}\n\
         Occurred at ms: {occurred_at_ms}\n\n\
         Investigate this BusinessOS diagnostic. Use the repo instructions, preserve receipts/outbox invariants, and report what you changed and verified.",
        diagnostic_id = row.diagnostic_id,
        source = row.source,
        severity = row.severity,
        category = row.category,
        entity_kind = row.entity_kind.as_deref().unwrap_or("-"),
        entity_id = row.entity_id.as_deref().unwrap_or("-"),
        operation = row.operation.as_deref().unwrap_or("-"),
        error_code = row.error_code,
        error_message = row.error_message.as_deref().unwrap_or("-"),
        correlation_id = row.correlation_id.as_deref().unwrap_or("-"),
        reference_id = row.reference_id.as_deref().unwrap_or("-"),
        occurred_at_ms = row.occurred_at_ms,
    )
}
