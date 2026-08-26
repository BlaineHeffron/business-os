//! Slice tests: name grounding in the fill, the missing-records proposal,
//! the approval gate, and the stage → approve store round-trip.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use bos_contracts::crm_record_drafts::{
    CrmRecordDraft, CrmRecordDraftStatus, CrmRecordProviderIds, CrmResearchFieldAnnotation,
};
use bos_contracts::enrichment::{
    EnrichmentConfidence, EnrichmentEligibility, EnrichmentFieldProposal, EnrichmentFieldSpec,
    EnrichmentMode, EnrichmentPlan, EnrichmentRunStatus, EnrichmentSeedEvidence, EnrichmentTier,
    EnrichmentTierEvent,
};
use bos_contracts::operator_notes::OperatorNote;
use bos_contracts::work_queue::{WorkItem, WorkItemStatus};
use serde_json::json;
use std::collections::BTreeMap;
use tower::ServiceExt;

use super::service::{self, RecordFill, RecordMatches};
use super::store::{self, DraftActionContext};
use crate::http::{build_router, test_support, test_support::EnvGuard};
use crate::persistence::Persistence;
use crate::slices::enrichment::store as enrichment_store;

const CLIENT: &str = "test-client";

fn accepted_item() -> WorkItem {
    WorkItem {
        item_id: "wi_operator_note_note_1".to_string(),
        source_kind: "operator_note".to_string(),
        source_ref: "note_1".to_string(),
        category_id: "operator_note".to_string(),
        title: "Went to examplecompany HQ".to_string(),
        summary: String::new(),
        packet_kinds: vec!["crm_record_create".to_string()],
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

fn source_message(
    body_excerpt: &str,
    body_full: &str,
) -> bos_contracts::email_triage::InboundMessageRecord {
    bos_contracts::email_triage::InboundMessageRecord {
        source_key: "m_wholesale".to_string(),
        message_id: "m_wholesale".to_string(),
        thread_id: None,
        internal_date_ms: Some(1_000),
        from_addr: Some("ask@business-914f630770.example.test".to_string()),
        to_addr: Some("casey@business-914f630770.example.test".to_string()),
        subject: Some("Fwd: New Wholesale Account Application".to_string()),
        body_excerpt: body_excerpt.to_string(),
        body_full: body_full.to_string(),
        headers: Vec::new(),
        labels: Vec::new(),
        resolved_category: "wholesale_application".to_string(),
        matched_rule_id: Some("wholesale_account_application".to_string()),
        ingested_at_ms: 1_000,
        ai_triage_status: None,
        ai_triage_rationale: None,
        attachments: Vec::new(),
        source_user_id: None,
    }
}

fn grounded_fill_response() -> serde_json::Value {
    json!({
        "company_name": "examplecompany",
        "company_website": "business-63644db2f2.example.test",
        "contact_first_name": "casey",
        "contact_last_name": "Sullivan",
        "contact_email": "casey@business-63644db2f2.example.test",
        "contact_phone": null,
        "contact_title": null,
        "confidence": "high",
        "provenance": [
            {"field": "company_name", "quote": "examplecompany uses business-63644db2f2.example.test"},
            {"field": "contact_name", "quote": "casey Sullivan is the contact"}
        ]
    })
}

#[test]
fn grounded_names_survive_and_ungrounded_ones_drop() {
    let fill = service::parse_record_fill_response(&grounded_fill_response()).expect("parse");
    assert_eq!(fill.company_name.as_deref(), Some("examplecompany"));
    assert_eq!(
        fill.company_website.as_deref(),
        Some("https://business-63644db2f2.example.test/")
    );
    assert_eq!(fill.contact_first_name.as_deref(), Some("casey"));
    assert_eq!(
        fill.contact_email.as_deref(),
        Some("casey@business-63644db2f2.example.test")
    );

    // An invented company name (no provenance quote containing it) is dropped.
    let ungrounded = json!({
        "company_name": "Globex",
        "contact_first_name": "casey",
        "contact_last_name": "Sullivan",
        "confidence": "low",
        "provenance": [
            {"field": "contact_name", "quote": "casey Sullivan is the contact"}
        ]
    });
    let fill = service::parse_record_fill_response(&ungrounded).expect("parse");
    assert_eq!(fill.company_name, None, "invented company name is refused");
    assert_eq!(fill.contact_first_name.as_deref(), Some("casey"));
}

#[test]
fn record_fill_normalizes_deep_website_to_homepage() {
    let response = json!({
        "company_name": "example_retailer Technology",
        "company_website": "https://retailer.example.test/aboutus/",
        "contact_first_name": "Trevor",
        "contact_last_name": "Kirkpatrick",
        "confidence": "high",
        "provenance": [
            {"field": "company_name", "quote": "example_retailer Technology"},
            {"field": "company_website", "quote": "https://retailer.example.test/aboutus/"},
            {"field": "contact_name", "quote": "Trevor Kirkpatrick"}
        ]
    });

    let fill = service::parse_record_fill_response(&response).expect("parse");

    assert_eq!(
        fill.company_website.as_deref(),
        Some("https://retailer.example.test/")
    );
}

#[test]
fn record_fill_accepts_multiple_grounded_contacts() {
    let response = json!({
        "company_name": "example_retailer Technology",
        "company_website": "https://retailer.example.test/aboutus/",
        "contacts": [
            {
                "first_name": "Trevor",
                "last_name": "Kirkpatrick",
                "email": "trevor@retailer.example.test",
                "phone": null,
                "title": null,
                "quote": "Trevor Kirkpatrick works at example_retailer Technology"
            },
            {
                "first_name": "Zach",
                "last_name": "Hodgeboom",
                "email": null,
                "phone": null,
                "title": "Owner",
                "quote": "Zach Hodgeboom is owner"
            },
            {
                "first_name": "Made",
                "last_name": "Up",
                "email": null,
                "phone": null,
                "title": null,
                "quote": "not in the note"
            }
        ],
        "confidence": "high",
        "provenance": [
            {"field": "company_name", "quote": "example_retailer Technology"},
            {"field": "company_website", "quote": "https://retailer.example.test/aboutus/"}
        ]
    });

    let fill = service::parse_record_fill_response(&response).expect("parse");

    assert_eq!(fill.contacts.len(), 2);
    assert_eq!(fill.contact_first_name.as_deref(), Some("Trevor"));
    assert_eq!(
        fill.contacts[1].contact_title.as_deref(),
        Some("Owner"),
        "contact object title is retained when the person is grounded"
    );
}

#[test]
fn record_fill_request_uses_raw_full_email_body() {
    let item = WorkItem {
        item_id: "wi_email_wholesale".to_string(),
        source_kind: "email".to_string(),
        source_ref: "m_wholesale".to_string(),
        category_id: "wholesale_application".to_string(),
        title: "Wholesale application".to_string(),
        summary: String::new(),
        packet_kinds: vec!["crm_record_create".to_string()],
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
    };
    let full = "\
---------- Forwarded message ---------
From: Form Submit <info@business-014bb695de.example.test>
Subject: New Wholesale Account Application

Business Name: Taylor Repair Service
Primary Contact: Davey Jones
Primary Contact Email: info@business-df29801f39.example.test
Current Average Annual Purchases: $15000-$30000";
    let message = source_message("Business Name: Taylor Repair Service", full);

    let request =
        service::build_record_fill_request(CLIENT, &item, &message, &serde_json::json!({}), 1);
    let text = &request.input.text_blocks[0].text;

    assert!(
        text.contains("---------- Forwarded message ---------"),
        "{text}"
    );
    assert!(text.contains("From: Form Submit <info@business-014bb695de.example.test>"));
    assert!(text.contains("Primary Contact Email: info@business-df29801f39.example.test"));
    assert!(text.contains("Current Average Annual Purchases: $15000-$30000"));
}

#[test]
fn draft_proposes_only_the_missing_records() {
    let fill = service::parse_record_fill_response(&grounded_fill_response()).expect("parse");
    let item = accepted_item();

    // Neither exists → propose both.
    let draft = service::draft_from_fill(&item, &fill, &RecordMatches::default(), 1, "m", 2_000)
        .expect("draft");
    assert!(draft.create_company && draft.create_contact);

    // Company exists → propose only the contact, but keep the company name so
    // the ensure-chain can link the new contact to it.
    let matched_company = RecordMatches {
        account_id: Some("acc-1".to_string()),
        contact_id: None,
    };
    let draft =
        service::draft_from_fill(&item, &fill, &matched_company, 1, "m", 2_000).expect("draft");
    assert!(!draft.create_company);
    assert!(draft.create_contact);
    assert_eq!(draft.company_name.as_deref(), Some("examplecompany"));

    // Both exist → nothing to propose.
    let both = RecordMatches {
        account_id: Some("acc-1".to_string()),
        contact_id: Some("con-1".to_string()),
    };
    assert!(service::draft_from_fill(&item, &fill, &both, 1, "m", 2_000).is_none());
}

#[test]
fn drafts_from_fill_fans_out_missing_contacts() {
    let fill = service::parse_record_fill_response(&json!({
        "company_name": "example_retailer Technology",
        "company_website": "https://retailer.example.test/aboutus/",
        "contacts": [
            {
                "first_name": "Trevor",
                "last_name": "Kirkpatrick",
                "email": "trevor@retailer.example.test",
                "phone": null,
                "title": null,
                "quote": "Trevor Kirkpatrick works at example_retailer Technology"
            },
            {
                "first_name": "Zach",
                "last_name": "Hodgeboom",
                "email": null,
                "phone": null,
                "title": "Owner",
                "quote": "Zach Hodgeboom is owner"
            }
        ],
        "confidence": "high",
        "provenance": [
            {"field": "company_name", "quote": "example_retailer Technology"}
        ]
    }))
    .expect("parse");
    let item = accepted_item();
    let contact_matches = fill
        .contacts
        .iter()
        .cloned()
        .map(|contact| (contact, None))
        .collect::<Vec<_>>();

    let drafts =
        service::drafts_from_fill(&item, &fill, None, &contact_matches, 3, "model-x", 9_000);

    assert_eq!(drafts.len(), 2);
    assert_eq!(drafts[0].draft_id, "crd_wi_operator_note_note_1_3_1");
    assert_eq!(drafts[1].draft_id, "crd_wi_operator_note_note_1_3_2");
    assert_eq!(drafts[0].contact_first_name.as_deref(), Some("Trevor"));
    assert_eq!(drafts[1].contact_first_name.as_deref(), Some("Zach"));
    assert!(drafts.iter().all(|draft| draft.create_company));
    assert!(drafts.iter().all(|draft| draft.create_contact));
}

#[test]
fn drafts_from_fill_skips_redundant_company_only_when_contact_draft_will_create_company() {
    let fill = service::parse_record_fill_response(&json!({
        "company_name": "example_retailer Technology",
        "contacts": [
            {
                "first_name": "Trevor",
                "last_name": "Kirkpatrick",
                "email": "trevor@retailer.example.test",
                "phone": null,
                "title": null,
                "quote": "Trevor Kirkpatrick works at example_retailer Technology"
            },
            {
                "first_name": "Zach",
                "last_name": "Hodgeboom",
                "email": null,
                "phone": null,
                "title": "Owner",
                "quote": "Zach Hodgeboom is owner"
            }
        ],
        "confidence": "high",
        "provenance": [
            {"field": "company_name", "quote": "example_retailer Technology"}
        ]
    }))
    .expect("parse");
    let item = accepted_item();
    let contact_matches = vec![
        (
            fill.contacts[0].clone(),
            Some("existing-trevor".to_string()),
        ),
        (fill.contacts[1].clone(), None),
    ];

    let drafts =
        service::drafts_from_fill(&item, &fill, None, &contact_matches, 4, "model-x", 9_000);

    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].contact_first_name.as_deref(), Some("Zach"));
    assert!(
        drafts[0].create_company,
        "missing contact draft should also ensure the missing company"
    );
    assert!(drafts[0].create_contact);
}

#[test]
fn cached_crm_contact_id_requires_unique_email_match() {
    let context = json!({
        "crm_contact_lookup": {
            "email": "casey@business-63644db2f2.example.test",
            "company": null,
            "contacts": [
                {
                    "provider": "hubspot",
                    "provider_contact_id": "con-1",
                    "email": "casey@business-63644db2f2.example.test",
                    "name": "casey Sullivan"
                },
                {
                    "provider": "hubspot",
                    "provider_contact_id": "con-2",
                    "email": "casey@business-63644db2f2.example.test",
                    "name": "casey S."
                }
            ],
            "deals": []
        }
    });

    assert_eq!(
        service::cached_crm_contact_id_for_test(
            &context,
            Some("casey@business-63644db2f2.example.test")
        ),
        None,
        "duplicate cached CRM contacts with the same email must not graft an arbitrary provider id"
    );

    let unique_context = json!({
        "crm_contact_lookup": {
            "email": "casey@business-63644db2f2.example.test",
            "company": null,
            "contacts": [
                {
                    "provider": "hubspot",
                    "provider_contact_id": "con-1",
                    "email": "casey@business-63644db2f2.example.test",
                    "name": "casey Sullivan"
                },
                {
                    "provider": "hubspot",
                    "provider_contact_id": "con-2",
                    "email": "other@business-63644db2f2.example.test",
                    "name": "Other Contact"
                }
            ],
            "deals": []
        }
    });

    assert_eq!(
        service::cached_crm_contact_id_for_test(
            &unique_context,
            Some("  casey@business-63644db2f2.example.test ")
        ),
        Some("con-1".to_string())
    );
}

fn staged_draft() -> CrmRecordDraft {
    CrmRecordDraft {
        draft_id: "crd_wi_operator_note_note_1_1".to_string(),
        item_id: "wi_operator_note_note_1".to_string(),
        source_kind: "operator_note".to_string(),
        source_ref: "note_1".to_string(),
        status: CrmRecordDraftStatus::Staged,
        create_company: true,
        company_name: Some("examplecompany".to_string()),
        company_website: Some("business-63644db2f2.example.test".to_string()),
        company_phone: None,
        company_address: None,
        company_description: None,
        create_contact: true,
        contact_first_name: Some("casey".to_string()),
        contact_last_name: Some("Sullivan".to_string()),
        contact_email: Some("casey@business-63644db2f2.example.test".to_string()),
        contact_phone: None,
        contact_title: None,
        provider_ids: CrmRecordProviderIds::default(),
        provenance: vec![bos_contracts::calendar_drafts::DraftFieldProvenance {
            field: "company_name".to_string(),
            quote: "Went to business-63644db2f2.example.test HQ".to_string(),
        }],
        enrichment_trace: None,
        research_annotations: Vec::new(),
        model: "test-model".to_string(),
        confidence: "high".to_string(),
        outbox_job_id: None,
        created_at_ms: 2_000,
        updated_at_ms: 2_000,
    }
}

#[test]
fn crm_draft_json_is_byte_stable_without_research_annotations() {
    let serialized = serde_json::to_string(&staged_draft()).expect("serialize draft");
    let expected = r#"{"draft_id":"crd_wi_operator_note_note_1_1","item_id":"wi_operator_note_note_1","source_kind":"operator_note","source_ref":"note_1","status":"staged","create_company":true,"company_name":"examplecompany","company_website":"business-63644db2f2.example.test","create_contact":true,"contact_first_name":"casey","contact_last_name":"Sullivan","contact_email":"casey@business-63644db2f2.example.test","provider_ids":{},"provenance":[{"field":"company_name","quote":"Went to business-63644db2f2.example.test HQ"}],"model":"test-model","confidence":"high","created_at_ms":2000,"updated_at_ms":2000}"#;
    assert_eq!(serialized, expected);
    assert!(!serialized.contains("research_annotations"));
}

#[test]
fn research_graft_maps_company_safe_and_person_sensitive_annotations() {
    let accepted = vec![
        crate::slices::enrichment::research_finalize::AcceptedField {
            field_id: "company_website".to_string(),
            value: "business-63644db2f2.example.test".to_string(),
            confidence: EnrichmentConfidence::High,
            evidence_id: "ev_0".to_string(),
            quote: "Official site business-63644db2f2.example.test".to_string(),
            display_byte_start: 0,
            display_byte_end: 27,
        },
        crate::slices::enrichment::research_finalize::AcceptedField {
            field_id: "contact_email".to_string(),
            value: "casey@business-63644db2f2.example.test".to_string(),
            confidence: EnrichmentConfidence::Medium,
            evidence_id: "ev_0".to_string(),
            quote: "Email casey at casey@business-63644db2f2.example.test".to_string(),
            display_byte_start: 28,
            display_byte_end: 65,
        },
    ];
    let apply = service::research_apply_from_accepted(&accepted);
    assert_eq!(
        apply
            .company_website
            .as_ref()
            .map(|value| value.value.as_str()),
        Some("business-63644db2f2.example.test")
    );
    assert_eq!(
        apply
            .contact_email
            .as_ref()
            .map(|value| value.value.as_str()),
        Some("casey@business-63644db2f2.example.test")
    );

    let mut evidence = bos_integrations::evidence::EvidenceStore::new();
    evidence
        .insert_html_page_urls(
            "https://business-63644db2f2.example.test/contact",
            "https://business-63644db2f2.example.test/contact",
            1_000,
            200,
            "<html><body>Official site business-63644db2f2.example.test\nEmail casey at casey@business-63644db2f2.example.test</body></html>",
            8_000,
        )
        .expect("evidence");
    let unresolved: BTreeMap<String, EnrichmentFieldSpec> =
        test_enrichment_plan("crm_record_contact")
            .fields
            .into_iter()
            .chain([
                EnrichmentFieldSpec {
                    field_id: "company_website".to_string(),
                    value_kind: "domain".to_string(),
                    eligibility: EnrichmentEligibility::MissingOnly,
                    min_confidence: EnrichmentConfidence::Medium,
                    provenance_required: true,
                    operator_override: true,
                },
                EnrichmentFieldSpec {
                    field_id: "contact_email".to_string(),
                    value_kind: "email".to_string(),
                    eligibility: EnrichmentEligibility::MissingOnly,
                    min_confidence: EnrichmentConfidence::Medium,
                    provenance_required: true,
                    operator_override: true,
                },
            ])
            .map(|field| (field.field_id.clone(), field))
            .collect();
    let annotations =
        service::research_annotations_from_accepted(&accepted, &evidence, &unresolved);
    assert_eq!(annotations.len(), 2);
    assert_eq!(annotations[0].source_domain, "example.test");
    assert!(!annotations[0].person_sensitive);
    assert_eq!(annotations[1].source_domain, "example.test");
    assert!(annotations[1].person_sensitive);
}

#[test]
fn research_decision_request_excludes_operator_note_text() {
    let secret_note = "operator private note do not send to navigation model";
    let plan = EnrichmentPlan {
        subject: "crm_record_contact".to_string(),
        fields: vec![EnrichmentFieldSpec {
            field_id: "contact_email".to_string(),
            value_kind: "email".to_string(),
            eligibility: EnrichmentEligibility::MissingOnly,
            min_confidence: EnrichmentConfidence::Medium,
            provenance_required: true,
            operator_override: true,
        }],
        seed_evidence: vec![EnrichmentSeedEvidence {
            source_id: "operator_note:note_1".to_string(),
            label: "Source".to_string(),
            quote: Some(secret_note.to_string()),
        }],
        enabled_tiers: vec![EnrichmentTier::Local, EnrichmentTier::WebSearch],
        stop_policy: vec!["all_fields_accepted".to_string()],
    };
    let unresolved: BTreeMap<String, EnrichmentFieldSpec> = plan
        .fields
        .iter()
        .cloned()
        .map(|field| (field.field_id.clone(), field))
        .collect();
    let input = service::research_run_input(
        &plan,
        "business-63644db2f2.example.test".to_string(),
        &unresolved,
    );
    let context = crate::slices::enrichment::research::ResearchDecisionContext {
        subject: input.subject,
        seed_domain: input.seed_domain,
        unresolved_field_ids: input.unresolved_field_ids,
        surfaced_urls: vec!["https://business-63644db2f2.example.test/contact".to_string()],
        rejected_urls: Vec::new(),
        evidence_records: Vec::new(),
        step: 0,
    };
    let request = crate::slices::enrichment::research::build_research_action_request(
        CLIENT,
        "enr_1",
        &crate::env_registry::agentic_web_research_config(),
        &context,
    );
    let rendered = serde_json::to_string(&request.input.json).expect("json");
    let blocks = request
        .input
        .text_blocks
        .iter()
        .map(|block| block.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!rendered.contains(secret_note));
    assert!(!blocks.contains(secret_note));
    assert!(rendered.contains("business-63644db2f2.example.test"));
    assert!(rendered.contains("contact_email"));
}

#[test]
fn research_kickoff_is_explicitly_rejected_when_feature_off() {
    let _guard = EnvGuard::unset("BOS_AGENTIC_WEB_RESEARCH_ENABLED");
    let result = service::kick_on_demand_enrichment(
        test_support::test_state(),
        "missing_draft".to_string(),
        "operator".to_string(),
        "idem_research_disabled".to_string(),
        Some("business-63644db2f2.example.test".to_string()),
        Some(EnrichmentMode::Research),
    );
    match result {
        Err(service::OnDemandEnrichmentError::ResearchModeDisabled) => {}
        Err(other) => panic!("unexpected error: {other:?}"),
        Ok(_) => panic!("research mode should be disabled by default"),
    }
}

fn test_enrichment_plan(subject: &str) -> EnrichmentPlan {
    EnrichmentPlan {
        subject: subject.to_string(),
        fields: vec![EnrichmentFieldSpec {
            field_id: "company_phone".to_string(),
            value_kind: "phone".to_string(),
            eligibility: EnrichmentEligibility::MissingOnly,
            min_confidence: EnrichmentConfidence::Medium,
            provenance_required: true,
            operator_override: true,
        }],
        seed_evidence: vec![EnrichmentSeedEvidence {
            source_id: "operator_note:note_1".to_string(),
            label: "Source".to_string(),
            quote: Some("business-63644db2f2.example.test".to_string()),
        }],
        enabled_tiers: vec![EnrichmentTier::Local, EnrichmentTier::WebSearch],
        stop_policy: vec!["terminal_status_required".to_string()],
    }
}

fn test_enrichment_event(reason: &str) -> EnrichmentTierEvent {
    EnrichmentTierEvent {
        event_type: "tier_lifecycle".to_string(),
        tier: EnrichmentTier::WebSearch,
        field_id: None,
        status: Some("recorded".to_string()),
        reason: Some(reason.to_string()),
        source_id: None,
        url: None,
        final_url: None,
        query: None,
        rank: None,
        title: None,
        snippet: None,
        proposed_value: None,
        confidence: None,
        quote: None,
        latency_ms: None,
        bytes: None,
        cost_micros: None,
        ..Default::default()
    }
}

fn insert_crm_freshness_source(conn: &mut rusqlite::Connection, draft: &CrmRecordDraft) {
    crate::slices::operator_notes::store::insert_note(
        conn,
        CLIENT,
        &OperatorNote {
            note_id: draft.source_ref.clone(),
            body: "examplecompany asked us to add their CRM records. Website business-63644db2f2.example.test."
                .to_string(),
            category_id: "operator_note".to_string(),
            created_by: "jordan".to_string(),
            created_at_ms: 1_000,
        },
        &format!("note:{}", draft.source_ref),
    )
    .expect("note");
    let mut item = accepted_item();
    item.item_id = draft.item_id.clone();
    item.source_ref = draft.source_ref.clone();
    crate::slices::work_queue::store::insert_item(conn, CLIENT, &item).expect("item");
}

#[test]
fn freshness_candidates_require_actionable_gap_and_skip_current_bucket_run() {
    let state = test_support::test_state();
    let adapter = crate::slices::enrichment::service::registered_freshness_adapters()
        .iter()
        .find(|adapter| adapter.subject_id == "crm_record_contact")
        .expect("crm contact adapter");
    let stale_after_ms = 30 * 24 * 60 * 60 * 1000;
    let now_ms = stale_after_ms * 2;

    {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        let draft = staged_draft();
        insert_crm_freshness_source(conn, &draft);
        store::insert_draft(conn, CLIENT, "jordan", &draft, "produce:freshness").expect("draft");
    }

    let candidates = service::freshness_candidates(&state, adapter, stale_after_ms, now_ms, 10)
        .expect("candidates");
    assert_eq!(candidates.len(), 1);

    {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        enrichment_store::start_run(
            conn,
            CLIENT,
            "enrichment_freshness",
            enrichment_store::StartRun {
                run_id: &candidates[0].run_id,
                slice_id: "crm_record_drafts",
                draft_id: &candidates[0].draft_id,
                item_id: &candidates[0].item_id,
                plan: &test_enrichment_plan("crm_record_contact"),
                created_by: "enrichment_freshness",
                now_ms,
            },
        )
        .expect("start current bucket");
    }
    assert!(
        service::freshness_candidates(&state, adapter, stale_after_ms, now_ms, 10)
            .expect("candidates")
            .is_empty()
    );

    let state = test_support::test_state();
    {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        let mut filled = staged_draft();
        filled.company_phone = Some("555-0100".to_string());
        filled.company_address = Some("1 Main St".to_string());
        filled.contact_phone = Some("555-0101".to_string());
        insert_crm_freshness_source(conn, &filled);
        store::insert_draft(conn, CLIENT, "jordan", &filled, "produce:filled").expect("draft");
    }
    assert!(
        service::freshness_candidates(&state, adapter, stale_after_ms, now_ms, 10)
            .expect("candidates")
            .is_empty()
    );
}

#[test]
fn freshness_candidates_ignore_recent_acceptance_for_already_filled_fields() {
    let state = test_support::test_state();
    let adapter = crate::slices::enrichment::service::registered_freshness_adapters()
        .iter()
        .find(|adapter| adapter.subject_id == "crm_record_contact")
        .expect("crm contact adapter");
    let stale_after_ms = 30 * 24 * 60 * 60 * 1000;
    let now_ms = stale_after_ms * 2;

    {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        let mut draft = staged_draft();
        draft.contact_email = None;
        draft.company_phone = Some("555-0100".to_string());
        draft.company_address = Some("1 Main St".to_string());
        draft.contact_phone = Some("555-0101".to_string());
        insert_crm_freshness_source(conn, &draft);
        store::insert_draft(conn, CLIENT, "jordan", &draft, "produce:freshness").expect("draft");
        enrichment_store::start_run(
            conn,
            CLIENT,
            "enrichment_freshness",
            enrichment_store::StartRun {
                run_id: "enr_recent_company_phone",
                slice_id: "crm_record_drafts",
                draft_id: &draft.draft_id,
                item_id: &draft.item_id,
                plan: &test_enrichment_plan("crm_record_contact"),
                created_by: "enrichment_freshness",
                now_ms: now_ms - 1,
            },
        )
        .expect("start run");
        enrichment_store::finish_run(
            conn,
            CLIENT,
            "enrichment_freshness",
            enrichment_store::FinishRun {
                run_id: "enr_recent_company_phone",
                status: EnrichmentRunStatus::Completed,
                diagnostics: &[],
                proposals: &[EnrichmentFieldProposal {
                    field_id: "company_phone".to_string(),
                    proposed_value: "555-0100".to_string(),
                    source_tier: EnrichmentTier::WebSearch,
                    confidence: EnrichmentConfidence::Medium,
                    provenance_refs: vec![
                        "page:https://business-63644db2f2.example.test".to_string()
                    ],
                    accepted: true,
                    reason: "deterministic".to_string(),
                }],
                cost_micros: 0,
                now_ms: now_ms - 1,
                reason: "accepted_fields_applied",
            },
        )
        .expect("finish run");
    }

    let candidates = service::freshness_candidates(&state, adapter, stale_after_ms, now_ms, 10)
        .expect("candidates");
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].subject_id, "crm_record_contact");
}

async fn response_error(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json body");
    body.get("error")
        .and_then(serde_json::Value::as_str)
        .expect("error code")
        .to_string()
}

#[tokio::test]
async fn enrich_route_rejects_research_mode_when_feature_disabled() {
    let router = build_router(test_support::test_state());
    let response = router
        .oneshot(
            Request::post("/api/crm-record-drafts/draft_1/enrich")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "idempotency_key": "research_mode_1",
                        "mode": "research"
                    })
                    .to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(response_error(response).await, "research_mode_disabled");
}

#[test]
fn freshness_candidates_include_weak_ai_company_name() {
    let state = test_support::test_state();
    let adapter = crate::slices::enrichment::service::registered_freshness_adapters()
        .iter()
        .find(|adapter| adapter.subject_id == "crm_record_contact")
        .expect("crm contact adapter");
    let stale_after_ms = 30 * 24 * 60 * 60 * 1000;
    let now_ms = stale_after_ms * 2;

    {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        let mut draft = staged_draft();
        draft.company_name = Some("business-63644db2f2.example.test".to_string());
        draft.company_phone = Some("555-0100".to_string());
        draft.company_address = Some("1 Main St".to_string());
        draft.contact_phone = Some("555-0101".to_string());
        draft.provenance = vec![bos_contracts::calendar_drafts::DraftFieldProvenance {
            field: "company_name".to_string(),
            quote: "Went to business-63644db2f2.example.test HQ".to_string(),
        }];
        insert_crm_freshness_source(conn, &draft);
        store::insert_draft(conn, CLIENT, "jordan", &draft, "produce:weak-name").expect("draft");
    }

    let candidates = service::freshness_candidates(&state, adapter, stale_after_ms, now_ms, 10)
        .expect("candidates");
    assert_eq!(candidates.len(), 1);
}

#[test]
fn after_stage_enrichment_lifecycle_branches_end_terminal() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let draft = staged_draft();
    let plan = test_enrichment_plan("crm_record_company");
    let branches = [
        (
            "disabled",
            EnrichmentRunStatus::Skipped,
            "web_enrichment_disabled",
        ),
        (
            "applied",
            EnrichmentRunStatus::Completed,
            "accepted_fields_applied",
        ),
        ("apply_failed", EnrichmentRunStatus::Failed, "apply_failed"),
    ];

    for (suffix, expected_status, reason) in branches {
        let run_id = format!("enr_lifecycle_{suffix}");
        enrichment_store::start_run(
            conn,
            CLIENT,
            service::WEB_ENRICHMENT_ACTOR,
            enrichment_store::StartRun {
                run_id: &run_id,
                slice_id: "crm_record_drafts",
                draft_id: &draft.draft_id,
                item_id: &draft.item_id,
                plan: &plan,
                created_by: service::WEB_ENRICHMENT_ACTOR,
                now_ms: 10,
            },
        )
        .expect("start run");
        let diagnostics = vec![test_enrichment_event(reason)];
        let proposals = if expected_status == EnrichmentRunStatus::Completed {
            vec![EnrichmentFieldProposal {
                field_id: "company_phone".to_string(),
                proposed_value: "555-0100".to_string(),
                source_tier: EnrichmentTier::WebSearch,
                confidence: EnrichmentConfidence::Medium,
                provenance_refs: vec!["page:https://business-63644db2f2.example.test".to_string()],
                accepted: true,
                reason: reason.to_string(),
            }]
        } else {
            Vec::new()
        };
        enrichment_store::append_run_diagnostics(
            conn,
            CLIENT,
            service::WEB_ENRICHMENT_ACTOR,
            enrichment_store::AppendRunDiagnostics {
                run_id: &run_id,
                event_seq: reason,
                diagnostics: &diagnostics,
                proposals: &proposals,
                cost_micros: 0,
                now_ms: 11,
            },
        )
        .expect("append diagnostics");
        enrichment_store::transition_run_status(
            conn,
            CLIENT,
            service::WEB_ENRICHMENT_ACTOR,
            enrichment_store::TransitionRunStatus {
                run_id: &run_id,
                status: expected_status,
                now_ms: 12,
                reason,
            },
        )
        .expect("terminal transition");

        let run = enrichment_store::list_runs(
            conn,
            CLIENT,
            Some("crm_record_drafts"),
            Some(&draft.draft_id),
            None,
            10,
        )
        .expect("runs")
        .into_iter()
        .find(|run| run.run_id == run_id)
        .expect("run");
        assert_eq!(run.status, expected_status);
        assert_ne!(run.status, EnrichmentRunStatus::Started);
        assert!(run.finished_at_ms.is_some());
    }
}

#[test]
fn approval_job_targets_espocrm_create_records() {
    let job =
        service::build_approval_job(&staged_draft(), "jordan", 3_000, "espocrm").expect("job");
    assert_eq!(job.provider, "espocrm");
    assert_eq!(job.capability, "create_records");
    assert!(job.payload_json.contains("examplecompany"));
    assert!(job
        .payload_json
        .contains("casey@business-63644db2f2.example.test"));
}

#[test]
fn approval_job_targets_hubspot_when_configured() {
    let mut draft = staged_draft();
    draft.company_description = Some("Boutique vacation rentals near Charleston".to_string());
    let job = service::build_approval_job(&draft, "jordan", 3_000, "hubspot").expect("job");
    assert_eq!(job.provider, "hubspot");
    assert_eq!(job.capability, "create_records");
    // The payload deserializes as the HubSpot records shape with grounded names
    // and the enriched company description.
    let payload: bos_integrations::hubspot::HubSpotRecordsCreateOutboxPayload =
        serde_json::from_str(&job.payload_json).expect("hubspot payload");
    let company = payload.company.unwrap();
    assert_eq!(company.name, "examplecompany");
    assert_eq!(
        company.description.as_deref(),
        Some("Boutique vacation rentals near Charleston"),
        "company description flows into the HubSpot create"
    );
    assert_eq!(
        payload.contact.unwrap().email.as_deref(),
        Some("casey@business-63644db2f2.example.test")
    );
}

#[test]
fn approval_gate_refuses_nothing_proposed() {
    let mut draft = staged_draft();
    draft.create_company = false;
    draft.create_contact = false;
    let err =
        service::build_approval_job(&draft, "jordan", 3_000, "espocrm").expect_err("must refuse");
    assert_eq!(err, "crm_record_nothing_proposed");

    // A proposed company with no name is refused too.
    let mut draft = staged_draft();
    draft.create_contact = false;
    draft.company_name = None;
    let err =
        service::build_approval_job(&draft, "jordan", 3_000, "espocrm").expect_err("must refuse");
    assert_eq!(err, "crm_record_company_name_required");
}

#[test]
fn espocrm_approval_requires_contact_last_name() {
    let mut draft = staged_draft();
    draft.create_company = false;
    draft.contact_last_name = None;

    let err =
        service::build_approval_job(&draft, "jordan", 3_000, "espocrm").expect_err("must refuse");
    assert_eq!(err, "crm_record_contact_last_name_required");

    let job = service::build_approval_job(&draft, "jordan", 3_000, "hubspot")
        .expect("HubSpot can accept a first-name-only contact");
    assert_eq!(job.provider, "hubspot");
}

#[test]
fn approval_job_rejects_unknown_provider() {
    let err = service::build_approval_job(&staged_draft(), "jordan", 3_000, "salesforce")
        .expect_err("unknown providers must not fall back to EspoCRM");
    assert_eq!(err, "crm_provider_unsupported:salesforce");
}

#[test]
fn stage_via_insert_then_approve_round_trips() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let draft = staged_draft();
    store::insert_draft(conn, CLIENT, "jordan", &draft, "produce_1").expect("stage");

    // Multiple active CRM record drafts are allowed so one source note can
    // stage one draft per missing contact.
    let mut second = staged_draft();
    second.draft_id = "crd_wi_operator_note_note_1_2".to_string();
    store::insert_draft(conn, CLIENT, "jordan", &second, "produce_2").expect("second active draft");
    assert_eq!(
        store::count_drafts_for_item(conn, CLIENT, &draft.item_id).expect("count"),
        2
    );
    assert_eq!(
        store::staged_item_ids(conn, CLIENT).expect("staged ids"),
        vec![draft.item_id.clone()],
        "queue decoration should still get one item id for multiple staged CRM record drafts"
    );

    let job = service::build_approval_job(&draft, "jordan", 3_000, "espocrm").expect("job");
    let ctx = DraftActionContext {
        client_id: CLIENT,
        actor_id: "jordan",
        expected_revision: None,
        idempotency_key: "approve_1",
        now_ms: 3_000,
    };
    store::approve_draft(conn, ctx, &draft.draft_id, &job).expect("approve");

    let stored = store::get_draft(conn, CLIENT, &draft.draft_id)
        .expect("get")
        .expect("present");
    assert_eq!(stored.draft.status, CrmRecordDraftStatus::Approved);
    assert!(stored.draft.outbox_job_id.is_some());
    // The approved draft is no longer the active one for re-produce only after
    // rejection; approval keeps it active (non-rejected).
    assert!(store::active_draft_for_item(conn, CLIENT, &draft.item_id)
        .expect("active")
        .is_some());
}

#[test]
fn sanitize_edit_validates_names_and_proposed_set() {
    use bos_contracts::crm_record_drafts::CrmRecordDraftUpdateRequest;
    let base = CrmRecordDraftUpdateRequest {
        create_company: true,
        company_name: Some("  examplecompany  ".to_string()),
        company_website: Some(String::new()),
        company_phone: None,
        company_address: None,
        company_description: None,
        create_contact: false,
        contact_first_name: None,
        contact_last_name: None,
        contact_email: None,
        contact_phone: None,
        contact_title: None,
        expected_revision: None,
        idempotency_key: "k".to_string(),
        actor_id: None,
    };
    let edit = service::sanitize_record_edit(&base).expect("ok");
    assert_eq!(
        edit.company_name.as_deref(),
        Some("examplecompany"),
        "trimmed"
    );
    assert_eq!(edit.company_website, None, "empty website nulled");

    // Nothing proposed → refused.
    let mut none = base.clone();
    none.create_company = false;
    assert_eq!(
        service::sanitize_record_edit(&none),
        Err("crm_record_nothing_proposed")
    );

    // Proposed company without a name → refused.
    let mut nameless = base.clone();
    nameless.company_name = Some("   ".to_string());
    assert_eq!(
        service::sanitize_record_edit(&nameless),
        Err("crm_record_company_name_required")
    );
}

// --- website enrichment (Increment E) ---

use super::store::{EnrichedValue, WebEnrichmentApply};
use bos_integrations::web_page_read::{EnrichmentField, WebEnrichment};

fn enrichment_field(value: &str, url: &str) -> EnrichmentField {
    EnrichmentField {
        value: value.to_string(),
        provenance: format!("page:{url}"),
    }
}

#[test]
fn deterministic_apply_fills_only_missing_company_fields() {
    let mut draft = staged_draft();
    draft.company_website = Some("business-63644db2f2.example.test".to_string()); // note already had it
    draft.company_phone = None;
    let enrich = WebEnrichment {
        company_website: Some(enrichment_field(
            "https://business-63644db2f2.example.test",
            "https://business-63644db2f2.example.test/",
        )),
        company_phone: Some(enrichment_field(
            "+1-415-555-0100",
            "https://business-63644db2f2.example.test/contact",
        )),
        ..WebEnrichment::default()
    };
    let apply = service::deterministic_apply(&enrich, &draft);
    // Website was already on the draft (note-fill wins) → not in the apply set.
    assert!(apply.company_website.is_none());
    // Phone was missing → filled with page provenance.
    let phone = apply.company_phone.expect("phone");
    assert_eq!(phone.value, "+1-415-555-0100");
    assert_eq!(
        phone.provenance_quote,
        "page:https://business-63644db2f2.example.test/contact"
    );
}

#[test]
fn deterministic_apply_revises_weak_domain_company_name() {
    let mut draft = staged_draft();
    draft.company_name = Some("business-63644db2f2.example.test".to_string());
    let enrich = WebEnrichment {
        company_name: Some(enrichment_field(
            "example Stays",
            "https://business-63644db2f2.example.test/",
        )),
        ..WebEnrichment::default()
    };

    let apply = service::deterministic_apply(&enrich, &draft);

    assert_eq!(apply.company_name.unwrap().value, "example Stays");
}

#[test]
fn deterministic_apply_keeps_operator_edited_company_name() {
    let mut draft = staged_draft();
    draft.company_name = Some("RS Hospitality".to_string());
    let enrich = WebEnrichment {
        company_name: Some(enrichment_field(
            "example Stays",
            "https://business-63644db2f2.example.test/",
        )),
        ..WebEnrichment::default()
    };

    let apply = service::deterministic_apply(&enrich, &draft);

    assert!(apply.company_name.is_none());
}

#[test]
fn deterministic_apply_fills_company_description_from_og() {
    let mut draft = staged_draft();
    draft.company_description = None;
    let enrich = WebEnrichment {
        company_description: Some(enrichment_field(
            "Boutique vacation rentals in the Lowcountry",
            "https://business-63644db2f2.example.test/",
        )),
        ..WebEnrichment::default()
    };
    let apply = service::deterministic_apply(&enrich, &draft);
    assert_eq!(
        apply.company_description.unwrap().value,
        "Boutique vacation rentals in the Lowcountry"
    );
}

#[test]
fn crm_enrichment_request_uses_legacy_stable_flat_page_text() {
    let item = accepted_item();
    let draft = staged_draft();
    let html = r#"<html><head><style>.x{}</style><script>bad()</script></head>
        <body><h1>example&nbsp;Stays</h1><p>Email hello@business-63644db2f2.example.test &amp; call (415) 555-0199.</p></body></html>"#;
    let flat = bos_integrations::web_page_read::normalize_page_text(html, None, 8_000).flat_text;
    assert_eq!(
        flat,
        bos_integrations::web_page_read::strip_to_text(html, 8_000)
    );
    let pages = vec![bos_integrations::web_page_read::EnrichedPageText {
        url: "https://business-63644db2f2.example.test/contact".to_string(),
        text: flat,
    }];
    let request = service::build_enrichment_request(
        CLIENT,
        &item,
        &draft,
        &[
            "company_phone".to_string(),
            "company_description".to_string(),
        ],
        &pages,
    );
    assert_eq!(
        request.input.text_blocks[0].text,
        format!(
            "URL: https://business-63644db2f2.example.test/contact\n{}",
            bos_integrations::web_page_read::strip_to_text(html, 8_000)
        )
    );
    assert_eq!(
        request.input.json["eligible_fields"],
        json!(["company_phone", "company_description"])
    );
    assert_eq!(
        request.input.json["target_shape"]["company_address"],
        json!("string|null")
    );
    assert_eq!(
        request.input.json["target_shape"]["contact_first_name"],
        json!("string|null")
    );
    assert_eq!(
        request.input.json["current_values"]["company_website"],
        json!("business-63644db2f2.example.test")
    );
    assert_eq!(
        request.input.json["current_values"]["contact_first_name"],
        json!("casey")
    );
    assert!(request.input.json["instructions"]
        .as_str()
        .expect("instructions")
        .contains(
            "\"fields\":{\"field_id\":{\"value\":\"...\",\"quote\":\"literal evidence span\"}}"
        ));
}

#[test]
fn gap_fill_description_requires_value_supported_by_real_page_quote() {
    let page_text = "About us: example Stays manages luxury beach homes near Charleston.";
    let missing = vec!["company_description".to_string()];
    let ok = json!({
        "company_description": "manages luxury beach homes near Charleston",
        "confidence": "medium",
        "provenance": [{"field": "company_description", "quote": "manages luxury beach homes near Charleston"}]
    });
    assert!(service::parse_enrichment_response(&ok, page_text, &missing)
        .company_description
        .is_some());

    // A real quote that does not support the returned value is still refused.
    let bad = json!({
        "company_description": "We sell rockets.",
        "confidence": "low",
        "provenance": [{"field": "company_description", "quote": "manages luxury beach homes near Charleston"}]
    });
    assert!(
        service::parse_enrichment_response(&bad, page_text, &missing)
            .company_description
            .is_none()
    );
}

#[test]
fn gap_fill_accepts_shape_based_fields_response() {
    let page_text =
        "Contact Us Call Us 843-284-7105 LOCATION: 997 Morrison Dr Charleston, SC 29203";
    let missing = vec!["company_phone".to_string(), "company_address".to_string()];
    let response = json!({
        "confidence": "medium",
        "fields": {
            "company_phone": {
                "value": "843-284-7105",
                "quote": "Call Us 843-284-7105"
            },
            "company_address": {
                "value": "997 Morrison Dr Charleston, SC 29203",
                "quote": "997 Morrison Dr Charleston, SC 29203"
            },
            "contact_email": {
                "value": "info@retailer.example.test",
                "quote": "info@retailer.example.test"
            }
        }
    });

    let apply = service::parse_enrichment_response(&response, page_text, &missing);

    assert_eq!(apply.company_phone.unwrap().value, "843-284-7105");
    assert_eq!(
        apply.company_address.unwrap().value,
        "997 Morrison Dr Charleston, SC 29203"
    );
    assert!(apply.contact_email.is_none(), "not eligible for this run");
}

#[test]
fn gap_fill_records_rejected_model_candidates() {
    let page_text = "Contact Us LOCATION: 997 Morrison Dr Charleston, SC 29203";
    let missing = vec!["company_address".to_string()];
    let response = json!({
        "fields": {
            "company_address": {
                "value": "12 Made Up Lane Charleston, SC 29203",
                "quote": "997 Morrison Dr Charleston, SC 29203"
            },
            "contact_email": {
                "value": "info@retailer.example.test",
                "quote": "info@retailer.example.test"
            },
            "company_phone": {
                "value": "843-284-7105"
            }
        }
    });

    let result =
        service::parse_enrichment_response_with_diagnostics(&response, page_text, &missing);

    assert!(result.apply.company_address.is_none());
    assert!(result.diagnostics.iter().any(|event| {
        event.field_id.as_deref() == Some("company_address")
            && event.status.as_deref() == Some("rejected")
            && event.reason.as_deref() == Some("value_not_supported_by_quote")
    }));
    assert!(result.diagnostics.iter().any(|event| {
        event.field_id.as_deref() == Some("contact_email")
            && event.status.as_deref() == Some("rejected")
            && event.reason.as_deref() == Some("field_not_eligible")
    }));
    assert!(result.diagnostics.iter().any(|event| {
        event.field_id.as_deref() == Some("company_phone")
            && event.status.as_deref() == Some("rejected")
            && event.reason.as_deref() == Some("field_not_eligible")
    }));
    assert_eq!(result.proposals.len(), 3);
}

#[test]
fn gap_fill_accepts_normalized_value_supported_by_quote() {
    let page_text = "LOCATION:\n997 Morrison Dr\nCharleston, SC 29203";
    let missing = vec!["company_address".to_string()];
    let response = json!({
        "fields": {
            "company_address": {
                "value": "997 Morrison Dr Charleston SC 29203",
                "quote": "997 Morrison Dr\nCharleston, SC 29203"
            }
        }
    });

    let result =
        service::parse_enrichment_response_with_diagnostics(&response, page_text, &missing);

    assert_eq!(
        result.apply.company_address.expect("address").value,
        "997 Morrison Dr Charleston SC 29203"
    );
    assert!(result.diagnostics.iter().any(|event| {
        event.field_id.as_deref() == Some("company_address")
            && event.status.as_deref() == Some("accepted")
            && event.reason.as_deref() == Some("grounded_quote")
    }));
}

#[test]
fn gap_fill_records_quote_missing_for_eligible_candidate() {
    let missing = vec!["company_phone".to_string()];
    let response = json!({
        "fields": {
            "company_phone": {
                "value": "843-284-7105"
            }
        }
    });

    let result = service::parse_enrichment_response_with_diagnostics(&response, "", &missing);

    assert!(result.apply.company_phone.is_none());
    assert!(result.diagnostics.iter().any(|event| {
        event.field_id.as_deref() == Some("company_phone")
            && event.status.as_deref() == Some("rejected")
            && event.reason.as_deref() == Some("quote_missing")
    }));
}

#[test]
fn gap_fill_normalizes_deep_website_to_homepage() {
    let page_text = "Visit https://retailer.example.test/aboutus/ to learn about example_retailer.";
    let missing = vec!["company_website".to_string()];
    let response = json!({
        "company_website": "https://retailer.example.test/aboutus/",
        "confidence": "medium",
        "provenance": [
            {"field": "company_website", "quote": "https://retailer.example.test/aboutus/"}
        ]
    });

    let apply = service::parse_enrichment_response(&response, page_text, &missing);

    assert_eq!(
        apply.company_website.expect("website").value,
        "https://retailer.example.test/"
    );
}

#[test]
fn deterministic_apply_skips_company_when_not_creating() {
    let mut draft = staged_draft();
    draft.create_company = false; // matched existing company — don't prefill it
    draft.company_phone = None;
    let enrich = WebEnrichment {
        company_phone: Some(enrichment_field(
            "+1-415-555-0100",
            "https://business-63644db2f2.example.test/",
        )),
        ..WebEnrichment::default()
    };
    assert!(service::deterministic_apply(&enrich, &draft).is_empty());
}

#[test]
fn missing_fields_reflect_create_flags_and_existing_values() {
    let mut draft = staged_draft();
    draft.company_phone = None;
    draft.company_address = None;
    draft.contact_title = None;
    let apply = WebEnrichmentApply::default();
    let missing = service::missing_enrich_fields(&draft, &apply);
    assert!(missing.contains(&"company_phone".to_string()));
    assert!(missing.contains(&"company_address".to_string()));
    assert!(missing.contains(&"contact_title".to_string()));
    // company_website is present on the draft → not missing.
    assert!(!missing.contains(&"company_website".to_string()));
}

#[test]
fn missing_fields_include_weak_ai_company_name() {
    let mut draft = staged_draft();
    draft.company_name = Some("business-63644db2f2.example.test".to_string());
    let missing = service::missing_enrich_fields(&draft, &WebEnrichmentApply::default());

    assert!(missing.contains(&"company_name".to_string()));
}

#[test]
fn gap_fill_keeps_grounded_company_name() {
    let page_text = "Welcome to example Stays, a boutique vacation rental company.";
    let missing = vec!["company_name".to_string()];
    let response = json!({
        "company_name": "example Stays",
        "confidence": "medium",
        "provenance": [{"field": "company_name", "quote": "Welcome to example Stays"}]
    });

    let apply = service::parse_enrichment_response(&response, page_text, &missing);

    assert_eq!(apply.company_name.unwrap().value, "example Stays");
}

#[test]
fn gap_fill_keeps_grounded_fields_and_drops_ungrounded() {
    let page_text = "Reception line: (415) 555-0199. casey Sullivan, Managing Director.";
    let missing = vec![
        "company_phone".to_string(),
        "contact_title".to_string(),
        "company_address".to_string(),
    ];
    let response = json!({
        "company_phone": "(415) 555-0199",
        "contact_title": "Managing Director",
        "company_address": "123 Invented Way",       // not in page text → dropped
        "confidence": "medium",
        "provenance": [
            {"field": "company_phone", "quote": "(415) 555-0199"},
            {"field": "contact_title", "quote": "casey Sullivan, Managing Director"},
            {"field": "company_address", "quote": "123 Invented Way"}
        ]
    });
    let apply = service::parse_enrichment_response(&response, page_text, &missing);
    assert_eq!(apply.company_phone.unwrap().value, "(415) 555-0199");
    assert_eq!(apply.contact_title.unwrap().value, "Managing Director");
    // The address quote is not a literal span of the page → refused.
    assert!(apply.company_address.is_none());
}

#[test]
fn search_gap_fill_can_revise_only_requested_grounded_company_name() {
    let evidence =
        "SEARCH_PAGE_0\nTitle: example Stays\nSnippet: example Stays offers vacation rentals.";
    let missing = vec!["company_name".to_string()];
    let response = json!({
        "company_name": "example Stays",
        "confidence": "high",
        "provenance": [{"field": "company_name", "quote": "example Stays offers vacation rentals"}]
    });
    let apply = service::parse_enrichment_response(&response, evidence, &missing);
    assert_eq!(apply.company_name.unwrap().value, "example Stays");

    let not_requested = service::parse_enrichment_response(&response, evidence, &[]);
    assert!(not_requested.company_name.is_none());
}

#[test]
fn search_gap_fill_keeps_grounded_example_retailer_contact_fields() {
    let evidence = "SEARCH_PAGE_0\nTitle: Contact Us | example_retailer Technology\nSnippet: Contact Us Call Us 843-284-7105 Phone Support Hours: Monday-Friday: 8:00am-5:00pm EST LOCATION: example_retailer Technology 997 Morrison Dr Charleston, SC 29203 info@retailer.example.test";
    let missing = vec![
        "company_phone".to_string(),
        "company_address".to_string(),
        "contact_email".to_string(),
    ];
    let response = json!({
        "company_phone": "843-284-7105",
        "company_address": "997 Morrison Dr Charleston, SC 29203",
        "contact_email": "info@retailer.example.test",
        "confidence": "medium",
        "provenance": [
            {"field": "company_phone", "quote": "Call Us 843-284-7105"},
            {"field": "company_address", "quote": "997 Morrison Dr Charleston, SC 29203"},
            {"field": "contact_email", "quote": "info@retailer.example.test"}
        ]
    });

    let apply = service::parse_enrichment_response(&response, evidence, &missing);

    assert_eq!(apply.company_phone.unwrap().value, "843-284-7105");
    assert_eq!(
        apply.company_address.unwrap().value,
        "997 Morrison Dr Charleston, SC 29203"
    );
    assert_eq!(
        apply.contact_email.unwrap().value,
        "info@retailer.example.test"
    );
}

#[test]
fn search_queries_include_current_name_and_domain() {
    let mut draft = staged_draft();
    draft.company_name = Some("business-63644db2f2.example.test".to_string());
    let queries =
        service::crm_search_enrichment_queries("business-63644db2f2.example.test", &draft);
    assert_eq!(
        queries,
        vec![
            "business-63644db2f2.example.test contact address phone email business-63644db2f2.example.test",
            "business-63644db2f2.example.test official company name business-63644db2f2.example.test",
        ]
    );
}

#[test]
fn enrichment_domain_seed_falls_back_to_draft_website() {
    let mut draft = staged_draft();
    draft.company_website = Some("https://retailer.example.test/about/".to_string());

    assert_eq!(
        service::enrichment_domain_seed(&draft, "no website in this note", None).as_deref(),
        Some("retailer.example.test")
    );
    assert_eq!(
        service::enrichment_domain_seed(
            &draft,
            "note mentions business-63644db2f2.example.test",
            Some("https://business-6746421a55.example.test/contact")
        )
        .as_deref(),
        Some("business-6746421a55.example.test")
    );
    assert_eq!(
        service::enrichment_domain_seed(
            &draft,
            "note mentions business-63644db2f2.example.test",
            None
        )
        .as_deref(),
        Some("business-63644db2f2.example.test")
    );
}

#[test]
fn enrichment_plan_records_draft_website_seed() {
    let mut draft = staged_draft();
    draft.company_website = Some("https://retailer.example.test/about/".to_string());
    let plan = service::crm_enrichment_plan_for_test(&draft, "no website in this note", None);

    assert!(plan.seed_evidence.iter().any(|seed| {
        seed.source_id == "draft_website_domain_seed"
            && seed.quote.as_deref() == Some("retailer.example.test")
    }));
}

#[test]
fn gap_fill_drops_values_not_backed_by_their_quote() {
    let page_text = "Reception line: (415) 555-0199.";
    let missing = vec!["company_phone".to_string()];
    let response = json!({
        "company_phone": "(415) 555-0100",
        "confidence": "medium",
        "provenance": [{"field": "company_phone", "quote": "Reception line: (415) 555-0199"}]
    });
    // The quote is a literal page span, but it does not contain the returned
    // value, so the field is still ungrounded and must be dropped.
    assert!(service::parse_enrichment_response(&response, page_text, &missing).is_empty());
}

#[test]
fn gap_fill_ignores_fields_outside_the_missing_set() {
    let page_text = "Call us at (415) 555-0199.";
    let missing = vec!["company_address".to_string()];
    let response = json!({
        "company_phone": "(415) 555-0199",
        "confidence": "low",
        "provenance": [{"field": "company_phone", "quote": "(415) 555-0199"}]
    });
    // company_phone was grounded but it isn't in the missing set → not applied.
    assert!(service::parse_enrichment_response(&response, page_text, &missing).is_empty());
}

#[test]
fn apply_web_enrichment_fills_nulls_keeps_existing_and_appends_provenance() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let draft = staged_draft(); // company_phone/address NULL, website set
    store::insert_draft(conn, CLIENT, "jordan", &draft, "produce_1").expect("stage");

    let apply = WebEnrichmentApply {
        company_website: Some(EnrichedValue {
            value: "https://business-e9efb21f74.example.test".to_string(),
            provenance_quote: "page:https://business-63644db2f2.example.test/".to_string(),
        }),
        company_phone: Some(EnrichedValue {
            value: "+1-415-555-0100".to_string(),
            provenance_quote: "page:https://business-63644db2f2.example.test/contact".to_string(),
        }),
        ..WebEnrichmentApply::default()
    };
    let ctx = DraftActionContext {
        client_id: CLIENT,
        actor_id: "crm_web_enrichment",
        expected_revision: None,
        idempotency_key: "crmenrich_1",
        now_ms: 4_000,
    };
    let trace = bos_contracts::crm_record_drafts::CrmEnrichmentTrace {
        captured_at_ms: 4_000,
        domain: "business-63644db2f2.example.test".to_string(),
        pages: vec!["https://business-63644db2f2.example.test/".to_string()],
        items: vec![
            bos_contracts::crm_record_drafts::CrmEnrichmentTraceItem {
                field: "company_phone".to_string(),
                previous_value: None,
                value: "+1-415-555-0100".to_string(),
                source: "page:https://business-63644db2f2.example.test/contact".to_string(),
                via: "deterministic".to_string(),
            },
            bos_contracts::crm_record_drafts::CrmEnrichmentTraceItem {
                field: "company_website".to_string(),
                previous_value: Some("business-63644db2f2.example.test".to_string()),
                value: "https://business-e9efb21f74.example.test".to_string(),
                source: "page:https://business-63644db2f2.example.test/".to_string(),
                via: "research".to_string(),
            },
        ],
        llm_ran: false,
        llm_input_chars: 42,
        llm_input_preview: "Welcome to example Stays".to_string(),
        search_ran: false,
        search_reason: None,
        search_queries: Vec::new(),
        search_results: Vec::new(),
        failures: Vec::new(),
        research_annotations: vec![
            CrmResearchFieldAnnotation {
                field_id: "company_phone".to_string(),
                confidence: EnrichmentConfidence::High,
                source_domain: "business-63644db2f2.example.test".to_string(),
                quote: "Call +1-415-555-0100".to_string(),
                person_sensitive: false,
            },
            CrmResearchFieldAnnotation {
                field_id: "company_website".to_string(),
                confidence: EnrichmentConfidence::High,
                source_domain: "business-63644db2f2.example.test".to_string(),
                quote: "Visit https://business-e9efb21f74.example.test".to_string(),
                person_sensitive: false,
            },
        ],
    };
    store::apply_web_enrichment(conn, ctx, &draft.draft_id, &apply, Some(&trace)).expect("apply");

    let stored = store::get_draft(conn, CLIENT, &draft.draft_id)
        .expect("get")
        .expect("present")
        .draft;
    // Website was already set by the note-fill → enrichment did NOT overwrite.
    assert_eq!(
        stored.company_website.as_deref(),
        Some("business-63644db2f2.example.test")
    );
    // Phone was NULL → filled.
    assert_eq!(stored.company_phone.as_deref(), Some("+1-415-555-0100"));
    // Provenance appended only for the field actually filled.
    assert!(stored.provenance.iter().any(|p| p.field == "company_phone"));
    assert!(!stored
        .provenance
        .iter()
        .any(|p| p.field == "company_website"));
    // The enrichment trace round-trips for the panel to render.
    let stored_trace = stored.enrichment_trace.clone().expect("trace stored");
    assert_eq!(stored_trace.domain, "business-63644db2f2.example.test");
    assert_eq!(stored_trace.items.len(), 1);
    assert_eq!(stored_trace.items[0].field, "company_phone");
    assert!(!stored_trace.llm_ran);
    assert_eq!(
        stored.research_annotations,
        stored_trace.research_annotations
    );
    assert_eq!(
        stored.research_annotations,
        vec![CrmResearchFieldAnnotation {
            field_id: "company_phone".to_string(),
            confidence: EnrichmentConfidence::High,
            source_domain: "business-63644db2f2.example.test".to_string(),
            quote: "Call +1-415-555-0100".to_string(),
            person_sensitive: false,
        }]
    );

    // Approval clears the trace (pre-approval review aid only).
    let job = service::build_approval_job(&stored, "jordan", 5_000, "espocrm").expect("job");
    let approve_ctx = DraftActionContext {
        client_id: CLIENT,
        actor_id: "jordan",
        expected_revision: None,
        idempotency_key: "approve_enrich",
        now_ms: 5_000,
    };
    store::approve_draft(conn, approve_ctx, &draft.draft_id, &job).expect("approve");
    let approved = store::get_draft(conn, CLIENT, &draft.draft_id)
        .expect("get")
        .expect("present")
        .draft;
    assert!(
        approved.enrichment_trace.is_none(),
        "trace cleared on approval"
    );
    assert!(
        approved.research_annotations.is_empty(),
        "research annotations clear with the trace on approval"
    );
}

#[test]
fn apply_web_enrichment_replaces_weak_ai_company_name() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let mut draft = staged_draft();
    draft.company_name = Some("business-63644db2f2.example.test".to_string());
    store::insert_draft(conn, CLIENT, "jordan", &draft, "produce_weak_name").expect("stage");

    let apply = WebEnrichmentApply {
        company_name: Some(EnrichedValue {
            value: "example Stays".to_string(),
            provenance_quote: "page:https://business-63644db2f2.example.test/".to_string(),
        }),
        ..WebEnrichmentApply::default()
    };
    let trace = bos_contracts::crm_record_drafts::CrmEnrichmentTrace {
        captured_at_ms: 4_000,
        domain: "business-63644db2f2.example.test".to_string(),
        pages: vec!["https://business-63644db2f2.example.test/".to_string()],
        items: vec![bos_contracts::crm_record_drafts::CrmEnrichmentTraceItem {
            field: "company_name".to_string(),
            previous_value: Some("business-63644db2f2.example.test".to_string()),
            value: "example Stays".to_string(),
            source: "page:https://business-63644db2f2.example.test/".to_string(),
            via: "deterministic".to_string(),
        }],
        llm_ran: false,
        llm_input_chars: 42,
        llm_input_preview: "Welcome to example Stays".to_string(),
        search_ran: false,
        search_reason: None,
        search_queries: Vec::new(),
        search_results: Vec::new(),
        failures: Vec::new(),
        research_annotations: Vec::new(),
    };
    let ctx = DraftActionContext {
        client_id: CLIENT,
        actor_id: "crm_web_enrichment",
        expected_revision: None,
        idempotency_key: "crmenrich_weak_name",
        now_ms: 4_000,
    };
    store::apply_web_enrichment(conn, ctx, &draft.draft_id, &apply, Some(&trace)).expect("apply");

    let stored = store::get_draft(conn, CLIENT, &draft.draft_id)
        .expect("get")
        .expect("present")
        .draft;
    assert_eq!(stored.company_name.as_deref(), Some("example Stays"));
    assert!(stored.provenance.iter().any(|p| p.field == "company_name"
        && p.quote == "page:https://business-63644db2f2.example.test/"));
    assert!(stored
        .enrichment_trace
        .expect("trace")
        .items
        .iter()
        .any(|item| item.field == "company_name"
            && item.previous_value.as_deref() == Some("business-63644db2f2.example.test")));
}

#[test]
fn apply_web_enrichment_does_not_replace_operator_edited_company_name() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let mut draft = staged_draft();
    draft.company_name = Some("RS Hospitality".to_string());
    store::insert_draft(conn, CLIENT, "jordan", &draft, "produce_operator_name").expect("stage");

    let apply = WebEnrichmentApply {
        company_name: Some(EnrichedValue {
            value: "example Stays".to_string(),
            provenance_quote: "page:https://business-63644db2f2.example.test/".to_string(),
        }),
        ..WebEnrichmentApply::default()
    };
    let ctx = DraftActionContext {
        client_id: CLIENT,
        actor_id: "crm_web_enrichment",
        expected_revision: None,
        idempotency_key: "crmenrich_operator_name",
        now_ms: 4_000,
    };
    store::apply_web_enrichment(conn, ctx, &draft.draft_id, &apply, None).expect("apply");

    let stored = store::get_draft(conn, CLIENT, &draft.draft_id)
        .expect("get")
        .expect("present")
        .draft;
    assert_eq!(stored.company_name.as_deref(), Some("RS Hospitality"));
}

/// RecordFill helper coverage so the contact-name assembly stays correct.
#[test]
fn contact_full_name_joins_present_parts() {
    let fill = RecordFill {
        contact_first_name: Some("casey".to_string()),
        contact_last_name: Some("Sullivan".to_string()),
        ..RecordFill::default()
    };
    assert_eq!(fill.contact_full_name(), "casey Sullivan");
    let last_only = RecordFill {
        contact_last_name: Some("Sullivan".to_string()),
        ..RecordFill::default()
    };
    assert_eq!(last_only.contact_full_name(), "Sullivan");
}
