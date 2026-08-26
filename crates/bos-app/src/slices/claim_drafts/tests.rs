//! Slice tests: packet-completeness gate (the four harvested roles),
//! grounded narrative parsing, claim-amount precedence, the claims pump
//! (snapshot upserts, work-item emission, pack-photo fetches, 429
//! standdown, receipt-quiet steady state), and the approve lifecycle
//! (Gmail-draft outbox job + tracking task in one tx; incomplete packets
//! refused). No live LLM, no network.

use std::collections::HashMap;

use bos_contracts::claim_drafts::{ClaimShipmentDocumentRef, ClaimShipmentRefs};
use bos_contracts::work_queue::{WorkItem, WorkItemStatus};
use bos_integrations::stockforge_read::{
    FixtureStockforgeReadClient, SfDamageEventRecord, SfOrderCardRecord, SfPackPhotoRecord,
};
use serde_json::json;

use super::service::{self, ClaimContext};
use super::store::{self, DraftActionContext};
use super::worker;
use crate::http::test_support::test_state;
use crate::http::AppState;

const CLIENT: &str = "test-client";

fn damage_event(id: &str, shipment_id: &str) -> SfDamageEventRecord {
    SfDamageEventRecord {
        damage_event_id: id.to_string(),
        shipment_id: shipment_id.to_string(),
        reported_at: Some("2026-06-09T15:00:00Z".to_string()),
        reported_by: "CUSTOMER".to_string(),
        severity: "HIGH".to_string(),
        damage_type: "Crushed carton".to_string(),
        photos: vec!["https://business-cafdd46cb1.example.test/asset-c1c3e62eb2.jpg".to_string()],
        description: Some(
            "Box arrived crushed on one side; two cleaning solution containers dented and leaking."
                .to_string(),
        ),
        claim_status: "OPEN".to_string(),
        claim_amount_cents: Some(15_000),
        resolution: None,
        shipment_number: Some("SHP-77".to_string()),
        carrier: Some("UPS".to_string()),
        tracking_number: Some("1Z999AA10123456784".to_string()),
        shipment_refs: None,
        shipment_status: Some("DELIVERED".to_string()),
    }
}

fn resolved_damage_event(id: &str, shipment_id: &str) -> SfDamageEventRecord {
    let mut event = damage_event(id, shipment_id);
    event.claim_status = "RESOLVED".to_string();
    event
}

fn order_card(order_id: &str, shipment_id: &str) -> SfOrderCardRecord {
    SfOrderCardRecord {
        order_id: order_id.to_string(),
        order_number: format!("#{order_id}"),
        external_order_id: Some(format!("shopify-{order_id}")),
        platform: Some("shopify".to_string()),
        board_status: "DELIVERED".to_string(),
        raw_status: None,
        customer_name: Some("Dana".to_string()),
        customer_email: Some("dana@example.test".to_string()),
        total_amount_cents: 21_999,
        currency: Some("USD".to_string()),
        order_date: Some("2026-06-01".to_string()),
        processed_at: None,
        item_count: 2,
        unit_count: 3,
        mapped_line_count: 2,
        line_material_ids: vec!["m1".to_string()],
        line_identity_complete: true,
        carrier: Some("UPS".to_string()),
        tracking_number: Some("1Z999AA10123456784".to_string()),
        shipment_refs: None,
        shipment_id: Some(shipment_id.to_string()),
        ship_date: Some("2026-06-03".to_string()),
        photo_count: 2,
        pack_station_container_id: Some("cont-1".to_string()),
        needs_mapping: false,
        blocked: false,
        deducted: true,
        deduction_failed: false,
        label_needed: false,
        packed_missing_photo: false,
        exception: true,
        depletion_total: 2,
        depletion_applied: 2,
        depletion_failed: 0,
        depletion_reversed: 0,
        blocked_reasons_json: "[]".to_string(),
    }
}

fn seed_order(state: &AppState, order_id: &str, shipment_id: &str) {
    let mut persistence = state.persistence.lock();
    crate::slices::inventory::store::upsert_order_snapshots(
        persistence.connection(),
        CLIENT,
        &[order_card(order_id, shipment_id)],
        1_000,
    )
    .expect("seed order");
}

fn full_context() -> ClaimContext {
    ClaimContext {
        damage_event_id: "dmg-1".to_string(),
        shipment_id: "shp-1".to_string(),
        reported_at: Some("2026-06-09T15:00:00Z".to_string()),
        reported_by: "CUSTOMER".to_string(),
        severity: "HIGH".to_string(),
        damage_type: "Crushed carton".to_string(),
        damage_photo_urls: vec![
            "https://business-cafdd46cb1.example.test/asset-c1c3e62eb2.jpg".to_string(),
        ],
        description: Some(
            "Box arrived crushed on one side; two cleaning solution containers dented and leaking."
                .to_string(),
        ),
        damage_claim_amount_cents: Some(15_000),
        shipment_number: Some("SHP-77".to_string()),
        carrier: Some("UPS".to_string()),
        tracking_number: Some("1Z999AA10123456784".to_string()),
        shipment_refs: None,
        shipment_context_source: Some("stockforge".to_string()),
        order_number: Some("#o1".to_string()),
        order_platform: Some("shopify".to_string()),
        external_order_id: Some("shopify-o1".to_string()),
        customer_name: Some("Dana".to_string()),
        order_total_cents: Some(21_999),
        order_date: Some("2026-06-01".to_string()),
        ship_date: Some("2026-06-03".to_string()),
        item_count: Some(2),
        pack_photo_urls: vec![
            "https://business-cafdd46cb1.example.test/asset-8d9c57bdde.jpg".to_string(),
        ],
        pack_photo_count: 2,
    }
}

fn work_item(item_id: &str, damage_event_id: &str) -> WorkItem {
    WorkItem {
        item_id: item_id.to_string(),
        source_kind: "stockforge_damage".to_string(),
        source_ref: damage_event_id.to_string(),
        category_id: super::DAMAGE_CATEGORY.to_string(),
        title: "Shipping damage".to_string(),
        summary: String::new(),
        packet_kinds: vec!["claim_draft".to_string()],
        status: WorkItemStatus::Accepted,
        accept_actor: Some(bos_contracts::work_queue::WorkItemAcceptActor::Operator),
        ai_suggested: false,
        rationale: String::new(),
        produce_guidance: String::new(),
        source_user_id: None,
        assignee_user_id: None,
        visible_to_user_ids: Vec::new(),
        created_at_ms: 1,
        updated_at_ms: 1,
    }
}

fn valid_fill() -> serde_json::Value {
    json!({
        "damage_narrative": "The shipment arrived with the carton crushed on one side; two cleaning solution containers were dented and leaking on delivery.",
        "item_description": "Two 1-gallon cleaning solution containers",
        "confidence": "high",
        "provenance": [
            {"field": "damage_narrative", "quote": "crushed on one side; two cleaning solution containers dented and leaking"}
        ]
    })
}

#[test]
fn narrative_request_serialized_input_stays_inside_declared_byte_budget() {
    let mut message = service::produce_source_view(&store::DamageSnapshot {
        damage_event_id: "dmg-long".to_string(),
        shipment_id: "shp-long".to_string(),
        reported_at: None,
        reported_by: "CUSTOMER".to_string(),
        severity: "HIGH".to_string(),
        damage_type: "Crushed carton".to_string(),
        photos: Vec::new(),
        description: None,
        claim_status: "OPEN".to_string(),
        claim_amount_cents: None,
        shipment_number: None,
        carrier: None,
        tracking_number: None,
        shipment_refs: None,
        pack_photo_urls: Vec::new(),
        pack_photos_fetched: false,
        first_seen_at_ms: 1,
    });
    message.body_full = "x".repeat(70_000);
    message.body_excerpt = "short".to_string();
    let request = service::build_narrative_fill_request(
        CLIENT,
        &work_item("wi-long", "dmg-long"),
        &message,
        &full_context(),
        1,
    );
    let serialized = serde_json::to_string(&request.input).expect("serialize input");
    assert!(
        serialized.len() as u64 <= request.spec.max_input_bytes,
        "serialized input was {} bytes; max is {}",
        serialized.len(),
        request.spec.max_input_bytes
    );
}

// ---------------------------------------------------------------------------
// Packet gate + fill grounding + amount precedence
// ---------------------------------------------------------------------------

#[test]
fn packet_gate_requires_all_four_roles() {
    let complete = full_context();
    let gate = service::evaluate_packet_gate(&complete);
    assert!(gate.ready, "missing: {:?}", gate.missing_roles);

    let mut no_order = complete.clone();
    no_order.order_number = None;
    let gate = service::evaluate_packet_gate(&no_order);
    assert!(!gate.ready);
    assert_eq!(gate.missing_roles, vec!["order_reference"]);

    let mut no_pack = complete.clone();
    no_pack.pack_photo_count = 0;
    no_pack.pack_photo_urls.clear();
    assert_eq!(
        service::evaluate_packet_gate(&no_pack).missing_roles,
        vec!["packing_proof"]
    );

    let mut not_ups = complete.clone();
    not_ups.carrier = Some("FedEx".to_string());
    assert_eq!(
        service::evaluate_packet_gate(&not_ups).missing_roles,
        Vec::<String>::new()
    );

    let mut no_tracking = complete.clone();
    no_tracking.tracking_number = None;
    no_tracking.shipment_number = None;
    assert_eq!(
        service::evaluate_packet_gate(&no_tracking).missing_roles,
        vec!["tracking_reference"]
    );

    let mut ltl_refs = no_tracking.clone();
    ltl_refs.shipment_refs = Some(ClaimShipmentRefs {
        shipping_platform: Some("speedship".to_string()),
        platform_shipment_id: Some("ss-123".to_string()),
        carrier: Some("LTL Carrier".to_string()),
        mode: Some("ltl".to_string()),
        pro_number: Some("PRO-456".to_string()),
        bol_number: Some("BOL-789".to_string()),
        ..Default::default()
    });
    assert!(
        service::evaluate_packet_gate(&ltl_refs).ready,
        "PRO/BOL/platform refs satisfy tracking_reference"
    );

    let mut no_damage_photo = complete.clone();
    no_damage_photo.damage_photo_urls.clear();
    assert_eq!(
        service::evaluate_packet_gate(&no_damage_photo).missing_roles,
        vec!["damage_photo"]
    );
}

#[test]
fn narrative_fill_requires_a_literal_quote_when_the_report_has_text() {
    let description = full_context().description;
    let fill = service::parse_narrative_fill_response(&valid_fill(), description.as_deref())
        .expect("grounded fill");
    assert!(fill.damage_narrative.contains("crushed"));
    assert_eq!(
        fill.item_description,
        "Two 1-gallon cleaning solution containers"
    );

    // A quote that is not a literal span of the report is refused.
    let mut ungrounded = valid_fill();
    ungrounded["provenance"] = json!([
        {"field": "damage_narrative", "quote": "totally invented quotation"}
    ]);
    assert!(service::parse_narrative_fill_response(&ungrounded, description.as_deref()).is_err());

    // No description on the report → no grounding requirement.
    service::parse_narrative_fill_response(&ungrounded, None).expect("no-description fill");

    let mut bad_confidence = valid_fill();
    bad_confidence["confidence"] = json!("sure");
    assert!(service::parse_narrative_fill_response(&bad_confidence, None).is_err());
}

#[test]
fn claim_amount_grounds_to_damage_report_then_order_total() {
    let fill = service::parse_narrative_fill_response(&valid_fill(), None).expect("fill");
    let item = work_item("wi_1", "dmg-1");

    let context = full_context();
    let draft = service::draft_from_fill(&item, &context, &fill, 1, "m", 5_000);
    assert_eq!(
        draft.claim_amount_cents, 15_000,
        "damage report amount wins"
    );
    assert!(draft.packet.ready);

    let mut no_damage_amount = full_context();
    no_damage_amount.damage_claim_amount_cents = None;
    let draft = service::draft_from_fill(&item, &no_damage_amount, &fill, 1, "m", 5_000);
    assert_eq!(draft.claim_amount_cents, 21_999, "order total fallback");

    let mut neither = no_damage_amount.clone();
    neither.order_total_cents = None;
    let draft = service::draft_from_fill(&item, &neither, &fill, 1, "m", 5_000);
    assert_eq!(draft.claim_amount_cents, 0, "operator must set one");
}

#[test]
fn packet_email_renders_refs_links_and_checklist() {
    let fill = service::parse_narrative_fill_response(&valid_fill(), None).expect("fill");
    let draft = service::draft_from_fill(
        &work_item("wi_1", "dmg-1"),
        &full_context(),
        &fill,
        1,
        "m",
        5_000,
    );

    let (subject, body) = service::render_packet_email(&draft);

    assert!(subject.contains("1Z999AA10123456784"));
    assert!(body.contains("Claim amount: $150.00"));
    assert!(body.contains("https://business-cafdd46cb1.example.test/asset-c1c3e62eb2.jpg"));
    assert!(body.contains("https://business-cafdd46cb1.example.test/asset-8d9c57bdde.jpg"));
    assert!(body.contains("CHECKLIST"));
    assert!(body.contains(&draft.damage_narrative));
}

#[test]
fn packet_email_renders_speedship_ltl_refs_and_documents() {
    let fill = service::parse_narrative_fill_response(&valid_fill(), None).expect("fill");
    let mut context = full_context();
    context.tracking_number = None;
    context.shipment_number = None;
    context.carrier = Some("LTL Carrier".to_string());
    context.shipment_refs = Some(ClaimShipmentRefs {
        shipping_platform: Some("speedship".to_string()),
        platform_shipment_id: Some("ss-123".to_string()),
        carrier: Some("LTL Carrier".to_string()),
        carrier_service: Some("standard".to_string()),
        mode: Some("ltl".to_string()),
        pro_number: Some("PRO-456".to_string()),
        bol_number: Some("BOL-789".to_string()),
        tracking_url: Some("https://business-d86a254831.example.test/track/ss-123".to_string()),
        document_refs: vec![ClaimShipmentDocumentRef {
            kind: "pod".to_string(),
            url: "https://business-cafdd46cb1.example.test/asset-c3d450cd72.jpg".to_string(),
        }],
        claim_platform: Some("speedship".to_string()),
        claim_api_supported: Some(false),
        tracking_number: None,
    });
    let draft = service::draft_from_fill(
        &work_item("wi_ltl", "dmg-1"),
        &context,
        &fill,
        1,
        "m",
        5_000,
    );

    assert!(draft.packet.ready);
    let (subject, body) = service::render_packet_email(&draft);

    assert!(subject.contains("PRO PRO-456"));
    assert!(body.contains("Shipping platform: speedship"));
    assert!(body.contains("PRO number: PRO-456"));
    assert!(body.contains("BOL number: BOL-789"));
    assert!(body
        .contains("Document pod: https://business-cafdd46cb1.example.test/asset-c3d450cd72.jpg"));
    assert!(body.contains("Claim API supported: no"));
}

// ---------------------------------------------------------------------------
// Claims pump
// ---------------------------------------------------------------------------

fn receipt_count(state: &AppState) -> i64 {
    let persistence = state.persistence.lock();
    persistence
        .connection_ref()
        .query_row("SELECT COUNT(*) FROM receipts", [], |row| row.get(0))
        .expect("receipt count")
}

#[test]
fn pump_upserts_damage_emits_items_and_fetches_pack_photos() {
    let state = test_state();
    seed_order(&state, "o1", "shp-1");
    let fixture = FixtureStockforgeReadClient {
        damage_events: vec![damage_event("dmg-1", "shp-1")],
        container_photos: HashMap::from([(
            "cont-1".to_string(),
            vec![SfPackPhotoRecord {
                photo_id: "p1".to_string(),
                url: "https://business-cafdd46cb1.example.test/asset-8d9c57bdde.jpg".to_string(),
                captured_at: Some("2026-06-03T10:00:00Z".to_string()),
            }],
        )]),
        ..Default::default()
    };

    let summary = worker::run_sync_cycle(&state, &fixture, "key", 5, 2_000).expect("cycle");

    // OPEN damage list + RESOLVED damage list + 1 container photo request.
    assert_eq!(summary.requests_used, 3);
    assert_eq!(summary.upserted, 1);
    assert_eq!(summary.items_emitted, 1);
    assert_eq!(summary.photos_fetched, 1);
    {
        let persistence = state.persistence.lock();
        let conn = persistence.connection_ref();
        let snapshot = store::get_damage_snapshot(conn, CLIENT, "dmg-1")
            .expect("get")
            .expect("exists");
        assert!(snapshot.pack_photos_fetched);
        assert_eq!(
            snapshot.pack_photo_urls,
            vec!["https://business-cafdd46cb1.example.test/asset-8d9c57bdde.jpg"]
        );
        let items = crate::slices::work_queue::store::list_items(
            conn,
            CLIENT,
            None,
            10,
            &crate::http::OperatorScope::All,
        )
        .expect("items");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].item.source_kind, "stockforge_damage");
        assert_eq!(items[0].item.packet_kinds, vec!["claim_draft"]);
        assert_eq!(items[0].item.category_id, super::DAMAGE_CATEGORY);
    }

    // Steady state: same event again → zero writes, zero receipts, no
    // duplicate work item.
    let receipts_after_first = receipt_count(&state);
    let summary = worker::run_sync_cycle(&state, &fixture, "key", 5, 60_000).expect("cycle 2");
    assert_eq!(summary.upserted, 0);
    assert_eq!(summary.items_emitted, 0);
    assert_eq!(summary.requests_used, 2); // OPEN + RESOLVED damage lists only
    assert_eq!(receipt_count(&state), receipts_after_first);
}

#[test]
fn pump_refreshes_resolved_damage_without_emitting_queue_items() {
    let state = test_state();
    let fixture = FixtureStockforgeReadClient {
        damage_events: vec![resolved_damage_event("dmg-resolved", "shp-2")],
        ..Default::default()
    };

    let summary = worker::run_sync_cycle(&state, &fixture, "key", 5, 2_000).expect("cycle");

    assert_eq!(summary.requests_used, 2);
    assert_eq!(summary.upserted, 1);
    assert_eq!(summary.items_emitted, 0);
    let persistence = state.persistence.lock();
    let snapshot = store::get_damage_snapshot(persistence.connection_ref(), CLIENT, "dmg-resolved")
        .expect("get")
        .expect("exists");
    assert_eq!(snapshot.claim_status, "RESOLVED");
    let items = crate::slices::work_queue::store::list_items(
        persistence.connection_ref(),
        CLIENT,
        None,
        10,
        &crate::http::OperatorScope::All,
    )
    .expect("items");
    assert!(items.is_empty());
}

#[test]
fn pump_marks_unmatched_shipments_done_without_spending_requests() {
    let state = test_state();
    // No order snapshot for shp-9 — pack photos resolve locally to none.
    let fixture = FixtureStockforgeReadClient {
        damage_events: vec![damage_event("dmg-9", "shp-9")],
        ..Default::default()
    };

    let summary = worker::run_sync_cycle(&state, &fixture, "key", 5, 2_000).expect("cycle");

    assert_eq!(
        summary.requests_used, 2,
        "no photo request for unmatched shipment"
    );
    let persistence = state.persistence.lock();
    let snapshot = store::get_damage_snapshot(persistence.connection_ref(), CLIENT, "dmg-9")
        .expect("get")
        .expect("exists");
    assert!(snapshot.pack_photos_fetched);
    assert!(snapshot.pack_photo_urls.is_empty());
}

#[test]
fn rate_limit_stamps_the_cursor_and_the_next_cycle_stands_down() {
    use bos_integrations::stockforge_read::{SfPage, StockforgeError, StockforgeReadClient};
    struct ThrottledClient;
    impl StockforgeReadClient for ThrottledClient {
        fn fetch_materials(
            &self,
            _: &str,
            _: u32,
            _: u32,
        ) -> Result<SfPage<bos_integrations::stockforge_read::SfMaterialRecord>, StockforgeError>
        {
            unreachable!()
        }
        fn fetch_active_alerts(
            &self,
            _: &str,
        ) -> Result<Vec<bos_integrations::stockforge_read::SfAlertRecord>, StockforgeError>
        {
            unreachable!()
        }
        fn fetch_reorder_suggestions(
            &self,
            _: &str,
        ) -> Result<
            Vec<bos_integrations::stockforge_read::SfReorderSuggestionRecord>,
            StockforgeError,
        > {
            unreachable!()
        }
        fn fetch_order_board(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<Vec<SfOrderCardRecord>, StockforgeError> {
            unreachable!()
        }
        fn fetch_purchase_orders(
            &self,
            _: &str,
            _: u32,
            _: u32,
        ) -> Result<SfPage<bos_integrations::stockforge_read::SfPurchaseOrderRecord>, StockforgeError>
        {
            unreachable!()
        }
        fn fetch_damage_events(
            &self,
            _: &str,
            _: &str,
            _: u32,
            _: u32,
        ) -> Result<SfPage<SfDamageEventRecord>, StockforgeError> {
            Err(StockforgeError::RateLimited {
                retry_after_ms: Some(45_000),
                message: "throttled".to_string(),
            })
        }
        fn fetch_container_photos(
            &self,
            _: &str,
            _: &str,
        ) -> Result<Option<Vec<SfPackPhotoRecord>>, StockforgeError> {
            unreachable!()
        }
    }

    let state = test_state();
    let summary = worker::run_sync_cycle(&state, &ThrottledClient, "key", 5, 1_000).expect("cycle");
    assert!(summary.rate_limited);
    let summary =
        worker::run_sync_cycle(&state, &ThrottledClient, "key", 5, 10_000).expect("standdown");
    assert_eq!(summary.requests_used, 0);
}

// ---------------------------------------------------------------------------
// Approve lifecycle
// ---------------------------------------------------------------------------

fn staged_claim(state: &AppState, item_id: &str, context: &ClaimContext) -> String {
    let fill = service::parse_narrative_fill_response(&valid_fill(), None).expect("fill");
    let draft = service::draft_from_fill(
        &work_item(item_id, &context.damage_event_id),
        context,
        &fill,
        1,
        "m",
        5_000,
    );
    let draft_id = draft.draft_id.clone();
    let mut persistence = state.persistence.lock();
    store::insert_draft(
        persistence.connection(),
        CLIENT,
        "operator",
        &draft,
        &format!("stage:{draft_id}"),
    )
    .expect("insert");
    draft_id
}

#[test]
fn approve_enqueues_the_gmail_job_and_creates_the_tracking_task() {
    let state = test_state();
    let draft_id = staged_claim(&state, "wi_ok", &full_context());

    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let staged = store::get_draft(conn, CLIENT, &draft_id)
        .expect("get")
        .expect("exists");
    let job = service::build_approval_job(
        &staged.draft,
        "claims@business-27771bded2.example.test",
        Some("example"),
        "example",
        6_000,
    )
    .expect("job");
    assert_eq!(job.provider, "gmail");
    assert_eq!(job.capability, "create_draft");
    let task = service::tracking_task(&staged.draft, 6_000);
    store::approve_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "example",
            expected_revision: Some(staged.revision),
            idempotency_key: "appr-1",
            now_ms: 6_000,
        },
        &draft_id,
        &job,
        &task,
    )
    .expect("approve");

    let approved = store::get_draft(conn, CLIENT, &draft_id)
        .expect("get")
        .expect("exists");
    assert_eq!(
        approved.draft.status,
        bos_contracts::claim_drafts::ClaimDraftStatus::Approved
    );
    assert_eq!(
        approved.draft.outbox_job_id.as_deref(),
        Some(job.job_id.as_str())
    );
    assert_eq!(
        approved.draft.follow_up_task_id.as_deref(),
        Some(task.task_id.as_str())
    );
    let summary = approved.outbox_job.expect("job summary");
    assert_eq!(summary.status, "pending");
    // The payload is the standard Gmail draft-create shape — the existing
    // gated gmail delivery executes it.
    let payload: serde_json::Value = conn
        .query_row(
            "SELECT payload_json FROM outbox_jobs WHERE client_id = ?1 AND job_id = ?2",
            rusqlite::params![CLIENT, job.job_id],
            |row| row.get::<_, String>(0),
        )
        .map(|raw| serde_json::from_str(&raw).expect("payload json"))
        .expect("job row");
    assert_eq!(payload["to"], "claims@business-27771bded2.example.test");
    assert!(payload["body_text"]
        .as_str()
        .unwrap()
        .contains("1Z999AA10123456784"));
    // The tracking task landed in the tasks table (follow_up_tasks slice).
    let task_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tasks WHERE client_id = ?1 AND task_id = ?2",
            rusqlite::params![CLIENT, task.task_id],
            |row| row.get(0),
        )
        .expect("task row");
    assert_eq!(task_count, 1);
}

#[test]
fn approval_refuses_incomplete_packets_and_zero_amounts() {
    let state = test_state();
    let mut incomplete = full_context();
    incomplete.damage_event_id = "dmg-2".to_string();
    incomplete.damage_photo_urls.clear(); // missing damage_photo role
    let draft_id = staged_claim(&state, "wi_bad", &incomplete);

    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let staged = store::get_draft(conn, CLIENT, &draft_id)
        .expect("get")
        .expect("exists");
    assert!(!staged.draft.packet.ready);
    let job = service::build_approval_job(
        &staged.draft,
        "claims@business-27771bded2.example.test",
        None,
        "operator",
        6_000,
    )
    .expect("job");
    let task = service::tracking_task(&staged.draft, 6_000);
    let err = store::approve_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "operator",
            expected_revision: None,
            idempotency_key: "appr-2",
            now_ms: 6_000,
        },
        &draft_id,
        &job,
        &task,
    )
    .expect_err("incomplete packet must refuse");
    assert!(matches!(
        err,
        crate::store_core::StoreError::Domain(code) if code == "claim_packet_incomplete"
    ));

    // Zero amount refuses too (complete packet, no grounded amount).
    drop(persistence);
    let mut amountless = full_context();
    amountless.damage_event_id = "dmg-3".to_string();
    amountless.damage_claim_amount_cents = None;
    amountless.order_total_cents = None;
    // order_number stays, so the packet itself remains complete.
    let draft_id = staged_claim(&state, "wi_zero", &amountless);
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let staged = store::get_draft(conn, CLIENT, &draft_id)
        .expect("get")
        .expect("exists");
    assert!(staged.draft.packet.ready);
    assert_eq!(staged.draft.claim_amount_cents, 0);
    let err = store::approve_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "operator",
            expected_revision: None,
            idempotency_key: "appr-3",
            now_ms: 7_000,
        },
        &draft_id,
        &job,
        &task,
    )
    .expect_err("zero amount must refuse");
    assert!(matches!(
        err,
        crate::store_core::StoreError::Domain(code) if code == "claim_draft_amount_required"
    ));

    // Operator sets the amount (the human is the grounding), then the
    // amount gate passes.
    store::update_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "operator",
            expected_revision: None,
            idempotency_key: "edit-1",
            now_ms: 8_000,
        },
        &draft_id,
        "Edited narrative for the claim form.",
        "Two cleaning solution containers",
        9_900,
    )
    .expect("update");
    let updated = store::get_draft(conn, CLIENT, &draft_id)
        .expect("get")
        .expect("exists");
    assert_eq!(updated.draft.claim_amount_cents, 9_900);
    assert_eq!(
        updated.draft.damage_narrative,
        "Edited narrative for the claim form."
    );
}

#[test]
fn produce_context_flows_from_local_caches() {
    use crate::produce::ProduceFlavor;
    let state = test_state();
    seed_order(&state, "o1", "shp-1");
    {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        store::upsert_damage_snapshot(conn, CLIENT, &damage_event("dmg-1", "shp-1"), 1_000)
            .expect("snapshot");
        store::set_pack_photos(
            conn,
            CLIENT,
            "dmg-1",
            &["https://business-cafdd46cb1.example.test/asset-8d9c57bdde.jpg".to_string()],
            1_500,
        )
        .expect("pack photos");
    }
    let item = work_item("wi_ctx", "dmg-1");
    let flavor = service::Produce;

    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let snapshot = store::get_damage_snapshot(conn, CLIENT, "dmg-1")
        .expect("get")
        .expect("exists");
    let message = service::produce_source_view(&snapshot);
    assert!(message.body_excerpt.contains("crushed"));
    let context_value = flavor
        .prepare_context(
            conn,
            CLIENT,
            &item,
            &message,
            &crate::http::OperatorScope::All,
            "operator",
        )
        .expect("context");
    let context: ClaimContext = serde_json::from_value(context_value.clone()).expect("round-trip");
    assert_eq!(context.order_number.as_deref(), Some("#o1"));
    assert_eq!(context.order_platform.as_deref(), Some("shopify"));
    assert_eq!(context.external_order_id.as_deref(), Some("shopify-o1"));
    assert_eq!(context.order_total_cents, Some(21_999));
    assert_eq!(context.pack_photo_urls.len(), 1);
    assert!(service::evaluate_packet_gate(&context).ready);

    // Stage through the flavor: the staged draft carries the packet gate.
    flavor
        .stage(crate::produce::StageContext {
            conn: &mut *conn,
            client_id: CLIENT,
            actor_id: "operator",
            item: &item,
            message: &message,
            response: &valid_fill(),
            context: &context_value,
            model: "model-x",
            attempt: 1,
            idempotency_key: "stage:ctx",
            now_ms: 10_000,
        })
        .expect("stage");
    let staged = store::active_draft_for_item(conn, CLIENT, "wi_ctx")
        .expect("get")
        .expect("exists");
    assert!(staged.draft.packet.ready);
    assert_eq!(staged.draft.claim_amount_cents, 15_000);
    assert_eq!(staged.draft.model, "model-x");
}
