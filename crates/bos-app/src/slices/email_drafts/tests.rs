//! Slice tests: reply-fill parsing, grounded recipient/subject/thread, the
//! stage → approve → outbox lifecycle, and gated dry-run delivery. No live
//! LLM or network. Rewrite validation and replay-before-model-spend are covered
//! through the HTTP seam; executing a fresh rewrite still awaits an injectable
//! LLM stub in the test harness.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use bos_contracts::email_drafts::{
    EmailDraftFollowUpRequest, EmailDraftStatus, EmailOutboundFollowUpStatus,
    GmailThreadFollowUpState,
};
use bos_contracts::email_triage::InboundMessageRecord;
use bos_contracts::operator_users::OperatorUser;
use bos_contracts::work_queue::{WorkItem, WorkItemStatus};
use bos_integrations::gmail_draft_write::GmailDraftCreateOutboxPayload;
use bos_integrations::gmail_inbox_read::GmailFullMessage;
use bos_integrations::GoogleOAuthConfig;
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

use super::service;
use super::store::{self, DraftActionContext};
use crate::http::{build_router, test_support::test_state_configured, OperatorScope};
use crate::outbox::{self, AttemptOutcome};
use crate::persistence::Persistence;
use crate::store_core::{MutationOutcome, StoreError};

const CLIENT: &str = "test-client";

#[test]
fn manual_email_is_typed_unthreaded_and_allows_ai_first_empty_body() {
    let item = accepted_item();
    let fields = store::normalize_editable_fields(
        "Dana <DANA@example.test>",
        &["alex@example.test".to_string()],
        "Storefront quote",
        "",
        true,
    )
    .expect("manual fields");
    let draft = service::manual_draft(&item, fields, 1, 2_000);
    assert_eq!(draft.to_addr, "Dana <dana@example.test>");
    assert_eq!(draft.cc_addrs, vec!["alex@example.test"]);
    assert_eq!(draft.subject, "Storefront quote");
    assert!(draft.body_text.is_empty());
    assert_eq!(draft.model, "manual");
    assert_eq!(draft.thread_id, None);

    let err = store::normalize_editable_fields(
        &draft.to_addr,
        &draft.cc_addrs,
        &draft.subject,
        &draft.body_text,
        false,
    )
    .expect_err("approval/edit validation requires body");
    assert!(matches!(err, StoreError::Domain(code) if code == "email_draft_body_required"));
}

#[tokio::test]
async fn manual_email_route_stages_the_typed_owner_draft_and_receipt() {
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
            Request::post("/api/email-drafts/manual")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "item_id": "wi_email_m4",
                        "to_addr": "dana@example.test",
                        "cc_addrs": ["ops@example.test"],
                        "subject": "Storefront quote",
                        "body_text": "Hi Dana — I will review the measurements.",
                        "idempotency_key": "manual_email_route",
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
    let body: bos_contracts::email_drafts::EmailDraftProduceResponse =
        serde_json::from_slice(&bytes).expect("typed response");
    assert_eq!(body.draft.draft.subject, "Storefront quote");
    assert_eq!(body.draft.draft.model, "manual");

    let persistence = state.persistence.lock();
    let receipt_count: i64 = persistence
        .connection_ref()
        .query_row(
            "SELECT COUNT(*) FROM receipts WHERE client_id = ?1 AND entity_kind = ?2 AND idempotency_key = ?3 AND outcome = 'applied'",
            rusqlite::params![CLIENT, store::DRAFT_ENTITY_KIND, "manual_email_route"],
            |row| row.get(0),
        )
        .expect("receipt count");
    assert_eq!(receipt_count, 1);
}

#[tokio::test]
async fn draft_update_route_applies_exact_revision() {
    let state = test_state_configured(None, &[]);
    let (draft_id, revision) = {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        let draft_id = staged_draft(conn);
        let revision = store::get_draft(conn, CLIENT, &draft_id, &OperatorScope::All)
            .expect("get draft")
            .expect("draft")
            .revision;
        (draft_id, revision)
    };

    let applied = post_json(
        build_router(state.clone()),
        &format!("/api/email-drafts/{draft_id}/update"),
        json!({
            "to_addr": "Dana <dana@example.test>, ops@example.test",
            "cc_addrs": ["finance@example.test"],
            "subject": "Updated subject",
            "body_text": "Updated body",
            "expected_revision": revision,
            "idempotency_key": "route_edit_applied",
            "actor_id": null
        }),
    )
    .await;
    assert_eq!(applied.status(), StatusCode::OK);

    let listed: bos_contracts::email_drafts::EmailDraftsResponse =
        response_body(get(build_router(state), "/api/email-drafts?item_id=wi_email_m4").await)
            .await;
    let stored = listed.drafts.into_iter().next().expect("updated draft");
    assert_eq!(stored.revision, revision + 1);
    assert_eq!(stored.draft.subject, "Updated subject");
    assert_eq!(stored.draft.body_text, "Updated body");
}

#[tokio::test]
async fn draft_rewrite_route_replays_without_model_spend() {
    let state = test_state_configured(None, &[]);
    let (draft_id, initial_revision) = {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        let draft_id = staged_draft(conn);
        let initial_revision = store::get_draft(conn, CLIENT, &draft_id, &OperatorScope::All)
            .expect("get draft")
            .expect("draft")
            .revision;
        (draft_id, initial_revision)
    };

    let rewritten_revision = {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        let outcome = store::apply_ai_rewrite(
            conn,
            DraftActionContext {
                client_id: CLIENT,
                actor_id: "op_test",
                scope: &OperatorScope::All,
                expected_revision: Some(initial_revision),
                idempotency_key: "route_rewrite_replay",
                now_ms: 3_500,
            },
            &draft_id,
            "Hi Dana — could you share the storefront measurements?",
            &[],
            "test-rewrite-model",
            "high",
        )
        .expect("seed applied rewrite");
        let MutationOutcome::Applied { revision, .. } = outcome else {
            panic!("expected applied rewrite")
        };
        revision
    };

    let replay = post_json(
        build_router(state.clone()),
        &format!("/api/email-drafts/{draft_id}/rewrite"),
        json!({
            "instructions": "These instructions must not trigger model spend",
            "expected_revision": initial_revision,
            "idempotency_key": "route_rewrite_replay",
            "actor_id": null
        }),
    )
    .await;
    assert_eq!(replay.status(), StatusCode::OK);
    let body: bos_contracts::email_drafts::EmailDraftRewriteResponse = response_body(replay).await;
    assert_eq!(body.draft.revision, rewritten_revision);
    assert_eq!(
        body.draft.draft.body_text,
        "Hi Dana — could you share the storefront measurements?"
    );
}

#[tokio::test]
async fn follow_ups_list_route_returns_open_workflows_and_rejects_bad_status() {
    let state = test_state_configured(None, &[]);
    let open_id = {
        let mut persistence = state.persistence.lock();
        approved_follow_up(
            persistence.connection(),
            "wi_follow_up_open",
            None,
            "2020-01-01",
        )
    };

    let open: bos_contracts::email_drafts::EmailOutboundFollowUpsResponse =
        response_body(get(build_router(state.clone()), "/api/email-drafts/follow-ups").await).await;
    assert_eq!(
        open.follow_ups
            .iter()
            .map(|summary| summary.follow_up_id.as_str())
            .collect::<Vec<_>>(),
        vec![open_id.as_str()]
    );

    let invalid = get(
        build_router(state),
        "/api/email-drafts/follow-ups?status=pending",
    )
    .await;
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response_error(invalid).await,
        "email_follow_up_status_invalid"
    );
}

#[tokio::test]
async fn follow_up_check_route_marks_unthreaded_workflow_not_applicable_without_network() {
    let state = test_state_configured(None, &[]);
    let follow_up_id = {
        let mut persistence = state.persistence.lock();
        approved_follow_up(
            persistence.connection(),
            "wi_follow_up_unthreaded",
            None,
            "2020-01-01",
        )
    };

    let response = post_json(
        build_router(state.clone()),
        &format!("/api/email-drafts/follow-ups/{follow_up_id}/check"),
        json!({
            "idempotency_key": "route_follow_up_check",
            "actor_id": null
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: bos_contracts::email_drafts::EmailOutboundFollowUpCheckResponse =
        response_body(response).await;
    assert_eq!(
        body.follow_up.thread_state,
        GmailThreadFollowUpState::NotApplicable
    );
    assert_eq!(body.follow_up.status, EmailOutboundFollowUpStatus::Active);
    assert!(body.follow_up.last_check_error.is_none());
}

#[tokio::test]
async fn follow_up_draft_route_creates_one_due_waiting_reply_item_and_replays() {
    let state = test_state_configured(None, &[]);
    let follow_up_id = {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        let follow_up_id = approved_follow_up(
            conn,
            "wi_follow_up_due",
            Some("thread-follow-up-due"),
            "2020-01-01",
        );
        store::apply_thread_reconciliation(
            conn,
            DraftActionContext {
                client_id: CLIENT,
                actor_id: "op_test",
                scope: &OperatorScope::All,
                expected_revision: None,
                idempotency_key: "waiting_reply_for_route_draft",
                now_ms: 6_000,
            },
            &follow_up_id,
            store::ThreadReconciliation {
                thread_state: GmailThreadFollowUpState::SentWaitingReply,
                status: EmailOutboundFollowUpStatus::Active,
                sent_message_id: Some("sent-route-draft".to_string()),
                sent_at_ms: Some(5_100),
                reply_message_id: None,
                reply_at_ms: None,
                resolution_reason: None,
                last_check_error: None,
            },
        )
        .expect("mark waiting reply");
        follow_up_id
    };
    let path = format!("/api/email-drafts/follow-ups/{follow_up_id}/draft");

    let created = post_json(
        build_router(state.clone()),
        &path,
        json!({
            "idempotency_key": "route_follow_up_draft",
            "actor_id": null
        }),
    )
    .await;
    assert_eq!(created.status(), StatusCode::OK);
    let created: bos_contracts::email_drafts::EmailOutboundFollowUpDraftResponse =
        response_body(created).await;
    assert_eq!(
        created.item.item.source_kind,
        store::SOURCE_KIND_EMAIL_FOLLOW_UP
    );
    assert_eq!(created.item.item.source_ref, follow_up_id);
    assert_eq!(created.item.item.status, WorkItemStatus::Accepted);
    assert_eq!(created.item.item.packet_kinds, vec![service::PACKET_KIND]);

    let replayed = post_json(
        build_router(state.clone()),
        &path,
        json!({
            "idempotency_key": "route_follow_up_draft_second_request",
            "actor_id": null
        }),
    )
    .await;
    assert_eq!(replayed.status(), StatusCode::OK);
    let replayed: bos_contracts::email_drafts::EmailOutboundFollowUpDraftResponse =
        response_body(replayed).await;
    assert_eq!(replayed.item.item.item_id, created.item.item.item_id);
}

#[test]
fn rewrite_request_is_bounded_and_has_no_side_effect_authority() {
    let item = accepted_item();
    let fields = store::normalize_editable_fields(
        "dana@example.test",
        &[],
        "Storefront quote",
        "Rough draft",
        false,
    )
    .expect("fields");
    let draft = service::manual_draft(&item, fields, 1, 2_000);
    let request = service::build_rewrite_request(
        CLIENT,
        &draft,
        &source_message(),
        "Make this concise and warm",
        None,
        1,
    )
    .expect("request");
    assert_eq!(
        request.spec.task_class,
        bos_integrations::llm_typed_tasks::TypedLlmTaskClass::Rewrite
    );
    assert_eq!(request.spec.schema_ref, service::FILL_SCHEMA_REF);
    assert_eq!(
        request.spec.authority,
        bos_integrations::llm_typed_tasks::TypedLlmAuthority::no_side_effects()
    );
    assert!(request
        .input
        .text_blocks
        .iter()
        .any(|block| block.block_id == "current_draft"));
    assert!(request
        .input
        .text_blocks
        .iter()
        .any(|block| block.block_id == "source_context"));
}

#[test]
fn rewrite_provenance_keeps_only_literal_evidence_spans() {
    use bos_contracts::calendar_drafts::DraftFieldProvenance;
    use bos_integrations::llm_typed_tasks::TypedLlmTextBlock;

    let provenance = vec![
        DraftFieldProvenance {
            field: "body_text".to_string(),
            quote: "Storefront quote".to_string(),
        },
        DraftFieldProvenance {
            field: "body_text".to_string(),
            quote: "invented commitment".to_string(),
        },
        DraftFieldProvenance {
            field: "body_text".to_string(),
            quote: String::new(),
        },
    ];
    let evidence = vec![TypedLlmTextBlock {
        block_id: "source_context".to_string(),
        text: "Dana asked about the Storefront quote.".to_string(),
    }];

    assert_eq!(
        service::grounded_rewrite_provenance(&provenance, &evidence),
        vec![provenance[0].clone()]
    );
}

#[test]
fn ai_rewrite_updates_only_body_metadata_at_the_exact_revision() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let draft_id = staged_draft(conn);
    let before = store::get_draft(conn, CLIENT, &draft_id, &OperatorScope::All)
        .expect("get before")
        .expect("draft");
    let provenance = vec![bos_contracts::calendar_drafts::DraftFieldProvenance {
        field: "body_text".to_string(),
        quote: "Could you send me a quote".to_string(),
    }];
    let outcome = store::apply_ai_rewrite(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "op_test",
            scope: &OperatorScope::All,
            expected_revision: Some(before.revision),
            idempotency_key: "rewrite_exact_revision",
            now_ms: 3_500,
        },
        &draft_id,
        "Hi Dana — I can help with that quote.",
        &provenance,
        "test-rewrite-model",
        "high",
    )
    .expect("rewrite");
    assert!(matches!(
        outcome,
        MutationOutcome::Applied { revision, .. } if revision == before.revision + 1
    ));
    let after = store::get_draft(conn, CLIENT, &draft_id, &OperatorScope::All)
        .expect("get after")
        .expect("draft");
    assert_eq!(after.draft.to_addr, before.draft.to_addr);
    assert_eq!(after.draft.cc_addrs, before.draft.cc_addrs);
    assert_eq!(after.draft.subject, before.draft.subject);
    assert_eq!(
        after.draft.body_text,
        "Hi Dana — I can help with that quote."
    );
    assert_eq!(after.draft.model, "test-rewrite-model");

    let stale = store::apply_ai_rewrite(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "op_test",
            scope: &OperatorScope::All,
            expected_revision: Some(before.revision),
            idempotency_key: "rewrite_stale_revision",
            now_ms: 3_600,
        },
        &draft_id,
        "This stale rewrite must not win.",
        &provenance,
        "test-rewrite-model",
        "high",
    )
    .expect("conflict outcome");
    assert!(matches!(stale, MutationOutcome::RevisionConflict { .. }));
}

#[test]
fn reply_fill_request_includes_background_when_present() {
    use bos_integrations::llm_typed_tasks::TypedLlmTextBlock;
    let item = accepted_item();
    let message = source_message();

    let plain = service::build_reply_fill_request(
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

    let block = TypedLlmTextBlock {
        block_id: "background".to_string(),
        text: "Company: Example Company".to_string(),
    };
    let context = json!({ "background": serde_json::to_value(&block).unwrap() });
    let grounded = service::build_reply_fill_request(CLIENT, &item, &message, &context, 1);
    let backgrounds: Vec<_> = grounded
        .input
        .text_blocks
        .iter()
        .filter(|b| b.block_id == "background")
        .collect();
    assert_eq!(backgrounds.len(), 1);
    assert_eq!(backgrounds[0].text, "Company: Example Company");
}

#[test]
fn reply_grounding_includes_prior_conversation_and_records_evidence() {
    use crate::produce::ProduceFlavor;

    let state = test_state_configured(None, &[]);
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let item = accepted_item();
    crate::slices::work_queue::store::insert_item(conn, CLIENT, &item).expect("work item");
    let message = source_message();
    let mut prior = source_message();
    prior.source_key = "m_prior".to_string();
    prior.message_id = "m_prior".to_string();
    prior.internal_date_ms = Some(500);
    prior.from_addr = Some("Dana <dana@example.test>".to_string());
    prior.subject = Some("Earlier storefront details".to_string());
    prior.body_excerpt = "The storefront is about 900 square feet.".to_string();
    crate::slices::email_triage::store::record_inbound_message(conn, CLIENT, &prior)
        .expect("prior");
    let context = service::Produce
        .prepare_context(
            conn,
            CLIENT,
            &item,
            &message,
            &OperatorScope::All,
            "operator",
        )
        .expect("context");
    drop(persistence);
    let context = service::Produce.enrich_context_unlocked(crate::produce::EnrichContext {
        state: &state,
        item: &item,
        message: &message,
        scope: &OperatorScope::All,
        actor_id: "auto_produce_pump",
        actor_kind: bos_contracts::receipt::ActorKindDto::System,
        context,
        attempt: 1,
        now_ms: 1_000,
    });
    let request = service::build_reply_fill_request(CLIENT, &item, &message, &context, 1);
    let grounding = request
        .input
        .text_blocks
        .iter()
        .find(|block| block.block_id == "grounding")
        .expect("grounding block");
    assert!(grounding.text.contains("Prior cached conversations"));
    assert!(grounding.text.contains("900 square feet"));
    let persistence = state.persistence.lock();
    let evidence = crate::slices::grounding::grounding_evidence_for_item(
        persistence.connection_ref(),
        CLIENT,
        &item.item_id,
    )
    .expect("evidence");
    assert!(evidence
        .iter()
        .any(|row| row.tool_name == crate::slices::grounding::TOOL_PRIOR_CONVERSATION_LOOKUP));
    assert!(evidence.iter().any(|row| {
        row.tool_name == crate::slices::grounding::TOOL_PRIOR_CONVERSATION_LOOKUP
            && row.actor_kind == bos_contracts::receipt::ActorKindDto::System
    }));
}

fn accepted_item() -> WorkItem {
    WorkItem {
        item_id: "wi_email_m4".to_string(),
        source_kind: "email".to_string(),
        source_ref: "m4".to_string(),
        category_id: "inquiries".to_string(),
        title: "Quote request".to_string(),
        summary: "Dana wants a quote".to_string(),
        packet_kinds: vec!["email_draft_reply".to_string()],
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

fn source_message() -> InboundMessageRecord {
    InboundMessageRecord {
        source_key: "m4".to_string(),
        message_id: "m4".to_string(),
        thread_id: Some("thread-9".to_string()),
        internal_date_ms: Some(1_000),
        from_addr: Some("dana@example.test".to_string()),
        to_addr: Some("jordan@example.test".to_string()),
        subject: Some("storefront quote".to_string()),
        body_excerpt: "Could you send me a quote for repainting the storefront?".to_string(),
        body_full: String::new(),
        headers: vec![
            (
                "Message-ID".to_string(),
                "<source-message@example.test>".to_string(),
            ),
            (
                "References".to_string(),
                "<root-message@example.test> <prior-message@example.test>".to_string(),
            ),
        ],
        labels: Vec::new(),
        resolved_category: "inquiries".to_string(),
        matched_rule_id: None,
        ingested_at_ms: 1_000,
        ai_triage_status: None,
        ai_triage_rationale: None,
        attachments: Vec::new(),
        source_user_id: None,
    }
}

fn valid_fill_response() -> serde_json::Value {
    json!({
        "body_text": "Hi Dana — happy to quote that. Could you share the approximate square footage and current coating condition?\n\n— Jordan",
        "confidence": "high",
        "provenance": [
            {"field": "body_text", "quote": "send me a quote for repainting the storefront"},
            {"field": "subject", "quote": "dropped — not a fillable field"}
        ]
    })
}

fn gmail_message(id: &str, labels: &[&str], at: i64) -> GmailFullMessage {
    GmailFullMessage {
        message_id: id.to_string(),
        thread_id: Some("thread-9".to_string()),
        label_ids: labels.iter().map(|label| label.to_string()).collect(),
        internal_date_epoch_ms: Some(at),
        subject: Some("storefront quote".to_string()),
        from: Some("dana@example.test".to_string()),
        to: Some("jordan@example.test".to_string()),
        headers: Vec::new(),
        plain_text_body: String::new(),
        html_body: None,
        attachments: Vec::new(),
    }
}

fn staged_draft(conn: &mut rusqlite::Connection) -> String {
    staged_draft_with_source(conn, "wi_email_m4", None, "produce_1")
}

fn staged_draft_with_source(
    conn: &mut rusqlite::Connection,
    item_id: &str,
    source_user_id: Option<&str>,
    idempotency_key: &str,
) -> String {
    let item = WorkItem {
        item_id: item_id.to_string(),
        source_user_id: source_user_id.map(str::to_string),
        ..accepted_item()
    };
    let fill = service::parse_reply_fill_response(&valid_fill_response()).expect("fill");
    let draft = service::draft_from_fill(&item, &source_message(), &fill, 1, "test-model", 2_000)
        .expect("draft");
    store::insert_draft(conn, CLIENT, "op_test", &draft, idempotency_key).expect("stage");
    draft.draft_id
}

fn approved_follow_up(
    conn: &mut rusqlite::Connection,
    item_id: &str,
    thread_id: Option<&str>,
    due_date: &str,
) -> String {
    let item = WorkItem {
        item_id: item_id.to_string(),
        ..accepted_item()
    };
    let fill = service::parse_reply_fill_response(&valid_fill_response()).expect("fill");
    let mut message = source_message();
    message.thread_id = thread_id.map(str::to_string);
    let draft =
        service::draft_from_fill(&item, &message, &fill, 1, "test-model", 2_000).expect("draft");
    let stage_key = format!("stage_{item_id}");
    store::insert_draft(conn, CLIENT, "op_test", &draft, &stage_key).expect("stage draft");
    let stored = store::get_draft(conn, CLIENT, &draft.draft_id, &OperatorScope::All)
        .expect("get staged draft")
        .expect("staged draft");
    let job = service::build_approval_job(&stored.draft, "op_test", "op_test", 5_000)
        .expect("approval job");
    let plan = store::EmailFollowUpPlan::from_request(
        &stored.draft,
        &EmailDraftFollowUpRequest {
            enabled: true,
            due_date: Some(due_date.to_string()),
            title: format!("Follow up {item_id}"),
            context: "Check whether Dana replied.".to_string(),
            create_follow_up_draft: true,
        },
        5_000,
    )
    .expect("valid follow-up")
    .expect("enabled follow-up");
    let follow_up_id = plan.follow_up_id.clone();
    let approve_key = format!("approve_{item_id}");
    store::approve_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "op_test",
            scope: &OperatorScope::All,
            expected_revision: Some(stored.revision),
            idempotency_key: &approve_key,
            now_ms: 5_000,
        },
        &draft.draft_id,
        &job,
        Some(plan),
    )
    .expect("approve with follow-up");
    follow_up_id
}

fn personal_operator(user_id: &str) -> OperatorUser {
    OperatorUser {
        user_id: user_id.to_string(),
        display_name: user_id.to_string(),
        active: true,
        archived_at_ms: None,
        default_calendar_id: None,
        created_at_ms: 1_000,
        updated_at_ms: 1_000,
    }
}

fn create_operator(conn: &mut rusqlite::Connection, user_id: &str, token: &str) {
    crate::slices::operator_users::store::create_user(
        conn,
        CLIENT,
        "operator",
        &personal_operator(user_id),
        token,
        &format!("create_{user_id}"),
    )
    .expect("operator user");
}

fn store_gmail_credential(conn: &mut rusqlite::Connection, user_id: &str) {
    crate::slices::google_connector::store::store_credential(
        conn,
        CLIENT,
        user_id,
        crate::slices::google_connector::SERVICE_GMAIL,
        "refresh-token",
        &["https://www.googleapis.com/auth/gmail.compose".to_string()],
        3_000,
    )
    .expect("credential");
}

async fn response_error(response: axum::response::Response) -> String {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json body");
    body.get("error")
        .and_then(serde_json::Value::as_str)
        .expect("error code")
        .to_string()
}

async fn response_body<T: serde::de::DeserializeOwned>(response: axum::response::Response) -> T {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("typed response body")
}

async fn post_json(
    router: axum::Router,
    path: &str,
    body: serde_json::Value,
) -> axum::response::Response {
    router
        .oneshot(
            Request::post(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("request"),
        )
        .await
        .expect("response")
}

async fn get(router: axum::Router, path: &str) -> axum::response::Response {
    router
        .oneshot(Request::get(path).body(Body::empty()).expect("request"))
        .await
        .expect("response")
}

async fn approve_email_draft(
    router: axum::Router,
    draft_id: &str,
    actor_id: &str,
    key: &str,
) -> axum::response::Response {
    router
        .oneshot(
            Request::post(format!("/api/email-drafts/{draft_id}/action"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "action": "approve",
                        "expected_revision": null,
                        "idempotency_key": key,
                        "actor_id": actor_id
                    })
                    .to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("response")
}

#[test]
fn fill_parses_and_grounding_overrides_model() {
    let fill = service::parse_reply_fill_response(&valid_fill_response()).expect("fill");
    let fields: Vec<&str> = fill.provenance.iter().map(|p| p.field.as_str()).collect();
    assert_eq!(fields, vec!["body_text"], "only body provenance kept");

    let draft = service::draft_from_fill(
        &accepted_item(),
        &source_message(),
        &fill,
        1,
        "test-model",
        2_000,
    )
    .expect("draft");
    assert_eq!(draft.to_addr, "dana@example.test", "recipient grounded");
    assert!(
        draft.cc_addrs.is_empty(),
        "single source mailbox is excluded"
    );
    assert_eq!(draft.subject, "Re: storefront quote", "subject computed");
    assert_eq!(draft.thread_id.as_deref(), Some("thread-9"));
    assert_eq!(
        draft.reply_message_id.as_deref(),
        Some("<source-message@example.test>")
    );
    assert_eq!(
        draft.reference_message_ids,
        vec![
            "<root-message@example.test>".to_string(),
            "<prior-message@example.test>".to_string(),
            "<source-message@example.test>".to_string()
        ]
    );

    // No usable sender → produce fails cleanly instead of guessing.
    let mut no_sender = source_message();
    no_sender.from_addr = None;
    assert!(
        service::draft_from_fill(&accepted_item(), &no_sender, &fill, 1, "test-model", 2_000)
            .is_err()
    );

    let mut empty_body = valid_fill_response();
    empty_body["body_text"] = json!("  ");
    assert!(service::parse_reply_fill_response(&empty_body).is_err());
}

#[test]
fn reply_headers_extract_message_ids_from_rfcish_values() {
    let fill = service::parse_reply_fill_response(&valid_fill_response()).expect("fill");
    let mut message = source_message();
    message.headers = vec![
        (
            "Message-ID".to_string(),
            "(comment) <source-message@example.test>".to_string(),
        ),
        (
            "References".to_string(),
            "<root@example.test>, <prior@example.test> invalid <bad id@example.test>".to_string(),
        ),
        (
            "In-Reply-To".to_string(),
            "<prior@example.test> <reply-parent@example.test>".to_string(),
        ),
        (
            "References".to_string(),
            "unterminated <ignored@example.test".to_string(),
        ),
    ];

    let draft = service::draft_from_fill(&accepted_item(), &message, &fill, 1, "test-model", 2_000)
        .expect("draft");

    assert_eq!(
        draft.reply_message_id.as_deref(),
        Some("<source-message@example.test>")
    );
    assert_eq!(
        draft.reference_message_ids,
        vec![
            "<root@example.test>".to_string(),
            "<prior@example.test>".to_string(),
            "<reply-parent@example.test>".to_string(),
            "<source-message@example.test>".to_string(),
        ]
    );
}

#[test]
fn draft_store_roundtrips_reply_headers() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let draft_id = staged_draft(conn);
    let stored = store::get_draft(conn, CLIENT, &draft_id, &OperatorScope::All)
        .expect("get")
        .expect("draft");

    assert_eq!(
        stored.draft.reply_message_id.as_deref(),
        Some("<source-message@example.test>")
    );
    assert_eq!(
        stored.draft.reference_message_ids,
        vec![
            "<root-message@example.test>".to_string(),
            "<prior-message@example.test>".to_string(),
            "<source-message@example.test>".to_string(),
        ]
    );
}

#[test]
fn reply_all_cc_is_grounded_from_original_to_and_cc() {
    let fill = service::parse_reply_fill_response(&valid_fill_response()).expect("fill");
    let mut item = accepted_item();
    item.source_user_id = Some("jordan@example.test".to_string());
    let mut message = source_message();
    message.from_addr = Some("Dana <dana@example.test>".to_string());
    message.to_addr = Some("Jordan <jordan@example.test>, Ops <ops@example.test>".to_string());
    message.headers = vec![
        (
            "Cc".to_string(),
            "Alex <alex@example.test>, Dana <dana@example.test>".to_string(),
        ),
        (
            "Delivered-To".to_string(),
            "jordan@example.test".to_string(),
        ),
    ];

    let draft =
        service::draft_from_fill(&item, &message, &fill, 1, "test-model", 2_000).expect("draft");

    assert_eq!(draft.to_addr, "Dana <dana@example.test>");
    assert_eq!(
        draft.cc_addrs,
        vec![
            "ops@example.test".to_string(),
            "alex@example.test".to_string()
        ]
    );
}

#[test]
fn reply_all_uses_headers_preserved_by_ingest() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    crate::slices::email_triage::worker::ingest_messages(
        persistence.connection(),
        CLIENT,
        Some("user_jordan"),
        &[GmailFullMessage {
            message_id: "reply-all-stored".to_string(),
            thread_id: Some("thread-99".to_string()),
            label_ids: vec![],
            internal_date_epoch_ms: Some(1_500),
            subject: Some("storefront quote".to_string()),
            from: Some("Dana <dana@example.test>".to_string()),
            to: Some("Jordan <jordan@example.test>, Ops <ops@example.test>".to_string()),
            headers: vec![
                (
                    "Cc".to_string(),
                    "Alex <alex@example.test>, Dana <dana@example.test>".to_string(),
                ),
                (
                    "Delivered-To".to_string(),
                    "jordan@example.test".to_string(),
                ),
                (
                    "Message-ID".to_string(),
                    "<reply-all-stored@example.test>".to_string(),
                ),
                (
                    "References".to_string(),
                    "<root@example.test> <prior@example.test>".to_string(),
                ),
            ],
            plain_text_body: "Could you send me a quote?".to_string(),
            html_body: None,
            attachments: Vec::new(),
        }],
        2_000,
    )
    .expect("ingest");
    let source_key =
        crate::slices::email_triage::store::source_key_for(Some("user_jordan"), "reply-all-stored");
    let stored = crate::slices::email_triage::store::inbound_by_source_keys(
        persistence.connection_ref(),
        CLIENT,
        &[source_key],
        &OperatorScope::All,
    )
    .expect("stored");
    let message = stored.into_iter().next().expect("stored message");
    let fill = service::parse_reply_fill_response(&valid_fill_response()).expect("fill");
    let mut item = accepted_item();
    item.source_user_id = Some("user_jordan".to_string());

    let draft =
        service::draft_from_fill(&item, &message, &fill, 1, "test-model", 2_000).expect("draft");

    assert_eq!(
        draft.cc_addrs,
        vec![
            "ops@example.test".to_string(),
            "alex@example.test".to_string()
        ]
    );
    assert_eq!(
        draft.reply_message_id.as_deref(),
        Some("<reply-all-stored@example.test>")
    );
    assert_eq!(
        draft.reference_message_ids,
        vec![
            "<root@example.test>".to_string(),
            "<prior@example.test>".to_string(),
            "<reply-all-stored@example.test>".to_string()
        ]
    );
}

#[test]
fn reply_subject_does_not_stack_re_prefixes() {
    assert_eq!(service::reply_subject(Some("hello")), "Re: hello");
    assert_eq!(service::reply_subject(Some("Re: hello")), "Re: hello");
    assert_eq!(service::reply_subject(Some("RE: hello")), "RE: hello");
    assert_eq!(service::reply_subject(None), "Re: (no subject)");
}

#[test]
fn draft_from_fill_inherits_item_source_user() {
    let mut item = accepted_item();
    item.source_user_id = Some("user_jordan".to_string());
    let fill = service::parse_reply_fill_response(&valid_fill_response()).expect("fill");

    let draft = service::draft_from_fill(&item, &source_message(), &fill, 1, "test-model", 2_000)
        .expect("draft");

    assert_eq!(draft.source_user_id.as_deref(), Some("user_jordan"));
}

#[test]
fn list_and_get_drafts_apply_operator_scope() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let null_id = staged_draft_with_source(conn, "wi_null", None, "produce_null");
    let u1_id = staged_draft_with_source(conn, "wi_u1", Some("u1"), "produce_u1");
    let u2_id = staged_draft_with_source(conn, "wi_u2", Some("u2"), "produce_u2");

    let all = store::list_drafts(conn, CLIENT, None, 10, &OperatorScope::All).expect("list all");
    let all_ids: std::collections::HashSet<_> = all
        .iter()
        .map(|entry| entry.draft.draft_id.as_str())
        .collect();
    assert!(all_ids.contains(null_id.as_str()));
    assert!(all_ids.contains(u1_id.as_str()));
    assert!(all_ids.contains(u2_id.as_str()));

    let u1_scope = OperatorScope::User("u1".to_string());
    let u1 = store::list_drafts(conn, CLIENT, None, 10, &u1_scope).expect("list u1");
    assert_eq!(u1.len(), 1);
    assert_eq!(u1[0].draft.draft_id, u1_id);

    assert!(store::get_draft(conn, CLIENT, &u1_id, &u1_scope)
        .expect("get own")
        .is_some());
    assert!(store::get_draft(conn, CLIENT, &u2_id, &u1_scope)
        .expect("get other")
        .is_none());
    assert!(store::get_draft(conn, CLIENT, &null_id, &u1_scope)
        .expect("get null")
        .is_none());
}

#[test]
fn draft_mutations_reject_cross_scope_access() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let update_id = staged_draft_with_source(conn, "wi_update_u2", Some("u2"), "produce_update_u2");
    let reject_id = staged_draft_with_source(conn, "wi_reject_u2", Some("u2"), "produce_reject_u2");
    let approve_id =
        staged_draft_with_source(conn, "wi_approve_u2", Some("u2"), "produce_approve_u2");
    let u1_scope = OperatorScope::User("u1".to_string());
    let cross_scope = |key: &'static str| DraftActionContext {
        client_id: CLIENT,
        actor_id: "u1",
        scope: &u1_scope,
        expected_revision: None,
        idempotency_key: key,
        now_ms: 5_000,
    };

    let err = store::update_draft(
        conn,
        cross_scope("update_cross"),
        &update_id,
        "dana@example.test",
        &[],
        "Blocked subject",
        "Blocked edit",
    )
    .expect_err("cross-scope update rejected");
    assert!(matches!(err, StoreError::Domain(code) if code == "scope_forbidden"));

    let err = store::reject_draft(conn, cross_scope("reject_cross"), &reject_id)
        .expect_err("cross-scope reject rejected");
    assert!(matches!(err, StoreError::Domain(code) if code == "scope_forbidden"));

    let draft = store::get_draft(conn, CLIENT, &approve_id, &OperatorScope::All)
        .expect("get")
        .expect("exists");
    let job = service::build_approval_job(&draft.draft, "u1", "u2", 5_000).expect("job");
    let err = store::approve_draft(conn, cross_scope("approve_cross"), &approve_id, &job, None)
        .expect_err("cross-scope approve rejected");
    assert!(matches!(err, StoreError::Domain(code) if code == "scope_forbidden"));
}

#[test]
fn draft_update_edits_recipient_and_body() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let draft_id = staged_draft(conn);

    store::update_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "op_test",
            scope: &OperatorScope::All,
            expected_revision: None,
            idempotency_key: "edit_to_body",
            now_ms: 3_000,
        },
        &draft_id,
        "Dana <dana@example.test>, ops@example.test",
        &["finance@example.test".to_string()],
        "Updated subject",
        "Updated body",
    )
    .expect("edit");

    let draft = store::get_draft(conn, CLIENT, &draft_id, &OperatorScope::All)
        .expect("get")
        .expect("draft");
    assert_eq!(
        draft.draft.to_addr,
        "Dana <dana@example.test>, ops@example.test"
    );
    assert_eq!(draft.draft.body_text, "Updated body");
    assert_eq!(draft.draft.cc_addrs, vec!["finance@example.test"]);
    assert_eq!(draft.draft.subject, "Updated subject");

    let receipt_after: String = conn
        .query_row(
            "SELECT after_json FROM receipts WHERE client_id = ?1 AND entity_kind = 'email_reply_draft' AND entity_id = ?2 AND change_kind = 'edit'",
            rusqlite::params![CLIENT, draft_id],
            |row| row.get(0),
        )
        .expect("receipt");
    assert!(receipt_after.contains("ops@example.test"));
}

#[test]
fn draft_update_requires_recipient() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let draft_id = staged_draft(conn);

    let err = store::update_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "op_test",
            scope: &OperatorScope::All,
            expected_revision: None,
            idempotency_key: "edit_missing_to",
            now_ms: 3_000,
        },
        &draft_id,
        "not-an-address",
        &[],
        "Updated subject",
        "Updated body",
    )
    .expect_err("invalid recipient rejected");
    assert!(matches!(err, StoreError::Domain(code) if code == "email_draft_to_addr_invalid"));
}

#[test]
fn draft_update_rejects_invalid_recipient_list_entry() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let draft_id = staged_draft(conn);

    let err = store::update_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "op_test",
            scope: &OperatorScope::All,
            expected_revision: None,
            idempotency_key: "edit_bad_recipient_list",
            now_ms: 3_000,
        },
        &draft_id,
        "dana@example.test, not-an-address",
        &[],
        "Updated subject",
        "Updated body",
    )
    .expect_err("invalid recipient list rejected");
    assert!(matches!(err, StoreError::Domain(code) if code == "email_draft_to_addr_invalid"));
}

#[test]
fn draft_update_rejects_recipient_control_chars() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let draft_id = staged_draft(conn);

    let err = store::update_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "op_test",
            scope: &OperatorScope::All,
            expected_revision: None,
            idempotency_key: "edit_recipient_control",
            now_ms: 3_000,
        },
        &draft_id,
        "dana@example.test\r\nBcc: attacker@example.test",
        &[],
        "Updated subject",
        "Updated body",
    )
    .expect_err("recipient header injection rejected");
    assert!(matches!(err, StoreError::Domain(code) if code == "email_draft_to_addr_invalid"));
}

#[test]
fn approve_enqueues_gmail_draft_job_and_dry_run_delivers() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let draft_id = staged_draft(conn);
    let draft = store::get_draft(conn, CLIENT, &draft_id, &OperatorScope::All)
        .expect("get")
        .expect("exists");
    let job =
        service::build_approval_job(&draft.draft, "op_test", "user_jordan", 5_000).expect("job");
    assert_eq!(job.provider, "gmail");
    assert_eq!(job.capability, "create_draft");
    let payload: GmailDraftCreateOutboxPayload =
        serde_json::from_str(&job.payload_json).expect("payload");
    assert_eq!(payload.to, "dana@example.test");
    assert!(payload.cc.is_empty());
    assert_eq!(payload.thread_id.as_deref(), Some("thread-9"));
    assert_eq!(
        payload.reply_message_id.as_deref(),
        Some("<source-message@example.test>")
    );
    assert_eq!(
        payload.reference_message_ids,
        vec![
            "<root-message@example.test>".to_string(),
            "<prior-message@example.test>".to_string(),
            "<source-message@example.test>".to_string()
        ]
    );
    assert_eq!(
        payload.credential_user_id.as_deref(),
        Some("user_jordan"),
        "the reply is drafted with the SOURCE account's credential"
    );

    // No source binding (legacy/note items) → the approver's credential.
    let fallback =
        service::build_approval_job(&draft.draft, "op_test", "op_test", 5_000).expect("job");
    let fallback_payload: GmailDraftCreateOutboxPayload =
        serde_json::from_str(&fallback.payload_json).expect("payload");
    assert_eq!(
        fallback_payload.credential_user_id.as_deref(),
        Some("op_test")
    );

    let mut cc_draft = draft.draft.clone();
    cc_draft.cc_addrs = vec!["ops@example.test".to_string()];
    let cc_job =
        service::build_approval_job(&cc_draft, "op_test", "user_jordan", 5_000).expect("cc job");
    let cc_payload: GmailDraftCreateOutboxPayload =
        serde_json::from_str(&cc_job.payload_json).expect("payload");
    assert_eq!(cc_payload.cc, vec!["ops@example.test".to_string()]);

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
        &job,
        None,
    )
    .expect("approve");
    assert!(matches!(outcome, MutationOutcome::Applied { .. }));
    let approved = store::get_draft(conn, CLIENT, &draft_id, &OperatorScope::All)
        .expect("get")
        .expect("exists");
    assert_eq!(approved.draft.status, EmailDraftStatus::Approved);

    // Claim and deliver with the gate CLOSED: dry-run, no network.
    let claimed =
        outbox::claim_due_jobs(conn, CLIENT, Some("gmail"), 60_000, 10, 6_000).expect("claim");
    assert_eq!(claimed.len(), 1);
    let oauth = GoogleOAuthConfig {
        client_id: "app".to_string(),
        client_secret: "secret".to_string(),
        refresh_token: "refresh".to_string(),
        scopes: vec!["https://www.googleapis.com/auth/gmail.compose".to_string()],
        token_url: None,
    };
    let outcome = service::execute_job(&claimed[0], Some(&oauth), false, 6_000);
    let AttemptOutcome::Delivered { result_json } = &outcome else {
        panic!("expected delivered, got {outcome:?}");
    };
    let result: serde_json::Value = serde_json::from_str(result_json).expect("json");
    assert_eq!(result["dry_run"], json!(true));

    let status =
        outbox::record_attempt(conn, CLIENT, &claimed[0], &outcome, 6_500).expect("record");
    assert_eq!(status, outbox::STATUS_DELIVERED);
    let final_draft = store::get_draft(conn, CLIENT, &draft_id, &OperatorScope::All)
        .expect("get")
        .expect("exists");
    assert_eq!(final_draft.outbox_job.expect("summary").dry_run, Some(true));
}

#[test]
fn approve_with_follow_up_inserts_task_workflow_and_replays_cleanly() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let draft_id = staged_draft(conn);
    let draft = store::get_draft(conn, CLIENT, &draft_id, &OperatorScope::All)
        .expect("get")
        .expect("exists");
    let job = service::build_approval_job(&draft.draft, "op_test", "op_test", 5_000).expect("job");
    let request = EmailDraftFollowUpRequest {
        enabled: true,
        due_date: Some("2026-06-26".to_string()),
        title: "Follow up: storefront quote".to_string(),
        context: "Ask Dana if she can send dimensions.".to_string(),
        create_follow_up_draft: false,
    };
    let plan = store::EmailFollowUpPlan::from_request(&draft.draft, &request, 5_000)
        .expect("valid")
        .expect("enabled");
    let follow_up_id = plan.follow_up_id.clone();
    let task_id = plan.task.task_id.clone();

    let outcome = store::approve_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "op_test",
            scope: &OperatorScope::All,
            expected_revision: Some(draft.revision),
            idempotency_key: "approve_follow_up",
            now_ms: 5_000,
        },
        &draft_id,
        &job,
        Some(plan.clone()),
    )
    .expect("approve");
    assert!(matches!(outcome, MutationOutcome::Applied { .. }));

    let outbox_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM outbox_jobs WHERE client_id = ?1",
            [CLIENT],
            |row| row.get(0),
        )
        .expect("outbox count");
    assert_eq!(outbox_count, 1);
    let task_status: String = conn
        .query_row(
            "SELECT status FROM tasks WHERE client_id = ?1 AND task_id = ?2",
            rusqlite::params![CLIENT, task_id],
            |row| row.get(0),
        )
        .expect("task");
    assert_eq!(task_status, "open");
    let summary = store::get_follow_up(conn, CLIENT, &follow_up_id, &OperatorScope::All)
        .expect("follow up")
        .expect("exists");
    assert_eq!(summary.thread_state, GmailThreadFollowUpState::DraftCreated);
    assert_eq!(summary.status, EmailOutboundFollowUpStatus::Active);
    assert_eq!(
        summary.follow_up_task_id.as_deref(),
        Some(plan.task.task_id.as_str())
    );

    let approved = store::get_draft(conn, CLIENT, &draft_id, &OperatorScope::All)
        .expect("get")
        .expect("exists");
    assert_eq!(
        approved.follow_up.expect("summary").follow_up_id,
        follow_up_id
    );

    let replay = store::approve_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "op_test",
            scope: &OperatorScope::All,
            expected_revision: Some(draft.revision),
            idempotency_key: "approve_follow_up",
            now_ms: 5_000,
        },
        &draft_id,
        &job,
        Some(plan),
    )
    .expect("replay");
    assert!(matches!(replay, MutationOutcome::ReplayedIdempotent { .. }));
    let follow_up_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM email_outbound_follow_ups WHERE client_id = ?1",
            [CLIENT],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(follow_up_count, 1);
}

#[test]
fn task_decoration_filters_email_follow_ups_by_operator_scope() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();

    let create_for_user = |conn: &mut rusqlite::Connection, user_id: &str, item_id: &str| {
        let draft_id =
            staged_draft_with_source(conn, item_id, Some(user_id), &format!("produce_{item_id}"));
        let draft = store::get_draft(conn, CLIENT, &draft_id, &OperatorScope::All)
            .expect("get")
            .expect("exists");
        let job =
            service::build_approval_job(&draft.draft, "op_test", user_id, 5_000).expect("job");
        let plan = store::EmailFollowUpPlan::from_request(
            &draft.draft,
            &EmailDraftFollowUpRequest {
                enabled: true,
                due_date: Some("2026-06-26".to_string()),
                title: format!("Follow up {user_id}"),
                context: String::new(),
                create_follow_up_draft: false,
            },
            5_000,
        )
        .expect("valid")
        .expect("enabled");
        store::approve_draft(
            conn,
            DraftActionContext {
                client_id: CLIENT,
                actor_id: "op_test",
                scope: &OperatorScope::All,
                expected_revision: Some(draft.revision),
                idempotency_key: &format!("approve_{item_id}"),
                now_ms: 5_000,
            },
            &draft_id,
            &job,
            Some(plan),
        )
        .expect("approve");
    };

    create_for_user(conn, "u1", "wi_u1_follow_up");
    create_for_user(conn, "u2", "wi_u2_follow_up");

    let mut tasks = crate::slices::follow_up_tasks::store::list_tasks(
        conn,
        CLIENT,
        None,
        10,
        &OperatorScope::User("u1".to_string()),
    )
    .expect("tasks");
    store::decorate_tasks_with_follow_ups(
        conn,
        CLIENT,
        &OperatorScope::User("u1".to_string()),
        &mut tasks,
    )
    .expect("decorate");

    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].task.title, "Follow up u1");
    assert_eq!(
        tasks[0]
            .follow_up
            .as_ref()
            .expect("follow-up summary")
            .follow_up_title,
        "Follow up u1"
    );
}

#[test]
fn thread_classifier_ignores_drafts_and_detects_sent_then_reply() {
    let draft_only = vec![gmail_message("draft-1", &["DRAFT"], 5_100)];
    let result = service::classify_thread_follow_up(&draft_only, 5_000);
    assert_eq!(result.thread_state, GmailThreadFollowUpState::DraftCreated);
    assert_eq!(result.status, EmailOutboundFollowUpStatus::Active);

    let sent = vec![
        gmail_message("draft-1", &["DRAFT"], 5_100),
        gmail_message("sent-1", &["SENT"], 5_200),
    ];
    let result = service::classify_thread_follow_up(&sent, 5_000);
    assert_eq!(
        result.thread_state,
        GmailThreadFollowUpState::SentWaitingReply
    );
    assert_eq!(result.sent_message_id.as_deref(), Some("sent-1"));

    let replied = vec![
        gmail_message("sent-1", &["SENT"], 5_200),
        gmail_message("inbound-1", &["INBOX"], 5_300),
    ];
    let result = service::classify_thread_follow_up(&replied, 5_000);
    assert_eq!(
        result.thread_state,
        GmailThreadFollowUpState::RepliedAfterSend
    );
    assert_eq!(result.status, EmailOutboundFollowUpStatus::Resolved);
    assert_eq!(result.reply_message_id.as_deref(), Some("inbound-1"));
    assert_eq!(result.resolution_reason.as_deref(), Some("they_replied"));
}

#[test]
fn reconciliation_resolves_linked_follow_up_task() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let draft_id = staged_draft(conn);
    let draft = store::get_draft(conn, CLIENT, &draft_id, &OperatorScope::All)
        .expect("get")
        .expect("exists");
    let job = service::build_approval_job(&draft.draft, "op_test", "op_test", 5_000).expect("job");
    let plan = store::EmailFollowUpPlan::from_request(
        &draft.draft,
        &EmailDraftFollowUpRequest {
            enabled: true,
            due_date: Some("2026-06-26".to_string()),
            title: "Follow up".to_string(),
            context: String::new(),
            create_follow_up_draft: false,
        },
        5_000,
    )
    .expect("valid")
    .expect("enabled");
    let linked_task = plan.task.task_id.clone();
    let follow_up_id = plan.follow_up_id.clone();
    store::approve_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "op_test",
            scope: &OperatorScope::All,
            expected_revision: Some(draft.revision),
            idempotency_key: "approve_resolve",
            now_ms: 5_000,
        },
        &draft_id,
        &job,
        Some(plan),
    )
    .expect("approve");
    let before_revision: i64 = conn
        .query_row(
            "SELECT revision FROM entity_revisions WHERE client_id = ?1 AND entity_kind = 'task' AND entity_id = ?2",
            rusqlite::params![CLIENT, linked_task],
            |row| row.get(0),
        )
        .expect("task revision before");
    let outcome = store::apply_thread_reconciliation(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "op_test",
            scope: &OperatorScope::All,
            expected_revision: None,
            idempotency_key: "check_resolve",
            now_ms: 6_000,
        },
        &follow_up_id,
        store::ThreadReconciliation {
            thread_state: GmailThreadFollowUpState::RepliedAfterSend,
            status: EmailOutboundFollowUpStatus::Resolved,
            sent_message_id: Some("sent-1".to_string()),
            sent_at_ms: Some(5_200),
            reply_message_id: Some("reply-1".to_string()),
            reply_at_ms: Some(5_300),
            resolution_reason: Some("they_replied".to_string()),
            last_check_error: None,
        },
    )
    .expect("reconcile");
    assert!(outcome.should_complete_linked_task);
    assert_eq!(
        outcome.linked_task_id.as_deref(),
        Some(linked_task.as_str())
    );

    let still_open: String = conn
        .query_row(
            "SELECT status FROM tasks WHERE client_id = ?1 AND task_id = ?2",
            rusqlite::params![CLIENT, linked_task],
            |row| row.get(0),
        )
        .expect("linked task still open");
    assert_eq!(
        still_open, "open",
        "email reconciliation must not raw-update the linked task"
    );
    crate::slices::follow_up_tasks::store::apply_task_action(
        conn,
        crate::slices::follow_up_tasks::store::DraftActionContext {
            client_id: CLIENT,
            actor_id: "op_test",
            scope: &OperatorScope::All,
            expected_revision: None,
            idempotency_key: "check_resolve:linked_task_done",
            now_ms: 6_000,
        },
        &linked_task,
        crate::slices::follow_up_tasks::store::TaskAction::Complete,
    )
    .expect("complete linked task through task store");

    let linked_status: String = conn
        .query_row(
            "SELECT status FROM tasks WHERE client_id = ?1 AND task_id = ?2",
            rusqlite::params![CLIENT, linked_task],
            |row| row.get(0),
        )
        .expect("linked task");
    assert_eq!(linked_status, "done");
    let after_revision: i64 = conn
        .query_row(
            "SELECT revision FROM entity_revisions WHERE client_id = ?1 AND entity_kind = 'task' AND entity_id = ?2",
            rusqlite::params![CLIENT, linked_task],
            |row| row.get(0),
        )
        .expect("task revision after");
    assert_eq!(after_revision, before_revision + 1);
    let task_receipt_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM receipts WHERE client_id = ?1 AND entity_kind = 'task' AND entity_id = ?2 AND change_kind = 'complete' AND outcome = 'applied'",
            rusqlite::params![CLIENT, linked_task],
            |row| row.get(0),
        )
        .expect("task receipt count");
    assert_eq!(task_receipt_count, 1);
    let summary = store::get_follow_up(conn, CLIENT, &follow_up_id, &OperatorScope::All)
        .expect("summary")
        .expect("exists");
    assert_eq!(summary.resolution_reason.as_deref(), Some("they_replied"));
}

#[test]
fn missing_credential_retries_and_unsupported_is_terminal() {
    let job = outbox::ClaimedJob {
        job_id: "obj_x".to_string(),
        provider: "gmail".to_string(),
        capability: "create_draft".to_string(),
        payload_json: serde_json::to_string(&GmailDraftCreateOutboxPayload {
            credential_user_id: None,
            idempotency_key: "k".to_string(),
            approval: bos_integrations::gmail_draft_write::GmailDraftApprovalMetadata {
                approval_id: "a".to_string(),
                approved_by: "op".to_string(),
                approved_at: "2026-06-10T00:00:00Z".to_string(),
            },
            to: "dana@example.test".to_string(),
            cc: Vec::new(),
            subject: "Re: x".to_string(),
            body_text: "body".to_string(),
            thread_id: None,
            reply_message_id: None,
            reference_message_ids: Vec::new(),
        })
        .expect("payload"),
        attempts: 0,
        source_entity_kind: "x".to_string(),
        source_entity_id: "x".to_string(),
        correlation_id: None,
        idempotency_key: "k".to_string(),
    };
    assert!(matches!(
        service::execute_job(&job, None, false, 1_000),
        AttemptOutcome::Retry { .. }
    ));

    let mut wrong = job.clone();
    wrong.capability = "send".to_string();
    let oauth = GoogleOAuthConfig {
        client_id: "app".to_string(),
        client_secret: "secret".to_string(),
        refresh_token: "refresh".to_string(),
        scopes: Vec::new(),
        token_url: None,
    };
    assert!(matches!(
        service::execute_job(&wrong, Some(&oauth), false, 1_000),
        AttemptOutcome::Terminal { .. }
    ));
}

#[test]
fn second_active_draft_refused_and_reject_frees_reproduce() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let draft_id = staged_draft(conn);

    let fill = service::parse_reply_fill_response(&valid_fill_response()).expect("fill");
    let second = service::draft_from_fill(
        &accepted_item(),
        &source_message(),
        &fill,
        2,
        "test-model",
        3_000,
    )
    .expect("draft");
    let err = store::insert_draft(conn, CLIENT, "op_test", &second, "produce_2")
        .expect_err("second active draft must be refused");
    assert!(matches!(err, StoreError::Domain(code) if code == "email_draft_already_active"));

    store::reject_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "op_test",
            scope: &OperatorScope::All,
            expected_revision: None,
            idempotency_key: "reject_1",
            now_ms: 4_000,
        },
        &draft_id,
    )
    .expect("reject");
    store::insert_draft(conn, CLIENT, "op_test", &second, "produce_2")
        .expect("re-produce after reject");
}

#[tokio::test]
async fn approval_route_binds_email_job_to_source_user_credential() {
    let state = test_state_configured(None, &[]);
    let draft_id = {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        create_operator(conn, "approver", "tok_approver");
        create_operator(conn, "u2", "tok_u2");
        store_gmail_credential(conn, "u2");
        staged_draft_with_source(conn, "wi_route_u2", Some("u2"), "produce_route_u2")
    };
    let router = build_router(state.clone());

    let response = approve_email_draft(router, &draft_id, "approver", "approve_route_u2").await;
    assert_eq!(response.status(), StatusCode::OK);

    let mut persistence = state.persistence.lock();
    let claimed = outbox::claim_due_jobs(
        persistence.connection(),
        CLIENT,
        Some("gmail"),
        60_000,
        10,
        i64::MAX as u64,
    )
    .expect("claim");
    assert_eq!(claimed.len(), 1);
    let payload: GmailDraftCreateOutboxPayload =
        serde_json::from_str(&claimed[0].payload_json).expect("payload");
    assert_eq!(payload.credential_user_id.as_deref(), Some("u2"));
    assert_eq!(payload.approval.approved_by, "approver");
}

#[tokio::test]
async fn approval_route_rejects_source_bound_email_without_source_credential() {
    let state = test_state_configured(None, &[]);
    let draft_id = {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        create_operator(conn, "approver", "tok_approver");
        create_operator(conn, "u2", "tok_u2");
        staged_draft_with_source(
            conn,
            "wi_route_missing",
            Some("u2"),
            "produce_route_missing",
        )
    };
    let router = build_router(state.clone());

    let response =
        approve_email_draft(router, &draft_id, "approver", "approve_route_missing").await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        response_error(response).await,
        "source_user_credential_unavailable"
    );

    let mut persistence = state.persistence.lock();
    let claimed = outbox::claim_due_jobs(
        persistence.connection(),
        CLIENT,
        Some("gmail"),
        60_000,
        10,
        i64::MAX as u64,
    )
    .expect("claim");
    assert!(claimed.is_empty());
}

#[tokio::test]
async fn approval_route_keeps_email_legacy_approver_fallback_for_null_draft() {
    let state = test_state_configured(None, &[]);
    let draft_id = {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        create_operator(conn, "approver", "tok_approver");
        staged_draft_with_source(conn, "wi_route_null", None, "produce_route_null")
    };
    let router = build_router(state.clone());

    let response = approve_email_draft(router, &draft_id, "approver", "approve_route_null").await;
    assert_eq!(response.status(), StatusCode::OK);

    let mut persistence = state.persistence.lock();
    let claimed = outbox::claim_due_jobs(
        persistence.connection(),
        CLIENT,
        Some("gmail"),
        60_000,
        10,
        i64::MAX as u64,
    )
    .expect("claim");
    assert_eq!(claimed.len(), 1);
    let payload: GmailDraftCreateOutboxPayload =
        serde_json::from_str(&claimed[0].payload_json).expect("payload");
    assert_eq!(payload.credential_user_id.as_deref(), Some("approver"));
    assert_eq!(payload.approval.approved_by, "approver");
}
