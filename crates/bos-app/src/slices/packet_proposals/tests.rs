use axum::body::Body;
use axum::http::{Request, StatusCode};
use bos_contracts::email_triage::{CategoryRecord, InboundMessageRecord};
use bos_contracts::packet_proposals::{
    PacketProposalDecisionMode, PacketProposalExecutionMode, PacketProposalKindOutcomeStatus,
    PacketProposalReasonCode, PacketProposalRunStatus, SmartDraftResponse,
    SmartDraftSourceStateResponse,
};
use bos_contracts::receipt::{ActorKindDto, ReceiptOutcomeDto};
use bos_contracts::work_queue::{WorkItem, WorkItemAcceptActor, WorkItemStatus, WorkQueuePolicy};
use bos_integrations::llm_api::{DirectLlmToolCall, DirectLlmToolTurnResponse};
use bos_integrations::llm_typed_tasks::{TypedLlmExecutionRoute, TypedLlmTaskOutputEnvelope};
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

use crate::produce::{PreparedProposalKind, ProposalContract};

use super::{
    service::{self, SmartDraftCandidateMode, SmartDraftInput},
    store::{self, NewRun, RUN_ENTITY_KIND},
};

const CLIENT: &str = "test-client";

#[test]
fn phase_a_execution_mode_is_bounded_typed() {
    assert_eq!(
        service::EXECUTION_MODE_BOUNDED_TYPED,
        bos_contracts::packet_proposals::PacketProposalExecutionMode::BoundedTyped
    );
}

#[test]
fn smart_draft_stages_existing_draft_writer_and_records_non_null_draft_id() {
    let _guard = service::test_packet_proposal_lock();
    service::reset_test_packet_proposal_state();
    let state = setup_email(
        "msg_draft_success",
        "billing",
        policy(vec!["email_draft_reply"], vec![]),
    );
    service::set_test_packet_proposal_response(json!({
        "suggested_category": null,
        "rationale": "The sender expects a reply.",
        "outcomes": [{
            "packet_kind": "email_draft_reply",
            "status": "drafted",
            "draft": {
                "body_text": "Thanks for reaching out. Could you send the haul-out date?",
                "confidence": "high",
                "provenance": [{ "field": "body_text", "quote": "Need a quote" }]
            }
        }]
    }));

    let response =
        service::run_smart_draft(state.clone(), input("msg_draft_success", "key-success"))
            .expect("smart draft");

    assert_eq!(response.run.status, PacketProposalRunStatus::Completed);
    assert_eq!(
        response.run.resolved_decision_mode,
        PacketProposalDecisionMode::FillFixed
    );
    assert_eq!(response.run.outcomes.len(), 1);
    assert_eq!(
        response.run.outcomes[0].status,
        PacketProposalKindOutcomeStatus::Drafted
    );
    assert!(
        response.run.outcomes[0]
            .draft_id
            .as_deref()
            .is_some_and(|id| !id.is_empty()),
        "drafted outcomes must carry the existing draft row id"
    );
    let item = response.item.expect("item").item;
    assert_eq!(item.accept_actor, Some(WorkItemAcceptActor::System));
    assert_eq!(item.packet_kinds, vec!["email_draft_reply".to_string()]);
    let requests = service::take_test_packet_proposal_requests();
    assert_eq!(requests.len(), 1);
    let source_block = requests[0]
        .input
        .text_blocks
        .iter()
        .find(|block| block.block_id == "source")
        .expect("source block");
    assert!(source_block.text.contains("Date (epoch ms): 1700000000000"));
    assert!(source_block.text.contains("Email date (UTC): 2023-11-14"));
    assert!(requests[0]
        .input
        .text_blocks
        .iter()
        .any(|block| block.block_id == "shared_context"));
    assert_eq!(
        requests[0].input.json["packet_contracts"][0]["context_ref"],
        "shared_context"
    );
    assert_eq!(
        requests[0].input.json["packet_contracts"][0]["instructions"],
        crate::slices::email_drafts::service::FILL_INSTRUCTIONS
    );
}

#[test]
fn smart_draft_calendar_no_event_outcome_includes_stage_reason() {
    let _guard = service::test_packet_proposal_lock();
    service::reset_test_packet_proposal_state();
    let state = setup_email(
        "msg_calendar_no_event",
        "billing",
        policy(vec!["calendar_event_draft"], vec![]),
    );
    service::set_test_packet_proposal_response(json!({
        "suggested_category": null,
        "rationale": "The message does not describe a dated event.",
        "outcomes": [{
            "packet_kind": "calendar_event_draft",
            "status": "drafted",
            "draft": {
                "extractable": false,
                "reason": "newsletter with no concrete dated event"
            }
        }]
    }));

    let response = service::run_smart_draft(
        state.clone(),
        input("msg_calendar_no_event", "key-calendar-no-event"),
    )
    .expect("smart draft");

    assert_eq!(response.run.status, PacketProposalRunStatus::Completed);
    assert_eq!(response.run.outcomes.len(), 1);
    let outcome = &response.run.outcomes[0];
    assert_eq!(
        outcome.status,
        PacketProposalKindOutcomeStatus::RejectedByGate
    );
    assert_eq!(
        outcome.reason_code,
        Some(PacketProposalReasonCode::GateRejected)
    );
    assert_eq!(
        outcome.message.as_deref(),
        Some("newsletter with no concrete dated event")
    );
    assert!(outcome.draft_id.is_none());
    let persistence = state.persistence.lock();
    let evidence =
        store::evidence_for_run(persistence.connection_ref(), CLIENT, &response.run.run_id)
            .expect("evidence");
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].tool_name, "proposal_stage");
    assert_eq!(evidence[0].result_ref, "calendar_extract_no_event");
    assert!(evidence[0]
        .result_excerpt
        .contains("newsletter with no concrete dated event"));
    assert!(evidence[0].result_excerpt.contains("\"extractable\":false"));
}

#[test]
fn smart_draft_confidence_fallback_reads_packet_specific_response_key() {
    let _guard = service::test_packet_proposal_lock();
    service::reset_test_packet_proposal_state();
    let state = setup_email(
        "msg_response_key_confidence",
        "billing",
        policy(vec!["email_draft_reply"], vec![]),
    );
    service::set_test_packet_proposal_response(json!({
        "suggested_category": null,
        "rationale": "The sender expects a reply.",
        "outcomes": [{
            "packet_kind": "email_draft_reply",
            "status": "drafted",
            "email_draft_reply": {
                "body_text": "Thanks for reaching out. Could you send the haul-out date?",
                "confidence": "high",
                "provenance": [{ "field": "body_text", "quote": "Need a quote" }]
            }
        }]
    }));

    let mut input = input("msg_response_key_confidence", "key-response-confidence");
    input.min_confidence = Some(crate::slices::email_triage::service::AiConfidence::High);
    let response = service::run_smart_draft(state, input).expect("smart draft");

    assert_eq!(response.run.status, PacketProposalRunStatus::Completed);
    assert_eq!(response.run.confidence.as_deref(), Some("high"));
    assert_eq!(
        response.run.outcomes[0].status,
        PacketProposalKindOutcomeStatus::Drafted
    );
}

#[test]
fn smart_draft_records_malformed_outcome_evidence() {
    let _guard = service::test_packet_proposal_lock();
    service::reset_test_packet_proposal_state();
    let state = setup_email(
        "msg_malformed_outcome",
        "billing",
        policy(vec!["email_draft_reply"], vec![]),
    );
    service::set_test_packet_proposal_response(json!({
        "suggested_category": null,
        "rationale": "The sender expects a reply.",
        "outcomes": [{
            "packet_kind": "email_draft_reply",
            "status": "drafted",
            "body_text": "This is not nested under draft or email_draft_reply."
        }]
    }));

    let response = service::run_smart_draft(
        state.clone(),
        input("msg_malformed_outcome", "key-malformed-outcome"),
    )
    .expect("smart draft");

    assert_eq!(
        response.run.outcomes[0].reason_code,
        Some(PacketProposalReasonCode::ModelOutputInvalid)
    );
    assert_eq!(
        response.run.outcomes[0].message.as_deref(),
        Some("draft payload missing or invalid")
    );
    let persistence = state.persistence.lock();
    let evidence =
        store::evidence_for_run(persistence.connection_ref(), CLIENT, &response.run.run_id)
            .expect("evidence");
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].tool_name, "proposal_stage");
    assert_eq!(evidence[0].result_ref, "model_output_invalid");
    assert!(evidence[0]
        .result_excerpt
        .contains("This is not nested under draft"));
}

#[test]
fn unavailable_outcome_without_optional_reason_is_context_unavailable() {
    let _guard = service::test_packet_proposal_lock();
    service::reset_test_packet_proposal_state();
    let state = setup_email(
        "msg_unavailable_without_reason",
        "billing",
        policy(vec!["email_draft_reply"], vec![]),
    );
    service::set_test_packet_proposal_response(json!({
        "suggested_category": null,
        "confidence": "high",
        "rationale": "The source does not warrant a reply.",
        "outcomes": [{
            "packet_kind": "email_draft_reply",
            "status": "unavailable"
        }]
    }));

    let response = service::run_smart_draft(
        state,
        input(
            "msg_unavailable_without_reason",
            "key-unavailable-without-reason",
        ),
    )
    .expect("smart draft");

    assert_eq!(response.run.outcomes.len(), 1);
    assert_eq!(
        response.run.outcomes[0].reason_code,
        Some(PacketProposalReasonCode::ContextUnavailable)
    );
}

#[test]
fn unavailable_outcome_with_null_optional_reason_is_context_unavailable() {
    let _guard = service::test_packet_proposal_lock();
    service::reset_test_packet_proposal_state();
    let state = setup_email(
        "msg_unavailable_with_null_reason",
        "billing",
        policy(vec!["email_draft_reply"], vec![]),
    );
    service::set_test_packet_proposal_response(json!({
        "suggested_category": null,
        "confidence": "high",
        "rationale": "The source does not warrant a reply.",
        "outcomes": [{
            "packet_kind": "email_draft_reply",
            "status": "unavailable",
            "reason_code": null
        }]
    }));

    let response = service::run_smart_draft(
        state,
        input("msg_unavailable_with_null_reason", "key-null-reason"),
    )
    .expect("smart draft");

    assert_eq!(response.run.outcomes.len(), 1);
    assert_eq!(
        response.run.outcomes[0].reason_code,
        Some(PacketProposalReasonCode::ContextUnavailable)
    );
}

#[test]
fn smart_draft_confidence_fallback_uses_lowest_drafted_confidence() {
    let _guard = service::test_packet_proposal_lock();
    service::reset_test_packet_proposal_state();
    let state = setup_email(
        "msg_mixed_confidence",
        "billing",
        policy(vec!["email_draft_reply", "follow_up_task"], vec![]),
    );
    service::set_test_packet_proposal_response(json!({
        "suggested_category": null,
        "rationale": "The sender expects a reply and follow-up.",
        "outcomes": [
            {
                "packet_kind": "email_draft_reply",
                "status": "drafted",
                "draft": {
                    "body_text": "Thanks for reaching out. Could you send the haul-out date?",
                    "confidence": "high",
                    "provenance": [{ "field": "body_text", "quote": "Need a quote" }]
                }
            },
            {
                "packet_kind": "follow_up_task",
                "status": "drafted",
                "draft": {
                    "title": "Follow up about the quote",
                    "due_date": null,
                    "context": "The sender asked for quote next steps.",
                    "confidence": "low",
                    "provenance": [{ "field": "title", "quote": "Need a quote" }]
                }
            }
        ]
    }));

    let mut input = input("msg_mixed_confidence", "key-mixed-confidence");
    input.min_confidence = Some(crate::slices::email_triage::service::AiConfidence::High);
    let response = service::run_smart_draft(state, input).expect("smart draft");

    assert_eq!(response.run.status, PacketProposalRunStatus::Completed);
    assert_eq!(response.run.confidence.as_deref(), Some("low"));
    assert!(response
        .run
        .outcomes
        .iter()
        .all(|outcome| outcome.status == PacketProposalKindOutcomeStatus::Unavailable));
    assert!(response.item.is_none());
}

#[test]
fn fill_fixed_missing_kind_becomes_unavailable_not_declined() {
    let _guard = service::test_packet_proposal_lock();
    service::reset_test_packet_proposal_state();
    let state = setup_email(
        "msg_fill_fixed_missing",
        "billing",
        policy(vec!["email_draft_reply"], vec![]),
    );
    service::set_test_packet_proposal_response(json!({
        "suggested_category": null,
        "rationale": "No reliable reply can be drafted.",
        "outcomes": []
    }));

    let response = service::run_smart_draft(state, input("msg_fill_fixed_missing", "key-missing"))
        .expect("smart draft");

    assert_eq!(response.run.status, PacketProposalRunStatus::Completed);
    assert_eq!(
        response.run.resolved_decision_mode,
        PacketProposalDecisionMode::FillFixed
    );
    assert_eq!(response.run.outcomes.len(), 1);
    assert_eq!(
        response.run.outcomes[0].status,
        PacketProposalKindOutcomeStatus::Unavailable
    );
    assert!(response.run.outcomes[0].draft_id.is_none());
}

#[test]
fn terminal_matching_run_short_circuits_before_second_llm_call() {
    let _guard = service::test_packet_proposal_lock();
    service::reset_test_packet_proposal_state();
    let state = setup_email(
        "msg_short_circuit",
        "billing",
        policy(vec!["email_draft_reply"], vec![]),
    );
    service::set_test_packet_proposal_response(json!({
        "suggested_category": null,
        "rationale": "The sender expects a reply.",
        "outcomes": [{
            "packet_kind": "email_draft_reply",
            "status": "drafted",
            "draft": {
                "body_text": "Thanks for reaching out. Could you send the haul-out date?",
                "confidence": "high",
                "provenance": [{ "field": "body_text", "quote": "Need a quote" }]
            }
        }]
    }));
    let first = service::run_smart_draft(state.clone(), input("msg_short_circuit", "key-first"))
        .expect("first run");
    assert_eq!(service::take_test_packet_proposal_requests().len(), 1);

    let second = service::run_smart_draft(state, input("msg_short_circuit", "key-second"))
        .expect("second run");

    assert_eq!(second.run.run_id, first.run.run_id);
    assert_eq!(service::take_test_packet_proposal_requests().len(), 0);
}

#[test]
fn failed_matching_run_does_not_block_fresh_key_retry() {
    let _guard = service::test_packet_proposal_lock();
    service::reset_test_packet_proposal_state();
    let state = setup_email(
        "msg_failed_retry",
        "billing",
        policy(vec!["email_draft_reply"], vec![]),
    );
    service::set_test_packet_proposal_response(json!({
        "suggested_category": null,
        "rationale": "Malformed response.",
        "outcomes": "not an array"
    }));
    let first = service::run_smart_draft(state.clone(), input("msg_failed_retry", "key-failed"));
    assert!(
        first.is_err(),
        "malformed proposal should fail the first run"
    );
    assert_eq!(service::take_test_packet_proposal_requests().len(), 1);

    service::set_test_packet_proposal_response(json!({
        "suggested_category": null,
        "rationale": "The sender expects a reply.",
        "outcomes": [{
            "packet_kind": "email_draft_reply",
            "status": "drafted",
            "draft": {
                "body_text": "Thanks for reaching out. Could you send the haul-out date?",
                "confidence": "high",
                "provenance": [{ "field": "body_text", "quote": "Need a quote" }]
            }
        }]
    }));
    let second = service::run_smart_draft(state, input("msg_failed_retry", "key-retry"))
        .expect("fresh-key retry should run the model again");

    assert_eq!(second.run.status, PacketProposalRunStatus::Completed);
    assert_eq!(service::take_test_packet_proposal_requests().len(), 1);
}

#[test]
fn existing_open_item_requires_expected_revision_before_smart_draft_accepts() {
    let _guard = service::test_packet_proposal_lock();
    service::reset_test_packet_proposal_state();
    let state = setup_email(
        "msg_existing_open",
        "billing",
        policy(vec!["email_draft_reply"], vec![]),
    );
    insert_open_item(&state, "msg_existing_open");

    let err = service::run_smart_draft(state, input("msg_existing_open", "key-open"))
        .expect_err("missing expected_revision should be rejected");

    assert!(matches!(
        err,
        service::SmartDraftError::Store(crate::store_core::StoreError::Domain(code))
            if code == "expected_revision_required"
    ));
    assert_eq!(service::take_test_packet_proposal_requests().len(), 0);
}

#[test]
fn stale_expected_revision_rejects_before_smart_draft_spends_llm() {
    let _guard = service::test_packet_proposal_lock();
    service::reset_test_packet_proposal_state();
    let state = setup_email(
        "msg_existing_open_stale",
        "billing",
        policy(vec!["email_draft_reply"], vec![]),
    );
    let current_revision = insert_open_item(&state, "msg_existing_open_stale");

    let mut stale_input = input("msg_existing_open_stale", "key-open-stale");
    stale_input.expected_revision = Some(current_revision + 1);
    let err = service::run_smart_draft(state, stale_input)
        .expect_err("stale expected_revision should be rejected");

    assert!(matches!(
        err,
        service::SmartDraftError::RevisionConflict {
            current_revision: Some(revision)
        } if revision == current_revision
    ));
    assert_eq!(service::take_test_packet_proposal_requests().len(), 0);
}

#[test]
fn post_claim_revision_conflict_marks_run_failed_without_deadlock() {
    let _guard = service::test_packet_proposal_lock();
    service::reset_test_packet_proposal_state();
    let gate = service::TestPacketProposalLlmGate::new();
    service::set_test_packet_proposal_llm_gate(gate.clone());
    let state = setup_email(
        "msg_post_claim_revision_conflict",
        "billing",
        policy(vec!["email_draft_reply"], vec![]),
    );
    let expected_revision = insert_open_item(&state, "msg_post_claim_revision_conflict");
    service::set_test_packet_proposal_response(json!({
        "suggested_category": null,
        "rationale": "The sender expects a reply.",
        "outcomes": [{
            "packet_kind": "email_draft_reply",
            "status": "drafted",
            "draft": {
                "body_text": "Thanks for reaching out. Could you send the haul-out date?",
                "confidence": "high",
                "provenance": [{ "field": "body_text", "quote": "Need a quote" }]
            }
        }]
    }));

    let mut draft_input = input(
        "msg_post_claim_revision_conflict",
        "key-post-claim-conflict",
    );
    draft_input.expected_revision = Some(expected_revision);
    let worker_state = state.clone();
    let worker = std::thread::spawn(move || service::run_smart_draft(worker_state, draft_input));
    gate.wait_entered();

    let item_id = {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        let item = crate::slices::work_queue::store::get_item_for_source(
            conn,
            CLIENT,
            crate::slices::work_queue::SOURCE_KIND_EMAIL,
            "msg_post_claim_revision_conflict",
        )
        .expect("load item")
        .expect("item");
        let scope = crate::http::OperatorScope::All;
        crate::slices::work_queue::store::update_produce_guidance(
            conn,
            crate::slices::work_queue::store::ItemActionContext {
                client_id: CLIENT,
                actor_id: "op_other",
                scope: &scope,
                expected_revision: Some(expected_revision),
                idempotency_key: "bump-guidance-before-finish",
                now_ms: 1_250,
            },
            &item.item.item_id,
            "Operator changed guidance.",
        )
        .expect("bump item revision");
        item.item.item_id
    };

    gate.release();
    let err = worker
        .join()
        .expect("worker thread")
        .expect_err("revision conflict");
    assert!(matches!(
        err,
        service::SmartDraftError::RevisionConflict {
            current_revision: Some(revision)
        } if revision > expected_revision
    ));

    let run_id = service::test_smart_draft_run_id(
        crate::slices::work_queue::SOURCE_KIND_EMAIL,
        "msg_post_claim_revision_conflict",
        "key-post-claim-conflict",
    );
    let persistence = state.persistence.lock();
    let run = store::get_run(persistence.connection_ref(), CLIENT, &run_id)
        .expect("run")
        .expect("run row");
    assert_eq!(run.item_id.as_deref(), Some(item_id.as_str()));
    assert_eq!(run.status, PacketProposalRunStatus::Failed);
    assert_eq!(
        run.error_code.as_deref(),
        Some("expected_revision_conflict")
    );
    service::clear_test_packet_proposal_llm_gate();
}

#[test]
fn same_key_running_run_replays_without_second_llm_call() {
    let _guard = service::test_packet_proposal_lock();
    service::reset_test_packet_proposal_state();
    service::set_test_packet_proposal_stale_after_ms(u64::MAX);
    let state = setup_email(
        "msg_same_key_running",
        "billing",
        policy(vec!["email_draft_reply"], vec![]),
    );
    seed_running_run(&state, "msg_same_key_running", "key-running", 1_100);

    let response = service::run_smart_draft(state, input("msg_same_key_running", "key-running"))
        .expect("running replay");

    assert_eq!(response.run.status, PacketProposalRunStatus::Running);
    assert_eq!(service::take_test_packet_proposal_requests().len(), 0);
    service::clear_test_packet_proposal_stale_after_ms();
}

#[test]
fn different_key_running_run_for_same_source_replays_without_second_llm_call() {
    let _guard = service::test_packet_proposal_lock();
    service::reset_test_packet_proposal_state();
    service::set_test_packet_proposal_stale_after_ms(u64::MAX);
    let state = setup_email(
        "msg_different_key_running",
        "billing",
        policy(vec!["email_draft_reply"], vec![]),
    );
    let run_id = seed_running_run(&state, "msg_different_key_running", "key-first", 1_100);

    let response =
        service::run_smart_draft(state, input("msg_different_key_running", "key-second"))
            .expect("running replay");

    assert_eq!(response.run.run_id, run_id);
    assert_eq!(response.run.status, PacketProposalRunStatus::Running);
    assert_eq!(service::take_test_packet_proposal_requests().len(), 0);
    service::clear_test_packet_proposal_stale_after_ms();
}

#[test]
fn simultaneous_same_key_submits_single_flight_before_llm_execution() {
    let _guard = service::test_packet_proposal_lock();
    service::reset_test_packet_proposal_state();
    service::set_test_packet_proposal_stale_after_ms(u64::MAX);
    let gate = service::TestPacketProposalLlmGate::new();
    service::set_test_packet_proposal_llm_gate(gate.clone());
    let state = setup_email(
        "msg_same_key_concurrent",
        "billing",
        policy(vec!["email_draft_reply"], vec![]),
    );
    service::set_test_packet_proposal_response(json!({
        "suggested_category": null,
        "rationale": "The sender expects a reply.",
        "outcomes": [{
            "packet_kind": "email_draft_reply",
            "status": "drafted",
            "draft": {
                "body_text": "Thanks for reaching out. Could you send the haul-out date?",
                "confidence": "high",
                "provenance": [{ "field": "body_text", "quote": "Need a quote" }]
            }
        }]
    }));

    let first_state = state.clone();
    let first = std::thread::spawn(move || {
        service::run_smart_draft(
            first_state,
            input("msg_same_key_concurrent", "key-concurrent"),
        )
        .expect("first smart draft")
    });
    gate.wait_entered();

    let second_state = state.clone();
    let second = std::thread::spawn(move || {
        service::run_smart_draft(
            second_state,
            input("msg_same_key_concurrent", "key-concurrent"),
        )
        .expect("second smart draft")
    });
    let second = second.join().expect("second thread");
    assert_eq!(second.run.status, PacketProposalRunStatus::Running);

    gate.release();
    let first = first.join().expect("first thread");

    assert_eq!(first.run.status, PacketProposalRunStatus::Completed);
    assert_eq!(second.run.run_id, first.run.run_id);
    assert_eq!(service::take_test_packet_proposal_requests().len(), 1);
    service::clear_test_packet_proposal_llm_gate();
    service::clear_test_packet_proposal_stale_after_ms();
}

#[test]
fn different_key_stale_running_run_is_failed_before_fresh_claim() {
    let _guard = service::test_packet_proposal_lock();
    service::reset_test_packet_proposal_state();
    service::set_test_packet_proposal_stale_after_ms(0);
    let state = setup_email(
        "msg_different_key_stale",
        "billing",
        policy(vec!["email_draft_reply"], vec![]),
    );
    let stale_run_id = seed_running_run(&state, "msg_different_key_stale", "key-stale", 1_100);
    service::set_test_packet_proposal_response(json!({
        "suggested_category": null,
        "rationale": "The sender expects a reply.",
        "outcomes": [{
            "packet_kind": "email_draft_reply",
            "status": "drafted",
            "draft": {
                "body_text": "Thanks for reaching out. Could you send the haul-out date?",
                "confidence": "high",
                "provenance": [{ "field": "body_text", "quote": "Need a quote" }]
            }
        }]
    }));

    let response =
        service::run_smart_draft(state.clone(), input("msg_different_key_stale", "key-fresh"))
            .expect("fresh run after stale failure");

    assert_ne!(response.run.run_id, stale_run_id);
    assert_eq!(response.run.status, PacketProposalRunStatus::Completed);
    assert_eq!(service::take_test_packet_proposal_requests().len(), 1);
    let persistence = state.persistence.lock();
    let stale_run = store::get_run(persistence.connection_ref(), CLIENT, &stale_run_id)
        .expect("stale run")
        .expect("stale run row");
    assert_eq!(stale_run.status, PacketProposalRunStatus::Failed);
    assert_eq!(
        stale_run.error_code.as_deref(),
        Some(service::STALE_RUNNING_ERROR_CODE)
    );
    service::clear_test_packet_proposal_stale_after_ms();
}

#[test]
fn stale_running_run_is_marked_failed_on_read_with_receipt() {
    let _guard = service::test_packet_proposal_lock();
    service::reset_test_packet_proposal_state();
    service::set_test_packet_proposal_stale_after_ms(0);
    let state = setup_email(
        "msg_stale_running",
        "billing",
        policy(vec!["email_draft_reply"], vec![]),
    );
    let run_id = seed_running_run(&state, "msg_stale_running", "key-stale", 1_100);

    let response = service::run_smart_draft(state.clone(), input("msg_stale_running", "key-stale"))
        .expect("stale replay");

    assert_eq!(response.run.status, PacketProposalRunStatus::Failed);
    assert_eq!(
        response.run.error_code.as_deref(),
        Some(service::STALE_RUNNING_ERROR_CODE)
    );
    assert_eq!(service::take_test_packet_proposal_requests().len(), 0);
    let persistence = state.persistence.lock();
    let receipts = crate::store_core::receipts_for_entity(
        persistence.connection_ref(),
        CLIENT,
        RUN_ENTITY_KIND,
        &run_id,
        10,
    )
    .expect("receipts");
    assert!(receipts.iter().any(|receipt| {
        receipt.change_kind == "finish"
            && receipt.outcome == ReceiptOutcomeDto::Applied
            && receipt.idempotency_key == format!("smart_draft:{run_id}:stale")
    }));
    service::clear_test_packet_proposal_stale_after_ms();
}

#[test]
fn tool_loop_execution_mode_fails_closed_before_llm_spend() {
    let _guard = service::test_packet_proposal_lock();
    service::reset_test_packet_proposal_state();
    service::set_test_packet_proposal_execution_mode(PacketProposalExecutionMode::ToolLoopAgentic);
    service::clear_test_packet_proposal_tool_loop_enabled();
    let state = setup_email(
        "msg_tool_loop_closed",
        "billing",
        policy(vec!["email_draft_reply"], vec![]),
    );
    let err = service::run_smart_draft(
        state.clone(),
        input("msg_tool_loop_closed", "key-tool-loop"),
    )
    .expect_err("tool loop must fail closed");

    match err {
        service::SmartDraftError::Llm(code) => {
            assert_eq!(code, service::TOOL_LOOP_UNAVAILABLE_ERROR_CODE)
        }
        other => panic!("unexpected error: {other:?}"),
    }
    assert_eq!(service::take_test_packet_proposal_requests().len(), 0);
    let run_id = service::test_smart_draft_run_id(
        crate::slices::work_queue::SOURCE_KIND_EMAIL,
        "msg_tool_loop_closed",
        "key-tool-loop",
    );
    let persistence = state.persistence.lock();
    let run = store::get_run(persistence.connection_ref(), CLIENT, &run_id)
        .expect("run")
        .expect("run row");
    assert_eq!(
        run.execution_mode,
        PacketProposalExecutionMode::ToolLoopAgentic
    );
    assert_eq!(run.status, PacketProposalRunStatus::Failed);
    assert_eq!(
        run.error_code.as_deref(),
        Some(service::TOOL_LOOP_UNAVAILABLE_ERROR_CODE)
    );
    service::clear_test_packet_proposal_execution_mode();
}

#[test]
fn tool_loop_agentic_serde_round_trips() {
    let raw =
        serde_json::to_string(&PacketProposalExecutionMode::ToolLoopAgentic).expect("serialize");
    assert_eq!(raw, "\"tool_loop_agentic\"");
    let parsed: PacketProposalExecutionMode =
        serde_json::from_str("\"tool_loop_agentic\"").expect("deserialize");
    assert_eq!(parsed, PacketProposalExecutionMode::ToolLoopAgentic);
}

#[test]
fn tool_loop_agentic_exposes_final_grounding_tool_set_without_resolve_party() {
    let tools = service::test_packet_proposal_tool_names();

    assert_eq!(
        tools,
        &[
            crate::slices::grounding::TOOL_EMAIL_THREAD_LOOKUP,
            crate::slices::grounding::TOOL_CRM_CONTACT_LOOKUP,
            crate::slices::grounding::TOOL_ORDER_STATUS_LOOKUP,
            crate::slices::grounding::TOOL_PRODUCT_LOOKUP,
            crate::slices::grounding::TOOL_PRIOR_CONVERSATION_LOOKUP,
            crate::slices::grounding::TOOL_CUSTOMER_INVOICE_HISTORY,
            crate::slices::grounding::TOOL_CALL_TRANSCRIPT_LOOKUP,
        ]
    );
    assert!(!tools.contains(&crate::slices::grounding::TOOL_RESOLVE_PARTY));
}

#[test]
fn tool_loop_agentic_records_evidence_and_stages_final_proposal() {
    let _guard = service::test_packet_proposal_lock();
    service::reset_test_packet_proposal_state();
    service::set_test_packet_proposal_execution_mode(PacketProposalExecutionMode::ToolLoopAgentic);
    service::set_test_packet_proposal_tool_loop_enabled(true);
    let state = setup_email(
        "msg_tool_loop_success",
        "billing",
        policy(vec!["email_draft_reply"], vec![]),
    );
    service::set_test_packet_proposal_tool_loop_turns(vec![
        DirectLlmToolTurnResponse::ToolCalls {
            provider_id: "test".to_string(),
            model: "test-model".to_string(),
            tool_calls: vec![DirectLlmToolCall {
                id: "call-email".to_string(),
                name: "email_thread_lookup".to_string(),
                arguments: json!({ "scope": "thread" }),
            }],
            usage: None,
            finish_reason: Some("tool_calls".to_string()),
            latency_ms: 1,
            provider_request_id: Some("turn-1".to_string()),
        },
        DirectLlmToolTurnResponse::Final(test_tool_loop_envelope(
            "msg_tool_loop_success",
            json!({
                "suggested_category": null,
                "rationale": "The sender expects a reply.",
                "outcomes": [{
                    "packet_kind": "email_draft_reply",
                    "status": "drafted",
                    "draft": {
                        "body_text": "Thanks for reaching out. Could you send the haul-out date?",
                        "confidence": "high",
                        "provenance": [{ "field": "body_text", "quote": "Need a quote" }]
                    }
                }]
            }),
        )),
    ]);

    let response = service::run_smart_draft(
        state.clone(),
        input("msg_tool_loop_success", "key-tool-success"),
    )
    .expect("tool-loop smart draft");

    assert_eq!(response.run.status, PacketProposalRunStatus::Completed);
    assert_eq!(
        response.run.execution_mode,
        PacketProposalExecutionMode::ToolLoopAgentic
    );
    assert_eq!(
        response.run.outcomes[0].status,
        PacketProposalKindOutcomeStatus::Drafted
    );
    assert_eq!(service::take_test_packet_proposal_requests().len(), 0);
    let persistence = state.persistence.lock();
    let evidence =
        store::evidence_for_run(persistence.connection_ref(), CLIENT, &response.run.run_id)
            .expect("evidence");
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].tool_name, "email_thread_lookup");
    assert!(evidence[0]
        .result_excerpt
        .contains("Need a quote for a storefront repair job."));
    let item_id = response.item.expect("item").item.item_id;
    let staged = crate::produce::staged_draft_kinds_by_item(persistence.connection_ref(), CLIENT)
        .expect("staged kinds");
    assert_eq!(
        staged.get(&item_id),
        Some(&vec!["email_draft_reply".to_string()])
    );
    service::clear_test_packet_proposal_execution_mode();
    service::clear_test_packet_proposal_tool_loop_enabled();
    service::clear_test_packet_proposal_tool_loop_turns();
}

#[test]
fn tool_loop_agentic_records_denied_tool_call_evidence() {
    let _guard = service::test_packet_proposal_lock();
    service::reset_test_packet_proposal_state();
    service::set_test_packet_proposal_execution_mode(PacketProposalExecutionMode::ToolLoopAgentic);
    service::set_test_packet_proposal_tool_loop_enabled(true);
    let state = setup_email(
        "msg_tool_loop_denied",
        "billing",
        policy(vec!["email_draft_reply"], vec![]),
    );
    service::set_test_packet_proposal_tool_loop_turns(vec![
        DirectLlmToolTurnResponse::ToolCalls {
            provider_id: "test".to_string(),
            model: "test-model".to_string(),
            tool_calls: vec![DirectLlmToolCall {
                id: "call-wrong-source".to_string(),
                name: "email_thread_lookup".to_string(),
                arguments: json!({
                    "scope": "source",
                    "source_ref": "msg_other_customer"
                }),
            }],
            usage: None,
            finish_reason: Some("tool_calls".to_string()),
            latency_ms: 1,
            provider_request_id: Some("turn-1".to_string()),
        },
        DirectLlmToolTurnResponse::Final(test_tool_loop_envelope(
            "msg_tool_loop_denied",
            json!({
                "suggested_category": null,
                "rationale": "The requested source was out of scope.",
                "outcomes": [{
                    "packet_kind": "email_draft_reply",
                    "status": "unavailable",
                    "reason_code": "context_unavailable"
                }]
            }),
        )),
    ]);

    let response = service::run_smart_draft(
        state.clone(),
        input("msg_tool_loop_denied", "key-tool-denied"),
    )
    .expect("tool-loop smart draft");

    let persistence = state.persistence.lock();
    let evidence =
        store::evidence_for_run(persistence.connection_ref(), CLIENT, &response.run.run_id)
            .expect("evidence");
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].result_ref, "tool_denied");
    assert_eq!(
        evidence[0].result_excerpt,
        "packet_proposal_tool_source_out_of_scope"
    );
    assert!(evidence[0].tool_args_json.contains("msg_other_customer"));
    service::clear_test_packet_proposal_execution_mode();
    service::clear_test_packet_proposal_tool_loop_enabled();
    service::clear_test_packet_proposal_tool_loop_turns();
}

#[test]
fn tool_loop_agentic_denies_cache_lookup_for_identity_outside_source() {
    let _guard = service::test_packet_proposal_lock();
    service::reset_test_packet_proposal_state();
    service::set_test_packet_proposal_execution_mode(PacketProposalExecutionMode::ToolLoopAgentic);
    service::set_test_packet_proposal_tool_loop_enabled(true);
    let state = setup_email(
        "msg_tool_loop_out_of_scope_identity",
        "billing",
        policy(vec!["email_draft_reply"], vec![]),
    );
    service::set_test_packet_proposal_tool_loop_turns(vec![
        DirectLlmToolTurnResponse::ToolCalls {
            provider_id: "test".to_string(),
            model: "test-model".to_string(),
            tool_calls: vec![DirectLlmToolCall {
                id: "call-other-crm".to_string(),
                name: crate::slices::grounding::TOOL_CRM_CONTACT_LOOKUP.to_string(),
                arguments: json!({
                    "email": "other-customer@example.com"
                }),
            }],
            usage: None,
            finish_reason: Some("tool_calls".to_string()),
            latency_ms: 1,
            provider_request_id: Some("turn-1".to_string()),
        },
        DirectLlmToolTurnResponse::Final(test_tool_loop_envelope(
            "msg_tool_loop_out_of_scope_identity",
            json!({
                "suggested_category": null,
                "rationale": "The requested CRM identity was out of scope.",
                "outcomes": [{
                    "packet_kind": "email_draft_reply",
                    "status": "unavailable",
                    "reason_code": "context_unavailable"
                }]
            }),
        )),
    ]);

    let response = service::run_smart_draft(
        state.clone(),
        input(
            "msg_tool_loop_out_of_scope_identity",
            "key-tool-out-of-scope-identity",
        ),
    )
    .expect("tool-loop smart draft");

    let persistence = state.persistence.lock();
    let evidence =
        store::evidence_for_run(persistence.connection_ref(), CLIENT, &response.run.run_id)
            .expect("evidence");
    assert_eq!(evidence.len(), 1);
    assert_eq!(
        evidence[0].tool_name,
        crate::slices::grounding::TOOL_CRM_CONTACT_LOOKUP
    );
    assert_eq!(evidence[0].result_ref, "tool_denied");
    assert_eq!(
        evidence[0].result_excerpt,
        "packet_proposal_tool_query_out_of_scope"
    );
    assert!(evidence[0]
        .tool_args_json
        .contains("other-customer@example.com"));
    service::clear_test_packet_proposal_execution_mode();
    service::clear_test_packet_proposal_tool_loop_enabled();
    service::clear_test_packet_proposal_tool_loop_turns();
}

#[test]
fn tool_loop_agentic_invalid_final_output_records_failed_usage() {
    let _guard = service::test_packet_proposal_lock();
    service::reset_test_packet_proposal_state();
    service::set_test_packet_proposal_execution_mode(PacketProposalExecutionMode::ToolLoopAgentic);
    service::set_test_packet_proposal_tool_loop_enabled(true);
    let state = setup_email(
        "msg_tool_loop_invalid_final",
        "billing",
        policy(vec!["email_draft_reply"], vec![]),
    );
    service::set_test_packet_proposal_tool_loop_turns(vec![DirectLlmToolTurnResponse::Final(
        test_tool_loop_envelope("msg_tool_loop_invalid_final", json!(["not-an-object"])),
    )]);

    let err = service::run_smart_draft(
        state.clone(),
        input("msg_tool_loop_invalid_final", "key-tool-invalid-final"),
    )
    .expect_err("invalid final output should fail");

    match err {
        service::SmartDraftError::Llm(code) => assert_eq!(code, "llm_output_not_object"),
        other => panic!("unexpected error: {other:?}"),
    }
    let persistence = state.persistence.lock();
    let rows = crate::slices::ai_usage::store::list_recent(persistence.connection_ref(), CLIENT, 4)
        .expect("usage rows");
    let usage = rows
        .iter()
        .find(|row| row.correlation_id == "msg_tool_loop_invalid_final")
        .expect("invalid final usage row");
    assert!(!usage.success);
    assert_eq!(usage.error_code.as_deref(), Some("llm_output_not_object"));
    service::clear_test_packet_proposal_execution_mode();
    service::clear_test_packet_proposal_tool_loop_enabled();
    service::clear_test_packet_proposal_tool_loop_turns();
}

#[test]
fn tool_loop_agentic_caps_exhaustion_fails_without_partial_draft() {
    let _guard = service::test_packet_proposal_lock();
    service::reset_test_packet_proposal_state();
    service::set_test_packet_proposal_execution_mode(PacketProposalExecutionMode::ToolLoopAgentic);
    service::set_test_packet_proposal_tool_loop_enabled(true);
    service::set_test_packet_proposal_tool_loop_limits(
        crate::slices::ai_usage::service::ToolLoopLimits {
            max_turns: 1,
            max_tool_calls: 4,
            max_evidence_bytes: 24 * 1024,
            wall_clock_ms: 180_000,
        },
    );
    let state = setup_email(
        "msg_tool_loop_exhausted",
        "billing",
        policy(vec!["email_draft_reply"], vec![]),
    );
    service::set_test_packet_proposal_tool_loop_turns(vec![DirectLlmToolTurnResponse::ToolCalls {
        provider_id: "test".to_string(),
        model: "test-model".to_string(),
        tool_calls: vec![DirectLlmToolCall {
            id: "call-email".to_string(),
            name: "email_thread_lookup".to_string(),
            arguments: json!({ "scope": "source" }),
        }],
        usage: None,
        finish_reason: Some("tool_calls".to_string()),
        latency_ms: 1,
        provider_request_id: Some("turn-1".to_string()),
    }]);

    let err = service::run_smart_draft(
        state.clone(),
        input("msg_tool_loop_exhausted", "key-tool-exhausted"),
    )
    .expect_err("tool loop should exhaust");

    match err {
        service::SmartDraftError::Llm(code) => {
            assert_eq!(code, service::TOOL_LOOP_EXHAUSTED_ERROR_CODE)
        }
        other => panic!("unexpected error: {other:?}"),
    }
    let run_id = service::test_smart_draft_run_id(
        crate::slices::work_queue::SOURCE_KIND_EMAIL,
        "msg_tool_loop_exhausted",
        "key-tool-exhausted",
    );
    let persistence = state.persistence.lock();
    let run = store::get_run(persistence.connection_ref(), CLIENT, &run_id)
        .expect("run")
        .expect("run row");
    assert_eq!(run.status, PacketProposalRunStatus::Failed);
    assert_eq!(
        run.error_code.as_deref(),
        Some(service::TOOL_LOOP_EXHAUSTED_ERROR_CODE)
    );
    let staged = crate::produce::staged_draft_kinds_by_item(persistence.connection_ref(), CLIENT)
        .expect("staged kinds");
    assert!(staged.is_empty());
    service::clear_test_packet_proposal_execution_mode();
    service::clear_test_packet_proposal_tool_loop_enabled();
    service::clear_test_packet_proposal_tool_loop_turns();
    service::clear_test_packet_proposal_tool_loop_limits();
}

#[test]
fn shared_background_dedupe_requires_identical_blocks() {
    let matching = vec![
        prepared_kind("follow_up_task", "Company background"),
        prepared_kind("email_draft_reply", "Company background"),
    ];
    assert!(service::shared_background_for_prompt(&matching).is_some());

    let mismatched = vec![
        prepared_kind("follow_up_task", "Company background"),
        prepared_kind("email_draft_reply", "Different background"),
    ];
    assert!(service::shared_background_for_prompt(&mismatched).is_none());
}

#[tokio::test]
// The guard serializes the fake LLM response/request globals across the async route call.
#[allow(clippy::await_holding_lock)]
async fn smart_draft_route_accepts_and_stages_draft() {
    let _guard = service::test_packet_proposal_lock();
    service::reset_test_packet_proposal_state();
    let gate = service::TestPacketProposalLlmGate::new();
    service::set_test_packet_proposal_llm_gate(gate.clone());
    let state = setup_email(
        "msg_route_success",
        "billing",
        policy(vec!["email_draft_reply"], vec![]),
    );
    service::set_test_packet_proposal_response(json!({
        "suggested_category": null,
        "rationale": "The sender expects a reply.",
        "outcomes": [{
            "packet_kind": "email_draft_reply",
            "status": "drafted",
            "draft": {
                "body_text": "Thanks for reaching out. Could you send the haul-out date?",
                "confidence": "high",
                "provenance": [{ "field": "body_text", "quote": "Need a quote" }]
            }
        }]
    }));
    let router = crate::http::build_router(state.clone());

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/packet-proposals/smart-draft")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "source_kind": crate::slices::work_queue::SOURCE_KIND_EMAIL,
                        "source_ref": "msg_route_success",
                        "idempotency_key": "route-key-success",
                        "expected_revision": null,
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
    let response: SmartDraftResponse = serde_json::from_slice(&bytes).expect("smart draft json");
    assert_eq!(response.run.status, PacketProposalRunStatus::Running);
    assert_eq!(
        response.run.resolved_decision_mode,
        PacketProposalDecisionMode::AiDecides
    );
    assert!(response
        .run
        .candidate_packet_kinds
        .contains(&"follow_up_task".to_string()));
    assert!(response
        .run
        .candidate_packet_kinds
        .contains(&"email_draft_reply".to_string()));
    assert!(response.run.outcomes.is_empty());

    gate.release();
    let mut completed = None;
    // Deadline instead of a fixed poll count: a saturated parallel test run
    // can delay the background completion well past 20 * 25ms.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        let poll_response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/packet-proposals/smart-draft/source-state")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "source_kind": crate::slices::work_queue::SOURCE_KIND_EMAIL,
                            "source_ref": "msg_route_success",
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("poll response");
        let poll_status = poll_response.status();
        let bytes = poll_response
            .into_body()
            .collect()
            .await
            .expect("poll body")
            .to_bytes();
        if poll_status == StatusCode::INTERNAL_SERVER_ERROR
            && String::from_utf8_lossy(&bytes).contains("storage_failure")
        {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            continue;
        }
        assert_eq!(
            poll_status,
            StatusCode::OK,
            "poll failed: {}",
            String::from_utf8_lossy(&bytes)
        );
        let state: SmartDraftSourceStateResponse =
            serde_json::from_slice(&bytes).expect("source state json");
        if state
            .run
            .as_ref()
            .is_some_and(|run| run.status == PacketProposalRunStatus::Completed)
        {
            completed = state.run;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let run = completed.expect("completed run");
    let email_outcome = run
        .outcomes
        .iter()
        .find(|outcome| outcome.packet_kind == "email_draft_reply")
        .expect("email outcome");
    assert_eq!(
        email_outcome.status,
        PacketProposalKindOutcomeStatus::Drafted
    );
    assert!(email_outcome.draft_id.is_some());

    let persistence = state.persistence.lock();
    let staged = crate::produce::staged_draft_kinds_by_item(persistence.connection_ref(), CLIENT)
        .expect("staged kinds");
    assert!(staged
        .values()
        .any(|kinds| kinds == &vec!["email_draft_reply".to_string()]));
    service::clear_test_packet_proposal_llm_gate();
}

#[tokio::test]
async fn smart_draft_source_state_route_returns_existing_open_item_revision() {
    let state = setup_email(
        "msg_source_state",
        "billing",
        policy(vec!["email_draft_reply"], vec![]),
    );
    let expected_revision = insert_open_item(&state, "msg_source_state");
    let router = crate::http::build_router(state);

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/packet-proposals/smart-draft/source-state")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "source_kind": crate::slices::work_queue::SOURCE_KIND_EMAIL,
                        "source_ref": "msg_source_state",
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
    let response: SmartDraftSourceStateResponse =
        serde_json::from_slice(&bytes).expect("source state json");
    assert_eq!(response.expected_revision, Some(expected_revision));
    assert_eq!(
        response.item.expect("item").item.source_ref,
        "msg_source_state"
    );
}

#[tokio::test]
async fn smart_draft_source_state_route_can_poll_specific_run_for_source() {
    let state = setup_email(
        "msg_source_state_run_id",
        "billing",
        policy(vec!["email_draft_reply"], vec![]),
    );
    let requested_run_id = seed_running_run(&state, "msg_source_state_run_id", "key-first", 1_100);
    let latest_run_id = seed_running_run(&state, "msg_source_state_run_id", "key-second", 1_200);
    let router = crate::http::build_router(state);

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/packet-proposals/smart-draft/source-state")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "source_kind": crate::slices::work_queue::SOURCE_KIND_EMAIL,
                        "source_ref": "msg_source_state_run_id",
                        "run_id": requested_run_id,
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
    let response: SmartDraftSourceStateResponse =
        serde_json::from_slice(&bytes).expect("source state json");
    let run = response.run.expect("run");
    assert_eq!(run.run_id, requested_run_id);
    assert_ne!(run.run_id, latest_run_id);
}

fn prepared_kind(packet_kind: &'static str, background: &str) -> PreparedProposalKind {
    PreparedProposalKind {
        packet_kind: packet_kind.to_string(),
        contract: ProposalContract {
            packet_kind,
            schema_ref: "test.schema",
            response_key: "draft",
            instructions: "Test packet instructions.",
        },
        context: json!({
            "background": {
                "block_id": "background",
                "text": background,
            },
            "facts": { "source": "test" },
        }),
        attempt: 0,
    }
}

fn test_tool_loop_envelope(
    source_ref: &str,
    mut response_json: serde_json::Value,
) -> TypedLlmTaskOutputEnvelope {
    if let Some(object) = response_json.as_object_mut() {
        object
            .entry("confidence".to_string())
            .or_insert_with(|| json!("high"));
    }
    TypedLlmTaskOutputEnvelope {
        task_id: format!("packet_proposal_test_{source_ref}"),
        execution_route: TypedLlmExecutionRoute::DirectApi,
        provider_id: "test".to_string(),
        model: "test-model".to_string(),
        schema_ref: service::PROPOSAL_SCHEMA_REF.to_string(),
        raw_response_hash: "test".to_string(),
        response_json,
        usage: None,
        finish_reason: Some("stop".to_string()),
        latency_ms: 1,
        retry_count: 0,
        provider_request_id: Some("final".to_string()),
        correlation_id: source_ref.to_string(),
    }
}

fn setup_email(
    source_ref: &str,
    category_id: &str,
    policy: WorkQueuePolicy,
) -> crate::http::AppState {
    let state = crate::http::test_support::test_state();
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    crate::slices::email_triage::store::upsert_category(
        conn,
        CLIENT,
        "op_test",
        &CategoryRecord {
            category_id: category_id.to_string(),
            display_name: "Billing".to_string(),
            description: "Billing and quote requests.".to_string(),
            color: "#38bdf8".to_string(),
            sort: 10,
            is_system: false,
            default_agent_dir: String::new(),
            default_agent_context: String::new(),
        },
        &format!("category:{category_id}"),
        1_000,
    )
    .expect("category");
    crate::slices::work_queue::store::upsert_policy(
        conn,
        CLIENT,
        "op_test",
        &policy,
        &format!("policy:{category_id}"),
        1_001,
    )
    .expect("policy");
    crate::slices::client_profile::store::upsert_profile(
        conn,
        CLIENT,
        "op_test",
        &bos_contracts::client_profile::ClientProfile {
            client_id: CLIENT.to_string(),
            company_name: Some("Example Company".to_string()),
            bio: Some("appliance repair and maintenance service.".to_string()),
            industry: Some("repair services".to_string()),
            website: None,
            persona: Some("Concise and practical".to_string()),
        },
        &format!("profile:{source_ref}"),
        1_002,
    )
    .expect("profile");
    crate::slices::email_triage::store::record_inbound_message(
        conn,
        CLIENT,
        &message(source_ref, category_id),
    )
    .expect("message");
    drop(persistence);
    state
}

fn seed_running_run(
    state: &crate::http::AppState,
    source_ref: &str,
    idempotency_key: &str,
    now_ms: u64,
) -> String {
    let run_id = service::test_smart_draft_run_id(
        crate::slices::work_queue::SOURCE_KIND_EMAIL,
        source_ref,
        idempotency_key,
    );
    let candidate_packet_kinds = vec!["email_draft_reply".to_string()];
    let mut persistence = state.persistence.lock();
    store::insert_run(
        persistence.connection(),
        CLIENT,
        NewRun {
            run_id: &run_id,
            source_kind: crate::slices::work_queue::SOURCE_KIND_EMAIL,
            source_ref,
            item_id: None,
            resolved_decision_mode: PacketProposalDecisionMode::FillFixed,
            execution_mode: PacketProposalExecutionMode::BoundedTyped,
            candidate_packet_kinds: &candidate_packet_kinds,
            idempotency_key: &format!("{idempotency_key}:run"),
            actor_id: "op_test",
            actor_kind: ActorKindDto::Operator,
            now_ms,
        },
    )
    .expect("seed running run");
    run_id
}

fn policy(packet_kinds: Vec<&str>, ai_suggestible: Vec<&str>) -> WorkQueuePolicy {
    WorkQueuePolicy {
        category_id: "billing".to_string(),
        create_work_item: true,
        packet_kinds: packet_kinds.into_iter().map(str::to_string).collect(),
        ai_suggestible_packet_kinds: ai_suggestible.into_iter().map(str::to_string).collect(),
        ai_suggestible_gmail_scope: Default::default(),
        ai_suggestible_gmail_categories: Vec::new(),
        auto_produce: false,
    }
}

fn message(source_ref: &str, category_id: &str) -> InboundMessageRecord {
    InboundMessageRecord {
        source_key: source_ref.to_string(),
        message_id: source_ref.to_string(),
        thread_id: Some(format!("thread_{source_ref}")),
        internal_date_ms: Some(1_700_000_000_000),
        from_addr: Some("customer@example.com".to_string()),
        to_addr: Some("ops@example.com".to_string()),
        subject: Some("Need a quote".to_string()),
        body_excerpt: "Need a quote for a storefront repair job.".to_string(),
        body_full: "Need a quote for a storefront repair job.".to_string(),
        headers: Vec::new(),
        labels: Vec::new(),
        resolved_category: category_id.to_string(),
        matched_rule_id: None,
        ingested_at_ms: 1_003,
        ai_triage_status: None,
        ai_triage_rationale: None,
        attachments: Vec::new(),
        source_user_id: None,
    }
}

fn insert_open_item(state: &crate::http::AppState, source_ref: &str) -> u64 {
    let mut persistence = state.persistence.lock();
    let item = WorkItem {
        item_id: format!("wi_email_{source_ref}"),
        source_kind: crate::slices::work_queue::SOURCE_KIND_EMAIL.to_string(),
        source_ref: source_ref.to_string(),
        category_id: "billing".to_string(),
        title: "Need a quote".to_string(),
        summary: "Need a quote for a storefront repair job.".to_string(),
        packet_kinds: vec!["email_draft_reply".to_string()],
        status: WorkItemStatus::Open,
        accept_actor: None,
        ai_suggested: false,
        rationale: String::new(),
        produce_guidance: String::new(),
        source_user_id: None,
        assignee_user_id: None,
        visible_to_user_ids: Vec::new(),
        created_at_ms: 1_100,
        updated_at_ms: 1_100,
    };
    crate::slices::work_queue::store::insert_item(persistence.connection(), CLIENT, &item)
        .expect("insert open item");
    crate::slices::work_queue::store::get_item_for_source(
        persistence.connection_ref(),
        CLIENT,
        &item.source_kind,
        &item.source_ref,
    )
    .expect("load inserted item")
    .expect("inserted item")
    .revision
}

fn input(source_ref: &str, idempotency_key: &str) -> SmartDraftInput {
    SmartDraftInput {
        source_kind: crate::slices::work_queue::SOURCE_KIND_EMAIL.to_string(),
        source_ref: source_ref.to_string(),
        idempotency_key: idempotency_key.to_string(),
        expected_revision: None,
        min_confidence: None,
        candidate_mode: SmartDraftCandidateMode::Policy,
        actor_id: "op_test".to_string(),
        scope: crate::http::OperatorScope::All,
    }
}
