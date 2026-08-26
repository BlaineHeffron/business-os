//! HTTP transport for the optional BusinessOS MCP endpoint.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};

use super::service;
use crate::http::{error_response, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/agent-mcp", get(manifest))
        .route("/api/agent-mcp", post(mcp_post))
}

async fn manifest(State(state): State<AppState>) -> Response {
    Json(service::manifest(&state)).into_response()
}

async fn mcp_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(message): Json<serde_json::Value>,
) -> Response {
    if !service::enabled() {
        return error_response(StatusCode::NOT_FOUND, "route_not_found");
    }
    let auth = match state.authenticate_agent_mcp(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if let Some(error) = service::validate_http_request(&headers, &message) {
        return (StatusCode::BAD_REQUEST, Json(error)).into_response();
    }
    let mut response = match service::handle_request(state, auth, message) {
        service::McpHttpResponse::Json(value) => Json(value).into_response(),
        service::McpHttpResponse::Accepted => StatusCode::ACCEPTED.into_response(),
    };
    if let Some(protocol_version) = headers.get("mcp-protocol-version") {
        response
            .headers_mut()
            .insert("mcp-protocol-version", protocol_version.clone());
    }
    response
}
