use bos_contracts::calendar_drafts::DraftFieldProvenance;
use bos_contracts::enrichment::{
    EnrichmentConfidence, EnrichmentEligibility, EnrichmentFieldProposal, EnrichmentFieldSpec,
    EnrichmentKickoffRequest, EnrichmentMode, EnrichmentPlan, EnrichmentRun, EnrichmentRunStatus,
    EnrichmentSeedEvidence, EnrichmentTier, EnrichmentTierEvent,
};
use bos_contracts::work_queue::{WorkItem, WorkItemStatus};
use std::cell::Cell;

use super::service::{
    self, EnrichmentOutcome, EnrichmentRunContext, EnrichmentRunHandle, EnrichmentSubject,
    ValueKindAcceptanceStatus,
};
use super::store::{self, FinishRun, OnDemandKickoff, StartRun};
use crate::http::test_support;
use crate::http::AppState;
use crate::persistence::Persistence;
use crate::store_core::MutationOutcome;

const CLIENT: &str = "test-client";

fn plan() -> EnrichmentPlan {
    EnrichmentPlan {
        subject: "crm_record_company".to_string(),
        fields: vec![EnrichmentFieldSpec {
            field_id: "company_phone".to_string(),
            value_kind: "phone".to_string(),
            eligibility: EnrichmentEligibility::MissingOnly,
            min_confidence: EnrichmentConfidence::Medium,
            provenance_required: true,
            operator_override: true,
        }],
        seed_evidence: vec![EnrichmentSeedEvidence {
            source_id: "email:msg_1".to_string(),
            label: "Source".to_string(),
            quote: Some("Call Example Company".to_string()),
        }],
        enabled_tiers: vec![EnrichmentTier::Local, EnrichmentTier::WebSearch],
        stop_policy: vec!["all_fields_accepted".to_string()],
    }
}

#[test]
fn enrichment_run_json_is_byte_stable_when_pr2_fields_are_none() {
    let mut diagnostics = vec![
        service::source_evidence_event("email:msg_1", "operator_source_loaded"),
        service::field_event(
            EnrichmentTier::Local,
            "company_phone",
            "(415) 555-0199",
            "accepted",
            "existing_draft_prefill",
            Some("Call (415) 555-0199"),
        ),
    ];
    diagnostics.extend(super::web_tier::page_fetch_events(&[
        bos_integrations::web_page_read::FetchedPage {
            url: "https://example.com/contact".to_string(),
            html: "<html><body>Contact</body></html>".to_string(),
        },
    ]));
    diagnostics.extend(super::web_tier::search_evidence_events(
        &bos_integrations::web_search_enrichment::SearchEvidence {
            purpose: "crm_record_drafts".to_string(),
            reason: "weak_domain_company_name".to_string(),
            queries: vec!["example official phone".to_string()],
            results: vec![bos_integrations::web_search_enrichment::SearchResult {
                query: "example official phone".to_string(),
                title: "Example Contact".to_string(),
                url: "https://example.com/contact".to_string(),
                snippet: "Call Example".to_string(),
            }],
            pages: Vec::new(),
            failures: vec!["search_timeout".to_string()],
        },
    ));
    let run = EnrichmentRun {
        run_id: "enr_1".to_string(),
        slice_id: "crm_record_drafts".to_string(),
        draft_id: "crd_1".to_string(),
        item_id: "item_1".to_string(),
        subject: "crm_record_company".to_string(),
        status: EnrichmentRunStatus::Completed,
        started_at_ms: 10,
        finished_at_ms: Some(20),
        plan: plan(),
        diagnostics,
        proposals: vec![EnrichmentFieldProposal {
            field_id: "company_phone".to_string(),
            proposed_value: "(415) 555-0199".to_string(),
            source_tier: EnrichmentTier::Local,
            confidence: EnrichmentConfidence::High,
            provenance_refs: vec!["email:msg_1".to_string()],
            accepted: true,
            reason: "existing_draft_prefill".to_string(),
        }],
        cost_micros: 0,
        created_by: "crm_web_enrichment".to_string(),
    };

    let serialized = serde_json::to_string(&run).expect("serialize");
    let expected = r#"{"run_id":"enr_1","slice_id":"crm_record_drafts","draft_id":"crd_1","item_id":"item_1","subject":"crm_record_company","status":"completed","started_at_ms":10,"finished_at_ms":20,"plan":{"subject":"crm_record_company","fields":[{"field_id":"company_phone","value_kind":"phone","eligibility":"missing_only","min_confidence":"medium","provenance_required":true,"operator_override":true}],"seed_evidence":[{"source_id":"email:msg_1","label":"Source","quote":"Call Example Company"}],"enabled_tiers":["local","web_search"],"stop_policy":["all_fields_accepted"]},"diagnostics":[{"event_type":"source_evidence","tier":"local","status":"available","reason":"operator_source_loaded","source_id":"email:msg_1","confidence":"high"},{"event_type":"field_proposal","tier":"local","field_id":"company_phone","status":"accepted","reason":"existing_draft_prefill","proposed_value":"(415) 555-0199","confidence":"high","quote":"Call (415) 555-0199"},{"event_type":"page_fetch","tier":"web_search","status":"fetched","source_id":"page:https://example.com/contact","url":"https://example.com/contact","final_url":"https://example.com/contact","bytes":33},{"event_type":"search_query","tier":"web_search","status":"completed","reason":"weak_domain_company_name","query":"example official phone"},{"event_type":"search_result","tier":"web_search","status":"considered","reason":"weak_domain_company_name","source_id":"search:https://example.com/contact","url":"https://example.com/contact","query":"example official phone","rank":1,"title":"Example Contact","snippet":"Call Example"},{"event_type":"failure","tier":"web_search","status":"skipped","reason":"search_timeout"}],"proposals":[{"field_id":"company_phone","proposed_value":"(415) 555-0199","source_tier":"local","confidence":"high","provenance_refs":["email:msg_1"],"accepted":true,"reason":"existing_draft_prefill"}],"cost_micros":0,"created_by":"crm_web_enrichment"}"#;
    assert_eq!(serialized, expected);
    assert!(!serialized.contains("action_kind"));
    assert!(!serialized.contains("budget_remaining"));
    assert!(!serialized.contains("refusal_code"));
    assert!(!serialized.contains("\"step\""));
}

#[test]
fn pr2_additive_contract_fields_deserialize_from_old_json_as_none() {
    let event: EnrichmentTierEvent =
        serde_json::from_str(r#"{"event_type":"field_proposal","tier":"web_search"}"#)
            .expect("old event json");
    assert_eq!(event.step, None);
    assert_eq!(event.action_kind, None);
    assert_eq!(event.budget_remaining, None);
    assert_eq!(event.refusal_code, None);

    let kickoff: EnrichmentKickoffRequest =
        serde_json::from_str(r#"{"idempotency_key":"idem_1","domain_seed":"example.com"}"#)
            .expect("old kickoff json");
    assert_eq!(kickoff.mode, None);
    assert_eq!(
        serde_json::to_string(&kickoff).expect("serialize old-mode kickoff"),
        r#"{"idempotency_key":"idem_1","domain_seed":"example.com"}"#
    );

    let research = EnrichmentKickoffRequest {
        idempotency_key: "idem_2".to_string(),
        domain_seed: None,
        mode: Some(EnrichmentMode::Research),
    };
    assert_eq!(
        serde_json::to_string(&research).expect("serialize research mode"),
        r#"{"idempotency_key":"idem_2","mode":"research"}"#
    );
}

#[test]
fn research_provider_registration_is_inert_with_feature_off() {
    assert!(service::registered_providers().iter().any(|provider| {
        provider.provider_id == "agentic_web_research" && provider.tier == EnrichmentTier::Research
    }));
    assert_eq!(
        crate::env_registry::BOS_AGENTIC_WEB_RESEARCH_ENABLED.default,
        None
    );
    assert!(!service::registered_providers()
        .iter()
        .any(|provider| provider.tier == EnrichmentTier::Research
            && provider.provider_id == "guarded_crawl"));
}

#[test]
fn start_and_finish_run_round_trips_contract_json() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let plan = plan();

    store::start_run(
        conn,
        CLIENT,
        "crm_web_enrichment",
        StartRun {
            run_id: "enr_1",
            slice_id: "crm_record_drafts",
            draft_id: "crd_1",
            item_id: "item_1",
            plan: &plan,
            created_by: "crm_web_enrichment",
            now_ms: 10,
        },
    )
    .expect("start");

    let diagnostics = vec![EnrichmentTierEvent {
        event_type: "field_proposal".to_string(),
        tier: EnrichmentTier::WebSearch,
        field_id: Some("company_phone".to_string()),
        status: Some("accepted".to_string()),
        reason: Some("literal_quote".to_string()),
        source_id: Some("page:https://example.com/".to_string()),
        url: Some("https://example.com/".to_string()),
        final_url: None,
        query: None,
        rank: None,
        title: None,
        snippet: None,
        proposed_value: Some("555-0100".to_string()),
        confidence: Some(EnrichmentConfidence::Medium),
        quote: Some("Call 555-0100".to_string()),
        latency_ms: None,
        bytes: None,
        cost_micros: None,
        ..Default::default()
    }];
    let proposals = vec![EnrichmentFieldProposal {
        field_id: "company_phone".to_string(),
        proposed_value: "555-0100".to_string(),
        source_tier: EnrichmentTier::WebSearch,
        confidence: EnrichmentConfidence::Medium,
        provenance_refs: vec!["page:https://example.com/".to_string()],
        accepted: true,
        reason: "literal_quote".to_string(),
    }];
    store::finish_run(
        conn,
        CLIENT,
        "crm_web_enrichment",
        FinishRun {
            run_id: "enr_1",
            status: EnrichmentRunStatus::Completed,
            diagnostics: &diagnostics,
            proposals: &proposals,
            cost_micros: 0,
            now_ms: 20,
            reason: "completed",
        },
    )
    .expect("finish");

    let rows = store::list_runs(
        conn,
        CLIENT,
        Some("crm_record_drafts"),
        Some("crd_1"),
        None,
        10,
    )
    .expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, EnrichmentRunStatus::Completed);
    assert_eq!(rows[0].plan.fields[0].field_id, "company_phone");
    assert_eq!(rows[0].diagnostics, diagnostics);
    assert_eq!(rows[0].proposals, proposals);
}

#[test]
fn last_accepted_proposal_at_uses_terminal_runs_and_critical_fields() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let plan = plan();

    store::start_run(
        conn,
        CLIENT,
        "crm_web_enrichment",
        StartRun {
            run_id: "enr_ignored_started",
            slice_id: "crm_record_drafts",
            draft_id: "crd_1",
            item_id: "item_1",
            plan: &plan,
            created_by: "crm_web_enrichment",
            now_ms: 10,
        },
    )
    .expect("start");
    store::append_run_diagnostics(
        conn,
        CLIENT,
        "crm_web_enrichment",
        store::AppendRunDiagnostics {
            run_id: "enr_ignored_started",
            event_seq: "tier1",
            diagnostics: &[],
            proposals: &[EnrichmentFieldProposal {
                field_id: "company_phone".to_string(),
                proposed_value: "555-0100".to_string(),
                source_tier: EnrichmentTier::WebSearch,
                confidence: EnrichmentConfidence::Medium,
                provenance_refs: vec!["page:https://example.com".to_string()],
                accepted: true,
                reason: "deterministic".to_string(),
            }],
            cost_micros: 0,
            now_ms: 11,
        },
    )
    .expect("append");

    store::start_run(
        conn,
        CLIENT,
        "crm_web_enrichment",
        StartRun {
            run_id: "enr_terminal",
            slice_id: "crm_record_drafts",
            draft_id: "crd_1",
            item_id: "item_1",
            plan: &plan,
            created_by: "crm_web_enrichment",
            now_ms: 20,
        },
    )
    .expect("start terminal");
    store::finish_run(
        conn,
        CLIENT,
        "crm_web_enrichment",
        FinishRun {
            run_id: "enr_terminal",
            status: EnrichmentRunStatus::Partial,
            diagnostics: &[],
            proposals: &[EnrichmentFieldProposal {
                field_id: "company_phone".to_string(),
                proposed_value: "555-0101".to_string(),
                source_tier: EnrichmentTier::WebSearch,
                confidence: EnrichmentConfidence::Medium,
                provenance_refs: vec!["page:https://example.com".to_string()],
                accepted: true,
                reason: "deterministic".to_string(),
            }],
            cost_micros: 0,
            now_ms: 30,
            reason: "partial",
        },
    )
    .expect("finish terminal");

    assert_eq!(
        store::last_accepted_proposal_at_ms(
            conn,
            CLIENT,
            "crm_record_drafts",
            "crd_1",
            "crm_record_company",
            &["company_phone"],
        )
        .expect("lookup"),
        Some(30)
    );
    assert_eq!(
        store::last_accepted_proposal_at_ms(
            conn,
            CLIENT,
            "crm_record_drafts",
            "crd_1",
            "crm_record_company",
            &["company_address"],
        )
        .expect("lookup"),
        None
    );
}

fn test_item() -> WorkItem {
    WorkItem {
        item_id: "item_1".to_string(),
        source_kind: "operator_note".to_string(),
        source_ref: "note_1".to_string(),
        category_id: "operator_note".to_string(),
        title: "Source mentioned example.com".to_string(),
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
        created_at_ms: 1,
        updated_at_ms: 1,
    }
}

struct FakeSubject {
    plan: EnrichmentPlan,
    web_called: Cell<bool>,
}

impl FakeSubject {
    fn new(enabled_tiers: Vec<EnrichmentTier>) -> Self {
        let mut plan = plan();
        plan.enabled_tiers = enabled_tiers;
        Self {
            plan,
            web_called: Cell::new(false),
        }
    }
}

impl EnrichmentSubject for FakeSubject {
    fn draft_id(&self) -> &str {
        "draft_1"
    }

    fn item_id(&self) -> &str {
        "item_1"
    }

    fn plan(&self) -> EnrichmentPlan {
        self.plan.clone()
    }

    fn tier1_events(&self) -> (Vec<EnrichmentTierEvent>, Vec<EnrichmentFieldProposal>) {
        (Vec::new(), Vec::new())
    }

    fn literal_domain(&self) -> Option<String> {
        Some("example.com".to_string())
    }

    fn run_web_search_tier(
        &self,
        state: &AppState,
        _ctx: EnrichmentRunContext<'_>,
        run: EnrichmentRunHandle<'_>,
        _domain: &str,
    ) -> EnrichmentOutcome {
        self.web_called.set(true);
        run.transition(state, EnrichmentRunStatus::Completed, "fake_web_completed")
    }
}

#[test]
fn runner_skips_web_when_plan_does_not_enable_web_search() {
    let state = test_support::test_state();
    let item = test_item();
    let subject = FakeSubject::new(vec![EnrichmentTier::Local]);

    let outcome = service::run(
        &state,
        EnrichmentRunContext {
            slice_id: "crm_record_drafts",
            actor_id: "test_enrichment",
            item: &item,
        },
        &subject,
    );

    assert!(!subject.web_called.get());
    assert_eq!(outcome.status, EnrichmentRunStatus::Skipped);
    assert_eq!(outcome.reason, "web_search_tier_disabled_by_plan");
    let persistence = state.persistence.lock();
    let rows = store::list_runs(
        persistence.connection_ref(),
        CLIENT,
        Some("crm_record_drafts"),
        Some("draft_1"),
        None,
        10,
    )
    .expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, EnrichmentRunStatus::Skipped);
    assert!(rows[0]
        .diagnostics
        .iter()
        .any(|event| event.reason.as_deref() == Some("web_search_tier_disabled_by_plan")));
}

#[test]
fn planned_run_id_changes_when_plan_seed_changes() {
    let item = test_item();
    let ctx = EnrichmentRunContext {
        slice_id: "crm_record_drafts",
        actor_id: "operator",
        item: &item,
    };
    let subject = FakeSubject::new(vec![EnrichmentTier::Local, EnrichmentTier::WebSearch]);
    let first = service::planned_run_id(ctx, &subject);
    assert_eq!(first, "enr_aebc0ca45f51b3e48adaa3c5de84bd1a");
    assert_eq!(first, service::planned_run_id(ctx, &subject));

    let mut plan = plan();
    plan.seed_evidence.push(EnrichmentSeedEvidence {
        source_id: "operator_domain_seed".to_string(),
        label: "Operator domain seed".to_string(),
        quote: Some("example.org".to_string()),
    });
    let overridden = FakeSubject {
        plan,
        web_called: Cell::new(false),
    };
    assert_ne!(first, service::planned_run_id(ctx, &overridden));
}

#[test]
fn freshness_epoch_changes_run_id_without_moving_default_run_id() {
    let item = test_item();
    let ctx = EnrichmentRunContext {
        slice_id: "crm_record_drafts",
        actor_id: "operator",
        item: &item,
    };
    let subject = FakeSubject::new(vec![EnrichmentTier::Local, EnrichmentTier::WebSearch]);
    let default = service::planned_run_id(ctx, &subject);
    assert_eq!(default, "enr_aebc0ca45f51b3e48adaa3c5de84bd1a");

    let stale_after_ms = 30 * 24 * 60 * 60 * 1000;
    let first_epoch = service::freshness_epoch(stale_after_ms, stale_after_ms + 123);
    let same_bucket = service::freshness_epoch(stale_after_ms, (2 * stale_after_ms) - 1);
    let next_bucket = service::freshness_epoch(stale_after_ms, 2 * stale_after_ms);
    assert_eq!(first_epoch, same_bucket);
    assert_ne!(first_epoch, next_bucket);

    let first_refresh = service::planned_run_id_with_epoch(ctx, &subject, &first_epoch);
    assert_ne!(default, first_refresh);
    assert_eq!(
        first_refresh,
        service::planned_run_id_with_epoch(ctx, &subject, &same_bucket)
    );
    assert_ne!(
        first_refresh,
        service::planned_run_id_with_epoch(ctx, &subject, &next_bucket)
    );
}

#[test]
fn research_run_id_fingerprint_splits_mode_and_unresolved_fields() {
    let item = test_item();
    let ctx = EnrichmentRunContext {
        slice_id: "crm_record_drafts",
        actor_id: "operator",
        item: &item,
    };
    let subject = FakeSubject::new(vec![EnrichmentTier::Local, EnrichmentTier::WebSearch]);
    let standard = service::planned_run_id(ctx, &subject);
    let research_email = service::planned_run_id_with_runtime_fingerprint(
        ctx,
        &subject,
        EnrichmentMode::Research,
        &["contact_email".to_string()],
    );
    let research_email_deduped = service::planned_run_id_with_runtime_fingerprint(
        ctx,
        &subject,
        EnrichmentMode::Research,
        &["contact_email".to_string(), "contact_email".to_string()],
    );
    let research_phone = service::planned_run_id_with_runtime_fingerprint(
        ctx,
        &subject,
        EnrichmentMode::Research,
        &["contact_phone".to_string()],
    );
    let standard_runtime = service::planned_run_id_with_runtime_fingerprint(
        ctx,
        &subject,
        EnrichmentMode::Standard,
        &["contact_email".to_string()],
    );
    assert_ne!(standard, research_email);
    assert_ne!(research_email, standard_runtime);
    assert_eq!(research_email, research_email_deduped);
    assert_ne!(research_email, research_phone);
}

#[test]
fn on_demand_kickoff_uses_operator_idempotency_key() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();

    let first = store::record_on_demand_kickoff(
        conn,
        CLIENT,
        "operator",
        OnDemandKickoff {
            run_id: "enr_kick",
            slice_id: "crm_record_drafts",
            draft_id: "crd_1",
            item_id: "item_1",
            idempotency_key: "operator_kick_1",
            now_ms: 10,
        },
    )
    .expect("kickoff");
    assert!(matches!(first.mutation, MutationOutcome::Applied { .. }));
    assert_eq!(first.run_id, "enr_kick");

    let replay = store::record_on_demand_kickoff(
        conn,
        CLIENT,
        "operator",
        OnDemandKickoff {
            run_id: "enr_changed_body",
            slice_id: "crm_record_drafts",
            draft_id: "crd_1",
            item_id: "item_1",
            idempotency_key: "operator_kick_1",
            now_ms: 11,
        },
    )
    .expect("replay");
    assert!(matches!(
        replay.mutation,
        MutationOutcome::ReplayedIdempotent { .. }
    ));
    assert_eq!(replay.run_id, "enr_kick");
}

#[test]
fn shared_prefill_helpers_match_existing_tier1_shape() {
    let source_id = "email:msg_1";
    let source_event = service::source_evidence_event(source_id, "operator_source_loaded");
    assert_eq!(
        source_event,
        EnrichmentTierEvent {
            event_type: "source_evidence".to_string(),
            tier: EnrichmentTier::Local,
            field_id: None,
            status: Some("available".to_string()),
            reason: Some("operator_source_loaded".to_string()),
            source_id: Some(source_id.to_string()),
            url: None,
            final_url: None,
            query: None,
            rank: None,
            title: None,
            snippet: None,
            proposed_value: None,
            confidence: Some(EnrichmentConfidence::High),
            quote: None,
            latency_ms: None,
            bytes: None,
            cost_micros: None,
            ..Default::default()
        }
    );

    let (events, proposals) = service::existing_prefill_events(
        source_id,
        [
            ("company_name", Some(" Example Company ")),
            ("company_phone", Some("")),
            ("company_email", None),
        ],
    );
    assert_eq!(
        events,
        vec![service::field_event(
            EnrichmentTier::Local,
            "company_name",
            "Example Company",
            "accepted",
            "existing_draft_prefill",
            None,
        )]
    );
    assert_eq!(
        proposals,
        vec![EnrichmentFieldProposal {
            field_id: "company_name".to_string(),
            proposed_value: "Example Company".to_string(),
            source_tier: EnrichmentTier::Local,
            confidence: EnrichmentConfidence::High,
            provenance_refs: vec![source_id.to_string()],
            accepted: true,
            reason: "existing_draft_prefill".to_string(),
        }]
    );
}

#[test]
fn shared_grounding_primitives_match_existing_filters() {
    let provenance = vec![DraftFieldProvenance {
        field: "company_name".to_string(),
        quote: "Please add Example Company as a customer".to_string(),
    }];
    assert!(service::quote_grounds(
        &provenance,
        &["company_name"],
        "Example Company"
    ));
    assert!(!service::quote_grounds(
        &provenance,
        &["contact_name"],
        "Example Company"
    ));
    assert!(service::literal_span_in_text(
        "Contact us at billing@example.com",
        "BILLING@example.com"
    ));
    assert!(service::literal_span_in_text(
        "Contact us at billing@example.com",
        " billing@example.com "
    ));
    assert!(service::literal_span_in_text(
        "Location:\n997 Morrison Dr\tCharleston, SC 29203",
        "997 Morrison Dr Charleston, SC 29203"
    ));
    assert!(service::quote_contains_value(
        "Call us at 555-0100",
        "555-0100"
    ));
    assert!(service::quote_contains_value(
        "LOCATION: 997 Morrison Dr, Charleston, SC 29203",
        "997 Morrison Dr Charleston SC 29203"
    ));
    assert!(!service::quote_contains_value(
        "Please add Example Company",
        " "
    ));
    assert!(!service::quote_grounds(&provenance, &["company_name"], ""));
    assert!(service::valid_email_shape("billing@example.com"));
    assert!(!service::valid_email_shape("billing example.com"));
}

#[test]
fn shared_web_diagnostics_preserve_explicit_provenance_refs() {
    let (events, proposals) = super::web_tier::accepted_value_diagnostics(
        [super::web_tier::AcceptedValue {
            field_id: "company_facts",
            value: "Example Company offers extended stay lodging.",
            quote: "Example Company offers extended stay lodging.",
            provenance_refs: vec!["web:target:abc123".to_string()],
        }],
        EnrichmentTier::WebSearch,
        "deterministic",
    );

    assert_eq!(
        events,
        vec![service::field_event(
            EnrichmentTier::WebSearch,
            "company_facts",
            "Example Company offers extended stay lodging.",
            "accepted",
            "deterministic",
            Some("Example Company offers extended stay lodging."),
        )]
    );
    assert_eq!(
        proposals,
        vec![EnrichmentFieldProposal {
            field_id: "company_facts".to_string(),
            proposed_value: "Example Company offers extended stay lodging.".to_string(),
            source_tier: EnrichmentTier::WebSearch,
            confidence: EnrichmentConfidence::Medium,
            provenance_refs: vec!["web:target:abc123".to_string()],
            accepted: true,
            reason: "deterministic".to_string(),
        }]
    );
}

#[test]
fn shared_page_and_search_events_match_existing_shapes() {
    let pages = vec![bos_integrations::web_page_read::FetchedPage {
        url: "https://example.com/".to_string(),
        html: "<html>hello</html>".to_string(),
    }];
    assert_eq!(
        super::web_tier::page_fetch_events(&pages),
        vec![EnrichmentTierEvent {
            event_type: "page_fetch".to_string(),
            tier: EnrichmentTier::WebSearch,
            field_id: None,
            status: Some("fetched".to_string()),
            reason: None,
            source_id: Some("page:https://example.com/".to_string()),
            url: Some("https://example.com/".to_string()),
            final_url: Some("https://example.com/".to_string()),
            query: None,
            rank: None,
            title: None,
            snippet: None,
            proposed_value: None,
            confidence: None,
            quote: None,
            latency_ms: None,
            bytes: Some("<html>hello</html>".len() as u64),
            cost_micros: None,
            ..Default::default()
        }]
    );

    let evidence = bos_integrations::web_search_enrichment::SearchEvidence {
        purpose: "crm_record_drafts".to_string(),
        reason: "weak_domain_company_name".to_string(),
        queries: vec!["example official company name".to_string()],
        results: vec![bos_integrations::web_search_enrichment::SearchResult {
            query: "example official company name".to_string(),
            title: "Example Co".to_string(),
            url: "https://example.com/about".to_string(),
            snippet: "Example Co is...".to_string(),
        }],
        pages: Vec::new(),
        failures: vec!["search_timeout".to_string()],
    };
    let events = super::web_tier::search_evidence_events(&evidence);
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].event_type, "search_query");
    assert_eq!(events[0].status.as_deref(), Some("completed"));
    assert_eq!(events[1].event_type, "search_result");
    assert_eq!(events[1].rank, Some(1));
    assert_eq!(events[2].event_type, "failure");
    assert_eq!(events[2].reason.as_deref(), Some("search_timeout"));
}

#[test]
fn value_kind_acceptance_verdicts_are_in_memory_only() {
    let mut plan = plan();
    plan.fields = vec![service::field_spec(
        "company_email",
        service::VALUE_KIND_EMAIL,
        EnrichmentEligibility::MissingOnly,
        EnrichmentConfidence::Medium,
    )];
    let diagnostics = vec![service::field_event(
        EnrichmentTier::WebSearch,
        "company_email",
        "billing@example.com",
        "accepted",
        "ai",
        Some("Email billing@example.com for invoices"),
    )];
    let proposals = vec![EnrichmentFieldProposal {
        field_id: "company_email".to_string(),
        proposed_value: "billing@example.com".to_string(),
        source_tier: EnrichmentTier::WebSearch,
        confidence: EnrichmentConfidence::Medium,
        provenance_refs: vec!["Email billing@example.com for invoices".to_string()],
        accepted: true,
        reason: "ai".to_string(),
    }];

    let verdicts = service::value_kind_acceptance_verdicts(&plan, &diagnostics, &proposals);
    assert_eq!(verdicts.len(), 1);
    assert_eq!(verdicts[0].status, ValueKindAcceptanceStatus::Passed);
    assert_eq!(verdicts[0].reason, "accepted_by_current_registry");
    assert!(proposals[0].accepted);
}

#[test]
fn value_kind_acceptance_marks_would_reject_without_filtering() {
    let mut plan = plan();
    plan.fields = vec![service::field_spec(
        "company_email",
        service::VALUE_KIND_EMAIL,
        EnrichmentEligibility::MissingOnly,
        EnrichmentConfidence::High,
    )];
    let proposals = vec![EnrichmentFieldProposal {
        field_id: "company_email".to_string(),
        proposed_value: "billing@example.com".to_string(),
        source_tier: EnrichmentTier::WebSearch,
        confidence: EnrichmentConfidence::Medium,
        provenance_refs: vec!["Email billing@example.com".to_string()],
        accepted: true,
        reason: "ai".to_string(),
    }];

    let verdicts = service::value_kind_acceptance_verdicts(&plan, &[], &proposals);
    assert!(proposals[0].accepted);
    assert_eq!(verdicts[0].status, ValueKindAcceptanceStatus::WouldReject);
    assert_eq!(verdicts[0].reason, "below_min_confidence");

    let mut invalid_email = proposals.clone();
    invalid_email[0].confidence = EnrichmentConfidence::High;
    invalid_email[0].proposed_value = "billing example.com".to_string();
    let verdicts = service::value_kind_acceptance_verdicts(&plan, &[], &invalid_email);
    assert!(invalid_email[0].accepted);
    assert_eq!(verdicts[0].status, ValueKindAcceptanceStatus::WouldReject);
    assert_eq!(verdicts[0].reason, "invalid_email_shape");
}

#[test]
fn value_kind_citation_quote_grounds_deterministic_extraction() {
    let mut plan = plan();
    plan.fields = vec![service::field_spec(
        "company_phone",
        service::VALUE_KIND_PHONE,
        EnrichmentEligibility::MissingOnly,
        EnrichmentConfidence::Medium,
    )];
    // Deterministic extraction: the field_proposal quote IS the provenance citation
    // (`page:<url>`), not a literal text span — it must ground by source, not reject.
    let citation = "page:https://book.example.com/all-rentals";
    let proposals = vec![EnrichmentFieldProposal {
        field_id: "company_phone".to_string(),
        proposed_value: "(843) 882-9224".to_string(),
        source_tier: EnrichmentTier::WebSearch,
        confidence: EnrichmentConfidence::Medium,
        provenance_refs: vec![citation.to_string()],
        accepted: true,
        reason: "deterministic".to_string(),
    }];
    let diagnostics = vec![EnrichmentTierEvent {
        event_type: "field_proposal".to_string(),
        tier: EnrichmentTier::WebSearch,
        field_id: Some("company_phone".to_string()),
        status: Some("accepted".to_string()),
        reason: Some("deterministic".to_string()),
        source_id: None,
        url: None,
        final_url: None,
        query: None,
        rank: None,
        title: None,
        snippet: None,
        proposed_value: Some("(843) 882-9224".to_string()),
        confidence: Some(EnrichmentConfidence::Medium),
        quote: Some(citation.to_string()),
        latency_ms: None,
        bytes: None,
        cost_micros: None,
        ..Default::default()
    }];

    let verdicts = service::value_kind_acceptance_verdicts(&plan, &diagnostics, &proposals);
    assert_eq!(verdicts[0].status, ValueKindAcceptanceStatus::Passed);

    // Regression guard: a real TEXT quote that does not contain the value still rejects.
    let mut text_quote = diagnostics.clone();
    text_quote[0].quote = Some("Contact us for booking details".to_string());
    let verdicts = service::value_kind_acceptance_verdicts(&plan, &text_quote, &proposals);
    assert_eq!(verdicts[0].status, ValueKindAcceptanceStatus::WouldReject);
    assert_eq!(verdicts[0].reason, "quote_does_not_ground_value");

    // Literal quotes may also appear in provenance_refs for AI gap-fill proposals;
    // that must not bypass value grounding.
    let mut text_ref_proposals = proposals.clone();
    text_ref_proposals[0].provenance_refs = vec!["Contact us for booking details".to_string()];
    let verdicts = service::value_kind_acceptance_verdicts(&plan, &text_quote, &text_ref_proposals);
    assert_eq!(verdicts[0].status, ValueKindAcceptanceStatus::WouldReject);
    assert_eq!(verdicts[0].reason, "quote_does_not_ground_value");
}

#[test]
fn value_kind_acceptance_missing_quote_is_not_a_hard_gate() {
    let mut plan = plan();
    plan.fields = vec![service::field_spec(
        "company_name",
        service::VALUE_KIND_NAME,
        EnrichmentEligibility::MissingOnly,
        EnrichmentConfidence::Medium,
    )];
    let proposals = vec![EnrichmentFieldProposal {
        field_id: "company_name".to_string(),
        proposed_value: "Example Company".to_string(),
        source_tier: EnrichmentTier::Local,
        confidence: EnrichmentConfidence::High,
        provenance_refs: vec!["operator_note:note_1".to_string()],
        accepted: true,
        reason: "existing_draft_prefill".to_string(),
    }];

    let verdicts = service::value_kind_acceptance_verdicts(&plan, &[], &proposals);
    assert!(proposals[0].accepted);
    assert_eq!(verdicts[0].status, ValueKindAcceptanceStatus::Passed);
    assert_eq!(verdicts[0].reason, "quote_not_observed_for_annotation");
}

#[test]
fn value_kind_acceptance_tracing_does_not_change_persisted_diagnostics() {
    let state = test_support::test_state();
    let item = test_item();
    let diagnostics = vec![service::field_event(
        EnrichmentTier::Local,
        "company_phone",
        "555-0100",
        "accepted",
        "existing_draft_prefill",
        None,
    )];
    let proposals = vec![EnrichmentFieldProposal {
        field_id: "company_phone".to_string(),
        proposed_value: "555-0100".to_string(),
        source_tier: EnrichmentTier::Local,
        confidence: EnrichmentConfidence::High,
        provenance_refs: vec!["email:msg_1".to_string()],
        accepted: true,
        reason: "existing_draft_prefill".to_string(),
    }];
    struct Tier1ProposalSubject {
        diagnostics: Vec<EnrichmentTierEvent>,
        proposals: Vec<EnrichmentFieldProposal>,
    }
    impl EnrichmentSubject for Tier1ProposalSubject {
        fn draft_id(&self) -> &str {
            "draft_1"
        }

        fn item_id(&self) -> &str {
            "item_1"
        }

        fn plan(&self) -> EnrichmentPlan {
            let mut plan = plan();
            plan.enabled_tiers = vec![EnrichmentTier::Local];
            plan
        }

        fn tier1_events(&self) -> (Vec<EnrichmentTierEvent>, Vec<EnrichmentFieldProposal>) {
            (self.diagnostics.clone(), self.proposals.clone())
        }

        fn literal_domain(&self) -> Option<String> {
            Some("example.com".to_string())
        }

        fn run_web_search_tier(
            &self,
            state: &AppState,
            _ctx: EnrichmentRunContext<'_>,
            run: EnrichmentRunHandle<'_>,
            _domain: &str,
        ) -> EnrichmentOutcome {
            run.transition(state, EnrichmentRunStatus::Completed, "unused")
        }
    }
    let subject = Tier1ProposalSubject {
        diagnostics: diagnostics.clone(),
        proposals: proposals.clone(),
    };

    let outcome = service::run(
        &state,
        EnrichmentRunContext {
            slice_id: "crm_record_drafts",
            actor_id: "test_enrichment",
            item: &item,
        },
        &subject,
    );
    assert_eq!(outcome.status, EnrichmentRunStatus::Skipped);

    let persistence = state.persistence.lock();
    let rows = store::list_runs(
        persistence.connection_ref(),
        CLIENT,
        Some("crm_record_drafts"),
        Some(subject.draft_id()),
        None,
        10,
    )
    .expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(
        &rows[0].diagnostics[..diagnostics.len()],
        diagnostics.as_slice()
    );
    assert!(!rows[0]
        .diagnostics
        .iter()
        .any(|event| event.event_type == "value_kind_would_reject"));
    assert_eq!(rows[0].proposals, proposals);
}

#[test]
fn value_kind_would_reject_verdict_is_persisted_for_review() {
    let state = test_support::test_state();
    let item = test_item();
    let diagnostics = vec![service::field_event(
        EnrichmentTier::Local,
        "company_email",
        "billing@example.com",
        "accepted",
        "existing_draft_prefill",
        None,
    )];
    // company_email at Medium below the field's High floor -> would_reject.
    let proposals = vec![EnrichmentFieldProposal {
        field_id: "company_email".to_string(),
        proposed_value: "billing@example.com".to_string(),
        source_tier: EnrichmentTier::Local,
        confidence: EnrichmentConfidence::Medium,
        provenance_refs: vec!["email:msg_1".to_string()],
        accepted: true,
        reason: "existing_draft_prefill".to_string(),
    }];
    struct WouldRejectSubject {
        diagnostics: Vec<EnrichmentTierEvent>,
        proposals: Vec<EnrichmentFieldProposal>,
    }
    impl EnrichmentSubject for WouldRejectSubject {
        fn draft_id(&self) -> &str {
            "draft_wr"
        }

        fn item_id(&self) -> &str {
            "item_1"
        }

        fn plan(&self) -> EnrichmentPlan {
            let mut plan = plan();
            plan.enabled_tiers = vec![EnrichmentTier::Local];
            plan.fields = vec![service::field_spec(
                "company_email",
                service::VALUE_KIND_EMAIL,
                EnrichmentEligibility::MissingOnly,
                EnrichmentConfidence::High,
            )];
            plan
        }

        fn tier1_events(&self) -> (Vec<EnrichmentTierEvent>, Vec<EnrichmentFieldProposal>) {
            (self.diagnostics.clone(), self.proposals.clone())
        }

        fn literal_domain(&self) -> Option<String> {
            None
        }

        fn run_web_search_tier(
            &self,
            state: &AppState,
            _ctx: EnrichmentRunContext<'_>,
            run: EnrichmentRunHandle<'_>,
            _domain: &str,
        ) -> EnrichmentOutcome {
            run.transition(state, EnrichmentRunStatus::Completed, "unused")
        }
    }
    let subject = WouldRejectSubject {
        diagnostics: diagnostics.clone(),
        proposals: proposals.clone(),
    };

    let _ = service::run(
        &state,
        EnrichmentRunContext {
            slice_id: "crm_record_drafts",
            actor_id: "test_enrichment",
            item: &item,
        },
        &subject,
    );

    let persistence = state.persistence.lock();
    let rows = store::list_runs(
        persistence.connection_ref(),
        CLIENT,
        Some("crm_record_drafts"),
        Some(subject.draft_id()),
        None,
        10,
    )
    .expect("list");
    assert_eq!(rows.len(), 1);
    // The gate's would_reject verdict is recorded as a durable, reviewable event.
    let would_reject: Vec<_> = rows[0]
        .diagnostics
        .iter()
        .filter(|event| event.event_type == "value_kind_would_reject")
        .collect();
    assert_eq!(would_reject.len(), 1);
    assert_eq!(would_reject[0].field_id.as_deref(), Some("company_email"));
    assert_eq!(
        would_reject[0].reason.as_deref(),
        Some("below_min_confidence")
    );
    // Still NON-enforcing: the proposal itself is untouched.
    assert_eq!(rows[0].proposals, proposals);
}
