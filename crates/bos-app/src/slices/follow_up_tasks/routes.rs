//! Thin HTTP handlers for follow-up drafts and the local task list. Produce
//! runs the typed fill off the async path (spawn_blocking) and never holds
//! the persistence lock across the LLM call.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bos_contracts::follow_up_tasks::{
    FollowUpDraftActionKind, FollowUpDraftActionRequest, FollowUpDraftManualStageRequest,
    FollowUpDraftProduceRequest, FollowUpDraftProduceResponse, FollowUpDraftUpdateRequest,
    FollowUpDraftsResponse, TaskActionKind, TaskActionRequest, TaskStatus, TasksResponse,
};
use serde::Deserialize;

use super::service;
use super::store::{self, DraftActionContext, TaskAction};
use crate::http::{error_response, mutation_response, now_ms, AppState};
use crate::store_core::StoreError;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/follow-up-drafts", get(drafts_list))
        .route("/api/follow-up-drafts/manual", post(manual_stage))
        .route("/api/follow-up-drafts/produce", post(produce))
        .route(
            "/api/follow-up-drafts/{draft_id}/action",
            post(draft_action),
        )
        .route(
            "/api/follow-up-drafts/{draft_id}/update",
            post(draft_update),
        )
        .route("/api/tasks", get(tasks_list))
        .route("/api/tasks/{task_id}/action", post(task_action))
}

async fn manual_stage(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<FollowUpDraftManualStageRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let scope = auth.scope;
    let now = now_ms();
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let item = match crate::slices::work_queue::store::get_item_scoped(
        conn,
        &state.client_id,
        &request.item_id,
        &scope,
    ) {
        Ok(Some(entry)) => entry.item,
        Ok(None) => return error_response(StatusCode::UNPROCESSABLE_ENTITY, "work_item_not_found"),
        Err(err) => return store_error_response(err),
    };
    if let Err(code) = crate::produce::validate_item_for_kind(&item, service::PACKET_KIND) {
        return error_response(StatusCode::UNPROCESSABLE_ENTITY, code);
    }
    match store::active_draft_for_item(conn, &state.client_id, &item.item_id) {
        Ok(Some(draft)) => return Json(FollowUpDraftProduceResponse { draft }).into_response(),
        Ok(None) => {}
        Err(err) => return store_error_response(err),
    }
    let fields = match store::normalize_editable_fields(
        &request.title,
        request.due_date.as_deref(),
        &request.context,
        now,
    ) {
        Ok(fields) => fields,
        Err(err) => return store_error_response(err),
    };
    let attempt = match store::count_drafts_for_item(conn, &state.client_id, &item.item_id) {
        Ok(count) => count + 1,
        Err(err) => return store_error_response(err),
    };
    let draft = service::manual_draft(&item, fields, attempt, now);
    if let Err(err) = store::insert_draft(
        conn,
        &state.client_id,
        &actor_id,
        &draft,
        &request.idempotency_key,
    ) {
        return store_error_response(err);
    }
    match store::active_draft_for_item(conn, &state.client_id, &item.item_id) {
        Ok(Some(draft)) => Json(FollowUpDraftProduceResponse { draft }).into_response(),
        Ok(None) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "follow_up_draft_not_found",
        ),
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
        Ok(drafts) => Json(FollowUpDraftsResponse { drafts }).into_response(),
        Err(err) => store_error_response(err),
    }
}

async fn produce(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<FollowUpDraftProduceRequest>,
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
    Json(request): Json<FollowUpDraftUpdateRequest>,
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
        &request.title,
        request.due_date.as_deref(),
        &request.context,
    ) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

async fn draft_action(
    State(state): State<AppState>,
    Path(draft_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<FollowUpDraftActionRequest>,
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
        FollowUpDraftActionKind::Approve => {
            let draft = match store::get_draft(conn, &state.client_id, &draft_id, &scope) {
                Ok(Some(found)) => found.draft,
                Ok(None) => {
                    return error_response(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "follow_up_draft_not_found",
                    )
                }
                Err(err) => return store_error_response(err),
            };
            let task = service::task_from_draft(&draft, ctx.now_ms);
            store::approve_draft(conn, ctx, &draft_id, &task)
        }
        FollowUpDraftActionKind::Reject => store::reject_draft(conn, ctx, &draft_id),
    };
    match outcome {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

#[derive(Debug, Deserialize)]
struct TasksQuery {
    #[serde(default)]
    status: Option<String>,
    /// Operator's local date (YYYY-MM-DD); when present, open tasks are
    /// decorated with watchdog escalation lanes.
    #[serde(default)]
    today: Option<String>,
}

async fn tasks_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<TasksQuery>,
) -> Response {
    let scope = match state.require_scope(&headers) {
        Ok(scope) => scope,
        Err(denied) => return *denied,
    };
    let status = match query.status.as_deref() {
        None => None,
        Some("open") => Some(TaskStatus::Open),
        Some("done") => Some(TaskStatus::Done),
        Some(_) => return error_response(StatusCode::BAD_REQUEST, "task_status_invalid"),
    };
    let today = match query.today.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(raw) if service::is_iso_date(raw) => Some(raw.to_string()),
        Some(_) => return error_response(StatusCode::BAD_REQUEST, "task_today_invalid"),
    };
    let persistence = state.persistence.lock();
    match store::list_tasks(
        persistence.connection_ref(),
        &state.client_id,
        status,
        200,
        &scope,
    ) {
        Ok(mut tasks) => {
            if let Err(err) = crate::slices::email_drafts::store::decorate_tasks_with_follow_ups(
                persistence.connection_ref(),
                &state.client_id,
                &scope,
                &mut tasks,
            ) {
                return store_error_response(err);
            }
            if let Some(today) = today.as_deref() {
                service::decorate_task_escalations(&mut tasks, today);
            }
            Json(TasksResponse { tasks }).into_response()
        }
        Err(err) => store_error_response(err),
    }
}

async fn task_action(
    State(state): State<AppState>,
    Path(task_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<TaskActionRequest>,
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
    let action = match request.action {
        TaskActionKind::Complete => TaskAction::Complete,
        TaskActionKind::Reopen => TaskAction::Reopen,
    };
    let mut persistence = state.persistence.lock();
    let ctx = DraftActionContext {
        client_id: &state.client_id,
        actor_id: &actor_id,
        scope: &scope,
        expected_revision: request.expected_revision,
        idempotency_key: &request.idempotency_key,
        now_ms: now_ms(),
    };
    match store::apply_task_action(persistence.connection(), ctx, &task_id, action) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

fn store_error_response(err: StoreError) -> Response {
    crate::http::store_error_response("follow_up_tasks", err)
}
