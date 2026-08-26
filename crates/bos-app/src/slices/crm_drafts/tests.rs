//! Slice tests: note-fill parsing, grounded occurred_at, the stage → approve
//! → outbox lifecycle, and gated dry-run delivery. No live LLM or network.

use bos_contracts::crm_drafts::CrmDraftStatus;
use bos_contracts::email_triage::InboundMessageRecord;
use bos_contracts::work_queue::{WorkItem, WorkItemStatus};
use bos_integrations::hubspot::{HubSpotNoteCreateOutboxPayload, HubSpotWriteConfig};
use serde_json::json;

use super::service;
use super::store::{self, DraftActionContext};
use crate::http::OperatorScope;
use crate::outbox::{self, AttemptOutcome};
use crate::persistence::Persistence;
use crate::store_core::{MutationOutcome, StoreError};

const CLIENT: &str = "test-client";

fn accepted_item() -> WorkItem {
    WorkItem {
        item_id: "wi_email_m3".to_string(),
        source_kind: "email".to_string(),
        source_ref: "m3".to_string(),
        category_id: "call_log".to_string(),
        title: "Ruby Call Summary — Dana".to_string(),
        summary: "Dana called about the storefront quote".to_string(),
        packet_kinds: vec!["crm_activity".to_string()],
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
        source_key: "m3".to_string(),
        message_id: "m3".to_string(),
        thread_id: None,
        internal_date_ms: Some(1_781_094_896_000), // 2026-06-10T12:34:56Z
        from_addr: Some("summaries@business-8edee2ecb0.example.test".to_string()),
        to_addr: Some("jordan@business-e48a50b69d.example.test".to_string()),
        subject: Some("Ruby Call Summary".to_string()),
        body_excerpt: "Dana (dana@example.test) called about a storefront repaint quote."
            .to_string(),
        body_full: String::new(),
        headers: Vec::new(),
        labels: vec!["Ruby Call Summary".to_string()],
        resolved_category: "call_log".to_string(),
        matched_rule_id: Some("ruby_call_summary".to_string()),
        ingested_at_ms: 1_000,
        ai_triage_status: None,
        ai_triage_rationale: None,
        attachments: Vec::new(),
        source_user_id: None,
    }
}

fn valid_fill_response() -> serde_json::Value {
    json!({
        "note_body": "Dana called about a storefront repaint quote; wants pricing this week.",
        "contact_email": "dana@example.test",
        "confidence": "high",
        "provenance": [
            {"field": "note_body", "quote": "called about a storefront repaint quote"},
            {"field": "contact_email", "quote": "dana@example.test"},
            {"field": "bogus", "quote": "dropped"}
        ]
    })
}

fn staged_draft(conn: &mut rusqlite::Connection) -> String {
    staged_draft_with_source(conn, "wi_email_m3", None, "produce_1")
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
    let fill = service::parse_note_fill_response(&valid_fill_response()).expect("fill");
    let draft = service::draft_from_fill(&item, &source_message(), &fill, 1, "test-model", 2_000);
    store::insert_draft(conn, CLIENT, "op_test", &draft, idempotency_key).expect("stage");
    draft.draft_id
}

#[test]
fn parse_keeps_known_provenance_and_validates_email() {
    let fill = service::parse_note_fill_response(&valid_fill_response()).expect("fill");
    assert_eq!(fill.contact_email.as_deref(), Some("dana@example.test"));
    let fields: Vec<&str> = fill.provenance.iter().map(|p| p.field.as_str()).collect();
    assert_eq!(fields, vec!["note_body", "contact_email"]);

    let mut bad_email = valid_fill_response();
    bad_email["contact_email"] = json!("not an email");
    let fill = service::parse_note_fill_response(&bad_email).expect("fill");
    assert_eq!(fill.contact_email, None, "invalid email dropped, not fatal");

    let mut no_body = valid_fill_response();
    no_body["note_body"] = json!("");
    assert!(service::parse_note_fill_response(&no_body).is_err());
}

#[test]
fn occurred_at_is_grounded_from_the_email_date() {
    let fill = service::parse_note_fill_response(&valid_fill_response()).expect("fill");
    let draft = service::draft_from_fill(
        &accepted_item(),
        &source_message(),
        &fill,
        1,
        "test-model",
        9_999_999,
    );
    assert_eq!(draft.occurred_at, "2026-06-10T12:34:56Z");
}

#[test]
fn draft_from_fill_inherits_item_source_user() {
    let mut item = accepted_item();
    item.source_user_id = Some("user_jordan".to_string());
    let fill = service::parse_note_fill_response(&valid_fill_response()).expect("fill");

    let draft = service::draft_from_fill(&item, &source_message(), &fill, 1, "test-model", 2_000);

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
        "Blocked edit",
        Some("blocked@example.test"),
    )
    .expect_err("cross-scope update rejected");
    assert!(matches!(err, StoreError::Domain(code) if code == "scope_forbidden"));

    let err = store::reject_draft(conn, cross_scope("reject_cross"), &reject_id)
        .expect_err("cross-scope reject rejected");
    assert!(matches!(err, StoreError::Domain(code) if code == "scope_forbidden"));

    let draft = store::get_draft(conn, CLIENT, &approve_id, &OperatorScope::All)
        .expect("get")
        .expect("exists");
    let job = service::build_approval_job(&draft.draft, "u1", 5_000, service::PROVIDER_HUBSPOT)
        .expect("job");
    let err = store::approve_draft(conn, cross_scope("approve_cross"), &approve_id, &job)
        .expect_err("cross-scope approve rejected");
    assert!(matches!(err, StoreError::Domain(code) if code == "scope_forbidden"));
}

#[test]
fn approve_enqueues_hubspot_job_with_contact_and_source_in_note() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let draft_id = staged_draft(conn);
    let draft = store::get_draft(conn, CLIENT, &draft_id, &OperatorScope::All)
        .expect("get")
        .expect("exists");
    let job =
        service::build_approval_job(&draft.draft, "op_test", 5_000, service::PROVIDER_HUBSPOT)
            .expect("job");
    assert_eq!(job.provider, "hubspot");
    assert_eq!(job.capability, "create_note");
    let payload: HubSpotNoteCreateOutboxPayload =
        serde_json::from_str(&job.payload_json).expect("payload");
    assert!(payload.note_body.contains("Contact: dana@example.test"));
    assert!(payload.note_body.contains("Source: email m3"));
    assert_eq!(payload.occurred_at, "2026-06-10T12:34:56Z");

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
    )
    .expect("approve");
    assert!(matches!(outcome, MutationOutcome::Applied { .. }));

    let approved = store::get_draft(conn, CLIENT, &draft_id, &OperatorScope::All)
        .expect("get")
        .expect("exists");
    assert_eq!(approved.draft.status, CrmDraftStatus::Approved);
    assert_eq!(
        approved.outbox_job.expect("summary").status,
        outbox::STATUS_PENDING
    );
}

#[test]
fn second_active_draft_refused_and_reject_frees_reproduce() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let draft_id = staged_draft(conn);

    let fill = service::parse_note_fill_response(&valid_fill_response()).expect("fill");
    let second = service::draft_from_fill(
        &accepted_item(),
        &source_message(),
        &fill,
        2,
        "test-model",
        3_000,
    );
    let err = store::insert_draft(conn, CLIENT, "op_test", &second, "produce_2")
        .expect_err("second active draft must be refused");
    assert!(matches!(err, StoreError::Domain(code) if code == "crm_draft_already_active"));

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

#[test]
fn approved_draft_delivers_dry_run_while_gate_closed() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let draft_id = staged_draft(conn);
    let draft = store::get_draft(conn, CLIENT, &draft_id, &OperatorScope::All)
        .expect("get")
        .expect("exists");
    let job =
        service::build_approval_job(&draft.draft, "op_test", 5_000, service::PROVIDER_HUBSPOT)
            .expect("job");
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
        &job,
    )
    .expect("approve");

    let claimed =
        outbox::claim_due_jobs(conn, CLIENT, Some("hubspot"), 60_000, 10, 6_000).expect("claim");
    assert_eq!(claimed.len(), 1);

    // Gate closed → dry-run client, no network, delivered with dry_run=true.
    let config = HubSpotWriteConfig {
        access_token: Some("tok".to_string()),
        write_enabled: false,
    };
    let outcome = service::execute_job(&claimed[0], &config, 6_000);
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
    let summary = final_draft.outbox_job.expect("summary");
    assert_eq!(summary.status, outbox::STATUS_DELIVERED);
    assert_eq!(summary.dry_run, Some(true));
}

#[test]
fn unsupported_or_malformed_jobs_are_terminal() {
    let config = HubSpotWriteConfig {
        access_token: None,
        write_enabled: false,
    };
    let mut job = outbox::ClaimedJob {
        job_id: "obj_x".to_string(),
        provider: "hubspot".to_string(),
        capability: "create_note".to_string(),
        payload_json: "{not json".to_string(),
        attempts: 0,
        source_entity_kind: "x".to_string(),
        source_entity_id: "x".to_string(),
        correlation_id: None,
        idempotency_key: "k".to_string(),
    };
    assert!(matches!(
        service::execute_job(&job, &config, 1_000),
        AttemptOutcome::Terminal { .. }
    ));

    job.capability = "delete_everything".to_string();
    assert!(matches!(
        service::execute_job(&job, &config, 1_000),
        AttemptOutcome::Terminal { .. }
    ));
}

#[test]
fn espocrm_provider_builds_and_dry_run_delivers_through_its_arm() {
    use bos_integrations::espocrm::{EspoCrmNoteCreateOutboxPayload, EspoCrmWriteConfig};

    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let draft_id = staged_draft(conn);
    let draft = store::get_draft(conn, CLIENT, &draft_id, &OperatorScope::All)
        .expect("get")
        .expect("exists");

    let job =
        service::build_approval_job(&draft.draft, "op_test", 5_000, service::PROVIDER_ESPOCRM)
            .expect("job");
    assert_eq!(job.provider, "espocrm");
    assert_eq!(job.capability, "create_note");
    let payload: EspoCrmNoteCreateOutboxPayload =
        serde_json::from_str(&job.payload_json).expect("payload");
    assert!(payload.note_body.contains("Contact: dana@example.test"));
    assert!(payload.note_body.contains("Source: email m3"));
    assert_eq!(payload.occurred_at, "2026-06-10T12:34:56Z");
    // D3: the contact email rides structured so delivery can resolve it to a
    // Contact and attach the note to that record.
    assert_eq!(payload.contact_email.as_deref(), Some("dana@example.test"));

    store::approve_draft(
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
    )
    .expect("approve");

    let claimed =
        outbox::claim_due_jobs(conn, CLIENT, Some("espocrm"), 60_000, 10, 6_000).expect("claim");
    assert_eq!(claimed.len(), 1);

    // Gate closed → dry-run client, no network, delivered with dry_run=true.
    let config = EspoCrmWriteConfig {
        base_url: Some("http://localhost:4580".to_string()),
        api_key: Some("key".to_string()),
        write_enabled: false,
    };
    let outcome = service::execute_espocrm_job(&claimed[0], &config, 6_000);
    let AttemptOutcome::Delivered { result_json } = &outcome else {
        panic!("expected delivered, got {outcome:?}");
    };
    let result: serde_json::Value = serde_json::from_str(result_json).expect("json");
    assert_eq!(result["dry_run"], json!(true));

    // The espocrm executor refuses other providers' jobs.
    let mut foreign = claimed[0].clone();
    foreign.provider = "hubspot".to_string();
    assert!(matches!(
        service::execute_espocrm_job(&foreign, &config, 6_000),
        AttemptOutcome::Terminal { .. }
    ));
}

#[test]
fn unknown_crm_provider_is_a_build_error() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let draft_id = staged_draft(conn);
    let draft = store::get_draft(conn, CLIENT, &draft_id, &OperatorScope::All)
        .expect("get")
        .expect("exists");
    assert!(service::build_approval_job(&draft.draft, "op_test", 5_000, "salesforce").is_err());
}

#[test]
fn crm_records_autoadd_is_needed_for_name_only_or_unmatched_email() {
    let matches = crate::slices::crm_record_drafts::service::RecordMatches::default();
    assert!(
        service::records_autoadd_needed(None, &matches),
        "name-only notes need crm_record_create so name-based search can decide"
    );
    assert!(
        service::records_autoadd_needed(Some("dana@example.test"), &matches),
        "an email miss still needs a records draft"
    );

    let matches = crate::slices::crm_record_drafts::service::RecordMatches {
        account_id: None,
        contact_id: Some("contact_1".to_string()),
    };
    assert!(
        !service::records_autoadd_needed(Some("dana@example.test"), &matches),
        "an existing contact can receive the CRM note without a records draft"
    );
}

#[test]
fn crm_records_autoadd_and_produce_use_distinct_idempotency_keys() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let item = accepted_item();
    crate::slices::work_queue::store::insert_item(conn, CLIENT, &item).expect("insert item");

    let records_kind = crate::slices::crm_record_drafts::service::PACKET_KIND.to_string();
    let mut kinds = item.packet_kinds.clone();
    kinds.push(records_kind);
    let kinds_key = service::records_autoadd_kinds_key(&item.item_id);
    let produce_key = service::records_autoadd_produce_key(&item.item_id);
    assert_ne!(kinds_key, produce_key);

    crate::slices::work_queue::store::update_packet_kinds(
        conn,
        crate::slices::work_queue::store::ItemActionContext {
            client_id: CLIENT,
            actor_id: "crm_records_autoadd",
            scope: &crate::http::OperatorScope::All,
            expected_revision: None,
            idempotency_key: &kinds_key,
            now_ms: 2_000,
        },
        &item.item_id,
        &kinds,
    )
    .expect("auto-add records kind");

    let draft = bos_contracts::crm_record_drafts::CrmRecordDraft {
        draft_id: format!("crd_{}_1", item.item_id),
        item_id: item.item_id.clone(),
        source_kind: item.source_kind.clone(),
        source_ref: item.source_ref.clone(),
        status: bos_contracts::crm_record_drafts::CrmRecordDraftStatus::Staged,
        create_company: false,
        company_name: None,
        company_website: None,
        company_phone: None,
        company_address: None,
        company_description: None,
        create_contact: true,
        contact_first_name: Some("Dana".to_string()),
        contact_last_name: None,
        contact_email: Some("dana@example.test".to_string()),
        contact_phone: None,
        contact_title: None,
        provider_ids: Default::default(),
        provenance: Vec::new(),
        enrichment_trace: None,
        research_annotations: Vec::new(),
        model: "test-model".to_string(),
        confidence: "high".to_string(),
        outbox_job_id: None,
        created_at_ms: 3_000,
        updated_at_ms: 3_000,
    };
    crate::slices::crm_record_drafts::store::insert_draft(
        conn,
        CLIENT,
        "crm_records_autoadd",
        &draft,
        &produce_key,
    )
    .expect("records produce stages draft");

    let staged =
        crate::slices::crm_record_drafts::store::active_draft_for_item(conn, CLIENT, &item.item_id)
            .expect("lookup")
            .expect("records draft present");
    assert_eq!(
        staged.draft.contact_email.as_deref(),
        Some("dana@example.test")
    );
}

#[test]
fn note_fill_request_includes_background_when_present() {
    use bos_integrations::llm_typed_tasks::TypedLlmTextBlock;
    let item = accepted_item();
    let message = source_message();

    // No background in context → no background block.
    let plain = service::build_note_fill_request(
        CLIENT,
        &item,
        &message,
        &json!({ "background": null }),
        1,
    );
    assert!(
        !plain
            .input
            .text_blocks
            .iter()
            .any(|b| b.block_id == "background"),
        "absent background must not add a block"
    );

    // A serialized block in context → exactly one background block, text intact.
    let block = TypedLlmTextBlock {
        block_id: "background".to_string(),
        text: "Company: Acme Coatings".to_string(),
    };
    let context = json!({ "background": serde_json::to_value(&block).unwrap() });
    let grounded = service::build_note_fill_request(CLIENT, &item, &message, &context, 1);
    let backgrounds: Vec<_> = grounded
        .input
        .text_blocks
        .iter()
        .filter(|b| b.block_id == "background")
        .collect();
    assert_eq!(backgrounds.len(), 1);
    assert_eq!(backgrounds[0].text, "Company: Acme Coatings");
}
