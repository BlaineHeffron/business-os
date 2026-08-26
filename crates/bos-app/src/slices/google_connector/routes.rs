//! Connect-flow HTTP handlers. `connect` and `status`/`disconnect` are
//! operator-gated; `callback` is reached by browser redirect from Google and
//! is validated by the single-use CSRF state issued at `connect`.

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use super::service;
use super::store;
use crate::http::{error_response, now_ms, AppState};
use crate::store_core::StoreError;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/connectors/google/status", get(status))
        .route("/api/connectors/google/connect", get(connect))
        .route("/api/connectors/google/callback", get(callback))
        .route("/api/connectors/google/disconnect", post(disconnect))
        .route("/api/connectors/google/drive/folders", get(drive_folders))
}

async fn status(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let identity = match state.authenticate_operator(&headers) {
        Ok(identity) => identity,
        Err(denied) => return *denied,
    };
    let persistence = state.persistence.lock();
    let requested_scopes =
        service::requested_scopes_for_enabled_slices(|slice_id| state.slice_enabled(slice_id));
    match service::gmail_status(
        persistence.connection_ref(),
        &state.client_id,
        &identity.actor_id,
        &requested_scopes,
    ) {
        Ok(status) => Json(status).into_response(),
        Err(err) => store_error_response(err),
    }
}

#[derive(Debug, Deserialize)]
struct ConnectQuery {
    /// Operator token may arrive as a query param because this URL is opened
    /// in a browser tab, where setting an Authorization header is impractical.
    #[serde(default)]
    token: Option<String>,
}

async fn connect(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ConnectQuery>,
) -> Response {
    // The consent flow binds the resulting credential to WHO is connecting,
    // carried through the single-use CSRF state.
    let identity =
        match state.authenticate_operator_or_query_token(&headers, query.token.as_deref()) {
            Ok(identity) => identity,
            Err(denied) => return *denied,
        };
    let Some(app) = service::oauth_app_from_env() else {
        return error_response(StatusCode::CONFLICT, "oauth_app_unconfigured");
    };
    let csrf_state = service::generate_state();
    if let Err(err) = state.register_oauth_state("google", &csrf_state, &identity.actor_id) {
        return store_error_response(err);
    }
    let redirect_uri = service::redirect_uri_for_request(&headers);
    let requested_scopes =
        service::requested_scopes_for_enabled_slices(|slice_id| state.slice_enabled(slice_id));
    Redirect::temporary(&service::consent_url(
        &app,
        &redirect_uri,
        &requested_scopes,
        &csrf_state,
    ))
    .into_response()
}

#[derive(Debug, Deserialize)]
struct CallbackQuery {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

async fn callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<CallbackQuery>,
) -> Response {
    if let Some(err) = query.error {
        return Html(format!(
            "<h2>Google connect failed</h2><p>{}</p>",
            err.replace('<', "&lt;")
        ))
        .into_response();
    }
    let (Some(code), Some(csrf_state)) = (query.code, query.state) else {
        return error_response(StatusCode::BAD_REQUEST, "oauth_callback_missing_params");
    };
    // Single-use: consume validates AND removes, so a replayed callback fails.
    // The state carries which user initiated the connect.
    let user_id = match state.consume_oauth_state("google", &csrf_state) {
        Ok(Some(user_id)) => user_id,
        Ok(None) => return error_response(StatusCode::BAD_REQUEST, "oauth_state_invalid"),
        Err(err) => return store_error_response(err),
    };
    let Some(app) = service::oauth_app_from_env() else {
        return error_response(StatusCode::CONFLICT, "oauth_app_unconfigured");
    };
    let redirect_uri = service::redirect_uri_for_request(&headers);
    let exchange = tokio::task::spawn_blocking(move || {
        bos_integrations::google_oauth::exchange_authorization_code(
            &app.client_id,
            &app.client_secret,
            &redirect_uri,
            &code,
            None,
        )
    })
    .await;
    let grant = match exchange {
        Ok(Ok(grant)) => grant,
        Ok(Err(err)) => {
            tracing::warn!(error = ?err, "google oauth code exchange failed");
            return error_response(StatusCode::BAD_GATEWAY, "oauth_code_exchange_failed");
        }
        Err(_join) => {
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, "oauth_task_failed")
        }
    };
    let mut persistence = state.persistence.lock();
    let stored = store::store_credential(
        persistence.connection(),
        &state.client_id,
        &user_id,
        super::SERVICE_GMAIL,
        &grant.refresh_token,
        &grant.scopes,
        now_ms(),
    );
    match stored {
        Ok(_) => crate::http::connector_connected_page(
            "Google",
            "Your inbox and calendar are linked. New email will start arriving in a moment.",
        ),
        Err(err) => store_error_response(err),
    }
}

async fn disconnect(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let identity = match state.authenticate_operator(&headers) {
        Ok(identity) => identity,
        Err(denied) => return *denied,
    };
    let mut persistence = state.persistence.lock();
    match store::delete_credential(
        persistence.connection(),
        &state.client_id,
        &identity.actor_id,
        super::SERVICE_GMAIL,
        now_ms(),
    ) {
        Ok(_) => Json(serde_json::json!({"disconnected": true})).into_response(),
        Err(err) => store_error_response(err),
    }
}

#[derive(Debug, Deserialize)]
struct DriveFoldersQuery {
    #[serde(default)]
    q: Option<String>,
}

async fn drive_folders(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<DriveFoldersQuery>,
) -> Response {
    let identity = match state.authenticate_operator(&headers) {
        Ok(identity) => identity,
        Err(denied) => return *denied,
    };
    let persistence = state.persistence.lock();
    match service::drive_folder_options(
        persistence.connection_ref(),
        &state.client_id,
        &identity.actor_id,
        query.q.as_deref(),
    ) {
        Ok(response) => Json(response).into_response(),
        Err(err) => store_error_response(err),
    }
}

fn store_error_response(err: StoreError) -> Response {
    crate::http::store_error_response("google_connector", err)
}
