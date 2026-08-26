//! Thin HTTP handlers for email reply drafts (produce delegates to the
//! shared produce flow).

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bos_contracts::email_drafts::{
    EmailDraftActionKind, EmailDraftActionRequest, EmailDraftManualStageRequest,
    EmailDraftProduceRequest, EmailDraftProduceResponse, EmailDraftRewriteRequest,
    EmailDraftRewriteResponse, EmailDraftUpdateRequest, EmailDraftsResponse,
    EmailOutboundFollowUpActionRequest, EmailOutboundFollowUpCheckResponse,
    EmailOutboundFollowUpDraftResponse, EmailOutboundFollowUpStatus,
    EmailOutboundFollowUpsResponse, GmailThreadFollowUpState,
};
use bos_integrations::gmail_inbox_read::LiveGmailInboxReadClient;
use bos_integrations::ReqwestGmailHttpClient;
use serde::Deserialize;
use std::sync::Arc;

use super::service;
use super::store::{self, DraftActionContext};
use crate::http::{error_response, mutation_response, now_ms, AppState};
use crate::store_core::{MutationOutcome, StoreError};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/email-drafts", get(drafts_list))
        .route("/api/email-drafts/manual", post(manual_stage))
        .route("/api/email-drafts/produce", post(produce))
        .route("/api/email-drafts/{draft_id}/action", post(draft_action))
        .route("/api/email-drafts/{draft_id}/update", post(draft_update))
        .route("/api/email-drafts/{draft_id}/rewrite", post(draft_rewrite))
        .route("/api/email-drafts/follow-ups", get(follow_ups_list))
        .route(
            "/api/email-drafts/follow-ups/{follow_up_id}/check",
            post(follow_up_check),
        )
        .route(
            "/api/email-drafts/follow-ups/{follow_up_id}/draft",
            post(follow_up_draft),
        )
}

async fn draft_update(
    State(state): State<AppState>,
    Path(draft_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<EmailDraftUpdateRequest>,
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
        &request.to_addr,
        &request.cc_addrs,
        &request.subject,
        &request.body_text,
    ) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

async fn manual_stage(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<EmailDraftManualStageRequest>,
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
        Ok(Some(draft)) => return Json(EmailDraftProduceResponse { draft }).into_response(),
        Ok(None) => {}
        Err(err) => return store_error_response(err),
    }
    let fields = match store::normalize_editable_fields(
        &request.to_addr,
        &request.cc_addrs,
        &request.subject,
        &request.body_text,
        true,
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
        Ok(Some(draft)) => Json(EmailDraftProduceResponse { draft }).into_response(),
        Ok(None) => error_response(StatusCode::INTERNAL_SERVER_ERROR, "email_draft_not_found"),
        Err(err) => store_error_response(err),
    }
}

async fn draft_rewrite(
    State(state): State<AppState>,
    Path(draft_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<EmailDraftRewriteRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let Some(expected_revision) = request.expected_revision else {
        return error_response(StatusCode::BAD_REQUEST, "expected_revision_required");
    };
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let scope = auth.scope;
    let rewrite_request = {
        let persistence = state.persistence.lock();
        let conn = persistence.connection_ref();
        let draft = match store::get_draft(conn, &state.client_id, &draft_id, &scope) {
            Ok(Some(entry)) => entry,
            Ok(None) => {
                return error_response(StatusCode::UNPROCESSABLE_ENTITY, "email_draft_not_found")
            }
            Err(err) => return store_error_response(err),
        };
        match crate::store_core::applied_revision_for_idempotency(
            conn,
            &state.client_id,
            &request.idempotency_key,
        ) {
            Ok(Some(_)) => {
                return Json(EmailDraftRewriteResponse { draft }).into_response();
            }
            Ok(None) => {}
            Err(err) => return store_error_response(err),
        }
        if draft.revision != expected_revision {
            return error_response(StatusCode::CONFLICT, "revision_conflict");
        }
        if draft.draft.status != bos_contracts::email_drafts::EmailDraftStatus::Staged {
            return error_response(StatusCode::UNPROCESSABLE_ENTITY, "email_draft_not_staged");
        }
        let item = match crate::slices::work_queue::store::get_item_scoped(
            conn,
            &state.client_id,
            &draft.draft.item_id,
            &scope,
        ) {
            Ok(Some(entry)) => entry.item,
            Ok(None) => {
                return error_response(StatusCode::UNPROCESSABLE_ENTITY, "work_item_not_found")
            }
            Err(err) => return store_error_response(err),
        };
        let source = match crate::produce::resolve_source(conn, &state.client_id, &item) {
            Ok(Some(source)) => source,
            Ok(None) => {
                return error_response(StatusCode::UNPROCESSABLE_ENTITY, "produce_source_missing")
            }
            Err(crate::produce::SourceError::Unsupported) => {
                return error_response(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "produce_source_unsupported",
                )
            }
            Err(crate::produce::SourceError::Store(err)) => return store_error_response(err),
        };
        let background = match crate::produce::background_text_block(conn, &state.client_id) {
            Ok(background) => background,
            Err(err) => return store_error_response(err),
        };
        match service::build_rewrite_request(
            &state.client_id,
            &draft.draft,
            &source,
            &request.instructions,
            background,
            expected_revision,
        ) {
            Ok(request) => request,
            Err(code) => return error_response(StatusCode::UNPROCESSABLE_ENTITY, code),
        }
    };
    let rewrite_evidence = rewrite_request.input.text_blocks.clone();
    let llm_state = state.clone();
    let envelope = match tokio::task::spawn_blocking(move || {
        crate::slices::ai_usage::service::execute_recorded(
            llm_state.persistence.clone(),
            &llm_state.client_id,
            service::FILL_PURPOSE,
            &rewrite_request,
        )
    })
    .await
    {
        Ok(Ok(envelope)) => envelope,
        Ok(Err(err)) => return error_response(StatusCode::BAD_GATEWAY, err.code()),
        Err(_) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, "email_rewrite_failed"),
    };
    let fill = match service::parse_reply_fill_response(&envelope.response_json) {
        Ok(fill) => fill,
        Err(_) => {
            return error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "email_fill_invalid_response",
            )
        }
    };
    let grounded_provenance =
        service::grounded_rewrite_provenance(&fill.provenance, &rewrite_evidence);
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let ctx = DraftActionContext {
        client_id: &state.client_id,
        actor_id: &actor_id,
        scope: &scope,
        expected_revision: Some(expected_revision),
        idempotency_key: &request.idempotency_key,
        now_ms: now_ms(),
    };
    match store::apply_ai_rewrite(
        conn,
        ctx,
        &draft_id,
        &fill.body_text,
        &grounded_provenance,
        &envelope.model,
        &fill.confidence,
    ) {
        Ok(MutationOutcome::RevisionConflict { .. }) => {
            return error_response(StatusCode::CONFLICT, "revision_conflict")
        }
        Ok(_) => {}
        Err(err) => return store_error_response(err),
    }
    match store::get_draft(conn, &state.client_id, &draft_id, &scope) {
        Ok(Some(draft)) => Json(EmailDraftRewriteResponse { draft }).into_response(),
        Ok(None) => error_response(StatusCode::INTERNAL_SERVER_ERROR, "email_draft_not_found"),
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
        Ok(drafts) => Json(EmailDraftsResponse { drafts }).into_response(),
        Err(err) => store_error_response(err),
    }
}

async fn produce(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<EmailDraftProduceRequest>,
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
    Json(request): Json<EmailDraftActionRequest>,
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
        EmailDraftActionKind::Approve => {
            let draft = match store::get_draft(conn, &state.client_id, &draft_id, &scope) {
                Ok(Some(found)) => found.draft,
                Ok(None) => {
                    return error_response(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "email_draft_not_found",
                    )
                }
                Err(err) => return store_error_response(err),
            };
            if let Err(err) = store::normalize_editable_fields(
                &draft.to_addr,
                &draft.cc_addrs,
                &draft.subject,
                &draft.body_text,
                false,
            ) {
                return store_error_response(err);
            }
            let credential_user_id = if let Some(source_user_id) = draft.source_user_id.as_deref() {
                match crate::slices::google_connector::store::get_credential(
                    conn,
                    &state.client_id,
                    source_user_id,
                    crate::slices::google_connector::SERVICE_GMAIL,
                ) {
                    Ok(Some(_)) => source_user_id.to_string(),
                    Ok(None) => {
                        return error_response(
                            StatusCode::UNPROCESSABLE_ENTITY,
                            "source_user_credential_unavailable",
                        )
                    }
                    Err(err) => return store_error_response(err),
                }
            } else {
                actor_id.clone()
            };
            let job = match service::build_approval_job(
                &draft,
                &actor_id,
                &credential_user_id,
                ctx.now_ms,
            ) {
                Ok(job) => job,
                Err(message) => {
                    tracing::error!(error = %message, "approval job build failed");
                    return error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "approval_job_build_failed",
                    );
                }
            };
            let follow_up = match request.follow_up.as_ref() {
                Some(follow_up) => {
                    match store::EmailFollowUpPlan::from_request(&draft, follow_up, ctx.now_ms) {
                        Ok(plan) => plan,
                        Err(err) => return store_error_response(err),
                    }
                }
                None => None,
            };
            store::approve_draft(conn, ctx, &draft_id, &job, follow_up)
        }
        EmailDraftActionKind::Reject => store::reject_draft(conn, ctx, &draft_id),
    };
    match outcome {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

#[derive(Debug, Deserialize)]
struct FollowUpsQuery {
    #[serde(default = "default_follow_up_status")]
    status: String,
}

fn default_follow_up_status() -> String {
    "open".to_string()
}

async fn follow_ups_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<FollowUpsQuery>,
) -> Response {
    let scope = match state.require_scope(&headers) {
        Ok(scope) => scope,
        Err(denied) => return *denied,
    };
    let status = match query.status.as_str() {
        "open" => store::FollowUpListStatus::Open,
        "resolved" => store::FollowUpListStatus::Resolved,
        "all" => store::FollowUpListStatus::All,
        _ => return error_response(StatusCode::BAD_REQUEST, "email_follow_up_status_invalid"),
    };
    let persistence = state.persistence.lock();
    match store::list_follow_ups(
        persistence.connection_ref(),
        &state.client_id,
        status,
        &scope,
    ) {
        Ok(follow_ups) => Json(EmailOutboundFollowUpsResponse { follow_ups }).into_response(),
        Err(err) => store_error_response(err),
    }
}

async fn follow_up_check(
    State(state): State<AppState>,
    Path(follow_up_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<EmailOutboundFollowUpActionRequest>,
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
    let (target, oauth) = {
        let persistence = state.persistence.lock();
        let conn = persistence.connection_ref();
        let target = match store::get_follow_up_check_target(
            conn,
            &state.client_id,
            &follow_up_id,
            &scope,
        ) {
            Ok(Some(target)) => target,
            Ok(None) => {
                return error_response(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "email_follow_up_not_found",
                )
            }
            Err(err) => return store_error_response(err),
        };
        let oauth = match crate::slices::google_connector::service::resolve_google_oauth(
            conn,
            &state.client_id,
            target.source_user_id.as_deref(),
        ) {
            Ok(oauth) => oauth,
            Err(err) => return store_error_response(err),
        };
        (target, oauth)
    };

    let reconciliation = if target.summary.thread_id.is_none() {
        service::not_applicable_reconciliation()
    } else if let Some(oauth) = oauth {
        let thread_id = target.summary.thread_id.clone().unwrap_or_default();
        let approved_at_ms = target.approved_at_ms;
        match tokio::task::spawn_blocking(move || {
            let client = LiveGmailInboxReadClient::from_credentials(
                Arc::new(ReqwestGmailHttpClient::default()),
                &oauth,
            )?;
            client.read_thread_messages(&thread_id)
        })
        .await
        {
            Ok(Ok(messages)) => service::classify_thread_follow_up(&messages, approved_at_ms),
            Ok(Err(err)) => stale_from_summary(&target.summary, err.to_string()),
            Err(err) => stale_from_summary(&target.summary, err.to_string()),
        }
    } else {
        stale_from_summary(&target.summary, "google_credential_unavailable")
    };

    let mut persistence = state.persistence.lock();
    let ctx = DraftActionContext {
        client_id: &state.client_id,
        actor_id: &actor_id,
        scope: &scope,
        expected_revision: None,
        idempotency_key: &request.idempotency_key,
        now_ms: now_ms(),
    };
    match store::apply_thread_reconciliation(
        persistence.connection(),
        ctx,
        &follow_up_id,
        reconciliation,
    ) {
        Ok(outcome) => {
            if outcome.should_complete_linked_task {
                if let Some(task_id) = outcome.linked_task_id.as_deref() {
                    let task_idempotency_key =
                        format!("{}:email_followup_task_auto_done", request.idempotency_key);
                    let task_ctx = crate::slices::follow_up_tasks::store::DraftActionContext {
                        client_id: &state.client_id,
                        actor_id: &actor_id,
                        scope: &scope,
                        expected_revision: None,
                        idempotency_key: &task_idempotency_key,
                        now_ms: now_ms(),
                    };
                    if let Err(err) = crate::slices::follow_up_tasks::store::apply_task_action(
                        persistence.connection(),
                        task_ctx,
                        task_id,
                        crate::slices::follow_up_tasks::store::TaskAction::Complete,
                    ) {
                        return store_error_response(err);
                    }
                }
            }
            match store::get_follow_up(
                persistence.connection_ref(),
                &state.client_id,
                &follow_up_id,
                &scope,
            ) {
                Ok(Some(follow_up)) => {
                    Json(EmailOutboundFollowUpCheckResponse { follow_up }).into_response()
                }
                Ok(None) => error_response(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "email_follow_up_not_found",
                ),
                Err(err) => store_error_response(err),
            }
        }
        Err(err) => store_error_response(err),
    }
}

async fn follow_up_draft(
    State(state): State<AppState>,
    Path(follow_up_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<EmailOutboundFollowUpActionRequest>,
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
    let target =
        match store::get_follow_up_check_target(conn, &state.client_id, &follow_up_id, &scope) {
            Ok(Some(target)) => target,
            Ok(None) => {
                return error_response(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "email_follow_up_not_found",
                )
            }
            Err(err) => return store_error_response(err),
        };
    if target.summary.status != EmailOutboundFollowUpStatus::Active
        && target.summary.status != EmailOutboundFollowUpStatus::Stale
    {
        return error_response(StatusCode::UNPROCESSABLE_ENTITY, "email_follow_up_not_open");
    }
    if target.summary.thread_state != GmailThreadFollowUpState::SentWaitingReply {
        return error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "email_follow_up_not_waiting_reply",
        );
    }
    if target.summary.due_date > service::epoch_ms_to_iso_date(now_ms()) {
        return error_response(StatusCode::UNPROCESSABLE_ENTITY, "email_follow_up_not_due");
    }
    if let Ok(Some(existing)) = crate::slices::work_queue::store::get_item_for_source(
        conn,
        &state.client_id,
        store::SOURCE_KIND_EMAIL_FOLLOW_UP,
        &follow_up_id,
    ) {
        return Json(EmailOutboundFollowUpDraftResponse { item: existing }).into_response();
    }
    let item = service::follow_up_draft_work_item(
        &target.summary,
        target.source_user_id.clone(),
        now_ms(),
    );
    if let Err(err) = crate::slices::work_queue::store::insert_item_with_actor(
        conn,
        &state.client_id,
        &item,
        &actor_id,
        bos_contracts::receipt::ActorKindDto::Operator,
        &request.idempotency_key,
    ) {
        return store_error_response(err);
    }
    match crate::slices::work_queue::store::get_item_for_source(
        conn,
        &state.client_id,
        store::SOURCE_KIND_EMAIL_FOLLOW_UP,
        &follow_up_id,
    ) {
        Ok(Some(item)) => Json(EmailOutboundFollowUpDraftResponse { item }).into_response(),
        Ok(None) => error_response(StatusCode::INTERNAL_SERVER_ERROR, "follow_up_item_missing"),
        Err(err) => store_error_response(err),
    }
}

fn stale_from_summary(
    summary: &bos_contracts::email_drafts::EmailOutboundFollowUpSummary,
    error: impl Into<String>,
) -> store::ThreadReconciliation {
    let mut reconciliation = service::stale_reconciliation(error);
    reconciliation.sent_message_id = summary.sent_message_id.clone();
    reconciliation.sent_at_ms = summary.sent_at_ms;
    reconciliation.reply_message_id = summary.reply_message_id.clone();
    reconciliation.reply_at_ms = summary.reply_at_ms;
    reconciliation
}

fn store_error_response(err: StoreError) -> Response {
    crate::http::store_error_response("email_drafts", err)
}
