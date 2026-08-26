//! Thin HTTP handlers for operator notes.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use bos_contracts::operator_notes::{
    OperatorNote, OperatorNoteCreateRequest, OperatorNoteCreateResponse, OperatorNotesResponse,
};

use super::{service, store};
use crate::http::{error_response, now_ms, AppState};
use crate::store_core::StoreError;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/operator-notes", get(notes_list).post(note_create))
}

async fn notes_list(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    let persistence = state.persistence.lock();
    match store::list_recent(persistence.connection_ref(), &state.client_id, 100) {
        Ok(notes) => Json(OperatorNotesResponse { notes }).into_response(),
        Err(err) => store_error_response(err),
    }
}

/// Create the note AND emit its work item ACCEPTED with the operator-selected
/// action kinds, then kick produce for each so drafts land without a second
/// click (D2 — selection IS the consent to spend the LLM call). The note id
/// derives from the idempotency key, so a retried create replays quietly: the
/// item already exists (work_item_emitted false) and produce is not re-kicked.
async fn note_create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<OperatorNoteCreateRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    if request.body.trim().is_empty() {
        return error_response(StatusCode::UNPROCESSABLE_ENTITY, "operator_note_body_empty");
    }
    // Resolve + validate the selected actions against the packet-kind catalog.
    // Empty defaults to the CRM note (D2: CRM pre-checked).
    let actions = match service::resolve_actions(&request.actions) {
        Ok(actions) => actions,
        Err(code) => return error_response(StatusCode::BAD_REQUEST, code),
    };
    let now = now_ms();
    let note = OperatorNote {
        note_id: format!("note_{}", request.idempotency_key.trim()),
        body: request.body.trim().to_string(),
        category_id: service::DEFAULT_CATEGORY.to_string(),
        created_by: auth.actor_or(request.actor_id.as_deref()),
        created_at_ms: now,
    };
    let work_item_emitted = {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        if let Err(err) =
            store::insert_note(conn, &state.client_id, &note, &request.idempotency_key)
        {
            return store_error_response(err);
        }
        match service::emit_item_for_note(conn, &state.client_id, &note, &actions, now) {
            Ok(emitted) => emitted,
            Err(err) => return store_error_response(err),
        }
    };
    let work_item_id = format!(
        "wi_{}_{}",
        crate::slices::work_queue::SOURCE_KIND_OPERATOR_NOTE,
        note.note_id
    );
    // Kick produce for the item's kinds — only on first emission, so a replayed
    // create does not re-spend LLM calls. Read back the item so the kicked kinds
    // always match its packet_kinds (validate_item_for_kind would otherwise
    // reject any kind a policy dropped). Deterministic keys keep retries quiet.
    if work_item_emitted && request.auto_produce.unwrap_or(true) {
        let item_id = work_item_id.clone();
        let kinds = {
            let persistence = state.persistence.lock();
            match crate::slices::work_queue::store::get_item_unscoped(
                persistence.connection_ref(),
                &state.client_id,
                &item_id,
            ) {
                Ok(Some(found)) => found.item.packet_kinds,
                Ok(None) => Vec::new(),
                Err(err) => return store_error_response(err),
            }
        };
        for kind in kinds {
            crate::produce::kick_produce_for_kind(
                state.clone(),
                item_id.clone(),
                kind.clone(),
                format!("note_action:{item_id}:{kind}"),
                note.created_by.clone(),
                bos_contracts::receipt::ActorKindDto::Operator,
            );
        }
    }
    Json(OperatorNoteCreateResponse {
        note,
        work_item_id,
        work_item_emitted,
    })
    .into_response()
}

fn store_error_response(err: StoreError) -> Response {
    crate::http::store_error_response("operator_notes", err)
}
