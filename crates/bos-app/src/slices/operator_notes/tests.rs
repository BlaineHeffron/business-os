//! Slice tests: note creation emits its work item idempotently, policy
//! overrides the default kinds, and the produce-source view is consumable.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use bos_contracts::operator_notes::OperatorNote;
use bos_contracts::work_queue::WorkQueuePolicy;
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

use super::{service, store};
use crate::http::{build_router, test_support::test_state_configured};
use crate::persistence::Persistence;
use crate::store_core::StoreError;

const CLIENT: &str = "test-client";

fn note(body: &str) -> OperatorNote {
    OperatorNote {
        note_id: "note_k1".to_string(),
        body: body.to_string(),
        category_id: service::DEFAULT_CATEGORY.to_string(),
        created_by: "jordan".to_string(),
        created_at_ms: 1_000,
    }
}

#[test]
fn note_create_emits_accepted_item_once_with_selected_kinds() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let note = note("Dana called — wants the storefront quote by Friday.\nMore detail here.");
    store::insert_note(conn, CLIENT, &note, "k1").expect("insert");

    let actions = vec!["crm_activity".to_string(), "invoice_draft".to_string()];
    assert!(service::emit_item_for_note(conn, CLIENT, &note, &actions, 1_000).expect("emit"));
    // Replay (retried request) emits nothing new.
    assert!(!service::emit_item_for_note(conn, CLIENT, &note, &actions, 2_000).expect("re-emit"));

    let items = crate::slices::work_queue::store::list_items(
        conn,
        CLIENT,
        None,
        10,
        &crate::http::OperatorScope::All,
    )
    .expect("items");
    assert_eq!(items.len(), 1);
    let item = &items[0].item;
    assert_eq!(item.source_kind, "operator_note");
    assert_eq!(item.source_ref, "note_k1");
    assert_eq!(item.category_id, "operator_note");
    assert_eq!(
        item.title, "Dana called — wants the storefront quote by Friday.",
        "title is the first line"
    );
    assert_eq!(
        item.status,
        bos_contracts::work_queue::WorkItemStatus::Accepted,
        "note actions accept the self-authored item implicitly (D2)"
    );
    assert_eq!(
        item.packet_kinds, actions,
        "the selected actions ride as the item's kinds"
    );
}

#[tokio::test]
async fn manual_composer_note_returns_stable_item_without_kicking_produce() {
    let state = test_state_configured(None, &[]);
    let response = build_router(state.clone())
        .oneshot(
            Request::post("/api/operator-notes")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "body": "Create a typed email output.\nTo: dana@example.test",
                        "idempotency_key": "composer_manual_note",
                        "actor_id": null,
                        "actions": ["email_draft_reply"],
                        "auto_produce": false
                    })
                    .to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let body: bos_contracts::operator_notes::OperatorNoteCreateResponse =
        serde_json::from_slice(&bytes).expect("typed response");
    assert_eq!(
        body.work_item_id,
        "wi_operator_note_note_composer_manual_note"
    );
    assert!(body.work_item_emitted);
    assert!(crate::produce::produce_in_flight_snapshot(&state).is_empty());
}

#[test]
fn default_actions_is_the_crm_note() {
    // D2: CRM pre-checked, the others off — empty selection logs a CRM note.
    assert_eq!(service::default_actions(), vec!["crm_activity".to_string()]);
}

#[test]
fn resolve_actions_defaults_validates_and_dedups() {
    // Empty → the default CRM note.
    assert_eq!(
        service::resolve_actions(&[]).expect("default"),
        vec!["crm_activity".to_string()]
    );
    // Blank-only entries collapse to the default too.
    assert_eq!(
        service::resolve_actions(&["  ".to_string()]).expect("blank"),
        vec!["crm_activity".to_string()]
    );
    // Valid catalog kinds ride through, order-preserving + deduped.
    assert_eq!(
        service::resolve_actions(&[
            "crm_activity".to_string(),
            "invoice_draft".to_string(),
            "crm_activity".to_string(),
        ])
        .expect("valid"),
        vec!["crm_activity".to_string(), "invoice_draft".to_string()]
    );
    // The CRM-records kind is a valid, directly-selectable action (the note
    // form now offers it so the operator can request creating a contact +
    // company, not just a note).
    assert_eq!(
        service::resolve_actions(&["crm_record_create".to_string()]).expect("valid"),
        vec!["crm_record_create".to_string()]
    );
    // An unknown action id is refused with the 400 wire code.
    assert_eq!(
        service::resolve_actions(&["not_a_kind".to_string()]),
        Err("operator_note_action_invalid")
    );
}

#[test]
fn selected_kinds_override_category_policy() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    crate::slices::work_queue::store::upsert_policy(
        conn,
        CLIENT,
        "op_test",
        &WorkQueuePolicy {
            category_id: service::DEFAULT_CATEGORY.to_string(),
            create_work_item: false, // irrelevant for unconditional emit
            packet_kinds: vec!["calendar_event_draft".to_string()],
            ai_suggestible_packet_kinds: Vec::new(),
            ai_suggestible_gmail_scope: Default::default(),
            ai_suggestible_gmail_categories: Vec::new(),
            auto_produce: false,
        },
        "p1",
        500,
    )
    .expect("policy");

    let note = note("Soccer practice Thursday 4pm");
    store::insert_note(conn, CLIENT, &note, "k1").expect("insert");
    service::emit_item_for_note(
        conn,
        CLIENT,
        &note,
        &["invoice_draft".to_string(), "follow_up_task".to_string()],
        1_000,
    )
    .expect("emit");

    let items = crate::slices::work_queue::store::list_items(
        conn,
        CLIENT,
        None,
        10,
        &crate::http::OperatorScope::All,
    )
    .expect("items");
    assert_eq!(
        items[0].item.packet_kinds,
        vec!["invoice_draft", "follow_up_task"],
        "note-form checkboxes are an explicit per-item selection"
    );
}

#[test]
fn produce_source_view_feeds_kinds_without_a_sender() {
    let note = note("Dana called — wants the storefront quote by Friday.");
    let view = service::produce_source_view(&note);
    assert_eq!(view.message_id, "note_k1");
    assert_eq!(view.from_addr, None, "notes have no reply recipient");
    assert_eq!(view.body_excerpt, note.body);
    assert_eq!(view.resolved_category, "operator_note");

    // An email reply draft over a note must fail its recipient guard.
    let fill = crate::slices::email_drafts::service::ReplyFill {
        body_text: "hi".to_string(),
        confidence: "high".to_string(),
        provenance: Vec::new(),
    };
    let item = bos_contracts::work_queue::WorkItem {
        item_id: "wi_operator_note_note_k1".to_string(),
        source_kind: "operator_note".to_string(),
        source_ref: "note_k1".to_string(),
        category_id: "operator_note".to_string(),
        title: "t".to_string(),
        summary: String::new(),
        packet_kinds: vec!["email_draft_reply".to_string()],
        status: bos_contracts::work_queue::WorkItemStatus::Accepted,
        accept_actor: Some(bos_contracts::work_queue::WorkItemAcceptActor::Operator),
        ai_suggested: false,
        rationale: String::new(),
        produce_guidance: String::new(),
        source_user_id: None,
        assignee_user_id: None,
        visible_to_user_ids: Vec::new(),
        created_at_ms: 1,
        updated_at_ms: 1,
    };
    assert!(crate::slices::email_drafts::service::draft_from_fill(
        &item,
        &view,
        &fill,
        1,
        "test-model",
        2_000
    )
    .is_err());
}

#[test]
fn empty_note_body_is_refused() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let empty = note("   ");
    let err = store::insert_note(conn, CLIENT, &empty, "k1").expect_err("must refuse");
    assert!(matches!(err, StoreError::Domain(code) if code == "operator_note_body_empty"));
}
