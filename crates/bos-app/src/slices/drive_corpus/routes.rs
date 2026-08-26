//! Thin HTTP handlers for the Drive corpus: status, manual Sync-now (same
//! serialization guard as the pump), and BM25 search over the local index.

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bos_contracts::drive_corpus::{
    DriveCorpusSettingsUpdateRequest, DriveCorpusSettingsUpdateResponse, DriveSearchResponse,
    DriveSyncNowResponse,
};
use bos_contracts::mutation::MutationOutcomeKind;
use serde::Deserialize;

use super::{service, store, worker};
use crate::http::{error_response, now_ms, AppState};
use crate::store_core::MutationOutcome;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/drive-corpus/status", get(status))
        .route("/api/drive-corpus/settings", post(update_settings))
        .route("/api/drive-corpus/sync", post(sync_now))
        .route("/api/drive-corpus/search", get(search))
}

async fn status(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    let sync_status = state
        .sync_guards
        .guard(crate::http::Pump::Drive)
        .lock()
        .clone();
    let persistence = state.persistence.lock();
    match service::corpus_status(&state, persistence.connection_ref(), &sync_status) {
        Ok(status) => Json(status).into_response(),
        Err(err) => crate::http::store_error_response("drive_corpus", err),
    }
}

async fn update_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<DriveCorpusSettingsUpdateRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    if crate::env_registry::string(&crate::env_registry::BOS_DRIVE_CORPUS_FOLDER_IDS).is_some() {
        return error_response(StatusCode::CONFLICT, "drive_corpus_folder_env_pinned");
    }
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let mut persistence = state.persistence.lock();
    let credential_user_id = match crate::slices::google_connector::service::google_oauth_owner(
        persistence.connection_ref(),
        &state.client_id,
        &auth.actor_id,
    ) {
        Ok(owner) => owner,
        Err(err) => return crate::http::store_error_response("drive_corpus", err),
    };
    let before_config_hash =
        match service::corpus_pointer_for_state(&state, persistence.connection_ref()) {
            Ok(resolved) => service::corpus_config_hash(&resolved.pointer),
            Err(err) => return crate::http::store_error_response("drive_corpus", err),
        };
    let outcome = match service::replace_corpus_settings(
        persistence.connection(),
        &state.client_id,
        &actor_id,
        credential_user_id.as_deref(),
        &request,
        now_ms(),
    ) {
        Ok(outcome) => outcome,
        Err(err) => return crate::http::store_error_response("drive_corpus", err),
    };
    let after_config_hash =
        match service::corpus_pointer_for_state(&state, persistence.connection_ref()) {
            Ok(resolved) => service::corpus_config_hash(&resolved.pointer),
            Err(err) => return crate::http::store_error_response("drive_corpus", err),
        };
    let pointer_changed = before_config_hash != after_config_hash;
    drop(persistence);

    settings_update_response(&state, outcome, pointer_changed)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SettingsSyncKick {
    pub(super) sync_started: bool,
    pub(super) sync_refusal_reason: Option<String>,
}

fn settings_update_response(
    state: &AppState,
    outcome: MutationOutcome,
    pointer_changed: bool,
) -> Response {
    match outcome {
        MutationOutcome::Applied {
            receipt_id,
            revision,
        } => {
            let kick = start_settings_sync(state, pointer_changed);
            Json(DriveCorpusSettingsUpdateResponse {
                outcome: MutationOutcomeKind::Applied,
                receipt_id,
                revision: Some(revision),
                sync_started: kick.sync_started,
                sync_refusal_reason: kick.sync_refusal_reason,
            })
            .into_response()
        }
        MutationOutcome::ReplayedIdempotent {
            receipt_id,
            revision,
        } => Json(DriveCorpusSettingsUpdateResponse {
            outcome: MutationOutcomeKind::ReplayedIdempotent,
            receipt_id,
            revision,
            sync_started: false,
            sync_refusal_reason: None,
        })
        .into_response(),
        MutationOutcome::RevisionConflict {
            receipt_id,
            current_revision,
        } => (
            StatusCode::CONFLICT,
            Json(DriveCorpusSettingsUpdateResponse {
                outcome: MutationOutcomeKind::RevisionConflict,
                receipt_id,
                revision: current_revision,
                sync_started: false,
                sync_refusal_reason: None,
            }),
        )
            .into_response(),
    }
}

fn start_settings_sync(state: &AppState, bypass_cooldown: bool) -> SettingsSyncKick {
    start_settings_sync_with(
        state,
        now_ms(),
        bypass_cooldown,
        |worker_state, max_requests| {
            std::thread::Builder::new()
                .name("drive-settings-sync".to_string())
                .spawn(move || {
                    if let Err(err) = worker::run_guarded_cycle(&worker_state, max_requests) {
                        tracing::warn!(error = %err, "settings drive corpus sync failed");
                    }
                })
                .map(|_| ())
                .map_err(|_| "sync_spawn_failed")
        },
    )
}

pub(super) fn start_settings_sync_with<F>(
    state: &AppState,
    now: u64,
    bypass_cooldown: bool,
    spawn: F,
) -> SettingsSyncKick
where
    F: FnOnce(AppState, u32) -> Result<(), &'static str>,
{
    let configured = {
        let persistence = state.persistence.lock();
        match service::corpus_pointer_for_state(state, persistence.connection_ref()) {
            Ok(resolved) => resolved.pointer.is_configured(),
            Err(err) => {
                tracing::warn!(error = %err, "settings drive corpus sync config check failed");
                return SettingsSyncKick {
                    sync_started: false,
                    sync_refusal_reason: Some("sync_config_error".to_string()),
                };
            }
        }
    };
    if !configured {
        return SettingsSyncKick {
            sync_started: false,
            sync_refusal_reason: Some("drive_corpus_not_configured".to_string()),
        };
    }
    let begin = if bypass_cooldown {
        worker::try_begin_sync_ignoring_cooldown(state, now)
    } else {
        worker::try_begin_sync(state, now)
    };
    if let Err(reason) = begin {
        return SettingsSyncKick {
            sync_started: false,
            sync_refusal_reason: Some(reason.to_string()),
        };
    }
    let max_requests = {
        let persistence = state.persistence.lock();
        match worker::max_requests_from_settings(persistence.connection_ref(), &state.client_id) {
            Ok(max_requests) => max_requests,
            Err(err) => {
                tracing::warn!(error = %err, "settings drive corpus sync budget read failed");
                release_settings_sync_slot(state);
                return SettingsSyncKick {
                    sync_started: false,
                    sync_refusal_reason: Some("sync_config_error".to_string()),
                };
            }
        }
    };
    match spawn(state.clone(), max_requests) {
        Ok(()) => SettingsSyncKick {
            sync_started: true,
            sync_refusal_reason: None,
        },
        Err(reason) => {
            release_settings_sync_slot(state);
            SettingsSyncKick {
                sync_started: false,
                sync_refusal_reason: Some(reason.to_string()),
            }
        }
    }
}

fn release_settings_sync_slot(state: &AppState) {
    let mut status = state.sync_guards.guard(crate::http::Pump::Drive).lock();
    status.in_flight = false;
}

/// Kick one sync cycle on a background thread. 202 when claimed; 409 with the
/// reason when a sync is running, cooling down, or nothing is configured.
async fn sync_now(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    let configured = {
        let persistence = state.persistence.lock();
        match service::corpus_pointer_for_state(&state, persistence.connection_ref()) {
            Ok(resolved) => resolved.pointer.is_configured(),
            Err(err) => return crate::http::store_error_response("drive_corpus", err),
        }
    };
    if !configured {
        return (
            StatusCode::CONFLICT,
            Json(DriveSyncNowResponse {
                accepted: false,
                reason: Some("drive_corpus_not_configured".to_string()),
                next_allowed_at_ms: 0,
            }),
        )
            .into_response();
    }
    let now = now_ms();
    if let Err(reason) = worker::try_begin_sync(&state, now) {
        let next_allowed_at_ms = state
            .sync_guards
            .guard(crate::http::Pump::Drive)
            .lock()
            .next_allowed_at_ms;
        return (
            StatusCode::CONFLICT,
            Json(DriveSyncNowResponse {
                accepted: false,
                reason: Some(reason.to_string()),
                next_allowed_at_ms,
            }),
        )
            .into_response();
    }
    let max_requests = {
        let persistence = state.persistence.lock();
        match worker::max_requests_from_settings(persistence.connection_ref(), &state.client_id) {
            Ok(max_requests) => max_requests,
            Err(err) => return crate::http::store_error_response("drive_corpus", err),
        }
    };
    let worker_state = state.clone();
    std::thread::Builder::new()
        .name("drive-sync-now".to_string())
        .spawn(move || {
            if let Err(err) = worker::run_guarded_cycle(&worker_state, max_requests) {
                tracing::warn!(error = %err, "manual drive corpus sync failed");
            }
        })
        .ok();
    (
        StatusCode::ACCEPTED,
        Json(DriveSyncNowResponse {
            accepted: true,
            reason: None,
            next_allowed_at_ms: now + worker::DRIVE_SYNC_COOLDOWN_MS,
        }),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
struct SearchQuery {
    q: String,
    #[serde(default)]
    limit: Option<usize>,
}

async fn search(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<SearchQuery>,
) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    let Some(match_expr) = service::fts_match_expression(&query.q) else {
        return error_response(StatusCode::BAD_REQUEST, "drive_search_query_empty");
    };
    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    let persistence = state.persistence.lock();
    match store::search_chunks(
        persistence.connection_ref(),
        &state.client_id,
        &match_expr,
        limit,
    ) {
        Ok(hits) => Json(DriveSearchResponse { hits }).into_response(),
        Err(err) => crate::http::store_error_response("drive_corpus", err),
    }
}
