use bos_contracts::ai_usage::AiUsageRow;
use bos_contracts::packet_proposals::{
    PacketProposalDecisionMode, PacketProposalExecutionMode, PacketProposalKindOutcome,
    PacketProposalKindOutcomeStatus, PacketProposalReasonCode, PacketProposalRunStatus,
};
use bos_contracts::receipt::ActorKindDto;
use bos_integrations::google_drive_read::DriveFileMeta;

use super::store;
use crate::outbox::{AttemptOutcome, ClaimedJob, NewOutboxJob};
use crate::persistence::Persistence;
use crate::store_core::{self, MutationOutcome, MutationRequest, StoreError};

const CLIENT: &str = "test-client";

#[test]
fn debug_projection_includes_backend_error_surfaces() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();

    let failed_mutation = store_core::mutate(
        conn,
        MutationRequest {
            client_id: CLIENT,
            entity_kind: "work_item",
            entity_id: "wi_1",
            change_kind: "produce",
            actor_id: "operator",
            actor_kind: ActorKindDto::Operator,
            expected_revision: None,
            idempotency_key: "fail_mutation",
            correlation_id: Some("wi_1"),
            causation_id: None,
            before_json: None,
            after_json: None,
            now_ms: 1_000,
        },
        |_| Err(StoreError::Domain("produce_source_missing".to_string())),
    );
    assert!(failed_mutation.is_err());
    store_core::record_failed_receipt(
        conn,
        MutationRequest {
            client_id: CLIENT,
            entity_kind: "produce",
            entity_id: "wi_calendar",
            change_kind: "stage",
            actor_id: "operator",
            actor_kind: ActorKindDto::Operator,
            expected_revision: None,
            idempotency_key: "fail_calendar_stage",
            correlation_id: Some("wi_calendar"),
            causation_id: None,
            before_json: None,
            after_json: Some(
                serde_json::json!({
                    "packet_kind": "calendar_event_draft",
                    "error_code": "calendar_extract_no_event",
                    "message": "newsletter with no concrete dated event"
                })
                .to_string(),
            ),
            now_ms: 1_500,
        },
        "calendar_extract_no_event",
    )
    .expect("failed produce receipt");

    {
        let tx = conn.transaction().expect("tx");
        crate::outbox::enqueue_within(
            &tx,
            CLIENT,
            &NewOutboxJob {
                job_id: "job_1".to_string(),
                provider: "gmail".to_string(),
                capability: "create_draft".to_string(),
                payload_json: "{}".to_string(),
                source_entity_kind: "email_reply_draft".to_string(),
                source_entity_id: "draft_1".to_string(),
                correlation_id: Some("wi_1".to_string()),
                causation_id: None,
                idempotency_key: "job_1".to_string(),
            },
            2_000,
        )
        .expect("enqueue");
        tx.commit().expect("commit outbox seed");
    }

    let claimed = ClaimedJob {
        job_id: "job_1".to_string(),
        provider: "gmail".to_string(),
        capability: "create_draft".to_string(),
        payload_json: "{}".to_string(),
        attempts: 0,
        source_entity_kind: "email_reply_draft".to_string(),
        source_entity_id: "draft_1".to_string(),
        correlation_id: Some("wi_1".to_string()),
        idempotency_key: "job_1".to_string(),
    };
    crate::outbox::record_attempt(
        conn,
        CLIENT,
        &claimed,
        &AttemptOutcome::Terminal {
            error: "gmail rejected draft".to_string(),
            result_json: None,
        },
        3_000,
    )
    .expect("record outbox failure");

    crate::slices::inventory::store::put_cursor(
        conn,
        CLIENT,
        crate::slices::inventory::store::ENTITY_ORDER,
        &crate::slices::inventory::store::SfSyncCursor {
            last_error: Some("stockforge timeout".to_string()),
            ..Default::default()
        },
        3_500,
    )
    .expect("insert inventory sync error");

    crate::slices::drive_corpus::store::mark_stale_from_meta(
        conn,
        CLIENT,
        &DriveFileMeta {
            file_id: "drive_file_1".to_string(),
            name: "Catalog.pdf".to_string(),
            mime_type: "application/pdf".to_string(),
            modified_time: "2026-06-01T00:00:00Z".to_string(),
            version: Some("1".to_string()),
            parent_folder_ids: Vec::new(),
            web_view_link: None,
            trashed: false,
        },
        3_600,
    )
    .expect("insert drive doc");
    crate::slices::drive_corpus::store::mark_error(
        conn,
        CLIENT,
        "drive_file_1",
        "unsupported mime parser failed",
        3_700,
    )
    .expect("insert drive doc error");

    crate::slices::ai_usage::store::insert_usage(
        conn,
        CLIENT,
        &crate::slices::ai_usage::store::UsageInsert {
            row: AiUsageRow {
                usage_id: "aiu_1".to_string(),
                purpose: "invoice_fill".to_string(),
                route: "harness".to_string(),
                provider: "claude".to_string(),
                model: "claude-sonnet-4-6".to_string(),
                tokens_in: None,
                tokens_out: None,
                total_tokens: None,
                cost_micros: None,
                latency_ms: 250,
                success: false,
                error_code: Some("typed_llm_harness_session_exited".to_string()),
                correlation_id: "wi_1".to_string(),
                recorded_at_ms: 4_000,
            },
            task_kind: Some("fill".to_string()),
            thinking_level: None,
            cached_tokens: None,
            provider_request_id: Some("bos-llm-session".to_string()),
            error_message: Some("model unavailable".to_string()),
        },
    )
    .expect("insert usage");

    crate::slices::packet_proposals::store::insert_run(
        conn,
        CLIENT,
        crate::slices::packet_proposals::store::NewRun {
            run_id: "ppr_no_drafts",
            source_kind: "email",
            source_ref: "msg_1",
            item_id: Some("wi_1"),
            resolved_decision_mode: PacketProposalDecisionMode::FillFixed,
            execution_mode: PacketProposalExecutionMode::BoundedTyped,
            candidate_packet_kinds: &["calendar_event_draft".to_string()],
            idempotency_key: "ppr_no_drafts:start",
            actor_id: "smart_draft",
            actor_kind: ActorKindDto::System,
            now_ms: 4_100,
        },
    )
    .expect("insert packet proposal run");
    crate::slices::packet_proposals::store::append_evidence(
        conn,
        CLIENT,
        crate::slices::packet_proposals::store::NewEvidence {
            evidence_id: "ppe_no_event",
            run_id: "ppr_no_drafts",
            turn_index: 10_000,
            tool_name: "proposal_stage",
            tool_args_json: r#"{"packet_kind":"calendar_event_draft"}"#,
            result_ref: "calendar_extract_no_event",
            result_excerpt: r#"{"message":"newsletter with no concrete dated event"}"#,
            idempotency_key: "ppr_no_drafts:evidence",
            actor_id: "smart_draft",
            actor_kind: ActorKindDto::System,
            now_ms: 4_150,
        },
    )
    .expect("append packet proposal evidence");
    crate::slices::packet_proposals::store::update_run(
        conn,
        CLIENT,
        crate::slices::packet_proposals::store::RunUpdate {
            run_id: "ppr_no_drafts",
            item_id: Some("wi_1"),
            status: PacketProposalRunStatus::Completed,
            outcomes: &[PacketProposalKindOutcome {
                packet_kind: "calendar_event_draft".to_string(),
                status: PacketProposalKindOutcomeStatus::RejectedByGate,
                reason_code: Some(PacketProposalReasonCode::GateRejected),
                message: Some("newsletter with no concrete dated event".to_string()),
                draft_id: None,
            }],
            model: Some("test-model"),
            confidence: Some("medium"),
            error_code: None,
            idempotency_key: "ppr_no_drafts:finish",
            actor_id: "smart_draft",
            actor_kind: ActorKindDto::System,
            now_ms: 4_200,
        },
    )
    .expect("finish packet proposal run");

    let rows = store::list_recent(conn, CLIENT, 20).expect("debug rows");

    assert!(rows.iter().any(|row| row.source == "receipt"
        && row.entity_kind.as_deref() == Some("work_item")
        && row.error_code == "domain"));
    assert!(rows.iter().any(|row| row.source == "receipt"
        && row.entity_kind.as_deref() == Some("produce")
        && row.error_code == "calendar_extract_no_event"
        && row
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("no concrete dated event"))));
    assert!(rows.iter().any(|row| row.source == "outbox"
        && row.category == "provider_delivery"
        && row
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("gmail rejected"))));
    assert!(rows.iter().any(|row| row.source == "sync"
        && row.category == "inventory_sync"
        && row.entity_id.as_deref() == Some("order")
        && row
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("stockforge timeout"))));
    assert!(rows.iter().any(|row| row.source == "drive"
        && row.category == "document_index"
        && row.entity_id.as_deref() == Some("drive_file_1")
        && row.operation.as_deref() == Some("Catalog.pdf")));
    assert!(rows.iter().any(|row| row.source == "llm"
        && row.operation.as_deref() == Some("invoice_fill:fill")
        && row.reference_id.as_deref() == Some("bos-llm-session")));
    assert!(rows.iter().any(|row| row.source == "packet_proposal"
        && row.diagnostic_id == "packet_proposal:ppr_no_drafts"
        && row.category == "smart_draft"
        && row.error_code == "smart_draft_no_reviewable_drafts"
        && row
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("calendar_extract_no_event"))));
}

#[test]
fn debug_projection_omits_expected_packet_proposal_no_draft_completion() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    crate::slices::packet_proposals::store::insert_run(
        conn,
        CLIENT,
        crate::slices::packet_proposals::store::NewRun {
            run_id: "ppr_expected_no_drafts",
            source_kind: "email",
            source_ref: "msg_expected",
            item_id: Some("wi_expected"),
            resolved_decision_mode: PacketProposalDecisionMode::AiDecides,
            execution_mode: PacketProposalExecutionMode::BoundedTyped,
            candidate_packet_kinds: &["email_draft_reply".to_string()],
            idempotency_key: "ppr_expected_no_drafts:start",
            actor_id: "email_ai_triage",
            actor_kind: ActorKindDto::System,
            now_ms: 1_000,
        },
    )
    .expect("insert packet proposal run");
    crate::slices::packet_proposals::store::update_run(
        conn,
        CLIENT,
        crate::slices::packet_proposals::store::RunUpdate {
            run_id: "ppr_expected_no_drafts",
            item_id: Some("wi_expected"),
            status: PacketProposalRunStatus::Completed,
            outcomes: &[PacketProposalKindOutcome {
                packet_kind: "email_draft_reply".to_string(),
                status: PacketProposalKindOutcomeStatus::Unavailable,
                reason_code: Some(PacketProposalReasonCode::ContextUnavailable),
                message: None,
                draft_id: None,
            }],
            model: Some("test-model"),
            confidence: Some("high"),
            error_code: None,
            idempotency_key: "ppr_expected_no_drafts:finish",
            actor_id: "email_ai_triage",
            actor_kind: ActorKindDto::System,
            now_ms: 1_100,
        },
    )
    .expect("finish packet proposal run");

    let rows = store::list_recent(conn, CLIENT, 20).expect("debug rows");
    assert!(!rows
        .iter()
        .any(|row| row.diagnostic_id == "packet_proposal:ppr_expected_no_drafts"));
}

#[test]
fn debug_agent_launch_request_is_receipted_and_enqueues_outbox() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let job = crate::slices::work_queue::agent_launch::build_outbox_job_for_source(
        crate::slices::work_queue::agent_launch::AgentLaunchOutboxJobInput {
            source_id: "outbox:job_1",
            idempotency_key: "debug-launch-1",
            monitor_url: "http://monitor.local",
            display_name: "BusinessOS debug failed_terminal",
            initial_prompt: "Agent session: BusinessOS debug diagnostic\nDiagnostic: outbox:job_1",
            work_dir: crate::slices::work_queue::agent_launch::DEFAULT_AGENT_WORK_DIR,
            source_entity_kind: store::AGENT_LAUNCH_ENTITY_KIND,
            source_entity_id: "outbox:job_1",
            correlation_id: Some("outbox:job_1"),
        },
    )
    .expect("debug launch job");

    let first = store::record_agent_launch_request(
        conn,
        store::AgentLaunchRequestContext {
            client_id: CLIENT,
            diagnostic_id: "outbox:job_1",
            actor_id: "op_debug",
            job: &job,
            idempotency_key: "debug-launch-1",
            now_ms: 5_000,
        },
    )
    .expect("first debug launch receipt");
    assert!(matches!(
        first,
        MutationOutcome::Applied { revision: 1, .. }
    ));

    let (payload, stored_idempotency_key): (serde_json::Value, String) = conn
        .query_row(
            "SELECT provider, capability, source_entity_kind, source_entity_id, correlation_id, \
             payload_json, idempotency_key FROM outbox_jobs WHERE client_id = ?1 AND job_id = ?2",
            rusqlite::params![CLIENT, job.job_id],
            |row| {
                assert_eq!(
                    row.get::<_, String>(0)?,
                    crate::slices::work_queue::agent_launch::PROVIDER_AGENT_MONITOR
                );
                assert_eq!(
                    row.get::<_, String>(1)?,
                    crate::slices::work_queue::agent_launch::CAPABILITY_LAUNCH_AGENT
                );
                assert_eq!(row.get::<_, String>(2)?, store::AGENT_LAUNCH_ENTITY_KIND);
                assert_eq!(row.get::<_, String>(3)?, "outbox:job_1");
                assert_eq!(
                    row.get::<_, Option<String>>(4)?.as_deref(),
                    Some("outbox:job_1")
                );
                Ok((row.get::<_, String>(5)?, row.get::<_, String>(6)?))
            },
        )
        .and_then(|(raw, idempotency_key)| {
            serde_json::from_str(&raw)
                .map_err(|err| {
                    rusqlite::Error::FromSqlConversionFailure(
                        5,
                        rusqlite::types::Type::Text,
                        Box::new(err),
                    )
                })
                .map(|payload| (payload, idempotency_key))
        })
        .expect("outbox payload");
    assert_eq!(stored_idempotency_key, "debug-launch-1");
    assert_eq!(
        payload
            .pointer("/display_name")
            .and_then(serde_json::Value::as_str),
        Some("BusinessOS debug failed_terminal")
    );
    assert_eq!(
        payload
            .pointer("/work_dir")
            .and_then(serde_json::Value::as_str),
        Some(crate::slices::work_queue::agent_launch::DEFAULT_AGENT_WORK_DIR)
    );
    assert!(payload
        .pointer("/initial_prompt")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|prompt| prompt.contains("Diagnostic: outbox:job_1")));

    let replay = store::record_agent_launch_request(
        conn,
        store::AgentLaunchRequestContext {
            client_id: CLIENT,
            diagnostic_id: "outbox:job_1",
            actor_id: "op_debug",
            job: &job,
            idempotency_key: "debug-launch-1",
            now_ms: 5_100,
        },
    )
    .expect("replay debug launch receipt");
    assert!(matches!(
        replay,
        MutationOutcome::ReplayedIdempotent {
            revision: Some(1),
            ..
        }
    ));

    let job_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM outbox_jobs WHERE client_id = ?1",
            rusqlite::params![CLIENT],
            |row| row.get(0),
        )
        .expect("outbox count");
    assert_eq!(job_count, 1);
    let receipt_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM receipts \
             WHERE client_id = ?1 AND entity_kind = ?2 AND entity_id = ?3",
            rusqlite::params![CLIENT, store::AGENT_LAUNCH_ENTITY_KIND, "outbox:job_1"],
            |row| row.get(0),
        )
        .expect("receipt count");
    assert_eq!(receipt_count, 2);
}

#[test]
fn debug_agent_launch_reports_in_progress_when_outbox_pump_claimed_first() {
    let state = crate::http::test_support::test_state();
    let job = crate::slices::work_queue::agent_launch::build_outbox_job_for_source(
        crate::slices::work_queue::agent_launch::AgentLaunchOutboxJobInput {
            source_id: "outbox:job_2",
            idempotency_key: "debug-launch-2",
            monitor_url: "http://monitor.local",
            display_name: "BusinessOS debug failed_terminal",
            initial_prompt: "Agent session: BusinessOS debug diagnostic\nDiagnostic: outbox:job_2",
            work_dir: crate::slices::work_queue::agent_launch::DEFAULT_AGENT_WORK_DIR,
            source_entity_kind: store::AGENT_LAUNCH_ENTITY_KIND,
            source_entity_id: "outbox:job_2",
            correlation_id: Some("outbox:job_2"),
        },
    )
    .expect("debug launch job");
    let job_id = job.job_id.clone();
    let now_ms = crate::http::now_ms();

    {
        let mut persistence = state.persistence.lock();
        store::record_agent_launch_request(
            persistence.connection(),
            store::AgentLaunchRequestContext {
                client_id: CLIENT,
                diagnostic_id: "outbox:job_2",
                actor_id: "op_debug",
                job: &job,
                idempotency_key: "debug-launch-2",
                now_ms,
            },
        )
        .expect("record launch request");
    }
    {
        let mut persistence = state.persistence.lock();
        let claimed = crate::outbox::claim_due_job_by_id(
            persistence.connection(),
            CLIENT,
            &job_id,
            120_000,
            now_ms + 1,
        )
        .expect("pump claim");
        assert!(claimed.is_some(), "global pump should claim the job first");
    }

    let result = super::routes::launch_claimed_debug_agent_job(state, job_id);
    assert!(matches!(
        result,
        Err(super::routes::DebugAgentSpawnError::InProgress)
    ));
}
