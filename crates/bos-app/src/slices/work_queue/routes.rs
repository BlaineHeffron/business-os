//! Thin HTTP handlers for the work queue.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bos_contracts::email_identity::AttentionLevel;
use bos_contracts::work_queue::{
    LaunchAgentRequest, PacketKindsResponse, WorkItemActionKind, WorkItemActionRequest,
    WorkItemAssignRequest, WorkItemGuidanceUpdateRequest, WorkItemKindsUpdateRequest,
    WorkItemStatus, WorkQueuePoliciesResponse, WorkQueuePolicyUpsertRequest, WorkQueueResponse,
};
use serde::Deserialize;

use super::service::{self, ItemSourceError};
use super::store::{self, ItemAction, ItemActionContext};
use crate::http::{error_response, mutation_response, now_ms, AppState};
use crate::store_core::StoreError;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/work-queue", get(queue_list))
        .route("/api/work-queue/{item_id}/action", post(item_action))
        .route(
            "/api/work-queue/{item_id}/assignment",
            post(item_assignment),
        )
        .route("/api/work-queue/{item_id}/source", get(item_source))
        .route(
            "/api/work-queue/{item_id}/packet-kinds",
            post(item_packet_kinds),
        )
        .route(
            "/api/work-queue/{item_id}/produce-guidance",
            post(item_produce_guidance),
        )
        .route(
            "/api/work-queue/policies",
            get(policies_list).post(policy_upsert),
        )
        .route("/api/work-queue/packet-kinds", get(packet_kinds))
        .route("/api/work-queue/{item_id}/launch-agent", post(launch_agent))
}

async fn packet_kinds(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    Json(PacketKindsResponse {
        kinds: super::packet_kind_catalog_for_enabled(|slice| state.slice_enabled(slice)),
    })
    .into_response()
}

#[derive(Debug, Deserialize)]
struct QueueQuery {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    needs_attention: Option<bool>,
    #[serde(default)]
    attention_level: Option<String>,
}

async fn queue_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<QueueQuery>,
) -> Response {
    let scope = match state.require_scope(&headers) {
        Ok(scope) => scope,
        Err(denied) => return *denied,
    };
    let status = match query.status.as_deref() {
        None => None,
        Some("open") => Some(WorkItemStatus::Open),
        Some("accepted") => Some(WorkItemStatus::Accepted),
        Some("dismissed") => Some(WorkItemStatus::Dismissed),
        Some(_) => return error_response(StatusCode::BAD_REQUEST, "work_queue_status_invalid"),
    };
    let attention_level = match query.attention_level.as_deref() {
        None => None,
        Some("lower") => Some(AttentionLevel::Lower),
        Some("normal") => Some(AttentionLevel::Normal),
        Some("higher") => Some(AttentionLevel::Higher),
        Some(_) => return error_response(StatusCode::BAD_REQUEST, "attention_level_invalid"),
    };
    let debug_enabled = crate::env_registry::flag(&crate::env_registry::BOS_DEBUG_ENABLED);
    let in_flight = crate::produce::produce_in_flight_snapshot(&state);
    let persistence = match state.persistence_or_busy() {
        Ok(persistence) => persistence,
        Err(response) => return *response,
    };
    let auto_produce_running = match crate::slices::admin_settings::service::flag(
        persistence.connection_ref(),
        &state.client_id,
        &crate::env_registry::BOS_AUTO_PRODUCE_ENABLED,
    ) {
        Ok(enabled) => enabled,
        Err(err) => return store_error_response(err),
    };
    let items = if let Some(level) = attention_level {
        service::source_attention_feed(
            persistence.connection_ref(),
            &state.client_id,
            status,
            level,
            200,
            &scope,
            service::FeedOptions {
                now_ms: now_ms(),
                auto_produce_running,
                debug_enabled,
                in_flight: &in_flight,
            },
        )
    } else if query.needs_attention == Some(true) {
        service::attention_feed(
            persistence.connection_ref(),
            &state.client_id,
            status,
            200,
            &scope,
            service::FeedOptions {
                now_ms: now_ms(),
                auto_produce_running,
                debug_enabled,
                in_flight: &in_flight,
            },
        )
    } else {
        service::feed(
            persistence.connection_ref(),
            &state.client_id,
            status,
            200,
            &scope,
            service::FeedOptions {
                now_ms: now_ms(),
                auto_produce_running,
                debug_enabled,
                in_flight: &in_flight,
            },
        )
    };
    match items {
        Ok(items) => Json(WorkQueueResponse { items }).into_response(),
        Err(err) => store_error_response(err),
    }
}

async fn item_source(
    State(state): State<AppState>,
    Path(item_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let scope = match state.require_scope(&headers) {
        Ok(scope) => scope,
        Err(denied) => return *denied,
    };
    let persistence = match state.persistence_or_busy() {
        Ok(persistence) => persistence,
        Err(response) => return *response,
    };
    match service::item_source(
        persistence.connection_ref(),
        &state.client_id,
        &item_id,
        &scope,
    ) {
        Ok(source) => Json(source).into_response(),
        Err(ItemSourceError::ItemNotFound) => {
            error_response(StatusCode::NOT_FOUND, "work_item_not_found")
        }
        Err(ItemSourceError::SourceMissing) => {
            error_response(StatusCode::NOT_FOUND, "work_item_source_missing")
        }
        Err(ItemSourceError::SourceUnsupported) => error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "produce_source_unsupported",
        ),
        Err(ItemSourceError::Store(err)) => store_error_response(err),
    }
}

/// Launch a Agent Monitor agent session seeded with this work item's context
/// plus the operator's optional notes. Operator power tool, gated by
/// `BOS_AGENT_LAUNCH_ENABLED` (404s like any disabled route when off) and
/// requiring `BOS_DEBUG_AGENT_MONITOR_URL`. Reuses the Debug monitor endpoint.
async fn launch_agent(
    State(state): State<AppState>,
    Path(item_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<LaunchAgentRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    let scope = auth.scope.clone();
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let actor_id = auth.actor_or(None);
    match service::launch_agent_from_item(state, item_id, actor_id, scope, request).await {
        Ok(response) => Json(response).into_response(),
        Err(err) => launch_agent_error_response(err),
    }
}

fn launch_agent_error_response(err: service::LaunchAgentError) -> Response {
    match err {
        service::LaunchAgentError::Disabled => {
            error_response(StatusCode::NOT_FOUND, "route_not_found")
        }
        service::LaunchAgentError::MonitorUnconfigured => error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "agent_monitor_unconfigured",
        ),
        service::LaunchAgentError::ItemNotFound => {
            error_response(StatusCode::NOT_FOUND, "work_item_not_found")
        }
        service::LaunchAgentError::PayloadBuild => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "work_queue_agent_payload_build_failed",
        ),
        service::LaunchAgentError::AlreadyRequested => {
            error_response(StatusCode::CONFLICT, "agent_launch_already_requested")
        }
        service::LaunchAgentError::AttachmentStageFailed(err) => {
            crate::slices::email_triage::routes::attachment_evidence_error_response(err)
        }
        service::LaunchAgentError::ResultInvalid => {
            error_response(StatusCode::BAD_GATEWAY, "work_queue_agent_result_invalid")
        }
        service::LaunchAgentError::JobNotClaimable => error_response(
            StatusCode::BAD_GATEWAY,
            "work_queue_agent_job_not_claimable",
        ),
        service::LaunchAgentError::DeliveryFailed => error_response(
            StatusCode::BAD_GATEWAY,
            "work_queue_agent_monitor_delivery_failed",
        ),
        service::LaunchAgentError::JoinFailed => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "work_queue_agent_spawn_join_failed",
        ),
        service::LaunchAgentError::Store(err) => store_error_response(err),
    }
}

async fn item_packet_kinds(
    State(state): State<AppState>,
    Path(item_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<WorkItemKindsUpdateRequest>,
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
    let mut persistence = match state.persistence_or_busy() {
        Ok(persistence) => persistence,
        Err(response) => return *response,
    };
    let ctx = ItemActionContext {
        client_id: &state.client_id,
        actor_id: &actor_id,
        scope: &scope,
        expected_revision: request.expected_revision,
        idempotency_key: &request.idempotency_key,
        now_ms: now_ms(),
    };
    match store::update_packet_kinds(
        persistence.connection(),
        ctx,
        &item_id,
        &request.packet_kinds,
    ) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

async fn item_produce_guidance(
    State(state): State<AppState>,
    Path(item_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<WorkItemGuidanceUpdateRequest>,
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
    let mut persistence = match state.persistence_or_busy() {
        Ok(persistence) => persistence,
        Err(response) => return *response,
    };
    let ctx = ItemActionContext {
        client_id: &state.client_id,
        actor_id: &actor_id,
        scope: &scope,
        expected_revision: request.expected_revision,
        idempotency_key: &request.idempotency_key,
        now_ms: now_ms(),
    };
    match store::update_produce_guidance(
        persistence.connection(),
        ctx,
        &item_id,
        &request.produce_guidance,
    ) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

async fn item_action(
    State(state): State<AppState>,
    Path(item_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<WorkItemActionRequest>,
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
    let mut persistence = match state.persistence_or_busy() {
        Ok(persistence) => persistence,
        Err(response) => return *response,
    };
    let ctx = ItemActionContext {
        client_id: &state.client_id,
        actor_id: &actor_id,
        scope: &scope,
        expected_revision: request.expected_revision,
        idempotency_key: &request.idempotency_key,
        now_ms: now_ms(),
    };
    let outcome = match request.action {
        WorkItemActionKind::Accept => {
            store::apply_item_action(persistence.connection(), ctx, &item_id, ItemAction::Accept)
        }
        WorkItemActionKind::Dismiss => {
            store::apply_item_action(persistence.connection(), ctx, &item_id, ItemAction::Dismiss)
        }
        WorkItemActionKind::Reopen => {
            store::apply_item_action(persistence.connection(), ctx, &item_id, ItemAction::Reopen)
        }
        WorkItemActionKind::Trash => {
            let source = match service::item_source(
                persistence.connection_ref(),
                &state.client_id,
                &item_id,
                &scope,
            ) {
                Ok(source) if source.source_kind == super::SOURCE_KIND_EMAIL => source,
                Ok(_) => {
                    return error_response(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "email_trash_source_unsupported",
                    )
                }
                Err(ItemSourceError::ItemNotFound) => {
                    return error_response(StatusCode::NOT_FOUND, "work_item_not_found")
                }
                Err(ItemSourceError::SourceMissing) => {
                    return error_response(StatusCode::NOT_FOUND, "work_item_source_missing")
                }
                Err(ItemSourceError::SourceUnsupported) => {
                    return error_response(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "email_trash_source_unsupported",
                    )
                }
                Err(ItemSourceError::Store(err)) => return store_error_response(err),
            };
            crate::slices::email_triage::store::request_gmail_trash(
                persistence.connection(),
                ctx,
                &source.message,
            )
        }
    };
    match outcome {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

async fn item_assignment(
    State(state): State<AppState>,
    Path(item_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<WorkItemAssignRequest>,
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
    let mut persistence = match state.persistence_or_busy() {
        Ok(persistence) => persistence,
        Err(response) => return *response,
    };
    let ctx = ItemActionContext {
        client_id: &state.client_id,
        actor_id: &actor_id,
        scope: &scope,
        expected_revision: request.expected_revision,
        idempotency_key: &request.idempotency_key,
        now_ms: now_ms(),
    };
    match store::update_assignment(
        persistence.connection(),
        ctx,
        &item_id,
        request.action,
        request.assignee_user_id.as_deref(),
    ) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

async fn policies_list(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    let persistence = match state.persistence_or_busy() {
        Ok(persistence) => persistence,
        Err(response) => return *response,
    };
    match store::list_policies(persistence.connection_ref(), &state.client_id) {
        Ok(policies) => Json(WorkQueuePoliciesResponse { policies }).into_response(),
        Err(err) => store_error_response(err),
    }
}

async fn policy_upsert(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<WorkQueuePolicyUpsertRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let mut persistence = match state.persistence_or_busy() {
        Ok(persistence) => persistence,
        Err(response) => return *response,
    };
    match store::upsert_policy(
        persistence.connection(),
        &state.client_id,
        &actor_id,
        &request.policy,
        &request.idempotency_key,
        now_ms(),
    ) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

fn store_error_response(err: StoreError) -> Response {
    crate::http::store_error_response("work_queue", err)
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;
    use axum::http::StatusCode;
    use serde_json::Value;

    use super::launch_agent_error_response;
    use crate::slices::email_triage::service::AttachmentEvidenceError;
    use crate::slices::work_queue::service::LaunchAgentError;

    async fn attachment_error_response(err: AttachmentEvidenceError) -> (StatusCode, String) {
        let response = launch_agent_error_response(LaunchAgentError::AttachmentStageFailed(err));
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read response body");
        let json: Value = serde_json::from_slice(&body).expect("parse error response");
        let code = json["error"].as_str().expect("error code").to_string();
        (status, code)
    }

    #[tokio::test]
    async fn launch_agent_preserves_attachment_staging_errors() {
        assert_eq!(
            attachment_error_response(AttachmentEvidenceError::AttachmentNotFound).await,
            (
                StatusCode::NOT_FOUND,
                "email_attachment_not_found".to_string()
            )
        );
        assert_eq!(
            attachment_error_response(AttachmentEvidenceError::CredentialMissing).await,
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                "gmail_credential_missing".to_string()
            )
        );
        assert_eq!(
            attachment_error_response(AttachmentEvidenceError::AttachmentTooLarge).await,
            (
                StatusCode::PAYLOAD_TOO_LARGE,
                "email_attachment_too_large".to_string()
            )
        );
    }
}
