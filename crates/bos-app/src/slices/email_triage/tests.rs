use axum::body::Body;
use axum::http::{Request, StatusCode};
use bos_contracts::email_identity::{IdentityConfidence, ParsedInbound, RepresentedPartyCandidate};
use bos_contracts::email_triage::{
    CategoryRecord, EmailTriageCondition, EmailTriageConditionCatalogResponse,
    EmailTriageConditionId, EmailTriageConditionOperator, EmailTriageConditionV2,
    EmailTriageConditionValue, EmailTriageDryRunRequest, EmailTriageDryRunResponse,
    EmailTriageField, EmailTriageGmailCategory, EmailTriageInboxOptionsResponse,
    EmailTriageInboxResponse, EmailTriageInboxSettingsResponse,
    EmailTriageInboxSettingsUpdateRequest, EmailTriageMatchMode, EmailTriageOperator,
    EmailTriageRule, EmailTriageTriValue, FALLBACK_CATEGORY_ID,
};
use bos_contracts::operator_users::OperatorUser;
use bos_contracts::work_queue::{WorkItem, WorkItemStatus, WorkQueuePolicy};
use bos_integrations::crm_read::{CrmContactRecord, CrmDealRecord};
use http_body_util::BodyExt;
use std::sync::{Mutex, MutexGuard, OnceLock};
use tower::ServiceExt;

use super::service::{
    dry_run, dry_run_traces, dry_run_traces_with_fact_bags, merge_rules_for_dry_run,
    resolve_category, resolve_rule, MessageView,
};
use super::store::{self, RuleAction, RuleMutationContext};
use crate::http::{
    build_router,
    test_support::{test_state_configured, EnvGuard},
    OperatorScope,
};
use crate::persistence::Persistence;
use crate::store_core::MutationOutcome;

const CLIENT: &str = "test-client";

#[test]
fn gmail_ingest_cursor_advances_with_receipts_and_is_quiet_when_unchanged() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let query_hash = store::gmail_ingest_query_hash("in:inbox newer_than:14d");
    let first = store::GmailIngestCursor {
        query_hash: query_hash.clone(),
        next_page_token: Some("page-2".to_string()),
    };

    assert!(
        store::put_gmail_ingest_cursor(conn, CLIENT, "user-1", &first, 1_000)
            .expect("store first cursor")
    );
    assert!(
        !store::put_gmail_ingest_cursor(conn, CLIENT, "user-1", &first, 2_000)
            .expect("unchanged cursor")
    );
    assert_eq!(
        store::get_gmail_ingest_cursor(conn, CLIENT, "user-1").expect("read cursor"),
        Some(first)
    );

    let complete = store::GmailIngestCursor {
        query_hash,
        next_page_token: None,
    };
    assert!(
        store::put_gmail_ingest_cursor(conn, CLIENT, "user-1", &complete, 3_000)
            .expect("complete cursor")
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM receipts WHERE client_id = ?1 AND entity_kind = ?2",
            rusqlite::params![CLIENT, store::GMAIL_INGEST_CURSOR_ENTITY_KIND],
            |row| row.get::<_, i64>(0),
        )
        .expect("receipt count"),
        2
    );
}

#[test]
fn inbound_receipt_never_serializes_full_body_or_safe_headers() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let sentinel = "FULL_BODY_SENTINEL_MUST_NOT_ENTER_RECEIPT";
    let full_body = format!("{}{}", "x".repeat(700), sentinel);
    let message = store::InboundMessageRecord {
        source_key: "receipt-redaction".to_string(),
        message_id: "gmail-receipt-redaction".to_string(),
        thread_id: None,
        internal_date_ms: Some(1_000),
        from_addr: Some("sender@example.com".to_string()),
        to_addr: Some("operator@example.com".to_string()),
        subject: Some("Receipt redaction".to_string()),
        body_excerpt: "x".repeat(600),
        body_full: full_body,
        headers: vec![("List-Id".to_string(), "secret-list".to_string())],
        labels: Vec::new(),
        resolved_category: FALLBACK_CATEGORY_ID.to_string(),
        matched_rule_id: None,
        ingested_at_ms: 1_000,
        ai_triage_status: None,
        ai_triage_rationale: None,
        attachments: Vec::new(),
        source_user_id: None,
    };
    store::record_inbound_message(persistence.connection(), CLIENT, &message).expect("ingest");

    let after_json: String = persistence
        .connection_ref()
        .query_row(
            "SELECT after_json FROM receipts \
             WHERE client_id = ?1 AND entity_kind = 'email_inbound_message' \
               AND change_kind = 'ingest' AND outcome = 'applied'",
            [CLIENT],
            |row| row.get(0),
        )
        .expect("ingest receipt");
    let value: serde_json::Value = serde_json::from_str(&after_json).expect("receipt json");
    assert!(value.get("body_full").is_none());
    assert!(value.get("headers").is_none());
    assert!(!after_json.contains(sentinel));
    assert!(!after_json.contains("secret-list"));
}

fn call_log_rule() -> EmailTriageRule {
    EmailTriageRule {
        rule_id: "call-log".into(),
        conditions: vec![EmailTriageCondition {
            field: EmailTriageField::Subject,
            op: EmailTriageOperator::Contains,
            value: "call log".into(),
            header_name: None,
        }],
        conditions_v2: Vec::new(),
        match_mode: EmailTriageMatchMode::All,
        priority: 10,
        enabled: true,
        pinned_category: "operator_note".to_string(),
    }
}

fn personal_operator() -> OperatorUser {
    OperatorUser {
        user_id: "user_jordan".to_string(),
        display_name: "jordan".to_string(),
        active: true,
        archived_at_ms: None,
        default_calendar_id: None,
        created_at_ms: 1_000,
        updated_at_ms: 1_000,
    }
}

fn inbound_message(id: &str, source_user_id: Option<&str>) -> store::InboundMessageRecord {
    store::InboundMessageRecord {
        source_key: store::source_key_for(source_user_id, id),
        message_id: id.to_string(),
        thread_id: Some(format!("thr-{id}")),
        internal_date_ms: Some(1_000),
        from_addr: Some("customer@example.com".to_string()),
        to_addr: Some("ops@business-a91b8b0f88.example.test".to_string()),
        subject: Some("Follow up".to_string()),
        body_excerpt: "Body".to_string(),
        body_full: "Body".to_string(),
        headers: Vec::new(),
        labels: vec!["INBOX".to_string()],
        resolved_category: FALLBACK_CATEGORY_ID.to_string(),
        matched_rule_id: None,
        ingested_at_ms: 1_000,
        ai_triage_status: None,
        ai_triage_rationale: None,
        attachments: Vec::new(),
        source_user_id: source_user_id.map(str::to_string),
    }
}

fn crm_contact(id: &str, email: &str) -> CrmContactRecord {
    CrmContactRecord {
        provider_contact_id: id.to_string(),
        email: Some(email.to_string()),
        name: Some("Dana Customer".to_string()),
        company: Some("Acme Co".to_string()),
        phone: None,
        lifecycle_stage: Some("lead".to_string()),
        owner: None,
        last_activity_at: None,
    }
}

fn crm_deal(id: &str, email: &str, stage: &str, pipeline: &str) -> CrmDealRecord {
    CrmDealRecord {
        provider_deal_id: id.to_string(),
        name: Some("renovation project".to_string()),
        stage: Some(stage.to_string()),
        amount_cents: Some(12_345),
        currency: Some("USD".to_string()),
        pipeline: Some(pipeline.to_string()),
        close_date: Some("2026-07-01".to_string()),
        associated_contact_ids: Vec::new(),
        associated_contact_email: Some(email.to_string()),
        associated_contact_company: Some("Acme Co".to_string()),
    }
}

fn parsed_represented_party(email: &str) -> ParsedInbound {
    ParsedInbound {
        represented_parties: vec![RepresentedPartyCandidate {
            email: Some(email.to_string()),
            name: Some("Represented Customer".to_string()),
            phone: None,
            company: None,
            provenance: "body_fields".to_string(),
            confidence: IdentityConfidence::High,
        }],
        ..ParsedInbound::default()
    }
}

struct TestBackfillParser;

static TEST_BACKFILL_PARSER: TestBackfillParser = TestBackfillParser;
const TEST_BACKFILL_PARSER_ID: &str = "test_backfill_represented_party";
static TEST_BACKFILL_PARSER_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn test_backfill_parser_lock() -> MutexGuard<'static, ()> {
    TEST_BACKFILL_PARSER_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|err| err.into_inner())
}

impl bos_profile_api::InboundMessageParser for TestBackfillParser {
    fn parser_id(&self) -> &'static str {
        TEST_BACKFILL_PARSER_ID
    }

    fn parse(&self, input: &bos_profile_api::InboundParserInput) -> Option<ParsedInbound> {
        let body = input.body.as_deref().unwrap_or_default();
        let (_, email) = body.split_once("Customer Email:")?;
        let email = email.split_whitespace().next()?;
        Some(parsed_represented_party(email))
    }
}

fn set_runtime_override(
    conn: &mut rusqlite::Connection,
    var: &crate::env_registry::EnvVar,
    value: &str,
    key: &str,
) {
    crate::slices::admin_settings::store::upsert_override(
        conn,
        crate::slices::admin_settings::store::OverrideWrite {
            client_id: CLIENT,
            actor_id: "op_test",
            var_name: var.name,
            value,
            expected_revision: None,
            idempotency_key: key,
            now_ms: 1_000,
        },
    )
    .expect("setting override");
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

fn message(subject: &str) -> MessageView {
    MessageView {
        subject: Some(subject.to_string()),
        ..Default::default()
    }
}

fn ctx<'a>(idempotency_key: &'a str, expected_revision: Option<u64>) -> RuleMutationContext<'a> {
    RuleMutationContext {
        client_id: "test-client",
        actor_id: "op_test",
        expected_revision,
        idempotency_key,
        correlation_id: None,
        now_ms: 1_000,
    }
}

#[test]
fn resolver_pins_category_on_match_and_falls_back_otherwise() {
    let rules = vec![call_log_rule()];
    assert_eq!(
        resolve_category(&rules, FALLBACK_CATEGORY_ID, &message("Daily Call Log 6/9")),
        "operator_note"
    );
    assert_eq!(
        resolve_category(&rules, FALLBACK_CATEGORY_ID, &message("Invoice attached")),
        FALLBACK_CATEGORY_ID
    );
}

#[test]
fn resolver_skips_disabled_rules_and_respects_match_mode() {
    let mut disabled = call_log_rule();
    disabled.enabled = false;
    assert!(resolve_rule(&[disabled], &message("call log")).is_none());

    let any_rule = EmailTriageRule {
        rule_id: "any".into(),
        conditions: vec![
            EmailTriageCondition {
                field: EmailTriageField::Subject,
                op: EmailTriageOperator::Contains,
                value: "nope".into(),
                header_name: None,
            },
            EmailTriageCondition {
                field: EmailTriageField::Subject,
                op: EmailTriageOperator::StartsWith,
                value: "urgent".into(),
                header_name: None,
            },
        ],
        match_mode: EmailTriageMatchMode::Any,
        ..call_log_rule()
    };
    assert!(resolve_rule(&[any_rule], &message("URGENT: pay now")).is_some());
}

#[test]
fn header_and_regex_conditions_match() {
    let rule = EmailTriageRule {
        rule_id: "hdr".into(),
        conditions: vec![EmailTriageCondition {
            field: EmailTriageField::Header,
            op: EmailTriageOperator::Regex,
            value: r"^bulk".into(),
            header_name: Some("X-Mailer-Class".into()),
        }],
        ..call_log_rule()
    };
    let msg = MessageView {
        headers: vec![("x-mailer-class".into(), "Bulk-Promo".into())],
        ..Default::default()
    };
    assert!(resolve_rule(std::slice::from_ref(&rule), &msg).is_some());

    let bad_regex = EmailTriageRule {
        conditions: vec![EmailTriageCondition {
            field: EmailTriageField::Subject,
            op: EmailTriageOperator::Regex,
            value: "(unclosed".into(),
            header_name: None,
        }],
        ..rule
    };
    assert!(
        resolve_rule(&[bad_regex], &message("(unclosed")).is_none(),
        "non-compiling regex must never match"
    );
}

#[test]
fn crm_override_conditions_match_sender_facts() {
    let contact_rule = EmailTriageRule {
        rule_id: "known-contact".into(),
        conditions: vec![EmailTriageCondition {
            field: EmailTriageField::SenderInCrmContacts,
            op: EmailTriageOperator::Exists,
            value: String::new(),
            header_name: None,
        }],
        ..call_log_rule()
    };
    let domain_rule = EmailTriageRule {
        rule_id: "known-domain".into(),
        priority: 20,
        conditions: vec![EmailTriageCondition {
            field: EmailTriageField::SenderDomainInCrmCompanies,
            op: EmailTriageOperator::Equals,
            value: "true".into(),
            header_name: None,
        }],
        ..call_log_rule()
    };
    let msg = MessageView {
        from: Some("Ada <ada@example.com>".to_string()),
        ..Default::default()
    };

    assert!(resolve_rule(std::slice::from_ref(&contact_rule), &msg).is_none());

    let crm = super::facts::CrmFactOverrides {
        sender_contact_exists: Some(super::facts::CrmFactValue::live(
            super::facts::TriValue::True,
        )),
        sender_company_exists: Some(super::facts::CrmFactValue::live(
            super::facts::TriValue::True,
        )),
        ..Default::default()
    };
    let mut bag = super::facts::FactBag::new(None, "", &msg, None, None, crm.clone());
    assert_eq!(
        super::service::resolve_rule_with_fact_bag(
            &[contact_rule.clone(), domain_rule.clone()],
            &mut bag
        )
        .map(|rule| rule.rule_id.as_str()),
        Some("known-contact")
    );

    let results = dry_run_traces(
        &[domain_rule],
        FALLBACK_CATEGORY_ID,
        std::slice::from_ref(&msg),
        vec![crm],
    );
    assert_eq!(results[0].matched_rule_id.as_deref(), Some("known-domain"));
}

#[test]
fn store_upsert_list_action_round_trip_with_receipts() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();

    let outcome = store::upsert(conn, ctx("idem_1", None), &call_log_rule()).expect("upsert");
    assert!(matches!(
        outcome,
        MutationOutcome::Applied { revision: 1, .. }
    ));

    // Idempotent replay does not double-apply.
    let replay = store::upsert(conn, ctx("idem_1", None), &call_log_rule()).expect("replay");
    assert!(matches!(replay, MutationOutcome::ReplayedIdempotent { .. }));

    let listed = store::list_active(persistence.connection_ref(), "test-client").expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].rule.rule_id, "call-log");
    assert_eq!(listed[0].revision, 1);

    // Stale revision is rejected without mutating.
    let conflict = store::upsert(
        persistence.connection(),
        ctx("idem_2", Some(99)),
        &call_log_rule(),
    )
    .expect("conflict path");
    assert!(matches!(conflict, MutationOutcome::RevisionConflict { .. }));

    // Disable, then delete; deleted rules leave the active list.
    store::apply_action(
        persistence.connection(),
        ctx("idem_3", Some(1)),
        "call-log",
        RuleAction::Disable,
    )
    .expect("disable");
    let listed = store::list_active(persistence.connection_ref(), "test-client").expect("list");
    assert_eq!(listed.len(), 1);
    assert!(
        !listed[0].rule.enabled,
        "disable must reflect in the listed rule"
    );
    assert_eq!(listed[0].revision, 2);

    store::apply_action(
        persistence.connection(),
        ctx("idem_4", Some(2)),
        "call-log",
        RuleAction::Delete,
    )
    .expect("delete");
    let listed = store::list_active(persistence.connection_ref(), "test-client").expect("list");
    assert!(listed.is_empty(), "deleted rule must leave the active list");

    // Receipt trail covers the full history: upsert, replay, conflict, disable, delete.
    let receipts = crate::store_core::receipts_for_entity(
        persistence.connection_ref(),
        "test-client",
        store::ENTITY_KIND,
        "call-log",
        10,
    )
    .expect("receipts");
    assert_eq!(receipts.len(), 5);
}

#[test]
fn legacy_rule_cleanup_rewrites_json_without_changing_evaluation() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let rule = EmailTriageRule {
        rule_id: "legacy-display-name".into(),
        conditions: vec![
            EmailTriageCondition {
                field: EmailTriageField::From,
                op: EmailTriageOperator::Contains,
                value: "Acme Corp".into(),
                header_name: None,
            },
            EmailTriageCondition {
                field: EmailTriageField::To,
                op: EmailTriageOperator::Contains,
                value: "ops@business-0daad63254.example.test".into(),
                header_name: None,
            },
            EmailTriageCondition {
                field: EmailTriageField::SenderInCrmContacts,
                op: EmailTriageOperator::Exists,
                value: String::new(),
                header_name: None,
            },
        ],
        conditions_v2: Vec::new(),
        match_mode: EmailTriageMatchMode::Any,
        ..call_log_rule()
    };
    store::upsert(conn, ctx("legacy-cleanup-seed", None), &rule).expect("seed legacy rule");

    let fixture = MessageView {
        from: Some("Acme Corp <ada@gmail.com>".to_string()),
        to: Some("ops@business-0daad63254.example.test".to_string()),
        ..Default::default()
    };
    let before = store::list_active(conn, "test-client").expect("before");
    let before_category = resolve_category(
        &before
            .iter()
            .map(|stored| stored.rule.clone())
            .collect::<Vec<_>>(),
        FALLBACK_CATEGORY_ID,
        &fixture,
    );
    assert_eq!(before_category, "operator_note");

    let changed = store::cleanup_legacy_rule_json(conn, 9_000).expect("cleanup");
    assert_eq!(changed, 1);
    let after = store::list_active(conn, "test-client").expect("after");
    let cleaned = &after[0].rule;
    assert!(cleaned.conditions.is_empty());
    assert_eq!(cleaned.conditions_v2.len(), 3);
    assert_eq!(
        cleaned.conditions_v2[0].condition_id,
        EmailTriageConditionId::MessageFrom,
        "legacy From must stay raw"
    );
    assert_eq!(
        cleaned.conditions_v2[1].condition_id,
        EmailTriageConditionId::MessageTo,
        "legacy To must stay raw"
    );
    assert_eq!(
        cleaned.conditions_v2[2].condition_id,
        EmailTriageConditionId::CrmSenderContactExists
    );
    assert_eq!(
        cleaned.conditions_v2[2].op,
        EmailTriageConditionOperator::IsTrue
    );
    let after_category = resolve_category(
        &after
            .iter()
            .map(|stored| stored.rule.clone())
            .collect::<Vec<_>>(),
        FALLBACK_CATEGORY_ID,
        &fixture,
    );
    assert_eq!(after_category, before_category);
    assert_eq!(
        store::cleanup_legacy_rule_json(conn, 10_000).expect("again"),
        0
    );

    let receipts = crate::store_core::receipts_for_entity(
        conn,
        "test-client",
        store::ENTITY_KIND,
        "legacy-display-name",
        10,
    )
    .expect("receipts");
    assert!(
        receipts
            .iter()
            .any(|receipt| receipt.change_kind == "legacy_cleanup"),
        "cleanup must be receipted"
    );
}

#[test]
fn store_rejects_invalid_rule() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let invalid = EmailTriageRule {
        rule_id: "bad".into(),
        conditions: Vec::new(),
        ..call_log_rule()
    };
    let err = store::upsert(persistence.connection(), ctx("idem_1", None), &invalid)
        .expect_err("must reject");
    assert!(err
        .to_string()
        .contains("email_triage_rule_conditions_required"));
}

#[test]
fn dry_run_inserts_proposed_rule_in_priority_order() {
    let stored = call_log_rule();
    let proposed = EmailTriageRule {
        rule_id: "catch-all".into(),
        conditions: vec![EmailTriageCondition {
            field: EmailTriageField::Subject,
            op: EmailTriageOperator::Exists,
            value: String::new(),
            header_name: None,
        }],
        pinned_category: "operator_note".to_string(),
        ..call_log_rule()
    };
    let merged = merge_rules_for_dry_run(vec![stored], vec![proposed]);
    let results = dry_run(
        &merged,
        FALLBACK_CATEGORY_ID,
        &[message("call log today"), message("anything else")],
    );
    assert_eq!(results[0].matched_rule_id.as_deref(), Some("call-log"));
    assert_eq!(results[0].resolved_category, "operator_note");
    assert_eq!(results[1].matched_rule_id.as_deref(), Some("catch-all"));
    assert_eq!(results[1].resolved_category, "operator_note");
}

#[test]
fn dry_run_proposed_rule_replaces_saved_rule_with_same_id() {
    let stored = call_log_rule();
    let proposed = EmailTriageRule {
        conditions: vec![EmailTriageCondition {
            field: EmailTriageField::From,
            op: EmailTriageOperator::Contains,
            value: "@example.com".into(),
            header_name: None,
        }],
        pinned_category: "sales_lead".to_string(),
        ..call_log_rule()
    };
    let merged = merge_rules_for_dry_run(vec![stored], vec![proposed]);
    let samples = vec![
        message("call log today"),
        MessageView {
            from: Some("Alice <alice@example.com>".to_string()),
            ..Default::default()
        },
    ];

    let results = dry_run(&merged, FALLBACK_CATEGORY_ID, &samples);

    assert!(results[0].matched_rule_id.is_none());
    assert_eq!(results[0].resolved_category, FALLBACK_CATEGORY_ID);
    assert_eq!(results[1].matched_rule_id.as_deref(), Some("call-log"));
    assert_eq!(results[1].resolved_category, "sales_lead");
}

#[test]
fn dry_run_applies_proposed_domain_contains_condition() {
    let proposed = EmailTriageRule {
        rule_id: "from-example-com".into(),
        conditions: vec![EmailTriageCondition {
            field: EmailTriageField::From,
            op: EmailTriageOperator::Contains,
            value: "@example.com".into(),
            header_name: None,
        }],
        pinned_category: "operator_note".to_string(),
        ..call_log_rule()
    };
    let samples = vec![
        MessageView {
            from: Some("Alice <alice@example.com>".to_string()),
            ..Default::default()
        },
        MessageView {
            from: Some("Bob <bob@business-65dc317ec5.example.test>".to_string()),
            ..Default::default()
        },
    ];

    let results = dry_run(&[proposed], FALLBACK_CATEGORY_ID, &samples);

    assert_eq!(
        results[0].matched_rule_id.as_deref(),
        Some("from-example-com")
    );
    assert_eq!(results[0].resolved_category, "operator_note");
    assert!(results[1].matched_rule_id.is_none());
    assert_eq!(results[1].resolved_category, FALLBACK_CATEGORY_ID);
}

#[test]
fn source_provider_in_condition_matches_string_list() {
    let rule = EmailTriageRule {
        rule_id: "gmail-source".into(),
        conditions: Vec::new(),
        conditions_v2: vec![EmailTriageConditionV2 {
            condition_id: EmailTriageConditionId::SourceProvider,
            op: EmailTriageConditionOperator::In,
            value: EmailTriageConditionValue::StringList(vec![
                "gmail".to_string(),
                "outlook".to_string(),
            ]),
        }],
        match_mode: EmailTriageMatchMode::All,
        priority: 10,
        enabled: true,
        pinned_category: "operator_note".to_string(),
    };
    let results = dry_run(&[rule], FALLBACK_CATEGORY_ID, &[MessageView::default()]);

    assert_eq!(results[0].matched_rule_id.as_deref(), Some("gmail-source"));
    assert_eq!(results[0].resolved_category, "operator_note");
}

#[test]
fn dry_run_trace_marks_unknown_fact_refresh_without_matching() {
    let rule = EmailTriageRule {
        rule_id: "known-customer".into(),
        conditions: Vec::new(),
        conditions_v2: vec![EmailTriageConditionV2 {
            condition_id: EmailTriageConditionId::CrmSenderContactExists,
            op: EmailTriageConditionOperator::IsTrue,
            value: EmailTriageConditionValue::Empty,
        }],
        match_mode: EmailTriageMatchMode::All,
        priority: 10,
        enabled: true,
        pinned_category: "operator_note".to_string(),
    };
    let traces = dry_run_traces(
        &[rule],
        FALLBACK_CATEGORY_ID,
        &[MessageView {
            from: Some("Ada <ada@example.com>".to_string()),
            ..Default::default()
        }],
        Vec::new(),
    );

    assert_eq!(traces[0].matched_rule_id, None);
    assert_eq!(traces[0].resolved_category, FALLBACK_CATEGORY_ID);
    assert!(traces[0].needs_fact_refresh);
    assert_eq!(
        traces[0].rule_traces[0].result,
        EmailTriageTriValue::Unknown
    );
}

#[test]
fn accounting_customer_fact_reads_local_snapshots() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    crate::slices::accounting::store::upsert_customer_snapshots(
        persistence.connection(),
        "test-client",
        &[bos_integrations::accounting_read::CustomerRecord {
            customer_id: "cust_1".to_string(),
            display_name: "Ada Co".to_string(),
            company_name: None,
            email: Some("ada@example.com".to_string()),
            phone: None,
            active: true,
            tier_raw: None,
            tier_source: bos_integrations::accounting_read::TierSource::NotProvided,
            updated_at: None,
        }],
        1_000,
    )
    .expect("customer snapshot");
    let conn = persistence.connection_ref();
    let rule = EmailTriageRule {
        rule_id: "accounting-customer".into(),
        conditions: Vec::new(),
        conditions_v2: vec![EmailTriageConditionV2 {
            condition_id: EmailTriageConditionId::AccountingSenderCustomerExists,
            op: EmailTriageConditionOperator::IsTrue,
            value: EmailTriageConditionValue::Empty,
        }],
        match_mode: EmailTriageMatchMode::All,
        priority: 10,
        enabled: true,
        pinned_category: "operator_note".to_string(),
    };
    let message = MessageView {
        from: Some("Ada <ada@example.com>".to_string()),
        ..Default::default()
    };
    let bag = super::facts::FactBag::new(
        Some(conn),
        "test-client",
        &message,
        Some("m1"),
        None,
        Default::default(),
    );
    let traces = dry_run_traces_with_fact_bags(&[rule], FALLBACK_CATEGORY_ID, vec![bag]);

    assert_eq!(
        traces[0].matched_rule_id.as_deref(),
        Some("accounting-customer")
    );
    assert!(!traces[0].needs_fact_refresh);
    assert_eq!(
        traces[0].fact_traces[0].source,
        bos_contracts::email_triage::EmailTriageFactSource::AccountingSnapshot
    );
}

#[test]
fn crm_fact_cache_upsert_is_receipted_and_company_cache_hit_is_used() {
    let _provider = EnvGuard::set("BOS_CRM_PROVIDER", "hubspot");
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let write = super::service::CrmFactCacheWrite {
        fact_key: "crm.sender_company.exists:example.com".to_string(),
        value: super::facts::TriValue::True,
        provider: crate::slices::crm_drafts::service::PROVIDER_HUBSPOT.to_string(),
        fetched_at_ms: 1_000,
        expires_at_ms: 3_601_000,
    };
    super::service::persist_crm_fact_cache_writes(conn, "test-client", &[write])
        .expect("cache write");

    let receipts = crate::store_core::receipts_for_entity(
        conn,
        "test-client",
        super::store::FACT_CACHE_ENTITY_KIND,
        "crm.sender_company.exists:example.com",
        10,
    )
    .expect("receipts");
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].change_kind, "upsert");

    let message = MessageView {
        from: Some("Ada <ada@example.com>".to_string()),
        ..Default::default()
    };
    let (crm, misses) =
        super::service::crm_fact_overrides_from_cache(conn, "test-client", &message, 2_000);
    assert!(misses
        .iter()
        .all(|miss| miss.kind != super::service::CrmFactKind::SenderCompany));
    let fact = crm.sender_company_exists.expect("cached company fact");
    assert_eq!(fact.value, super::facts::TriValue::True);
    assert_eq!(
        fact.source,
        bos_contracts::email_triage::EmailTriageFactSource::CrmCache
    );
    assert!(fact.detail.contains("saved lookup"));
}

#[test]
fn crm_company_fact_uses_contact_snapshot_domain_without_live_miss() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    crate::slices::crm_cache::store::upsert_contact_snapshots(
        conn,
        "test-client",
        &[CrmContactRecord {
            provider_contact_id: "c1".to_string(),
            email: Some("owner@example.com".to_string()),
            name: Some("Example Owner".to_string()),
            company: Some("Example Co".to_string()),
            phone: None,
            lifecycle_stage: None,
            owner: None,
            last_activity_at: None,
        }],
        1_000,
    )
    .expect("snapshot");

    let message = MessageView {
        from: Some("Ada <ada@example.com>".to_string()),
        ..Default::default()
    };
    let (crm, misses) =
        super::service::crm_fact_overrides_from_cache(conn, "test-client", &message, 2_000);

    assert!(misses
        .iter()
        .all(|miss| miss.kind != super::service::CrmFactKind::SenderCompany));
    let fact = crm.sender_company_exists.expect("company fact");
    assert_eq!(fact.value, super::facts::TriValue::True);
    assert_eq!(
        fact.source,
        bos_contracts::email_triage::EmailTriageFactSource::CrmCache
    );
    assert!(fact.detail.contains("contact snapshots"));
}

#[test]
fn crm_facts_skip_platform_sender_without_false_match_or_live_miss() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    crate::slices::crm_cache::store::upsert_contact_snapshots(
        conn,
        "test-client",
        &[CrmContactRecord {
            provider_contact_id: "shopify-contact".to_string(),
            email: Some("mailer@shopify.com".to_string()),
            name: Some("America's Best Varnish (Shopify)".to_string()),
            company: Some("America's Best Varnish".to_string()),
            phone: None,
            lifecycle_stage: Some("lead".to_string()),
            owner: None,
            last_activity_at: None,
        }],
        1_000,
    )
    .expect("snapshot");

    let message = MessageView {
        from: Some("Shopify <mailer@bounce.notifications.shopify.com>".to_string()),
        ..Default::default()
    };
    let (crm, misses) =
        super::service::crm_fact_overrides_from_cache(conn, "test-client", &message, 2_000);

    assert!(misses.is_empty());
    assert_eq!(
        crm.sender_contact_exists.expect("contact fact").value,
        super::facts::TriValue::Unknown
    );
    assert_eq!(
        crm.sender_company_exists.expect("company fact").value,
        super::facts::TriValue::Unknown
    );
}

#[test]
fn crm_facts_skip_neutral_email_local_sender_without_false_match_or_live_miss() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    crate::slices::crm_cache::store::upsert_contact_snapshots(
        conn,
        "test-client",
        &[CrmContactRecord {
            provider_contact_id: "shopify-email-contact".to_string(),
            email: Some("email@email.shopify.com".to_string()),
            name: Some("Shopify Updates".to_string()),
            company: Some("Shopify".to_string()),
            phone: None,
            lifecycle_stage: Some("lead".to_string()),
            owner: None,
            last_activity_at: None,
        }],
        1_000,
    )
    .expect("snapshot");

    let message = MessageView {
        from: Some("Shopify <email@email.shopify.com>".to_string()),
        ..Default::default()
    };
    let (crm, misses) =
        super::service::crm_fact_overrides_from_cache(conn, "test-client", &message, 2_000);

    assert!(misses.is_empty());
    assert_eq!(
        crm.sender_contact_exists.expect("contact fact").value,
        super::facts::TriValue::Unknown
    );
    assert_eq!(
        crm.sender_company_exists.expect("company fact").value,
        super::facts::TriValue::Unknown
    );
}

#[test]
fn crm_facts_skip_auto_submitted_sender_without_false_match_or_live_miss() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    crate::slices::crm_cache::store::upsert_contact_snapshots(
        conn,
        "test-client",
        &[CrmContactRecord {
            provider_contact_id: "auto-contact".to_string(),
            email: Some("ada@example.com".to_string()),
            name: Some("Ada Example".to_string()),
            company: Some("Example".to_string()),
            phone: None,
            lifecycle_stage: Some("lead".to_string()),
            owner: None,
            last_activity_at: None,
        }],
        1_000,
    )
    .expect("snapshot");

    let message = MessageView {
        from: Some("Ada <ada@example.com>".to_string()),
        headers: vec![("Auto-Submitted".to_string(), "auto-generated".to_string())],
        ..Default::default()
    };
    let (crm, misses) =
        super::service::crm_fact_overrides_from_cache(conn, "test-client", &message, 2_000);

    assert!(misses.is_empty());
    assert_eq!(
        crm.sender_contact_exists.expect("contact fact").value,
        super::facts::TriValue::Unknown
    );
}

#[test]
fn crm_facts_prefer_represented_party_email_with_trace_detail() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    crate::slices::crm_cache::store::upsert_contact_snapshots(
        conn,
        "test-client",
        &[CrmContactRecord {
            provider_contact_id: "represented-contact".to_string(),
            email: Some("alex@example.com".to_string()),
            name: Some("Alex Rivera".to_string()),
            company: Some("Example Co".to_string()),
            phone: None,
            lifecycle_stage: Some("lead".to_string()),
            owner: None,
            last_activity_at: None,
        }],
        1_000,
    )
    .expect("snapshot");

    let parsed = ParsedInbound {
        represented_parties: vec![RepresentedPartyCandidate {
            email: Some("alex@example.com".to_string()),
            name: Some("Alex Rivera".to_string()),
            phone: None,
            company: None,
            provenance: "body_fields".to_string(),
            confidence: IdentityConfidence::High,
        }],
        ..ParsedInbound::default()
    };
    store::upsert_inbound_enrichment(
        conn,
        store::InboundEnrichmentWrite {
            client_id: "test-client",
            source_key: "source-1",
            parser_id: "test_parser",
            parser_version: "1",
            parsed: &parsed,
            now_ms: 1_500,
        },
    )
    .expect("enrichment");

    let message = MessageView {
        message_id: Some("source-1".to_string()),
        from: Some("Platform <mailer@bounce.notifications.shopify.com>".to_string()),
        ..Default::default()
    };
    let (crm, misses) =
        super::service::crm_fact_overrides_from_cache(conn, "test-client", &message, 2_000);

    assert!(misses
        .iter()
        .all(|miss| miss.kind != super::service::CrmFactKind::SenderContact));
    let fact = crm.sender_contact_exists.expect("contact fact");
    assert_eq!(fact.value, super::facts::TriValue::True);
    assert!(fact.detail.contains("represented contact from test_parser"));
}

#[test]
fn refresh_represented_identity_populates_clears_and_receipts_message_row() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let mut message = inbound_message("m-represented", Some("user_jordan"));
    message.from_addr = Some("Platform <noreply@shopify.com>".to_string());
    store::record_inbound_message(conn, CLIENT, &message).expect("record inbound");
    assert!(
        store::refresh_represented_identity(conn, CLIENT, &message.source_key, 1_100)
            .expect("refresh unchanged")
            .is_none()
    );

    let parsed = parsed_represented_party("business-1798e87f34.example.test@Example.COM");
    store::upsert_inbound_enrichment(
        conn,
        store::InboundEnrichmentWrite {
            client_id: CLIENT,
            source_key: &message.source_key,
            parser_id: "test_parser",
            parser_version: "1",
            parsed: &parsed,
            now_ms: 1_200,
        },
    )
    .expect("enrichment");
    let applied = store::refresh_represented_identity(conn, CLIENT, &message.source_key, 1_300)
        .expect("refresh represented")
        .expect("represented identity changed");
    assert!(matches!(applied, MutationOutcome::Applied { .. }));

    let represented: (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT represented_email, represented_domain \
             FROM email_inbound_messages WHERE client_id = ?1 AND source_key = ?2",
            rusqlite::params![CLIENT, &message.source_key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("represented columns");
    assert_eq!(
        represented,
        (
            Some("business-1798e87f34.example.test@example.com".to_string()),
            Some("example.com".to_string())
        )
    );

    assert!(
        store::refresh_represented_identity(conn, CLIENT, &message.source_key, 1_400)
            .expect("refresh unchanged represented")
            .is_none()
    );

    let empty = ParsedInbound::default();
    store::upsert_inbound_enrichment(
        conn,
        store::InboundEnrichmentWrite {
            client_id: CLIENT,
            source_key: &message.source_key,
            parser_id: "test_parser",
            parser_version: "2",
            parsed: &empty,
            now_ms: 1_500,
        },
    )
    .expect("clear enrichment");
    let cleared = store::refresh_represented_identity(conn, CLIENT, &message.source_key, 1_600)
        .expect("refresh cleared")
        .expect("represented identity cleared");
    assert!(matches!(cleared, MutationOutcome::Applied { .. }));

    let represented: (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT represented_email, represented_domain \
             FROM email_inbound_messages WHERE client_id = ?1 AND source_key = ?2",
            rusqlite::params![CLIENT, &message.source_key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("cleared represented columns");
    assert_eq!(represented, (None, None));

    store::upsert_inbound_enrichment(
        conn,
        store::InboundEnrichmentWrite {
            client_id: CLIENT,
            source_key: &message.source_key,
            parser_id: "test_parser",
            parser_version: "3",
            parsed: &parsed,
            now_ms: 1_700,
        },
    )
    .expect("restore enrichment");
    store::refresh_represented_identity(conn, CLIENT, &message.source_key, 1_800)
        .expect("refresh restored")
        .expect("represented identity restored");
    let represented: (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT represented_email, represented_domain \
             FROM email_inbound_messages WHERE client_id = ?1 AND source_key = ?2",
            rusqlite::params![CLIENT, &message.source_key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("restored represented columns");
    assert_eq!(
        represented,
        (
            Some("business-1798e87f34.example.test@example.com".to_string()),
            Some("example.com".to_string())
        )
    );

    let receipts = crate::store_core::receipts_for_entity(
        conn,
        CLIENT,
        store::INBOUND_ENTITY_KIND,
        &message.source_key,
        10,
    )
    .expect("receipts");
    assert_eq!(
        receipts
            .iter()
            .filter(|receipt| receipt.change_kind == "represented_identity")
            .count(),
        3
    );
}

#[test]
fn enrichment_backfill_disabled_noops() {
    let _lock = test_backfill_parser_lock();
    let _parser_guard = super::service::set_test_inbound_parsers(vec![&TEST_BACKFILL_PARSER]);
    let mut state = test_state_configured(None, &[]);
    state.email_triage_overlay = std::sync::Arc::new(crate::overlay::EmailTriageOverlay {
        inbound_parser_ids: vec![TEST_BACKFILL_PARSER_ID.to_string()],
        inbox_defaults: Vec::new(),
    });
    let source_key = {
        let mut persistence = state.persistence.lock();
        let mut message = inbound_message("m-backfill-disabled", Some("user_jordan"));
        message.body_full = "Customer Email: alex@example.com".to_string();
        store::record_inbound_message(persistence.connection(), CLIENT, &message)
            .expect("record inbound");
        message.source_key
    };

    let summary = super::worker::run_enrichment_backfill_cycle(&state).expect("backfill cycle");
    assert_eq!(summary, super::worker::EnrichmentBackfillSummary::default());

    let persistence = state.persistence.lock();
    assert!(
        store::list_inbound_enrichments(persistence.connection_ref(), CLIENT, &source_key)
            .expect("enrichments")
            .is_empty()
    );
    let represented: (Option<String>, Option<String>) = persistence
        .connection_ref()
        .query_row(
            "SELECT represented_email, represented_domain \
             FROM email_inbound_messages WHERE client_id = ?1 AND source_key = ?2",
            rusqlite::params![CLIENT, &source_key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("represented columns");
    assert_eq!(represented, (None, None));
}

#[test]
fn enrichment_backfill_reprocesses_existing_mail_idempotently() {
    let _lock = test_backfill_parser_lock();
    let _parser_guard = super::service::set_test_inbound_parsers(vec![&TEST_BACKFILL_PARSER]);
    let mut state = test_state_configured(None, &[]);
    state.email_triage_overlay = std::sync::Arc::new(crate::overlay::EmailTriageOverlay {
        inbound_parser_ids: vec![TEST_BACKFILL_PARSER_ID.to_string()],
        inbox_defaults: Vec::new(),
    });
    let source_key = {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        set_runtime_override(
            conn,
            &crate::env_registry::BOS_EMAIL_ENRICHMENT_BACKFILL_ENABLED,
            "1",
            "enable-enrichment-backfill",
        );
        set_runtime_override(
            conn,
            &crate::env_registry::BOS_EMAIL_ENRICHMENT_BACKFILL_BATCH,
            "10",
            "set-enrichment-backfill-batch",
        );
        let mut message = inbound_message("m-backfill", Some("user_jordan"));
        message.from_addr = Some("Platform <noreply@shopify.com>".to_string());
        message.body_full =
            "Website inquiry\nCustomer Email: business-1798e87f34.example.test@Example.COM\n"
                .to_string();
        store::record_inbound_message(conn, CLIENT, &message).expect("record inbound");
        assert!(
            store::list_inbound_enrichments(conn, CLIENT, &message.source_key)
                .expect("initial enrichments")
                .is_empty()
        );
        message.source_key
    };

    let first = super::worker::run_enrichment_backfill_cycle(&state).expect("first backfill");
    assert_eq!(
        first,
        super::worker::EnrichmentBackfillSummary {
            enabled: true,
            examined: 1,
            enriched: 1,
            represented_refreshed: 1,
        }
    );
    {
        let persistence = state.persistence.lock();
        let enrichments =
            store::list_inbound_enrichments(persistence.connection_ref(), CLIENT, &source_key)
                .expect("enrichments");
        assert_eq!(enrichments.len(), 1);
        assert_eq!(enrichments[0].parser_id, TEST_BACKFILL_PARSER_ID);
        let represented: (Option<String>, Option<String>) = persistence
            .connection_ref()
            .query_row(
                "SELECT represented_email, represented_domain \
                 FROM email_inbound_messages WHERE client_id = ?1 AND source_key = ?2",
                rusqlite::params![CLIENT, &source_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("represented columns");
        assert_eq!(
            represented,
            (
                Some("business-1798e87f34.example.test@example.com".to_string()),
                Some("example.com".to_string())
            )
        );
        let represented_receipts = crate::store_core::receipts_for_entity(
            persistence.connection_ref(),
            CLIENT,
            store::INBOUND_ENTITY_KIND,
            &source_key,
            20,
        )
        .expect("receipts")
        .into_iter()
        .filter(|receipt| receipt.change_kind == "represented_identity")
        .count();
        assert_eq!(represented_receipts, 1);
    }

    let second = super::worker::run_enrichment_backfill_cycle(&state).expect("second backfill");
    assert_eq!(
        second,
        super::worker::EnrichmentBackfillSummary {
            enabled: true,
            examined: 1,
            enriched: 1,
            represented_refreshed: 0,
        }
    );
    let persistence = state.persistence.lock();
    let enrichments =
        store::list_inbound_enrichments(persistence.connection_ref(), CLIENT, &source_key)
            .expect("enrichments after replay");
    assert_eq!(enrichments.len(), 1);
    let represented_receipts = crate::store_core::receipts_for_entity(
        persistence.connection_ref(),
        CLIENT,
        store::INBOUND_ENTITY_KIND,
        &source_key,
        20,
    )
    .expect("receipts after replay")
    .into_iter()
    .filter(|receipt| receipt.change_kind == "represented_identity")
    .count();
    assert_eq!(
        represented_receipts, 1,
        "unchanged second pass must not emit represented_identity receipt"
    );
}

#[test]
fn crm_facts_still_match_real_person_at_neutral_domain() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    crate::slices::crm_cache::store::upsert_contact_snapshots(
        conn,
        "test-client",
        &[CrmContactRecord {
            provider_contact_id: "shopify-person".to_string(),
            email: Some("dana@shopify.com".to_string()),
            name: Some("Dana Shopify".to_string()),
            company: Some("Shopify".to_string()),
            phone: None,
            lifecycle_stage: Some("lead".to_string()),
            owner: None,
            last_activity_at: None,
        }],
        1_000,
    )
    .expect("snapshot");

    let message = MessageView {
        from: Some("Dana <dana@shopify.com>".to_string()),
        ..Default::default()
    };
    let (crm, misses) =
        super::service::crm_fact_overrides_from_cache(conn, "test-client", &message, 2_000);

    assert!(misses
        .iter()
        .all(|miss| miss.kind != super::service::CrmFactKind::SenderContact));
    assert_eq!(
        crm.sender_contact_exists.expect("contact fact").value,
        super::facts::TriValue::True
    );
}

#[test]
fn crm_facts_use_configured_neutral_sender_domain() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    crate::slices::admin_settings::store::upsert_override(
        conn,
        crate::slices::admin_settings::store::OverrideWrite {
            client_id: "test-client",
            actor_id: "test",
            var_name: crate::env_registry::BOS_CRM_CONTEXT_NEUTRAL_SENDER_DOMAINS.name,
            value: "business-b22be42d44.example.test",
            expected_revision: None,
            idempotency_key: "set-platform-neutral-domain",
            now_ms: 1_000,
        },
    )
    .expect("setting override");
    crate::slices::crm_cache::store::upsert_contact_snapshots(
        conn,
        "test-client",
        &[CrmContactRecord {
            provider_contact_id: "platform-contact".to_string(),
            email: Some("mailer@business-b22be42d44.example.test".to_string()),
            name: Some("Wrong Platform Contact".to_string()),
            company: Some("Platform".to_string()),
            phone: None,
            lifecycle_stage: Some("lead".to_string()),
            owner: None,
            last_activity_at: None,
        }],
        1_000,
    )
    .expect("snapshot");

    let message = MessageView {
        from: Some("Platform <notifications@updates.business-b22be42d44.example.test>".to_string()),
        ..Default::default()
    };
    let (crm, misses) =
        super::service::crm_fact_overrides_from_cache(conn, "test-client", &message, 2_000);

    assert!(misses.is_empty());
    assert_eq!(
        crm.sender_contact_exists.expect("contact fact").value,
        super::facts::TriValue::Unknown
    );
}

#[test]
fn crm_fact_cache_misses_when_provider_changes() {
    let _provider = EnvGuard::set("BOS_CRM_PROVIDER", "hubspot");
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let write = super::service::CrmFactCacheWrite {
        fact_key: "crm.sender_company.exists:example.com".to_string(),
        value: super::facts::TriValue::True,
        provider: crate::slices::crm_drafts::service::PROVIDER_ESPOCRM.to_string(),
        fetched_at_ms: 1_000,
        expires_at_ms: 3_601_000,
    };
    super::service::persist_crm_fact_cache_writes(conn, "test-client", &[write])
        .expect("cache write");

    let message = MessageView {
        from: Some("Ada <ada@example.com>".to_string()),
        ..Default::default()
    };
    let (crm, misses) =
        super::service::crm_fact_overrides_from_cache(conn, "test-client", &message, 2_000);

    assert!(crm.sender_company_exists.is_none());
    assert!(misses
        .iter()
        .any(|miss| miss.kind == super::service::CrmFactKind::SenderCompany));
}

#[test]
fn crm_fact_budget_exhaustion_is_unknown_without_provider_call() {
    struct FakeLookup {
        calls: usize,
    }
    impl super::service::CrmLiveLookup for FakeLookup {
        fn provider(&self) -> &'static str {
            "fake"
        }
        fn contact_exists(&mut self, _email: &str) -> super::facts::TriValue {
            self.calls += 1;
            super::facts::TriValue::True
        }
        fn company_domain_exists(&mut self, _domain: &str) -> super::facts::TriValue {
            self.calls += 1;
            super::facts::TriValue::True
        }
    }

    let misses = vec![super::service::CrmFactMiss {
        kind: super::service::CrmFactKind::SenderContact,
        subject: "ada@example.com".to_string(),
        fact_key: "crm.sender_contact.exists:ada@example.com".to_string(),
    }];
    let mut budget = 0;
    let mut lookup = FakeLookup { calls: 0 };
    let (crm, writes) = super::service::resolve_crm_fact_misses(
        &misses,
        &mut budget,
        &mut lookup,
        1_000,
        super::service::CrmFactTtls {
            positive_secs: 60,
            negative_secs: 60,
        },
    );

    assert_eq!(lookup.calls, 0);
    assert!(writes.is_empty());
    let fact = crm.sender_contact_exists.expect("unknown contact fact");
    assert_eq!(fact.value, super::facts::TriValue::Unknown);
    assert!(fact.detail.contains("rate-limited"));
}

#[test]
fn crm_provider_error_is_unknown_and_not_cached_as_false() {
    struct ErrorLookup {
        calls: usize,
    }
    impl super::service::CrmLiveLookup for ErrorLookup {
        fn provider(&self) -> &'static str {
            "fake"
        }
        fn contact_exists(&mut self, _email: &str) -> super::facts::TriValue {
            self.calls += 1;
            super::facts::TriValue::Unknown
        }
        fn company_domain_exists(&mut self, _domain: &str) -> super::facts::TriValue {
            self.calls += 1;
            super::facts::TriValue::Unknown
        }
    }

    let misses = vec![super::service::CrmFactMiss {
        kind: super::service::CrmFactKind::SenderContact,
        subject: "ada@example.com".to_string(),
        fact_key: "crm.sender_contact.exists:ada@example.com".to_string(),
    }];
    let mut budget = 1;
    let mut lookup = ErrorLookup { calls: 0 };
    let (crm, writes) = super::service::resolve_crm_fact_misses(
        &misses,
        &mut budget,
        &mut lookup,
        1_000,
        super::service::CrmFactTtls {
            positive_secs: 60,
            negative_secs: 60,
        },
    );

    assert_eq!(lookup.calls, 1);
    assert!(
        writes.is_empty(),
        "Unknown provider result is not cached false"
    );
    assert_eq!(
        crm.sender_contact_exists.expect("contact fact").value,
        super::facts::TriValue::Unknown
    );
}

#[test]
fn reclassify_uses_cache_only_for_crm_facts() {
    use crate::slices::email_triage::service::reclassify_all;

    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    store::list_categories(conn, "test-client", 100).expect("seed");
    crate::slices::email_triage::worker::ingest_messages(
        conn,
        "test-client",
        None,
        &[bos_integrations::gmail_inbox_read::GmailFullMessage {
            message_id: "crm-cache-only".to_string(),
            thread_id: Some("thread-crm-cache-only".to_string()),
            label_ids: vec!["INBOX".to_string()],
            internal_date_epoch_ms: Some(1_000),
            subject: Some("Hello".to_string()),
            from: Some("Ada <ada@example.com>".to_string()),
            to: Some("ops@example.com".to_string()),
            headers: Vec::new(),
            plain_text_body: "body".to_string(),
            html_body: None,
            attachments: Vec::new(),
        }],
        1_000,
    )
    .expect("ingest");
    let rule = EmailTriageRule {
        rule_id: "crm-contact".into(),
        conditions: Vec::new(),
        conditions_v2: vec![EmailTriageConditionV2 {
            condition_id: EmailTriageConditionId::CrmSenderContactExists,
            op: EmailTriageConditionOperator::IsTrue,
            value: EmailTriageConditionValue::Empty,
        }],
        match_mode: EmailTriageMatchMode::All,
        priority: 10,
        enabled: true,
        pinned_category: "operator_note".to_string(),
    };
    store::upsert(conn, ctx("crm_cache_rule", None), &rule).expect("rule");

    reclassify_all(
        conn,
        "test-client",
        "op_test",
        FALLBACK_CATEGORY_ID,
        &crate::overlay::WorkQueueOverlay::default(),
        2_000,
    )
    .expect("reclassify");
    let stored = store::inbound_by_source_keys(
        conn,
        "test-client",
        &["crm-cache-only".to_string()],
        &OperatorScope::All,
    )
    .expect("read")
    .remove(0);
    assert_eq!(stored.resolved_category, FALLBACK_CATEGORY_ID);
    assert_eq!(stored.matched_rule_id, None);

    crate::slices::crm_cache::store::upsert_contact_snapshots(
        conn,
        "test-client",
        &[crm_contact("c-ada", "ada@example.com")],
        3_000,
    )
    .expect("contact snapshot");
    reclassify_all(
        conn,
        "test-client",
        "op_test",
        FALLBACK_CATEGORY_ID,
        &crate::overlay::WorkQueueOverlay::default(),
        4_000,
    )
    .expect("reclassify cached");
    let stored = store::inbound_by_source_keys(
        conn,
        "test-client",
        &["crm-cache-only".to_string()],
        &OperatorScope::All,
    )
    .expect("read")
    .remove(0);
    assert_eq!(stored.resolved_category, "operator_note");
    assert_eq!(stored.matched_rule_id.as_deref(), Some("crm-contact"));
}

#[test]
fn crm_deal_rule_conditions_use_local_snapshots() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    crate::slices::crm_cache::store::upsert_contact_snapshots(
        conn,
        CLIENT,
        &[crm_contact("c1", "dana@example.com")],
        1_000,
    )
    .expect("contact snapshot");
    crate::slices::crm_cache::store::upsert_deal_snapshots(
        conn,
        CLIENT,
        &[
            crm_deal("d1", "dana@example.com", "qualified", "sales"),
            crm_deal("d2", "dana@example.com", "proposal", "expansion"),
        ],
        1_000,
    )
    .expect("deal snapshots");

    let rules = vec![EmailTriageRule {
        rule_id: "proposal-deal".into(),
        priority: 1,
        conditions: Vec::new(),
        conditions_v2: vec![
            EmailTriageConditionV2 {
                condition_id: EmailTriageConditionId::CrmSenderDealExists,
                op: EmailTriageConditionOperator::IsTrue,
                value: EmailTriageConditionValue::Empty,
            },
            EmailTriageConditionV2 {
                condition_id: EmailTriageConditionId::CrmSenderDealStage,
                op: EmailTriageConditionOperator::In,
                value: EmailTriageConditionValue::StringList(vec![
                    "proposal".to_string(),
                    "closedwon".to_string(),
                ]),
            },
            EmailTriageConditionV2 {
                condition_id: EmailTriageConditionId::CrmSenderDealPipeline,
                op: EmailTriageConditionOperator::Equals,
                value: EmailTriageConditionValue::Text("expansion".to_string()),
            },
        ],
        match_mode: EmailTriageMatchMode::All,
        enabled: true,
        pinned_category: "operator_note".to_string(),
    }];
    let message = MessageView {
        from: Some("Dana <dana@example.com>".to_string()),
        ..Default::default()
    };
    let crm = super::service::crm_fact_overrides_from_cache(conn, CLIENT, &message, 2_000).0;
    let mut bag = super::facts::FactBag::new(Some(conn), CLIENT, &message, None, None, crm);

    assert_eq!(
        super::service::resolve_rule_with_fact_bag(&rules, &mut bag)
            .map(|rule| rule.rule_id.as_str()),
        Some("proposal-deal")
    );

    let no_match = MessageView {
        from: Some("No Match <nomatch@example.com>".to_string()),
        ..Default::default()
    };
    let crm = super::service::crm_fact_overrides_from_cache(conn, CLIENT, &no_match, 2_000).0;
    let mut bag = super::facts::FactBag::new(Some(conn), CLIENT, &no_match, None, None, crm);
    assert!(super::service::resolve_rule_with_fact_bag(&rules, &mut bag).is_none());
}

#[test]
fn quick_alias_runtime_expansion_matches_catalog() {
    for id in [
        EmailTriageConditionId::QuickKnownCustomer,
        EmailTriageConditionId::QuickNewSalesLead,
        EmailTriageConditionId::QuickBillingFollowup,
        EmailTriageConditionId::QuickExistingWorkThread,
    ] {
        let catalog_item = super::catalog::condition_catalog()
            .groups
            .into_iter()
            .flat_map(|group| group.items)
            .find(|item| item.condition_id == id)
            .expect("alias in catalog");
        let expansion = catalog_item.expansion.expect("catalog expansion");
        let (runtime_mode, runtime_conditions) = super::service::alias_conditions(id);
        let catalog_conditions: Vec<_> = expansion
            .conditions
            .into_iter()
            .map(|condition| (condition.condition_id, condition.op, condition.value))
            .collect();
        let runtime_conditions: Vec<_> = runtime_conditions
            .into_iter()
            .map(|condition| (condition.condition_id, condition.op, condition.value))
            .collect();

        assert_eq!(runtime_mode, expansion.match_mode);
        assert_eq!(runtime_conditions, catalog_conditions);
    }
}

#[test]
fn business_domain_fact_is_tri_valued() {
    let gmail = MessageView {
        from: Some("Ada <ada@gmail.com>".to_string()),
        ..Default::default()
    };
    let mut bag = super::facts::FactBag::new(None, "", &gmail, None, None, Default::default());
    assert_eq!(
        bag.fact(EmailTriageConditionId::MessageFromDomainIsBusiness),
        super::facts::TriValue::False
    );

    let business = MessageView {
        from: Some("Ada <ada@business-1194228da8.example.test>".to_string()),
        ..Default::default()
    };
    let mut bag = super::facts::FactBag::new(None, "", &business, None, None, Default::default());
    assert_eq!(
        bag.fact(EmailTriageConditionId::MessageFromDomainIsBusiness),
        super::facts::TriValue::True
    );

    let malformed = MessageView {
        from: Some("Ada".to_string()),
        ..Default::default()
    };
    let mut bag = super::facts::FactBag::new(None, "", &malformed, None, None, Default::default());
    assert_eq!(
        bag.fact(EmailTriageConditionId::MessageFromDomainIsBusiness),
        super::facts::TriValue::Unknown
    );
}

#[test]
fn quick_new_sales_lead_uses_business_domain_at_eval_time() {
    let rule = EmailTriageRule {
        rule_id: "quick-lead".into(),
        conditions: Vec::new(),
        conditions_v2: vec![EmailTriageConditionV2 {
            condition_id: EmailTriageConditionId::QuickNewSalesLead,
            op: EmailTriageConditionOperator::IsTrue,
            value: EmailTriageConditionValue::Empty,
        }],
        match_mode: EmailTriageMatchMode::All,
        priority: 10,
        enabled: true,
        pinned_category: "operator_note".to_string(),
    };
    let gmail = MessageView {
        from: Some("Ada <ada@gmail.com>".to_string()),
        body: Some("Can you send a quote?".to_string()),
        ..Default::default()
    };
    let traces = dry_run_traces(
        std::slice::from_ref(&rule),
        FALLBACK_CATEGORY_ID,
        &[gmail],
        Vec::new(),
    );
    assert_eq!(traces[0].rule_traces[0].result, EmailTriageTriValue::False);

    let malformed = MessageView {
        from: Some("Ada".to_string()),
        body: Some("Can you send a quote?".to_string()),
        ..Default::default()
    };
    let traces = dry_run_traces(&[rule], FALLBACK_CATEGORY_ID, &[malformed], Vec::new());
    assert_eq!(
        traces[0].rule_traces[0].result,
        EmailTriageTriValue::Unknown
    );
    assert!(traces[0].needs_fact_refresh);
    assert_eq!(traces[0].matched_rule_id, None);
}

#[tokio::test]
async fn condition_catalog_route_returns_grouped_closed_catalog() {
    let state = test_state_configured(None, &[]);
    {
        let mut persistence = state.persistence.lock();
        crate::slices::operator_users::store::create_user(
            persistence.connection(),
            CLIENT,
            "operator",
            &personal_operator(),
            "bosu_tok_jordan",
            "user_1",
        )
        .expect("operator user");
    }
    let router = build_router(state);

    let response = router
        .oneshot(
            Request::get("/api/email-triage/condition-catalog")
                .header("authorization", "Bearer bosu_tok_jordan")
                .body(Body::empty())
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
    let catalog: EmailTriageConditionCatalogResponse =
        serde_json::from_slice(&bytes).expect("catalog json");
    let ids: Vec<EmailTriageConditionId> = catalog
        .groups
        .iter()
        .flat_map(|group| group.items.iter().map(|item| item.condition_id))
        .collect();
    assert_eq!(ids.len(), 24);
    assert_eq!(ids[0], EmailTriageConditionId::QuickKnownCustomer);
    assert!(ids.contains(&EmailTriageConditionId::MessageFrom));
    assert!(ids.contains(&EmailTriageConditionId::MessageTo));
    assert!(ids.contains(&EmailTriageConditionId::MessageFromDomainIsBusiness));
    assert!(ids.contains(&EmailTriageConditionId::CrmSenderDealExists));
    assert!(ids.contains(&EmailTriageConditionId::CrmSenderDealStage));
    assert!(ids.contains(&EmailTriageConditionId::CrmSenderDealPipeline));
    assert!(ids.contains(&EmailTriageConditionId::QuickExistingWorkThread));
}

#[tokio::test]
async fn dry_run_route_uses_sample_metadata_for_workflow_facts() {
    let state = test_state_configured(None, &[]);
    {
        let mut persistence = state.persistence.lock();
        crate::slices::operator_users::store::create_user(
            persistence.connection(),
            CLIENT,
            "operator",
            &personal_operator(),
            "bosu_tok_jordan",
            "user_1",
        )
        .expect("operator user");
        crate::slices::work_queue::store::insert_item(
            persistence.connection(),
            CLIENT,
            &WorkItem {
                item_id: "wi_1".to_string(),
                source_kind: "email".to_string(),
                source_ref: "m-work".to_string(),
                category_id: "operator_note".to_string(),
                title: "Work".to_string(),
                summary: String::new(),
                packet_kinds: Vec::new(),
                status: WorkItemStatus::Open,
                accept_actor: None,
                ai_suggested: false,
                rationale: String::new(),
                produce_guidance: String::new(),
                source_user_id: None,
                assignee_user_id: None,
                visible_to_user_ids: Vec::new(),
                created_at_ms: 1,
                updated_at_ms: 1,
            },
        )
        .expect("work item");
    }
    let router = build_router(state);
    let request = EmailTriageDryRunRequest {
        proposed_rules: vec![EmailTriageRule {
            rule_id: "existing-work".into(),
            conditions: Vec::new(),
            conditions_v2: vec![EmailTriageConditionV2 {
                condition_id: EmailTriageConditionId::WorkflowThreadHasOpenItem,
                op: EmailTriageConditionOperator::IsTrue,
                value: EmailTriageConditionValue::Empty,
            }],
            match_mode: EmailTriageMatchMode::All,
            priority: 10,
            enabled: true,
            pinned_category: "operator_note".to_string(),
        }],
        fallback_category: None,
        samples: vec![MessageView {
            message_id: Some("m-work".to_string()),
            source_user_id: Some("user_1".to_string()),
            ..Default::default()
        }],
    };

    let response = router
        .oneshot(
            Request::post("/api/email-triage/dry-run")
                .header("authorization", "Bearer bosu_tok_jordan")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&request).expect("request json"),
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
    let body: EmailTriageDryRunResponse = serde_json::from_slice(&bytes).expect("dry-run response");
    assert_eq!(
        body.results[0].matched_rule_id.as_deref(),
        Some("existing-work")
    );
    assert_eq!(
        body.traces[0].fact_traces[0].source,
        bos_contracts::email_triage::EmailTriageFactSource::Workflow
    );
}

#[test]
fn inbox_filters_gmail_categories_labels_and_mailboxes_with_scope() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    {
        let conn = persistence.connection();
        for (id, labels, source_user_id, date) in [
            (
                "m-primary-project",
                vec!["INBOX", "CATEGORY_PERSONAL", "Project A"],
                Some("user_jordan"),
                3_000,
            ),
            (
                "m-primary-inbox-only",
                vec!["INBOX", "Project A"],
                Some("user_jordan"),
                2_500,
            ),
            (
                "m-updates-invoices",
                vec!["INBOX", "CATEGORY_UPDATES", "Invoices"],
                Some("user_jordan"),
                2_000,
            ),
            (
                "m-social-project",
                vec!["INBOX", "CATEGORY_SOCIAL", "Project A"],
                Some("user_casey"),
                1_000,
            ),
        ] {
            let mut message = inbound_message(id, source_user_id);
            message.internal_date_ms = Some(date);
            message.labels = labels.into_iter().map(str::to_string).collect();
            store::record_inbound_message(conn, CLIENT, &message).expect("record inbound");
        }
    }

    let filter = store::InboxFilter {
        categories: vec![
            EmailTriageGmailCategory::Primary,
            EmailTriageGmailCategory::Updates,
        ],
        dashboard_categories: Vec::new(),
        labels: vec!["Project A".to_string()],
        source_user_ids: vec![Some("user_jordan".to_string())],
        ..Default::default()
    };
    let filtered = store::list_recent_inbound(
        persistence.connection_ref(),
        CLIENT,
        10,
        &OperatorScope::All,
        &filter,
    )
    .expect("filtered inbox");
    assert_eq!(filtered.len(), 2);
    assert_eq!(filtered[0].message_id, "m-primary-project");
    assert_eq!(filtered[1].message_id, "m-primary-inbox-only");

    let cross_mailbox = store::InboxFilter {
        categories: vec![EmailTriageGmailCategory::Social],
        dashboard_categories: Vec::new(),
        labels: Vec::new(),
        source_user_ids: vec![Some("user_casey".to_string())],
        ..Default::default()
    };
    let scoped = store::list_recent_inbound(
        persistence.connection_ref(),
        CLIENT,
        10,
        &OperatorScope::User("user_jordan".to_string()),
        &cross_mailbox,
    )
    .expect("scoped inbox");
    assert!(scoped.is_empty());
}

#[test]
fn gmail_trash_request_dismisses_queue_hides_inbox_and_enqueues_effect() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let message = inbound_message("trash-me", Some("user_jordan"));
    let item = WorkItem {
        item_id: format!("wi_email_{}", message.source_key),
        source_kind: "email".to_string(),
        source_ref: message.source_key.clone(),
        category_id: message.resolved_category.clone(),
        title: "Trash me".to_string(),
        summary: String::new(),
        packet_kinds: vec!["follow_up_task".to_string()],
        status: WorkItemStatus::Open,
        accept_actor: None,
        ai_suggested: false,
        rationale: String::new(),
        produce_guidance: String::new(),
        source_user_id: message.source_user_id.clone(),
        assignee_user_id: None,
        visible_to_user_ids: Vec::new(),
        created_at_ms: 1_000,
        updated_at_ms: 1_000,
    };
    {
        let conn = persistence.connection();
        store::record_inbound_message(conn, CLIENT, &message).expect("message");
        crate::slices::work_queue::store::insert_item(conn, CLIENT, &item).expect("item");
        store::request_gmail_trash(
            conn,
            crate::slices::work_queue::store::ItemActionContext {
                client_id: CLIENT,
                actor_id: "user_jordan",
                scope: &OperatorScope::User("user_jordan".to_string()),
                expected_revision: None,
                idempotency_key: "trash-request-1",
                now_ms: 2_000,
            },
            &message,
        )
        .expect("trash request");
    }

    let stored_item = crate::slices::work_queue::store::get_item_unscoped(
        persistence.connection_ref(),
        CLIENT,
        &item.item_id,
    )
    .expect("item read")
    .expect("item exists");
    assert_eq!(stored_item.item.status, WorkItemStatus::Dismissed);
    let inbox = store::list_recent_inbound(
        persistence.connection_ref(),
        CLIENT,
        10,
        &OperatorScope::All,
        &store::InboxFilter::default(),
    )
    .expect("inbox");
    assert!(inbox.is_empty());
    // The external id is intentionally hashed; assert through the stored row.
    let count: i64 = persistence
        .connection_ref()
        .query_row(
            "SELECT COUNT(*) FROM outbox_jobs WHERE client_id = ?1 AND capability = ?2",
            rusqlite::params![CLIENT, store::GMAIL_TRASH_CAPABILITY],
            |row| row.get(0),
        )
        .expect("outbox count");
    assert_eq!(count, 1);

    let updated_message = store::inbound_by_source_keys(
        persistence.connection_ref(),
        CLIENT,
        std::slice::from_ref(&message.source_key),
        &OperatorScope::All,
    )
    .expect("updated message")
    .pop()
    .expect("message still retained");
    assert!(updated_message.labels.iter().any(|label| label == "TRASH"));
    {
        let conn = persistence.connection();
        store::request_gmail_trash(
            conn,
            crate::slices::work_queue::store::ItemActionContext {
                client_id: CLIENT,
                actor_id: "user_jordan",
                scope: &OperatorScope::User("user_jordan".to_string()),
                expected_revision: None,
                idempotency_key: "trash-request-2",
                now_ms: 3_000,
            },
            &updated_message,
        )
        .expect("second trash request");
        let replay = store::request_gmail_trash(
            conn,
            crate::slices::work_queue::store::ItemActionContext {
                client_id: CLIENT,
                actor_id: "user_jordan",
                scope: &OperatorScope::User("user_jordan".to_string()),
                expected_revision: None,
                idempotency_key: "trash-request-2",
                now_ms: 3_100,
            },
            &updated_message,
        )
        .expect("replayed trash request");
        assert!(matches!(replay, MutationOutcome::ReplayedIdempotent { .. }));
    }
    let count: i64 = persistence
        .connection_ref()
        .query_row(
            "SELECT COUNT(*) FROM outbox_jobs WHERE client_id = ?1 AND capability = ?2",
            rusqlite::params![CLIENT, store::GMAIL_TRASH_CAPABILITY],
            |row| row.get(0),
        )
        .expect("outbox count after repeat");
    assert_eq!(count, 2);
}

#[test]
fn inbox_filters_dashboard_categories_with_other_filters() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    {
        let conn = persistence.connection();
        for (id, resolved_category, labels, date) in [
            (
                "m-billing-project",
                "billing",
                vec!["INBOX", "CATEGORY_UPDATES", "Project A"],
                3_000,
            ),
            (
                "m-lead-project",
                "sales_lead",
                vec!["INBOX", "CATEGORY_UPDATES", "Project A"],
                2_000,
            ),
            (
                "m-billing-other-label",
                "billing",
                vec!["INBOX", "CATEGORY_UPDATES", "Other"],
                1_000,
            ),
        ] {
            let mut message = inbound_message(id, Some("user_jordan"));
            message.internal_date_ms = Some(date);
            message.resolved_category = resolved_category.to_string();
            message.labels = labels.into_iter().map(str::to_string).collect();
            store::record_inbound_message(conn, CLIENT, &message).expect("record inbound");
        }
    }

    let filter = store::InboxFilter {
        categories: vec![EmailTriageGmailCategory::Updates],
        dashboard_categories: vec!["billing".to_string()],
        labels: vec!["Project A".to_string()],
        source_user_ids: Vec::new(),
        ..Default::default()
    };
    let filtered = store::list_recent_inbound(
        persistence.connection_ref(),
        CLIENT,
        10,
        &OperatorScope::All,
        &filter,
    )
    .expect("filtered inbox");

    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].message_id, "m-billing-project");
}

#[test]
fn inbox_filters_cached_crm_contact_deals_stage_and_pipeline() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    {
        let conn = persistence.connection();
        for (id, from, date) in [
            ("m-contact", "\"dana@example.com\"", 4_000),
            ("m-deal", "Eli <eli@example.com>", 3_000),
            ("m-other", "Other <other@example.com>", 2_000),
        ] {
            let mut message = inbound_message(id, Some("user_jordan"));
            message.from_addr = Some(from.to_string());
            message.internal_date_ms = Some(date);
            store::record_inbound_message(conn, CLIENT, &message).expect("record inbound");
        }
        crate::slices::crm_cache::store::upsert_contact_snapshots(
            conn,
            CLIENT,
            &[crm_contact("c-dana", "dana@example.com")],
            1_000,
        )
        .expect("contact snapshot");
        crate::slices::crm_cache::store::upsert_deal_snapshots(
            conn,
            CLIENT,
            &[
                crm_deal("d-eli-1", "eli@example.com", "proposal", "sales"),
                crm_deal("d-eli-2", "eli@example.com", "qualified", "expansion"),
            ],
            1_000,
        )
        .expect("deal snapshots");
    }

    let has_contact = store::list_recent_inbound(
        persistence.connection_ref(),
        CLIENT,
        10,
        &OperatorScope::All,
        &store::InboxFilter {
            crm_match: Some(store::InboxCrmMatchFilter::HasContact),
            ..Default::default()
        },
    )
    .expect("has contact");
    assert_eq!(has_contact.len(), 1);
    assert_eq!(has_contact[0].message_id, "m-contact");

    let has_deal = store::list_recent_inbound(
        persistence.connection_ref(),
        CLIENT,
        10,
        &OperatorScope::All,
        &store::InboxFilter {
            crm_match: Some(store::InboxCrmMatchFilter::HasDeal),
            ..Default::default()
        },
    )
    .expect("has deal");
    assert_eq!(has_deal.len(), 1);
    assert_eq!(has_deal[0].message_id, "m-deal");

    let proposal = store::list_recent_inbound(
        persistence.connection_ref(),
        CLIENT,
        10,
        &OperatorScope::All,
        &store::InboxFilter {
            crm_deal_stages: vec!["proposal".to_string()],
            ..Default::default()
        },
    )
    .expect("proposal stage");
    assert_eq!(proposal.len(), 1);
    assert_eq!(proposal[0].message_id, "m-deal");

    let expansion = store::list_recent_inbound(
        persistence.connection_ref(),
        CLIENT,
        10,
        &OperatorScope::All,
        &store::InboxFilter {
            crm_deal_pipelines: vec!["expansion".to_string()],
            ..Default::default()
        },
    )
    .expect("expansion pipeline");
    assert_eq!(expansion.len(), 1);
    assert_eq!(expansion[0].message_id, "m-deal");

    let no_match = store::list_recent_inbound(
        persistence.connection_ref(),
        CLIENT,
        10,
        &OperatorScope::All,
        &store::InboxFilter {
            crm_match: Some(store::InboxCrmMatchFilter::NoMatch),
            ..Default::default()
        },
    )
    .expect("no match");
    assert_eq!(no_match.len(), 1);
    assert_eq!(no_match[0].message_id, "m-other");

    let options = store::inbox_options(persistence.connection_ref(), CLIENT, &OperatorScope::All)
        .expect("options");
    assert!(options
        .crm_deal_stages
        .iter()
        .any(|option| option.value == "proposal" && option.count == 1));
    assert!(options
        .crm_deal_pipelines
        .iter()
        .any(|option| option.value == "expansion" && option.count == 1));
}

#[test]
fn inbox_crm_filters_apply_neutral_sender_policy() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    {
        let conn = persistence.connection();
        let mut platform = inbound_message("m-platform", Some("user_jordan"));
        platform.from_addr = Some("Platform <noreply@shopify.com>".to_string());
        platform.internal_date_ms = Some(4_000);
        store::record_inbound_message(conn, CLIENT, &platform).expect("record platform inbound");

        let mut platform_email = inbound_message("m-platform-email", Some("user_jordan"));
        platform_email.from_addr = Some("Shopify <email@email.shopify.com>".to_string());
        platform_email.internal_date_ms = Some(5_000);
        store::record_inbound_message(conn, CLIENT, &platform_email)
            .expect("record platform email inbound");

        let mut person = inbound_message("m-person", Some("user_jordan"));
        person.from_addr = Some("Dana Shopify <dana@shopify.com>".to_string());
        person.internal_date_ms = Some(3_000);
        store::record_inbound_message(conn, CLIENT, &person).expect("record person inbound");

        crate::slices::crm_cache::store::upsert_contact_snapshots(
            conn,
            CLIENT,
            &[
                crm_contact("c-platform", "noreply@shopify.com"),
                crm_contact("c-platform-email", "email@email.shopify.com"),
                crm_contact("c-dana", "dana@shopify.com"),
            ],
            1_000,
        )
        .expect("contact snapshots");
        crate::slices::crm_cache::store::upsert_deal_snapshots(
            conn,
            CLIENT,
            &[
                crm_deal(
                    "d-platform",
                    "noreply@shopify.com",
                    "lead299164005",
                    "sales",
                ),
                crm_deal(
                    "d-platform-email",
                    "email@email.shopify.com",
                    "lead299164005",
                    "sales",
                ),
                crm_deal("d-dana", "dana@shopify.com", "proposal", "sales"),
            ],
            1_000,
        )
        .expect("deal snapshots");
    }

    let has_contact = store::list_recent_inbound(
        persistence.connection_ref(),
        CLIENT,
        10,
        &OperatorScope::All,
        &store::InboxFilter {
            crm_match: Some(store::InboxCrmMatchFilter::HasContact),
            ..Default::default()
        },
    )
    .expect("has contact");
    assert_eq!(has_contact.len(), 1);
    assert_eq!(has_contact[0].message_id, "m-person");

    let has_deal = store::list_recent_inbound(
        persistence.connection_ref(),
        CLIENT,
        10,
        &OperatorScope::All,
        &store::InboxFilter {
            crm_match: Some(store::InboxCrmMatchFilter::HasDeal),
            ..Default::default()
        },
    )
    .expect("has deal");
    assert_eq!(has_deal.len(), 1);
    assert_eq!(has_deal[0].message_id, "m-person");

    let platform_stage = store::list_recent_inbound(
        persistence.connection_ref(),
        CLIENT,
        10,
        &OperatorScope::All,
        &store::InboxFilter {
            crm_deal_stages: vec!["lead299164005".to_string()],
            ..Default::default()
        },
    )
    .expect("platform stage");
    assert!(platform_stage.is_empty());

    let no_match = store::list_recent_inbound(
        persistence.connection_ref(),
        CLIENT,
        10,
        &OperatorScope::All,
        &store::InboxFilter {
            crm_match: Some(store::InboxCrmMatchFilter::NoMatch),
            ..Default::default()
        },
    )
    .expect("no match");
    assert_eq!(no_match.len(), 2);
    let no_match_ids = no_match
        .iter()
        .map(|message| message.message_id.as_str())
        .collect::<Vec<_>>();
    assert!(no_match_ids.contains(&"m-platform"));
    assert!(no_match_ids.contains(&"m-platform-email"));

    let options = store::inbox_options(persistence.connection_ref(), CLIENT, &OperatorScope::All)
        .expect("options");
    assert!(options
        .crm_deal_stages
        .iter()
        .all(|option| option.value != "lead299164005"));
}

#[test]
fn inbox_crm_filters_prefer_represented_party_over_automation_sender() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    {
        let conn = persistence.connection();
        let mut represented = inbound_message("m-represented", Some("user_jordan"));
        represented.from_addr = Some("Platform <noreply@shopify.com>".to_string());
        represented.internal_date_ms = Some(4_000);
        store::record_inbound_message(conn, CLIENT, &represented)
            .expect("record represented inbound");
        let parsed = parsed_represented_party("alex@example.com");
        store::upsert_inbound_enrichment(
            conn,
            store::InboundEnrichmentWrite {
                client_id: CLIENT,
                source_key: &represented.source_key,
                parser_id: "test_parser",
                parser_version: "1",
                parsed: &parsed,
                now_ms: 1_100,
            },
        )
        .expect("represented enrichment");
        store::refresh_represented_identity(conn, CLIENT, &represented.source_key, 1_200)
            .expect("refresh represented");

        let mut automation_only = inbound_message("m-automation-only", Some("user_jordan"));
        automation_only.from_addr = Some("Platform <noreply@shopify.com>".to_string());
        automation_only.internal_date_ms = Some(3_000);
        store::record_inbound_message(conn, CLIENT, &automation_only)
            .expect("record automation inbound");

        crate::slices::crm_cache::store::upsert_contact_snapshots(
            conn,
            CLIENT,
            &[
                crm_contact("c-alex", "alex@example.com"),
                crm_contact("c-platform", "noreply@shopify.com"),
            ],
            1_000,
        )
        .expect("contact snapshots");
        crate::slices::crm_cache::store::upsert_deal_snapshots(
            conn,
            CLIENT,
            &[
                crm_deal("d-alex", "alex@example.com", "proposal", "sales"),
                crm_deal(
                    "d-platform",
                    "noreply@shopify.com",
                    "lead299164005",
                    "sales",
                ),
            ],
            1_000,
        )
        .expect("deal snapshots");
    }

    let has_contact = store::list_recent_inbound(
        persistence.connection_ref(),
        CLIENT,
        10,
        &OperatorScope::All,
        &store::InboxFilter {
            crm_match: Some(store::InboxCrmMatchFilter::HasContact),
            ..Default::default()
        },
    )
    .expect("has contact");
    assert_eq!(has_contact.len(), 1);
    assert_eq!(has_contact[0].message_id, "m-represented");

    let has_deal = store::list_recent_inbound(
        persistence.connection_ref(),
        CLIENT,
        10,
        &OperatorScope::All,
        &store::InboxFilter {
            crm_match: Some(store::InboxCrmMatchFilter::HasDeal),
            ..Default::default()
        },
    )
    .expect("has deal");
    assert_eq!(has_deal.len(), 1);
    assert_eq!(has_deal[0].message_id, "m-represented");

    let proposal = store::list_recent_inbound(
        persistence.connection_ref(),
        CLIENT,
        10,
        &OperatorScope::All,
        &store::InboxFilter {
            crm_deal_stages: vec!["proposal".to_string()],
            ..Default::default()
        },
    )
    .expect("proposal stage");
    assert_eq!(proposal.len(), 1);
    assert_eq!(proposal[0].message_id, "m-represented");

    let blocked_platform_stage = store::list_recent_inbound(
        persistence.connection_ref(),
        CLIENT,
        10,
        &OperatorScope::All,
        &store::InboxFilter {
            crm_deal_stages: vec!["lead299164005".to_string()],
            ..Default::default()
        },
    )
    .expect("blocked platform stage");
    assert!(blocked_platform_stage.is_empty());

    let no_match = store::list_recent_inbound(
        persistence.connection_ref(),
        CLIENT,
        10,
        &OperatorScope::All,
        &store::InboxFilter {
            crm_match: Some(store::InboxCrmMatchFilter::NoMatch),
            ..Default::default()
        },
    )
    .expect("no match");
    assert_eq!(no_match.len(), 1);
    assert_eq!(no_match[0].message_id, "m-automation-only");

    let options = store::inbox_options(persistence.connection_ref(), CLIENT, &OperatorScope::All)
        .expect("options");
    assert!(options
        .crm_deal_stages
        .iter()
        .any(|option| option.value == "proposal" && option.count == 1));
    assert!(options
        .crm_deal_stages
        .iter()
        .all(|option| option.value != "lead299164005"));
}

#[test]
fn inbox_crm_filters_reapply_neutral_policy_after_ingest() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    {
        let conn = persistence.connection();
        let mut platform = inbound_message("m-platform", Some("user_jordan"));
        platform.from_addr = Some("Platform <noreply@shopify.com>".to_string());
        store::record_inbound_message(conn, CLIENT, &platform).expect("record platform inbound");

        crate::slices::crm_cache::store::upsert_contact_snapshots(
            conn,
            CLIENT,
            &[crm_contact("c-platform", "noreply@shopify.com")],
            1_000,
        )
        .expect("contact snapshots");
    }

    let hidden_by_default = store::list_recent_inbound(
        persistence.connection_ref(),
        CLIENT,
        10,
        &OperatorScope::All,
        &store::InboxFilter {
            crm_match: Some(store::InboxCrmMatchFilter::HasContact),
            ..Default::default()
        },
    )
    .expect("has contact under default policy");
    assert!(hidden_by_default.is_empty());

    crate::slices::admin_settings::store::upsert_override(
        persistence.connection(),
        crate::slices::admin_settings::store::OverrideWrite {
            client_id: CLIENT,
            actor_id: "test",
            var_name: crate::env_registry::BOS_CRM_CONTEXT_NEUTRAL_SENDER_DOMAINS.name,
            value: "stripe.com",
            expected_revision: None,
            idempotency_key: "change-neutral-domains-after-ingest",
            now_ms: 2_000,
        },
    )
    .expect("override neutral sender domains");

    let visible_after_policy_change = store::list_recent_inbound(
        persistence.connection_ref(),
        CLIENT,
        10,
        &OperatorScope::All,
        &store::InboxFilter {
            crm_match: Some(store::InboxCrmMatchFilter::HasContact),
            ..Default::default()
        },
    )
    .expect("has contact after policy change");
    assert_eq!(visible_after_policy_change.len(), 1);
    assert_eq!(visible_after_policy_change[0].message_id, "m-platform");
}

#[test]
fn inbox_search_filters_across_message_fields_and_terms() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    {
        let conn = persistence.connection();
        for (id, subject, body, labels, date) in [
            (
                "m-rush-invoice",
                "Rush invoice for CNC work",
                "Please approve the deposit today.",
                vec!["INBOX", "Project A"],
                3_000,
            ),
            (
                "m-rush-intro",
                "Rush intro",
                "Following up on the website lead.",
                vec!["INBOX", "Leads"],
                2_000,
            ),
            (
                "m-ordinary-invoice",
                "Invoice copy",
                "Standard monthly billing.",
                vec!["INBOX", "Project A"],
                1_000,
            ),
        ] {
            let mut message = inbound_message(id, Some("user_jordan"));
            message.internal_date_ms = Some(date);
            message.subject = Some(subject.to_string());
            message.body_excerpt = body.to_string();
            message.body_full = body.to_string();
            message.labels = labels.into_iter().map(str::to_string).collect();
            store::record_inbound_message(conn, CLIENT, &message).expect("record inbound");
        }
    }

    let filtered = store::list_recent_inbound(
        persistence.connection_ref(),
        CLIENT,
        10,
        &OperatorScope::All,
        &store::InboxFilter {
            search: Some("rush invoice".to_string()),
            ..Default::default()
        },
    )
    .expect("filtered inbox");
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].message_id, "m-rush-invoice");

    let label_match = store::list_recent_inbound(
        persistence.connection_ref(),
        CLIENT,
        10,
        &OperatorScope::All,
        &store::InboxFilter {
            search: Some("\"Project A\"".to_string()),
            ..Default::default()
        },
    )
    .expect("filtered inbox");
    assert_eq!(label_match.len(), 2);
    assert_eq!(label_match[0].message_id, "m-rush-invoice");
    assert_eq!(label_match[1].message_id, "m-ordinary-invoice");
}

#[test]
fn inbox_options_are_derived_from_scoped_ingested_mail() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    {
        let conn = persistence.connection();
        crate::slices::operator_users::store::create_user(
            conn,
            CLIENT,
            "operator",
            &personal_operator(),
            "bosu_tok_jordan",
            "user_1",
        )
        .expect("operator user");
        for (id, labels, source_user_id) in [
            (
                "m-jordan",
                vec!["INBOX", "CATEGORY_UPDATES", "Invoices"],
                Some("user_jordan"),
            ),
            (
                "m-jordan-primary",
                vec!["INBOX", "General"],
                Some("user_jordan"),
            ),
            (
                "m-casey",
                vec!["INBOX", "CATEGORY_SOCIAL", "Project A"],
                Some("user_casey"),
            ),
        ] {
            let mut message = inbound_message(id, source_user_id);
            message.labels = labels.into_iter().map(str::to_string).collect();
            if id == "m-jordan" {
                message.resolved_category = "billing".to_string();
            }
            if id == "m-casey" {
                message.resolved_category = "sales_lead".to_string();
            }
            store::record_inbound_message(conn, CLIENT, &message).expect("record inbound");
        }
    }

    let options = store::inbox_options(
        persistence.connection_ref(),
        CLIENT,
        &OperatorScope::User("user_jordan".to_string()),
    )
    .expect("options");
    assert_eq!(options.mailboxes.len(), 1);
    assert_eq!(
        options.mailboxes[0].source_user_id.as_deref(),
        Some("user_jordan")
    );
    assert!(options.labels.iter().any(|label| label.label == "Invoices"));
    assert!(!options
        .labels
        .iter()
        .any(|label| label.label == "Project A"));
    let updates = options
        .categories
        .iter()
        .find(|category| category.category == EmailTriageGmailCategory::Updates)
        .expect("updates");
    assert_eq!(updates.count, 1);
    let primary = options
        .categories
        .iter()
        .find(|category| category.category == EmailTriageGmailCategory::Primary)
        .expect("primary");
    assert_eq!(primary.count, 1);
    let social = options
        .categories
        .iter()
        .find(|category| category.category == EmailTriageGmailCategory::Social)
        .expect("social");
    assert_eq!(social.count, 0);
    assert!(options
        .dashboard_categories
        .iter()
        .any(|category| category.category_id == "billing" && category.count == 1));
    assert!(!options
        .dashboard_categories
        .iter()
        .any(|category| category.category_id == "sales_lead"));
}

#[test]
fn inbox_options_collapse_shared_and_operator_source_accounts() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    {
        let conn = persistence.connection();
        for (id, source_user_id) in [
            ("m-legacy", None),
            ("m-operator", Some("operator")),
            ("m-jordan", Some("user_jordan")),
        ] {
            store::record_inbound_message(conn, CLIENT, &inbound_message(id, source_user_id))
                .expect("record inbound");
        }
    }

    let options = store::inbox_options(persistence.connection_ref(), CLIENT, &OperatorScope::All)
        .expect("options");

    assert_eq!(options.mailboxes.len(), 2);
    let operator = options
        .mailboxes
        .iter()
        .find(|mailbox| mailbox.source_user_id.as_deref() == Some("operator"))
        .expect("operator mailbox");
    assert_eq!(operator.display_name, "operator");
    assert_eq!(operator.count, 2);
    assert!(!options
        .mailboxes
        .iter()
        .any(|mailbox| mailbox.source_user_id.is_none()));
}

#[test]
fn inbox_settings_replace_is_receipted_revision_checked_and_ordered() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    assert!(store::get_inbox_settings(conn, CLIENT)
        .expect("default settings")
        .is_none());
    assert_eq!(
        store::default_visible_gmail_categories(),
        vec![
            EmailTriageGmailCategory::Primary,
            EmailTriageGmailCategory::Updates,
            EmailTriageGmailCategory::Social,
            EmailTriageGmailCategory::Promotions,
            EmailTriageGmailCategory::Forums,
        ]
    );

    let request = EmailTriageInboxSettingsUpdateRequest {
        expected_revision: None,
        idempotency_key: "inbox-settings-1".to_string(),
        actor_id: None,
        visible_gmail_categories: vec![
            EmailTriageGmailCategory::Forums,
            EmailTriageGmailCategory::Primary,
            EmailTriageGmailCategory::Primary,
        ],
    };
    let outcome = store::replace_inbox_settings(conn, CLIENT, "operator", &request, 10_000)
        .expect("replace settings");
    let revision = match outcome {
        MutationOutcome::Applied { revision, .. } => revision,
        other => panic!("unexpected outcome: {other:?}"),
    };
    assert_eq!(revision, 1);

    let stored = store::get_inbox_settings(conn, CLIENT)
        .expect("load settings")
        .expect("settings row");
    assert_eq!(
        stored.visible_gmail_categories,
        vec![
            EmailTriageGmailCategory::Primary,
            EmailTriageGmailCategory::Forums,
        ]
    );
    assert_eq!(stored.revision, Some(1));

    let mut stale = request.clone();
    stale.expected_revision = Some(0);
    stale.idempotency_key = "inbox-settings-stale".to_string();
    let conflict = store::replace_inbox_settings(conn, CLIENT, "operator", &stale, 10_100)
        .expect("conflict outcome");
    assert!(matches!(
        conflict,
        MutationOutcome::RevisionConflict {
            current_revision: Some(1),
            ..
        }
    ));

    let mut empty = request;
    empty.expected_revision = Some(1);
    empty.idempotency_key = "inbox-settings-empty".to_string();
    empty.visible_gmail_categories = Vec::new();
    let err = store::replace_inbox_settings(conn, CLIENT, "operator", &empty, 10_200)
        .expect_err("empty visible category list is refused");
    assert!(matches!(
        err,
        crate::store_core::StoreError::Domain(code)
            if code == "email_triage_visible_gmail_categories_empty"
    ));
}

#[test]
fn inbox_operator_source_filter_includes_legacy_rows_for_shared_scope() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    {
        let conn = persistence.connection();
        for (id, source_user_id, date) in [
            ("m-legacy", None, 3_000),
            ("m-operator", Some("operator"), 2_000),
            ("m-jordan", Some("user_jordan"), 1_000),
        ] {
            let mut message = inbound_message(id, source_user_id);
            message.internal_date_ms = Some(date);
            store::record_inbound_message(conn, CLIENT, &message).expect("record inbound");
        }
    }

    let filtered = store::list_recent_inbound(
        persistence.connection_ref(),
        CLIENT,
        10,
        &OperatorScope::All,
        &store::InboxFilter {
            categories: Vec::new(),
            dashboard_categories: Vec::new(),
            labels: Vec::new(),
            source_user_ids: vec![Some("operator".to_string())],
            ..Default::default()
        },
    )
    .expect("filtered inbox");

    assert_eq!(filtered.len(), 2);
    assert_eq!(filtered[0].message_id, "m-legacy");
    assert_eq!(filtered[1].message_id, "m-operator");
}

#[tokio::test]
async fn inbox_options_route_returns_overlay_defaults_for_actor() {
    let mut state = test_state_configured(None, &[]);
    state.email_triage_overlay = std::sync::Arc::new(crate::overlay::EmailTriageOverlay {
        inbound_parser_ids: Vec::new(),
        inbox_defaults: vec![crate::overlay::EmailTriageInboxDefaultOverlay {
            user_id: "user_jordan".to_string(),
            categories: vec![
                EmailTriageGmailCategory::Primary,
                EmailTriageGmailCategory::Updates,
            ],
            label: None,
            source_user_id: None,
            limit: Some(100),
        }],
    });
    {
        let mut persistence = state.persistence.lock();
        crate::slices::operator_users::store::create_user(
            persistence.connection(),
            CLIENT,
            "operator",
            &personal_operator(),
            "bosu_tok_jordan",
            "user_1",
        )
        .expect("operator user");
    }
    let router = build_router(state);

    let response = router
        .oneshot(
            Request::get("/api/email-triage/inbox/options")
                .header("authorization", "Bearer bosu_tok_jordan")
                .body(Body::empty())
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
    let body: EmailTriageInboxOptionsResponse =
        serde_json::from_slice(&bytes).expect("options response");
    assert_eq!(
        body.visible_gmail_categories,
        vec![
            EmailTriageGmailCategory::Primary,
            EmailTriageGmailCategory::Updates,
            EmailTriageGmailCategory::Social,
            EmailTriageGmailCategory::Promotions,
            EmailTriageGmailCategory::Forums,
        ]
    );
    assert_eq!(
        body.defaults.categories,
        vec![
            EmailTriageGmailCategory::Primary,
            EmailTriageGmailCategory::Updates
        ]
    );
    assert_eq!(body.defaults.limit, 100);
}

#[tokio::test]
async fn inbox_settings_route_returns_default_and_saved_visibility() {
    let state = test_state_configured(Some("test-token"), &[]);
    let router = build_router(state);

    let response = router
        .clone()
        .oneshot(
            Request::get("/api/email-triage/inbox/settings")
                .header("authorization", "Bearer test-token")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let body: EmailTriageInboxSettingsResponse = serde_json::from_slice(
        &response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes(),
    )
    .expect("settings response");
    assert_eq!(body.revision, None);
    assert_eq!(body.visible_gmail_categories.len(), 5);

    let request = EmailTriageInboxSettingsUpdateRequest {
        expected_revision: body.revision,
        idempotency_key: "settings-route".to_string(),
        actor_id: None,
        visible_gmail_categories: vec![EmailTriageGmailCategory::Primary],
    };
    let response = router
        .clone()
        .oneshot(
            Request::post("/api/email-triage/inbox/settings")
                .header("authorization", "Bearer test-token")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&request).expect("json")))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);

    let response = router
        .oneshot(
            Request::get("/api/email-triage/inbox/settings")
                .header("authorization", "Bearer test-token")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let body: EmailTriageInboxSettingsResponse = serde_json::from_slice(
        &response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes(),
    )
    .expect("settings response");
    assert_eq!(body.revision, Some(1));
    assert_eq!(
        body.visible_gmail_categories,
        vec![EmailTriageGmailCategory::Primary]
    );
}

#[tokio::test]
async fn inbox_route_accepts_legacy_source_user_filter() {
    let state = test_state_configured(None, &[]);
    {
        let mut persistence = state.persistence.lock();
        store::record_inbound_message(
            persistence.connection(),
            CLIENT,
            &inbound_message("m-legacy", None),
        )
        .expect("record legacy message");
        store::record_inbound_message(
            persistence.connection(),
            CLIENT,
            &inbound_message("m-user", Some("user_jordan")),
        )
        .expect("record user message");
    }
    let router = build_router(state);

    let response = router
        .oneshot(
            Request::get("/api/email-triage/inbox?source_user_id=__legacy")
                .body(Body::empty())
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
    let body: EmailTriageInboxResponse = serde_json::from_slice(&bytes).expect("inbox response");
    assert_eq!(body.messages.len(), 1);
    assert_eq!(body.messages[0].message_id, "m-legacy");
}

#[tokio::test]
async fn rule_upsert_with_personal_token_does_not_deadlock() {
    let state = test_state_configured(None, &[]);
    {
        let mut persistence = state.persistence.lock();
        crate::slices::operator_users::store::create_user(
            persistence.connection(),
            CLIENT,
            "operator",
            &personal_operator(),
            "bosu_tok_jordan",
            "user_1",
        )
        .expect("operator user");
    }
    let router = build_router(state.clone());

    let response = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        router.oneshot(
            Request::post("/api/email-triage/rules")
                .header("authorization", "Bearer bosu_tok_jordan")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "actor_id": "spoofed",
                        "rule": call_log_rule(),
                        "expected_revision": null,
                        "idempotency_key": "rule_1"
                    })
                    .to_string(),
                ))
                .expect("request"),
        ),
    )
    .await
    .expect("route timed out")
    .expect("response");
    assert_eq!(response.status(), 200);

    let persistence = state.persistence.lock();
    let receipts = crate::store_core::receipts_for_entity(
        persistence.connection_ref(),
        CLIENT,
        store::ENTITY_KIND,
        "call-log",
        10,
    )
    .expect("receipts");
    assert_eq!(receipts[0].actor_id, "user_jordan");
}

#[tokio::test]
async fn inbox_follow_up_hides_other_users_message_id() {
    let state = test_state_configured(None, &[]);
    {
        let mut persistence = state.persistence.lock();
        crate::slices::operator_users::store::create_user(
            persistence.connection(),
            CLIENT,
            "operator",
            &personal_operator(),
            "bosu_tok_jordan",
            "user_1",
        )
        .expect("operator user");
        store::record_inbound_message(
            persistence.connection(),
            CLIENT,
            &inbound_message("m-casey", Some("user_casey")),
        )
        .expect("record message");
    }
    let router = build_router(state);

    let casey_source_key = store::source_key_for(Some("user_casey"), "m-casey");
    let response = router
        .oneshot(
            Request::post(format!(
                "/api/email-triage/inbox/{}/follow-up",
                casey_source_key
            ))
            .header("authorization", "Bearer bosu_tok_jordan")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "actor_id": "user_jordan",
                    "expected_revision": null,
                    "idempotency_key": "follow_up_hidden"
                })
                .to_string(),
            ))
            .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        response_error(response).await,
        "email_inbound_message_not_found"
    );
}

#[tokio::test]
async fn maintenance_routes_require_all_scope() {
    let state = test_state_configured(None, &[]);
    {
        let mut persistence = state.persistence.lock();
        crate::slices::operator_users::store::create_user(
            persistence.connection(),
            CLIENT,
            "operator",
            &personal_operator(),
            "bosu_tok_jordan",
            "user_1",
        )
        .expect("operator user");
    }
    let router = build_router(state);

    let named_reclassify = router
        .clone()
        .oneshot(
            Request::post("/api/email-triage/reclassify")
                .header("authorization", "Bearer bosu_tok_jordan")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(named_reclassify.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(response_error(named_reclassify).await, "scope_forbidden");

    let named_reset = router
        .clone()
        .oneshot(
            Request::post("/api/email-triage/ai-retriage-reset")
                .header("authorization", "Bearer bosu_tok_jordan")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "actor_id": "user_jordan",
                        "scope": "all",
                        "idempotency_key": "reset_named"
                    })
                    .to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(named_reset.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(response_error(named_reset).await, "scope_forbidden");

    let all_reclassify = router
        .clone()
        .oneshot(
            Request::post("/api/email-triage/reclassify")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(all_reclassify.status(), StatusCode::OK);

    let all_reset = router
        .oneshot(
            Request::post("/api/email-triage/ai-retriage-reset")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "actor_id": "operator",
                        "scope": "all",
                        "idempotency_key": "reset_all"
                    })
                    .to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(all_reset.status(), StatusCode::OK);
}

#[tokio::test]
async fn ai_retriage_message_scope_uses_source_key() {
    let state = test_state_configured(None, &[]);
    let source_key = store::source_key_for(Some("user_jordan"), "same-provider-id");
    {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        let mut message = inbound_message("same-provider-id", Some("user_jordan"));
        message.source_key = source_key.clone();
        store::record_inbound_message(conn, CLIENT, &message).expect("inbound message");
        store::set_ai_triage_result(
            conn,
            CLIENT,
            &source_key,
            "no_suggestion",
            None,
            None,
            2_000,
        )
        .expect("verdict");
    }
    let router = build_router(state.clone());
    let response = router
        .oneshot(
            Request::post("/api/email-triage/ai-retriage-reset")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "actor_id": "operator",
                        "scope": "message",
                        "source_key": source_key,
                        "idempotency_key": "reset_source_key"
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
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json body");
    assert_eq!(body["reset"], serde_json::json!(1));

    let persistence = state.persistence.lock();
    let status: Option<String> = persistence
        .connection_ref()
        .query_row(
            "SELECT ai_triage_status FROM email_inbound_messages \
             WHERE client_id = ?1 AND source_key = ?2",
            rusqlite::params![CLIENT, source_key],
            |row| row.get(0),
        )
        .expect("status");
    assert_eq!(status, None);
}

mod display_body_trim {
    use super::super::service::display_body_for_excerpt;

    #[test]
    fn collapses_gmail_forward_preamble_and_preserves_originator() {
        let body = "\
FYI

---------- Forwarded message ---------
From: Form Submit <info@business-014bb695de.example.test>
Date: Fri, Jun 12, 2026 at 5:36 AM
Subject: New Wholesale Account Application
To: example Info <ask@business-914f630770.example.test>

Hi jordan,
Business Name: Taylor Repair Service
Primary Contact: Davey Jones";

        let trimmed = display_body_for_excerpt(body);

        assert!(trimmed.contains("FYI"));
        assert!(trimmed.contains("From: Form Submit <info@business-014bb695de.example.test>"));
        assert!(trimmed.contains("Subject: New Wholesale Account Application"));
        assert!(trimmed.contains("Business Name: Taylor Repair Service"));
        assert!(!trimmed.contains("Date: Fri"));
        assert!(!trimmed.contains("To: example Info"));
        assert!(!trimmed.contains("---------- Forwarded message"));
    }

    #[test]
    fn removes_trailing_reply_chain() {
        let body = "\
Can you send a quote for this full project and confirm the account setup path?

On Thu, Jun 11, 2026 at 9:00 AM Alex <alex@example.com> wrote:
> prior message";

        assert_eq!(
            display_body_for_excerpt(body),
            "Can you send a quote for this full project and confirm the account setup path?"
        );
    }

    #[test]
    fn no_marker_returns_body_unchanged() {
        let body = "Plain message with no forwarded or quoted boundary.";
        assert_eq!(display_body_for_excerpt(body), body);
    }

    #[test]
    fn empty_trim_falls_back_to_raw_head() {
        let body = "On Thu, Jun 11, 2026 at 9:00 AM Alex <alex@example.com> wrote:\n> quoted";
        assert_eq!(display_body_for_excerpt(body), body);
    }
}

mod ingest {
    use super::*;
    use crate::slices::email_triage::worker::{ingest_messages, ingest_messages_with_overlay};
    use bos_integrations::gmail_inbox_read::{GmailAttachmentMeta, GmailFullMessage};

    fn gmail_message(id: &str, subject: &str, body: &str) -> GmailFullMessage {
        GmailFullMessage {
            message_id: id.to_string(),
            thread_id: Some(format!("thr-{id}")),
            label_ids: vec!["INBOX".to_string()],
            internal_date_epoch_ms: Some(1_000_000),
            subject: Some(subject.to_string()),
            from: Some("customer@example.com".to_string()),
            to: Some("ops@business-a91b8b0f88.example.test".to_string()),
            headers: Vec::new(),
            plain_text_body: body.to_string(),
            html_body: None,
            attachments: Vec::new(),
        }
    }

    #[test]
    fn shared_inbox_overlay_makes_ingested_work_item_visible_to_configured_users() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        let policy = bos_contracts::work_queue::WorkQueuePolicy {
            category_id: FALLBACK_CATEGORY_ID.to_string(),
            create_work_item: true,
            packet_kinds: vec!["follow_up_task".to_string()],
            ai_suggestible_packet_kinds: Vec::new(),
            ai_suggestible_gmail_scope: Default::default(),
            ai_suggestible_gmail_categories: Vec::new(),
            auto_produce: false,
        };
        crate::slices::work_queue::store::upsert_policy(
            conn,
            "test-client",
            "op_test",
            &policy,
            "policy",
            1_500,
        )
        .expect("policy");
        let mut shared = std::collections::BTreeMap::new();
        shared.insert(
            "ask".to_string(),
            crate::overlay::SharedInboxOverlay {
                match_to: vec!["ask@business-914f630770.example.test".to_string()],
                visible_to_user_ids: vec!["user_jordan".to_string(), "user_casey".to_string()],
            },
        );
        let overlay = crate::overlay::WorkQueueOverlay {
            shared_inboxes: shared,
        };
        let mut message = gmail_message("shared", "Question", "Can you help?");
        message.to =
            Some("example Info <ask@business-914f630770.example.test>, Orders <orders@business-914f630770.example.test>".into());

        let summary = ingest_messages_with_overlay(
            conn,
            "test-client",
            Some("source_mailbox"),
            &[message],
            &crate::overlay::EmailTriageOverlay::default(),
            &overlay,
            2_000,
        )
        .expect("ingest");
        assert_eq!(summary.ingested, 1);

        let jordan = crate::slices::work_queue::store::list_items(
            persistence.connection_ref(),
            "test-client",
            None,
            10,
            &OperatorScope::User("user_jordan".to_string()),
        )
        .expect("jordan list");
        assert_eq!(jordan.len(), 1);
        assert_eq!(
            jordan[0].item.visible_to_user_ids,
            vec!["user_casey".to_string(), "user_jordan".to_string()]
        );
        let third = crate::slices::work_queue::store::list_items(
            persistence.connection_ref(),
            "test-client",
            None,
            10,
            &OperatorScope::User("third".to_string()),
        )
        .expect("third list");
        assert!(third.is_empty());
    }

    #[test]
    fn shared_inbox_overlay_repairs_existing_work_item_visibility_on_replay() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let policy = bos_contracts::work_queue::WorkQueuePolicy {
            category_id: FALLBACK_CATEGORY_ID.to_string(),
            create_work_item: true,
            packet_kinds: vec!["follow_up_task".to_string()],
            ai_suggestible_packet_kinds: Vec::new(),
            ai_suggestible_gmail_scope: Default::default(),
            ai_suggestible_gmail_categories: Vec::new(),
            auto_produce: false,
        };
        crate::slices::work_queue::store::upsert_policy(
            persistence.connection(),
            "test-client",
            "op_test",
            &policy,
            "policy",
            1_500,
        )
        .expect("policy");
        let mut message = gmail_message("shared-existing", "Question", "Can you help?");
        message.to = Some("example Info <ask@business-914f630770.example.test>".into());

        let first = ingest_messages(
            persistence.connection(),
            "test-client",
            Some("source_mailbox"),
            &[message.clone()],
            2_000,
        )
        .expect("initial ingest");
        assert_eq!(first.ingested, 1);
        assert!(crate::slices::work_queue::store::list_items(
            persistence.connection_ref(),
            "test-client",
            None,
            10,
            &OperatorScope::User("user_jordan".to_string()),
        )
        .expect("jordan before overlay")
        .is_empty());

        let mut shared = std::collections::BTreeMap::new();
        shared.insert(
            "ask".to_string(),
            crate::overlay::SharedInboxOverlay {
                match_to: vec!["ask@business-914f630770.example.test".to_string()],
                visible_to_user_ids: vec!["user_jordan".to_string(), "user_casey".to_string()],
            },
        );
        let overlay = crate::overlay::WorkQueueOverlay {
            shared_inboxes: shared,
        };
        let replay = ingest_messages_with_overlay(
            persistence.connection(),
            "test-client",
            Some("source_mailbox"),
            &[message],
            &crate::overlay::EmailTriageOverlay::default(),
            &overlay,
            3_000,
        )
        .expect("replay with overlay");
        assert_eq!(replay.skipped_existing, 1);

        let jordan = crate::slices::work_queue::store::list_items(
            persistence.connection_ref(),
            "test-client",
            None,
            10,
            &OperatorScope::User("user_jordan".to_string()),
        )
        .expect("jordan after overlay");
        assert_eq!(jordan.len(), 1);
        assert_eq!(
            jordan[0].item.visible_to_user_ids,
            vec![
                "source_mailbox".to_string(),
                "user_casey".to_string(),
                "user_jordan".to_string()
            ]
        );
    }

    #[test]
    fn existing_inbound_message_backfills_attachment_metadata() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        ingest_messages(
            conn,
            "test-client",
            None,
            &[gmail_message("m1", "Quote files", "see attached")],
            2_000,
        )
        .expect("initial ingest");

        let mut with_attachment = gmail_message("m1", "Quote files", "see attached");
        with_attachment.attachments = vec![GmailAttachmentMeta {
            attachment_id: "att-1".to_string(),
            part_id: Some("1".to_string()),
            filename: "fixture-8316d8157c.test".to_string(),
            mime_type: Some("application/pdf".to_string()),
            size_bytes: Some(2048),
            inline: false,
            content_id: None,
        }];
        let replay = ingest_messages(conn, "test-client", None, &[with_attachment], 3_000)
            .expect("re-ingest");
        assert_eq!(replay.ingested, 0);
        assert_eq!(replay.skipped_existing, 1);

        let stored = store::inbound_by_source_keys(
            conn,
            "test-client",
            &["m1".to_string()],
            &OperatorScope::All,
        )
        .expect("read")
        .remove(0);
        assert_eq!(stored.attachments.len(), 1);
        assert_eq!(stored.attachments[0].attachment_id, "att-1");
        assert_eq!(stored.attachments[0].filename, "fixture-8316d8157c.test");
    }

    #[test]
    fn relabel_backfills_stored_label_ids_to_names_idempotently() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        let mut message = gmail_message("m1", "Hello", "body");
        message.label_ids = vec!["INBOX".to_string(), "Label_13".to_string()];
        ingest_messages(conn, "test-client", None, &[message], 2_000).expect("ingest");

        let map: std::collections::HashMap<String, String> = [
            ("INBOX".to_string(), "INBOX".to_string()),
            (
                "Label_13".to_string(),
                "example@business-f620c5b153.example.test".to_string(),
            ),
        ]
        .into_iter()
        .collect();
        let updated = store::relabel_inbound_messages(conn, "test-client", None, &map, 3_000)
            .expect("relabel");
        assert_eq!(updated, 1);
        let stored = store::inbound_by_source_keys(
            conn,
            "test-client",
            &["m1".to_string()],
            &OperatorScope::All,
        )
        .expect("read")
        .remove(0);
        assert_eq!(
            stored.labels,
            vec![
                "INBOX".to_string(),
                "example@business-f620c5b153.example.test".to_string()
            ]
        );

        // Second pass: nothing left to rename, no new mutations.
        let again =
            store::relabel_inbound_messages(conn, "test-client", None, &map, 4_000).expect("again");
        assert_eq!(again, 0);

        // Unmapped ids survive untouched (deleted label, missing map entry).
        let empty_map = std::collections::HashMap::new();
        assert_eq!(
            store::relabel_inbound_messages(conn, "test-client", None, &empty_map, 5_000)
                .expect("empty map"),
            0
        );
    }

    #[test]
    fn ingest_uses_local_accounting_facts_on_first_classification() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        store::list_categories(conn, "test-client", 1_000).expect("seed categories");
        crate::slices::accounting::store::upsert_customer_snapshots(
            conn,
            "test-client",
            &[bos_integrations::accounting_read::CustomerRecord {
                customer_id: "cust_1".to_string(),
                display_name: "Customer Co".to_string(),
                company_name: None,
                email: Some("customer@example.com".to_string()),
                phone: None,
                active: true,
                tier_raw: None,
                tier_source: bos_integrations::accounting_read::TierSource::NotProvided,
                updated_at: None,
            }],
            1_000,
        )
        .expect("customer snapshot");
        let rule = EmailTriageRule {
            rule_id: "accounting-customer".into(),
            conditions: Vec::new(),
            conditions_v2: vec![EmailTriageConditionV2 {
                condition_id: EmailTriageConditionId::AccountingSenderCustomerExists,
                op: EmailTriageConditionOperator::IsTrue,
                value: EmailTriageConditionValue::Empty,
            }],
            match_mode: EmailTriageMatchMode::All,
            priority: 10,
            enabled: true,
            pinned_category: "operator_note".to_string(),
        };
        store::upsert(conn, ctx("ingest_local_fact_rule", None), &rule).expect("rule");

        ingest_messages(
            conn,
            "test-client",
            Some("mailbox_1"),
            &[gmail_message("m-local-accounting", "Hello", "body")],
            2_000,
        )
        .expect("ingest");

        let stored = store::inbound_by_source_keys(
            conn,
            "test-client",
            &[store::source_key_for(
                Some("mailbox_1"),
                "m-local-accounting",
            )],
            &OperatorScope::All,
        )
        .expect("read")
        .remove(0);
        assert_eq!(stored.resolved_category, "operator_note");
        assert_eq!(
            stored.matched_rule_id.as_deref(),
            Some("accounting-customer")
        );
    }

    #[test]
    fn ingest_tags_the_source_account_and_relabel_stays_per_account() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();

        // Two connected accounts ingest; label ids are PER account, so the
        // same raw id means different labels in each mailbox.
        let mut jons = gmail_message("m-jordan", "jordan's mail", "body");
        jons.label_ids = vec!["Label_13".to_string()];
        let mut davids = gmail_message("m-casey", "casey's mail", "body");
        davids.label_ids = vec!["Label_13".to_string()];
        ingest_messages(conn, "test-client", Some("user_jordan"), &[jons], 2_000).expect("jordan");
        ingest_messages(conn, "test-client", Some("user_casey"), &[davids], 2_100).expect("casey");

        let stored = store::inbound_by_source_keys(
            conn,
            "test-client",
            &[
                store::source_key_for(Some("user_jordan"), "m-jordan"),
                store::source_key_for(Some("user_casey"), "m-casey"),
            ],
            &OperatorScope::All,
        )
        .expect("read");
        assert_eq!(stored[0].source_user_id.as_deref(), Some("user_jordan"));
        assert_eq!(stored[1].source_user_id.as_deref(), Some("user_casey"));

        // jordan's label map renames ONLY jordan's rows.
        let jons_map: std::collections::HashMap<String, String> =
            [("Label_13".to_string(), "Ruby Summary".to_string())]
                .into_iter()
                .collect();
        let updated = store::relabel_inbound_messages(
            conn,
            "test-client",
            Some("user_jordan"),
            &jons_map,
            3_000,
        )
        .expect("relabel");
        assert_eq!(updated, 1, "only jordan's row renamed");
        let stored = store::inbound_by_source_keys(
            conn,
            "test-client",
            &[
                store::source_key_for(Some("user_jordan"), "m-jordan"),
                store::source_key_for(Some("user_casey"), "m-casey"),
            ],
            &OperatorScope::All,
        )
        .expect("read");
        assert_eq!(stored[0].labels, vec!["Ruby Summary".to_string()]);
        assert_eq!(
            stored[1].labels,
            vec!["Label_13".to_string()],
            "casey's row untouched by jordan's label map"
        );
    }

    #[test]
    fn ingest_same_gmail_id_from_two_users_keeps_distinct_source_keys_and_work_items() {
        use bos_contracts::work_queue::WorkQueuePolicy;

        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        crate::slices::work_queue::store::upsert_policy(
            conn,
            "test-client",
            "op_test",
            &WorkQueuePolicy {
                category_id: FALLBACK_CATEGORY_ID.to_string(),
                create_work_item: true,
                packet_kinds: vec!["follow_up_task".to_string()],
                ai_suggestible_packet_kinds: Vec::new(),
                ai_suggestible_gmail_scope: Default::default(),
                ai_suggestible_gmail_categories: Vec::new(),
                auto_produce: false,
            },
            "fallback-policy",
            1_000,
        )
        .expect("policy");

        ingest_messages(
            conn,
            "test-client",
            Some("user_jordan"),
            &[gmail_message("same-gmail-id", "jordan mailbox", "body")],
            2_000,
        )
        .expect("jordan ingest");
        ingest_messages(
            conn,
            "test-client",
            Some("user_casey"),
            &[gmail_message("same-gmail-id", "casey mailbox", "body")],
            2_100,
        )
        .expect("casey ingest");

        let messages = store::list_recent_inbound(
            conn,
            "test-client",
            10,
            &OperatorScope::All,
            &store::InboxFilter::default(),
        )
        .expect("messages");
        assert_eq!(messages.len(), 2);
        assert!(messages
            .iter()
            .all(|message| message.message_id == "same-gmail-id"));
        let source_keys: std::collections::HashSet<_> = messages
            .iter()
            .map(|message| message.source_key.as_str())
            .collect();
        assert_eq!(source_keys.len(), 2);
        assert!(messages
            .iter()
            .any(|message| message.source_user_id.as_deref() == Some("user_jordan")));
        assert!(messages
            .iter()
            .any(|message| message.source_user_id.as_deref() == Some("user_casey")));

        let items = crate::slices::work_queue::store::list_items(
            conn,
            "test-client",
            None,
            10,
            &OperatorScope::All,
        )
        .expect("items");
        assert_eq!(items.len(), 2);
        let item_source_refs: std::collections::HashSet<_> = items
            .iter()
            .map(|entry| entry.item.source_ref.as_str())
            .collect();
        assert_eq!(item_source_refs, source_keys);
    }

    #[test]
    fn ingest_skips_migrated_per_user_legacy_row_and_work_item() {
        use bos_contracts::work_queue::{WorkItem, WorkItemStatus, WorkQueuePolicy};

        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        crate::slices::work_queue::store::upsert_policy(
            conn,
            "test-client",
            "op_test",
            &WorkQueuePolicy {
                category_id: FALLBACK_CATEGORY_ID.to_string(),
                create_work_item: true,
                packet_kinds: vec!["follow_up_task".to_string()],
                ai_suggestible_packet_kinds: Vec::new(),
                ai_suggestible_gmail_scope: Default::default(),
                ai_suggestible_gmail_categories: Vec::new(),
                auto_produce: false,
            },
            "fallback-policy",
            1_000,
        )
        .expect("policy");

        store::record_inbound_message(
            conn,
            "test-client",
            &store::InboundMessageRecord {
                source_key: "legacy-gmail-id".to_string(),
                message_id: "legacy-gmail-id".to_string(),
                thread_id: Some("thr-legacy-gmail-id".to_string()),
                internal_date_ms: Some(1_000_000),
                from_addr: Some("customer@example.com".to_string()),
                to_addr: Some("ops@business-a91b8b0f88.example.test".to_string()),
                subject: Some("Legacy migrated mail".to_string()),
                body_excerpt: "body".to_string(),
                body_full: "body".to_string(),
                headers: Vec::new(),
                labels: Vec::new(),
                resolved_category: FALLBACK_CATEGORY_ID.to_string(),
                matched_rule_id: None,
                ingested_at_ms: 1_000,
                ai_triage_status: None,
                ai_triage_rationale: None,
                attachments: Vec::new(),
                source_user_id: Some("user_jordan".to_string()),
            },
        )
        .expect("legacy inbound row");
        crate::slices::work_queue::store::insert_item(
            conn,
            "test-client",
            &WorkItem {
                item_id: "wi_email_legacy-gmail-id".to_string(),
                source_kind: crate::slices::work_queue::SOURCE_KIND_EMAIL.to_string(),
                source_ref: "legacy-gmail-id".to_string(),
                category_id: FALLBACK_CATEGORY_ID.to_string(),
                title: "Legacy migrated mail".to_string(),
                summary: "body".to_string(),
                packet_kinds: vec!["follow_up_task".to_string()],
                status: WorkItemStatus::Open,
                accept_actor: None,
                ai_suggested: false,
                rationale: String::new(),
                produce_guidance: String::new(),
                source_user_id: Some("user_jordan".to_string()),
                assignee_user_id: None,
                visible_to_user_ids: Vec::new(),
                created_at_ms: 1_000,
                updated_at_ms: 1_000,
            },
        )
        .expect("legacy work item");

        let summary = ingest_messages(
            conn,
            "test-client",
            Some("user_jordan"),
            &[gmail_message("legacy-gmail-id", "Same mail", "new body")],
            2_000,
        )
        .expect("reingest");
        assert_eq!(summary.ingested, 0);
        assert_eq!(summary.skipped_existing, 1);

        let messages = store::list_recent_inbound(
            conn,
            "test-client",
            10,
            &OperatorScope::All,
            &store::InboxFilter::default(),
        )
        .expect("messages");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].source_key, "legacy-gmail-id");

        let items = crate::slices::work_queue::store::list_items(
            conn,
            "test-client",
            None,
            10,
            &OperatorScope::All,
        )
        .expect("items");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].item.source_ref, "legacy-gmail-id");
    }

    #[test]
    fn inbound_reads_filter_by_operator_scope() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();

        ingest_messages(
            conn,
            "test-client",
            None,
            &[gmail_message("m-null", "Legacy", "body")],
            2_000,
        )
        .expect("legacy");
        ingest_messages(
            conn,
            "test-client",
            Some("user_jordan"),
            &[gmail_message("m-u1", "jordan", "body")],
            2_100,
        )
        .expect("jordan");
        ingest_messages(
            conn,
            "test-client",
            Some("user_casey"),
            &[gmail_message("m-u2", "casey", "body")],
            2_200,
        )
        .expect("casey");

        let all = store::list_recent_inbound(
            conn,
            "test-client",
            10,
            &OperatorScope::All,
            &store::InboxFilter::default(),
        )
        .expect("all inbox");
        let all_ids: std::collections::HashSet<_> = all
            .iter()
            .map(|message| message.message_id.as_str())
            .collect();
        assert_eq!(
            all_ids,
            std::collections::HashSet::from(["m-null", "m-u1", "m-u2"])
        );

        let user = store::list_recent_inbound(
            conn,
            "test-client",
            10,
            &OperatorScope::User("user_jordan".to_string()),
            &store::InboxFilter::default(),
        )
        .expect("user inbox");
        assert_eq!(user.len(), 1);
        assert_eq!(user[0].message_id, "m-u1");

        let scoped_lookup = store::inbound_by_source_keys(
            conn,
            "test-client",
            &[
                "m-null".to_string(),
                store::source_key_for(Some("user_jordan"), "m-u1"),
                store::source_key_for(Some("user_casey"), "m-u2"),
            ],
            &OperatorScope::User("user_jordan".to_string()),
        )
        .expect("scoped lookup");
        assert_eq!(scoped_lookup.len(), 1);
        assert_eq!(scoped_lookup[0].message_id, "m-u1");
    }

    #[test]
    fn ingest_classifies_persists_and_skips_known_ids() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        store::upsert(
            persistence.connection(),
            ctx("rule_1", None),
            &call_log_rule(),
        )
        .expect("seed rule");

        let messages = vec![
            gmail_message("m1", "Daily call log 6/9", "calls: jamie, dana"),
            gmail_message("m2", "Invoice question", "where is my invoice?"),
        ];
        let summary = ingest_messages(
            persistence.connection(),
            "test-client",
            None,
            &messages,
            2_000,
        )
        .expect("ingest");
        assert_eq!(summary.ingested, 2);
        assert_eq!(summary.skipped_existing, 0);

        // Re-poll with the same batch: nothing new, no extra receipts.
        let replay = ingest_messages(
            persistence.connection(),
            "test-client",
            None,
            &messages,
            3_000,
        )
        .expect("re-ingest");
        assert_eq!(replay.ingested, 0);
        assert_eq!(replay.skipped_existing, 2);

        let inbox = store::list_recent_inbound(
            persistence.connection_ref(),
            "test-client",
            10,
            &OperatorScope::All,
            &store::InboxFilter::default(),
        )
        .expect("inbox");
        assert_eq!(inbox.len(), 2);
        let call_log = inbox.iter().find(|m| m.message_id == "m1").expect("m1");
        assert_eq!(call_log.resolved_category, "operator_note");
        assert_eq!(call_log.matched_rule_id.as_deref(), Some("call-log"));
        let plain = inbox.iter().find(|m| m.message_id == "m2").expect("m2");
        assert_eq!(plain.resolved_category, FALLBACK_CATEGORY_ID);
        assert!(plain.matched_rule_id.is_none());

        // Each ingested message has exactly one system receipt; re-poll added none.
        for id in ["m1", "m2"] {
            let receipts = crate::store_core::receipts_for_entity(
                persistence.connection_ref(),
                "test-client",
                store::INBOUND_ENTITY_KIND,
                id,
                10,
            )
            .expect("receipts");
            assert_eq!(receipts.len(), 1, "exactly one ingest receipt for {id}");
            assert_eq!(receipts[0].change_kind, "ingest");
        }
    }

    #[test]
    fn ingest_bounds_stored_body_excerpt() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let long_body = "x".repeat(5_000);
        let messages = vec![gmail_message("big", "huge", &long_body)];
        ingest_messages(
            persistence.connection(),
            "test-client",
            None,
            &messages,
            2_000,
        )
        .expect("ingest");
        let inbox = store::list_recent_inbound(
            persistence.connection_ref(),
            "test-client",
            10,
            &OperatorScope::All,
            &store::InboxFilter::default(),
        )
        .expect("inbox");
        assert_eq!(inbox[0].body_excerpt.chars().count(), 600);
        assert_eq!(inbox[0].body_full.chars().count(), 5_000);
    }

    #[test]
    fn ingest_persists_capped_full_body_and_excerpt_uses_display_trim() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let body = format!(
            "{}\n{}",
            "x".repeat(70_000),
            "Current Average Annual Purchases: $15000-$30000"
        );
        let messages = vec![gmail_message(
            "long",
            "Wholesale Account Application",
            &body,
        )];
        ingest_messages(
            persistence.connection(),
            "test-client",
            None,
            &messages,
            2_000,
        )
        .expect("ingest");

        let inbox = store::list_recent_inbound(
            persistence.connection_ref(),
            "test-client",
            10,
            &OperatorScope::All,
            &store::InboxFilter::default(),
        )
        .expect("inbox");
        assert_eq!(inbox[0].body_excerpt.chars().count(), 600);
        assert_eq!(
            inbox[0].body_full.chars().count(),
            store::BODY_FULL_MAX_CHARS
        );
    }

    #[test]
    fn forwarded_form_excerpt_skips_preamble_but_full_body_stays_raw() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let body = wholesale_forward_body();
        let messages = vec![gmail_message(
            "wholesale",
            "Fwd: New Wholesale Account Application submission received on 06/12/2026 at 05:36 AM",
            &body,
        )];
        ingest_messages(
            persistence.connection(),
            "test-client",
            None,
            &messages,
            2_000,
        )
        .expect("ingest");

        let inbox = store::list_recent_inbound(
            persistence.connection_ref(),
            "test-client",
            10,
            &OperatorScope::All,
            &store::InboxFilter::default(),
        )
        .expect("inbox");
        assert!(inbox[0]
            .body_full
            .contains("---------- Forwarded message ---------"));
        assert!(inbox[0]
            .body_full
            .contains("Current Average Annual Purchases"));
        assert!(!inbox[0]
            .body_excerpt
            .contains("---------- Forwarded message"));
        assert!(inbox[0].body_excerpt.contains("From: Form Submit"));
    }

    pub(super) fn wholesale_forward_body() -> String {
        "\
---------- Forwarded message ---------
From: Form Submit <info@business-014bb695de.example.test>
Date: Fri, Jun 12, 2026 at 5:36 AM
Subject: New Wholesale Account Application
To: example Info <ask@business-914f630770.example.test>

Hi jordan,
Business Name: Taylor Repair Service
Primary Contact: Davey Jones
Phone Number: +12512337651
Primary Contact Email: info@business-df29801f39.example.test
What is your primary business: Property maintenance and repair
Current Average Annual Purchases: $15000-$30000
Tax Exempt: Yes
EIN: 12-3456789
W-9: attached"
            .to_string()
    }
}

mod categories {
    use super::*;
    use crate::slices::email_triage::store;
    use crate::store_core::StoreError;

    #[test]
    fn category_and_policy_creation_rolls_back_together() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        conn.execute_batch(
            "CREATE TRIGGER reject_atomic_policy BEFORE INSERT ON work_queue_policies \
             WHEN policy.category_id = 'atomic_test' BEGIN SELECT RAISE(ABORT, 'reject policy'); END;",
        )
        .expect("install failure trigger");
        let category = CategoryRecord {
            category_id: "atomic_test".to_string(),
            display_name: "Atomic test".to_string(),
            description: "Must not survive a policy failure.".to_string(),
            color: "#38bdf8".to_string(),
            sort: 40,
            is_system: false,
            default_agent_dir: String::new(),
            default_agent_context: String::new(),
        };
        let policy = WorkQueuePolicy {
            category_id: category.category_id.clone(),
            create_work_item: true,
            packet_kinds: Vec::new(),
            ai_suggestible_packet_kinds: Vec::new(),
            ai_suggestible_gmail_scope: Default::default(),
            ai_suggestible_gmail_categories: Vec::new(),
            auto_produce: false,
        };

        let result = store::upsert_category_with_policy(
            conn,
            CLIENT,
            "op_test",
            &category,
            &policy,
            "atomic_category_1",
            1_000,
        );

        assert!(result.is_err(), "forced policy failure must surface");
        assert!(
            store::category_by_id(conn, CLIENT, &category.category_id)
                .expect("read category")
                .is_none(),
            "category write must roll back with policy write"
        );
        assert_eq!(
            conn.query_row(
                "SELECT outcome FROM receipts WHERE client_id = ?1 AND idempotency_key = ?2",
                rusqlite::params![CLIENT, "atomic_category_1"],
                |row| row.get::<_, String>(0),
            )
            .expect("failure receipt"),
            "failed"
        );
    }

    #[test]
    fn first_read_seeds_defaults_and_crud_round_trips() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();

        let seeded = store::list_categories(conn, "test-client", 1_000).expect("seed");
        assert_eq!(seeded.len(), 2);
        assert!(seeded
            .iter()
            .any(|c| c.category_id == FALLBACK_CATEGORY_ID && c.is_system));

        store::upsert_category(
            conn,
            "test-client",
            "op_test",
            &CategoryRecord {
                category_id: "billing".to_string(),
                display_name: "Billing".to_string(),
                description: "Invoices, payments, and utility bills.".to_string(),
                color: "#10b981".to_string(),
                sort: 40,
                is_system: false,
                default_agent_dir: "/home/example/projects/billing-client".to_string(),
                default_agent_context: "Use the billing-client runbook.".to_string(),
            },
            "cat_1",
            2_000,
        )
        .expect("create");
        let listed = store::list_categories(conn, "test-client", 2_500).expect("list");
        assert_eq!(listed.len(), 3);
        let billing = store::category_by_id(conn, "test-client", "billing")
            .expect("read billing")
            .expect("billing category");
        assert_eq!(
            billing.default_agent_dir,
            "/home/example/projects/billing-client"
        );
        assert_eq!(
            billing.default_agent_context,
            "Use the billing-client runbook."
        );

        // Client-supplied is_system must NOT grant protection.
        store::upsert_category(
            conn,
            "test-client",
            "op_test",
            &CategoryRecord {
                category_id: "sneaky".to_string(),
                display_name: "Sneaky".to_string(),
                description: String::new(),
                color: "#fff".to_string(),
                sort: 50,
                is_system: true,
                default_agent_dir: String::new(),
                default_agent_context: String::new(),
            },
            "cat_2",
            3_000,
        )
        .expect("create sneaky");
        let listed = store::list_categories(conn, "test-client", 3_100).expect("list");
        let sneaky = listed
            .iter()
            .find(|c| c.category_id == "sneaky")
            .expect("sneaky");
        assert!(!sneaky.is_system, "wire is_system must be ignored");

        store::delete_category(conn, "test-client", "op_test", "billing", "cat_3", 4_000)
            .expect("delete unused");
        let listed = store::list_categories(conn, "test-client", 4_100).expect("list");
        assert!(!listed.iter().any(|c| c.category_id == "billing"));
    }

    #[test]
    fn deleted_default_category_stays_deleted_across_reads() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        store::list_categories(conn, "test-client", 1_000).expect("seed");

        store::delete_category(conn, "test-client", "op_test", "operator_note", "k1", 2_000)
            .expect("delete default");
        let listed = store::list_categories(conn, "test-client", 3_000).expect("list");
        assert!(
            !listed.iter().any(|c| c.category_id == "operator_note"),
            "per-id seeding must not resurrect a deleted default"
        );
    }

    #[test]
    fn delete_refused_for_system_and_in_use_categories() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        store::list_categories(conn, "test-client", 1_000).expect("seed");

        let err = store::delete_category(
            conn,
            "test-client",
            "op_test",
            FALLBACK_CATEGORY_ID,
            "k1",
            2_000,
        )
        .expect_err("system");
        assert!(
            matches!(err, StoreError::Domain(code) if code == "email_triage_category_is_system")
        );

        store::upsert(conn, ctx("rule_1", None), &call_log_rule())
            .expect("rule pinning operator_note");
        let err =
            store::delete_category(conn, "test-client", "op_test", "operator_note", "k2", 3_000)
                .expect_err("in use");
        assert!(matches!(err, StoreError::Domain(code) if code == "email_triage_category_in_use"));
    }

    #[test]
    fn rule_upsert_rejects_unknown_category() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        store::list_categories(conn, "test-client", 1_000).expect("seed");
        let rule = EmailTriageRule {
            pinned_category: "nonexistent_cat".to_string(),
            ..call_log_rule()
        };
        let err = store::upsert(conn, ctx("r1", None), &rule).expect_err("unknown category");
        assert!(err.to_string().contains("email_triage_category_unknown"));
    }
}

#[test]
fn reclassify_applies_new_rules_to_old_mail_and_backfills_work_items() {
    use crate::slices::email_triage::service::reclassify_all;
    use crate::slices::work_queue::store as wq_store;
    use bos_contracts::work_queue::WorkQueuePolicy;

    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    store::list_categories(conn, "test-client", 100).expect("seed");

    // Mail ingested BEFORE any rule exists -> fallback category.
    crate::slices::email_triage::worker::ingest_messages(
        conn,
        "test-client",
        None,
        &[bos_integrations::gmail_inbox_read::GmailFullMessage {
            message_id: "old1".to_string(),
            thread_id: None,
            label_ids: vec![],
            internal_date_epoch_ms: Some(500),
            subject: Some("Climbing gym session Thursday".to_string()),
            from: Some("gym@business-33a6208d95.example.test".to_string()),
            to: None,
            headers: vec![],
            plain_text_body: "see you there".to_string(),
            html_body: None,
            attachments: Vec::new(),
        }],
        1_000,
    )
    .expect("ingest");
    let stored = store::list_recent_inbound(
        persistence.connection_ref(),
        "test-client",
        10,
        &OperatorScope::All,
        &store::InboxFilter::default(),
    )
    .expect("list");
    assert_eq!(stored[0].resolved_category, FALLBACK_CATEGORY_ID);

    // Operator then creates category + rule + policy (the example scenario).
    let conn = persistence.connection();
    store::upsert_category(
        conn,
        "test-client",
        "op_test",
        &CategoryRecord {
            category_id: "hobbies".to_string(),
            display_name: "Hobbies".to_string(),
            description: "Personal hobby mail".to_string(),
            color: "#22c55e".to_string(),
            sort: 50,
            is_system: false,
            default_agent_dir: String::new(),
            default_agent_context: String::new(),
        },
        "c1",
        2_000,
    )
    .expect("category");
    store::upsert(
        conn,
        ctx("r1", None),
        &EmailTriageRule {
            rule_id: "hobby-gym".to_string(),
            conditions: vec![EmailTriageCondition {
                field: EmailTriageField::From,
                op: EmailTriageOperator::Contains,
                value: "@business-33a6208d95.example.test".to_string(),
                header_name: None,
            }],
            conditions_v2: Vec::new(),
            match_mode: EmailTriageMatchMode::All,
            priority: 5,
            enabled: true,
            pinned_category: "hobbies".to_string(),
        },
    )
    .expect("rule");
    wq_store::upsert_policy(
        conn,
        "test-client",
        "op_test",
        &WorkQueuePolicy {
            category_id: "hobbies".to_string(),
            create_work_item: true,
            packet_kinds: vec!["follow_up_task".to_string()],
            ai_suggestible_packet_kinds: Vec::new(),
            ai_suggestible_gmail_scope: Default::default(),
            ai_suggestible_gmail_categories: Vec::new(),
            auto_produce: false,
        },
        "p1",
        2_100,
    )
    .expect("policy");

    // Nothing in the queue yet — old mail still carries the old category.
    assert!(wq_store::list_items(
        persistence.connection_ref(),
        "test-client",
        None,
        10,
        &OperatorScope::All
    )
    .expect("items")
    .is_empty());

    // Re-run: classification updates AND the work item appears.
    let (examined, reclassified, emitted) = reclassify_all(
        persistence.connection(),
        "test-client",
        "op_test",
        FALLBACK_CATEGORY_ID,
        &crate::overlay::WorkQueueOverlay::default(),
        3_000,
    )
    .expect("reclassify");
    assert_eq!((examined, reclassified, emitted), (1, 1, 1));

    let stored = store::list_recent_inbound(
        persistence.connection_ref(),
        "test-client",
        10,
        &OperatorScope::All,
        &store::InboxFilter::default(),
    )
    .expect("list");
    assert_eq!(stored[0].resolved_category, "hobbies");
    assert_eq!(stored[0].matched_rule_id.as_deref(), Some("hobby-gym"));
    let items = wq_store::list_items(
        persistence.connection_ref(),
        "test-client",
        None,
        10,
        &OperatorScope::All,
    )
    .expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].item.category_id, "hobbies");
    assert_eq!(
        items[0].item.packet_kinds,
        vec!["follow_up_task".to_string()]
    );

    // Second re-run with nothing changed: fully quiet.
    let (_, reclassified, emitted) = reclassify_all(
        persistence.connection(),
        "test-client",
        "op_test",
        FALLBACK_CATEGORY_ID,
        &crate::overlay::WorkQueueOverlay::default(),
        4_000,
    )
    .expect("re-run");
    assert_eq!((reclassified, emitted), (0, 0));
}

#[test]
fn ingest_ignores_non_safe_headers_for_initial_rule_matching() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    {
        let conn = persistence.connection();
        store::list_categories(conn, "test-client", 100).expect("seed");
        store::upsert_category(
            conn,
            "test-client",
            "op_test",
            &CategoryRecord {
                category_id: "custom_header".to_string(),
                display_name: "Custom header".to_string(),
                description: "Mail matched by a custom header".to_string(),
                color: "#64748b".to_string(),
                sort: 60,
                is_system: false,
                default_agent_dir: String::new(),
                default_agent_context: String::new(),
            },
            "custom-header-cat",
            1_000,
        )
        .expect("category");
        store::upsert(
            conn,
            ctx("custom-header-rule", None),
            &EmailTriageRule {
                rule_id: "custom-header".to_string(),
                conditions: vec![EmailTriageCondition {
                    field: EmailTriageField::Header,
                    op: EmailTriageOperator::Contains,
                    value: "match-me".to_string(),
                    header_name: Some("X-Custom-Unsafe".to_string()),
                }],
                conditions_v2: Vec::new(),
                match_mode: EmailTriageMatchMode::All,
                priority: 5,
                enabled: true,
                pinned_category: "custom_header".to_string(),
            },
        )
        .expect("rule");

        crate::slices::email_triage::worker::ingest_messages(
            conn,
            "test-client",
            None,
            &[bos_integrations::gmail_inbox_read::GmailFullMessage {
                message_id: "unsafe-header-1".to_string(),
                thread_id: None,
                label_ids: vec![],
                internal_date_epoch_ms: Some(500),
                subject: Some("Unsafe header".to_string()),
                from: Some("person@example.test".to_string()),
                to: None,
                headers: vec![("X-Custom-Unsafe".to_string(), "match-me".to_string())],
                plain_text_body: "Body".to_string(),
                html_body: None,
                attachments: Vec::new(),
            }],
            2_000,
        )
        .expect("ingest");
    }

    let stored = store::list_recent_inbound(
        persistence.connection_ref(),
        "test-client",
        10,
        &OperatorScope::All,
        &store::InboxFilter::default(),
    )
    .expect("list");
    assert_eq!(stored[0].resolved_category, FALLBACK_CATEGORY_ID);
    assert_eq!(stored[0].matched_rule_id, None);
    assert_eq!(stored[0].headers, Vec::<(String, String)>::new());
}

#[test]
fn ingest_persists_safe_headers_for_later_reclassification() {
    use crate::slices::email_triage::service::reclassify_all;

    let mut persistence = Persistence::open_in_memory().expect("db");
    {
        let conn = persistence.connection();
        store::list_categories(conn, "test-client", 100).expect("seed");

        crate::slices::email_triage::worker::ingest_messages(
            conn,
            "test-client",
            None,
            &[bos_integrations::gmail_inbox_read::GmailFullMessage {
                message_id: "auto-reply-1".to_string(),
                thread_id: None,
                label_ids: vec![],
                internal_date_epoch_ms: Some(500),
                subject: Some("Out of office".to_string()),
                from: Some("person@example.test".to_string()),
                to: None,
                headers: vec![
                    ("Auto-Submitted".to_string(), " auto-replied ".to_string()),
                    (
                        "Cc".to_string(),
                        "Ops <ops@example.test>, Alex <alex@example.test>".to_string(),
                    ),
                    ("Delivered-To".to_string(), "inbox@example.test".to_string()),
                    (
                        "Authentication-Results".to_string(),
                        "private auth material".to_string(),
                    ),
                    (
                        "List-Unsubscribe".to_string(),
                        "<mailto:leave@example.test>\n".to_string(),
                    ),
                    (
                        "X-Original-To".to_string(),
                        "original@example.test".to_string(),
                    ),
                ],
                plain_text_body: "I am away this week.".to_string(),
                html_body: None,
                attachments: Vec::new(),
            }],
            1_000,
        )
        .expect("ingest");
    }

    let stored = store::list_recent_inbound(
        persistence.connection_ref(),
        "test-client",
        10,
        &OperatorScope::All,
        &store::InboxFilter::default(),
    )
    .expect("list");
    assert_eq!(
        stored[0].headers,
        vec![
            ("auto-submitted".to_string(), "auto-replied".to_string()),
            (
                "cc".to_string(),
                "Ops <ops@example.test>, Alex <alex@example.test>".to_string()
            ),
            ("delivered-to".to_string(), "inbox@example.test".to_string()),
            (
                "list-unsubscribe".to_string(),
                "<mailto:leave@example.test>".to_string()
            ),
            (
                "x-original-to".to_string(),
                "original@example.test".to_string()
            ),
        ]
    );
    let serialized = serde_json::to_value(&stored[0]).expect("serialize inbound");
    assert!(
        serialized.get("headers").is_none(),
        "stored headers are for server-side rules only"
    );

    {
        let conn = persistence.connection();
        store::upsert_category(
            conn,
            "test-client",
            "op_test",
            &CategoryRecord {
                category_id: "automated".to_string(),
                display_name: "Automated".to_string(),
                description: "Automated sender mail".to_string(),
                color: "#64748b".to_string(),
                sort: 60,
                is_system: false,
                default_agent_dir: String::new(),
                default_agent_context: String::new(),
            },
            "auto-cat",
            2_000,
        )
        .expect("category");
        store::upsert(
            conn,
            ctx("auto-rule", None),
            &EmailTriageRule {
                rule_id: "auto-submitted".to_string(),
                conditions: vec![EmailTriageCondition {
                    field: EmailTriageField::Header,
                    op: EmailTriageOperator::Contains,
                    value: "auto-replied".to_string(),
                    header_name: Some("Auto-Submitted".to_string()),
                }],
                conditions_v2: Vec::new(),
                match_mode: EmailTriageMatchMode::All,
                priority: 5,
                enabled: true,
                pinned_category: "automated".to_string(),
            },
        )
        .expect("rule");
    }

    let (examined, reclassified, emitted) = reclassify_all(
        persistence.connection(),
        "test-client",
        "op_test",
        FALLBACK_CATEGORY_ID,
        &crate::overlay::WorkQueueOverlay::default(),
        3_000,
    )
    .expect("reclassify");
    assert_eq!((examined, reclassified, emitted), (1, 1, 0));

    let stored = store::list_recent_inbound(
        persistence.connection_ref(),
        "test-client",
        10,
        &OperatorScope::All,
        &store::InboxFilter::default(),
    )
    .expect("list");
    assert_eq!(stored[0].resolved_category, "automated");
    assert_eq!(stored[0].matched_rule_id.as_deref(), Some("auto-submitted"));
}

#[test]
fn reclassify_all_examines_more_than_inbox_page_limit() {
    use crate::slices::email_triage::service::reclassify_all;

    let mut persistence = Persistence::open_in_memory().expect("db");
    {
        let conn = persistence.connection();
        for idx in 0..600 {
            let mut message = inbound_message(&format!("bulk-{idx:03}"), None);
            message.internal_date_ms = Some(idx);
            store::record_inbound_message(conn, CLIENT, &message).expect("record inbound");
        }
    }

    let (examined, reclassified, emitted) = reclassify_all(
        persistence.connection(),
        CLIENT,
        "op_test",
        FALLBACK_CATEGORY_ID,
        &crate::overlay::WorkQueueOverlay::default(),
        5_000,
    )
    .expect("reclassify");

    assert_eq!(examined, 600);
    assert_eq!(reclassified, 0);
    assert_eq!(emitted, 0);
}

mod ai_triage {
    use super::*;
    use crate::slices::email_triage::service::{
        ai_suggestible_kinds, ai_suggestible_kinds_for_enabled, ai_suggestible_kinds_for_policy,
        build_ai_triage_request, parse_ai_triage_response, retain_enabled_ai_suggestions,
        AiConfidence,
    };
    use bos_contracts::email_triage::InboundMessageRecord;
    use bos_contracts::work_queue::WorkQueuePolicy;
    use serde_json::json;

    fn set_runtime_override(
        conn: &mut rusqlite::Connection,
        var: &crate::env_registry::EnvVar,
        value: &str,
        key: &str,
    ) {
        crate::slices::admin_settings::store::upsert_override(
            conn,
            crate::slices::admin_settings::store::OverrideWrite {
                client_id: "test-client",
                actor_id: "op_test",
                var_name: var.name,
                value,
                expected_revision: None,
                idempotency_key: key,
                now_ms: 1_000,
            },
        )
        .expect("setting override");
    }

    fn categories() -> Vec<CategoryRecord> {
        vec![CategoryRecord {
            category_id: "hobbies".to_string(),
            display_name: "Hobbies".to_string(),
            description: "Personal hobby mail".to_string(),
            color: "#22c55e".to_string(),
            sort: 50,
            is_system: false,
            default_agent_dir: String::new(),
            default_agent_context: String::new(),
        }]
    }

    #[test]
    fn parse_drops_unknown_kinds_and_categories() {
        let response = json!({
            "suggested_packet_kinds": [
                "calendar_event_draft",
                "rm_rf_slash",
                "follow_up_task",
                "email_draft_reply",
                "crm_activity",
                "crm_record_create"
            ],
            "suggested_category": "made_up_category",
            "confidence": "HIGH",
            "rationale": "Registration confirmation with a specific date.",
        });
        let suggestion = parse_ai_triage_response(&response, &categories()).expect("parse");
        assert_eq!(
            suggestion.suggested_packet_kinds,
            vec![
                "calendar_event_draft".to_string(),
                "follow_up_task".to_string(),
                "email_draft_reply".to_string(),
                "crm_activity".to_string(),
                "crm_record_create".to_string()
            ]
        );
        assert_eq!(
            suggestion.suggested_category, None,
            "unknown category dropped"
        );
        assert_eq!(suggestion.confidence, AiConfidence::High);
        let suggestible: Vec<&str> = ai_suggestible_kinds().collect();
        assert!(suggestible.contains(&"calendar_event_draft"));
        assert!(suggestible.contains(&"email_draft_reply"));
        assert!(suggestible.contains(&"crm_activity"));
        assert!(suggestible.contains(&"crm_record_create"));
        for kind in suggestible {
            assert!(
                crate::slices::work_queue::packet_kind_slice(kind).is_some(),
                "AI-suggestible kind {kind} must have a work_queue slice owner",
            );
        }
    }

    #[test]
    fn parse_accepts_known_category_and_rejects_garbage() {
        let response = json!({
            "suggested_packet_kinds": [],
            "suggested_category": "hobbies",
            "confidence": "medium",
            "rationale": "Hobby newsletter, no action.",
        });
        let suggestion = parse_ai_triage_response(&response, &categories()).expect("parse");
        assert_eq!(suggestion.suggested_category.as_deref(), Some("hobbies"));
        assert!(suggestion.suggested_packet_kinds.is_empty());

        let garbage = json!({"confidence": "very sure", "suggested_packet_kinds": []});
        assert!(parse_ai_triage_response(&garbage, &categories()).is_err());
        let missing = json!({"confidence": "high"});
        assert!(parse_ai_triage_response(&missing, &categories()).is_err());
    }

    #[test]
    fn request_offers_reply_and_crm_packet_kinds() {
        let message = InboundMessageRecord {
            source_key: "m-ai-1".to_string(),
            message_id: "m-ai-1".to_string(),
            thread_id: Some("t-ai-1".to_string()),
            internal_date_ms: Some(1_000),
            from_addr: Some("buyer@example.test".to_string()),
            to_addr: Some("ops@example.test".to_string()),
            subject: Some("Can you send a quote?".to_string()),
            body_excerpt: "Please reply with a quote and add Acme Co to CRM.".to_string(),
            body_full: String::new(),
            headers: Vec::new(),
            labels: vec![],
            resolved_category: FALLBACK_CATEGORY_ID.to_string(),
            matched_rule_id: None,
            ingested_at_ms: 1_000,
            ai_triage_status: None,
            ai_triage_rationale: None,
            attachments: Vec::new(),
            source_user_id: None,
        };
        let request = build_ai_triage_request(
            "test-client",
            &message,
            &categories(),
            crate::slices::work_queue::packet_kind_catalog(),
        );
        let offered: Vec<String> = request.input.json["packet_kind_catalog"]
            .as_array()
            .expect("catalog array")
            .iter()
            .map(|kind| kind["kind_id"].as_str().expect("kind id").to_string())
            .collect();
        assert!(offered.contains(&"email_draft_reply".to_string()));
        assert!(offered.contains(&"crm_activity".to_string()));
        assert!(offered.contains(&"crm_record_create".to_string()));
        assert!(!offered.contains(&"claim_draft".to_string()));
    }

    #[test]
    fn request_uses_raw_full_body_not_display_excerpt() {
        let message = InboundMessageRecord {
            source_key: "m-ai-wholesale".to_string(),
            message_id: "m-ai-wholesale".to_string(),
            thread_id: None,
            internal_date_ms: Some(1_000),
            from_addr: Some("ask@business-914f630770.example.test".to_string()),
            to_addr: Some("casey@business-914f630770.example.test".to_string()),
            subject: Some("Fwd: New Wholesale Account Application".to_string()),
            body_excerpt: "Business Name: Taylor Repair Service".to_string(),
            body_full: super::ingest::wholesale_forward_body(),
            headers: Vec::new(),
            labels: vec![],
            resolved_category: FALLBACK_CATEGORY_ID.to_string(),
            matched_rule_id: None,
            ingested_at_ms: 1_000,
            ai_triage_status: None,
            ai_triage_rationale: None,
            attachments: Vec::new(),
            source_user_id: None,
        };
        let request = build_ai_triage_request(
            "test-client",
            &message,
            &categories(),
            crate::slices::work_queue::packet_kind_catalog(),
        );
        let text = &request.input.text_blocks[0].text;
        assert!(text.contains("Current Average Annual Purchases: $15000-$30000"));
        assert!(text.contains("Primary Contact Email: info@business-df29801f39.example.test"));
        assert!(text.contains("From: Form Submit <info@business-014bb695de.example.test>"));
        assert!(text.contains("Subject: New Wholesale Account Application"));
        assert!(text.contains("---------- Forwarded message ---------"));
        assert!(text.contains("Date: Fri"));
    }

    #[test]
    fn request_serialized_input_stays_inside_declared_byte_budget() {
        let message = InboundMessageRecord {
            source_key: "m-ai-long".to_string(),
            message_id: "m-ai-long".to_string(),
            thread_id: None,
            internal_date_ms: Some(1_000),
            from_addr: Some("buyer@example.test".to_string()),
            to_addr: Some("ops@example.test".to_string()),
            subject: Some("Long request".to_string()),
            body_excerpt: "short".to_string(),
            body_full: "x".repeat(70_000),
            headers: Vec::new(),
            labels: vec![],
            resolved_category: FALLBACK_CATEGORY_ID.to_string(),
            matched_rule_id: None,
            ingested_at_ms: 1_000,
            ai_triage_status: None,
            ai_triage_rationale: None,
            attachments: Vec::new(),
            source_user_id: None,
        };
        let request = build_ai_triage_request(
            "test-client",
            &message,
            &categories(),
            crate::slices::work_queue::packet_kind_catalog(),
        );
        let serialized = serde_json::to_string(&request.input).expect("serialize input");
        assert!(
            serialized.len() as u64 <= request.spec.max_input_bytes,
            "serialized input was {} bytes; max is {}",
            serialized.len(),
            request.spec.max_input_bytes
        );
        assert_eq!(
            request.input.text_blocks[0].text.len(),
            crate::slices::email_triage::service::MODEL_BODY_MAX_BYTES
                + "From: buyer@example.test\nTo: ops@example.test\nSubject: Long request\nLabels: \n\n"
                    .len()
        );
    }

    #[test]
    fn suggestible_kinds_follow_enabled_slices() {
        let enabled = ai_suggestible_kinds_for_enabled(
            crate::slices::work_queue::packet_kind_catalog(),
            |slice_id| {
                matches!(
                    slice_id,
                    "email_drafts" | "crm_drafts" | "crm_record_drafts" | "follow_up_tasks"
                )
            },
        );
        let offered: Vec<String> = enabled.into_iter().map(|kind| kind.kind_id).collect();
        assert_eq!(
            offered,
            vec![
                "follow_up_task".to_string(),
                "email_draft_reply".to_string(),
                "crm_activity".to_string(),
                "crm_record_create".to_string()
            ]
        );

        let mut returned = vec![
            "email_draft_reply".to_string(),
            "invoice_draft".to_string(),
            "rm_rf_slash".to_string(),
            "crm_activity".to_string(),
        ];
        retain_enabled_ai_suggestions(&mut returned, |slice_id| {
            matches!(slice_id, "email_drafts" | "crm_drafts")
        });
        assert_eq!(
            returned,
            vec!["email_draft_reply".to_string(), "crm_activity".to_string()]
        );
    }

    #[test]
    fn suggestible_kinds_follow_category_policy_allow_list() {
        let enabled = ai_suggestible_kinds_for_enabled(
            crate::slices::work_queue::packet_kind_catalog(),
            |_| true,
        );
        let policy = WorkQueuePolicy {
            category_id: FALLBACK_CATEGORY_ID.to_string(),
            create_work_item: true,
            packet_kinds: vec!["crm_activity".to_string()],
            ai_suggestible_packet_kinds: vec![
                "follow_up_task".to_string(),
                "email_draft_reply".to_string(),
                "claim_draft".to_string(),
            ],
            ai_suggestible_gmail_scope: Default::default(),
            ai_suggestible_gmail_categories: Vec::new(),
            auto_produce: false,
        };
        let offered: Vec<String> = ai_suggestible_kinds_for_policy(&enabled, Some(&policy))
            .into_iter()
            .map(|kind| kind.kind_id)
            .collect();
        assert_eq!(
            offered,
            vec![
                "follow_up_task".to_string(),
                "email_draft_reply".to_string()
            ]
        );
        assert!(
            ai_suggestible_kinds_for_policy(&enabled, None).is_empty(),
            "no explicit category policy means AI may not add packet kinds"
        );
        let disabled_policy = WorkQueuePolicy {
            create_work_item: false,
            ..policy
        };
        assert!(
            ai_suggestible_kinds_for_policy(&enabled, Some(&disabled_policy)).is_empty(),
            "disabled category policy means AI may not add packet kinds"
        );
    }

    #[test]
    fn suggestible_kinds_sentinel_offers_every_enabled_kind() {
        let enabled = ai_suggestible_kinds_for_enabled(
            crate::slices::work_queue::packet_kind_catalog(),
            |_| true,
        );
        let policy = WorkQueuePolicy {
            category_id: FALLBACK_CATEGORY_ID.to_string(),
            create_work_item: true,
            packet_kinds: vec![],
            ai_suggestible_packet_kinds: vec![
                bos_contracts::work_queue::AI_SUGGEST_ALL_SENTINEL.to_string()
            ],
            ai_suggestible_gmail_scope: Default::default(),
            ai_suggestible_gmail_categories: Vec::new(),
            auto_produce: false,
        };
        let offered: Vec<String> = ai_suggestible_kinds_for_policy(&enabled, Some(&policy))
            .into_iter()
            .map(|kind| kind.kind_id)
            .collect();
        let all: Vec<String> = enabled.iter().map(|k| k.kind_id.clone()).collect();
        assert_eq!(
            offered, all,
            "the all-or-nothing sentinel offers the AI every enabled kind"
        );
    }

    #[test]
    fn ai_suggestion_batch_includes_ai_only_and_deterministic_categorized_messages() {
        use crate::slices::work_queue::store as wq_store;

        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();

        let upsert_policy = |conn: &mut rusqlite::Connection,
                             category_id: &str,
                             packet_kinds: Vec<String>,
                             ai_suggestible_packet_kinds: Vec<String>| {
            wq_store::upsert_policy(
                conn,
                "test-client",
                "op_test",
                &WorkQueuePolicy {
                    category_id: category_id.to_string(),
                    create_work_item: true,
                    packet_kinds,
                    ai_suggestible_packet_kinds,
                    ai_suggestible_gmail_scope: Default::default(),
                    ai_suggestible_gmail_categories: Vec::new(),
                    auto_produce: false,
                },
                &format!("policy_{category_id}"),
                1_000,
            )
            .expect("policy");
        };

        upsert_policy(
            conn,
            "example_service",
            Vec::new(),
            vec![bos_contracts::work_queue::AI_SUGGEST_ALL_SENTINEL.to_string()],
        );
        upsert_policy(conn, "plain", Vec::new(), Vec::new());
        upsert_policy(
            conn,
            "deterministic",
            vec!["follow_up_task".to_string()],
            vec![bos_contracts::work_queue::AI_SUGGEST_ALL_SENTINEL.to_string()],
        );

        let mut ai_only = inbound_message("ai-only", None);
        ai_only.resolved_category = "example_service".to_string();
        ai_only.matched_rule_id = Some("demo-rule".to_string());
        store::record_inbound_message(conn, "test-client", &ai_only).expect("ai-only message");

        let mut no_ai = inbound_message("no-ai", None);
        no_ai.resolved_category = "plain".to_string();
        no_ai.matched_rule_id = Some("plain-rule".to_string());
        store::record_inbound_message(conn, "test-client", &no_ai).expect("no-ai message");

        let mut deterministic = inbound_message("deterministic", None);
        deterministic.resolved_category = "deterministic".to_string();
        deterministic.matched_rule_id = Some("det-rule".to_string());
        store::record_inbound_message(conn, "test-client", &deterministic)
            .expect("deterministic message");
        crate::slices::work_queue::service::emit_for_inbound_message(
            conn,
            "test-client",
            &deterministic,
            2_000,
        )
        .expect("deterministic emit");

        let batch =
            store::list_unexamined_ai_suggestible(persistence.connection_ref(), "test-client", 10)
                .expect("batch");
        let ids: Vec<String> = batch
            .into_iter()
            .map(|message| message.message_id)
            .collect();
        assert_eq!(
            ids,
            vec!["ai-only".to_string(), "deterministic".to_string()],
            "AI-only and deterministic categorized messages are examined; no-AI messages stay quiet"
        );
    }

    #[test]
    fn fallback_policy_gmail_scope_limits_ai_suggestion_batch() {
        use crate::slices::work_queue::store as wq_store;
        use bos_contracts::email_triage::EmailTriageGmailCategory;

        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        store::list_categories(conn, "test-client", 1_000).expect("seed categories");
        wq_store::upsert_policy(
            conn,
            "test-client",
            "op_test",
            &WorkQueuePolicy {
                category_id: FALLBACK_CATEGORY_ID.to_string(),
                create_work_item: true,
                packet_kinds: Vec::new(),
                ai_suggestible_packet_kinds: vec!["follow_up_task".to_string()],
                ai_suggestible_gmail_scope: Default::default(),
                ai_suggestible_gmail_categories: vec![
                    EmailTriageGmailCategory::Primary,
                    EmailTriageGmailCategory::Updates,
                ],
                auto_produce: false,
            },
            "policy_primary_updates",
            1_001,
        )
        .expect("policy");

        for (id, label) in [
            ("primary", "CATEGORY_PERSONAL"),
            ("updates", "CATEGORY_UPDATES"),
            ("promotions", "CATEGORY_PROMOTIONS"),
            ("social", "CATEGORY_SOCIAL"),
            ("forums", "CATEGORY_FORUMS"),
            ("plain", "INBOX"),
            ("trash-updates", "CATEGORY_UPDATES"),
            ("spam-primary", "CATEGORY_PERSONAL"),
        ] {
            let mut message = inbound_message(id, None);
            message.labels = vec!["INBOX".to_string(), label.to_string()];
            if id == "trash-updates" {
                message.labels.push("TRASH".to_string());
            }
            if id == "spam-primary" {
                message.labels.push("SPAM".to_string());
            }
            store::record_inbound_message(conn, "test-client", &message).expect("message");
        }
        let mut rule_matched_promotions = inbound_message("rule-matched-promotions", None);
        rule_matched_promotions.labels =
            vec!["INBOX".to_string(), "CATEGORY_PROMOTIONS".to_string()];
        rule_matched_promotions.matched_rule_id = Some("fallback-pin-rule".to_string());
        store::record_inbound_message(conn, "test-client", &rule_matched_promotions)
            .expect("rule matched message");

        let batch =
            store::list_unexamined_ai_suggestible(persistence.connection_ref(), "test-client", 10)
                .expect("batch");
        let ids: Vec<String> = batch
            .into_iter()
            .map(|message| message.message_id)
            .collect();
        assert_eq!(
            ids,
            vec![
                "primary".to_string(),
                "rule-matched-promotions".to_string(),
                "updates".to_string()
            ]
        );
    }

    #[test]
    fn fallback_policy_all_gmail_scope_includes_uncategorized_mail() {
        use crate::slices::work_queue::store as wq_store;
        use bos_contracts::work_queue::WorkQueueAiGmailScope;

        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        store::list_categories(conn, "test-client", 1_000).expect("seed categories");
        wq_store::upsert_policy(
            conn,
            "test-client",
            "op_test",
            &WorkQueuePolicy {
                category_id: FALLBACK_CATEGORY_ID.to_string(),
                create_work_item: true,
                packet_kinds: Vec::new(),
                ai_suggestible_packet_kinds: vec!["follow_up_task".to_string()],
                ai_suggestible_gmail_scope: WorkQueueAiGmailScope::All,
                ai_suggestible_gmail_categories: Vec::new(),
                auto_produce: false,
            },
            "policy_all_tabs",
            1_001,
        )
        .expect("policy");

        for (id, labels) in [
            (
                "primary",
                vec!["INBOX".to_string(), "CATEGORY_PERSONAL".to_string()],
            ),
            (
                "promotions",
                vec!["INBOX".to_string(), "CATEGORY_PROMOTIONS".to_string()],
            ),
            ("plain", vec!["INBOX".to_string()]),
            (
                "trash",
                vec!["TRASH".to_string(), "CATEGORY_UPDATES".to_string()],
            ),
            (
                "spam",
                vec!["SPAM".to_string(), "CATEGORY_PERSONAL".to_string()],
            ),
        ] {
            let mut message = inbound_message(id, None);
            message.labels = labels;
            store::record_inbound_message(conn, "test-client", &message).expect("message");
        }

        let batch =
            store::list_unexamined_ai_suggestible(persistence.connection_ref(), "test-client", 10)
                .expect("batch");
        let mut ids: Vec<String> = batch
            .into_iter()
            .map(|message| message.message_id)
            .collect();
        ids.sort();
        assert_eq!(
            ids,
            vec![
                "plain".to_string(),
                "primary".to_string(),
                "promotions".to_string()
            ]
        );
    }

    #[test]
    fn untriaged_query_result_write_and_ai_item_emission() {
        use crate::slices::work_queue::store as wq_store;

        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        store::list_categories(conn, "test-client", 100).expect("seed");

        crate::slices::email_triage::worker::ingest_messages(
            conn,
            "test-client",
            None,
            &[bos_integrations::gmail_inbox_read::GmailFullMessage {
                message_id: "g1".to_string(),
                thread_id: None,
                label_ids: vec![],
                internal_date_epoch_ms: Some(500),
                subject: Some("Registration approved for Launch 'n Lunch".to_string()),
                from: Some("grace@business-d5659c6e10.example.test".to_string()),
                to: None,
                headers: vec![],
                plain_text_body: "Thursday July 18, 12pm, Charleston".to_string(),
                html_body: None,
                attachments: Vec::new(),
            }],
            1_000,
        )
        .expect("ingest");

        // Fallback + rule-less + never examined -> in the untriaged batch.
        let batch = store::list_untriaged_fallback(
            persistence.connection_ref(),
            "test-client",
            FALLBACK_CATEGORY_ID,
            10,
        )
        .expect("batch");
        assert_eq!(batch.len(), 1);

        // Record a suggestion + emit the AI item.
        let conn = persistence.connection();
        store::set_ai_triage_result(
            conn,
            "test-client",
            "g1",
            "suggested",
            Some("Dated event registration."),
            Some("{}".to_string()),
            2_000,
        )
        .expect("result");
        let emitted = crate::slices::work_queue::service::emit_ai_suggested_item(
            conn,
            "test-client",
            &batch[0],
            &crate::overlay::WorkQueueOverlay::default(),
            vec!["calendar_event_draft".to_string()],
            "Dated event registration.",
            2_000,
        )
        .expect("emit");
        assert!(emitted);

        // Examined messages leave the batch; the item is flagged + rationaled.
        assert!(store::list_untriaged_fallback(
            persistence.connection_ref(),
            "test-client",
            FALLBACK_CATEGORY_ID,
            10,
        )
        .expect("batch")
        .is_empty());
        let items = wq_store::list_items(
            persistence.connection_ref(),
            "test-client",
            None,
            10,
            &OperatorScope::All,
        )
        .expect("items");
        assert_eq!(items.len(), 1);
        assert!(items[0].item.ai_suggested);
        assert_eq!(items[0].item.rationale, "Dated event registration.");
        assert_eq!(
            items[0].item.packet_kinds,
            vec!["calendar_event_draft".to_string()]
        );

        // AI suggestions append missing packet kinds to the existing item.
        let again = crate::slices::work_queue::service::emit_ai_suggested_item(
            persistence.connection(),
            "test-client",
            &batch[0],
            &crate::overlay::WorkQueueOverlay::default(),
            vec!["follow_up_task".to_string()],
            "dup",
            3_000,
        )
        .expect("re-emit");
        assert!(again);
        let items = wq_store::list_items(
            persistence.connection_ref(),
            "test-client",
            None,
            10,
            &OperatorScope::All,
        )
        .expect("items");
        assert_eq!(
            items[0].item.packet_kinds,
            vec![
                "calendar_event_draft".to_string(),
                "follow_up_task".to_string()
            ]
        );

        // A suggestion that adds no new kinds stays receipt-quiet.
        let duplicate = crate::slices::work_queue::service::emit_ai_suggested_item(
            persistence.connection(),
            "test-client",
            &batch[0],
            &crate::overlay::WorkQueueOverlay::default(),
            vec!["follow_up_task".to_string()],
            "dup",
            4_000,
        )
        .expect("duplicate");
        assert!(!duplicate);
    }

    #[test]
    fn ai_triage_packet_proposals_flag_uses_unified_runner_and_stages_draft() {
        use crate::slices::packet_proposals::service as packet_proposals;
        use crate::slices::work_queue::{service as wq_service, store as wq_store};
        use bos_contracts::work_queue::{WorkItemAcceptActor, WorkItemStatus};

        let _guard = packet_proposals::test_packet_proposal_lock();
        packet_proposals::reset_test_packet_proposal_state();
        let state = crate::http::test_support::test_state();
        {
            let mut persistence = state.persistence.lock();
            let conn = persistence.connection();
            set_runtime_override(
                conn,
                &crate::env_registry::BOS_AI_TRIAGE_ENABLED,
                "1",
                "enable_ai_triage_unified",
            );
            set_runtime_override(
                conn,
                &crate::env_registry::BOS_AI_TRIAGE_PACKET_PROPOSALS_ENABLED,
                "1",
                "enable_ai_triage_packet_proposals_unified",
            );
            assert!(crate::slices::admin_settings::service::flag(
                conn,
                "test-client",
                &crate::env_registry::BOS_AI_TRIAGE_ENABLED,
            )
            .expect("ai triage enabled override"));
            assert!(crate::slices::admin_settings::service::flag(
                conn,
                "test-client",
                &crate::env_registry::BOS_AI_TRIAGE_PACKET_PROPOSALS_ENABLED,
            )
            .expect("packet proposal enabled override"));
            store::list_categories(conn, "test-client", 100).expect("seed categories");
            wq_store::upsert_policy(
                conn,
                "test-client",
                "op_test",
                &WorkQueuePolicy {
                    category_id: FALLBACK_CATEGORY_ID.to_string(),
                    create_work_item: true,
                    packet_kinds: Vec::new(),
                    ai_suggestible_packet_kinds: vec!["email_draft_reply".to_string()],
                    ai_suggestible_gmail_scope: Default::default(),
                    ai_suggestible_gmail_categories: Vec::new(),
                    auto_produce: false,
                },
                "policy_unified_ai_triage",
                1_000,
            )
            .expect("policy");
            crate::slices::client_profile::store::upsert_profile(
                conn,
                "test-client",
                "op_test",
                &bos_contracts::client_profile::ClientProfile {
                    client_id: "test-client".to_string(),
                    company_name: Some("Example Service".to_string()),
                    bio: Some("appliance repair and maintenance service.".to_string()),
                    industry: Some("repair services".to_string()),
                    website: None,
                    persona: Some("Concise and practical".to_string()),
                },
                "profile_unified_ai_triage",
                1_001,
            )
            .expect("profile");
            let mut message = inbound_message("unified-ai-triage", None);
            message.subject = Some("Can you send a quote?".to_string());
            message.body_excerpt = "Please reply with next steps for a haul-out quote.".to_string();
            message.body_full = "Please reply with next steps for a haul-out quote.".to_string();
            message.labels = vec!["INBOX".to_string(), "CATEGORY_PERSONAL".to_string()];
            store::record_inbound_message(conn, "test-client", &message).expect("message");
            assert_eq!(
                store::list_unexamined_ai_suggestible(conn, "test-client", 10)
                    .expect("ai batch")
                    .len(),
                1
            );
        }

        packet_proposals::set_test_packet_proposal_response(json!({
            "suggested_category": null,
            "rationale": "The sender expects a reply.",
            "outcomes": [{
                "packet_kind": "email_draft_reply",
                "status": "drafted",
                "draft": {
                    "body_text": "Thanks for reaching out. Could you send the haul-out date?",
                    "confidence": "high",
                    "provenance": [{ "field": "body_text", "quote": "Please reply" }]
                }
            }]
        }));

        crate::slices::email_triage::worker::run_ai_triage_pass(&state);

        let requests = packet_proposals::take_test_packet_proposal_requests();
        assert_eq!(
            requests.len(),
            1,
            "the unified branch makes one proposal call"
        );
        assert_eq!(
            requests[0].spec.schema_ref,
            packet_proposals::PROPOSAL_SCHEMA_REF
        );

        let persistence = state.persistence.lock();
        let conn = persistence.connection_ref();
        let status: String = conn
            .query_row(
                "SELECT ai_triage_status \
                 FROM email_inbound_messages \
                 WHERE client_id = 'test-client' AND source_key = 'unified-ai-triage'",
                [],
                |row| row.get(0),
            )
            .expect("ai triage result");
        assert_eq!(status, "suggested");
        let payload: String = conn
            .query_row(
                "SELECT after_json FROM receipts \
                 WHERE client_id = 'test-client' \
                   AND entity_kind = ?1 \
                   AND entity_id = 'unified-ai-triage' \
                   AND change_kind = 'ai_triage' \
                   AND outcome = 'applied' \
                 ORDER BY created_at_ms DESC, receipt_id DESC LIMIT 1",
                [store::INBOUND_ENTITY_KIND],
                |row| row.get(0),
            )
            .expect("ai triage receipt payload");
        let payload: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert_eq!(
            payload["suggested_packet_kinds"],
            json!(["email_draft_reply"])
        );
        assert_eq!(payload["confidence"], json!("high"));
        assert!(payload["packet_proposal_run_id"]
            .as_str()
            .is_some_and(|run_id| run_id.starts_with("ppr_")));

        let in_flight = std::collections::HashSet::new();
        let feed = wq_service::feed(
            conn,
            "test-client",
            None,
            10,
            &OperatorScope::All,
            wq_service::FeedOptions {
                now_ms: 5_000,
                auto_produce_running: false,
                debug_enabled: false,
                in_flight: &in_flight,
            },
        )
        .expect("queue feed");
        assert_eq!(feed.len(), 1);
        assert_eq!(feed[0].item.status, WorkItemStatus::Accepted);
        assert_eq!(feed[0].item.accept_actor, Some(WorkItemAcceptActor::System));
        assert_eq!(
            feed[0].staged_draft_kinds,
            vec!["email_draft_reply".to_string()]
        );
    }

    #[test]
    fn unified_packet_proposals_respects_ai_triage_min_confidence() {
        use crate::slices::packet_proposals::service as packet_proposals;
        use crate::slices::work_queue::store as wq_store;

        let _guard = packet_proposals::test_packet_proposal_lock();
        packet_proposals::reset_test_packet_proposal_state();
        let state = crate::http::test_support::test_state();
        {
            let mut persistence = state.persistence.lock();
            let conn = persistence.connection();
            set_runtime_override(
                conn,
                &crate::env_registry::BOS_AI_TRIAGE_ENABLED,
                "1",
                "enable_ai_triage_low_confidence",
            );
            set_runtime_override(
                conn,
                &crate::env_registry::BOS_AI_TRIAGE_PACKET_PROPOSALS_ENABLED,
                "1",
                "enable_ai_triage_packet_proposals_low_confidence",
            );
            set_runtime_override(
                conn,
                &crate::env_registry::BOS_AI_TRIAGE_MIN_CONFIDENCE,
                "high",
                "set_ai_triage_min_confidence_high",
            );
            assert!(crate::slices::admin_settings::service::flag(
                conn,
                "test-client",
                &crate::env_registry::BOS_AI_TRIAGE_ENABLED,
            )
            .expect("ai triage enabled override"));
            assert!(crate::slices::admin_settings::service::flag(
                conn,
                "test-client",
                &crate::env_registry::BOS_AI_TRIAGE_PACKET_PROPOSALS_ENABLED,
            )
            .expect("packet proposal enabled override"));
            assert_eq!(
                crate::slices::admin_settings::service::value(
                    conn,
                    "test-client",
                    &crate::env_registry::BOS_AI_TRIAGE_MIN_CONFIDENCE,
                )
                .expect("min confidence override")
                .as_deref(),
                Some("high")
            );
            store::list_categories(conn, "test-client", 100).expect("seed categories");
            wq_store::upsert_policy(
                conn,
                "test-client",
                "op_test",
                &WorkQueuePolicy {
                    category_id: FALLBACK_CATEGORY_ID.to_string(),
                    create_work_item: true,
                    packet_kinds: Vec::new(),
                    ai_suggestible_packet_kinds: vec!["email_draft_reply".to_string()],
                    ai_suggestible_gmail_scope: Default::default(),
                    ai_suggestible_gmail_categories: Vec::new(),
                    auto_produce: false,
                },
                "policy_unified_ai_triage_low_confidence",
                1_000,
            )
            .expect("policy");
            crate::slices::client_profile::store::upsert_profile(
                conn,
                "test-client",
                "op_test",
                &bos_contracts::client_profile::ClientProfile {
                    client_id: "test-client".to_string(),
                    company_name: Some("Example Service".to_string()),
                    bio: Some("appliance repair and maintenance service.".to_string()),
                    industry: Some("repair services".to_string()),
                    website: None,
                    persona: Some("Concise and practical".to_string()),
                },
                "profile_unified_ai_triage_low_confidence",
                1_001,
            )
            .expect("profile");
            let mut message = inbound_message("unified-ai-triage-low-confidence", None);
            message.subject = Some("Maybe send a quote?".to_string());
            message.body_excerpt = "Maybe reply if this is relevant.".to_string();
            message.body_full = "Maybe reply if this is relevant.".to_string();
            message.labels = vec!["INBOX".to_string(), "CATEGORY_PERSONAL".to_string()];
            store::record_inbound_message(conn, "test-client", &message).expect("message");
            assert_eq!(
                store::list_unexamined_ai_suggestible(conn, "test-client", 10)
                    .expect("ai batch")
                    .len(),
                1
            );
        }

        packet_proposals::set_test_packet_proposal_response(json!({
            "suggested_category": null,
            "confidence": "medium",
            "rationale": "The sender may expect a reply.",
            "outcomes": [{
                "packet_kind": "email_draft_reply",
                "status": "drafted",
                "draft": {
                    "body_text": "Thanks for reaching out. Could you send more detail?",
                    "confidence": "medium",
                    "provenance": [{ "field": "body_text", "quote": "Maybe reply" }]
                }
            }]
        }));

        crate::slices::email_triage::worker::run_ai_triage_pass(&state);

        assert_eq!(
            packet_proposals::take_test_packet_proposal_requests().len(),
            1
        );
        let persistence = state.persistence.lock();
        let conn = persistence.connection_ref();
        let status: String = conn
            .query_row(
                "SELECT ai_triage_status \
                 FROM email_inbound_messages \
                 WHERE client_id = 'test-client' \
                   AND source_key = 'unified-ai-triage-low-confidence'",
                [],
                |row| row.get(0),
            )
            .expect("ai triage result");
        assert_eq!(status, "no_suggestion");
        let payload: String = conn
            .query_row(
                "SELECT after_json FROM receipts \
                 WHERE client_id = 'test-client' \
                   AND entity_kind = ?1 \
                   AND entity_id = 'unified-ai-triage-low-confidence' \
                   AND change_kind = 'ai_triage' \
                   AND outcome = 'applied' \
                 ORDER BY created_at_ms DESC, receipt_id DESC LIMIT 1",
                [store::INBOUND_ENTITY_KIND],
                |row| row.get(0),
            )
            .expect("ai triage receipt payload");
        let payload: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert_eq!(payload["suggested_packet_kinds"], json!([]));
        assert_eq!(payload["confidence"], json!("medium"));
        assert_eq!(payload["actionable"], json!(false));
        let items = wq_store::list_items(conn, "test-client", None, 10, &OperatorScope::All)
            .expect("items");
        assert!(items.is_empty());
    }

    #[test]
    fn ai_suggestion_appends_to_existing_deterministic_item() {
        use crate::slices::work_queue::store as wq_store;

        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        wq_store::upsert_policy(
            conn,
            "test-client",
            "op_test",
            &WorkQueuePolicy {
                category_id: "mixed".to_string(),
                create_work_item: true,
                packet_kinds: vec!["follow_up_task".to_string()],
                ai_suggestible_packet_kinds: vec![
                    bos_contracts::work_queue::AI_SUGGEST_ALL_SENTINEL.to_string(),
                ],
                ai_suggestible_gmail_scope: Default::default(),
                ai_suggestible_gmail_categories: Vec::new(),
                auto_produce: false,
            },
            "policy_mixed",
            1_000,
        )
        .expect("policy");
        let mut message = inbound_message("mixed-message", None);
        message.resolved_category = "mixed".to_string();
        message.matched_rule_id = Some("mixed-rule".to_string());
        store::record_inbound_message(conn, "test-client", &message).expect("message");
        assert!(
            crate::slices::work_queue::service::emit_for_inbound_message(
                conn,
                "test-client",
                &message,
                2_000,
            )
            .expect("deterministic emit")
        );

        let appended = crate::slices::work_queue::service::emit_ai_suggested_item(
            conn,
            "test-client",
            &message,
            &crate::overlay::WorkQueueOverlay::default(),
            vec![
                "follow_up_task".to_string(),
                "email_draft_reply".to_string(),
            ],
            "Needs a drafted reply too.",
            3_000,
        )
        .expect("ai append");
        assert!(appended);

        let items = wq_store::list_items(
            persistence.connection_ref(),
            "test-client",
            None,
            10,
            &OperatorScope::All,
        )
        .expect("items");
        assert_eq!(items.len(), 1, "one item per source remains intact");
        assert!(items[0].item.ai_suggested);
        assert_eq!(items[0].item.rationale, "Needs a drafted reply too.");
        assert_eq!(
            items[0].item.packet_kinds,
            vec![
                "follow_up_task".to_string(),
                "email_draft_reply".to_string()
            ]
        );
    }

    #[test]
    fn ai_suggested_shared_inbox_item_is_visible_to_configured_users() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        let mut shared = std::collections::BTreeMap::new();
        shared.insert(
            "ask".to_string(),
            crate::overlay::SharedInboxOverlay {
                match_to: vec!["ask@business-914f630770.example.test".to_string()],
                visible_to_user_ids: vec!["user_jordan".to_string(), "user_casey".to_string()],
            },
        );
        let overlay = crate::overlay::WorkQueueOverlay {
            shared_inboxes: shared,
        };
        let message = InboundMessageRecord {
            source_key: "ai-shared".to_string(),
            message_id: "ai-shared".to_string(),
            thread_id: None,
            internal_date_ms: Some(1_000),
            from_addr: Some("buyer@example.test".to_string()),
            to_addr: Some("example Info <ask@business-914f630770.example.test>".to_string()),
            subject: Some("Need a follow up".to_string()),
            body_excerpt: "Please follow up.".to_string(),
            body_full: String::new(),
            headers: Vec::new(),
            labels: Vec::new(),
            resolved_category: FALLBACK_CATEGORY_ID.to_string(),
            matched_rule_id: None,
            ingested_at_ms: 1_000,
            ai_triage_status: None,
            ai_triage_rationale: None,
            attachments: Vec::new(),
            source_user_id: Some("source_mailbox".to_string()),
        };

        let emitted = crate::slices::work_queue::service::emit_ai_suggested_item(
            conn,
            "test-client",
            &message,
            &overlay,
            vec!["follow_up_task".to_string()],
            "Needs a follow-up.",
            2_000,
        )
        .expect("emit");
        assert!(emitted);

        for user_id in ["user_jordan", "user_casey"] {
            let items = crate::slices::work_queue::store::list_items(
                persistence.connection_ref(),
                "test-client",
                None,
                10,
                &OperatorScope::User(user_id.to_string()),
            )
            .expect("list");
            assert_eq!(items.len(), 1, "{user_id} should see AI item");
            assert!(items[0].item.ai_suggested);
            assert_eq!(
                items[0].item.visible_to_user_ids,
                vec!["user_casey".to_string(), "user_jordan".to_string()]
            );
        }
        let third = crate::slices::work_queue::store::list_items(
            persistence.connection_ref(),
            "test-client",
            None,
            10,
            &OperatorScope::User("third".to_string()),
        )
        .expect("third list");
        assert!(third.is_empty());
    }

    fn ingest_fallback_message(conn: &mut rusqlite::Connection, id: &str, now_ms: u64) {
        crate::slices::email_triage::worker::ingest_messages(
            conn,
            "test-client",
            None,
            &[bos_integrations::gmail_inbox_read::GmailFullMessage {
                message_id: id.to_string(),
                thread_id: None,
                label_ids: vec![],
                internal_date_epoch_ms: Some(500),
                subject: Some(format!("Message {id}")),
                from: Some("someone@example.test".to_string()),
                to: None,
                headers: vec![],
                plain_text_body: "No rules match this.".to_string(),
                html_body: None,
                attachments: Vec::new(),
            }],
            now_ms,
        )
        .expect("ingest");
    }

    fn triage_status(conn: &rusqlite::Connection, id: &str) -> Option<String> {
        conn.query_row(
            "SELECT ai_triage_status FROM email_inbound_messages \
             WHERE client_id = 'test-client' AND message_id = ?1",
            [id],
            |row| row.get(0),
        )
        .expect("row")
    }

    #[test]
    fn reset_bumps_generation_so_a_new_verdict_lands() {
        use crate::slices::work_queue::store as wq_store;

        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        wq_store::upsert_policy(
            conn,
            "test-client",
            "op_test",
            &WorkQueuePolicy {
                category_id: FALLBACK_CATEGORY_ID.to_string(),
                create_work_item: true,
                packet_kinds: Vec::new(),
                ai_suggestible_packet_kinds: vec!["follow_up_task".to_string()],
                ai_suggestible_gmail_scope: bos_contracts::work_queue::WorkQueueAiGmailScope::All,
                ai_suggestible_gmail_categories: Vec::new(),
                auto_produce: false,
            },
            "policy_fallback",
            1_500,
        )
        .expect("policy");
        ingest_fallback_message(conn, "r1", 1_000);
        store::set_ai_triage_result(
            conn,
            "test-client",
            "r1",
            "no_suggestion",
            None,
            None,
            2_000,
        )
        .expect("first verdict");
        assert_eq!(triage_status(conn, "r1").as_deref(), Some("no_suggestion"));

        let reset = store::reset_ai_triage(
            conn,
            "test-client",
            "op_test",
            &store::AiRetriageScope::All,
            "reset_1",
            3_000,
        )
        .expect("reset");
        assert_eq!(reset, 1);
        assert_eq!(triage_status(conn, "r1"), None, "verdict cleared");

        // Back in the untriaged batch...
        let batch = store::list_untriaged_fallback(
            persistence.connection_ref(),
            "test-client",
            FALLBACK_CATEGORY_ID,
            10,
        )
        .expect("batch");
        assert_eq!(batch.len(), 1, "reset message is re-examinable");

        // ...and the SECOND verdict must actually apply (generation re-keys
        // the idempotency; without the bump this would replay quietly).
        let conn = persistence.connection();
        store::set_ai_triage_result(
            conn,
            "test-client",
            "r1",
            "suggested",
            Some("now actionable"),
            None,
            4_000,
        )
        .expect("second verdict");
        assert_eq!(triage_status(conn, "r1").as_deref(), Some("suggested"));
    }

    #[test]
    fn stale_scope_resets_pre_category_change_verdicts_and_errors_only() {
        use crate::slices::work_queue::store as wq_store;
        use bos_contracts::work_queue::WorkQueuePolicy;

        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        store::list_categories(conn, "test-client", 100).expect("seed");
        ingest_fallback_message(conn, "old", 1_000);
        ingest_fallback_message(conn, "new", 1_000);
        ingest_fallback_message(conn, "policy_old", 1_000);
        ingest_fallback_message(conn, "errored", 1_000);
        let mut categorized = inbound_message("categorized_old", None);
        categorized.resolved_category = "categorized_ai".to_string();
        categorized.matched_rule_id = Some("categorized-rule".to_string());
        store::record_inbound_message(conn, "test-client", &categorized)
            .expect("categorized message");
        store::set_ai_triage_result(
            conn,
            "test-client",
            "old",
            "no_suggestion",
            None,
            None,
            2_000,
        )
        .expect("verdict");
        store::set_ai_triage_result(
            conn,
            "test-client",
            "policy_old",
            "no_suggestion",
            None,
            None,
            2_200,
        )
        .expect("policy-gated verdict");
        store::set_ai_triage_result(conn, "test-client", "errored", "error", None, None, 2_500)
            .expect("verdict");
        store::set_ai_triage_result(
            conn,
            "test-client",
            "categorized_old",
            "no_suggestion",
            None,
            None,
            2_700,
        )
        .expect("categorized verdict");

        // Category catalog changes at t=5000 (newer than "old"'s verdict).
        store::upsert_category(
            conn,
            "test-client",
            "op_test",
            &CategoryRecord {
                category_id: "events".to_string(),
                display_name: "Events".to_string(),
                description: "Dated events".to_string(),
                color: "#22c55e".to_string(),
                sort: 50,
                is_system: false,
                default_agent_dir: String::new(),
                default_agent_context: String::new(),
            },
            "cat_1",
            5_000,
        )
        .expect("category");
        // AI-suggestible packet policy also changes the prompt catalog and
        // must stale old no_suggestion verdicts.
        wq_store::upsert_policy(
            conn,
            "test-client",
            "op_test",
            &WorkQueuePolicy {
                category_id: FALLBACK_CATEGORY_ID.to_string(),
                create_work_item: true,
                packet_kinds: Vec::new(),
                ai_suggestible_packet_kinds: vec!["follow_up_task".to_string()],
                ai_suggestible_gmail_scope: bos_contracts::work_queue::WorkQueueAiGmailScope::All,
                ai_suggestible_gmail_categories: Vec::new(),
                auto_produce: false,
            },
            "policy_stale",
            5_500,
        )
        .expect("policy");
        wq_store::upsert_policy(
            conn,
            "test-client",
            "op_test",
            &WorkQueuePolicy {
                category_id: "categorized_ai".to_string(),
                create_work_item: true,
                packet_kinds: Vec::new(),
                ai_suggestible_packet_kinds: vec!["email_draft_reply".to_string()],
                ai_suggestible_gmail_scope: Default::default(),
                ai_suggestible_gmail_categories: Vec::new(),
                auto_produce: false,
            },
            "policy_categorized_stale",
            5_600,
        )
        .expect("categorized policy");

        // "new" gets its verdict AFTER the category change.
        store::set_ai_triage_result(
            conn,
            "test-client",
            "new",
            "no_suggestion",
            None,
            None,
            6_500,
        )
        .expect("verdict");

        let reset = store::reset_ai_triage(
            conn,
            "test-client",
            "op_test",
            &store::AiRetriageScope::Stale,
            "reset_stale",
            7_000,
        )
        .expect("reset");
        assert_eq!(
            reset, 4,
            "old no_suggestion + policy-gated no_suggestion + error + categorized no_suggestion reset; fresh verdict kept"
        );
        assert_eq!(triage_status(conn, "old"), None);
        assert_eq!(triage_status(conn, "policy_old"), None);
        assert_eq!(triage_status(conn, "errored"), None);
        assert_eq!(triage_status(conn, "categorized_old"), None);
        assert_eq!(triage_status(conn, "new").as_deref(), Some("no_suggestion"));
    }

    #[test]
    fn message_scope_resets_one_and_noops_on_unexamined() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        ingest_fallback_message(conn, "m_one", 1_000);
        ingest_fallback_message(conn, "m_two", 1_000);
        store::set_ai_triage_result(
            conn,
            "test-client",
            "m_one",
            "no_suggestion",
            None,
            None,
            2_000,
        )
        .expect("verdict");

        let untouched = store::reset_ai_triage(
            conn,
            "test-client",
            "op_test",
            &store::AiRetriageScope::Message("m_two".to_string()),
            "reset_m2",
            3_000,
        )
        .expect("reset");
        assert_eq!(untouched, 0, "unexamined message is a no-op");

        let reset = store::reset_ai_triage(
            conn,
            "test-client",
            "op_test",
            &store::AiRetriageScope::Message("m_one".to_string()),
            "reset_m1",
            3_500,
        )
        .expect("reset");
        assert_eq!(reset, 1);
        assert_eq!(triage_status(conn, "m_one"), None);
    }
}
