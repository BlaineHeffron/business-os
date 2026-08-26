//! Thin HTTP handlers for calendar drafts. Produce runs the typed Extract off
//! the async path (spawn_blocking) and never holds the persistence lock
//! across the LLM call.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bos_contracts::calendar_drafts::{
    CalendarDraftActionKind, CalendarDraftActionRequest, CalendarDraftProduceRequest,
    CalendarDraftUpdateRequest, CalendarDraftsResponse, CalendarListResponse, CalendarOption,
};
use serde::Deserialize;

use super::service;
use super::store::{self, DraftActionContext};
use crate::http::{error_response, mutation_response, now_ms, AppState};
use crate::store_core::StoreError;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/calendar-drafts", get(drafts_list))
        .route("/api/calendar-drafts/produce", post(produce))
        .route("/api/calendar-drafts/{draft_id}/action", post(draft_action))
        .route("/api/calendar-drafts/{draft_id}/update", post(draft_update))
        .route("/api/calendar-drafts/calendars", get(calendars_list))
}

/// Default write target (BOS_GOOGLE_CALENDAR_ID): what a draft with no picked
/// calendar means when the acting user has no personal default either.
fn default_calendar_id() -> String {
    crate::env_registry::string(&crate::env_registry::BOS_GOOGLE_CALENDAR_ID)
        .unwrap_or_else(|| "primary".to_string())
}

/// The acting user's default calendar when set, else the server default —
/// the full chain for a draft with no picked calendar.
fn default_calendar_for(conn: &rusqlite::Connection, client_id: &str, actor_id: &str) -> String {
    crate::slices::operator_users::store::get_user(conn, client_id, actor_id)
        .unwrap_or_else(|err| {
            tracing::warn!(error = %err, "default-calendar user lookup failed");
            None
        })
        .and_then(|user| user.default_calendar_id)
        .unwrap_or_else(default_calendar_id)
}

fn is_mcp_operator_note(
    conn: &rusqlite::Connection,
    client_id: &str,
    draft: &bos_contracts::calendar_drafts::CalendarEventDraft,
) -> Result<bool, StoreError> {
    if draft.source_kind != crate::slices::work_queue::SOURCE_KIND_OPERATOR_NOTE {
        return Ok(false);
    }
    Ok(
        crate::slices::operator_notes::store::get_note(conn, client_id, &draft.source_ref)?
            .is_some_and(|note| note.created_by.starts_with("mcp:")),
    )
}

/// Calendars the acting user's connected account can write to — the
/// event-draft picker. Live Google call (calendarList), run off the async
/// path.
async fn calendars_list(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let identity = match state.authenticate_operator(&headers) {
        Ok(identity) => identity,
        Err(denied) => return *denied,
    };
    let (oauth, default_calendar) = {
        let persistence = state.persistence.lock();
        let conn = persistence.connection_ref();
        (
            crate::slices::google_connector::service::resolve_google_oauth(
                conn,
                &state.client_id,
                Some(&identity.actor_id),
            )
            .unwrap_or_default(),
            default_calendar_for(conn, &state.client_id, &identity.actor_id),
        )
    };
    let Some(oauth) = oauth else {
        return error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "google_credential_unavailable",
        );
    };
    let listed = tokio::task::spawn_blocking(move || {
        let client =
            bos_integrations::google_calendar::live::LiveGoogleCalendarClient::from_credentials(
                std::sync::Arc::new(
                    bos_integrations::google_calendar::live::ReqwestCalendarHttpClient::default(),
                ),
                &oauth,
            )?;
        client.list_writable_calendars()
    })
    .await;
    match listed {
        Ok(Ok(entries)) => Json(CalendarListResponse {
            calendars: entries
                .into_iter()
                .map(|entry| CalendarOption {
                    id: entry.id,
                    summary: entry.summary,
                    primary: entry.primary,
                })
                .collect(),
            default_calendar_id: default_calendar,
        })
        .into_response(),
        Ok(Err(err)) => {
            let code = match &err {
                bos_integrations::google_calendar::GoogleCalendarWriteError::Permanent {
                    code,
                    ..
                }
                | bos_integrations::google_calendar::GoogleCalendarWriteError::Retryable {
                    code,
                    ..
                } => code.clone(),
                _ => "google_calendar_list_failed".to_string(),
            };
            error_response(StatusCode::BAD_GATEWAY, &code)
        }
        Err(join_err) => {
            tracing::error!(error = %join_err, "calendar list task panicked");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "calendar_list_failed")
        }
    }
}

async fn draft_update(
    State(state): State<AppState>,
    Path(draft_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<CalendarDraftUpdateRequest>,
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
    let edit = store::CalendarDraftEdit {
        title: request.title,
        start_at: request.start_at,
        end_at: request.end_at,
        timezone: request.timezone,
        location: request.location,
        description: request.description,
        calendar_id: request.calendar_id,
        attendees: request.attendees,
        send_invitations: request.send_invitations,
    };
    match store::update_draft(persistence.connection(), ctx, &draft_id, &edit) {
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
        Ok(drafts) => Json(CalendarDraftsResponse { drafts }).into_response(),
        Err(err) => store_error_response(err),
    }
}

async fn produce(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CalendarDraftProduceRequest>,
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
    Json(request): Json<CalendarDraftActionRequest>,
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
        CalendarDraftActionKind::Approve => {
            let draft = match store::get_draft(conn, &state.client_id, &draft_id, &scope) {
                Ok(Some(found)) => found.draft,
                Ok(None) => {
                    return error_response(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "calendar_draft_not_found",
                    )
                }
                Err(err) => return store_error_response(err),
            };
            let is_mcp_operator_note = match is_mcp_operator_note(conn, &state.client_id, &draft) {
                Ok(value) => value,
                Err(err) => return store_error_response(err),
            };
            let (credential_user_id, default_calendar) = if is_mcp_operator_note {
                match crate::slices::google_connector::service::resolve_bound_google_oauth(
                    conn,
                    &state.client_id,
                    &actor_id,
                ) {
                    Ok(Some(_)) => {}
                    Ok(None) => {
                        return error_response(
                            StatusCode::UNPROCESSABLE_ENTITY,
                            "google_credential_unavailable",
                        )
                    }
                    Err(err) => return store_error_response(err),
                }
                (
                    actor_id.clone(),
                    default_calendar_for(conn, &state.client_id, &actor_id),
                )
            } else if let Some(source_user_id) = draft.source_user_id.as_deref() {
                match crate::slices::google_connector::store::get_credential(
                    conn,
                    &state.client_id,
                    source_user_id,
                    crate::slices::google_connector::SERVICE_GMAIL,
                ) {
                    Ok(Some(_)) => {}
                    Ok(None) => {
                        return error_response(
                            StatusCode::UNPROCESSABLE_ENTITY,
                            "source_user_credential_unavailable",
                        )
                    }
                    Err(err) => return store_error_response(err),
                }
                (
                    source_user_id.to_string(),
                    default_calendar_for(conn, &state.client_id, source_user_id),
                )
            } else {
                (
                    actor_id.clone(),
                    default_calendar_for(conn, &state.client_id, &actor_id),
                )
            };
            let write_enabled = crate::slices::admin_settings::service::flag(
                conn,
                &state.client_id,
                &crate::env_registry::BOS_GOOGLE_CALENDAR_WRITE_ENABLED,
            )
            .unwrap_or(false);
            if write_enabled {
                let oauth = crate::slices::google_connector::service::resolve_google_oauth(
                    conn,
                    &state.client_id,
                    Some(&credential_user_id),
                )
                .unwrap_or_default();
                if let Some(oauth) = oauth {
                    if !oauth.scopes.is_empty()
                        && !bos_integrations::google_calendar::live::has_calendar_scope(&oauth)
                    {
                        return error_response(
                            StatusCode::UNPROCESSABLE_ENTITY,
                            "google_calendar_scope_missing",
                        );
                    }
                }
            }
            let job = match service::build_approval_job(
                &draft,
                &credential_user_id,
                &actor_id,
                ctx.now_ms,
                &default_calendar,
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
            store::approve_draft(conn, ctx, &draft_id, &job)
        }
        CalendarDraftActionKind::Reject => store::reject_draft(conn, ctx, &draft_id),
    };
    match outcome {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

fn store_error_response(err: StoreError) -> Response {
    crate::http::store_error_response("calendar_drafts", err)
}
