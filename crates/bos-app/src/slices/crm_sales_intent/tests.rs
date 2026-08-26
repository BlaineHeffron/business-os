use bos_contracts::crm_sales_intent::CrmSalesIntentProviderTarget;
use bos_integrations::espocrm::EspoCrmWriteConfig;
use rusqlite::params;

use super::{service, store};
use crate::http::OperatorScope;
use crate::persistence::Persistence;
use crate::slices::mutation_context::ScopedMutationContext;

#[test]
fn parse_fill_keeps_sales_intent_separate_from_records() {
    let fill = service::parse_fill_response(&serde_json::json!({
        "company_name": "Acme",
        "contact_name": "Sarah",
        "contact_email": "sarah@example.test",
        "lead_title": "Acme wholesale pricing",
        "intent_summary": "Sarah asked about wholesale pricing.",
        "rationale": "The source says Sarah is interested in wholesale pricing.",
        "qualification_status": "qualified",
        "next_step_text": "Follow up next Tuesday.",
        "follow_up_due_date": "2026-06-30",
        "provider_target": "lead",
        "create_businessos_task": true,
        "confidence": "high",
        "provenance": [
            {"field": "lead_title", "quote": "interested in wholesale pricing"}
        ]
    }))
    .expect("valid fill");
    assert_eq!(fill.company_name.as_deref(), Some("Acme"));
    assert_eq!(fill.provider_target, CrmSalesIntentProviderTarget::Lead);
    assert!(fill.create_businessos_task);
    assert_eq!(fill.provenance.len(), 1);
}

#[test]
fn parse_fill_rejects_bad_target_and_date() {
    let bad_target = service::parse_fill_response(&serde_json::json!({
        "lead_title": "Acme wholesale pricing",
        "intent_summary": "Sarah asked about wholesale pricing.",
        "rationale": "The source says Sarah is interested in wholesale pricing.",
        "qualification_status": "qualified",
        "next_step_text": "Follow up.",
        "provider_target": "contact",
        "confidence": "high",
    }))
    .expect_err("unknown provider target should fail typed validation");
    assert!(bad_target.contains("provider_target invalid"));

    let bad_date = service::parse_fill_response(&serde_json::json!({
        "lead_title": "Acme wholesale pricing",
        "intent_summary": "Sarah asked about wholesale pricing.",
        "rationale": "The source says Sarah is interested in wholesale pricing.",
        "qualification_status": "qualified",
        "next_step_text": "Follow up.",
        "follow_up_due_date": "next Tuesday",
        "provider_target": "lead",
        "confidence": "high",
    }))
    .expect_err("invalid follow-up date should fail typed validation");
    assert!(bad_date.contains("follow_up_due_date"));
}

#[test]
fn approval_job_supports_only_espocrm_lead_target() {
    let lead_draft = draft(CrmSalesIntentProviderTarget::Lead);
    let job = service::build_approval_job(
        &lead_draft,
        "op",
        1_780_000_000_000,
        crate::slices::crm_drafts::service::PROVIDER_ESPOCRM,
    )
    .expect("espocrm lead job");
    assert_eq!(
        job.provider,
        crate::slices::crm_drafts::service::PROVIDER_ESPOCRM
    );
    assert_eq!(job.capability, service::CAPABILITY_CREATE_LEAD);

    let unsupported_provider = service::build_approval_job(&lead_draft, "op", 1, "hubspot")
        .expect_err("hubspot lead mapping is not implemented");
    assert_eq!(
        unsupported_provider,
        "crm_sales_intent_provider_unsupported"
    );

    let unsupported_target = service::build_approval_job(
        &draft(CrmSalesIntentProviderTarget::Deal),
        "op",
        1,
        crate::slices::crm_drafts::service::PROVIDER_ESPOCRM,
    )
    .expect_err("deal mapping is not implemented");
    assert_eq!(unsupported_target, "crm_sales_intent_target_unsupported");
}

#[test]
fn approve_enqueues_lead_job_and_creates_follow_up_task() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let draft = draft(CrmSalesIntentProviderTarget::Lead);
    store::insert_draft(conn, "test-client", "op", &draft, "produce_1").expect("stage");
    let job = service::build_approval_job(
        &draft,
        "op",
        1_780_000_000_000,
        crate::slices::crm_drafts::service::PROVIDER_ESPOCRM,
    )
    .expect("job");
    let task = service::task_from_draft(&draft, 1_780_000_000_000);
    store::approve_draft(
        conn,
        ScopedMutationContext {
            client_id: "test-client",
            actor_id: "op",
            scope: &OperatorScope::All,
            expected_revision: Some(1),
            idempotency_key: "approve_1",
            now_ms: 1_780_000_000_000,
        },
        &draft.draft_id,
        &job,
        Some(&task),
    )
    .expect("approve");

    let outbox_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM outbox_jobs WHERE client_id = ?1 AND job_id = ?2",
            params!["test-client", job.job_id],
            |row| row.get(0),
        )
        .expect("outbox count");
    assert_eq!(outbox_count, 1);
    let task_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tasks WHERE client_id = ?1 AND task_id = ?2 AND status = 'open'",
            params!["test-client", task.task_id],
            |row| row.get(0),
        )
        .expect("task count");
    assert_eq!(task_count, 1);
}

#[test]
fn espocrm_lead_dry_run_delivers() {
    let draft = draft(CrmSalesIntentProviderTarget::Lead);
    let job = service::build_approval_job(
        &draft,
        "op",
        1_780_000_000_000,
        crate::slices::crm_drafts::service::PROVIDER_ESPOCRM,
    )
    .expect("job");
    let claimed = crate::outbox::ClaimedJob {
        job_id: job.job_id,
        provider: job.provider,
        capability: job.capability,
        payload_json: job.payload_json,
        attempts: 0,
        source_entity_kind: job.source_entity_kind,
        source_entity_id: job.source_entity_id,
        correlation_id: job.correlation_id,
        idempotency_key: job.idempotency_key,
    };
    let outcome = service::execute_espocrm_job(
        &claimed,
        &EspoCrmWriteConfig {
            base_url: None,
            api_key: None,
            write_enabled: false,
        },
        1,
    );
    assert!(matches!(
        outcome,
        crate::outbox::AttemptOutcome::Delivered { .. }
    ));
}

fn draft(
    target: CrmSalesIntentProviderTarget,
) -> bos_contracts::crm_sales_intent::CrmSalesIntentDraft {
    bos_contracts::crm_sales_intent::CrmSalesIntentDraft {
        draft_id: "csi_wi_1_1".to_string(),
        item_id: "wi_1".to_string(),
        source_kind: "email".to_string(),
        source_ref: "msg_1".to_string(),
        source_user_id: None,
        status: bos_contracts::crm_sales_intent::CrmSalesIntentDraftStatus::Staged,
        company_name: Some("Acme".to_string()),
        contact_name: Some("Sarah".to_string()),
        contact_email: Some("sarah@example.test".to_string()),
        lead_title: "Acme wholesale pricing".to_string(),
        intent_summary: "Sarah asked about wholesale pricing.".to_string(),
        rationale: "Explicit pricing interest.".to_string(),
        qualification_status: "qualified".to_string(),
        next_step_text: "Follow up next Tuesday.".to_string(),
        follow_up_due_date: Some("2026-06-30".to_string()),
        provider_target: target,
        create_businessos_task: true,
        provenance: Vec::new(),
        model: "test".to_string(),
        confidence: "high".to_string(),
        outbox_job_id: None,
        created_at_ms: 1,
        updated_at_ms: 1,
    }
}
