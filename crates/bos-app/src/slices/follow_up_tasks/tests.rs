//! Slice tests: fill parsing/validation and the stage → approve-creates-task
//! lifecycle. LLM interactions are tested at the parse/build seams only.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use bos_contracts::follow_up_tasks::{FollowUpDraftStatus, TaskStatus};
use bos_contracts::work_queue::{WorkItem, WorkItemStatus};
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

use super::service;
use super::store::{self, DraftActionContext, TaskAction};
use crate::http::{build_router, test_support::test_state_configured, OperatorScope};
use crate::persistence::Persistence;
use crate::store_core::{MutationOutcome, StoreError};

const CLIENT: &str = "test-client";

#[test]
fn manual_follow_up_uses_the_same_typed_validation_as_edits() {
    let item = accepted_item();
    let fields = store::normalize_editable_fields(
        "Call Dana about the quote",
        Some("2026-06-15"),
        "Confirm the storefront measurements.",
        1_781_000_000_000,
    )
    .expect("manual fields");
    let draft = service::manual_draft(&item, fields, 1, 2_000);
    assert_eq!(draft.title, "Call Dana about the quote");
    assert_eq!(draft.due_date.as_deref(), Some("2026-06-15"));
    assert_eq!(draft.model, "manual");
    assert_eq!(draft.status, FollowUpDraftStatus::Staged);
}

#[tokio::test]
async fn manual_follow_up_route_stages_the_typed_owner_draft_and_receipt() {
    let state = test_state_configured(None, &[]);
    {
        let mut persistence = state.persistence.lock();
        crate::slices::work_queue::store::insert_item(
            persistence.connection(),
            CLIENT,
            &accepted_item(),
        )
        .expect("work item");
    }
    let response = build_router(state.clone())
        .oneshot(
            Request::post("/api/follow-up-drafts/manual")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "item_id": "wi_email_m2",
                        "title": "Call Dana about the quote",
                        "due_date": "2026-06-15",
                        "context": "Confirm the storefront measurements.",
                        "idempotency_key": "manual_follow_up_route",
                        "actor_id": null
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
    let body: bos_contracts::follow_up_tasks::FollowUpDraftProduceResponse =
        serde_json::from_slice(&bytes).expect("typed response");
    assert_eq!(body.draft.draft.title, "Call Dana about the quote");
    assert_eq!(body.draft.draft.model, "manual");

    let persistence = state.persistence.lock();
    let receipt_count: i64 = persistence
        .connection_ref()
        .query_row(
            "SELECT COUNT(*) FROM receipts WHERE client_id = ?1 AND entity_kind = ?2 AND idempotency_key = ?3 AND outcome = 'applied'",
            rusqlite::params![CLIENT, store::DRAFT_ENTITY_KIND, "manual_follow_up_route"],
            |row| row.get(0),
        )
        .expect("receipt count");
    assert_eq!(receipt_count, 1);
}

#[test]
fn task_fill_request_includes_background_when_present() {
    use bos_integrations::llm_typed_tasks::TypedLlmTextBlock;
    let item = accepted_item();
    let message = bos_contracts::email_triage::InboundMessageRecord {
        source_key: "m_bg".to_string(),
        message_id: "m_bg".to_string(),
        thread_id: None,
        internal_date_ms: Some(1_781_000_000_000),
        from_addr: Some("a@test".to_string()),
        to_addr: Some("b@test".to_string()),
        subject: Some("Reminder".to_string()),
        body_excerpt: "Call the supplier back next week.".to_string(),
        body_full: String::new(),
        headers: Vec::new(),
        labels: Vec::new(),
        resolved_category: "operator_note".to_string(),
        matched_rule_id: None,
        ingested_at_ms: 1_000,
        ai_triage_status: None,
        ai_triage_rationale: None,
        attachments: Vec::new(),
        source_user_id: None,
    };

    let plain = service::build_task_fill_request(
        CLIENT,
        &item,
        &message,
        &json!({ "background": null }),
        1,
    );
    assert!(!plain
        .input
        .text_blocks
        .iter()
        .any(|b| b.block_id == "background"));
    let email_block = plain
        .input
        .text_blocks
        .iter()
        .find(|b| b.block_id == "email")
        .expect("email block");
    assert!(email_block.text.contains("Date (epoch ms): 1781000000000"));
    assert!(email_block.text.contains("Email date (UTC): 2026-06-09"));

    let block = TypedLlmTextBlock {
        block_id: "background".to_string(),
        text: "Company: Example Company".to_string(),
    };
    let context = json!({ "background": serde_json::to_value(&block).unwrap() });
    let grounded = service::build_task_fill_request(CLIENT, &item, &message, &context, 1);
    let backgrounds: Vec<_> = grounded
        .input
        .text_blocks
        .iter()
        .filter(|b| b.block_id == "background")
        .collect();
    assert_eq!(backgrounds.len(), 1);
    assert_eq!(backgrounds[0].text, "Company: Example Company");
}

fn accepted_item() -> WorkItem {
    WorkItem {
        item_id: "wi_email_m2".to_string(),
        source_kind: "email".to_string(),
        source_ref: "m2".to_string(),
        category_id: "inquiries".to_string(),
        title: "Quote request from Dana".to_string(),
        summary: "From dana@example.test — needs a decision".to_string(),
        packet_kinds: vec!["follow_up_task".to_string()],
        status: WorkItemStatus::Accepted,
        accept_actor: Some(bos_contracts::work_queue::WorkItemAcceptActor::Operator),
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

fn valid_fill_response() -> serde_json::Value {
    json!({
        "title": "Reply to Dana about the repair quote",
        "due_date": "2026-06-15",
        "context": "Dana asked for pricing on the storefront job and wants an answer by Monday.",
        "confidence": "high",
        "provenance": [
            {"field": "title", "quote": "could you send me a quote"},
            {"field": "due_date", "quote": "by Monday"},
            {"field": "not_a_field", "quote": "dropped"}
        ]
    })
}

fn staged_draft(conn: &mut rusqlite::Connection) -> String {
    let item = accepted_item();
    let fill = service::parse_task_fill_response(&valid_fill_response()).expect("fill");
    let draft = service::draft_from_fill(&item, &fill, 1, "test-model", 2_000);
    store::insert_draft(conn, CLIENT, "op_test", &draft, "produce_1").expect("stage");
    draft.draft_id
}

fn staged_draft_for_user(conn: &mut rusqlite::Connection, user_id: &str, key: &str) -> String {
    let mut item = accepted_item();
    item.item_id = format!("wi_email_{user_id}");
    item.source_ref = format!("m_{user_id}");
    item.source_user_id = Some(user_id.to_string());
    let fill = service::parse_task_fill_response(&valid_fill_response()).expect("fill");
    let draft = service::draft_from_fill(&item, &fill, 1, "test-model", 2_000);
    store::insert_draft(conn, CLIENT, user_id, &draft, key).expect("stage");
    draft.draft_id
}

// --- service: parse + validate ---

#[test]
fn parse_valid_fill_keeps_known_provenance_only() {
    let fill = service::parse_task_fill_response(&valid_fill_response()).expect("fill");
    assert_eq!(fill.title, "Reply to Dana about the repair quote");
    assert_eq!(fill.due_date.as_deref(), Some("2026-06-15"));
    assert_eq!(fill.confidence, "high");
    let fields: Vec<&str> = fill.provenance.iter().map(|p| p.field.as_str()).collect();
    assert_eq!(fields, vec!["title", "due_date"]);
}

#[test]
fn parse_allows_missing_due_date_but_rejects_bad_one() {
    let mut no_due = valid_fill_response();
    no_due["due_date"] = json!(null);
    let fill = service::parse_task_fill_response(&no_due).expect("fill without due date");
    assert_eq!(fill.due_date, None);

    let mut bad_due = valid_fill_response();
    bad_due["due_date"] = json!("next Monday");
    assert!(service::parse_task_fill_response(&bad_due).is_err());

    let mut bad_month = valid_fill_response();
    bad_month["due_date"] = json!("2026-13-01");
    assert!(service::parse_task_fill_response(&bad_month).is_err());
}

#[test]
fn parse_rejects_missing_title_or_confidence() {
    let mut no_title = valid_fill_response();
    no_title["title"] = json!("");
    assert!(service::parse_task_fill_response(&no_title).is_err());

    let mut bad_confidence = valid_fill_response();
    bad_confidence["confidence"] = json!("sure");
    assert!(service::parse_task_fill_response(&bad_confidence).is_err());
}

#[test]
fn iso_date_validator() {
    assert!(service::is_iso_date("2026-06-15"));
    assert!(service::is_iso_date("2026-12-31"));
    for bad in [
        "2026-6-15",
        "26-06-15",
        "2026/06/15",
        "2026-00-10",
        "2026-12-32",
        "tomorrow",
    ] {
        assert!(!service::is_iso_date(bad), "{bad} should fail");
    }
}

#[test]
fn produce_guards_require_accepted_item_with_kind() {
    let mut open_item = accepted_item();
    open_item.status = WorkItemStatus::Open;
    assert!(crate::produce::validate_item_for_kind(&open_item, service::PACKET_KIND).is_err());

    let mut wrong_kind = accepted_item();
    wrong_kind.packet_kinds = vec!["calendar_event_draft".to_string()];
    assert!(crate::produce::validate_item_for_kind(&wrong_kind, service::PACKET_KIND).is_err());

    assert!(crate::produce::validate_item_for_kind(&accepted_item(), service::PACKET_KIND).is_ok());
}

// --- store: stage → approve-creates-task lifecycle ---

#[test]
fn second_active_draft_for_item_is_refused() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    staged_draft(conn);

    let item = accepted_item();
    let fill = service::parse_task_fill_response(&valid_fill_response()).expect("fill");
    let second = service::draft_from_fill(&item, &fill, 2, "test-model", 3_000);
    let err = store::insert_draft(conn, CLIENT, "op_test", &second, "produce_2")
        .expect_err("second active draft must be refused");
    match err {
        StoreError::Domain(code) => assert_eq!(code, "follow_up_draft_already_active"),
        other => panic!("expected domain error, got {other:?}"),
    }
}

#[test]
fn approve_creates_local_task_atomically() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let draft_id = staged_draft(conn);
    let draft = store::get_draft(conn, CLIENT, &draft_id, &OperatorScope::All)
        .expect("get")
        .expect("exists");
    let task = service::task_from_draft(&draft.draft, 5_000);

    let outcome = store::approve_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "op_test",
            scope: &OperatorScope::All,
            expected_revision: Some(draft.revision),
            idempotency_key: "approve_1",
            now_ms: 5_000,
        },
        &draft_id,
        &task,
    )
    .expect("approve");
    assert!(matches!(outcome, MutationOutcome::Applied { .. }));

    let approved = store::get_draft(conn, CLIENT, &draft_id, &OperatorScope::All)
        .expect("get")
        .expect("exists");
    assert_eq!(approved.draft.status, FollowUpDraftStatus::Approved);
    assert_eq!(
        approved.draft.task_id.as_deref(),
        Some(task.task_id.as_str())
    );

    let tasks = store::list_tasks(
        conn,
        CLIENT,
        Some(TaskStatus::Open),
        10,
        &OperatorScope::All,
    )
    .expect("tasks");
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].task.task_id, task.task_id);
    assert_eq!(tasks[0].task.title, "Reply to Dana about the repair quote");
    assert_eq!(tasks[0].task.due_date.as_deref(), Some("2026-06-15"));
    assert_eq!(tasks[0].task.source_item_id.as_deref(), Some("wi_email_m2"));
    assert_eq!(tasks[0].revision, 1);

    let complete = store::apply_task_action(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "op_test",
            scope: &OperatorScope::All,
            expected_revision: Some(tasks[0].revision),
            idempotency_key: "complete_1",
            now_ms: 5_500,
        },
        &task.task_id,
        TaskAction::Complete,
    )
    .expect("fresh task revision should complete");
    assert!(matches!(
        complete,
        MutationOutcome::Applied { revision: 2, .. }
    ));

    // Double approve is refused (and would also violate the task PK).
    let again = store::approve_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "op_test",
            scope: &OperatorScope::All,
            expected_revision: None,
            idempotency_key: "approve_2",
            now_ms: 6_000,
        },
        &draft_id,
        &task,
    );
    assert!(again.is_err(), "double approve must be refused");
}

#[test]
fn revision_conflict_on_stale_approve_creates_no_task() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let draft_id = staged_draft(conn);
    let draft = store::get_draft(conn, CLIENT, &draft_id, &OperatorScope::All)
        .expect("get")
        .expect("exists");
    let task = service::task_from_draft(&draft.draft, 5_000);
    let outcome = store::approve_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "op_test",
            scope: &OperatorScope::All,
            expected_revision: Some(draft.revision + 9),
            idempotency_key: "approve_stale",
            now_ms: 5_000,
        },
        &draft_id,
        &task,
    )
    .expect("conflict path returns Ok");
    assert!(matches!(outcome, MutationOutcome::RevisionConflict { .. }));

    let tasks = store::list_tasks(conn, CLIENT, None, 10, &OperatorScope::All).expect("tasks");
    assert!(
        tasks.is_empty(),
        "conflicted approve must not create a task"
    );
}

#[test]
fn approve_refuses_to_overwrite_existing_task_revision() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let draft_id = staged_draft(conn);
    let draft = store::get_draft(conn, CLIENT, &draft_id, &OperatorScope::All)
        .expect("get")
        .expect("exists");
    let task = service::task_from_draft(&draft.draft, 5_000);
    let tx = conn.transaction().expect("tx");
    crate::store_core::initialize_revision_within(
        &tx,
        CLIENT,
        store::TASK_ENTITY_KIND,
        &task.task_id,
        9,
        4_000,
    )
    .expect("seed orphan revision");
    tx.commit().expect("commit orphan revision");

    let err = store::approve_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "op_test",
            scope: &OperatorScope::All,
            expected_revision: Some(draft.revision),
            idempotency_key: "approve_orphan_revision",
            now_ms: 5_000,
        },
        &draft_id,
        &task,
    )
    .expect_err("must not overwrite an existing task revision");
    assert!(matches!(err, StoreError::Sqlite(_)));

    let tasks = store::list_tasks(conn, CLIENT, None, 10, &OperatorScope::All).expect("tasks");
    assert!(
        tasks.is_empty(),
        "failed approve must roll back task insert"
    );
}

#[test]
fn edit_updates_staged_fields_and_validates() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let draft_id = staged_draft(conn);
    let ctx = |key: &'static str, rev: Option<u64>| DraftActionContext {
        client_id: CLIENT,
        actor_id: "op_test",
        scope: &OperatorScope::All,
        expected_revision: rev,
        idempotency_key: key,
        now_ms: 5_000,
    };

    let outcome = store::update_draft(
        conn,
        ctx("e1", Some(1)),
        &draft_id,
        "Call Dana back instead",
        Some("2026-06-20"),
        "She prefers a call.",
    )
    .expect("edit");
    assert!(matches!(
        outcome,
        MutationOutcome::Applied { revision: 2, .. }
    ));
    let drafts = store::list_drafts(
        persistence.connection_ref(),
        CLIENT,
        None,
        10,
        &OperatorScope::All,
    )
    .expect("list");
    let edited = &drafts[0].draft;
    assert_eq!(edited.title, "Call Dana back instead");
    assert_eq!(edited.due_date.as_deref(), Some("2026-06-20"));
    assert_eq!(edited.context, "She prefers a call.");
    // Provenance untouched — it documents the model's extraction.
    assert!(!edited.provenance.is_empty());

    let conn = persistence.connection();
    let err = store::update_draft(
        conn,
        ctx("e2", Some(2)),
        &draft_id,
        "t",
        Some("next Monday"),
        "",
    )
    .expect_err("bad date");
    assert!(err.to_string().contains("follow_up_draft_due_date_invalid"));
    let err = store::update_draft(conn, ctx("e3", Some(2)), &draft_id, "   ", None, "")
        .expect_err("empty title");
    assert!(err.to_string().contains("follow_up_draft_title_required"));

    // Only STAGED drafts are editable.
    store::reject_draft(conn, ctx("r1", Some(2)), &draft_id).expect("reject");
    let err = store::update_draft(conn, ctx("e4", None), &draft_id, "late edit", None, "")
        .expect_err("rejected draft");
    assert!(err.to_string().contains("follow_up_draft_not_staged"));
}

#[test]
fn reject_frees_the_item_for_reproduce() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let draft_id = staged_draft(conn);

    store::reject_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "op_test",
            scope: &OperatorScope::All,
            expected_revision: None,
            idempotency_key: "reject_1",
            now_ms: 5_000,
        },
        &draft_id,
    )
    .expect("reject");

    assert!(store::active_draft_for_item(conn, CLIENT, "wi_email_m2")
        .expect("query")
        .is_none());

    let item = accepted_item();
    let fill = service::parse_task_fill_response(&valid_fill_response()).expect("fill");
    let second = service::draft_from_fill(&item, &fill, 2, "test-model", 6_000);
    store::insert_draft(conn, CLIENT, "op_test", &second, "produce_2")
        .expect("re-produce after reject");
}

#[test]
fn drafts_and_tasks_are_filtered_by_source_user() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let jordan_draft_id = staged_draft_for_user(conn, "user_jordan", "produce_jordan");
    let casey_draft_id = staged_draft_for_user(conn, "user_casey", "produce_casey");
    let jordan_scope = OperatorScope::User("user_jordan".to_string());

    let jordan_drafts =
        store::list_drafts(conn, CLIENT, None, 10, &jordan_scope).expect("list jordan");
    assert_eq!(jordan_drafts.len(), 1);
    assert_eq!(jordan_drafts[0].draft.draft_id, jordan_draft_id);
    assert!(
        store::get_draft(conn, CLIENT, &casey_draft_id, &jordan_scope)
            .expect("get casey as jordan")
            .is_none()
    );

    let jordan_draft = store::get_draft(conn, CLIENT, &jordan_draft_id, &jordan_scope)
        .expect("get jordan")
        .expect("jordan draft");
    let jordan_task = service::task_from_draft(&jordan_draft.draft, 5_000);
    store::approve_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "user_jordan",
            scope: &jordan_scope,
            expected_revision: Some(jordan_draft.revision),
            idempotency_key: "approve_jordan",
            now_ms: 5_000,
        },
        &jordan_draft_id,
        &jordan_task,
    )
    .expect("approve jordan");

    let jordan_tasks =
        store::list_tasks(conn, CLIENT, Some(TaskStatus::Open), 10, &jordan_scope).expect("tasks");
    assert_eq!(jordan_tasks.len(), 1);
    assert_eq!(jordan_tasks[0].task.task_id, jordan_task.task_id);
    assert_eq!(
        jordan_tasks[0].task.source_user_id.as_deref(),
        Some("user_jordan")
    );
}

#[test]
fn cross_user_task_and_draft_mutations_are_refused() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let casey_draft_id = staged_draft_for_user(conn, "user_casey", "produce_casey");
    let casey_scope = OperatorScope::User("user_casey".to_string());
    let jordan_scope = OperatorScope::User("user_jordan".to_string());
    let casey_draft = store::get_draft(conn, CLIENT, &casey_draft_id, &casey_scope)
        .expect("get casey")
        .expect("casey draft");
    let task = service::task_from_draft(&casey_draft.draft, 5_000);

    let err = store::approve_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "user_jordan",
            scope: &jordan_scope,
            expected_revision: Some(casey_draft.revision),
            idempotency_key: "approve_cross",
            now_ms: 5_000,
        },
        &casey_draft_id,
        &task,
    )
    .expect_err("cross-user approve");
    assert!(err.to_string().contains("scope_forbidden"));

    store::approve_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "user_casey",
            scope: &casey_scope,
            expected_revision: Some(casey_draft.revision),
            idempotency_key: "approve_casey",
            now_ms: 5_000,
        },
        &casey_draft_id,
        &task,
    )
    .expect("approve casey");

    let err = store::apply_task_action(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "user_jordan",
            scope: &jordan_scope,
            expected_revision: Some(1),
            idempotency_key: "complete_cross",
            now_ms: 6_000,
        },
        &task.task_id,
        TaskAction::Complete,
    )
    .expect_err("cross-user complete");
    assert!(err.to_string().contains("scope_forbidden"));
}

#[test]
fn task_complete_and_reopen_lifecycle() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let draft_id = staged_draft(conn);
    let draft = store::get_draft(conn, CLIENT, &draft_id, &OperatorScope::All)
        .expect("get")
        .expect("exists");
    let task = service::task_from_draft(&draft.draft, 5_000);
    store::approve_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "op_test",
            scope: &OperatorScope::All,
            expected_revision: None,
            idempotency_key: "approve_1",
            now_ms: 5_000,
        },
        &draft_id,
        &task,
    )
    .expect("approve");

    store::apply_task_action(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "op_test",
            scope: &OperatorScope::All,
            expected_revision: None,
            idempotency_key: "complete_1",
            now_ms: 6_000,
        },
        &task.task_id,
        TaskAction::Complete,
    )
    .expect("complete");
    let done = store::list_tasks(
        conn,
        CLIENT,
        Some(TaskStatus::Done),
        10,
        &OperatorScope::All,
    )
    .expect("tasks");
    assert_eq!(done.len(), 1);

    store::apply_task_action(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "op_test",
            scope: &OperatorScope::All,
            expected_revision: None,
            idempotency_key: "reopen_1",
            now_ms: 7_000,
        },
        &task.task_id,
        TaskAction::Reopen,
    )
    .expect("reopen");
    let open = store::list_tasks(
        conn,
        CLIENT,
        Some(TaskStatus::Open),
        10,
        &OperatorScope::All,
    )
    .expect("tasks");
    assert_eq!(open.len(), 1);

    let missing = store::apply_task_action(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "op_test",
            scope: &OperatorScope::All,
            expected_revision: None,
            idempotency_key: "missing_1",
            now_ms: 8_000,
        },
        "task_nope",
        TaskAction::Complete,
    );
    assert!(missing.is_err());
}

#[test]
fn open_tasks_sort_by_due_date_with_undated_last() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();

    for (suffix, due) in [
        ("a", Some("2026-06-20")),
        ("b", None),
        ("c", Some("2026-06-12")),
    ] {
        let mut item = accepted_item();
        item.item_id = format!("wi_email_{suffix}");
        item.source_ref = suffix.to_string();
        let mut fill = service::parse_task_fill_response(&valid_fill_response()).expect("fill");
        fill.due_date = due.map(str::to_string);
        let draft = service::draft_from_fill(&item, &fill, 1, "test-model", 2_000);
        store::insert_draft(
            conn,
            CLIENT,
            "op_test",
            &draft,
            &format!("produce_{suffix}"),
        )
        .expect("stage");
        let task = service::task_from_draft(&draft, 3_000);
        store::approve_draft(
            conn,
            DraftActionContext {
                client_id: CLIENT,
                actor_id: "op_test",
                scope: &OperatorScope::All,
                expected_revision: None,
                idempotency_key: &format!("approve_{suffix}"),
                now_ms: 3_000,
            },
            &draft.draft_id,
            &task,
        )
        .expect("approve");
    }

    let tasks = store::list_tasks(
        conn,
        CLIENT,
        Some(TaskStatus::Open),
        10,
        &OperatorScope::All,
    )
    .expect("tasks");
    let due_dates: Vec<Option<&str>> = tasks.iter().map(|t| t.task.due_date.as_deref()).collect();
    assert_eq!(
        due_dates,
        vec![Some("2026-06-12"), Some("2026-06-20"), None],
        "due-dated first (ascending), undated last"
    );
}

/// Watchdog escalation, ported from agent_monitor's customer_follow_up tests:
/// due/overdue/upcoming lanes, missed→escalated→critical thresholds.
mod watchdog {
    use bos_contracts::follow_up_tasks::{TaskDueLane, TaskEscalationLevel};

    use crate::slices::follow_up_tasks::service::{classify_task_due, WatchdogPolicy};

    const TODAY: &str = "2026-04-29";

    #[test]
    fn classifies_due_today_upcoming_overdue_and_undated() {
        let policy = WatchdogPolicy::default();

        let due = classify_task_due(Some("2026-04-29"), TODAY, &policy);
        assert_eq!(due.lane, TaskDueLane::DueToday);
        assert_eq!(due.level, TaskEscalationLevel::None);

        let upcoming = classify_task_due(Some("2026-05-05"), TODAY, &policy);
        assert_eq!(upcoming.lane, TaskDueLane::Upcoming);
        assert_eq!(upcoming.days_until_due, 6);

        let overdue = classify_task_due(Some("2026-04-27"), TODAY, &policy);
        assert_eq!(overdue.lane, TaskDueLane::Overdue);
        assert_eq!(overdue.days_overdue, 2);

        let undated = classify_task_due(None, TODAY, &policy);
        assert_eq!(undated.lane, TaskDueLane::NoDueDate);
        let invalid = classify_task_due(Some("nope"), TODAY, &policy);
        assert_eq!(invalid.lane, TaskDueLane::NoDueDate);
    }

    #[test]
    fn escalation_thresholds_step_missed_escalated_critical() {
        // Wider thresholds so all three levels are reachable.
        let policy = WatchdogPolicy {
            escalation_after_days: 3,
            critical_after_days: 7,
        };

        let missed = classify_task_due(Some("2026-04-28"), TODAY, &policy);
        assert_eq!(missed.level, TaskEscalationLevel::Missed);
        assert_eq!(
            missed.reason.as_deref(),
            Some("missed follow-up by 1 day(s)")
        );

        let escalated = classify_task_due(Some("2026-04-26"), TODAY, &policy);
        assert_eq!(escalated.level, TaskEscalationLevel::Escalated);
        assert!(escalated
            .reason
            .as_deref()
            .unwrap()
            .contains("escalation threshold reached"));

        let critical = classify_task_due(Some("2026-04-22"), TODAY, &policy);
        assert_eq!(critical.level, TaskEscalationLevel::Critical);
        assert_eq!(critical.days_overdue, 7);
        assert!(critical
            .reason
            .as_deref()
            .unwrap()
            .contains("critical escalation threshold reached"));
    }

    #[test]
    fn default_policy_escalates_after_one_day_and_criticals_after_seven() {
        let policy = WatchdogPolicy::default();
        assert_eq!(
            classify_task_due(Some("2026-04-28"), TODAY, &policy).level,
            TaskEscalationLevel::Escalated
        );
        assert_eq!(
            classify_task_due(Some("2026-04-22"), TODAY, &policy).level,
            TaskEscalationLevel::Critical
        );
    }

    #[test]
    fn day_math_crosses_month_and_leap_boundaries() {
        let policy = WatchdogPolicy::default();
        // 2028 is a leap year: Feb 29 exists; Mar 1 is 1 day after.
        let leap = classify_task_due(Some("2028-02-29"), "2028-03-01", &policy);
        assert_eq!(leap.lane, TaskDueLane::Overdue);
        assert_eq!(leap.days_overdue, 1);

        let across_year = classify_task_due(Some("2027-01-02"), "2026-12-30", &policy);
        assert_eq!(across_year.days_until_due, 3);
    }
}
