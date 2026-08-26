//! Thin HTTP handlers for content plans.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bos_contracts::content_plans::{
    ContentCampaignGenerateRequest, ContentCampaignPublishRequest,
    ContentCampaignWorkspaceResponse, ContentDraftOverlapResponse, ContentInventoryArchiveRequest,
    ContentInventoryManualAddRequest, ContentInventoryRefreshRequest, ContentInventoryResponse,
    ContentInventoryStatus, ContentPlanItemCheckRequest, ContentPlanItemCreateRequest,
    ContentPlanItemMarkPublishedRequest, ContentPlanItemQueueRequest, ContentPlanItemUpdateRequest,
    ContentPlanItemsResponse, ContentPlanStatus,
};
use bos_contracts::work_queue::WorkItemStatus;
use serde::Deserialize;

use super::{service, store};
use crate::http::{error_response, mutation_response, now_ms, AppState};
use crate::slices::mutation_context::MutationContext;
use crate::store_core::StoreError;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/content-plans/items",
            get(items_list).post(item_create),
        )
        .route(
            "/api/content-plans/items/{plan_item_id}/update",
            post(item_update),
        )
        .route(
            "/api/content-plans/items/{plan_item_id}/queue",
            post(item_queue),
        )
        .route(
            "/api/content-plans/items/{plan_item_id}/campaign",
            get(campaign_workspace),
        )
        .route(
            "/api/content-plans/items/{plan_item_id}/generate",
            post(campaign_generate),
        )
        .route(
            "/api/content-plans/items/{plan_item_id}/publish-campaign",
            post(campaign_publish),
        )
        .route(
            "/api/content-plans/items/{plan_item_id}/check",
            post(item_check),
        )
        .route(
            "/api/content-plans/items/{plan_item_id}/mark-published",
            post(item_mark_published),
        )
        .route(
            "/api/content-plans/inventory",
            get(inventory_list).post(inventory_add),
        )
        .route(
            "/api/content-plans/draft-overlap/{draft_id}",
            get(draft_overlap),
        )
        .route(
            "/api/content-plans/inventory/refresh",
            post(inventory_refresh),
        )
        .route(
            "/api/content-plans/inventory/{inventory_id}/archive",
            post(inventory_archive),
        )
}

#[derive(Debug, Deserialize)]
struct ItemsQuery {
    #[serde(default)]
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct InventoryQuery {
    #[serde(default)]
    status: Option<String>,
}

async fn items_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ItemsQuery>,
) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    let status = match query.status.as_deref() {
        None => None,
        Some("planned") => Some(ContentPlanStatus::Planned),
        Some("queued") => Some(ContentPlanStatus::Queued),
        Some("published") => Some(ContentPlanStatus::Published),
        Some("cancelled") => Some(ContentPlanStatus::Cancelled),
        Some(_) => return error_response(StatusCode::BAD_REQUEST, "content_plan_status_invalid"),
    };
    let persistence = state.persistence.lock();
    match store::list_items(persistence.connection_ref(), &state.client_id, status, 100) {
        Ok(items) => Json(ContentPlanItemsResponse { items }).into_response(),
        Err(err) => store_error_response(err),
    }
}

async fn item_create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ContentPlanItemCreateRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let now = now_ms();
    let item = match service::item_from_create(&state.client_id, &request, now) {
        Ok(item) => item,
        Err(err) => return store_error_response(err),
    };
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let mut persistence = state.persistence.lock();
    let match_expr = service::collision_match_expression(&item);
    let item_key = service::canonical_key(None, &item.topic);
    let candidates = match store::collision_candidates(
        persistence.connection_ref(),
        &state.client_id,
        None,
        match_expr.as_deref(),
        &item_key,
        item.target_query.as_deref(),
    ) {
        Ok(candidates) => candidates,
        Err(err) => return store_error_response(err),
    };
    let summary = service::run_collision_check(&item, &candidates, now);
    match store::insert_item(
        persistence.connection(),
        &state.client_id,
        &actor_id,
        &item,
        &summary,
        &request.idempotency_key,
    ) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

async fn item_update(
    State(state): State<AppState>,
    Path(plan_item_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<ContentPlanItemUpdateRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let now = now_ms();
    let mut persistence = state.persistence.lock();
    let before = match store::get_item(
        persistence.connection_ref(),
        &state.client_id,
        &plan_item_id,
    ) {
        Ok(Some(item)) => item.item,
        Ok(None) => {
            return store_error_response(StoreError::Domain("content_plan_not_found".into()))
        }
        Err(err) => return store_error_response(err),
    };
    let after = match service::updated_item(&before, &request, now) {
        Ok(item) => item,
        Err(err) => return store_error_response(err),
    };
    let match_expr = service::collision_match_expression(&after);
    let item_key = service::canonical_key(None, &after.topic);
    let candidates = match store::collision_candidates(
        persistence.connection_ref(),
        &state.client_id,
        Some(&plan_item_id),
        match_expr.as_deref(),
        &item_key,
        after.target_query.as_deref(),
    ) {
        Ok(candidates) => candidates,
        Err(err) => return store_error_response(err),
    };
    let summary = service::run_collision_check(&after, &candidates, now);
    let ctx = MutationContext {
        client_id: &state.client_id,
        actor_id: &actor_id,
        expected_revision: request.expected_revision,
        idempotency_key: &request.idempotency_key,
        now_ms: now,
    };
    match store::update_item(persistence.connection(), ctx, &before, &after, &summary) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

async fn item_queue(
    State(state): State<AppState>,
    Path(plan_item_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<ContentPlanItemQueueRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let now = now_ms();
    let mut persistence = state.persistence.lock();
    let item = match store::get_item(
        persistence.connection_ref(),
        &state.client_id,
        &plan_item_id,
    ) {
        Ok(Some(item)) => item.item,
        Ok(None) => {
            return store_error_response(StoreError::Domain("content_plan_not_found".into()))
        }
        Err(err) => return store_error_response(err),
    };
    let match_expr = service::collision_match_expression(&item);
    let item_key = service::canonical_key(None, &item.topic);
    let candidates = match store::collision_candidates(
        persistence.connection_ref(),
        &state.client_id,
        Some(&plan_item_id),
        match_expr.as_deref(),
        &item_key,
        item.target_query.as_deref(),
    ) {
        Ok(candidates) => candidates,
        Err(err) => return store_error_response(err),
    };
    let summary = service::run_collision_check(&item, &candidates, now);
    let title = service::work_item_title(&item);
    let work_summary = service::work_item_summary(&item);
    let ctx = MutationContext {
        client_id: &state.client_id,
        actor_id: &actor_id,
        expected_revision: request.expected_revision,
        idempotency_key: &request.idempotency_key,
        now_ms: now,
    };
    match store::queue_item(
        persistence.connection(),
        ctx,
        &item,
        &summary,
        &title,
        &work_summary,
    ) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

async fn campaign_workspace(
    State(state): State<AppState>,
    Path(plan_item_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    let persistence = state.persistence.lock();
    let conn = persistence.connection_ref();
    let plan = match store::get_item(conn, &state.client_id, &plan_item_id) {
        Ok(Some(plan)) => plan,
        Ok(None) => return error_response(StatusCode::NOT_FOUND, "content_plan_not_found"),
        Err(err) => return store_error_response(err),
    };
    let content_draft = match plan.active_draft_id.as_deref() {
        Some(draft_id) => {
            match crate::slices::content_drafts::store::get_draft(conn, &state.client_id, draft_id)
            {
                Ok(draft) => draft,
                Err(err) => return store_error_response(err),
            }
        }
        None => None,
    };
    let social_proposal = match crate::slices::social_publishing::store::list_proposals(
        conn,
        &state.client_id,
        200,
    ) {
        Ok(proposals) => content_draft.as_ref().and_then(|draft| {
            proposals.into_iter().find(|proposal| {
                proposal.proposal.status
                    == bos_contracts::social_publishing::SocialProposalStatus::Staged
                    && proposal.proposal.source_content_draft_id.as_deref()
                        == Some(&draft.draft.draft_id)
                    && proposal.proposal.source_content_draft_revision == Some(draft.revision)
            })
        }),
        Err(err) => return store_error_response(err),
    };
    let preview_source =
        match crate::slices::social_publishing::store::list_sources(conn, &state.client_id, 200) {
            Ok(sources) => content_draft.as_ref().and_then(|draft| {
                sources.into_iter().find(|source| {
                    source.source_kind
                        == crate::slices::social_publishing::service::PREVIEW_SOURCE_KIND
                        && source.source_content_draft_id.as_deref() == Some(&draft.draft.draft_id)
                        && source.source_content_draft_revision == Some(draft.revision)
                })
            }),
            Err(err) => return store_error_response(err),
        };
    let publications =
        match store::list_campaign_publications(conn, &state.client_id, &plan_item_id, 20) {
            Ok(publications) => publications,
            Err(err) => return store_error_response(err),
        };
    let channels = match crate::slices::social_publishing::service::configured_channels() {
        Ok(channels) => channels,
        Err(StoreError::Domain(code)) if code == "social_channels_not_configured" => Vec::new(),
        Err(err) => return store_error_response(err),
    };
    Json(ContentCampaignWorkspaceResponse {
        plan,
        content_draft,
        social_proposal,
        social_generation_status: preview_source
            .as_ref()
            .map(|source| source.generation_status),
        social_generation_error: preview_source.and_then(|source| source.generation_error),
        publications,
        blog_publishing_available: crate::slices::content_drafts::service::publishing_available(),
        blog_live_enabled: crate::slices::content_drafts::service::publishing_live_enabled(
            conn,
            &state.client_id,
        ),
        social_configured: !channels.is_empty(),
        social_live_enabled: crate::slices::social_publishing::service::buffer_live_enabled(
            conn,
            &state.client_id,
        ),
        channels,
    })
    .into_response()
}

async fn campaign_generate(
    State(state): State<AppState>,
    Path(plan_item_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<ContentCampaignGenerateRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let scope = auth.scope.clone();
    let work_item_id = {
        let mut persistence = state.persistence.lock();
        let plan = match store::get_item(
            persistence.connection_ref(),
            &state.client_id,
            &plan_item_id,
        ) {
            Ok(Some(plan)) => plan,
            Ok(None) => return error_response(StatusCode::NOT_FOUND, "content_plan_not_found"),
            Err(err) => return store_error_response(err),
        };
        if plan.item.status == ContentPlanStatus::Planned {
            if plan.revision != request.expected_revision {
                return error_response(
                    StatusCode::CONFLICT,
                    "content_campaign_plan_revision_changed",
                );
            }
            let match_expr = service::collision_match_expression(&plan.item);
            let item_key = service::canonical_key(None, &plan.item.topic);
            let candidates = match store::collision_candidates(
                persistence.connection_ref(),
                &state.client_id,
                Some(&plan_item_id),
                match_expr.as_deref(),
                &item_key,
                plan.item.target_query.as_deref(),
            ) {
                Ok(candidates) => candidates,
                Err(err) => return store_error_response(err),
            };
            let now = now_ms();
            let summary = service::run_collision_check(&plan.item, &candidates, now);
            let outcome = match store::queue_item_for_generation(
                persistence.connection(),
                MutationContext {
                    client_id: &state.client_id,
                    actor_id: &actor_id,
                    expected_revision: Some(request.expected_revision),
                    idempotency_key: &format!("campaign-queue:{}", request.idempotency_key),
                    now_ms: now,
                },
                &plan.item,
                &summary,
                &service::work_item_title(&plan.item),
                &service::work_item_summary(&plan.item),
            ) {
                Ok(outcome) => outcome,
                Err(err) => return store_error_response(err),
            };
            if matches!(
                outcome,
                crate::store_core::MutationOutcome::RevisionConflict { .. }
            ) {
                return mutation_response(outcome);
            }
            store::work_item_id(&plan_item_id)
        } else if plan.item.status == ContentPlanStatus::Queued {
            let Some(work_item_id) = plan.item.work_item_id.clone() else {
                return error_response(StatusCode::CONFLICT, "content_campaign_work_item_missing");
            };
            let work_item = match crate::slices::work_queue::store::get_item_scoped(
                persistence.connection_ref(),
                &state.client_id,
                &work_item_id,
                &scope,
            ) {
                Ok(Some(item)) => item,
                Ok(None) => return error_response(StatusCode::NOT_FOUND, "work_item_not_found"),
                Err(err) => return store_error_response(err),
            };
            match work_item.item.status {
                WorkItemStatus::Accepted => {}
                WorkItemStatus::Open => {
                    let outcome = match crate::slices::work_queue::store::apply_item_action(
                        persistence.connection(),
                        crate::slices::work_queue::store::ItemActionContext {
                            client_id: &state.client_id,
                            actor_id: &actor_id,
                            scope: &scope,
                            expected_revision: Some(work_item.revision),
                            idempotency_key: &format!(
                                "campaign-accept:{}",
                                request.idempotency_key
                            ),
                            now_ms: now_ms(),
                        },
                        &work_item_id,
                        crate::slices::work_queue::store::ItemAction::Accept,
                    ) {
                        Ok(outcome) => outcome,
                        Err(err) => return store_error_response(err),
                    };
                    if matches!(
                        outcome,
                        crate::store_core::MutationOutcome::RevisionConflict { .. }
                    ) {
                        return mutation_response(outcome);
                    }
                }
                _ => {
                    return error_response(
                        StatusCode::CONFLICT,
                        "content_campaign_work_item_unavailable",
                    )
                }
            }
            work_item_id
        } else {
            return error_response(StatusCode::CONFLICT, "content_campaign_plan_not_queueable");
        }
    };

    crate::produce::run(
        state,
        crate::slices::content_drafts::service::Produce,
        &work_item_id,
        &format!("campaign-article:{}", request.idempotency_key),
        &actor_id,
        scope,
    )
    .await
}

async fn campaign_publish(
    State(state): State<AppState>,
    Path(plan_item_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<ContentCampaignPublishRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if !crate::slices::content_drafts::service::publishing_available() {
        return error_response(StatusCode::CONFLICT, "content_publish_adapter_unavailable");
    }
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let now = now_ms();
    let mut persistence = state.persistence.lock();
    let plan = match store::get_item(
        persistence.connection_ref(),
        &state.client_id,
        &plan_item_id,
    ) {
        Ok(Some(plan)) => plan,
        Ok(None) => return error_response(StatusCode::NOT_FOUND, "content_plan_not_found"),
        Err(err) => return store_error_response(err),
    };
    let approval = match service::prepare_campaign_publication(
        persistence.connection_ref(),
        &state.client_id,
        &plan.item,
        &request,
        &actor_id,
        now,
    ) {
        Ok(approval) => approval,
        Err(err) => return store_error_response(err),
    };
    match store::insert_campaign_publication(
        persistence.connection(),
        MutationContext {
            client_id: &state.client_id,
            actor_id: &actor_id,
            expected_revision: None,
            idempotency_key: &request.idempotency_key,
            now_ms: now,
        },
        &approval,
    ) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

async fn item_check(
    State(state): State<AppState>,
    Path(plan_item_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<ContentPlanItemCheckRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let now = now_ms();
    let mut persistence = state.persistence.lock();
    let item = match store::get_item(
        persistence.connection_ref(),
        &state.client_id,
        &plan_item_id,
    ) {
        Ok(Some(item)) => item.item,
        Ok(None) => {
            return store_error_response(StoreError::Domain("content_plan_not_found".into()))
        }
        Err(err) => return store_error_response(err),
    };
    let refresh_rows = match service::projected_inventory_rows(
        persistence.connection_ref(),
        &state.client_id,
        now,
    ) {
        Ok(rows) => rows,
        Err(err) => return store_error_response(err),
    };
    let refresh_key = check_refresh_idempotency_key(&plan_item_id, &request.idempotency_key);
    if let Err(err) = store::refresh_inventory(
        persistence.connection(),
        &state.client_id,
        &actor_id,
        &refresh_rows,
        &refresh_key,
        now,
    ) {
        return store_error_response(err);
    }
    let match_expr = service::collision_match_expression(&item);
    let item_key = service::canonical_key(None, &item.topic);
    let candidates = match store::collision_candidates(
        persistence.connection_ref(),
        &state.client_id,
        Some(&plan_item_id),
        match_expr.as_deref(),
        &item_key,
        item.target_query.as_deref(),
    ) {
        Ok(candidates) => candidates,
        Err(err) => return store_error_response(err),
    };
    let summary = service::run_collision_check(&item, &candidates, now);
    let ctx = MutationContext {
        client_id: &state.client_id,
        actor_id: &actor_id,
        expected_revision: request.expected_revision,
        idempotency_key: &request.idempotency_key,
        now_ms: now,
    };
    match store::persist_check(persistence.connection(), ctx, &item, &summary) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

async fn item_mark_published(
    State(state): State<AppState>,
    Path(plan_item_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<ContentPlanItemMarkPublishedRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    if request.published_url.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "published_url_required");
    }
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let now = now_ms();
    let mut persistence = state.persistence.lock();
    let item = match store::get_item(
        persistence.connection_ref(),
        &state.client_id,
        &plan_item_id,
    ) {
        Ok(Some(item)) => item.item,
        Ok(None) => {
            return store_error_response(StoreError::Domain("content_plan_not_found".into()))
        }
        Err(err) => return store_error_response(err),
    };
    let inventory_row = match service::published_plan_inventory_row(
        &state.client_id,
        &item,
        &request.published_url,
        now,
    ) {
        Ok(row) => row,
        Err(err) => return store_error_response(err),
    };
    let ctx = MutationContext {
        client_id: &state.client_id,
        actor_id: &actor_id,
        expected_revision: request.expected_revision,
        idempotency_key: &request.idempotency_key,
        now_ms: now,
    };
    match store::mark_published(
        persistence.connection(),
        ctx,
        &item,
        &request.published_url,
        &inventory_row,
    ) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

async fn inventory_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<InventoryQuery>,
) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    let status = match query.status.as_deref() {
        None => None,
        Some("pipeline") => Some(ContentInventoryStatus::Pipeline),
        Some("published") => Some(ContentInventoryStatus::Published),
        Some("archived") => Some(ContentInventoryStatus::Archived),
        Some(_) => {
            return error_response(StatusCode::BAD_REQUEST, "content_inventory_status_invalid")
        }
    };
    let persistence = state.persistence.lock();
    match store::list_inventory(persistence.connection_ref(), &state.client_id, status, 200) {
        Ok(items) => Json(ContentInventoryResponse { items }).into_response(),
        Err(err) => store_error_response(err),
    }
}

async fn draft_overlap(
    State(state): State<AppState>,
    Path(draft_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    let now = now_ms();
    let persistence = state.persistence.lock();
    let draft = match crate::slices::content_drafts::store::get_draft(
        persistence.connection_ref(),
        &state.client_id,
        &draft_id,
    ) {
        Ok(Some(draft)) => draft.draft,
        Ok(None) => {
            return store_error_response(StoreError::Domain("content_draft_not_found".into()))
        }
        Err(err) => return store_error_response(err),
    };
    let match_expr = service::draft_collision_match_expression(
        &draft.title,
        &draft.body_markdown,
        draft.target_query.as_deref(),
    );
    let draft_key = service::canonical_key(None, &draft.title);
    let candidates = match store::collision_candidates(
        persistence.connection_ref(),
        &state.client_id,
        None,
        match_expr.as_deref(),
        &draft_key,
        draft.target_query.as_deref(),
    ) {
        Ok(candidates) => candidates,
        Err(err) => return store_error_response(err),
    };
    let summary = service::run_draft_collision_check(
        &draft.draft_id,
        &draft.item_id,
        &draft.title,
        &draft.body_markdown,
        draft.target_query.as_deref(),
        &candidates,
        now,
    );
    Json(ContentDraftOverlapResponse { summary }).into_response()
}

async fn inventory_add(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ContentInventoryManualAddRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let now = now_ms();
    let row = match service::manual_inventory_row(&state.client_id, &request, now) {
        Ok(row) => row,
        Err(err) => return store_error_response(err),
    };
    let ctx = MutationContext {
        client_id: &state.client_id,
        actor_id: &actor_id,
        expected_revision: None,
        idempotency_key: &request.idempotency_key,
        now_ms: now,
    };
    let mut persistence = state.persistence.lock();
    match store::add_manual_inventory(persistence.connection(), ctx, &row) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

async fn inventory_refresh(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ContentInventoryRefreshRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let now = now_ms();
    let mut persistence = state.persistence.lock();
    let rows = match service::projected_inventory_rows(
        persistence.connection_ref(),
        &state.client_id,
        now,
    ) {
        Ok(rows) => rows,
        Err(err) => return store_error_response(err),
    };
    match store::refresh_inventory(
        persistence.connection(),
        &state.client_id,
        &actor_id,
        &rows,
        &request.idempotency_key,
        now,
    ) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

async fn inventory_archive(
    State(state): State<AppState>,
    Path(inventory_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<ContentInventoryArchiveRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let now = now_ms();
    let mut persistence = state.persistence.lock();
    let before = match store::get_inventory(
        persistence.connection_ref(),
        &state.client_id,
        &inventory_id,
    ) {
        Ok(Some(item)) => item.item,
        Ok(None) => {
            return store_error_response(StoreError::Domain("content_inventory_not_found".into()))
        }
        Err(err) => return store_error_response(err),
    };
    let ctx = MutationContext {
        client_id: &state.client_id,
        actor_id: &actor_id,
        expected_revision: request.expected_revision,
        idempotency_key: &request.idempotency_key,
        now_ms: now,
    };
    match store::archive_inventory(persistence.connection(), ctx, &before) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

fn store_error_response(err: StoreError) -> Response {
    crate::http::store_error_response("content_plans", err)
}

fn check_refresh_idempotency_key(plan_item_id: &str, check_idempotency_key: &str) -> String {
    format!("content_inventory_refresh:check:{plan_item_id}:{check_idempotency_key}")
}

#[cfg(test)]
mod tests {
    use super::check_refresh_idempotency_key;

    #[test]
    fn check_refresh_key_is_deterministic_child_of_check_request() {
        let first = check_refresh_idempotency_key("plan-1", "check-key");
        let second = check_refresh_idempotency_key("plan-1", "check-key");

        assert_eq!(first, second);
        assert_eq!(first, "content_inventory_refresh:check:plan-1:check-key");
    }
}
