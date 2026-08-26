//! Thin HTTP handlers for enrichment diagnostics.

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use bos_contracts::enrichment::EnrichmentRunsResponse;
use serde::Deserialize;

use crate::http::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/enrichment/runs", get(list_runs))
}

#[derive(Debug, Deserialize)]
struct RunsQuery {
    slice_id: Option<String>,
    draft_id: Option<String>,
    item_id: Option<String>,
    limit: Option<usize>,
}

async fn list_runs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<RunsQuery>,
) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    let persistence = state.persistence.lock();
    match super::store::list_runs(
        persistence.connection_ref(),
        &state.client_id,
        query.slice_id.as_deref(),
        query.draft_id.as_deref(),
        query.item_id.as_deref(),
        query.limit.unwrap_or(50),
    ) {
        Ok(runs) => Json(EnrichmentRunsResponse { runs }).into_response(),
        Err(err) => crate::http::store_error_response("enrichment", err),
    }
}
