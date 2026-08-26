use bos_contracts::email_triage::EmailAttachmentRecord;
use bos_contracts::quote_workflows::{
    QuoteDraftActionKind, QuoteDraftStatus, QuoteGuardrailStatus, WorkflowRunStatus,
};
use bos_profile_api::{
    QuoteProfileDraft, QuoteProfileLineItem, QuoteProfileRun, QuoteProfileStep,
    QuoteProfileStepKind,
};

use super::profiles;
use super::service;
use super::store::{self, DraftActionContext, QuoteWorkflowInput, TraceStartContext};
use crate::outbox;
use crate::persistence::Persistence;
use crate::store_core::MutationOutcome;

const CLIENT: &str = "test-client";
const ACTOR: &str = "op_test";

fn input() -> QuoteWorkflowInput {
    QuoteWorkflowInput {
        source_kind: "operator_note".to_string(),
        source_ref: "note_1".to_string(),
        source_attachments: Vec::new(),
        customer_name: "Acme Co".to_string(),
        customer_tier: Some("standard".to_string()),
        request_text: "2 gallons primer at $25.00\n1 finish coat at $40".to_string(),
    }
}

fn profile_run_with_line(line: QuoteProfileLineItem) -> QuoteProfileRun {
    QuoteProfileRun {
        steps: vec![QuoteProfileStep {
            node: "stage_draft".to_string(),
            kind: QuoteProfileStepKind::Stage,
            inputs: Vec::new(),
            outputs: Vec::new(),
            decision: None,
        }],
        draft: QuoteProfileDraft {
            summary: "Quote for Acme Co".to_string(),
            line_items: vec![line],
            policy_notes: Vec::new(),
        },
    }
}

fn guardrail_config() -> serde_json::Value {
    serde_json::json!({
        "enabled": true,
        "routine_max_discount_bps": 1000,
        "major_change_threshold_bps": 1500,
        "major_change_approver_id": "jordan",
        "price_lists": [
            {
                "sku": "LINE-1",
                "customer_tier": "standard",
                "unit_cents": 2500
            },
            {
                "sku": "LINE-2",
                "customer_tier": "standard",
                "unit_cents": 4000
            }
        ]
    })
}

#[test]
fn profile_registry_default_is_built_in_and_unknown_is_refused() {
    let profile =
        profiles::select_profile(profiles::BUILT_IN_PROFILE_ID).expect("built-in profile");
    assert_eq!(profile.profile_id(), profiles::BUILT_IN_PROFILE_ID);
    assert!(profiles::select_profile("missing_profile").is_none());
}

#[test]
fn workflow_input_snapshot_includes_source_attachment_metadata() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let mut with_attachment = input();
    with_attachment.source_kind = "email".to_string();
    with_attachment.source_ref = "msg-1".to_string();
    with_attachment.source_attachments = vec![EmailAttachmentRecord {
        attachment_id: "att-1".to_string(),
        part_id: Some("1".to_string()),
        filename: "plans.pdf".to_string(),
        mime_type: Some("application/pdf".to_string()),
        size_bytes: Some(2048),
        inline: false,
        content_id: None,
    }];
    let response = service::run_quote_builder(
        persistence.connection(),
        with_attachment,
        service::QuoteRunContext {
            client_id: CLIENT,
            actor_id: ACTOR,
            profile_id: profiles::BUILT_IN_PROFILE_ID,
            profile_config_json: serde_json::Value::Null,
            guardrail_config_json: guardrail_config(),
            request_idempotency_key: "idem_attachment_snapshot",
            now_ms: 100,
        },
    )
    .expect("run");
    let attachments = response
        .run
        .input_snapshot_json
        .get("source_attachments")
        .and_then(|value| value.as_array())
        .expect("attachments array");
    assert_eq!(attachments.len(), 1);
    assert_eq!(
        attachments[0]
            .get("filename")
            .and_then(|value| value.as_str()),
        Some("plans.pdf")
    );
}

#[test]
fn host_refuses_ungrounded_profile_line_items() {
    let run = profile_run_with_line(QuoteProfileLineItem {
        sku: "LINE-1".to_string(),
        product_line: None,
        description: "Not in the source".to_string(),
        quantity: 1,
        unit_cents: 1_000,
        total_cents: 1_000,
        source_quote: "Not in the source".to_string(),
    });
    let err = service::validate_profile_run(&input(), &run).expect_err("ungrounded line refused");
    assert!(format!("{err:?}").contains("quote_line_not_grounded"));
}

#[test]
fn host_refuses_profile_line_total_mismatch() {
    let run = profile_run_with_line(QuoteProfileLineItem {
        sku: "LINE-1".to_string(),
        product_line: None,
        description: "2 gallons primer at $25.00".to_string(),
        quantity: 2,
        unit_cents: 2_500,
        total_cents: 4_000,
        source_quote: "2 gallons primer at $25.00".to_string(),
    });
    let err = service::validate_profile_run(&input(), &run).expect_err("bad total refused");
    assert!(format!("{err:?}").contains("quote_line_total_mismatch"));
}

#[test]
fn profile_failures_are_recorded_as_failed_runs() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let bad_input = QuoteWorkflowInput {
        request_text: String::new(),
        ..input()
    };
    let run_id = service::quote_run_id("idem_profile_failure");
    let started = service::start_quote_builder(
        persistence.connection(),
        bad_input,
        service::QuoteRunContext {
            client_id: CLIENT,
            actor_id: ACTOR,
            profile_id: profiles::BUILT_IN_PROFILE_ID,
            profile_config_json: serde_json::Value::Null,
            guardrail_config_json: serde_json::Value::Null,
            request_idempotency_key: "idem_profile_failure",
            now_ms: 700,
        },
    )
    .expect("start");
    let permit = service::try_acquire_quote_run().expect("permit");
    let (started, err) = match service::prepare_quote_builder(started, permit) {
        Ok(_) => panic!("profile failure expected"),
        Err(failure) => *failure,
    };
    service::fail_started_quote_builder(persistence.connection(), &started, &err, 701)
        .expect("finish failed");

    let run = store::get_run(persistence.connection_ref(), CLIENT, &run_id)
        .expect("load run")
        .expect("run");
    assert_eq!(run.status, WorkflowRunStatus::Failed);
    assert_eq!(
        run.terminal_json
            .as_ref()
            .and_then(|value| value.get("error_code"))
            .and_then(|value| value.as_str()),
        Some("quote_request_text_required")
    );
    let receipts = crate::store_core::receipts_by_correlation(
        persistence.connection_ref(),
        CLIENT,
        &[run_id],
        20,
    )
    .expect("receipts");
    assert!(receipts
        .iter()
        .any(|receipt| receipt.change_kind == "finish"
            && receipt.entity_kind == store::RUN_ENTITY_KIND));
}

#[test]
fn idempotent_replay_returns_existing_run_before_profile_selection() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let first = service::run_quote_builder(
        persistence.connection(),
        input(),
        service::QuoteRunContext {
            client_id: CLIENT,
            actor_id: ACTOR,
            profile_id: profiles::BUILT_IN_PROFILE_ID,
            profile_config_json: serde_json::Value::Null,
            guardrail_config_json: serde_json::Value::Null,
            request_idempotency_key: "idem_replay_without_profile",
            now_ms: 800,
        },
    )
    .expect("first run");

    let replay = service::run_quote_builder(
        persistence.connection(),
        QuoteWorkflowInput {
            request_text: String::new(),
            ..input()
        },
        service::QuoteRunContext {
            client_id: CLIENT,
            actor_id: ACTOR,
            profile_id: "missing_profile",
            profile_config_json: serde_json::json!({"unexpected": true}),
            guardrail_config_json: serde_json::Value::Null,
            request_idempotency_key: "idem_replay_without_profile",
            now_ms: 900,
        },
    )
    .expect("replay");

    assert_eq!(replay.run.run_id, first.run.run_id);
    assert_eq!(replay.run.status, WorkflowRunStatus::Staged);
    assert!(replay.draft.is_some());
}

#[test]
fn workflow_persists_ordered_trace_with_causation_and_stage_entity_split() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let response = service::run_quote_builder(
        persistence.connection(),
        input(),
        service::QuoteRunContext {
            client_id: CLIENT,
            actor_id: ACTOR,
            profile_id: profiles::BUILT_IN_PROFILE_ID,
            profile_config_json: serde_json::Value::Null,
            guardrail_config_json: serde_json::Value::Null,
            request_idempotency_key: "idem_quote_run",
            now_ms: 1_000,
        },
    )
    .expect("run");
    let run_id = response.run.run_id;
    assert_eq!(response.run.profile_id, profiles::BUILT_IN_PROFILE_ID);
    let conn = persistence.connection_ref();
    let steps = store::steps_for_run(conn, CLIENT, &run_id).expect("steps");
    assert_eq!(
        steps
            .iter()
            .map(|step| step.node.as_str())
            .collect::<Vec<_>>(),
        vec![
            "gather_source",
            "parse_request",
            "validate_grounding",
            "policy",
            "stage_draft"
        ]
    );
    let parse = steps
        .iter()
        .find(|step| step.node == "parse_request")
        .expect("parse step");
    assert_eq!(
        parse
            .outputs
            .iter()
            .find(|value| value.label == "line_item_count")
            .and_then(|value| value.value.as_u64()),
        Some(2)
    );
    let policy = steps
        .iter()
        .find(|step| step.node == "policy")
        .expect("policy step");
    assert!(policy
        .inputs
        .iter()
        .any(|value| value.label == "subtotal" && value.unit.as_deref() == Some("cents")));

    let receipts =
        crate::store_core::receipts_by_correlation(conn, CLIENT, std::slice::from_ref(&run_id), 20)
            .expect("receipts");
    let stage_receipt = receipts
        .iter()
        .find(|receipt| receipt.change_kind == "stage")
        .expect("stage receipt");
    assert_eq!(stage_receipt.entity_kind, store::DRAFT_ENTITY_KIND);
    assert_eq!(
        stage_receipt.receipt_id,
        steps
            .iter()
            .find(|step| step.node == "stage_draft")
            .expect("stage step")
            .receipt_id
    );

    let mut ordered = receipts.clone();
    ordered.sort_by_key(|receipt| receipt.receipt_id.clone());
    let start = ordered
        .iter()
        .find(|receipt| receipt.change_kind == "start")
        .expect("start");
    let first_step = ordered
        .iter()
        .find(|receipt| receipt.change_kind == "step")
        .expect("step");
    assert_eq!(
        first_step.causation_id.as_deref(),
        Some(start.receipt_id.as_str())
    );
}

#[test]
fn quote_guardrails_flag_out_of_policy_pricing_and_snapshot_config() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let response = service::run_quote_builder(
        persistence.connection(),
        QuoteWorkflowInput {
            request_text: "2 gallons primer at $20.00".to_string(),
            ..input()
        },
        service::QuoteRunContext {
            client_id: CLIENT,
            actor_id: ACTOR,
            profile_id: profiles::BUILT_IN_PROFILE_ID,
            profile_config_json: serde_json::Value::Null,
            guardrail_config_json: guardrail_config(),
            request_idempotency_key: "idem_guardrail_flag",
            now_ms: 2_500,
        },
    )
    .expect("run");
    let draft = response.draft.expect("draft").draft;
    assert_eq!(draft.guardrails.status, QuoteGuardrailStatus::NeedsApproval);
    assert!(draft
        .guardrails
        .findings
        .iter()
        .any(|finding| finding.code == "quote_major_price_change"
            && finding.required_approver_id.as_deref() == Some("jordan")));
    assert_eq!(draft.guardrails.approval_routes[0].approver_id, "jordan");
    assert_eq!(
        draft
            .guardrails
            .config_snapshot_json
            .get("routine_max_discount_bps")
            .and_then(|value| value.as_u64()),
        Some(1000)
    );

    let receipts = crate::store_core::receipts_by_correlation(
        persistence.connection_ref(),
        CLIENT,
        std::slice::from_ref(&draft.run_id),
        20,
    )
    .expect("receipts");
    let stage = receipts
        .iter()
        .find(|receipt| receipt.change_kind == "stage")
        .expect("stage receipt");
    assert_eq!(stage.entity_kind, store::DRAFT_ENTITY_KIND);
}

#[test]
fn quote_guardrails_allow_routine_prices_without_escalation() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let response = service::run_quote_builder(
        persistence.connection(),
        input(),
        service::QuoteRunContext {
            client_id: CLIENT,
            actor_id: ACTOR,
            profile_id: profiles::BUILT_IN_PROFILE_ID,
            profile_config_json: serde_json::Value::Null,
            guardrail_config_json: guardrail_config(),
            request_idempotency_key: "idem_guardrail_clean",
            now_ms: 2_700,
        },
    )
    .expect("run");
    let draft = response.draft.expect("draft").draft;
    assert_eq!(
        draft.guardrails.status,
        QuoteGuardrailStatus::WithinGuardrails
    );
    assert!(draft.guardrails.approval_routes.is_empty());
}

#[test]
fn quote_guardrails_prefer_specific_price_over_wildcard_fallback() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let response = service::run_quote_builder(
        persistence.connection(),
        QuoteWorkflowInput {
            request_text: "2 gallons primer at $25.00".to_string(),
            ..input()
        },
        service::QuoteRunContext {
            client_id: CLIENT,
            actor_id: ACTOR,
            profile_id: profiles::BUILT_IN_PROFILE_ID,
            profile_config_json: serde_json::Value::Null,
            guardrail_config_json: serde_json::json!({
                "enabled": true,
                "routine_max_discount_bps": 1000,
                "major_change_threshold_bps": 1500,
                "major_change_approver_id": "jordan",
                "price_lists": [
                    {
                        "sku": "LINE-1",
                        "unit_cents": 2500
                    },
                    {
                        "sku": "LINE-1",
                        "customer_tier": "standard",
                        "unit_cents": 3000
                    }
                ]
            }),
            request_idempotency_key: "idem_guardrail_specificity",
            now_ms: 2_800,
        },
    )
    .expect("run");
    let draft = response.draft.expect("draft").draft;
    assert_eq!(draft.guardrails.status, QuoteGuardrailStatus::NeedsApproval);
    assert!(draft.guardrails.findings.iter().any(|finding| {
        finding.code == "quote_discount_exceeds_routine_max"
            && finding.list_unit_cents == Some(3_000)
            && finding.quoted_unit_cents == Some(2_500)
    }));
}

#[test]
fn quote_guardrail_escalations_require_configured_approver() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let response = service::run_quote_builder(
        persistence.connection(),
        QuoteWorkflowInput {
            request_text: "2 gallons primer at $20.00".to_string(),
            ..input()
        },
        service::QuoteRunContext {
            client_id: CLIENT,
            actor_id: ACTOR,
            profile_id: profiles::BUILT_IN_PROFILE_ID,
            profile_config_json: serde_json::Value::Null,
            guardrail_config_json: guardrail_config(),
            request_idempotency_key: "idem_guardrail_approval",
            now_ms: 2_900,
        },
    )
    .expect("run");
    let draft = response.draft.expect("draft").draft;
    let err = service::apply_draft_action(
        persistence.connection(),
        DraftActionContext {
            client_id: CLIENT,
            actor_id: ACTOR,
            expected_revision: Some(1),
            idempotency_key: "approve_guardrail_wrong_actor",
            now_ms: 3_000,
        },
        &draft.draft_id,
        QuoteDraftActionKind::Approve,
    )
    .expect_err("non-routed actor refused");
    assert!(format!("{err:?}").contains("quote_guardrail_approval_required"));

    service::apply_draft_action(
        persistence.connection(),
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "jordan",
            expected_revision: Some(1),
            idempotency_key: "approve_guardrail_jordan",
            now_ms: 3_001,
        },
        &draft.draft_id,
        QuoteDraftActionKind::Approve,
    )
    .expect("configured approver can approve");
}

#[test]
fn idempotent_replay_receipt_can_be_used_as_next_causation_edge() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let run_id = "qwr_replay";
    let mut trace = store::Trace::start(
        conn,
        TraceStartContext {
            client_id: CLIENT,
            actor_id: ACTOR,
            run_id,
            profile_id: profiles::BUILT_IN_PROFILE_ID,
            idempotency_key: "replay:start",
            now_ms: 2_000,
        },
        &input(),
    )
    .expect("trace");
    let first = trace
        .step(
            store::StepRecord {
                node: "first".to_string(),
                node_kind: "deterministic".to_string(),
                input_hash: None,
                output_hash: None,
                decision: None,
                inputs: Vec::new(),
                outputs: Vec::new(),
                llm_usage_json: None,
                latency_ms: 0,
                status: "succeeded".to_string(),
                error_code: None,
            },
            2_001,
        )
        .expect("first");
    drop(trace);

    let manual_replay = crate::store_core::mutate(
        conn,
        crate::store_core::MutationRequest {
            client_id: CLIENT,
            entity_kind: store::RUN_ENTITY_KIND,
            entity_id: run_id,
            change_kind: "step",
            actor_id: ACTOR,
            actor_kind: bos_contracts::receipt::ActorKindDto::Agent,
            expected_revision: None,
            idempotency_key: "qwr_replay:0",
            correlation_id: Some(run_id),
            causation_id: Some(&first),
            before_json: None,
            after_json: Some("{} ".trim().to_string()),
            now_ms: 2_002,
        },
        |_| Ok(()),
    )
    .expect("replay");
    let manual_replay_receipt = match manual_replay {
        MutationOutcome::ReplayedIdempotent { receipt_id, .. } => receipt_id,
        other => panic!("expected replay, got {other:?}"),
    };

    let mut trace = store::Trace::start(
        conn,
        TraceStartContext {
            client_id: CLIENT,
            actor_id: ACTOR,
            run_id,
            profile_id: profiles::BUILT_IN_PROFILE_ID,
            idempotency_key: "replay:start",
            now_ms: 2_003,
        },
        &input(),
    )
    .expect("trace replay");
    let replayed_step_receipt = trace
        .step(
            store::StepRecord {
                node: "first".to_string(),
                node_kind: "deterministic".to_string(),
                input_hash: None,
                output_hash: None,
                decision: None,
                inputs: Vec::new(),
                outputs: Vec::new(),
                llm_usage_json: None,
                latency_ms: 0,
                status: "succeeded".to_string(),
                error_code: None,
            },
            2_004,
        )
        .expect("replayed first");
    assert_ne!(replayed_step_receipt, first);
    trace
        .step(
            store::StepRecord {
                node: "second".to_string(),
                node_kind: "deterministic".to_string(),
                input_hash: None,
                output_hash: None,
                decision: None,
                inputs: Vec::new(),
                outputs: Vec::new(),
                llm_usage_json: None,
                latency_ms: 0,
                status: "succeeded".to_string(),
                error_code: None,
            },
            2_005,
        )
        .expect("second");
    drop(trace);

    let receipts =
        crate::store_core::receipts_by_correlation(conn, CLIENT, &[run_id.to_string()], 20)
            .expect("receipts");
    let second = receipts
        .iter()
        .find(|receipt| {
            receipt.change_kind == "step"
                && receipt.causation_id.as_deref() == Some(replayed_step_receipt.as_str())
        })
        .expect("second step caused by replayed step receipt");
    assert_eq!(second.entity_kind, store::RUN_ENTITY_KIND);
    assert!(receipts
        .iter()
        .any(|receipt| receipt.receipt_id == manual_replay_receipt));
}

#[test]
fn approval_stamps_run_id_on_receipt_and_outbox_job() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let response = service::run_quote_builder(
        persistence.connection(),
        input(),
        service::QuoteRunContext {
            client_id: CLIENT,
            actor_id: ACTOR,
            profile_id: profiles::BUILT_IN_PROFILE_ID,
            profile_config_json: serde_json::Value::Null,
            guardrail_config_json: serde_json::Value::Null,
            request_idempotency_key: "idem_approval",
            now_ms: 3_000,
        },
    )
    .expect("run");
    let draft = response.draft.expect("draft").draft;
    assert_eq!(draft.status, QuoteDraftStatus::Staged);
    let ctx = DraftActionContext {
        client_id: CLIENT,
        actor_id: ACTOR,
        expected_revision: Some(1),
        idempotency_key: "approve_quote",
        now_ms: 4_000,
    };
    service::apply_draft_action(
        persistence.connection(),
        ctx,
        &draft.draft_id,
        QuoteDraftActionKind::Approve,
    )
    .expect("approve");

    let ids = vec![draft.run_id.clone()];
    let receipts =
        crate::store_core::receipts_by_correlation(persistence.connection_ref(), CLIENT, &ids, 20)
            .expect("receipts");
    assert!(receipts
        .iter()
        .any(|receipt| receipt.change_kind == "approve"
            && receipt.entity_kind == store::DRAFT_ENTITY_KIND
            && receipt.correlation_id.as_deref() == Some(draft.run_id.as_str())));
    let jobs =
        outbox::jobs_by_correlation(persistence.connection_ref(), CLIENT, &ids, 10).expect("jobs");
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].job_id, format!("qwj_{}", draft.run_id));
}

#[test]
fn correlation_helpers_return_empty_for_empty_input() {
    let persistence = Persistence::open_in_memory().expect("db");
    assert!(crate::store_core::receipts_by_correlation(
        persistence.connection_ref(),
        CLIENT,
        &[],
        10
    )
    .expect("receipts")
    .is_empty());
    assert!(
        outbox::jobs_by_correlation(persistence.connection_ref(), CLIENT, &[], 10)
            .expect("jobs")
            .is_empty()
    );
}
