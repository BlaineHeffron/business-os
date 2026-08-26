use axum::body::Body;
use axum::http::{Request, StatusCode};
use bos_contracts::content_drafts::{ContentDraftStatus, ContentEvidenceSnippet};
use bos_contracts::content_plans::{
    ContentCampaignLaunchMode, ContentCampaignPublicationStatus, ContentCampaignPublishRequest,
    ContentInventoryManualAddRequest, ContentInventorySourceKind, ContentInventoryStatus,
    ContentPlanDraftState, ContentPlanItem, ContentPlanItemCreateRequest,
    ContentPlanItemUpdateRequest, ContentPlanStatus,
};
use bos_contracts::receipt::ActorKindDto;
use bos_contracts::social_publishing::{
    SocialProposalStageRequest, SocialProposalTarget, SocialProposalTargetInput,
    SocialScheduleMode, SocialUtmParameters,
};
use bos_contracts::work_queue::{WorkItem, WorkItemStatus};
use tower::ServiceExt;

use super::{service, store};
use crate::http::{
    build_router,
    test_support::{test_state, EnvGuard},
};
use crate::slices::mutation_context::MutationContext;
use crate::store_core::MutationOutcome;

const CLIENT: &str = "test-client";

fn create_request(topic: &str, key: &str) -> ContentPlanItemCreateRequest {
    ContentPlanItemCreateRequest {
        topic: topic.to_string(),
        angle: Some("Practical checklist".to_string()),
        format: Some("Blog post".to_string()),
        target_query: Some("epoxy floor prep".to_string()),
        audience: Some("Facility managers".to_string()),
        notes: Some("Cover degreasing and etching.".to_string()),
        idempotency_key: key.to_string(),
        actor_id: None,
    }
}

fn insert_plan(
    state: &crate::http::AppState,
    request: ContentPlanItemCreateRequest,
    now_ms: u64,
) -> String {
    let item = service::item_from_create(CLIENT, &request, now_ms).expect("item");
    let mut persistence = state.persistence.lock();
    let candidates = collision_candidates_for(persistence.connection_ref(), &item, None);
    let summary = service::run_collision_check(&item, &candidates, now_ms);
    store::insert_item(
        persistence.connection(),
        CLIENT,
        "operator",
        &item,
        &summary,
        &request.idempotency_key,
    )
    .expect("insert");
    item.plan_item_id
}

fn collision_candidates_for(
    conn: &rusqlite::Connection,
    item: &ContentPlanItem,
    exclude_plan_item_id: Option<&str>,
) -> Vec<store::CollisionCandidate> {
    let match_expr = service::collision_match_expression(item);
    let item_key = service::canonical_key(None, &item.topic);
    store::collision_candidates(
        conn,
        CLIENT,
        exclude_plan_item_id,
        match_expr.as_deref(),
        &item_key,
        item.target_query.as_deref(),
    )
    .expect("candidates")
}

fn add_manual_inventory(
    state: &crate::http::AppState,
    title: &str,
    target_query: &str,
    key: &str,
) -> String {
    let request = ContentInventoryManualAddRequest {
        title: title.to_string(),
        target_query: Some(target_query.to_string()),
        url: Some(format!(
            "https://example.com/{}",
            service::normalized_phrase(title).replace(' ', "-")
        )),
        summary: Some("Published content inventory row.".to_string()),
        idempotency_key: key.to_string(),
        actor_id: None,
    };
    let row = service::manual_inventory_row(CLIENT, &request, 1_000).expect("manual row");
    let inventory_id = row.inventory_id.clone();
    let mut persistence = state.persistence.lock();
    store::add_manual_inventory(
        persistence.connection(),
        MutationContext {
            client_id: CLIENT,
            actor_id: "operator",
            expected_revision: None,
            idempotency_key: &request.idempotency_key,
            now_ms: 1_000,
        },
        &row,
    )
    .expect("add manual");
    inventory_id
}

async fn route_json(
    router: &axum::Router,
    request: Request<Body>,
) -> (StatusCode, serde_json::Value) {
    let response = router.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    let body = serde_json::from_slice(&bytes).expect("json response");
    (status, body)
}

fn json_request(method: &str, uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request")
}

fn plan_revision(state: &crate::http::AppState, plan_item_id: &str) -> u64 {
    let persistence = state.persistence.lock();
    store::get_item(persistence.connection_ref(), CLIENT, plan_item_id)
        .expect("get plan")
        .expect("plan")
        .revision
}

fn draft_work_item(item_id: &str, source_kind: &str, source_ref: &str) -> WorkItem {
    WorkItem {
        item_id: item_id.to_string(),
        source_kind: source_kind.to_string(),
        source_ref: source_ref.to_string(),
        category_id: service::CATEGORY_ID.to_string(),
        title: "Content draft".to_string(),
        summary: String::new(),
        packet_kinds: vec![crate::slices::content_drafts::service::PACKET_KIND.to_string()],
        status: WorkItemStatus::Accepted,
        accept_actor: None,
        ai_suggested: false,
        rationale: String::new(),
        produce_guidance: String::new(),
        source_user_id: None,
        assignee_user_id: None,
        visible_to_user_ids: Vec::new(),
        created_at_ms: 1_000,
        updated_at_ms: 1_000,
    }
}

struct DraftSeed<'a> {
    item_id: &'a str,
    source_kind: &'a str,
    source_ref: &'a str,
    title: &'a str,
    body_markdown: &'a str,
    target_query: &'a str,
}

fn stage_content_draft(
    state: &crate::http::AppState,
    item_id: &str,
    source_kind: &str,
    source_ref: &str,
    title: &str,
    body_markdown: &str,
    target_query: &str,
) -> String {
    stage_content_draft_at(
        state,
        DraftSeed {
            item_id,
            source_kind,
            source_ref,
            title,
            body_markdown,
            target_query,
        },
        2_000,
    )
}

fn stage_content_draft_at(
    state: &crate::http::AppState,
    seed: DraftSeed<'_>,
    now_ms: u64,
) -> String {
    let evidence = vec![ContentEvidenceSnippet {
        snippet_id: "doc-1:0".to_string(),
        file_id: "doc-1".to_string(),
        doc_title: "Evidence".to_string(),
        heading_path: Vec::new(),
        text: "Degrease and etch the slab before coating.".to_string(),
        web_view_link: None,
    }];
    let fill_value = serde_json::json!({
        "title": seed.title,
        "body_markdown": seed.body_markdown,
        "target_query": seed.target_query,
        "meta_description": "Draft meta description.",
        "claims": [
            {"text": "Degrease and etch the slab before coating", "snippet_ids": ["doc-1:0"]}
        ],
        "confidence": "high"
    });
    let fill = crate::slices::content_drafts::service::parse_grounded_draft_response(&fill_value)
        .expect("fill");
    let (claims, gate) =
        crate::slices::content_drafts::service::evaluate_citation_gate(&fill.claims, &evidence);
    let draft = crate::slices::content_drafts::service::draft_from_fill(
        &draft_work_item(seed.item_id, seed.source_kind, seed.source_ref),
        &fill,
        evidence,
        claims,
        gate,
        1,
        "model-test",
        now_ms,
    );
    let draft_id = draft.draft_id.clone();
    let mut persistence = state.persistence.lock();
    crate::slices::content_drafts::store::insert_draft(
        persistence.connection(),
        CLIENT,
        "operator",
        &draft,
        &format!("stage:{draft_id}"),
    )
    .expect("stage draft");
    draft_id
}

#[tokio::test]
async fn campaign_workspace_route_returns_the_composed_plan_and_not_found_envelope() {
    let state = test_state();
    let plan_id = insert_plan(
        &state,
        create_request("Route-level campaign workspace", "route-workspace"),
        1_000,
    );
    let router = build_router(state);

    let (missing_status, missing) = route_json(
        &router,
        Request::get("/api/content-plans/items/missing/campaign")
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert_eq!(missing_status, StatusCode::NOT_FOUND);
    assert_eq!(missing["error"], "content_plan_not_found");

    let (status, body) = route_json(
        &router,
        Request::get(format!("/api/content-plans/items/{plan_id}/campaign"))
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["plan"]["item"]["plan_item_id"], plan_id);
    assert_eq!(body["plan"]["item"]["status"], "planned");
    assert!(body["content_draft"].is_null());
    assert!(body["publications"].as_array().is_some_and(Vec::is_empty));
}

#[tokio::test]
async fn campaign_generate_route_rejects_blank_keys_and_stale_plan_revisions_before_produce() {
    let state = test_state();
    let plan_id = insert_plan(
        &state,
        create_request("Route-level campaign generation", "route-generate"),
        1_000,
    );
    let revision = plan_revision(&state, &plan_id);
    let router = build_router(state);
    let uri = format!("/api/content-plans/items/{plan_id}/generate");

    let (missing_status, missing) = route_json(
        &router,
        json_request(
            "POST",
            "/api/content-plans/items/missing/generate",
            serde_json::json!({
                "expected_revision": 1,
                "idempotency_key": "route-generate-missing"
            }),
        ),
    )
    .await;
    assert_eq!(missing_status, StatusCode::NOT_FOUND);
    assert_eq!(missing["error"], "content_plan_not_found");

    let (blank_status, blank) = route_json(
        &router,
        json_request(
            "POST",
            &uri,
            serde_json::json!({
                "expected_revision": revision,
                "idempotency_key": ""
            }),
        ),
    )
    .await;
    assert_eq!(blank_status, StatusCode::BAD_REQUEST);
    assert_eq!(blank["error"], "idempotency_key_required");

    let (stale_status, stale) = route_json(
        &router,
        json_request(
            "POST",
            &uri,
            serde_json::json!({
                "expected_revision": revision + 1,
                "idempotency_key": "route-generate-stale"
            }),
        ),
    )
    .await;
    assert_eq!(stale_status, StatusCode::CONFLICT);
    assert_eq!(stale["error"], "content_campaign_plan_revision_changed");
}

#[tokio::test]
async fn item_update_route_persists_the_revisioned_edit_and_reports_stale_conflicts() {
    let state = test_state();
    let plan_id = insert_plan(
        &state,
        create_request("Original route topic", "route-update-create"),
        1_000,
    );
    let revision = plan_revision(&state, &plan_id);
    let router = build_router(state.clone());
    let uri = format!("/api/content-plans/items/{plan_id}/update");
    let update = serde_json::json!({
        "topic": "Updated through the HTTP boundary",
        "angle": "Operator angle",
        "format": "Guide",
        "target_query": "route update query",
        "audience": "Operators",
        "notes": "Persist this exact edit.",
        "expected_revision": revision,
        "idempotency_key": "route-update-apply"
    });

    let (status, applied) = route_json(&router, json_request("POST", &uri, update.clone())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(applied["outcome"], "applied");
    assert_eq!(applied["revision"], revision + 1);

    let mut stale_update = update;
    stale_update["idempotency_key"] = serde_json::json!("route-update-stale");
    let (stale_status, stale) = route_json(&router, json_request("POST", &uri, stale_update)).await;
    assert_eq!(stale_status, StatusCode::CONFLICT);
    assert_eq!(stale["outcome"], "revision_conflict");

    let persistence = state.persistence.lock();
    let stored = store::get_item(persistence.connection_ref(), CLIENT, &plan_id)
        .expect("get plan")
        .expect("plan");
    assert_eq!(stored.item.topic, "Updated through the HTTP boundary");
    assert_eq!(stored.revision, revision + 1);
    let applied_receipts: i64 = persistence
        .connection_ref()
        .query_row(
            "SELECT COUNT(*) FROM receipts WHERE client_id = ?1 AND entity_kind = ?2 \
             AND entity_id = ?3 AND change_kind = 'update' AND outcome = 'applied'",
            rusqlite::params![CLIENT, store::PLAN_ENTITY_KIND, plan_id],
            |row| row.get(0),
        )
        .expect("update receipt count");
    assert_eq!(applied_receipts, 1);
}

#[tokio::test]
async fn item_check_route_replays_without_a_second_check_mutation() {
    let state = test_state();
    let plan_id = insert_plan(
        &state,
        create_request("Route-level collision refresh", "route-check-create"),
        1_000,
    );
    let revision = plan_revision(&state, &plan_id);
    let router = build_router(state.clone());
    let uri = format!("/api/content-plans/items/{plan_id}/check");
    let request = serde_json::json!({
        "expected_revision": revision,
        "idempotency_key": "route-check"
    });

    let (first_status, first) =
        route_json(&router, json_request("POST", &uri, request.clone())).await;
    assert_eq!(first_status, StatusCode::OK);
    assert_eq!(first["outcome"], "applied");
    assert_eq!(first["revision"], revision + 1);

    let (replay_status, replay) = route_json(&router, json_request("POST", &uri, request)).await;
    assert_eq!(replay_status, StatusCode::OK);
    assert_eq!(replay["outcome"], "replayed_idempotent");
    assert_ne!(replay["receipt_id"], first["receipt_id"]);
    assert_eq!(replay["revision"], first["revision"]);

    let persistence = state.persistence.lock();
    let stored = store::get_item(persistence.connection_ref(), CLIENT, &plan_id)
        .expect("get plan")
        .expect("plan");
    assert_eq!(stored.revision, revision + 1);
    assert!(stored.item.collision_summary.is_some());
    let applied_checks: i64 = persistence
        .connection_ref()
        .query_row(
            "SELECT COUNT(*) FROM receipts WHERE client_id = ?1 AND entity_kind = ?2 \
             AND entity_id = ?3 AND change_kind = 'check' AND outcome = 'applied'",
            rusqlite::params![CLIENT, store::PLAN_ENTITY_KIND, plan_id],
            |row| row.get(0),
        )
        .expect("check receipt count");
    assert_eq!(applied_checks, 1);
}

#[tokio::test]
async fn item_mark_published_route_validates_then_atomically_publishes_inventory_once() {
    let state = test_state();
    let plan_id = insert_plan(
        &state,
        create_request("Route-level published plan", "route-publish-create"),
        1_000,
    );
    let revision = plan_revision(&state, &plan_id);
    let router = build_router(state.clone());
    let uri = format!("/api/content-plans/items/{plan_id}/mark-published");

    let (blank_status, blank) = route_json(
        &router,
        json_request(
            "POST",
            &uri,
            serde_json::json!({
                "published_url": " ",
                "expected_revision": revision,
                "idempotency_key": "route-publish-blank"
            }),
        ),
    )
    .await;
    assert_eq!(blank_status, StatusCode::BAD_REQUEST);
    assert_eq!(blank["error"], "published_url_required");

    let publish = serde_json::json!({
        "published_url": "https://example.com/route-level-published-plan",
        "expected_revision": revision,
        "idempotency_key": "route-publish-apply"
    });
    let (status, applied) = route_json(&router, json_request("POST", &uri, publish.clone())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(applied["outcome"], "applied");

    // Status is checked before store_core idempotency, so a second POST cannot replay.
    let (retry_status, retry) = route_json(&router, json_request("POST", &uri, publish)).await;
    assert_eq!(retry_status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(retry["error"], "content_plan_not_publishable");

    let persistence = state.persistence.lock();
    let stored = store::get_item(persistence.connection_ref(), CLIENT, &plan_id)
        .expect("get plan")
        .expect("plan");
    assert_eq!(stored.item.status, ContentPlanStatus::Published);
    assert_eq!(
        stored.item.published_url.as_deref(),
        Some("https://example.com/route-level-published-plan")
    );
    let inventory = store::list_inventory(
        persistence.connection_ref(),
        CLIENT,
        Some(ContentInventoryStatus::Published),
        20,
    )
    .expect("published inventory");
    assert_eq!(inventory.len(), 1);
    assert_eq!(inventory[0].item.source_ref, plan_id);
    assert_eq!(
        inventory[0].item.url.as_deref(),
        Some("https://example.com/route-level-published-plan")
    );
    let applied_publications: i64 = persistence
        .connection_ref()
        .query_row(
            "SELECT COUNT(*) FROM receipts WHERE client_id = ?1 AND entity_kind = ?2 \
             AND entity_id = ?3 AND change_kind = 'mark_published' AND outcome = 'applied'",
            rusqlite::params![CLIENT, store::PLAN_ENTITY_KIND, plan_id],
            |row| row.get(0),
        )
        .expect("mark-published receipt count");
    assert_eq!(applied_publications, 1);
}

#[test]
fn create_runs_advisory_check_against_existing_plans() {
    let state = test_state();
    let first_id = insert_plan(
        &state,
        create_request("Epoxy Floor Prep Guide", "plan-one"),
        1_000,
    );
    let second_id = insert_plan(
        &state,
        create_request("Concrete Coating Preparation", "plan-two"),
        2_000,
    );

    let persistence = state.persistence.lock();
    let second = store::get_item(persistence.connection_ref(), CLIENT, &second_id)
        .expect("get")
        .expect("second");
    let summary = second.item.collision_summary.expect("collision summary");

    assert_eq!(second.item.status, ContentPlanStatus::Planned);
    assert!(summary
        .matches
        .iter()
        .any(
            |candidate| candidate.inventory_id == format!("plan:{first_id}")
                && candidate.reason == "exact_query"
        ));

    let receipt_after_json: String = persistence
        .connection_ref()
        .query_row(
            "SELECT after_json FROM receipts \
             WHERE client_id = ?1 AND entity_kind = ?2 AND entity_id = ?3 \
               AND change_kind = 'create' AND outcome = 'applied'",
            rusqlite::params![CLIENT, store::PLAN_ENTITY_KIND, second_id],
            |row| row.get(0),
        )
        .expect("create receipt payload");
    let receipt_after: serde_json::Value =
        serde_json::from_str(&receipt_after_json).expect("receipt after json");
    assert!(
        receipt_after["collision_summary"]["matches"]
            .as_array()
            .is_some_and(|matches| !matches.is_empty()),
        "create receipt must include persisted advisory collision summary"
    );
}

#[test]
fn update_reruns_check_and_derived_draft_state_is_read_time_only() {
    let state = test_state();
    let first_id = insert_plan(
        &state,
        create_request("Cabinet Painting Guide", "plan-update-one"),
        1_000,
    );
    let second_id = insert_plan(
        &state,
        create_request("Kitchen Cabinet Prep", "plan-update-two"),
        2_000,
    );
    let before = {
        let persistence = state.persistence.lock();
        store::get_item(persistence.connection_ref(), CLIENT, &second_id)
            .expect("get")
            .expect("second")
    };
    let request = ContentPlanItemUpdateRequest {
        topic: "Cabinet Painting Process".to_string(),
        angle: Some("Preparation mistakes".to_string()),
        format: Some("Landing page".to_string()),
        target_query: Some("cabinet painting guide".to_string()),
        audience: None,
        notes: Some("Compare against the existing cabinet plan.".to_string()),
        expected_revision: Some(before.revision),
        idempotency_key: "plan-update-two-apply".to_string(),
        actor_id: None,
    };
    let after = service::updated_item(&before.item, &request, 3_000).expect("updated");
    let mut persistence = state.persistence.lock();
    let candidates =
        collision_candidates_for(persistence.connection_ref(), &after, Some(&second_id));
    let summary = service::run_collision_check(&after, &candidates, 3_000);
    let ctx = MutationContext {
        client_id: CLIENT,
        actor_id: "operator",
        expected_revision: request.expected_revision,
        idempotency_key: &request.idempotency_key,
        now_ms: 3_000,
    };
    store::update_item(
        persistence.connection(),
        ctx,
        &before.item,
        &after,
        &summary,
    )
    .expect("update");
    let updated = store::get_item(persistence.connection_ref(), CLIENT, &second_id)
        .expect("get updated")
        .expect("updated");

    assert_eq!(updated.draft_state, ContentPlanDraftState::None);
    assert!(updated
        .item
        .collision_summary
        .expect("summary")
        .matches
        .iter()
        .any(|candidate| candidate.inventory_id == format!("plan:{first_id}")));
}

#[test]
fn queue_sets_plan_status_and_inserts_work_item_in_one_mutation() {
    let state = test_state();
    let plan_id = insert_plan(
        &state,
        create_request("Epoxy Floor Prep Guide", "plan-queue"),
        1_000,
    );
    let current = {
        let persistence = state.persistence.lock();
        store::get_item(persistence.connection_ref(), CLIENT, &plan_id)
            .expect("get")
            .expect("plan")
    };
    let mut persistence = state.persistence.lock();
    let summary = service::run_collision_check(&current.item, &[], 2_000);
    let title = service::work_item_title(&current.item);
    let work_summary = service::work_item_summary(&current.item);
    let ctx = MutationContext {
        client_id: CLIENT,
        actor_id: "operator",
        expected_revision: Some(current.revision),
        idempotency_key: "queue-plan",
        now_ms: 2_000,
    };
    let outcome = store::queue_item(
        persistence.connection(),
        ctx,
        &current.item,
        &summary,
        &title,
        &work_summary,
    )
    .expect("queue");
    assert!(matches!(outcome, MutationOutcome::Applied { .. }));

    let queued = store::get_item(persistence.connection_ref(), CLIENT, &plan_id)
        .expect("get queued")
        .expect("queued");
    assert_eq!(queued.item.status, ContentPlanStatus::Queued);
    let work_item_id = queued.item.work_item_id.expect("work item id");
    let work_item = crate::slices::work_queue::store::get_item_unscoped(
        persistence.connection_ref(),
        CLIENT,
        &work_item_id,
    )
    .expect("work item")
    .expect("work item");

    assert_eq!(
        work_item.item.source_kind,
        crate::slices::content_plans::SOURCE_KIND_CONTENT_PLAN_ITEM
    );
    assert_eq!(work_item.item.source_ref, plan_id);
    assert_eq!(work_item.item.status, WorkItemStatus::Open);
    assert_eq!(
        work_item.item.packet_kinds,
        vec!["content_draft".to_string()]
    );
    assert_eq!(work_item.revision, 1);
}

#[test]
fn queue_only_from_planned() {
    let state = test_state();
    let plan_id = insert_plan(
        &state,
        create_request("Epoxy Floor Prep Guide", "plan-queue-once"),
        1_000,
    );
    let current = {
        let persistence = state.persistence.lock();
        store::get_item(persistence.connection_ref(), CLIENT, &plan_id)
            .expect("get")
            .expect("plan")
    };
    {
        let mut persistence = state.persistence.lock();
        let summary = service::run_collision_check(&current.item, &[], 2_000);
        let ctx = MutationContext {
            client_id: CLIENT,
            actor_id: "operator",
            expected_revision: Some(current.revision),
            idempotency_key: "queue-once",
            now_ms: 2_000,
        };
        store::queue_item(
            persistence.connection(),
            ctx,
            &current.item,
            &summary,
            &service::work_item_title(&current.item),
            &service::work_item_summary(&current.item),
        )
        .expect("queue once");
    }
    let queued = {
        let persistence = state.persistence.lock();
        store::get_item(persistence.connection_ref(), CLIENT, &plan_id)
            .expect("get queued")
            .expect("queued")
    };
    let mut persistence = state.persistence.lock();
    let summary = service::run_collision_check(&queued.item, &[], 3_000);
    let ctx = MutationContext {
        client_id: CLIENT,
        actor_id: "operator",
        expected_revision: Some(queued.revision),
        idempotency_key: "queue-twice",
        now_ms: 3_000,
    };
    let err = store::queue_item(
        persistence.connection(),
        ctx,
        &queued.item,
        &summary,
        &service::work_item_title(&queued.item),
        &service::work_item_summary(&queued.item),
    )
    .expect_err("second queue refused");
    assert!(matches!(
        err,
        crate::store_core::StoreError::Domain(code) if code == "content_plan_not_planned"
    ));
}

#[test]
fn manual_inventory_add_writes_item_and_fts_in_one_mutation() {
    let state = test_state();
    let request = ContentInventoryManualAddRequest {
        title: "Epoxy Floor Prep Guide".to_string(),
        target_query: Some("epoxy floor prep".to_string()),
        url: Some("https://example.com/guides/epoxy-floor-prep".to_string()),
        summary: Some("Published prep checklist.".to_string()),
        idempotency_key: "manual-inventory".to_string(),
        actor_id: None,
    };
    let row = service::manual_inventory_row(CLIENT, &request, 1_000).expect("manual row");
    let mut persistence = state.persistence.lock();
    let ctx = MutationContext {
        client_id: CLIENT,
        actor_id: "operator",
        expected_revision: None,
        idempotency_key: &request.idempotency_key,
        now_ms: 1_000,
    };
    store::add_manual_inventory(persistence.connection(), ctx, &row).expect("add manual");

    let stored = store::get_inventory(persistence.connection_ref(), CLIENT, &row.inventory_id)
        .expect("get inventory")
        .expect("inventory");
    assert_eq!(stored.item.source_kind, ContentInventorySourceKind::Manual);
    assert_eq!(stored.item.status, ContentInventoryStatus::Published);
    assert_eq!(stored.revision, 1);
    let fts_count: i64 = persistence
        .connection_ref()
        .query_row(
            "SELECT COUNT(*) FROM content_inventory_fts WHERE client_id = ?1 AND inventory_id = ?2",
            rusqlite::params![CLIENT, row.inventory_id],
            |row| row.get(0),
        )
        .expect("fts count");
    assert_eq!(fts_count, 1);
}

#[test]
fn refresh_inventory_is_idempotent_by_canonical_key_and_updates_fts() {
    let state = test_state();
    {
        use crate::slices::search_console::store::{DimensionMetricRow, SnapshotWindow};
        use bos_contracts::search_console::SearchConsoleMetricTotals;
        let mut persistence = state.persistence.lock();
        let dimensions = vec![DimensionMetricRow {
            date: "2026-07-01".to_string(),
            dimension_type: "page".to_string(),
            dimension_value: "https://example.com/guides/epoxy-floor-prep".to_string(),
            is_branded: false,
            metrics: SearchConsoleMetricTotals {
                clicks: 4,
                impressions: 40,
                ctr_micros: 0,
                position_micros: 0,
            },
        }];
        crate::slices::search_console::store::replace_window(
            persistence.connection(),
            CLIENT,
            "sc-domain:example.com",
            SnapshotWindow {
                start_date: "2026-07-01",
                end_date: "2026-07-01",
                daily: &[],
                dimensions: &dimensions,
            },
            1_000,
        )
        .expect("seed search console page");
    }
    let mut persistence = state.persistence.lock();
    let rows = service::projected_inventory_rows(persistence.connection_ref(), CLIENT, 2_000)
        .expect("project rows");
    assert_eq!(rows.len(), 1);
    let inventory_id = rows[0].inventory_id.clone();
    store::refresh_inventory(
        persistence.connection(),
        CLIENT,
        "operator",
        &rows,
        "refresh-one",
        2_000,
    )
    .expect("refresh one");
    let rows = service::projected_inventory_rows(persistence.connection_ref(), CLIENT, 3_000)
        .expect("project rows again");
    store::refresh_inventory(
        persistence.connection(),
        CLIENT,
        "operator",
        &rows,
        "refresh-two",
        3_000,
    )
    .expect("refresh two");

    let count: i64 = persistence
        .connection_ref()
        .query_row(
            "SELECT COUNT(*) FROM content_inventory_items WHERE client_id = ?1 AND canonical_key = ?2",
            rusqlite::params![CLIENT, rows[0].canonical_key],
            |row| row.get(0),
        )
        .expect("inventory count");
    assert_eq!(count, 1);
    assert_eq!(rows[0].inventory_id, inventory_id);
    let fts_count: i64 = persistence
        .connection_ref()
        .query_row(
            "SELECT COUNT(*) FROM content_inventory_fts WHERE client_id = ?1 AND inventory_id = ?2",
            rusqlite::params![CLIENT, inventory_id],
            |row| row.get(0),
        )
        .expect("fts count");
    assert_eq!(fts_count, 1);
}

#[test]
fn mark_published_updates_plan_and_inventory_in_one_mutation() {
    let state = test_state();
    let plan_id = insert_plan(
        &state,
        create_request("Epoxy Floor Prep Guide", "plan-published"),
        1_000,
    );
    let current = {
        let persistence = state.persistence.lock();
        store::get_item(persistence.connection_ref(), CLIENT, &plan_id)
            .expect("get")
            .expect("plan")
    };
    let inventory_row = service::published_plan_inventory_row(
        CLIENT,
        &current.item,
        "https://example.com/guides/epoxy-floor-prep",
        2_000,
    )
    .expect("inventory row");
    let mut persistence = state.persistence.lock();
    let ctx = MutationContext {
        client_id: CLIENT,
        actor_id: "operator",
        expected_revision: Some(current.revision),
        idempotency_key: "mark-published",
        now_ms: 2_000,
    };
    store::mark_published(
        persistence.connection(),
        ctx,
        &current.item,
        "https://example.com/guides/epoxy-floor-prep",
        &inventory_row,
    )
    .expect("mark published");

    let published = store::get_item(persistence.connection_ref(), CLIENT, &plan_id)
        .expect("get published")
        .expect("published");
    assert_eq!(published.item.status, ContentPlanStatus::Published);
    assert_eq!(
        published.item.published_url.as_deref(),
        Some("https://example.com/guides/epoxy-floor-prep")
    );
    let inventory = store::get_inventory(
        persistence.connection_ref(),
        CLIENT,
        &inventory_row.inventory_id,
    )
    .expect("get inventory")
    .expect("inventory");
    assert_eq!(
        inventory.item.source_kind,
        ContentInventorySourceKind::PlanItem
    );
    assert_eq!(inventory.item.source_ref, plan_id);
    let receipts: i64 = persistence
        .connection_ref()
        .query_row(
            "SELECT COUNT(*) FROM receipts WHERE client_id = ?1 AND idempotency_key = 'mark-published'",
            rusqlite::params![CLIENT],
            |row| row.get(0),
        )
        .expect("receipt count");
    assert_eq!(receipts, 1);
}

#[test]
fn collision_check_uses_inventory_bm25_below_exact_signals() {
    let state = test_state();
    let manual = ContentInventoryManualAddRequest {
        title: "Concrete Coating Surface Preparation".to_string(),
        target_query: Some("floor coating prep".to_string()),
        url: Some("https://example.com/concrete-coating-surface-preparation".to_string()),
        summary: Some("Degreasing, etching, and epoxy readiness checklist.".to_string()),
        idempotency_key: "manual-bm25".to_string(),
        actor_id: None,
    };
    let row = service::manual_inventory_row(CLIENT, &manual, 1_000).expect("manual row");
    let item = service::item_from_create(
        CLIENT,
        &create_request("Epoxy Floor Prep Guide", "plan-bm25"),
        2_000,
    )
    .expect("item");
    let mut persistence = state.persistence.lock();
    let ctx = MutationContext {
        client_id: CLIENT,
        actor_id: "operator",
        expected_revision: None,
        idempotency_key: "manual-bm25",
        now_ms: 1_000,
    };
    store::add_manual_inventory(persistence.connection(), ctx, &row).expect("add manual");
    let candidates = collision_candidates_for(persistence.connection_ref(), &item, None);
    let summary = service::run_collision_check(&item, &candidates, 2_000);

    assert!(summary.matches.iter().any(|matched| {
        matched.inventory_id == row.inventory_id && matched.reason == "similar"
    }));
}

#[test]
fn draft_overlap_returns_inventory_match() {
    let state = test_state();
    let inventory_id = add_manual_inventory(
        &state,
        "Epoxy Floor Prep Guide",
        "epoxy floor prep",
        "draft-overlap-manual",
    );
    let draft_id = stage_content_draft(
        &state,
        "wi_draft_overlap",
        "operator_note",
        "note-overlap",
        "How to Prep a Floor for Epoxy Coating",
        "Degrease and etch the slab before coating.",
        "epoxy floor prep",
    );

    let persistence = state.persistence.lock();
    let draft = crate::slices::content_drafts::store::get_draft(
        persistence.connection_ref(),
        CLIENT,
        &draft_id,
    )
    .expect("get draft")
    .expect("draft");
    let match_expr = service::draft_collision_match_expression(
        &draft.draft.title,
        &draft.draft.body_markdown,
        draft.draft.target_query.as_deref(),
    );
    let draft_key = service::canonical_key(None, &draft.draft.title);
    let candidates = store::collision_candidates(
        persistence.connection_ref(),
        CLIENT,
        None,
        match_expr.as_deref(),
        &draft_key,
        draft.draft.target_query.as_deref(),
    )
    .expect("candidates");
    let summary = service::run_draft_collision_check(
        &draft.draft.draft_id,
        &draft.draft.item_id,
        &draft.draft.title,
        &draft.draft.body_markdown,
        draft.draft.target_query.as_deref(),
        &candidates,
        3_000,
    );

    assert!(summary.matches.iter().any(|matched| {
        matched.inventory_id == inventory_id && matched.reason == "exact_query"
    }));
}

#[test]
fn draft_overlap_excludes_self_sibling_and_origin_plan() {
    let candidates = vec![
        store::CollisionCandidate {
            inventory_id: "draft:current".to_string(),
            source_kind: "content_draft".to_string(),
            source_ref: "current".to_string(),
            work_item_id: Some("wi-plan".to_string()),
            title: "Epoxy Floor Prep Guide".to_string(),
            target_query: Some("epoxy floor prep".to_string()),
            canonical_key: "epoxy-floor-prep-guide".to_string(),
            search_text: "self".to_string(),
            bm25_score: None,
        },
        store::CollisionCandidate {
            inventory_id: "draft:sibling".to_string(),
            source_kind: "content_draft".to_string(),
            source_ref: "sibling".to_string(),
            work_item_id: Some("wi-plan".to_string()),
            title: "Epoxy Floor Prep Guide".to_string(),
            target_query: Some("epoxy floor prep".to_string()),
            canonical_key: "epoxy-floor-prep-guide".to_string(),
            search_text: "sibling".to_string(),
            bm25_score: None,
        },
        store::CollisionCandidate {
            inventory_id: "plan:origin".to_string(),
            source_kind: "plan_item".to_string(),
            source_ref: "origin".to_string(),
            work_item_id: Some("wi-plan".to_string()),
            title: "Epoxy Floor Prep Guide".to_string(),
            target_query: Some("epoxy floor prep".to_string()),
            canonical_key: "epoxy-floor-prep-guide".to_string(),
            search_text: "origin".to_string(),
            bm25_score: None,
        },
        store::CollisionCandidate {
            inventory_id: "manual:real".to_string(),
            source_kind: "manual".to_string(),
            source_ref: "manual:real".to_string(),
            work_item_id: None,
            title: "Epoxy Coating Preparation".to_string(),
            target_query: Some("epoxy floor prep".to_string()),
            canonical_key: "epoxy-coating-preparation".to_string(),
            search_text: "manual".to_string(),
            bm25_score: None,
        },
    ];

    let summary = service::run_draft_collision_check(
        "current",
        "wi-plan",
        "Epoxy Floor Prep Guide",
        "Degrease and etch the slab before coating.",
        Some("epoxy floor prep"),
        &candidates,
        3_000,
    );

    assert_eq!(summary.matches.len(), 1);
    assert_eq!(summary.matches[0].inventory_id, "manual:real");
}

#[test]
fn draft_overlap_scores_body_text_not_only_titles() {
    let candidates = vec![store::CollisionCandidate {
        inventory_id: "draft:older-body".to_string(),
        source_kind: "content_draft".to_string(),
        source_ref: "older-body".to_string(),
        work_item_id: Some("wi-other".to_string()),
        title: "Maintenance Checklist".to_string(),
        target_query: None,
        canonical_key: "maintenance-checklist".to_string(),
        search_text: "Maintenance Checklist\nDegrease and etch the slab before coating."
            .to_string(),
        bm25_score: None,
    }];

    let summary = service::run_draft_collision_check(
        "current",
        "wi-current",
        "Epoxy Floor Prep Guide",
        "Degrease and etch the slab before coating.",
        None,
        &candidates,
        3_000,
    );

    assert_eq!(summary.matches.len(), 1);
    assert_eq!(summary.matches[0].inventory_id, "draft:older-body");
    assert_eq!(summary.matches[0].reason, "similar");
}

#[test]
fn draft_overlap_finds_exact_draft_signal_beyond_recent_fifty() {
    let state = test_state();
    let older_draft_id = stage_content_draft_at(
        &state,
        DraftSeed {
            item_id: "wi_old_overlap",
            source_kind: "operator_note",
            source_ref: "note-old-overlap",
            title: "Legacy floor coating article",
            body_markdown: "Older content about epoxy surface preparation.",
            target_query: "special epoxy overlap",
        },
        1_000,
    );
    for index in 0..55 {
        let item_id = format!("wi_recent_{index}");
        let source_ref = format!("note-recent-{index}");
        let title = format!("Recent unrelated draft {index}");
        let target_query = format!("unrelated query {index}");
        stage_content_draft_at(
            &state,
            DraftSeed {
                item_id: &item_id,
                source_kind: "operator_note",
                source_ref: &source_ref,
                title: &title,
                body_markdown: "A newer unrelated draft body.",
                target_query: &target_query,
            },
            2_000 + index,
        );
    }
    let current_draft_id = stage_content_draft_at(
        &state,
        DraftSeed {
            item_id: "wi_current_overlap",
            source_kind: "operator_note",
            source_ref: "note-current-overlap",
            title: "New coating article",
            body_markdown: "New content about epoxy surface preparation.",
            target_query: "special epoxy overlap",
        },
        10_000,
    );

    let persistence = state.persistence.lock();
    let draft = crate::slices::content_drafts::store::get_draft(
        persistence.connection_ref(),
        CLIENT,
        &current_draft_id,
    )
    .expect("get draft")
    .expect("draft");
    let match_expr = service::draft_collision_match_expression(
        &draft.draft.title,
        &draft.draft.body_markdown,
        draft.draft.target_query.as_deref(),
    );
    let draft_key = service::canonical_key(None, &draft.draft.title);
    let candidates = store::collision_candidates(
        persistence.connection_ref(),
        CLIENT,
        None,
        match_expr.as_deref(),
        &draft_key,
        draft.draft.target_query.as_deref(),
    )
    .expect("candidates");
    let summary = service::run_draft_collision_check(
        &draft.draft.draft_id,
        &draft.draft.item_id,
        &draft.draft.title,
        &draft.draft.body_markdown,
        draft.draft.target_query.as_deref(),
        &candidates,
        11_000,
    );

    assert!(summary.matches.iter().any(|matched| {
        matched.inventory_id == format!("draft:{older_draft_id}") && matched.reason == "exact_query"
    }));
}

#[test]
fn draft_overlap_match_expression_is_bounded() {
    let body = (0..200)
        .map(|index| format!("bodyterm{index}"))
        .collect::<Vec<_>>()
        .join(" ");
    let expression =
        service::draft_collision_match_expression("Title Seed", &body, Some("Target Query"))
            .expect("expression");
    let terms = expression.split(" OR ").collect::<Vec<_>>();

    assert!(terms.len() <= service::DRAFT_OVERLAP_MAX_TERMS);
    assert!(!expression.contains("bodyterm199"));
}

#[test]
fn plan_source_view_feeds_content_draft_brief() {
    let state = test_state();
    let plan_id = insert_plan(
        &state,
        create_request("Epoxy Floor Prep Guide", "plan-source"),
        1_000,
    );
    let plan = {
        let persistence = state.persistence.lock();
        store::get_item(persistence.connection_ref(), CLIENT, &plan_id)
            .expect("get")
            .expect("plan")
            .item
    };
    let source = service::source_view(&plan);
    assert_eq!(source.source_key, plan_id);
    assert_eq!(source.subject.as_deref(), Some("Epoxy Floor Prep Guide"));
    assert!(source
        .body_excerpt
        .contains("Target query: epoxy floor prep"));
    assert!(source
        .body_excerpt
        .contains("Notes: Cover degreasing and etching."));
}

#[test]
fn accepted_plan_work_item_can_produce_content_draft() {
    let state = test_state();
    seed_corpus_doc(
        &state,
        "doc-epoxy",
        "Epoxy Prep SOP",
        "# Epoxy Prep\n\nDegrease and etch the concrete slab before coating.",
    );
    let plan_id = insert_plan(
        &state,
        create_request("Epoxy Floor Prep Guide", "plan-produce"),
        1_000,
    );
    let queued = {
        let persistence = state.persistence.lock();
        store::get_item(persistence.connection_ref(), CLIENT, &plan_id)
            .expect("get")
            .expect("plan")
    };
    {
        let mut persistence = state.persistence.lock();
        let summary = service::run_collision_check(&queued.item, &[], 2_000);
        let ctx = MutationContext {
            client_id: CLIENT,
            actor_id: "operator",
            expected_revision: Some(queued.revision),
            idempotency_key: "queue-produce",
            now_ms: 2_000,
        };
        store::queue_item(
            persistence.connection(),
            ctx,
            &queued.item,
            &summary,
            &service::work_item_title(&queued.item),
            &service::work_item_summary(&queued.item),
        )
        .expect("queue");
    }
    let work_item_id = {
        let mut persistence = state.persistence.lock();
        let queued = store::get_item(persistence.connection_ref(), CLIENT, &plan_id)
            .expect("get queued")
            .expect("queued");
        let work_item_id = queued.item.work_item_id.clone().expect("work item");
        crate::slices::work_queue::store::system_accept_item(
            persistence.connection(),
            CLIENT,
            &work_item_id,
            "test",
            Some(1),
            "accept-plan-work-item",
            3_000,
        )
        .expect("accept");
        work_item_id
    };
    crate::produce::set_test_produce_llm_response(serde_json::json!({
        "title": "How to Prep Concrete for Epoxy",
        "body_markdown": "Degrease and etch the concrete slab before coating.",
        "target_query": "epoxy floor prep",
        "meta_description": "A guide to preparing concrete for epoxy coating.",
        "claims": [
            {
                "text": "Degrease and etch the concrete slab before coating",
                "snippet_ids": ["doc-epoxy:0"]
            }
        ],
        "confidence": "high"
    }));
    crate::produce::produce_blocking(
        &state,
        &crate::slices::content_drafts::service::Produce,
        &work_item_id,
        "produce-plan-content",
        "operator",
        bos_contracts::receipt::ActorKindDto::Operator,
        &crate::http::OperatorScope::All,
    )
    .expect("produce content draft");

    let persistence = state.persistence.lock();
    let draft = crate::slices::content_drafts::store::active_draft_for_item(
        persistence.connection_ref(),
        CLIENT,
        &work_item_id,
    )
    .expect("draft")
    .expect("draft");
    assert_eq!(
        draft.draft.source_kind,
        super::SOURCE_KIND_CONTENT_PLAN_ITEM
    );
    assert_eq!(draft.draft.source_ref, plan_id);
    assert!(draft.draft.citation_gate.passed);
}

#[test]
fn campaign_approval_snapshots_exact_revisions_and_enqueues_blog_only() {
    let _env = EnvGuard::set_many(&[
        (
            "BOS_CONTENT_PUBLISH_ADAPTER_URL",
            "https://publisher.example",
        ),
        ("BOS_CONTENT_PUBLISH_ADAPTER_TOKEN", "test-token"),
        (
            "BOS_BUFFER_CHANNELS_JSON",
            r#"[{"channel_id":"buf_linkedin","name":"Company LinkedIn","platform":"linkedin"}]"#,
        ),
    ]);
    let state = test_state();
    let plan_id = insert_plan(
        &state,
        create_request("Campaign guide", "campaign-plan"),
        1_000,
    );
    let work_item_id = store::work_item_id(&plan_id);
    {
        let mut persistence = state.persistence.lock();
        let item = store::get_item(persistence.connection_ref(), CLIENT, &plan_id)
            .expect("plan")
            .expect("plan");
        let summary = item.item.collision_summary.clone().expect("summary");
        store::queue_item_for_generation(
            persistence.connection(),
            MutationContext {
                client_id: CLIENT,
                actor_id: "operator",
                expected_revision: Some(item.revision),
                idempotency_key: "campaign-queue",
                now_ms: 2_000,
            },
            &item.item,
            &summary,
            "Campaign guide",
            "Campaign guide summary",
        )
        .expect("queue accepted");
    }
    let draft_id = stage_content_draft(
        &state,
        &work_item_id,
        super::SOURCE_KIND_CONTENT_PLAN_ITEM,
        &plan_id,
        "Campaign guide",
        "Degrease and etch the slab before coating.",
        "epoxy floor prep",
    );
    let (draft_revision, proposal_id, proposal_revision) = {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        let staged = crate::slices::content_drafts::store::get_draft(conn, CLIENT, &draft_id)
            .expect("draft")
            .expect("draft");
        crate::slices::content_drafts::store::approve_draft(
            conn,
            crate::slices::content_drafts::store::DraftActionContext {
                client_id: CLIENT,
                actor_id: "operator",
                expected_revision: Some(staged.revision),
                idempotency_key: "campaign-approve-article",
                now_ms: 3_000,
            },
            &draft_id,
        )
        .expect("approve article");
        let approved = crate::slices::content_drafts::store::get_draft(conn, CLIENT, &draft_id)
            .expect("draft")
            .expect("draft");
        assert_eq!(approved.draft.status, ContentDraftStatus::Approved);
        let preview_source = bos_contracts::social_publishing::SocialPublishedSource {
            source_id: "preview-source".to_string(),
            source_kind: crate::slices::social_publishing::service::PREVIEW_SOURCE_KIND.to_string(),
            external_id: "preview-external".to_string(),
            source_content_draft_id: Some(draft_id.clone()),
            source_content_draft_revision: Some(approved.revision),
            title: approved.draft.title.clone(),
            canonical_url: "https://example.com/campaign-guide".to_string(),
            excerpt: approved.draft.meta_description.clone(),
            published_at: None,
            generation_status:
                bos_contracts::social_publishing::SocialSourceGenerationStatus::Ready,
            generation_run_id: None,
            generation_error: None,
            proposal_id: None,
            revision: 0,
        };
        crate::slices::social_publishing::store::ingest_source(
            conn,
            MutationContext {
                client_id: CLIENT,
                actor_id: "operator",
                expected_revision: None,
                idempotency_key: "campaign-preview-source",
                now_ms: 3_100,
            },
            ActorKindDto::Operator,
            &preview_source,
        )
        .expect("preview source");
        let stage = SocialProposalStageRequest {
            source_id: Some(preview_source.source_id),
            source_content_draft_id: Some(draft_id.clone()),
            source_content_draft_revision: Some(approved.revision),
            canonical_url: "https://example.com/campaign-guide".to_string(),
            targets: vec![SocialProposalTargetInput {
                channel_id: "buf_linkedin".to_string(),
                text: "Read the campaign guide".to_string(),
                image_url: None,
                utm: SocialUtmParameters::default(),
                schedule_mode: SocialScheduleMode::Queue,
                due_at: None,
            }],
            idempotency_key: "campaign-stage-social".to_string(),
            actor_id: None,
        };
        let (_, proposal_id) = crate::slices::social_publishing::service::stage_request(
            conn,
            CLIENT,
            "operator",
            ActorKindDto::Operator,
            &stage,
            3_200,
        )
        .expect("stage social");
        let proposal =
            crate::slices::social_publishing::store::get_proposal(conn, CLIENT, &proposal_id)
                .expect("proposal")
                .expect("proposal");
        let standalone = crate::slices::social_publishing::service::approve_request(
            conn,
            CLIENT,
            "operator",
            &proposal_id,
            proposal.revision,
            "standalone-preview-approve",
            3_300,
        )
        .expect_err("preview cannot bypass campaign coordinator");
        assert!(matches!(
            standalone,
            crate::store_core::StoreError::Domain(code)
                if code == "social_preview_requires_campaign_approval"
        ));
        (approved.revision, proposal_id, proposal.revision)
    };
    let request = ContentCampaignPublishRequest {
        content_draft_id: draft_id.clone(),
        expected_content_draft_revision: draft_revision,
        social_proposal_id: Some(proposal_id.clone()),
        expected_social_proposal_revision: Some(proposal_revision),
        selected_channel_ids: vec!["buf_linkedin".to_string()],
        slug: "campaign-guide".to_string(),
        published_at: "2026-08-12".to_string(),
        expected_canonical_url: "https://example.com/campaign-guide".to_string(),
        launch_mode: ContentCampaignLaunchMode::PublishNow,
        idempotency_key: "campaign-publish".to_string(),
        actor_id: None,
    };
    let mut persistence = state.persistence.lock();
    let plan = store::get_item(persistence.connection_ref(), CLIENT, &plan_id)
        .expect("plan")
        .expect("plan");
    let approval = service::prepare_campaign_publication(
        persistence.connection_ref(),
        CLIENT,
        &plan.item,
        &request,
        "operator",
        4_000,
    )
    .expect("approval snapshot");
    store::insert_campaign_publication(
        persistence.connection(),
        MutationContext {
            client_id: CLIENT,
            actor_id: "operator",
            expected_revision: None,
            idempotency_key: &request.idempotency_key,
            now_ms: 4_000,
        },
        &approval,
    )
    .expect("campaign approval");
    let replay_approval = service::prepare_campaign_publication(
        persistence.connection_ref(),
        CLIENT,
        &plan.item,
        &request,
        "operator",
        4_000,
    )
    .expect("same campaign approval remains replayable after its locks exist");
    let replay = store::insert_campaign_publication(
        persistence.connection(),
        MutationContext {
            client_id: CLIENT,
            actor_id: "operator",
            expected_revision: None,
            idempotency_key: &request.idempotency_key,
            now_ms: 4_000,
        },
        &replay_approval,
    )
    .expect("campaign approval replay");
    assert!(matches!(
        replay,
        crate::store_core::MutationOutcome::ReplayedIdempotent { .. }
    ));
    let publications =
        store::list_campaign_publications(persistence.connection_ref(), CLIENT, &plan_id, 10)
            .expect("publications");
    assert_eq!(publications.len(), 1);
    assert_eq!(
        publications[0].publication.status,
        ContentCampaignPublicationStatus::AwaitingBlog
    );
    assert_eq!(
        publications[0].publication.content_draft_revision,
        draft_revision
    );
    assert_eq!(
        publications[0].publication.social_proposal_id,
        Some(proposal_id)
    );
    assert_eq!(publications[0].publication.social_outbox_jobs.len(), 0);
    let job_count: i64 = persistence
        .connection_ref()
        .query_row(
            "SELECT COUNT(*) FROM outbox_jobs WHERE client_id = ?1",
            rusqlite::params![CLIENT],
            |row| row.get(0),
        )
        .expect("job count");
    assert_eq!(
        job_count, 1,
        "only the blog job exists before canonical URL"
    );
    let blog_job_id = publications[0].publication.blog_outbox_job.job_id.clone();
    let claimed = crate::outbox::claim_due_job_by_id(
        persistence.connection(),
        CLIENT,
        &blog_job_id,
        60_000,
        4_001,
    )
    .expect("claim blog")
    .expect("blog job");
    crate::outbox::record_attempt(
        persistence.connection(),
        CLIENT,
        &claimed,
        &crate::outbox::AttemptOutcome::Delivered {
            result_json: serde_json::json!({
                "dry_run": false,
                "provider_object_id": "https://example.com/campaign-guide"
            })
            .to_string(),
        },
        4_002,
    )
    .expect("blog delivery");
    assert_eq!(
        service::reconcile_campaign_publications(persistence.connection(), CLIENT, 4_003,)
            .expect("reconcile"),
        1
    );
    let publication =
        store::list_campaign_publications(persistence.connection_ref(), CLIENT, &plan_id, 10)
            .expect("publication")
            .remove(0);
    assert_eq!(
        publication.publication.status,
        ContentCampaignPublicationStatus::SocialEnqueued
    );
    assert_eq!(publication.publication.social_outbox_jobs.len(), 1);
    let buffer_job: (String, String) = persistence
        .connection_ref()
        .query_row(
            "SELECT payload_json, causation_id FROM outbox_jobs \
             WHERE client_id = ?1 AND provider = 'buffer'",
            rusqlite::params![CLIENT],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("buffer job");
    let payload: serde_json::Value = serde_json::from_str(&buffer_job.0).expect("payload");
    assert_eq!(
        payload["canonical_url"],
        serde_json::json!("https://example.com/campaign-guide")
    );
    assert_eq!(buffer_job.1, blog_job_id);
    let social_job_id = publication.publication.social_outbox_jobs[0].job_id.clone();
    let claimed = crate::outbox::claim_due_job_by_id(
        persistence.connection(),
        CLIENT,
        &social_job_id,
        60_000,
        4_004,
    )
    .expect("claim social")
    .expect("social job");
    crate::outbox::record_attempt(
        persistence.connection(),
        CLIENT,
        &claimed,
        &crate::outbox::AttemptOutcome::Delivered {
            result_json: serde_json::json!({
                "dry_run": false,
                "provider_object_id": "buffer-post-1"
            })
            .to_string(),
        },
        4_005,
    )
    .expect("social delivery");
    assert_eq!(
        service::reconcile_campaign_publications(persistence.connection(), CLIENT, 4_006,)
            .expect("settle social"),
        1
    );
    let completed =
        store::list_campaign_publications(persistence.connection_ref(), CLIENT, &plan_id, 10)
            .expect("completed")
            .remove(0);
    assert_eq!(
        completed.publication.status,
        ContentCampaignPublicationStatus::Completed
    );
    assert_eq!(completed.publication.social_outbox_jobs.len(), 1);
}

#[test]
fn blog_failure_retry_resumes_canonical_validation_and_social_fanout() {
    let state = test_state();
    let plan_id = insert_plan(
        &state,
        create_request("Retry campaign", "retry-campaign-plan"),
        1_000,
    );
    let expected_url = "https://example.com/retry-campaign";
    let tracked_url =
        format!("{expected_url}?utm_source=linkedin&utm_medium=social&utm_campaign=retry");
    let social_target = SocialProposalTarget {
        target_id: "retry-target".to_string(),
        channel_id: "buf_linkedin".to_string(),
        channel_name: "Company LinkedIn".to_string(),
        platform: "linkedin".to_string(),
        text: format!("Retry campaign {tracked_url}"),
        tracked_url,
        image_url: None,
        utm: SocialUtmParameters {
            source: Some("linkedin".to_string()),
            medium: Some("social".to_string()),
            campaign: Some("retry".to_string()),
            content: None,
        },
        schedule_mode: SocialScheduleMode::Queue,
        due_at: None,
        outbox_job_id: None,
        outbox_job: None,
    };
    let blog_job = crate::outbox::NewOutboxJob {
        job_id: "retry-campaign-blog-job".to_string(),
        provider: crate::slices::content_drafts::service::PROVIDER_CONTENT_PUBLISH_ADAPTER
            .to_string(),
        capability: crate::slices::content_drafts::service::CAPABILITY_PUBLISH_POST.to_string(),
        payload_json: "{}".to_string(),
        source_entity_kind: crate::slices::content_drafts::store::DRAFT_ENTITY_KIND.to_string(),
        source_entity_id: "retry-campaign-draft".to_string(),
        correlation_id: Some(plan_id.clone()),
        causation_id: Some("retry-campaign-publication".to_string()),
        idempotency_key: "outbox:retry-campaign-blog".to_string(),
    };
    let approval = store::CampaignPublicationApproval {
        publication_id: "retry-campaign-publication".to_string(),
        plan_item_id: plan_id.clone(),
        content_draft_id: "retry-campaign-draft".to_string(),
        content_draft_revision: 1,
        social_proposal_id: Some("retry-campaign-proposal".to_string()),
        social_proposal_revision: Some(1),
        expected_canonical_url: expected_url.to_string(),
        launch_mode: ContentCampaignLaunchMode::PublishNow,
        selected_channel_ids: vec!["buf_linkedin".to_string()],
        approved_social_targets: vec![social_target],
        approved_by: "operator".to_string(),
        approved_at_ms: 2_000,
        blog_job,
    };

    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    store::insert_campaign_publication(
        conn,
        MutationContext {
            client_id: CLIENT,
            actor_id: "operator",
            expected_revision: None,
            idempotency_key: "retry-campaign-approval",
            now_ms: 2_000,
        },
        &approval,
    )
    .expect("insert campaign");
    let claimed =
        crate::outbox::claim_due_job_by_id(conn, CLIENT, &approval.blog_job.job_id, 60_000, 2_001)
            .expect("claim blog")
            .expect("blog job");
    crate::outbox::record_attempt(
        conn,
        CLIENT,
        &claimed,
        &crate::outbox::AttemptOutcome::Terminal {
            error: "publisher_rejected".to_string(),
            result_json: None,
        },
        2_002,
    )
    .expect("fail blog");
    assert_eq!(
        service::reconcile_campaign_publications(conn, CLIENT, 2_003).expect("reconcile failure"),
        1
    );
    let failed = store::list_campaign_publications(conn, CLIENT, &plan_id, 1)
        .expect("campaign")
        .remove(0);
    assert_eq!(
        failed.publication.status,
        ContentCampaignPublicationStatus::RequiresReview
    );
    assert_eq!(
        failed.publication.review_reason.as_deref(),
        Some("blog_publish_failed")
    );

    crate::outbox::retry_terminal_job(
        conn,
        CLIENT,
        &approval.blog_job.job_id,
        "operator",
        "retry-campaign-blog",
        2_100,
    )
    .expect("retry blog");
    assert_eq!(
        service::reconcile_campaign_publications(conn, CLIENT, 2_101).expect("pending retry"),
        0
    );
    let claimed =
        crate::outbox::claim_due_job_by_id(conn, CLIENT, &approval.blog_job.job_id, 60_000, 2_102)
            .expect("claim retried blog")
            .expect("retried blog job");
    crate::outbox::record_attempt(
        conn,
        CLIENT,
        &claimed,
        &crate::outbox::AttemptOutcome::Delivered {
            result_json: serde_json::json!({
                "dry_run": false,
                "provider_object_id": expected_url,
            })
            .to_string(),
        },
        2_103,
    )
    .expect("deliver retried blog");
    assert_eq!(
        service::reconcile_campaign_publications(conn, CLIENT, 2_104).expect("resume campaign"),
        1
    );
    let resumed = store::list_campaign_publications(conn, CLIENT, &plan_id, 1)
        .expect("campaign")
        .remove(0);
    assert_eq!(
        resumed.publication.status,
        ContentCampaignPublicationStatus::SocialEnqueued
    );
    assert_eq!(
        resumed.publication.actual_canonical_url.as_deref(),
        Some(expected_url)
    );
    assert_eq!(resumed.publication.social_outbox_jobs.len(), 1);
    assert_eq!(resumed.publication.social_outbox_jobs[0].status, "pending");

    let social_job_id = resumed.publication.social_outbox_jobs[0].job_id.clone();
    let claimed = crate::outbox::claim_due_job_by_id(conn, CLIENT, &social_job_id, 60_000, 2_105)
        .expect("claim resumed social job")
        .expect("resumed social job");
    crate::outbox::record_attempt(
        conn,
        CLIENT,
        &claimed,
        &crate::outbox::AttemptOutcome::Terminal {
            error: "buffer_rejected".to_string(),
            result_json: None,
        },
        2_106,
    )
    .expect("fail resumed social job");
    assert_eq!(
        service::reconcile_campaign_publications(conn, CLIENT, 2_107)
            .expect("reconcile resumed social failure"),
        1
    );
    let reviewed = store::list_campaign_publications(conn, CLIENT, &plan_id, 1)
        .expect("campaign")
        .remove(0);
    assert_eq!(
        reviewed.publication.status,
        ContentCampaignPublicationStatus::RequiresReview
    );
    assert_eq!(
        reviewed.publication.review_reason.as_deref(),
        Some("social_delivery_failed")
    );
}

#[test]
fn terminal_social_failures_do_not_starve_newer_actionable_campaigns() {
    let state = test_state();
    let plan_id = insert_plan(
        &state,
        create_request("Actionable campaign", "actionable-campaign-plan"),
        1_000,
    );
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    for index in 0..21 {
        let publication_id = format!("terminal-publication-{index:02}");
        store::seed_terminal_social_campaign_for_test(conn, CLIENT, &publication_id, index)
            .expect("seed terminal campaign");
    }

    let blog_job = crate::outbox::NewOutboxJob {
        job_id: "actionable-blog-job".to_string(),
        provider: crate::slices::content_drafts::service::PROVIDER_CONTENT_PUBLISH_ADAPTER
            .to_string(),
        capability: crate::slices::content_drafts::service::CAPABILITY_PUBLISH_POST.to_string(),
        payload_json: "{}".to_string(),
        source_entity_kind: crate::slices::content_drafts::store::DRAFT_ENTITY_KIND.to_string(),
        source_entity_id: "actionable-draft".to_string(),
        correlation_id: Some(plan_id.clone()),
        causation_id: Some("actionable-publication".to_string()),
        idempotency_key: "outbox:actionable-blog".to_string(),
    };
    let approval = store::CampaignPublicationApproval {
        publication_id: "actionable-publication".to_string(),
        plan_item_id: plan_id.clone(),
        content_draft_id: "actionable-draft".to_string(),
        content_draft_revision: 1,
        social_proposal_id: None,
        social_proposal_revision: None,
        expected_canonical_url: "https://example.com/actionable".to_string(),
        launch_mode: ContentCampaignLaunchMode::PublishNow,
        selected_channel_ids: Vec::new(),
        approved_social_targets: Vec::new(),
        approved_by: "operator".to_string(),
        approved_at_ms: 10_000,
        blog_job,
    };
    store::insert_campaign_publication(
        conn,
        MutationContext {
            client_id: CLIENT,
            actor_id: "operator",
            expected_revision: None,
            idempotency_key: "actionable-campaign-approval",
            now_ms: 10_000,
        },
        &approval,
    )
    .expect("insert actionable campaign");
    let claimed =
        crate::outbox::claim_due_job_by_id(conn, CLIENT, &approval.blog_job.job_id, 60_000, 10_001)
            .expect("claim actionable blog")
            .expect("actionable blog job");
    crate::outbox::record_attempt(
        conn,
        CLIENT,
        &claimed,
        &crate::outbox::AttemptOutcome::Delivered {
            result_json: serde_json::json!({
                "dry_run": false,
                "provider_object_id": "https://example.com/actionable",
            })
            .to_string(),
        },
        10_002,
    )
    .expect("deliver actionable blog");

    assert_eq!(
        service::reconcile_campaign_publications(conn, CLIENT, 10_003)
            .expect("reconcile actionable campaign"),
        1
    );
    let publication = store::list_campaign_publications(conn, CLIENT, &plan_id, 1)
        .expect("publication")
        .remove(0);
    assert_eq!(
        publication.publication.status,
        ContentCampaignPublicationStatus::Completed
    );
}

#[test]
fn campaign_reconcile_stops_social_when_provider_changes_canonical_url() {
    let _env = EnvGuard::set_many(&[
        (
            "BOS_CONTENT_PUBLISH_ADAPTER_URL",
            "https://publisher.example",
        ),
        ("BOS_CONTENT_PUBLISH_ADAPTER_TOKEN", "test-token"),
        (
            "BOS_BUFFER_CHANNELS_JSON",
            r#"[{"channel_id":"buf_linkedin","name":"Company LinkedIn","platform":"linkedin"}]"#,
        ),
    ]);
    let state = test_state();
    let plan_id = insert_plan(&state, create_request("URL guard", "url-plan"), 1_000);
    let work_item_id = store::work_item_id(&plan_id);
    {
        let mut persistence = state.persistence.lock();
        let plan = store::get_item(persistence.connection_ref(), CLIENT, &plan_id)
            .expect("plan")
            .expect("plan");
        store::queue_item_for_generation(
            persistence.connection(),
            MutationContext {
                client_id: CLIENT,
                actor_id: "operator",
                expected_revision: Some(plan.revision),
                idempotency_key: "url-queue",
                now_ms: 2_000,
            },
            &plan.item,
            &plan.item.collision_summary.clone().expect("summary"),
            "URL guard",
            "URL guard",
        )
        .expect("queue");
    }
    let draft_id = stage_content_draft(
        &state,
        &work_item_id,
        super::SOURCE_KIND_CONTENT_PLAN_ITEM,
        &plan_id,
        "URL guard",
        "Degrease and etch the slab before coating.",
        "epoxy floor prep",
    );
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let staged = crate::slices::content_drafts::store::get_draft(conn, CLIENT, &draft_id)
        .expect("draft")
        .expect("draft");
    crate::slices::content_drafts::store::approve_draft(
        conn,
        crate::slices::content_drafts::store::DraftActionContext {
            client_id: CLIENT,
            actor_id: "operator",
            expected_revision: Some(staged.revision),
            idempotency_key: "url-approve",
            now_ms: 3_000,
        },
        &draft_id,
    )
    .expect("approve");
    let approved = crate::slices::content_drafts::store::get_draft(conn, CLIENT, &draft_id)
        .expect("draft")
        .expect("draft");
    let plan = store::get_item(conn, CLIENT, &plan_id)
        .expect("plan")
        .expect("plan");
    let request = ContentCampaignPublishRequest {
        content_draft_id: draft_id,
        expected_content_draft_revision: approved.revision,
        social_proposal_id: None,
        expected_social_proposal_revision: None,
        selected_channel_ids: Vec::new(),
        slug: "url-guard".to_string(),
        published_at: "2026-08-12".to_string(),
        expected_canonical_url: "https://example.com/url-guard".to_string(),
        launch_mode: ContentCampaignLaunchMode::PublishNow,
        idempotency_key: "url-publish".to_string(),
        actor_id: None,
    };
    let approval = service::prepare_campaign_publication(
        conn, CLIENT, &plan.item, &request, "operator", 4_000,
    )
    .expect("approval");
    store::insert_campaign_publication(
        conn,
        MutationContext {
            client_id: CLIENT,
            actor_id: "operator",
            expected_revision: None,
            idempotency_key: &request.idempotency_key,
            now_ms: 4_000,
        },
        &approval,
    )
    .expect("insert");
    let claimed =
        crate::outbox::claim_due_job_by_id(conn, CLIENT, &approval.blog_job.job_id, 60_000, 4_001)
            .expect("claim")
            .expect("job");
    crate::outbox::record_attempt(
        conn,
        CLIENT,
        &claimed,
        &crate::outbox::AttemptOutcome::Delivered {
            result_json: serde_json::json!({
                "dry_run": false,
                "provider_object_id": "https://example.com/provider-changed-url"
            })
            .to_string(),
        },
        4_002,
    )
    .expect("deliver");
    assert_eq!(
        service::reconcile_campaign_publications(conn, CLIENT, 4_003).expect("reconcile"),
        1
    );
    let publication = store::list_campaign_publications(conn, CLIENT, &plan_id, 10)
        .expect("publication")
        .remove(0);
    assert_eq!(
        publication.publication.status,
        ContentCampaignPublicationStatus::RequiresReview
    );
    assert_eq!(
        publication.publication.review_reason.as_deref(),
        Some("blog_canonical_url_changed")
    );
    let buffer_jobs: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM outbox_jobs WHERE client_id = ?1 AND provider = 'buffer'",
            rusqlite::params![CLIENT],
            |row| row.get(0),
        )
        .expect("buffer jobs");
    assert_eq!(buffer_jobs, 0);
}

fn seed_corpus_doc(state: &crate::http::AppState, file_id: &str, title: &str, text: &str) {
    use bos_integrations::google_drive_read::{DriveFileMeta, GOOGLE_DOC_MIME};
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let meta = DriveFileMeta {
        file_id: file_id.to_string(),
        name: title.to_string(),
        mime_type: GOOGLE_DOC_MIME.to_string(),
        modified_time: "2026-06-01T00:00:00Z".to_string(),
        version: Some("1".to_string()),
        parent_folder_ids: vec!["folder-a".to_string()],
        web_view_link: None,
        trashed: false,
    };
    crate::slices::drive_corpus::store::mark_stale_from_meta(conn, CLIENT, &meta, 1_000)
        .expect("stale");
    let chunks = crate::slices::drive_corpus::service::chunk_document(text);
    crate::slices::drive_corpus::store::index_document(
        conn,
        CLIENT,
        file_id,
        title,
        &format!("hash-{file_id}"),
        &chunks,
        2_000,
    )
    .expect("index");
}
