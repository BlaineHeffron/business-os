use bos_contracts::receipt::ActorKindDto;
use bos_contracts::social_publishing::{
    SocialAdhocSourceCreateRequest, SocialProposalStageRequest, SocialProposalStatus,
    SocialProposalTargetInput, SocialProposalUpdateRequest, SocialPublishedContentIngressRequest,
    SocialScheduleMode, SocialSourceGenerationStatus, SocialUtmParameters,
};
use bos_integrations::buffer::{BufferPostOutboxPayload, BufferWriteConfig};
use serde_json::json;

use super::{service, store};
use crate::http::test_support::{test_state, EnvGuard};
use crate::store_core::MutationOutcome;

const CLIENT: &str = "test-client";
const CHANNELS: &str = r#"[
  {"channel_id":"buf_linkedin","name":"Company LinkedIn","platform":"linkedin"},
  {"channel_id":"buf_x","name":"Company X","platform":"twitter"}
]"#;

const FOUR_CONNECTED_CHANNELS: &str = r#"[
  {"channel_id":"buf_instagram","name":"Company Instagram","platform":"instagram"},
  {"channel_id":"buf_facebook","name":"Company Facebook","platform":"Facebook"},
  {"channel_id":"buf_linkedin","name":"Company LinkedIn","platform":"linkedin"},
  {"channel_id":"buf_google","name":"Company Google Business","platform":"GoogleBusiness"}
]"#;

fn target(channel_id: &str, text: &str) -> SocialProposalTargetInput {
    SocialProposalTargetInput {
        channel_id: channel_id.to_string(),
        text: text.to_string(),
        image_url: None,
        utm: SocialUtmParameters {
            source: Some(
                if channel_id == "buf_x" {
                    "x"
                } else {
                    "linkedin"
                }
                .to_string(),
            ),
            medium: Some("social".to_string()),
            campaign: Some("launch".to_string()),
            content: Some("blog".to_string()),
        },
        schedule_mode: SocialScheduleMode::Queue,
        due_at: None,
    }
}

fn stage_request(key: &str) -> SocialProposalStageRequest {
    SocialProposalStageRequest {
        source_id: None,
        source_content_draft_id: None,
        source_content_draft_revision: None,
        canonical_url: "https://example.com/blog/epoxy-guide#section".to_string(),
        targets: vec![
            target("buf_linkedin", "LinkedIn launch copy"),
            target("buf_x", "X launch copy"),
        ],
        idempotency_key: key.to_string(),
        actor_id: None,
    }
}

#[test]
fn stage_normalizes_exact_configured_channel_snapshot_and_system_actor() {
    let _env = EnvGuard::set("BOS_BUFFER_CHANNELS_JSON", CHANNELS);
    let state = test_state();
    let mut persistence = state.persistence.lock();
    let (outcome, proposal_id) = service::stage_request(
        persistence.connection(),
        CLIENT,
        "social_draft_generator",
        ActorKindDto::System,
        &stage_request("stage-social-1"),
        1_000,
    )
    .expect("stage");
    assert!(matches!(
        outcome,
        MutationOutcome::Applied { revision: 1, .. }
    ));
    let entry = store::get_proposal(persistence.connection_ref(), CLIENT, &proposal_id)
        .expect("read")
        .expect("proposal");
    assert_eq!(entry.proposal.status, SocialProposalStatus::Staged);
    assert_eq!(
        entry.proposal.canonical_url.as_deref(),
        Some("https://example.com/blog/epoxy-guide")
    );
    assert_eq!(entry.proposal.targets.len(), 2);
    assert_eq!(entry.proposal.targets[0].channel_name, "Company LinkedIn");
    assert!(entry.proposal.targets[0]
        .tracked_url
        .contains("utm_source=linkedin"));
    assert!(entry.proposal.targets[0]
        .text
        .ends_with(&entry.proposal.targets[0].tracked_url));
    let receipts = crate::store_core::receipts_for_entity(
        persistence.connection_ref(),
        CLIENT,
        store::PROPOSAL_ENTITY_KIND,
        &proposal_id,
        10,
    )
    .expect("receipts");
    assert_eq!(receipts[0].actor_kind, ActorKindDto::System);
    assert_eq!(receipts[0].actor_id, "social_draft_generator");
}

#[test]
fn connected_channels_approve_as_independent_buffer_jobs() {
    let _env = EnvGuard::set("BOS_BUFFER_CHANNELS_JSON", FOUR_CONNECTED_CHANNELS);
    let state = test_state();
    let channels = service::configured_channels().expect("connected channels");
    let targets = channels
        .iter()
        .map(|channel| SocialProposalTargetInput {
            channel_id: channel.channel_id.clone(),
            text: format!("Read the new article on {}.", channel.name),
            image_url: (channel.platform == "instagram")
                .then(|| "https://example.com/social.jpg".to_string()),
            utm: SocialUtmParameters {
                source: Some(channel.platform.clone()),
                medium: Some("social".to_string()),
                campaign: Some("blog".to_string()),
                content: None,
            },
            schedule_mode: SocialScheduleMode::Queue,
            due_at: None,
        })
        .collect();
    let request = SocialProposalStageRequest {
        source_id: None,
        source_content_draft_id: None,
        source_content_draft_revision: None,
        canonical_url: "https://example.com/blog/new-stay".to_string(),
        targets,
        idempotency_key: "four-connected-channels".to_string(),
        actor_id: None,
    };
    let mut persistence = state.persistence.lock();
    let (_, proposal_id) = service::stage_request(
        persistence.connection(),
        CLIENT,
        "user_casey",
        ActorKindDto::Operator,
        &request,
        1_000,
    )
    .expect("stage connected channels");
    service::approve_request(
        persistence.connection(),
        CLIENT,
        "user_casey",
        &proposal_id,
        1,
        "approve-four-connected-channels",
        2_000,
    )
    .expect("approve connected channels");

    let jobs = crate::outbox::claim_due_jobs(
        persistence.connection(),
        CLIENT,
        Some(service::PROVIDER_BUFFER),
        60_000,
        10,
        3_000,
    )
    .expect("claim connected channel jobs");
    let mut platforms = jobs
        .iter()
        .map(|job| {
            serde_json::from_str::<BufferPostOutboxPayload>(&job.payload_json)
                .expect("payload")
                .platform
        })
        .collect::<Vec<_>>();
    platforms.sort();
    assert_eq!(
        platforms,
        vec!["facebook", "googlebusiness", "instagram", "linkedin"]
    );
}

#[test]
fn stale_approval_enqueues_nothing_then_current_approval_fans_out_atomically() {
    let _env = EnvGuard::set("BOS_BUFFER_CHANNELS_JSON", CHANNELS);
    let state = test_state();
    let mut persistence = state.persistence.lock();
    let (_, proposal_id) = service::stage_request(
        persistence.connection(),
        CLIENT,
        "user_example",
        ActorKindDto::Operator,
        &stage_request("stage-social-2"),
        1_000,
    )
    .expect("stage");
    let update = SocialProposalUpdateRequest {
        canonical_url: "https://example.com/blog/epoxy-guide".to_string(),
        targets: vec![
            SocialProposalTargetInput {
                image_url: Some("https://example.com/social.jpg".to_string()),
                schedule_mode: SocialScheduleMode::Scheduled,
                due_at: Some("2026-08-20T14:00:00Z".to_string()),
                ..target("buf_linkedin", "Edited LinkedIn copy")
            },
            target("buf_x", "Edited X copy"),
        ],
        expected_revision: 1,
        idempotency_key: "edit-social-2".to_string(),
        actor_id: None,
    };
    let updated = service::update_request(
        persistence.connection(),
        CLIENT,
        "user_example",
        &proposal_id,
        &update,
        2_000,
    )
    .expect("update");
    assert!(matches!(
        updated,
        MutationOutcome::Applied { revision: 2, .. }
    ));

    let stale = service::approve_request(
        persistence.connection(),
        CLIENT,
        "user_example",
        &proposal_id,
        1,
        "approve-social-stale",
        3_000,
    )
    .expect("stale outcome");
    assert!(matches!(
        stale,
        MutationOutcome::RevisionConflict {
            current_revision: Some(2),
            ..
        }
    ));
    let job_count: i64 = persistence
        .connection_ref()
        .query_row("SELECT COUNT(*) FROM outbox_jobs", [], |row| row.get(0))
        .expect("job count");
    assert_eq!(job_count, 0);

    let approved = service::approve_request(
        persistence.connection(),
        CLIENT,
        "user_example",
        &proposal_id,
        2,
        "approve-social-current",
        4_000,
    )
    .expect("approve");
    assert!(matches!(
        approved,
        MutationOutcome::Applied { revision: 3, .. }
    ));
    let replay = service::approve_request(
        persistence.connection(),
        CLIENT,
        "user_example",
        &proposal_id,
        2,
        "approve-social-current",
        4_001,
    )
    .expect("approval replay");
    assert!(matches!(replay, MutationOutcome::ReplayedIdempotent { .. }));

    let entry = store::get_proposal(persistence.connection_ref(), CLIENT, &proposal_id)
        .expect("read")
        .expect("proposal");
    assert_eq!(entry.proposal.status, SocialProposalStatus::Approved);
    assert_eq!(entry.proposal.approved_revision, Some(2));
    assert_eq!(entry.proposal.targets.len(), 2);
    assert!(entry.proposal.targets.iter().all(|target| target
        .outbox_job
        .as_ref()
        .is_some_and(|job| job.status == "pending")));
    let jobs = crate::outbox::claim_due_jobs(
        persistence.connection(),
        CLIENT,
        Some(service::PROVIDER_BUFFER),
        60_000,
        10,
        5_000,
    )
    .expect("claim channel jobs");
    assert_eq!(jobs.len(), 2);
    let payloads = jobs
        .iter()
        .map(|job| {
            serde_json::from_str::<BufferPostOutboxPayload>(&job.payload_json).expect("payload")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        payloads
            .iter()
            .map(|payload| payload.channel_id.as_str())
            .collect::<Vec<_>>(),
        vec!["buf_linkedin", "buf_x"]
    );
    assert_eq!(payloads[0].text, entry.proposal.targets[0].text);
    assert_eq!(
        payloads[0].image_url.as_deref(),
        Some("https://example.com/social.jpg")
    );
    assert_eq!(payloads[0].approval.approved_revision, 2);
    assert_ne!(payloads[0].idempotency_key, payloads[1].idempotency_key);
    assert!(!jobs[0].payload_json.contains("BOS_BUFFER_ACCESS_TOKEN"));
}

#[test]
fn channel_results_are_independent_and_dry_run_is_default() {
    let _env = EnvGuard::set("BOS_BUFFER_CHANNELS_JSON", CHANNELS);
    let state = test_state();
    let mut persistence = state.persistence.lock();
    let (_, proposal_id) = service::stage_request(
        persistence.connection(),
        CLIENT,
        "user_example",
        ActorKindDto::Operator,
        &stage_request("stage-social-3"),
        1_000,
    )
    .expect("stage");
    service::approve_request(
        persistence.connection(),
        CLIENT,
        "user_example",
        &proposal_id,
        1,
        "approve-social-3",
        2_000,
    )
    .expect("approve");
    let jobs = crate::outbox::claim_due_jobs(
        persistence.connection(),
        CLIENT,
        Some(service::PROVIDER_BUFFER),
        60_000,
        10,
        3_000,
    )
    .expect("claim");
    let approved_entry = store::get_proposal(persistence.connection_ref(), CLIENT, &proposal_id)
        .expect("read approved")
        .expect("approved proposal");
    let first_job_id = approved_entry.proposal.targets[0]
        .outbox_job_id
        .clone()
        .expect("first job id");
    let second_job_id = approved_entry.proposal.targets[1]
        .outbox_job_id
        .clone()
        .expect("second job id");
    for job in &jobs {
        match service::execute_job(
            job,
            &BufferWriteConfig {
                api_url: "https://api.buffer.com".to_string(),
                access_token: None,
                write_enabled: false,
            },
            4_000,
        ) {
            crate::outbox::AttemptOutcome::Delivered { result_json } => {
                let result: serde_json::Value = serde_json::from_str(&result_json).expect("result");
                assert_eq!(result["dry_run"], true);
                assert_eq!(result["approved_revision"], 1);
            }
            other => panic!("expected dry-run delivery, got {other:?}"),
        }
    }

    let first_job = jobs
        .iter()
        .find(|job| job.job_id == first_job_id)
        .expect("claimed first job");
    crate::outbox::record_attempt(
        persistence.connection(),
        CLIENT,
        first_job,
        &crate::outbox::AttemptOutcome::Delivered {
            result_json: "{\"dry_run\":false,\"provider_object_id\":\"buf_post_1\"}".to_string(),
        },
        5_000,
    )
    .expect("deliver first");
    let second_job = jobs
        .iter()
        .find(|job| job.job_id == second_job_id)
        .expect("claimed second job");
    crate::outbox::record_attempt(
        persistence.connection(),
        CLIENT,
        second_job,
        &crate::outbox::AttemptOutcome::Terminal {
            error: "buffer_post_rejected".to_string(),
            result_json: None,
        },
        5_001,
    )
    .expect("fail second");
    let entry = store::get_proposal(persistence.connection_ref(), CLIENT, &proposal_id)
        .expect("read")
        .expect("proposal");
    let first = entry.proposal.targets[0]
        .outbox_job
        .as_ref()
        .expect("first job");
    let second = entry.proposal.targets[1]
        .outbox_job
        .as_ref()
        .expect("second job");
    assert_eq!(first.status, "delivered");
    assert_eq!(first.provider_object_id.as_deref(), Some("buf_post_1"));
    assert_eq!(second.status, "failed_terminal");
    assert_eq!(second.last_error.as_deref(), Some("buffer_post_rejected"));

    crate::outbox::retry_terminal_job(
        persistence.connection(),
        CLIENT,
        &second_job_id,
        "user_example",
        "retry-second-channel",
        6_000,
    )
    .expect("retry failed channel");
    let retried = store::get_proposal(persistence.connection_ref(), CLIENT, &proposal_id)
        .expect("read retried")
        .expect("proposal");
    assert_eq!(
        retried.proposal.targets[0]
            .outbox_job
            .as_ref()
            .expect("first job")
            .status,
        "delivered"
    );
    assert_eq!(
        retried.proposal.targets[1]
            .outbox_job
            .as_ref()
            .expect("second job")
            .status,
        "pending"
    );
}

#[test]
fn ambiguous_buffer_create_is_durable_and_cannot_be_blind_retried() {
    let _env = EnvGuard::set("BOS_BUFFER_CHANNELS_JSON", CHANNELS);
    let state = test_state();
    let mut persistence = state.persistence.lock();
    let (_, proposal_id) = service::stage_request(
        persistence.connection(),
        CLIENT,
        "user_example",
        ActorKindDto::Operator,
        &stage_request("stage-social-unknown"),
        1_000,
    )
    .expect("stage");
    service::approve_request(
        persistence.connection(),
        CLIENT,
        "user_example",
        &proposal_id,
        1,
        "approve-social-unknown",
        2_000,
    )
    .expect("approve");
    let job = crate::outbox::claim_due_jobs(
        persistence.connection(),
        CLIENT,
        Some(service::PROVIDER_BUFFER),
        60_000,
        1,
        3_000,
    )
    .expect("claim")
    .remove(0);
    crate::outbox::record_attempt(
        persistence.connection(),
        CLIENT,
        &job,
        &crate::outbox::AttemptOutcome::OutcomeUnknown {
            error: "buffer_delivery_outcome_unknown".to_string(),
            result_json: Some(
                serde_json::json!({
                    "delivery_outcome_unknown": true,
                    "manual_reconciliation_required": true,
                    "provider": "buffer",
                })
                .to_string(),
            ),
        },
        4_000,
    )
    .expect("record unknown outcome");

    let proposal = store::get_proposal(persistence.connection_ref(), CLIENT, &proposal_id)
        .expect("proposal")
        .expect("proposal");
    let summary = proposal
        .proposal
        .targets
        .iter()
        .find_map(|target| {
            target
                .outbox_job
                .as_ref()
                .filter(|summary| summary.job_id == job.job_id)
        })
        .expect("unknown job summary");
    assert_eq!(summary.status, "delivery_outcome_unknown");
    assert_eq!(
        summary.last_error.as_deref(),
        Some("buffer_delivery_outcome_unknown")
    );
    let receipts = crate::store_core::receipts_for_entity(
        persistence.connection_ref(),
        CLIENT,
        crate::outbox::JOB_ENTITY_KIND,
        &job.job_id,
        10,
    )
    .expect("outbox receipts");
    assert!(receipts
        .iter()
        .any(|receipt| receipt.change_kind == "deliver_outcome_unknown"));
    let retry = crate::outbox::retry_terminal_job(
        persistence.connection(),
        CLIENT,
        &job.job_id,
        "user_example",
        "retry-unknown-buffer-create",
        5_000,
    )
    .expect_err("unknown outcome requires manual reconciliation");
    assert!(matches!(
        retry,
        crate::store_core::StoreError::Domain(code)
            if code == "outbox_retry_not_failed:delivery_outcome_unknown"
    ));
}

#[test]
fn proposal_must_cover_exact_configured_channels_and_valid_schedule() {
    let _env = EnvGuard::set("BOS_BUFFER_CHANNELS_JSON", CHANNELS);
    let state = test_state();
    let mut request = stage_request("stage-social-invalid");
    request.targets.pop();
    let mut persistence = state.persistence.lock();
    let err = service::stage_request(
        persistence.connection(),
        CLIENT,
        "user_example",
        ActorKindDto::Operator,
        &request,
        1_000,
    )
    .expect_err("incomplete channels");
    assert!(matches!(
        err,
        crate::store_core::StoreError::Domain(code) if code == "social_channel_set_incomplete"
    ));

    let mut request = stage_request("stage-social-bad-date");
    request.targets[0].schedule_mode = SocialScheduleMode::Scheduled;
    request.targets[0].due_at = Some("tomorrow".to_string());
    let err = service::stage_request(
        persistence.connection(),
        CLIENT,
        "user_example",
        ActorKindDto::Operator,
        &request,
        2_000,
    )
    .expect_err("invalid schedule");
    assert!(matches!(
        err,
        crate::store_core::StoreError::Domain(code) if code == "social_schedule_due_at_invalid"
    ));
}

#[test]
fn provider_specific_future_platforms_fail_closed_until_their_contracts_exist() {
    for platform in ["pinterest", "tiktok"] {
        let channels = format!(
            r#"[{{"channel_id":"future","name":"Future channel","platform":"{platform}"}}]"#
        );
        let _env = EnvGuard::set("BOS_BUFFER_CHANNELS_JSON", &channels);
        let err = service::configured_channels().expect_err("unsupported platform");
        assert!(matches!(
            err,
            crate::store_core::StoreError::Domain(code)
                if code == "social_channels_config_invalid"
        ));
    }
}

fn ingress_request(external_id: &str, key: &str) -> SocialPublishedContentIngressRequest {
    SocialPublishedContentIngressRequest {
        source_kind: "wordpress".to_string(),
        external_id: external_id.to_string(),
        source_content_draft_id: None,
        canonical_url: format!("https://example.com/blog/{external_id}"),
        title: "How to prepare an epoxy floor".to_string(),
        excerpt: Some("Diamond grinding removes weak concrete before epoxy coating.".to_string()),
        published_at: Some("2026-08-12T14:00:00Z".to_string()),
        image_url: None,
        idempotency_key: key.to_string(),
    }
}

#[test]
fn reingress_preserves_generation_and_approved_proposal_lifecycle() {
    let _env = EnvGuard::set("BOS_BUFFER_CHANNELS_JSON", CHANNELS);
    let state = test_state();
    let source = {
        let mut persistence = state.persistence.lock();
        service::ingest_source_request(
            persistence.connection(),
            CLIENT,
            "mcp:openclaw",
            ActorKindDto::Agent,
            &ingress_request("lifecycle", "ingress-lifecycle"),
            1_000,
        )
        .expect("initial ingress")
    };

    let mut persistence = state.persistence.lock();
    store::begin_generation(
        persistence.connection(),
        crate::slices::mutation_context::MutationContext {
            client_id: CLIENT,
            actor_id: "operator",
            expected_revision: Some(source.revision),
            idempotency_key: "generation-lifecycle-start",
            now_ms: 2_000,
        },
        ActorKindDto::Operator,
        &source.source_id,
        "socialgen_lifecycle",
    )
    .expect("begin generation");

    let mut generating_refresh = ingress_request("lifecycle", "ingress-while-generating");
    generating_refresh.title = "Updated while generating".to_string();
    let generating = service::ingest_source_request(
        persistence.connection(),
        CLIENT,
        "mcp:openclaw",
        ActorKindDto::Agent,
        &generating_refresh,
        2_100,
    )
    .expect("refresh while generating");
    assert_eq!(
        generating.generation_status,
        SocialSourceGenerationStatus::Generating
    );
    assert_eq!(
        generating.generation_run_id.as_deref(),
        Some("socialgen_lifecycle")
    );
    assert_eq!(generating.title, "Updated while generating");
    drop(persistence);
    let service::GenerationKickoffOutcome::Accepted(same_run) = service::kickoff_generation(
        state.clone(),
        &source.source_id,
        generating.revision,
        "reingress-while-generating-kickoff",
        "social_draft_generator",
        ActorKindDto::System,
    )
    .expect("re-ingress does not replace the active run") else {
        panic!("active generation must not conflict");
    };
    assert_eq!(
        same_run.generation_run_id.as_deref(),
        Some("socialgen_lifecycle")
    );
    let mut persistence = state.persistence.lock();

    let mut proposal_request = stage_request("stage-lifecycle");
    proposal_request.source_id = Some(source.source_id.clone());
    proposal_request.canonical_url = source.canonical_url.clone().unwrap_or_default();
    let (_, proposal_id) = service::stage_request(
        persistence.connection(),
        CLIENT,
        "social_draft_generator",
        ActorKindDto::System,
        &proposal_request,
        2_200,
    )
    .expect("stage generated proposal");
    let current = store::get_source(persistence.connection_ref(), CLIENT, &source.source_id)
        .expect("source")
        .expect("source");
    store::finish_generation(
        persistence.connection(),
        crate::slices::mutation_context::MutationContext {
            client_id: CLIENT,
            actor_id: "social_draft_generator",
            expected_revision: Some(current.revision),
            idempotency_key: "generation-lifecycle-finish",
            now_ms: 2_300,
        },
        &source.source_id,
        "socialgen_lifecycle",
        Some(&proposal_id),
        None,
    )
    .expect("finish generation");
    service::approve_request(
        persistence.connection(),
        CLIENT,
        "user_example",
        &proposal_id,
        1,
        "approve-lifecycle",
        2_400,
    )
    .expect("approve proposal");

    let mut approved_refresh = ingress_request("lifecycle", "ingress-after-approved");
    approved_refresh.title = "Updated after approval".to_string();
    let refreshed = service::ingest_source_request(
        persistence.connection(),
        CLIENT,
        "mcp:openclaw",
        ActorKindDto::Agent,
        &approved_refresh,
        2_500,
    )
    .expect("refresh after approval");
    assert_eq!(
        refreshed.generation_status,
        SocialSourceGenerationStatus::ProposalStaged
    );
    assert_eq!(
        refreshed.generation_run_id.as_deref(),
        Some("socialgen_lifecycle")
    );
    assert_eq!(refreshed.proposal_id.as_deref(), Some(proposal_id.as_str()));
    assert_eq!(
        store::get_proposal(persistence.connection_ref(), CLIENT, &proposal_id)
            .expect("proposal")
            .expect("proposal")
            .proposal
            .status,
        SocialProposalStatus::Approved
    );
    drop(persistence);
    let service::GenerationKickoffOutcome::Accepted(same_proposal) = service::kickoff_generation(
        state.clone(),
        &source.source_id,
        refreshed.revision,
        "reingress-after-approved-kickoff",
        "social_draft_generator",
        ActorKindDto::System,
    )
    .expect("re-ingress does not reopen an approved proposal") else {
        panic!("staged proposal must not conflict");
    };
    assert_eq!(
        same_proposal.proposal_id.as_deref(),
        Some(proposal_id.as_str())
    );
    assert_eq!(
        same_proposal.generation_status,
        SocialSourceGenerationStatus::ProposalStaged
    );
    let mut persistence = state.persistence.lock();

    let applied_revision = refreshed.revision;
    approved_refresh.title = "Replay must not overwrite".to_string();
    let replayed = service::ingest_source_request(
        persistence.connection(),
        CLIENT,
        "mcp:openclaw",
        ActorKindDto::Agent,
        &approved_refresh,
        2_600,
    )
    .expect("idempotent replay");
    assert_eq!(replayed.revision, applied_revision);
    assert_eq!(replayed.title, "Updated after approval");

    let mut drift = ingress_request("lifecycle", "ingress-canonical-drift");
    drift.canonical_url = "https://example.com/blog/different".to_string();
    let err = service::ingest_source_request(
        persistence.connection(),
        CLIENT,
        "mcp:openclaw",
        ActorKindDto::Agent,
        &drift,
        2_700,
    )
    .expect_err("canonical identity drift");
    assert!(matches!(
        err,
        crate::store_core::StoreError::Domain(code)
            if code == "social_published_source_identity_changed"
    ));
    assert_eq!(
        store::get_source(persistence.connection_ref(), CLIENT, &source.source_id)
            .expect("source")
            .expect("source")
            .revision,
        applied_revision
    );
}

#[test]
fn typed_fill_schema_refuses_unknown_and_malformed_output() {
    let channels: Vec<bos_contracts::social_publishing::SocialPublishingChannel> =
        serde_json::from_str(CHANNELS).expect("channels");
    let grounding = "Diamond grinding removes weak concrete before epoxy coating.";
    let unknown = json!({
        "targets": [],
        "confidence": "high",
        "approval": "publish now"
    });
    assert_eq!(
        service::parse_social_draft_response(
            &unknown,
            &channels,
            grounding,
            Some("https://example.com/blog/epoxy-guide"),
        ),
        Err("social_draft_output_invalid".to_string())
    );
    let malformed = json!({
        "targets": [{"target_ref": "target_1", "text": "Draft"}],
        "confidence": "high"
    });
    assert_eq!(
        service::parse_social_draft_response(
            &malformed,
            &channels,
            grounding,
            Some("https://example.com/blog/epoxy-guide"),
        ),
        Err("social_draft_output_invalid".to_string())
    );
    let valid_targets = json!([
        {
            "target_ref": "target_1",
            "text": "Prepare concrete before the epoxy coating.",
            "utm_source": "linkedin",
            "utm_medium": "social",
            "utm_campaign": "epoxy_guide",
            "utm_content": "article",
            "source_quotes": ["before epoxy coating"]
        },
        {
            "target_ref": "target_2",
            "text": "Diamond grinding removes weak concrete.",
            "utm_source": "x",
            "utm_medium": "social",
            "utm_campaign": "epoxy_guide",
            "utm_content": null,
            "source_quotes": ["Diamond grinding removes weak concrete"]
        }
    ]);
    let numeric = json!({
        "targets": valid_targets,
        "confidence": 0.93
    });
    assert_eq!(
        service::parse_social_draft_response(
            &numeric,
            &channels,
            grounding,
            Some("https://example.com/blog/epoxy-guide"),
        ),
        Err("social_draft_confidence_invalid".to_string())
    );
    let unknown_grade = json!({
        "targets": valid_targets,
        "confidence": "sure"
    });
    assert_eq!(
        service::parse_social_draft_response(
            &unknown_grade,
            &channels,
            grounding,
            Some("https://example.com/blog/epoxy-guide"),
        ),
        Err("social_draft_confidence_invalid".to_string())
    );
}

/// Invented listing copy carrying the typography a CMS actually emits: a curly
/// apostrophe, a non-breaking hyphen, an en dash, an em dash, curly double
/// quotes, and a no-break space.
const TYPOGRAPHIC_GROUNDING: &str = concat!(
    "Fully equipped chef\u{2019}s kitchens for preparing family-style meals. ",
    "Pet\u{2011}friendly suites sleep 12\u{2013}14 guests \u{2014} ideal for large groups. ",
    "Guests call it \u{201c}the calmest week of the year\u{201d}. ",
    "Check-in\u{00a0}from 4 PM.",
);

const TYPOGRAPHIC_GROUNDING_URL: &str = "https://example.com/stays/lakeside-lodge";

fn draft_response_quoting(quote: &str) -> serde_json::Value {
    json!({
        "targets": [
            {
                "target_ref": "target_1",
                "text": "Lakeside lodge copy.",
                "utm_source": "linkedin",
                "utm_medium": "social",
                "utm_campaign": "lakeside_lodge",
                "utm_content": "listing",
                "source_quotes": [quote]
            },
            {
                "target_ref": "target_2",
                "text": "Lakeside lodge copy.",
                "utm_source": "x",
                "utm_medium": "social",
                "utm_campaign": "lakeside_lodge",
                "utm_content": null,
                "source_quotes": [quote]
            }
        ],
        "confidence": "high"
    })
}

#[test]
fn grounding_quotes_survive_model_typographic_normalization() {
    let channels: Vec<bos_contracts::social_publishing::SocialPublishingChannel> =
        serde_json::from_str(CHANNELS).expect("channels");
    for (case, quote) in [
        ("curly apostrophe", "chef's kitchens for preparing family-style meals"),
        ("non-breaking hyphen", "Pet-friendly suites"),
        ("en dash", "sleep 12-14 guests"),
        ("em dash", "guests - ideal for large groups"),
        ("curly double quotes", "\"the calmest week of the year\""),
        ("no-break space", "Check-in from 4 PM"),
    ] {
        let parsed = service::parse_social_draft_response(
            &draft_response_quoting(quote),
            &channels,
            TYPOGRAPHIC_GROUNDING,
            Some(TYPOGRAPHIC_GROUNDING_URL),
        );
        assert!(
            parsed.is_ok(),
            "{case}: faithful quote {quote:?} was rejected: {parsed:?}"
        );
    }
}

#[test]
fn grounding_fold_still_rejects_copy_that_is_not_in_the_source() {
    let channels: Vec<bos_contracts::social_publishing::SocialPublishingChannel> =
        serde_json::from_str(CHANNELS).expect("channels");
    for (case, quote) in [
        ("paraphrase", "chef-grade kitchens for cooking family meals"),
        ("invented fact", "free airport shuttle for every guest"),
        ("reordered words", "kitchens fully equipped for preparing meals"),
        ("zero-width padding only", "\u{200b}\u{200b}\u{200b}\u{200b}\u{200b}\u{200b}"),
    ] {
        assert_eq!(
            service::parse_social_draft_response(
                &draft_response_quoting(quote),
                &channels,
                TYPOGRAPHIC_GROUNDING,
                Some(TYPOGRAPHIC_GROUNDING_URL),
            ),
            Err("social_draft_grounding_invalid".to_string()),
            "{case}: ungrounded quote {quote:?} was accepted"
        );
    }
}

#[test]
fn draft_instructions_state_confidence_enum_for_url_and_urlless_sources() {
    let _env = EnvGuard::set("BOS_BUFFER_CHANNELS_JSON", CHANNELS);
    let state = test_state();
    let channels = service::configured_channels().expect("channels");
    let published = {
        let mut persistence = state.persistence.lock();
        service::ingest_source_request(
            persistence.connection(),
            CLIENT,
            "mcp:openclaw",
            ActorKindDto::Agent,
            &ingress_request("epoxy-guide", "ingress-confidence-url"),
            1_000,
        )
        .expect("ingest")
    };
    let url_request = service::build_social_draft_request(
        CLIENT,
        &published,
        &channels,
        "grounding",
        "run_url",
        1,
    );
    assert_eq!(url_request.spec.prompt_template_version, "2");
    let url_instructions = url_request.input.json["instructions"]
        .as_str()
        .expect("instructions")
        .to_string();
    let adhoc = {
        let mut persistence = state.persistence.lock();
        service::ingest_adhoc_source_request(
            persistence.connection(),
            CLIENT,
            "mcp:openclaw",
            ActorKindDto::Agent,
            &adhoc_request("adhoc-confidence", None),
            1_000,
        )
        .expect("ingest adhoc")
    };
    let urlless_instructions =
        service::build_social_draft_request(CLIENT, &adhoc, &channels, "grounding", "run_adhoc", 1)
            .input
            .json["instructions"]
            .as_str()
            .expect("instructions")
            .to_string();
    let clause = r#"confidence must be exactly one of the strings "high", "medium", or "low""#;
    assert!(url_instructions.contains(clause), "{url_instructions}");
    assert!(
        urlless_instructions.contains(clause),
        "{urlless_instructions}"
    );
}

fn wait_for_source_status(
    state: &crate::http::AppState,
    source_id: &str,
    expected: SocialSourceGenerationStatus,
) -> bos_contracts::social_publishing::SocialPublishedSource {
    for _ in 0..200 {
        let current = {
            let persistence = state.persistence.lock();
            store::get_source(persistence.connection_ref(), CLIENT, source_id)
                .expect("read source")
                .expect("source")
        };
        if current.generation_status == expected {
            return current;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("source did not reach {expected:?}");
}

#[test]
fn bos_typed_transform_stages_valid_grounded_campaign_without_channel_ids_in_prompt() {
    let _env = EnvGuard::set_many(&[
        ("BOS_BUFFER_CHANNELS_JSON", CHANNELS),
        ("BOS_BUFFER_ACCESS_TOKEN", "super-secret-buffer-token"),
    ]);
    let state = test_state();
    let source = {
        let mut persistence = state.persistence.lock();
        service::ingest_source_request(
            persistence.connection(),
            CLIENT,
            "mcp:openclaw",
            ActorKindDto::Agent,
            &ingress_request("epoxy-guide", "ingress-social-valid"),
            1_000,
        )
        .expect("ingest")
    };
    let request = service::build_social_draft_request(
        CLIENT,
        &source,
        &service::configured_channels().expect("channels"),
        source.excerpt.as_deref().expect("excerpt"),
        "socialgen_test",
        1,
    );
    let serialized = serde_json::to_string(&request).expect("request json");
    assert!(!serialized.contains("buf_linkedin"));
    assert!(!serialized.contains("buf_x"));
    assert!(!serialized.contains("super-secret-buffer-token"));

    service::set_test_social_draft_responses(vec![json!({
        "targets": [
            {
                "target_ref": "target_1",
                "text": "Prepare concrete before the epoxy coating.",
                "utm_source": "linkedin",
                "utm_medium": "social",
                "utm_campaign": "epoxy_guide",
                "utm_content": "article",
                "source_quotes": ["before epoxy coating"]
            },
            {
                "target_ref": "target_2",
                "text": "Diamond grinding removes weak concrete.",
                "utm_source": "x",
                "utm_medium": "social",
                "utm_campaign": "epoxy_guide",
                "utm_content": null,
                "source_quotes": ["Diamond grinding removes weak concrete"]
            }
        ],
        "confidence": "high"
    })]);
    assert!(matches!(
        service::kickoff_generation(
            state.clone(),
            &source.source_id,
            0,
            "generate-social-stale",
            "user_example",
            ActorKindDto::Operator,
        )
        .expect("stale kickoff outcome"),
        service::GenerationKickoffOutcome::Conflict(MutationOutcome::RevisionConflict { .. })
    ));
    assert!(matches!(
        service::kickoff_generation(
            state.clone(),
            &source.source_id,
            source.revision,
            "generate-social-valid",
            "user_example",
            ActorKindDto::Operator,
        )
        .expect("kickoff"),
        service::GenerationKickoffOutcome::Accepted(_)
    ));
    let completed = wait_for_source_status(
        &state,
        &source.source_id,
        SocialSourceGenerationStatus::ProposalStaged,
    );
    let proposal_id = completed.proposal_id.expect("proposal id");
    let persistence = state.persistence.lock();
    let proposal = store::get_proposal(persistence.connection_ref(), CLIENT, &proposal_id)
        .expect("proposal read")
        .expect("proposal");
    assert_eq!(proposal.proposal.status, SocialProposalStatus::Staged);
    assert_eq!(
        proposal.proposal.source_id.as_deref(),
        Some(source.source_id.as_str())
    );
    assert_eq!(proposal.proposal.targets.len(), 2);
    let job_count: i64 = persistence
        .connection_ref()
        .query_row("SELECT COUNT(*) FROM outbox_jobs", [], |row| row.get(0))
        .expect("job count");
    assert_eq!(job_count, 0, "AI staging cannot publish");
}

#[test]
fn ingest_image_url_is_copied_onto_drafted_targets() {
    let _env = EnvGuard::set("BOS_BUFFER_CHANNELS_JSON", CHANNELS);
    let state = test_state();
    let mut request = ingress_request("hero-image", "ingress-hero-image");
    request.image_url = Some("https://cdn.example.com/blog_image/hero".to_string());
    let source = {
        let mut persistence = state.persistence.lock();
        service::ingest_source_request(
            persistence.connection(),
            CLIENT,
            "mcp:openclaw",
            ActorKindDto::Agent,
            &request,
            1_000,
        )
        .expect("ingest")
    };
    assert_eq!(
        source.image_url.as_deref(),
        Some("https://cdn.example.com/blog_image/hero")
    );
    service::set_test_social_draft_responses(vec![json!({
        "targets": [
            {
                "target_ref": "target_1",
                "text": "Prepare concrete before the epoxy coating.",
                "utm_source": "linkedin",
                "utm_medium": "social",
                "utm_campaign": "epoxy_guide",
                "utm_content": "article",
                "source_quotes": ["before epoxy coating"]
            },
            {
                "target_ref": "target_2",
                "text": "Diamond grinding removes weak concrete.",
                "utm_source": "x",
                "utm_medium": "social",
                "utm_campaign": "epoxy_guide",
                "utm_content": null,
                "source_quotes": ["Diamond grinding removes weak concrete"]
            }
        ],
        "confidence": "high"
    })]);
    service::kickoff_generation(
        state.clone(),
        &source.source_id,
        source.revision,
        "generate-hero-image",
        "user_example",
        ActorKindDto::Operator,
    )
    .expect("kickoff");
    let completed = wait_for_source_status(
        &state,
        &source.source_id,
        SocialSourceGenerationStatus::ProposalStaged,
    );
    let persistence = state.persistence.lock();
    let proposal = store::get_proposal(
        persistence.connection_ref(),
        CLIENT,
        completed.proposal_id.as_deref().expect("proposal id"),
    )
    .expect("proposal read")
    .expect("proposal");
    assert_eq!(proposal.proposal.targets.len(), 2);
    assert!(proposal.proposal.targets.iter().all(|target| {
        target.image_url.as_deref() == Some("https://cdn.example.com/blog_image/hero")
    }));
}

#[test]
fn ingest_rejects_non_https_image_url() {
    let state = test_state();
    let mut request = ingress_request("bad-image", "ingress-bad-image");
    request.image_url = Some("http://cdn.example.com/blog_image/hero".to_string());
    let mut persistence = state.persistence.lock();
    let err = service::ingest_source_request(
        persistence.connection(),
        CLIENT,
        "mcp:openclaw",
        ActorKindDto::Agent,
        &request,
        1_000,
    )
    .expect_err("http image");
    assert!(matches!(
        err,
        crate::store_core::StoreError::Domain(code) if code == "social_image_url_invalid"
    ));
    assert!(
        store::list_sources(persistence.connection_ref(), CLIENT, 10)
            .expect("sources")
            .is_empty()
    );
}

#[test]
fn reingress_without_image_url_preserves_existing_image() {
    let state = test_state();
    let mut request = ingress_request("keep-image", "ingress-keep-image");
    request.image_url = Some("https://cdn.example.com/blog_image/hero".to_string());
    let mut persistence = state.persistence.lock();
    service::ingest_source_request(
        persistence.connection(),
        CLIENT,
        "mcp:openclaw",
        ActorKindDto::Agent,
        &request,
        1_000,
    )
    .expect("ingest");
    let mut refresh = ingress_request("keep-image", "ingress-keep-image-refresh");
    refresh.title = "Updated title".to_string();
    let refreshed = service::ingest_source_request(
        persistence.connection(),
        CLIENT,
        "mcp:openclaw",
        ActorKindDto::Agent,
        &refresh,
        2_000,
    )
    .expect("refresh");
    assert_eq!(refreshed.title, "Updated title");
    assert_eq!(
        refreshed.image_url.as_deref(),
        Some("https://cdn.example.com/blog_image/hero")
    );
}

#[test]
fn reingress_with_image_url_replaces_existing_image() {
    let state = test_state();
    let mut request = ingress_request("replace-image", "ingress-replace-image");
    request.image_url = Some("https://cdn.example.com/blog_image/old".to_string());
    let mut persistence = state.persistence.lock();
    service::ingest_source_request(
        persistence.connection(),
        CLIENT,
        "mcp:openclaw",
        ActorKindDto::Agent,
        &request,
        1_000,
    )
    .expect("ingest");
    let mut refresh = ingress_request("replace-image", "ingress-replace-image-refresh");
    refresh.image_url = Some("https://cdn.example.com/blog_image/new".to_string());
    let refreshed = service::ingest_source_request(
        persistence.connection(),
        CLIENT,
        "mcp:openclaw",
        ActorKindDto::Agent,
        &refresh,
        2_000,
    )
    .expect("refresh");
    assert_eq!(
        refreshed.image_url.as_deref(),
        Some("https://cdn.example.com/blog_image/new")
    );
}

#[test]
fn ungrounded_typed_fill_is_retried_then_refused_without_staging() {
    let _env = EnvGuard::set("BOS_BUFFER_CHANNELS_JSON", CHANNELS);
    let state = test_state();
    let source = {
        let mut persistence = state.persistence.lock();
        service::ingest_source_request(
            persistence.connection(),
            CLIENT,
            "mcp:openclaw",
            ActorKindDto::Agent,
            &ingress_request("ungrounded", "ingress-social-invalid"),
            1_000,
        )
        .expect("ingest")
    };
    let invalid = json!({
        "targets": [
            {
                "target_ref": "target_1", "text": "Invented warranty claim.",
                "utm_source": "linkedin", "utm_medium": "social",
                "utm_campaign": "epoxy_guide", "utm_content": null,
                "source_quotes": ["lifetime warranty"]
            },
            {
                "target_ref": "target_2", "text": "Invented warranty claim.",
                "utm_source": "x", "utm_medium": "social",
                "utm_campaign": "epoxy_guide", "utm_content": null,
                "source_quotes": ["lifetime warranty"]
            }
        ],
        "confidence": "high"
    });
    service::set_test_social_draft_responses(vec![invalid.clone(), invalid]);
    service::kickoff_generation(
        state.clone(),
        &source.source_id,
        source.revision,
        "generate-social-invalid",
        "user_example",
        ActorKindDto::Operator,
    )
    .expect("kickoff");
    let failed = wait_for_source_status(
        &state,
        &source.source_id,
        SocialSourceGenerationStatus::GenerationFailed,
    );
    assert_eq!(
        failed.generation_error.as_deref(),
        Some("social_draft_grounding_invalid")
    );
    let persistence = state.persistence.lock();
    assert!(
        store::list_proposals(persistence.connection_ref(), CLIENT, 10)
            .expect("proposals")
            .is_empty()
    );
}

fn adhoc_request(key: &str, link_url: Option<&str>) -> SocialAdhocSourceCreateRequest {
    SocialAdhocSourceCreateRequest {
        title: "Closed Christmas Day".to_string(),
        grounding_text: "The shop is closed December 25. Regular hours resume December 26."
            .to_string(),
        link_url: link_url.map(str::to_string),
        idempotency_key: key.to_string(),
    }
}

#[test]
fn adhoc_source_without_url_stages_grounded_copy_and_skips_tracked_link() {
    let _env = EnvGuard::set("BOS_BUFFER_CHANNELS_JSON", CHANNELS);
    let state = test_state();
    let source = {
        let mut persistence = state.persistence.lock();
        service::ingest_adhoc_source_request(
            persistence.connection(),
            CLIENT,
            "mcp:openclaw",
            ActorKindDto::Agent,
            &adhoc_request("adhoc-closed", None),
            1_000,
        )
        .expect("ingest adhoc")
    };
    assert_eq!(source.source_kind, service::ADHOC_SOURCE_KIND);
    assert_eq!(source.canonical_url, None);
    assert_eq!(
        source.excerpt.as_deref(),
        Some("The shop is closed December 25. Regular hours resume December 26.")
    );

    service::set_test_social_draft_responses(vec![json!({
        "targets": [
            {
                "target_ref": "target_1",
                "text": "We are closed December 25.",
                "utm_source": "",
                "utm_medium": "",
                "utm_campaign": "",
                "utm_content": null,
                "source_quotes": ["closed December 25"]
            },
            {
                "target_ref": "target_2",
                "text": "Regular hours resume December 26.",
                "utm_source": "linkedin",
                "utm_medium": "social",
                "utm_campaign": "holiday",
                "utm_content": null,
                "source_quotes": ["Regular hours resume December 26"]
            }
        ],
        "confidence": "high"
    })]);
    assert!(matches!(
        service::kickoff_generation(
            state.clone(),
            &source.source_id,
            source.revision,
            "generate-adhoc-closed",
            "social_draft_generator",
            ActorKindDto::System,
        )
        .expect("kickoff"),
        service::GenerationKickoffOutcome::Accepted(_)
    ));
    let staged = wait_for_source_status(
        &state,
        &source.source_id,
        SocialSourceGenerationStatus::ProposalStaged,
    );
    let persistence = state.persistence.lock();
    let proposal = store::get_proposal(
        persistence.connection_ref(),
        CLIENT,
        staged.proposal_id.as_deref().expect("proposal id"),
    )
    .expect("read")
    .expect("proposal");
    assert_eq!(proposal.proposal.canonical_url, None);
    assert_eq!(proposal.proposal.targets[0].tracked_url, "");
    assert_eq!(
        proposal.proposal.targets[0].text,
        "We are closed December 25."
    );
    assert!(!proposal.proposal.targets[0].text.contains("https://"));
    assert_eq!(
        proposal.proposal.targets[0].utm,
        SocialUtmParameters::default()
    );
    assert_eq!(
        proposal.proposal.targets[1].utm,
        SocialUtmParameters::default()
    );
}

#[test]
fn adhoc_source_with_link_keeps_tracked_url_and_rejects_identity_drift() {
    let _env = EnvGuard::set("BOS_BUFFER_CHANNELS_JSON", CHANNELS);
    let state = test_state();
    let mut persistence = state.persistence.lock();
    let source = service::ingest_adhoc_source_request(
        persistence.connection(),
        CLIENT,
        "mcp:openclaw",
        ActorKindDto::Agent,
        &adhoc_request("adhoc-offer", Some("https://example.com/holiday-hours")),
        1_000,
    )
    .expect("ingest linked adhoc");
    assert_eq!(
        source.canonical_url.as_deref(),
        Some("https://example.com/holiday-hours")
    );
    let replayed = service::ingest_adhoc_source_request(
        persistence.connection(),
        CLIENT,
        "mcp:openclaw",
        ActorKindDto::Agent,
        &adhoc_request("adhoc-offer", Some("https://example.com/holiday-hours")),
        1_100,
    )
    .expect("replay");
    assert_eq!(replayed.revision, source.revision);
    let err = service::ingest_adhoc_source_request(
        persistence.connection(),
        CLIENT,
        "mcp:openclaw",
        ActorKindDto::Agent,
        &adhoc_request("adhoc-offer", Some("https://example.com/other")),
        1_200,
    )
    .expect_err("same key cannot change destination");
    assert!(matches!(
        err,
        crate::store_core::StoreError::Domain(code)
            if code == "social_published_source_identity_changed"
    ));
    let other = service::ingest_adhoc_source_request(
        persistence.connection(),
        CLIENT,
        "mcp:openclaw",
        ActorKindDto::Agent,
        &adhoc_request("adhoc-offer-new", Some("https://example.com/other")),
        1_300,
    )
    .expect("new key is a new source");
    assert_ne!(other.source_id, source.source_id);
    assert_eq!(
        other.canonical_url.as_deref(),
        Some("https://example.com/other")
    );
}

#[test]
fn adhoc_stage_rejects_utm_when_there_is_no_destination() {
    let _env = EnvGuard::set("BOS_BUFFER_CHANNELS_JSON", CHANNELS);
    let state = test_state();
    let mut request = stage_request("adhoc-utm-rejected");
    request.canonical_url.clear();
    let mut persistence = state.persistence.lock();
    let err = service::stage_request(
        persistence.connection(),
        CLIENT,
        "user_example",
        ActorKindDto::Operator,
        &request,
        1_000,
    )
    .expect_err("utm without destination");
    assert!(matches!(
        err,
        crate::store_core::StoreError::Domain(code) if code == "social_utm_without_destination"
    ));
}
