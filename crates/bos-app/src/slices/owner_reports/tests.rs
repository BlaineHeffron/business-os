//! Slice tests: deterministic period math, metric assembly over seeded
//! local caches (no provider, no network), the dollar-grounding rule on the
//! narration transform (mocked — never a live LLM), idempotent regeneration,
//! the pump's staleness skip, and the email-staging lifecycle.

use std::cell::RefCell;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use bos_contracts::claim_drafts::{ClaimDraft, ClaimDraftStatus, ClaimEvidence, ClaimPacketGate};
use bos_contracts::client_profile::ClientProfile;
use bos_contracts::email_identity::{AttentionLevel, AttentionSignal, ParsedInbound};
use bos_contracts::email_triage::InboundMessageRecord;
use bos_contracts::follow_up_tasks::TaskRecord;
use bos_contracts::home_dashboard::{
    HubSpotDealMappedStatus, HubSpotDealPipelineMapping, HubSpotDealPipelineMappingSaveRequest,
    HubSpotDealStageMapping,
};
use bos_contracts::operator_users::OperatorUser;
use bos_contracts::owner_reports::{
    DigestCallMetrics, DigestClaimMetrics, DigestDealMetrics, DigestDealMetricsStatus,
    DigestFollowUpMetrics, DigestInventoryMetrics, DigestOrderMetrics, DigestSalesMetrics,
    DigestTrafficMetrics, OwnerDigestMetrics, OwnerReportPeriodKind, OwnerReportStatus,
};
use bos_contracts::receipt::ActorKindDto;
use bos_contracts::search_console::{AnalyticsMetricTotals, SearchConsoleMetricTotals};
use bos_integrations::qbo_oauth::QboTokenGrant;
use bos_integrations::stockforge_read::{
    SfDamageEventRecord, SfMaterialRecord, SfOrderCardRecord, SfPurchaseOrderRecord,
};
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

use super::service::{self, DigestNarration, DigestPeriod, ReportMetricSection, ReportWeekday};
use super::store::{self, EmailActionContext};
use super::worker;
use crate::http::{
    build_router,
    test_support::{test_state, EnvGuard},
    AppState,
};
use crate::overlay::{
    AccountingVisibilityPolicy, CallVolumeMetricOverlay, OwnerReportsOverlay, SearchConsoleOverlay,
};
use crate::slices::accounting;
use crate::slices::search_console::store as search_console_store;
use crate::store_core::MutationOutcome;

const CLIENT: &str = "test-client";
const TODAY: &str = "2026-06-10"; // a Wednesday
const WEEK_START: &str = "2026-06-08";
const MONTH_START: &str = "2026-06-01";

fn ms(date: &str) -> u64 {
    accounting::service::date_to_epoch_ms(date).expect("test date")
}

fn sc_totals(clicks: i64, impressions: i64) -> SearchConsoleMetricTotals {
    SearchConsoleMetricTotals {
        clicks,
        impressions,
        ctr_micros: 100_000,
        position_micros: 1_500_000,
    }
}

fn now_ms() -> u64 {
    ms(TODAY) + 12 * 60 * 60 * 1000
}

fn sample_metrics() -> OwnerDigestMetrics {
    OwnerDigestMetrics {
        metric_sections: service::metric_section_ids(&service::OwnerReportConfig::default()),
        sales: DigestSalesMetrics {
            basis: "quickbooks_pnl".to_string(),
            metric_basis: "gross_margin".to_string(),
            metric_basis_label: "Gross margin".to_string(),
            period_sales_cents: 452_000,
            prior_period_sales_cents: Some(380_050),
            mtd_gross_profit_cents: Some(300_000),
            baseline_monthly_margin_cents: None,
            margin_above_baseline_cents: Some(-123_456),
            metric_value_cents: Some(300_000),
            metric_baseline_cents: Some(423_456),
            metric_above_baseline_cents: Some(-123_456),
            metric_pending_reason: None,
            baseline_months_cached: 3,
            last_synced_at_ms: None,
        },
        calls: DigestCallMetrics {
            call_log_messages: 7,
            transfer_successful: 2,
            callback_needed: 4,
            no_callback_needed: 0,
            unknown_outcome: 1,
            label: "Incoming calls".to_string(),
            source_label:
                "Ruby summary emails; direct calls not summarized by Ruby are not included."
                    .to_string(),
            configured: true,
            pending_reason: None,
        },
        follow_ups: DigestFollowUpMetrics {
            open: 4,
            done_in_period: 2,
            due_today: 1,
            overdue: 2,
            escalated: 1,
            critical: 0,
        },
        orders: DigestOrderMetrics {
            configured: true,
            pending_reason: None,
            orders_in_period: 5,
            exceptions: 1,
            deduction_failed: 0,
            needs_mapping: 2,
            packed_missing_photo: 0,
            blocked: 0,
        },
        inventory: DigestInventoryMetrics {
            configured: true,
            pending_reason: None,
            stocked_sku_count: 12,
            out_of_stock_count: 1,
            critical_count: 2,
            stock_value_cents: 800_000,
            inbound_open_po_cents: 250_000,
        },
        claims: DigestClaimMetrics {
            configured: true,
            pending_reason: None,
            damage_events_in_period: 1,
            damage_open: 1,
            damage_resolved: 0,
            damage_by_severity: vec![],
            damage_by_status: vec![],
            damage_by_type: vec![],
            queue_open: 1,
            queue_accepted: 0,
            queue_dismissed: 0,
            claims_drafted_in_period: 1,
            claims_approved_in_period: 0,
            claim_drafts_by_status: vec![],
        },
        traffic: DigestTrafficMetrics {
            configured: false,
            property_url: None,
            has_data: false,
            last_synced_at_ms: None,
            totals: SearchConsoleMetricTotals::default(),
            branded: SearchConsoleMetricTotals::default(),
            nonbranded: SearchConsoleMetricTotals::default(),
            behavior_configured: false,
            behavior_pending_reason: Some(
                "GA4 behavior/acquisition data is not configured in BusinessOS yet.".to_string(),
            ),
            conversion_tracking_configured: false,
            conversion_tracking_pending_reason: Some(
                "GA4 conversion events are not configured in BusinessOS yet.".to_string(),
            ),
            retargeting_configured: false,
            retargeting_pending_reason: Some(
                "Retargeting pixel/audience setup is outside BusinessOS writes until separately designed."
                    .to_string(),
            ),
            behavior_has_data: false,
            behavior_week: AnalyticsMetricTotals::default(),
            behavior_month_to_date: AnalyticsMetricTotals::default(),
            top_landing_pages_week: Vec::new(),
            top_sources_week: Vec::new(),
        },
        deals: DigestDealMetrics::default(),
    }
}

fn call_volume_config() -> service::CallVolumeMetricConfig {
    service::CallVolumeMetricConfig::from_overlay(Some(&OwnerReportsOverlay {
        call_volume: CallVolumeMetricOverlay {
            category_id: "ruby_call_log".to_string(),
            label: "Incoming calls".to_string(),
            source_label:
                "Ruby summary emails; direct calls not summarized by Ruby are not included."
                    .to_string(),
            gmail_label: "Ruby call log".to_string(),
            gmail_query: "from:noreply@business-bbc4ea68b9.test".to_string(),
        },
        ..OwnerReportsOverlay::default()
    }))
}

fn accounting_metric_config() -> crate::slices::accounting::service::AccountingMetricBasisConfig {
    crate::slices::accounting::service::AccountingMetricBasisConfig::default()
}

fn state_with_call_volume_config() -> AppState {
    let mut state = test_state();
    state.owner_reports_overlay = Arc::new(Some(OwnerReportsOverlay {
        call_volume: CallVolumeMetricOverlay {
            category_id: "ruby_call_log".to_string(),
            label: "Incoming calls".to_string(),
            source_label:
                "Ruby summary emails; direct calls not summarized by Ruby are not included."
                    .to_string(),
            gmail_label: "Ruby call log".to_string(),
            gmail_query: "from:noreply@business-bbc4ea68b9.test".to_string(),
        },
        ..OwnerReportsOverlay::default()
    }));
    state
}

fn qbo_grant(now_ms: u64) -> QboTokenGrant {
    QboTokenGrant {
        access_token: "access-token".to_string(),
        access_token_expires_at_ms: now_ms + 3_600_000,
        refresh_token: "refresh-token".to_string(),
        refresh_token_expires_at_ms: now_ms + 8_640_000_000,
    }
}

// ---------------------------------------------------------------------------
// Period math
// ---------------------------------------------------------------------------

#[test]
fn current_periods_anchor_monday_and_first_of_month() {
    let periods = service::current_periods(TODAY);
    assert_eq!(periods.len(), 2);
    assert_eq!(periods[0].kind, OwnerReportPeriodKind::Weekly);
    assert_eq!(periods[0].start, WEEK_START);
    assert_eq!(periods[0].end, TODAY);
    assert_eq!(periods[1].kind, OwnerReportPeriodKind::Mtd);
    assert_eq!(periods[1].start, MONTH_START);
    assert_eq!(periods[1].end, TODAY);
    assert_eq!(
        service::report_id_for(periods[0].kind, &periods[0].start),
        "owr_weekly_2026-06-08"
    );
    assert_eq!(
        service::report_id_for(periods[1].kind, &periods[1].start),
        "owr_mtd_2026-06-01"
    );
}

#[test]
fn overlay_config_controls_schedule_and_email_presentation() {
    let overlay = crate::overlay::OwnerReportsOverlay {
        allowed_operator_user_ids: vec!["user_jordan".to_string()],
        delivery_enabled: true,
        recipients: vec![
            "jordan@example.com".to_string(),
            "casey@example.com".to_string(),
        ],
        weekly_weekday: Some("monday".to_string()),
        mtd_day: Some(1),
        metrics: vec![
            "sales".to_string(),
            "site_traffic".to_string(),
            "close_rate".to_string(),
        ],
        subject_prefix: Some("Demo owner update".to_string()),
        ..crate::overlay::OwnerReportsOverlay::default()
    };
    let config = service::config_from_sources(Some(&overlay));
    assert!(service::operator_allowed(&config, "user_jordan"));
    assert!(!service::operator_allowed(&config, "user_casey"));
    assert!(config.delivery_enabled);
    assert_eq!(config.recipients.len(), 2);
    assert_eq!(config.weekly_weekday, Some(ReportWeekday::Monday));
    assert_eq!(config.mtd_day, Some(1));
    assert_eq!(
        config.metrics,
        vec![
            ReportMetricSection::Sales,
            ReportMetricSection::SiteTraffic,
            ReportMetricSection::CloseRate,
        ]
    );
    assert_eq!(
        service::recipients_line(&config).as_deref(),
        Some("jordan@example.com, casey@example.com")
    );

    let monday_weekly = DigestPeriod {
        kind: OwnerReportPeriodKind::Weekly,
        start: "2026-06-15".to_string(),
        end: "2026-06-15".to_string(),
    };
    assert!(service::due_for_scheduled_delivery(
        &monday_weekly,
        "2026-06-15",
        &config
    ));
    assert!(!service::due_for_scheduled_delivery(
        &monday_weekly,
        "2026-06-16",
        &config
    ));

    let mtd = DigestPeriod {
        kind: OwnerReportPeriodKind::Mtd,
        start: "2026-07-01".to_string(),
        end: "2026-07-01".to_string(),
    };
    assert!(service::due_for_scheduled_delivery(
        &mtd,
        "2026-07-01",
        &config
    ));

    let report = service::report_from_parts(
        &monday_weekly,
        sample_metrics(),
        Err("llm_down".to_string()),
        now_ms(),
    );
    let (subject, body) = service::render_digest_email_with_config(&report, &config);
    assert!(subject.starts_with("Demo owner update"));
    assert!(body.contains("SALES"));
    assert!(body.contains("SITE TRAFFIC"));
    assert!(body.contains("CRM BUSINESS METRICS"));
    assert!(!body.contains("FOLLOW-UPS"));
}

#[tokio::test]
async fn owner_report_routes_require_configured_operator() {
    let mut state = test_state();
    state.owner_reports_overlay = Arc::new(Some(OwnerReportsOverlay {
        allowed_operator_user_ids: vec!["user_jordan".to_string()],
        ..OwnerReportsOverlay::default()
    }));
    {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        crate::slices::operator_users::store::create_user(
            conn,
            CLIENT,
            "operator",
            &OperatorUser {
                user_id: "user_jordan".to_string(),
                display_name: "Jordan".to_string(),
                active: true,
                archived_at_ms: None,
                default_calendar_id: None,
                created_at_ms: 1_000,
                updated_at_ms: 1_000,
            },
            "tok_jordan",
            "create_jordan",
        )
        .expect("create jordan");
        crate::slices::operator_users::store::create_user(
            conn,
            CLIENT,
            "operator",
            &OperatorUser {
                user_id: "user_casey".to_string(),
                display_name: "Casey".to_string(),
                active: true,
                archived_at_ms: None,
                default_calendar_id: None,
                created_at_ms: 1_001,
                updated_at_ms: 1_001,
            },
            "tok_casey",
            "create_casey",
        )
        .expect("create casey");
    }

    let router = build_router(state);
    let denied = router
        .clone()
        .oneshot(
            Request::get("/api/owner-reports")
                .header(header::AUTHORIZATION, "Bearer tok_casey")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("denied response");
    assert_eq!(denied.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = denied.into_body().collect().await.expect("body").to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&body).expect("json error");
    assert_eq!(body["error"], "owner_report_scope_forbidden");

    let allowed = router
        .oneshot(
            Request::get("/api/owner-reports")
                .header(header::AUTHORIZATION, "Bearer tok_jordan")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("allowed response");
    assert_eq!(allowed.status(), StatusCode::OK);
}

#[test]
fn recipient_profile_redacts_financial_owner_report_sections() {
    let overlay = crate::overlay::OwnerReportsOverlay {
        recipients: vec!["casey@example.com".to_string()],
        recipient_profiles: vec![crate::overlay::OwnerReportRecipientOverlay {
            recipients: vec!["casey@example.com".to_string()],
            metrics: vec![
                "calls".to_string(),
                "follow_ups".to_string(),
                "orders".to_string(),
                "damage_claims".to_string(),
            ],
        }],
        ..crate::overlay::OwnerReportsOverlay::default()
    };
    let config = service::config_from_sources(Some(&overlay));
    let report = service::report_from_parts(
        &DigestPeriod {
            kind: OwnerReportPeriodKind::Weekly,
            start: WEEK_START.to_string(),
            end: TODAY.to_string(),
        },
        sample_metrics(),
        Ok((
            DigestNarration {
                headline: "Sales reached $4,520.00".to_string(),
                narrative: "Gross profit was $3,000.00 and margin was -$1,234.56.".to_string(),
                callouts: vec!["Sales: $4,520.00".to_string()],
                confidence: "high".to_string(),
            },
            "test-model".to_string(),
        )),
        now_ms(),
    );
    let (_subject, body) = service::render_digest_email_with_config(&report, &config);

    assert!(!body.contains("SALES"));
    assert!(!body.contains('$'));
    assert!(!body.contains("gross profit"));
    assert!(!body.contains("baseline"));
    assert!(body.contains("CALLS"));
    assert!(body.contains("FOLLOW-UPS"));
    assert!(body.contains("ORDERS"));
    assert!(body.contains("DAMAGE / CLAIMS"));
}

#[test]
fn owner_report_api_redaction_removes_stored_financial_details() {
    let mut report = service::report_from_parts(
        &DigestPeriod {
            kind: OwnerReportPeriodKind::Mtd,
            start: MONTH_START.to_string(),
            end: TODAY.to_string(),
        },
        sample_metrics(),
        Ok((
            DigestNarration {
                headline: "Sales reached $4,520.00".to_string(),
                narrative: "Gross profit was $3,000.00 and margin was -$1,234.56.".to_string(),
                callouts: vec!["Sales: $4,520.00".to_string()],
                confidence: "high".to_string(),
            },
            "test-model".to_string(),
        )),
        now_ms(),
    );

    service::redact_financials(&mut report);

    assert_eq!(report.metrics.sales.basis, "redacted");
    assert_eq!(report.metrics.sales.period_sales_cents, 0);
    assert!(report.metrics.sales.prior_period_sales_cents.is_none());
    assert!(report.metrics.sales.mtd_gross_profit_cents.is_none());
    assert!(report.metrics.sales.baseline_monthly_margin_cents.is_none());
    assert!(report.metrics.sales.margin_above_baseline_cents.is_none());
    assert!(report.headline.is_none());
    assert!(report.narrative.is_none());
    assert!(report.callouts.is_empty());
}

#[test]
fn qbo_backed_owner_report_requires_authorizer_or_all_scope() {
    let mut report = service::report_from_parts(
        &DigestPeriod {
            kind: OwnerReportPeriodKind::Mtd,
            start: MONTH_START.to_string(),
            end: TODAY.to_string(),
        },
        sample_metrics(),
        Ok((
            DigestNarration {
                headline: "Sales reached $4,520.00".to_string(),
                narrative: "Gross profit was $3,000.00.".to_string(),
                callouts: vec![],
                confidence: "high".to_string(),
            },
            "test-model".to_string(),
        )),
        now_ms(),
    );
    let state = test_state();
    {
        let persistence = state.persistence.lock();
        let conn = persistence.connection_ref();
        assert!(service::report_financials_visible(
            conn,
            CLIENT,
            &crate::http::OperatorScope::All,
            &report,
            AccountingVisibilityPolicy::AuthorizerOnly,
        )
        .expect("all scope"));
        assert!(!service::report_financials_visible(
            conn,
            CLIENT,
            &crate::http::OperatorScope::User("user_casey".to_string()),
            &report,
            AccountingVisibilityPolicy::AuthorizerOnly,
        )
        .expect("no authorizer"));
    }

    {
        let mut persistence = state.persistence.lock();
        accounting::store::store_credential(
            persistence.connection(),
            CLIENT,
            "realm-1",
            "sandbox",
            &qbo_grant(2_000),
            "user_example",
            2_000,
        )
        .expect("store qbo credential");
        let conn = persistence.connection_ref();
        assert!(service::report_financials_visible(
            conn,
            CLIENT,
            &crate::http::OperatorScope::User("user_example".to_string()),
            &report,
            AccountingVisibilityPolicy::AuthorizerOnly,
        )
        .expect("authorizer"));
        assert!(!service::report_financials_visible(
            conn,
            CLIENT,
            &crate::http::OperatorScope::User("user_casey".to_string()),
            &report,
            AccountingVisibilityPolicy::AuthorizerOnly,
        )
        .expect("other user"));
    }

    report.metrics.sales.basis = "invoice_totals".to_string();
    let persistence = state.persistence.lock();
    assert!(service::report_financials_visible(
        persistence.connection_ref(),
        CLIENT,
        &crate::http::OperatorScope::User("user_casey".to_string()),
        &report,
        AccountingVisibilityPolicy::Shared,
    )
    .expect("non-qbo report"));
}

#[tokio::test]
async fn generate_route_refuses_non_authorizer_for_qbo_financials() {
    let mut state = test_state();
    state.accounting_visibility_policy = AccountingVisibilityPolicy::AuthorizerOnly;
    {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        crate::slices::operator_users::store::create_user(
            conn,
            CLIENT,
            "operator",
            &OperatorUser {
                user_id: "user_example".to_string(),
                display_name: "Avery".to_string(),
                active: true,
                archived_at_ms: None,
                default_calendar_id: None,
                created_at_ms: 1_000,
                updated_at_ms: 1_000,
            },
            "tok_example",
            "create_example",
        )
        .expect("create avery");
        crate::slices::operator_users::store::create_user(
            conn,
            CLIENT,
            "operator",
            &OperatorUser {
                user_id: "user_casey".to_string(),
                display_name: "Casey".to_string(),
                active: true,
                archived_at_ms: None,
                default_calendar_id: None,
                created_at_ms: 1_001,
                updated_at_ms: 1_001,
            },
            "tok_casey",
            "create_casey",
        )
        .expect("create casey");
        accounting::store::store_credential(
            conn,
            CLIENT,
            "realm-1",
            "sandbox",
            &qbo_grant(2_000),
            "user_example",
            2_000,
        )
        .expect("store qbo credential");
    }

    let response = build_router(state)
        .oneshot(
            Request::post("/api/owner-reports/generate")
                .header(header::AUTHORIZATION, "Bearer tok_casey")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&body).expect("json error");
    assert_eq!(body["error"], "qbo_financial_scope_forbidden");
}

// ---------------------------------------------------------------------------
// Dollar formatting + grounding
// ---------------------------------------------------------------------------

#[test]
fn format_dollars_groups_and_signs() {
    assert_eq!(service::format_dollars(0), "$0.00");
    assert_eq!(service::format_dollars(50), "$0.50");
    assert_eq!(service::format_dollars(123_456), "$1,234.56");
    assert_eq!(service::format_dollars(-123_456), "-$1,234.56");
    assert_eq!(service::format_dollars(100_000_000), "$1,000,000.00");
}

#[test]
fn narration_accepts_amounts_from_the_metrics() {
    let metrics = sample_metrics();
    let response = json!({
        "headline": "Sales of $4,520.00 this week, margin $1,234.56 below baseline.",
        "narrative": "Sales were $4,520.00 against $3,800.50 the prior period. Margin ran -$1,234.56 vs baseline.",
        "callouts": ["2 follow-ups overdue", "Gross profit $3,000.00 month to date"],
        "confidence": "high",
    });
    let narration = service::parse_narration_response(&response, &metrics).expect("grounded");
    assert_eq!(narration.callouts.len(), 2);
    assert_eq!(narration.confidence, "high");
}

#[test]
fn narration_rejects_invented_dollar_amounts() {
    let metrics = sample_metrics();
    let response = json!({
        "headline": "A great week",
        "narrative": "Sales were approximately $9,999.00 this week.",
        "callouts": [],
        "confidence": "high",
    });
    let err = service::parse_narration_response(&response, &metrics).unwrap_err();
    assert!(err.contains("$9,999.00"), "unexpected error: {err}");

    // The invented amount is refused anywhere in the output, callouts too.
    let response = json!({
        "headline": "A great week",
        "narrative": "Sales were $4,520.00.",
        "callouts": ["Watch the $12.34 in fees"],
        "confidence": "low",
    });
    assert!(service::parse_narration_response(&response, &metrics).is_err());
}

#[test]
fn narration_requires_fields_and_valid_confidence() {
    let metrics = sample_metrics();
    let missing = json!({ "narrative": "x", "confidence": "high" });
    assert!(service::parse_narration_response(&missing, &metrics).is_err());
    let bad_confidence = json!({ "headline": "x", "narrative": "y", "confidence": "sure" });
    assert!(service::parse_narration_response(&bad_confidence, &metrics).is_err());
}

#[test]
fn narration_request_uses_overlay_seeded_client_profile_and_metric_sections() {
    let mut metrics = sample_metrics();
    metrics.metric_sections = vec!["sales".to_string(), "site_traffic".to_string()];
    let profile = ClientProfile {
        client_id: CLIENT.to_string(),
        company_name: Some("Avery Example".to_string()),
        bio: Some("Personal owner report focused on search traffic.".to_string()),
        industry: Some("Personal business operations".to_string()),
        website: Some("https://example.com".to_string()),
        persona: Some("Direct and concise.".to_string()),
    };
    let period = DigestPeriod {
        kind: OwnerReportPeriodKind::Weekly,
        start: WEEK_START.to_string(),
        end: TODAY.to_string(),
    };
    let request = service::build_narration_request(
        CLIENT,
        "owr_weekly_2026-06-08",
        &period,
        &metrics,
        Some(&profile),
        1,
    );

    let input = request.input.json;
    assert_eq!(input["client_profile"]["company_name"], "Avery Example");
    assert_eq!(
        input["active_metric_sections"],
        json!(["sales", "site_traffic"])
    );
    let instructions = input["instructions"].as_str().expect("instructions");
    assert!(!instructions.contains("furniture manufacturer"));
    assert!(!instructions.contains("stock orders"));
}

// ---------------------------------------------------------------------------
// Metric assembly over seeded caches
// ---------------------------------------------------------------------------

fn inbound(message_id: &str, category: &str, at_ms: u64) -> InboundMessageRecord {
    InboundMessageRecord {
        source_key: message_id.to_string(),
        message_id: message_id.to_string(),
        thread_id: None,
        internal_date_ms: Some(at_ms as i64),
        from_addr: Some("calls@business-bbc4ea68b9.test".to_string()),
        to_addr: None,
        subject: Some("Call from Dana".to_string()),
        body_excerpt: "Message: Please call back.".to_string(),
        body_full: "Message: Please call back.".to_string(),
        headers: Vec::new(),
        labels: Vec::new(),
        resolved_category: category.to_string(),
        matched_rule_id: None,
        ingested_at_ms: at_ms,
        ai_triage_status: None,
        ai_triage_rationale: None,
        attachments: Vec::new(),
        source_user_id: None,
    }
}

fn inbound_with_body(
    message_id: &str,
    category: &str,
    at_ms: u64,
    subject: &str,
    body: &str,
) -> InboundMessageRecord {
    let mut record = inbound(message_id, category, at_ms);
    record.subject = Some(subject.to_string());
    record.body_excerpt = body.to_string();
    record.body_full = body.to_string();
    record
}

fn add_attention_enrichment(
    conn: &mut rusqlite::Connection,
    source_key: &str,
    level: AttentionLevel,
    reason_code: &str,
) {
    let parsed = ParsedInbound {
        attention_signals: vec![AttentionSignal {
            level,
            reason_code: reason_code.to_string(),
            label: Some(
                match level {
                    AttentionLevel::Higher => "Needs attention",
                    AttentionLevel::Normal => "Attention",
                    AttentionLevel::Lower => "Lower attention",
                }
                .to_string(),
            ),
            detail: None,
            provenance: "test".to_string(),
        }],
        ..ParsedInbound::default()
    };
    crate::slices::email_triage::store::upsert_inbound_enrichment(
        conn,
        crate::slices::email_triage::store::InboundEnrichmentWrite {
            client_id: CLIENT,
            source_key,
            parser_id: "test_call_log",
            parser_version: "1",
            parsed: &parsed,
            now_ms: 1_000,
        },
    )
    .expect("attention enrichment");
}

fn order(order_id: &str, order_date: &str) -> SfOrderCardRecord {
    SfOrderCardRecord {
        order_id: order_id.to_string(),
        order_number: format!("#{order_id}"),
        external_order_id: Some(format!("shopify-{order_id}")),
        platform: Some("shopify".to_string()),
        board_status: "PACKED".to_string(),
        raw_status: None,
        customer_name: Some("Dana".to_string()),
        customer_email: Some("dana@example.test".to_string()),
        total_amount_cents: 21_999,
        currency: Some("USD".to_string()),
        order_date: Some(order_date.to_string()),
        processed_at: None,
        item_count: 2,
        unit_count: 3,
        mapped_line_count: 2,
        line_material_ids: vec!["m1".to_string()],
        line_identity_complete: true,
        carrier: Some("UPS".to_string()),
        tracking_number: None,
        shipment_refs: None,
        shipment_id: Some(format!("shp-{order_id}")),
        ship_date: None,
        photo_count: 0,
        pack_station_container_id: None,
        needs_mapping: false,
        blocked: false,
        deducted: true,
        deduction_failed: false,
        label_needed: false,
        packed_missing_photo: false,
        exception: false,
        depletion_total: 2,
        depletion_applied: 2,
        depletion_failed: 0,
        depletion_reversed: 0,
        blocked_reasons_json: "[]".to_string(),
    }
}

fn inventory_material(material_id: &str, quantity: f64, stocked: bool) -> SfMaterialRecord {
    SfMaterialRecord {
        material_id: material_id.to_string(),
        name: format!("Material {material_id}"),
        sku: Some(format!("SKU-{material_id}")),
        category: Some("DISCRETE".to_string()),
        current_quantity: quantity,
        reserved_qty: Some(0.0),
        incoming_qty: Some(0.0),
        unit: Some("ea".to_string()),
        warning_threshold: Some(10.0),
        critical_threshold: Some(5.0),
        threshold_type: Some("ABSOLUTE".to_string()),
        unit_cost_cents: 500,
        lead_time_days: None,
        vendor_name: None,
        is_active: true,
        is_purchasable: Some(stocked),
        replenishment_policy: Some(if stocked { "PURCHASE" } else { "NONE" }.to_string()),
        sale_depletion_policy: Some(if stocked { "STOCK" } else { "COMPONENTS" }.to_string()),
        updated_at: None,
    }
}

fn purchase_order(po_id: &str, status: &str, cents: i64) -> SfPurchaseOrderRecord {
    SfPurchaseOrderRecord {
        po_id: po_id.to_string(),
        vendor_name: None,
        status: status.to_string(),
        total_estimated_cost_cents: cents,
        freight_mode: None,
        line_count: 1,
        line_material_ids: vec!["m2".to_string()],
        line_identity_complete: true,
        created_at: None,
        sent_at: None,
        received_at: None,
    }
}

fn damage_event(id: &str, reported_at: &str) -> SfDamageEventRecord {
    SfDamageEventRecord {
        damage_event_id: id.to_string(),
        shipment_id: "shp-1".to_string(),
        reported_at: Some(format!("{reported_at}T15:00:00Z")),
        reported_by: "CUSTOMER".to_string(),
        severity: "HIGH".to_string(),
        damage_type: "Crushed carton".to_string(),
        photos: vec!["https://files.example/damage-1.jpg".to_string()],
        description: Some("Box arrived crushed.".to_string()),
        claim_status: "OPEN".to_string(),
        claim_amount_cents: Some(15_000),
        resolution: None,
        shipment_number: Some("SHP-77".to_string()),
        carrier: Some("UPS".to_string()),
        tracking_number: Some("1Z999AA10123456784".to_string()),
        shipment_refs: None,
        shipment_status: Some("DELIVERED".to_string()),
    }
}

fn resolved_damage_event(id: &str, reported_at: &str) -> SfDamageEventRecord {
    let mut event = damage_event(id, reported_at);
    event.shipment_id = "shp-2".to_string();
    event.damage_type = "Leaking pail".to_string();
    event.severity = "LOW".to_string();
    event.claim_status = "RESOLVED".to_string();
    event
}

fn claim_draft(draft_id: &str, created_at_ms: u64) -> ClaimDraft {
    ClaimDraft {
        draft_id: draft_id.to_string(),
        item_id: format!("itm_{draft_id}"),
        source_kind: "stockforge_damage".to_string(),
        source_ref: "dmg-1".to_string(),
        status: ClaimDraftStatus::Staged,
        tracking_number: None,
        carrier: None,
        shipment_number: None,
        shipment_context_source: None,
        shipment_refs: None,
        order_number: None,
        order_platform: None,
        external_order_id: None,
        customer_name: None,
        order_total_cents: None,
        ship_date: None,
        damage_type: "Crushed carton".to_string(),
        damage_severity: "HIGH".to_string(),
        damage_reported_at: None,
        claim_amount_cents: 15_000,
        damage_narrative: "narrative".to_string(),
        item_description: String::new(),
        evidence: ClaimEvidence::default(),
        packet: ClaimPacketGate {
            ready: false,
            missing_roles: Vec::new(),
        },
        provenance: Vec::new(),
        model: "test".to_string(),
        confidence: "high".to_string(),
        outbox_job_id: None,
        follow_up_task_id: None,
        created_at_ms,
        updated_at_ms: created_at_ms,
    }
}

fn seed_task(state: &AppState, task_id: &str, due_date: Option<&str>, now: u64) {
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let tx = conn.transaction().expect("tx");
    crate::slices::follow_up_tasks::store::insert_task_within(
        &tx,
        CLIENT,
        &TaskRecord {
            task_id: task_id.to_string(),
            title: format!("task {task_id}"),
            due_date: due_date.map(str::to_string),
            context: String::new(),
            source_kind: "manual".to_string(),
            source_ref: task_id.to_string(),
            source_user_id: None,
            source_item_id: None,
            status: bos_contracts::follow_up_tasks::TaskStatus::Open,
            created_at_ms: now,
            updated_at_ms: now,
        },
        now,
    )
    .expect("seed task");
    tx.commit().expect("commit");
}

fn seed_caches(state: &AppState) {
    let in_window = ms("2026-06-09") + 1_000;
    let before_window = ms("2026-05-20");
    {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        // Ruby call log: one in the week, one before it, one other-category.
        crate::slices::email_triage::store::record_inbound_message(
            conn,
            CLIENT,
            &inbound("msg-1", "ruby_call_log", in_window),
        )
        .expect("seed message");
        add_attention_enrichment(conn, "msg-1", AttentionLevel::Higher, "callback_needed");
        crate::slices::email_triage::store::record_inbound_message(
            conn,
            CLIENT,
            &inbound("msg-2", "ruby_call_log", before_window),
        )
        .expect("seed message");
        add_attention_enrichment(conn, "msg-2", AttentionLevel::Higher, "callback_needed");
        crate::slices::email_triage::store::record_inbound_message(
            conn,
            CLIENT,
            &inbound("msg-3", "billing", in_window),
        )
        .expect("seed message");
        // Sales: the current week + current month P&L periods.
        accounting::store::upsert_pnl_snapshot(
            conn,
            CLIENT,
            &accounting::store::PnlSnapshotRow {
                period_kind: "week".to_string(),
                period_start: WEEK_START.to_string(),
                period_end: TODAY.to_string(),
                total_income_cents: 452_000,
                total_cogs_cents: 152_000,
                gross_profit_cents: 300_000,
                is_complete: false,
            },
            in_window,
        )
        .expect("seed pnl week");
        accounting::store::upsert_pnl_snapshot(
            conn,
            CLIENT,
            &accounting::store::PnlSnapshotRow {
                period_kind: "week".to_string(),
                period_start: "2026-06-01".to_string(),
                period_end: "2026-06-07".to_string(),
                total_income_cents: 999_000,
                total_cogs_cents: 499_000,
                gross_profit_cents: 500_000,
                is_complete: true,
            },
            in_window,
        )
        .expect("seed prior full pnl week");
        accounting::store::upsert_pnl_snapshot(
            conn,
            CLIENT,
            &accounting::store::PnlSnapshotRow {
                period_kind: "month".to_string(),
                period_start: MONTH_START.to_string(),
                period_end: TODAY.to_string(),
                total_income_cents: 1_000_000,
                total_cogs_cents: 700_000,
                gross_profit_cents: 300_000,
                is_complete: false,
            },
            in_window,
        )
        .expect("seed pnl month");
        accounting::store::upsert_pnl_snapshot(
            conn,
            CLIENT,
            &accounting::store::PnlSnapshotRow {
                period_kind: "month".to_string(),
                period_start: "2026-05-01".to_string(),
                period_end: "2026-05-31".to_string(),
                total_income_cents: 2_000_000,
                total_cogs_cents: 1_100_000,
                gross_profit_cents: 900_000,
                is_complete: true,
            },
            in_window,
        )
        .expect("seed prior full pnl month");
        for (date, cents) in [
            ("2026-06-01", 10_000),
            ("2026-06-02", 20_000),
            ("2026-06-03", 30_000),
        ] {
            accounting::store::upsert_pnl_snapshot(
                conn,
                CLIENT,
                &accounting::store::PnlSnapshotRow {
                    period_kind: "day".to_string(),
                    period_start: date.to_string(),
                    period_end: date.to_string(),
                    total_income_cents: cents,
                    total_cogs_cents: 0,
                    gross_profit_cents: cents,
                    is_complete: true,
                },
                in_window,
            )
            .expect("seed prior wtd pnl day");
        }
        for (date, cents) in [
            ("2026-05-01", 10_000),
            ("2026-05-02", 20_000),
            ("2026-05-03", 30_000),
            ("2026-05-04", 40_000),
            ("2026-05-05", 50_000),
            ("2026-05-06", 60_000),
            ("2026-05-07", 70_000),
            ("2026-05-08", 80_000),
            ("2026-05-09", 90_000),
            ("2026-05-10", 100_000),
        ] {
            accounting::store::upsert_pnl_snapshot(
                conn,
                CLIENT,
                &accounting::store::PnlSnapshotRow {
                    period_kind: "day".to_string(),
                    period_start: date.to_string(),
                    period_end: date.to_string(),
                    total_income_cents: cents,
                    total_cogs_cents: 0,
                    gross_profit_cents: cents,
                    is_complete: true,
                },
                in_window,
            )
            .expect("seed prior mtd pnl day");
        }
        // Orders: one in the week (exception), one in May (needs mapping —
        // counts in the CURRENT backlog, not the weekly window).
        let mut in_week = order("o1", "2026-06-09");
        in_week.exception = true;
        let mut older = order("o2", "2026-05-20");
        older.needs_mapping = true;
        crate::slices::inventory::store::upsert_order_snapshots(
            conn,
            CLIENT,
            &[in_week, older],
            in_window,
        )
        .expect("seed orders");
        crate::slices::inventory::store::upsert_material_snapshots(
            conn,
            CLIENT,
            &[
                inventory_material("m1", 0.0, true),
                inventory_material("m2", 4.0, true),
                inventory_material("catalog", 100.0, false),
            ],
            in_window,
        )
        .expect("seed inventory materials");
        crate::slices::inventory::store::upsert_po_snapshots(
            conn,
            CLIENT,
            &[
                purchase_order("po-open", "SENT", 75_000),
                purchase_order("po-received", "RECEIVED", 25_000),
            ],
            in_window,
        )
        .expect("seed purchase orders");
        // Damage: one event reported inside the week.
        crate::slices::claim_drafts::store::upsert_damage_snapshot(
            conn,
            CLIENT,
            &damage_event("dmg-1", "2026-06-09"),
            in_window,
        )
        .expect("seed damage");
        crate::slices::claim_drafts::store::upsert_damage_snapshot(
            conn,
            CLIENT,
            &resolved_damage_event("dmg-2", "2026-06-09"),
            in_window,
        )
        .expect("seed resolved damage");
        for damage_id in ["dmg-1", "dmg-2"] {
            crate::slices::work_queue::service::emit_unconditional(
                conn,
                CLIENT,
                crate::slices::work_queue::service::UnconditionalEmit {
                    source_kind: crate::slices::work_queue::SOURCE_KIND_STOCKFORGE_DAMAGE,
                    source_ref: damage_id,
                    category_id: crate::slices::claim_drafts::DAMAGE_CATEGORY,
                    title: "Shipping damage",
                    summary: "Damage report",
                    default_kinds: vec![
                        crate::slices::claim_drafts::service::PACKET_KIND.to_string()
                    ],
                    allow_policy_kinds: true,
                    source_user_id: None,
                    status: bos_contracts::work_queue::WorkItemStatus::Open,
                },
                in_window,
            )
            .expect("emit damage queue item");
        }
        crate::slices::work_queue::store::apply_item_action(
            conn,
            crate::slices::work_queue::store::ItemActionContext {
                client_id: CLIENT,
                actor_id: "operator",
                scope: &crate::http::OperatorScope::All,
                expected_revision: None,
                idempotency_key: "test:accept:dmg-1",
                now_ms: in_window + 1,
            },
            "wi_stockforge_damage_dmg-1",
            crate::slices::work_queue::store::ItemAction::Accept,
        )
        .expect("accept damage item");
        crate::slices::work_queue::store::apply_item_action(
            conn,
            crate::slices::work_queue::store::ItemActionContext {
                client_id: CLIENT,
                actor_id: "operator",
                scope: &crate::http::OperatorScope::All,
                expected_revision: None,
                idempotency_key: "test:dismiss:dmg-2",
                now_ms: in_window + 2,
            },
            "wi_stockforge_damage_dmg-2",
            crate::slices::work_queue::store::ItemAction::Dismiss,
        )
        .expect("dismiss damage item");
        // Claims: one draft staged inside the week.
        crate::slices::claim_drafts::store::insert_draft(
            conn,
            CLIENT,
            "operator",
            &claim_draft("clm-1", in_window),
            "test:clm-1",
        )
        .expect("seed claim draft");
    }
    // Tasks: overdue-escalated (3 days), due today, done in window.
    seed_task(state, "t-overdue", Some("2026-06-07"), ms("2026-06-05"));
    seed_task(state, "t-today", Some(TODAY), ms("2026-06-08"));
    seed_task(state, "t-done", Some("2026-06-08"), ms("2026-06-05"));
    {
        let mut persistence = state.persistence.lock();
        crate::slices::follow_up_tasks::store::apply_task_action(
            persistence.connection(),
            crate::slices::follow_up_tasks::store::DraftActionContext {
                client_id: CLIENT,
                actor_id: "operator",
                scope: &crate::http::OperatorScope::All,
                expected_revision: None,
                idempotency_key: "test:done:t-done",
                now_ms: ms("2026-06-09") + 2_000,
            },
            "t-done",
            crate::slices::follow_up_tasks::store::TaskAction::Complete,
        )
        .expect("flip task done");
    }
}

#[test]
fn assemble_metrics_reads_the_local_caches() {
    let state = test_state();
    seed_caches(&state);
    let period = DigestPeriod {
        kind: OwnerReportPeriodKind::Weekly,
        start: WEEK_START.to_string(),
        end: TODAY.to_string(),
    };
    let accounting_status = state
        .sync_guards
        .guard(crate::http::Pump::Accounting)
        .lock()
        .clone();
    let persistence = state.persistence.lock();
    let report_config = service::OwnerReportConfig {
        report_profile: Some("test_profile".to_string()),
        ..service::OwnerReportConfig::default()
    };
    let metrics = service::assemble_metrics(
        persistence.connection_ref(),
        CLIENT,
        &accounting_status,
        service::MetricAssemblyConfig {
            report: &report_config,
            call_volume: &call_volume_config(),
            accounting_metric: &accounting_metric_config(),
            search_console_overlay: state.search_console_overlay.as_ref().as_ref(),
        },
        &period,
        TODAY,
    )
    .expect("assemble");
    assert_eq!(
        metrics.metric_sections,
        service::metric_section_ids(&report_config)
    );

    // Sales mirror the accounting financials (QBO P&L basis; no baseline).
    assert_eq!(metrics.sales.basis, "quickbooks_pnl");
    assert_eq!(metrics.sales.period_sales_cents, 452_000);
    assert_eq!(metrics.sales.prior_period_sales_cents, Some(60_000));
    assert_eq!(metrics.sales.mtd_gross_profit_cents, Some(300_000));
    assert_eq!(metrics.sales.margin_above_baseline_cents, None);

    assert_eq!(metrics.calls.call_log_messages, 1);
    assert_eq!(metrics.calls.transfer_successful, 0);
    assert_eq!(metrics.calls.callback_needed, 0);
    assert_eq!(metrics.calls.no_callback_needed, 0);
    assert_eq!(metrics.calls.unknown_outcome, 1);

    assert_eq!(metrics.follow_ups.open, 2);
    assert_eq!(metrics.follow_ups.done_in_period, 1);
    assert_eq!(metrics.follow_ups.due_today, 1);
    assert_eq!(metrics.follow_ups.overdue, 1);
    assert_eq!(metrics.follow_ups.escalated, 1);
    assert_eq!(metrics.follow_ups.critical, 0);

    assert_eq!(metrics.orders.orders_in_period, 1);
    assert_eq!(metrics.orders.exceptions, 1);
    assert_eq!(metrics.orders.needs_mapping, 1);
    assert_eq!(metrics.orders.blocked, 0);

    let material_rows =
        crate::slices::inventory::store::list_materials(persistence.connection_ref(), CLIENT)
            .expect("inventory materials");
    let alert_rows =
        crate::slices::inventory::store::list_alerts(persistence.connection_ref(), CLIENT)
            .expect("inventory alerts");
    let (stock_kpis, _) =
        crate::slices::inventory::service::compute_stock(&material_rows, &alert_rows);
    let po_rows =
        crate::slices::inventory::store::list_purchase_orders(persistence.connection_ref(), CLIENT)
            .expect("purchase orders");
    let (_, open_po_total) = crate::slices::inventory::service::open_purchase_orders(&po_rows);
    assert!(metrics.inventory.configured);
    assert_eq!(
        metrics.inventory.stocked_sku_count,
        u64::from(stock_kpis.monitored_materials)
    );
    assert_eq!(
        metrics.inventory.out_of_stock_count,
        u64::from(stock_kpis.out_of_stock_count)
    );
    assert_eq!(
        metrics.inventory.critical_count,
        u64::from(stock_kpis.critical_count)
    );
    assert_eq!(
        metrics.inventory.stock_value_cents,
        stock_kpis.stock_value_cents
    );
    assert_eq!(metrics.inventory.inbound_open_po_cents, open_po_total);
    assert_eq!(open_po_total, 75_000, "received POs are not inbound");

    assert_eq!(metrics.claims.damage_events_in_period, 2);
    assert_eq!(metrics.claims.damage_open, 1);
    assert_eq!(metrics.claims.damage_resolved, 1);
    assert_eq!(metrics.claims.damage_by_severity.len(), 2);
    assert_eq!(metrics.claims.damage_by_status.len(), 2);
    assert_eq!(
        metrics
            .claims
            .damage_by_status
            .iter()
            .find(|entry| entry.status == "open")
            .map(|entry| entry.count),
        Some(1)
    );
    assert_eq!(
        metrics
            .claims
            .damage_by_type
            .iter()
            .find(|entry| entry.damage_type == "Leaking pail")
            .map(|entry| entry.count),
        Some(1)
    );
    assert_eq!(metrics.claims.queue_open, 0);
    assert_eq!(metrics.claims.queue_accepted, 1);
    assert_eq!(metrics.claims.queue_dismissed, 1);
    assert_eq!(metrics.claims.claims_drafted_in_period, 1);
    assert_eq!(metrics.claims.claims_approved_in_period, 0);
    assert_eq!(metrics.claims.claim_drafts_by_status.len(), 1);
    assert_eq!(metrics.claims.claim_drafts_by_status[0].status, "staged");
    assert_eq!(metrics.deals.status, DigestDealMetricsStatus::PendingConfig);
    assert!(!metrics.deals.message.is_empty());

    // The MTD window picks up the May order and the month P&L figure.
    let mtd = DigestPeriod {
        kind: OwnerReportPeriodKind::Mtd,
        start: MONTH_START.to_string(),
        end: TODAY.to_string(),
    };
    let metrics = service::assemble_metrics(
        persistence.connection_ref(),
        CLIENT,
        &accounting_status,
        service::MetricAssemblyConfig {
            report: &report_config,
            call_volume: &call_volume_config(),
            accounting_metric: &accounting_metric_config(),
            search_console_overlay: state.search_console_overlay.as_ref().as_ref(),
        },
        &mtd,
        TODAY,
    )
    .expect("assemble mtd");
    assert_eq!(metrics.sales.period_sales_cents, 1_000_000);
    assert_eq!(metrics.sales.prior_period_sales_cents, Some(550_000));
    assert_eq!(metrics.orders.orders_in_period, 1); // May order outside MTD
}

#[test]
fn call_volume_metric_counts_ruby_outcome_buckets() {
    let state = test_state();
    let in_window = ms("2026-06-09") + 1_000;
    {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        crate::slices::email_triage::store::record_inbound_message(
            conn,
            CLIENT,
            &inbound_with_body(
                "ruby-transfer",
                "ruby_call_log",
                in_window,
                "Ruby call summary",
                "Caller: Avery\nTransfer Status: Transfer successful\nDate: 06/09/2026 10:00 AM",
            ),
        )
        .expect("seed transfer");
        add_attention_enrichment(
            conn,
            "ruby-transfer",
            AttentionLevel::Lower,
            "call_handled_live",
        );
        crate::slices::email_triage::store::record_inbound_message(
            conn,
            CLIENT,
            &inbound_with_body(
                "ruby-callback",
                "ruby_call_log",
                in_window + 1,
                "Ruby call summary",
                "Caller: Jordan\nActions: Please contact\nDate: 06/09/2026 10:05 AM",
            ),
        )
        .expect("seed callback");
        add_attention_enrichment(
            conn,
            "ruby-callback",
            AttentionLevel::Higher,
            "callback_needed",
        );
        crate::slices::email_triage::store::record_inbound_message(
            conn,
            CLIENT,
            &inbound_with_body(
                "ruby-unknown",
                "ruby_call_log",
                in_window + 2,
                "Ruby call summary",
                "Caller: Morgan\nMessage: Asked about store hours.",
            ),
        )
        .expect("seed unknown");
        add_attention_enrichment(conn, "ruby-unknown", AttentionLevel::Lower, "unknown");
        crate::slices::email_triage::store::record_inbound_message(
            conn,
            CLIENT,
            &inbound_with_body(
                "ruby-no-callback",
                "ruby_call_log",
                in_window + 3,
                "Ruby call summary",
                "Caller: Taylor\nCallback Requested: No\nMessage: Asked about store hours.",
            ),
        )
        .expect("seed no callback");
        add_attention_enrichment(
            conn,
            "ruby-no-callback",
            AttentionLevel::Lower,
            "no_callback_needed",
        );
    }
    let period = DigestPeriod {
        kind: OwnerReportPeriodKind::Weekly,
        start: WEEK_START.to_string(),
        end: TODAY.to_string(),
    };
    let accounting_status = state
        .sync_guards
        .guard(crate::http::Pump::Accounting)
        .lock()
        .clone();
    let persistence = state.persistence.lock();
    let metrics = service::assemble_metrics(
        persistence.connection_ref(),
        CLIENT,
        &accounting_status,
        service::MetricAssemblyConfig {
            report: &service::OwnerReportConfig {
                report_profile: Some("test_profile".to_string()),
                ..service::OwnerReportConfig::default()
            },
            call_volume: &call_volume_config(),
            accounting_metric: &accounting_metric_config(),
            search_console_overlay: state.search_console_overlay.as_ref().as_ref(),
        },
        &period,
        TODAY,
    )
    .expect("assemble");

    assert_eq!(metrics.calls.call_log_messages, 4);
    assert_eq!(metrics.calls.transfer_successful, 0);
    assert_eq!(metrics.calls.callback_needed, 0);
    assert_eq!(metrics.calls.no_callback_needed, 0);
    assert_eq!(metrics.calls.unknown_outcome, 4);
}

#[test]
fn owner_report_sales_metrics_mirror_configured_adjusted_basis() {
    let state = test_state();
    seed_caches(&state);
    let period = DigestPeriod {
        kind: OwnerReportPeriodKind::Mtd,
        start: MONTH_START.to_string(),
        end: TODAY.to_string(),
    };
    let accounting_status = state
        .sync_guards
        .guard(crate::http::Pump::Accounting)
        .lock()
        .clone();
    let persistence = state.persistence.lock();
    let report_config = service::OwnerReportConfig::default();
    let accounting_metric = crate::slices::accounting::service::AccountingMetricBasisConfig {
        basis: crate::slices::accounting::service::AccountingMetricBasisKind::AdjustedGrossSales,
        label: "Adjusted gross sales".to_string(),
        baseline_cents: Some(900_000),
        freight_cents: Some(10_000),
        taxes_cents: Some(20_000),
        insurance_cents: Some(30_000),
        configured: true,
        ..crate::slices::accounting::service::AccountingMetricBasisConfig::default()
    };

    let metrics = service::assemble_metrics(
        persistence.connection_ref(),
        CLIENT,
        &accounting_status,
        service::MetricAssemblyConfig {
            report: &report_config,
            call_volume: &call_volume_config(),
            accounting_metric: &accounting_metric,
            search_console_overlay: state.search_console_overlay.as_ref().as_ref(),
        },
        &period,
        TODAY,
    )
    .expect("assemble");

    assert_eq!(metrics.sales.metric_basis, "adjusted_gross_sales");
    assert_eq!(metrics.sales.metric_basis_label, "Adjusted gross sales");
    assert_eq!(metrics.sales.period_sales_cents, 1_000_000);
    assert_eq!(metrics.sales.metric_value_cents, Some(940_000));
    assert_eq!(metrics.sales.metric_baseline_cents, Some(900_000));
    assert_eq!(metrics.sales.metric_above_baseline_cents, Some(40_000));
    assert_eq!(metrics.sales.metric_pending_reason, None);
}

#[test]
fn call_volume_metric_is_pending_when_source_config_is_missing() {
    let state = test_state();
    seed_caches(&state);
    let period = DigestPeriod {
        kind: OwnerReportPeriodKind::Weekly,
        start: WEEK_START.to_string(),
        end: TODAY.to_string(),
    };
    let accounting_status = state
        .sync_guards
        .guard(crate::http::Pump::Accounting)
        .lock()
        .clone();
    let persistence = state.persistence.lock();
    let metrics = service::assemble_metrics(
        persistence.connection_ref(),
        CLIENT,
        &accounting_status,
        service::MetricAssemblyConfig {
            report: &service::OwnerReportConfig::default(),
            call_volume: &service::CallVolumeMetricConfig::from_overlay(None),
            accounting_metric: &accounting_metric_config(),
            search_console_overlay: state.search_console_overlay.as_ref().as_ref(),
        },
        &period,
        TODAY,
    )
    .expect("assemble");

    assert!(!metrics.calls.configured);
    assert_eq!(metrics.calls.call_log_messages, 0);
    assert_eq!(metrics.calls.label, "Incoming calls");
    assert!(metrics
        .calls
        .pending_reason
        .as_deref()
        .unwrap_or_default()
        .contains("call-summary email category"));
}

#[test]
fn assemble_metrics_respects_search_console_overlay_configuration() {
    let mut state = test_state();
    state.search_console_overlay = Arc::new(Some(SearchConsoleOverlay {
        property_url: "sc-domain:example.com".to_string(),
        branded_query_patterns: vec!["example".to_string()],
        user_id: String::new(),
        sync_days: None,
        ga4_property_id: String::new(),
        analytics_excluded_referrer_domains: Vec::new(),
    }));
    seed_caches(&state);
    let period = DigestPeriod {
        kind: OwnerReportPeriodKind::Weekly,
        start: WEEK_START.to_string(),
        end: TODAY.to_string(),
    };
    let accounting_status = state
        .sync_guards
        .guard(crate::http::Pump::Accounting)
        .lock()
        .clone();
    let persistence = state.persistence.lock();
    let metrics = service::assemble_metrics(
        persistence.connection_ref(),
        CLIENT,
        &accounting_status,
        service::MetricAssemblyConfig {
            report: &service::OwnerReportConfig::default(),
            call_volume: &call_volume_config(),
            accounting_metric: &accounting_metric_config(),
            search_console_overlay: state.search_console_overlay.as_ref().as_ref(),
        },
        &period,
        TODAY,
    )
    .expect("assemble");

    assert!(metrics.traffic.configured);
    assert_eq!(
        metrics.traffic.property_url.as_deref(),
        Some("sc-domain:example.com")
    );
    assert!(!metrics.traffic.has_data);
}

#[test]
fn assemble_metrics_preserves_ga4_when_search_console_is_unconfigured() {
    let mut state = test_state();
    state.search_console_overlay = Arc::new(Some(SearchConsoleOverlay {
        property_url: String::new(),
        branded_query_patterns: Vec::new(),
        user_id: String::new(),
        sync_days: None,
        ga4_property_id: "123456789".to_string(),
        analytics_excluded_referrer_domains: Vec::new(),
    }));
    seed_caches(&state);
    let period = DigestPeriod {
        kind: OwnerReportPeriodKind::Weekly,
        start: WEEK_START.to_string(),
        end: TODAY.to_string(),
    };
    let accounting_status = state
        .sync_guards
        .guard(crate::http::Pump::Accounting)
        .lock()
        .clone();
    let mut persistence = state.persistence.lock();
    crate::slices::search_console::store::replace_analytics_window(
        persistence.connection(),
        CLIENT,
        "123456789",
        crate::slices::search_console::store::AnalyticsSnapshotWindow {
            start_date: WEEK_START,
            end_date: TODAY,
            daily: &[
                crate::slices::search_console::store::AnalyticsDailyMetricRow {
                    date: TODAY.to_string(),
                    metrics: AnalyticsMetricTotals {
                        sessions: 42,
                        total_users: 31,
                        event_count: 120,
                        conversions: 3,
                    },
                },
            ],
            dimensions: &[
                crate::slices::search_console::store::AnalyticsDimensionMetricRow {
                    date: TODAY.to_string(),
                    dimension_type: "landing_page".to_string(),
                    dimension_value: "/products".to_string(),
                    metrics: AnalyticsMetricTotals {
                        sessions: 20,
                        total_users: 15,
                        event_count: 60,
                        conversions: 2,
                    },
                },
            ],
        },
        now_ms(),
    )
    .expect("seed ga4");

    let metrics = service::assemble_metrics(
        persistence.connection_ref(),
        CLIENT,
        &accounting_status,
        service::MetricAssemblyConfig {
            report: &service::OwnerReportConfig::default(),
            call_volume: &call_volume_config(),
            accounting_metric: &accounting_metric_config(),
            search_console_overlay: state.search_console_overlay.as_ref().as_ref(),
        },
        &period,
        TODAY,
    )
    .expect("assemble");

    assert!(
        !metrics.traffic.configured,
        "organic search remains pending"
    );
    assert!(metrics.traffic.behavior_configured);
    assert!(metrics.traffic.behavior_has_data);
    assert_eq!(metrics.traffic.behavior_week.sessions, 42);
    assert_eq!(metrics.traffic.behavior_week.conversions, 3);
    assert_eq!(metrics.traffic.top_landing_pages_week[0].value, "/products");
}

#[test]
fn assemble_metrics_excludes_referrer_spam_from_ga4_report_metrics() {
    let mut state = test_state();
    state.search_console_overlay = Arc::new(Some(SearchConsoleOverlay {
        property_url: String::new(),
        branded_query_patterns: Vec::new(),
        user_id: String::new(),
        sync_days: None,
        ga4_property_id: "123456789".to_string(),
        analytics_excluded_referrer_domains: vec!["trafficheap.cc".to_string()],
    }));
    seed_caches(&state);
    let period = DigestPeriod {
        kind: OwnerReportPeriodKind::Weekly,
        start: WEEK_START.to_string(),
        end: TODAY.to_string(),
    };
    let accounting_status = state
        .sync_guards
        .guard(crate::http::Pump::Accounting)
        .lock()
        .clone();
    let mut persistence = state.persistence.lock();
    crate::slices::search_console::store::replace_analytics_window(
        persistence.connection(),
        CLIENT,
        "123456789",
        crate::slices::search_console::store::AnalyticsSnapshotWindow {
            start_date: MONTH_START,
            end_date: TODAY,
            daily: &[
                crate::slices::search_console::store::AnalyticsDailyMetricRow {
                    date: TODAY.to_string(),
                    metrics: AnalyticsMetricTotals {
                        sessions: 15,
                        total_users: 12,
                        event_count: 90,
                        conversions: 3,
                    },
                },
            ],
            dimensions: &[
                crate::slices::search_console::store::AnalyticsDimensionMetricRow {
                    date: TODAY.to_string(),
                    dimension_type: "source_medium".to_string(),
                    dimension_value: "google / organic".to_string(),
                    metrics: AnalyticsMetricTotals {
                        sessions: 10,
                        total_users: 8,
                        event_count: 50,
                        conversions: 2,
                    },
                },
                crate::slices::search_console::store::AnalyticsDimensionMetricRow {
                    date: TODAY.to_string(),
                    dimension_type: "source_medium".to_string(),
                    dimension_value: "trafficheap.cc / referral".to_string(),
                    metrics: AnalyticsMetricTotals {
                        sessions: 5,
                        total_users: 4,
                        event_count: 40,
                        conversions: 1,
                    },
                },
            ],
        },
        now_ms(),
    )
    .expect("seed ga4");

    let metrics = service::assemble_metrics(
        persistence.connection_ref(),
        CLIENT,
        &accounting_status,
        service::MetricAssemblyConfig {
            report: &service::OwnerReportConfig::default(),
            call_volume: &call_volume_config(),
            accounting_metric: &accounting_metric_config(),
            search_console_overlay: state.search_console_overlay.as_ref().as_ref(),
        },
        &period,
        TODAY,
    )
    .expect("assemble");

    assert!(metrics.traffic.behavior_has_data);
    assert_eq!(metrics.traffic.behavior_week.sessions, 10);
    assert_eq!(metrics.traffic.behavior_week.conversions, 2);
    assert_eq!(metrics.traffic.behavior_month_to_date.sessions, 10);
    assert_eq!(
        metrics
            .traffic
            .top_sources_week
            .iter()
            .map(|row| row.value.as_str())
            .collect::<Vec<_>>(),
        vec!["google / organic"]
    );

    let report =
        service::report_from_parts(&period, metrics, Err("llm_down".to_string()), now_ms());
    let (_subject, body) = service::render_digest_email(&report);
    assert!(body.contains("Website behavior: 10 sessions, 8 users, 2 conversions"));
    assert!(body.contains("Top acquisition sources: google / organic (10 sessions)"));
    assert!(!body.contains("trafficheap.cc"));

    let raw = crate::slices::search_console::store::sum_analytics_daily(
        persistence.connection_ref(),
        CLIENT,
        "123456789",
        TODAY,
        TODAY,
    )
    .expect("raw sum");
    assert_eq!(raw.sessions, 15, "raw GA4 cache stays unchanged");
}

#[test]
fn assemble_metrics_uses_selected_search_console_property_not_latest_synced() {
    let state = test_state();
    seed_caches(&state);
    {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        search_console_store::replace_discovered_properties(
            conn,
            CLIENT,
            &[
                bos_integrations::google_search_console::SearchConsoleSite {
                    site_url: "sc-domain:selected.example".to_string(),
                    permission_level: "siteOwner".to_string(),
                },
                bos_integrations::google_search_console::SearchConsoleSite {
                    site_url: "sc-domain:latest.example".to_string(),
                    permission_level: "siteOwner".to_string(),
                },
            ],
            1,
        )
        .expect("properties");
        search_console_store::select_property(
            conn,
            search_console_store::PropertySelectionContext {
                client_id: CLIENT,
                actor_id: "operator",
                expected_revision: None,
                idempotency_key: "select-search-console-property",
                now_ms: 2,
            },
            "sc-domain:selected.example",
        )
        .expect("select");
        search_console_store::replace_window(
            conn,
            CLIENT,
            "sc-domain:selected.example",
            search_console_store::SnapshotWindow {
                start_date: WEEK_START,
                end_date: TODAY,
                daily: &[search_console_store::DailyMetricRow {
                    date: TODAY.to_string(),
                    metrics: sc_totals(5, 50),
                }],
                dimensions: &[],
            },
            3,
        )
        .expect("selected window");
        search_console_store::replace_window(
            conn,
            CLIENT,
            "sc-domain:latest.example",
            search_console_store::SnapshotWindow {
                start_date: WEEK_START,
                end_date: TODAY,
                daily: &[search_console_store::DailyMetricRow {
                    date: TODAY.to_string(),
                    metrics: sc_totals(99, 990),
                }],
                dimensions: &[],
            },
            4,
        )
        .expect("latest window");
        search_console_store::put_cursor(
            conn,
            CLIENT,
            "sc-domain:selected.example",
            &search_console_store::SearchConsoleCursor {
                last_synced_at_ms: Some(10),
                ..Default::default()
            },
            5,
        )
        .expect("selected cursor");
        search_console_store::put_cursor(
            conn,
            CLIENT,
            "sc-domain:latest.example",
            &search_console_store::SearchConsoleCursor {
                last_synced_at_ms: Some(100),
                ..Default::default()
            },
            6,
        )
        .expect("latest cursor");
    }
    let period = DigestPeriod {
        kind: OwnerReportPeriodKind::Weekly,
        start: WEEK_START.to_string(),
        end: TODAY.to_string(),
    };
    let accounting_status = state
        .sync_guards
        .guard(crate::http::Pump::Accounting)
        .lock()
        .clone();
    let persistence = state.persistence.lock();
    let metrics = service::assemble_metrics(
        persistence.connection_ref(),
        CLIENT,
        &accounting_status,
        service::MetricAssemblyConfig {
            report: &service::OwnerReportConfig::default(),
            call_volume: &call_volume_config(),
            accounting_metric: &accounting_metric_config(),
            search_console_overlay: None,
        },
        &period,
        TODAY,
    )
    .expect("assemble");

    assert_eq!(
        metrics.traffic.property_url.as_deref(),
        Some("sc-domain:selected.example")
    );
    assert_eq!(metrics.traffic.totals.clicks, 5);
}

#[test]
fn call_volume_metric_counts_when_category_is_configured_without_gmail_metadata() {
    let state = test_state();
    seed_caches(&state);
    let period = DigestPeriod {
        kind: OwnerReportPeriodKind::Weekly,
        start: WEEK_START.to_string(),
        end: TODAY.to_string(),
    };
    let accounting_status = state
        .sync_guards
        .guard(crate::http::Pump::Accounting)
        .lock()
        .clone();
    let persistence = state.persistence.lock();
    let config = service::CallVolumeMetricConfig::from_overlay(Some(&OwnerReportsOverlay {
        call_volume: CallVolumeMetricOverlay {
            category_id: "ruby_call_log".to_string(),
            label: String::new(),
            source_label: String::new(),
            gmail_label: String::new(),
            gmail_query: String::new(),
        },
        ..OwnerReportsOverlay::default()
    }));
    let metrics = service::assemble_metrics(
        persistence.connection_ref(),
        CLIENT,
        &accounting_status,
        service::MetricAssemblyConfig {
            report: &service::OwnerReportConfig::default(),
            call_volume: &config,
            accounting_metric: &accounting_metric_config(),
            search_console_overlay: state.search_console_overlay.as_ref().as_ref(),
        },
        &period,
        TODAY,
    )
    .expect("assemble");

    assert!(metrics.calls.configured);
    assert_eq!(metrics.calls.call_log_messages, 1);
    assert_eq!(metrics.calls.pending_reason, None);
}

#[test]
fn assemble_metrics_respects_the_report_metric_profile() {
    let state = test_state();
    seed_caches(&state);
    let period = DigestPeriod {
        kind: OwnerReportPeriodKind::Weekly,
        start: WEEK_START.to_string(),
        end: TODAY.to_string(),
    };
    let accounting_status = state
        .sync_guards
        .guard(crate::http::Pump::Accounting)
        .lock()
        .clone();
    let report_config = service::OwnerReportConfig {
        metrics: vec![ReportMetricSection::Sales, ReportMetricSection::SiteTraffic],
        ..service::OwnerReportConfig::default()
    };
    let persistence = state.persistence.lock();
    let metrics = service::assemble_metrics(
        persistence.connection_ref(),
        CLIENT,
        &accounting_status,
        service::MetricAssemblyConfig {
            report: &report_config,
            call_volume: &call_volume_config(),
            accounting_metric: &accounting_metric_config(),
            search_console_overlay: state.search_console_overlay.as_ref().as_ref(),
        },
        &period,
        TODAY,
    )
    .expect("assemble");

    assert_eq!(
        metrics.metric_sections,
        vec!["sales".to_string(), "site_traffic".to_string()]
    );
    assert!(!metrics.calls.configured);
    assert_eq!(
        metrics.calls.pending_reason.as_deref(),
        Some("Call-volume reporting is not part of this report profile.")
    );
    assert!(!metrics.orders.configured);
    assert_eq!(metrics.orders.orders_in_period, 0);
    assert!(!metrics.inventory.configured);
    assert_eq!(
        metrics.inventory.pending_reason.as_deref(),
        Some("Inventory reporting is not part of this report profile.")
    );
    assert!(!metrics.claims.configured);
    assert_eq!(metrics.claims.damage_events_in_period, 0);
    assert_eq!(metrics.deals, DigestDealMetrics::default());
}

#[test]
fn espocrm_deal_metrics_do_not_read_hubspot_config() {
    let metrics = service::assemble_deal_metrics_for_provider("espocrm", ms(WEEK_START), ms(TODAY));
    assert_eq!(metrics.status, DigestDealMetricsStatus::PendingConfig);
    assert_eq!(metrics.source, "espocrm_business_metrics");
    assert!(metrics.message.contains("EspoCRM"));
}

#[test]
fn hubspot_deal_config_falls_back_to_saved_dashboard_mapping() {
    let _hubspot_token = EnvGuard::unset("BOS_HUBSPOT_ACCESS_TOKEN");
    let mut persistence = crate::persistence::Persistence::open_in_memory().expect("db");
    crate::slices::home_dashboard::store::save_hubspot_deal_mapping(
        persistence.connection(),
        CLIENT,
        "owner-report-test",
        &HubSpotDealPipelineMappingSaveRequest {
            mapping: HubSpotDealPipelineMapping {
                pipeline_id: "pipeline-from-setup".to_string(),
                stage_mappings: vec![
                    HubSpotDealStageMapping {
                        stage_id: "qualified".to_string(),
                        label: Some("Qualified".to_string()),
                        status: HubSpotDealMappedStatus::Open,
                    },
                    HubSpotDealStageMapping {
                        stage_id: "closedwon".to_string(),
                        label: Some("Closed won".to_string()),
                        status: HubSpotDealMappedStatus::Won,
                    },
                    HubSpotDealStageMapping {
                        stage_id: "closedlost".to_string(),
                        label: Some("Closed lost".to_string()),
                        status: HubSpotDealMappedStatus::Lost,
                    },
                ],
                started_date_property: "createdate".to_string(),
                closed_date_property: "closedate".to_string(),
            },
            expected_revision: None,
            idempotency_key: "hubspot-deal-config-fallback".to_string(),
            actor_id: None,
        },
        ms(TODAY),
    )
    .expect("save mapping");

    let config = service::hubspot_deal_config_for_client(persistence.connection_ref(), CLIENT)
        .expect("config");

    assert_eq!(config.pipeline_id.as_deref(), Some("pipeline-from-setup"));
    assert_eq!(config.open_stage_ids, vec!["qualified"]);
    assert_eq!(config.won_stage_ids, vec!["closedwon"]);
    assert_eq!(config.lost_stage_ids, vec!["closedlost"]);
    assert_eq!(config.started_date_property.as_deref(), Some("createdate"));
    assert_eq!(config.closed_date_property.as_deref(), Some("closedate"));
}

// ---------------------------------------------------------------------------
// Generation cycle (mocked narrator — never a live LLM)
// ---------------------------------------------------------------------------

fn ok_narrator(
    calls: &RefCell<u32>,
) -> impl Fn(&DigestPeriod, &OwnerDigestMetrics, &str) -> Result<(DigestNarration, String), String> + '_
{
    move |_, _, _| {
        *calls.borrow_mut() += 1;
        Ok((
            DigestNarration {
                headline: "Steady week.".to_string(),
                narrative: "Nothing unusual.".to_string(),
                callouts: vec![],
                confidence: "high".to_string(),
            },
            "test-model".to_string(),
        ))
    }
}

#[test]
fn cycle_generates_then_skips_fresh_periods() {
    let state = state_with_call_volume_config();
    seed_caches(&state);
    let calls = RefCell::new(0u32);
    let narrator = ok_narrator(&calls);

    let summary = worker::run_cycle_with(&state, false, now_ms(), &narrator).expect("cycle");
    assert_eq!(summary.generated, 2);
    assert_eq!(summary.skipped, 0);
    assert_eq!(*calls.borrow(), 2);

    // Same day, not forced: both periods are fresh — no LLM calls.
    let summary = worker::run_cycle_with(&state, false, now_ms(), &narrator).expect("cycle");
    assert_eq!(summary.generated, 0);
    assert_eq!(summary.skipped, 2);
    assert_eq!(*calls.borrow(), 2);

    // Forced (Generate-now): regenerates both.
    let summary = worker::run_cycle_with(&state, true, now_ms(), &narrator).expect("cycle");
    assert_eq!(summary.generated, 2);
    assert_eq!(*calls.borrow(), 4);

    let persistence = state.persistence.lock();
    let reports =
        store::list_reports(persistence.connection_ref(), CLIENT, None, 10).expect("list");
    assert_eq!(reports.len(), 2);
    assert!(reports
        .iter()
        .all(|entry| entry.report.status == OwnerReportStatus::Complete));
    let weekly = store::list_reports(persistence.connection_ref(), CLIENT, Some("weekly"), 10)
        .expect("list weekly");
    assert_eq!(weekly.len(), 1);
    assert_eq!(weekly[0].report.headline.as_deref(), Some("Steady week."));
}

#[test]
fn narration_failure_still_stores_the_metrics() {
    let state = state_with_call_volume_config();
    seed_caches(&state);
    let narrator = |_: &DigestPeriod, _: &OwnerDigestMetrics, _: &str| Err("llm_down".to_string());
    let summary = worker::run_cycle_with(&state, false, now_ms(), &narrator).expect("cycle");
    assert_eq!(summary.generated, 2);
    assert_eq!(summary.narration_failures, 2);
    let persistence = state.persistence.lock();
    let reports =
        store::list_reports(persistence.connection_ref(), CLIENT, None, 10).expect("list");
    assert_eq!(reports.len(), 2);
    for entry in &reports {
        assert_eq!(entry.report.status, OwnerReportStatus::NarrationFailed);
        assert_eq!(entry.report.narration_error.as_deref(), Some("llm_down"));
        assert!(entry.report.headline.is_none());
        // The deterministic numbers survive a narration failure.
        assert_eq!(entry.report.metrics.calls.call_log_messages, 1);
    }
}

// ---------------------------------------------------------------------------
// Email staging lifecycle
// ---------------------------------------------------------------------------

#[test]
fn email_stages_once_per_generation() {
    let state = test_state();
    let period = DigestPeriod {
        kind: OwnerReportPeriodKind::Weekly,
        start: WEEK_START.to_string(),
        end: TODAY.to_string(),
    };
    let narration = DigestNarration {
        headline: "Steady week.".to_string(),
        narrative: "Sales were $4,520.00.".to_string(),
        callouts: vec!["2 follow-ups overdue".to_string()],
        confidence: "high".to_string(),
    };
    let report = service::report_from_parts(
        &period,
        sample_metrics(),
        Ok((narration, "test-model".to_string())),
        now_ms(),
    );
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    store::upsert_report(conn, CLIENT, store::PUMP_ACTOR, &report).expect("upsert");

    // The rendered email carries the deterministic figures + pending rows.
    let (subject, body) = service::render_digest_email(&report);
    assert!(subject.contains("Week of 2026-06-08"));
    assert!(body.contains("$4,520.00"));
    assert!(body.contains("INVENTORY"));
    assert!(body.contains("Stocked SKUs: 12 (1 out, 2 critical)"));
    assert!(body.contains("Stocked valuation: $8,000.00"));
    assert!(body.contains("Inbound on open POs: $2,500.00"));
    assert!(body.contains("configure Search Console property/access"));
    assert!(body.contains("Behavior analytics pending"));
    assert!(body.contains("Conversion tracking pending"));
    assert!(body.contains("Retargeting pending"));

    let job = service::build_email_job(&report, "owners@example.com", None, "operator", now_ms())
        .expect("job");
    let ctx = |key: &'static str| EmailActionContext {
        client_id: CLIENT,
        actor_id: "operator",
        actor_kind: ActorKindDto::Operator,
        expected_revision: None,
        idempotency_key: key,
        now_ms: now_ms(),
    };
    store::stage_email(conn, ctx("email-1"), &report.report_id, &job).expect("stage");
    let staged = store::get_report(conn, CLIENT, &report.report_id)
        .expect("get")
        .expect("exists");
    assert_eq!(
        staged.report.outbox_job_id.as_deref(),
        Some(job.job_id.as_str())
    );

    // A retry with the same idempotency key replays instead of being blocked
    // by the already-staged guard.
    let replay =
        store::stage_email(conn, ctx("email-1"), &report.report_id, &job).expect("replay stage");
    assert!(matches!(replay, MutationOutcome::ReplayedIdempotent { .. }));

    // A second stage on the same generation refuses.
    let err = store::stage_email(conn, ctx("email-2"), &report.report_id, &job).unwrap_err();
    assert!(err
        .to_string()
        .contains("owner_report_email_already_staged"));

    // Regeneration resets the email association (fresh digest, not yet sent).
    let mut regenerated = report.clone();
    regenerated.generated_at_ms = now_ms() + 60_000;
    regenerated.as_of_date = "2026-06-11".to_string();
    store::upsert_report(conn, CLIENT, store::PUMP_ACTOR, &regenerated).expect("regenerate");
    let fresh = store::get_report(conn, CLIENT, &report.report_id)
        .expect("get")
        .expect("exists");
    assert!(fresh.report.outbox_job_id.is_none());
    let job2 = service::build_email_job(
        &regenerated,
        "owners@example.com",
        None,
        "operator",
        regenerated.generated_at_ms,
    )
    .expect("job2");
    assert_ne!(job.job_id, job2.job_id);
    store::stage_email(conn, ctx("email-3"), &report.report_id, &job2).expect("stage again");
}

#[test]
fn scheduled_delivery_stages_due_reports_once() {
    let mut state = test_state();
    state.owner_reports_overlay = std::sync::Arc::new(Some(crate::overlay::OwnerReportsOverlay {
        delivery_enabled: true,
        recipients: vec!["owners@example.com".to_string()],
        weekly_weekday: Some("wednesday".to_string()),
        mtd_day: None,
        metrics: vec!["sales".to_string(), "site_traffic".to_string()],
        subject_prefix: Some("Owner update".to_string()),
        ..crate::overlay::OwnerReportsOverlay::default()
    }));
    seed_caches(&state);
    let calls = RefCell::new(0u32);
    let narrator = ok_narrator(&calls);

    let summary = worker::run_cycle_with(&state, false, now_ms(), &narrator).expect("cycle");
    assert_eq!(summary.generated, 2);
    assert_eq!(summary.delivered, 1);
    assert_eq!(summary.delivery_skipped, 0);

    let persistence = state.persistence.lock();
    let weekly = store::list_reports(persistence.connection_ref(), CLIENT, Some("weekly"), 10)
        .expect("weekly");
    let job_id = weekly[0]
        .report
        .outbox_job_id
        .as_deref()
        .expect("scheduled email staged");
    let payload_json: String = persistence
        .connection_ref()
        .query_row(
            "SELECT payload_json FROM outbox_jobs WHERE client_id = ?1 AND job_id = ?2",
            rusqlite::params![CLIENT, job_id],
            |row| row.get(0),
        )
        .expect("payload");
    let payload: bos_integrations::gmail_draft_write::GmailDraftCreateOutboxPayload =
        serde_json::from_str(&payload_json).expect("payload json");
    assert_eq!(payload.to, "owners@example.com");
    assert!(payload.subject.starts_with("Owner update"));
    assert!(payload.body_text.contains("SITE TRAFFIC"));

    drop(persistence);
    let summary = worker::run_cycle_with(&state, false, now_ms(), &narrator).expect("cycle");
    assert_eq!(summary.generated, 0);
    assert_eq!(summary.delivered, 0);
    assert_eq!(summary.delivery_skipped, 1);
}

#[test]
fn forced_manual_generation_does_not_schedule_delivery() {
    let mut state = test_state();
    state.owner_reports_overlay = std::sync::Arc::new(Some(crate::overlay::OwnerReportsOverlay {
        delivery_enabled: true,
        recipients: vec!["owners@example.com".to_string()],
        weekly_weekday: Some("wednesday".to_string()),
        mtd_day: Some(10),
        metrics: Vec::new(),
        subject_prefix: None,
        ..crate::overlay::OwnerReportsOverlay::default()
    }));
    seed_caches(&state);
    let calls = RefCell::new(0u32);
    let narrator = ok_narrator(&calls);

    let summary = worker::run_cycle_with(&state, true, now_ms(), &narrator).expect("cycle");
    assert_eq!(summary.generated, 2);
    assert_eq!(summary.delivered, 0);

    let persistence = state.persistence.lock();
    let outbox_count: i64 = persistence
        .connection_ref()
        .query_row("SELECT COUNT(*) FROM outbox_jobs", [], |row| row.get(0))
        .expect("outbox count");
    assert_eq!(outbox_count, 0);
}

#[test]
fn rendered_email_includes_available_hubspot_deal_metrics() {
    let period = DigestPeriod {
        kind: OwnerReportPeriodKind::Weekly,
        start: WEEK_START.to_string(),
        end: TODAY.to_string(),
    };
    let mut metrics = sample_metrics();
    metrics.deals = DigestDealMetrics {
        status: DigestDealMetricsStatus::Available,
        source: "hubspot_deals".to_string(),
        message: "Computed from HubSpot deals closed in this period.".to_string(),
        closed_deals: Some(4),
        won_deals: Some(3),
        lost_deals: Some(1),
        close_rate_bps: Some(7_500),
        avg_contact_to_close_days: Some(18),
        contact_to_close_sample: Some(4),
        segment_cuts: vec!["dealtype:commercial=3".to_string()],
    };
    let report = service::report_from_parts(
        &period,
        metrics,
        Err("not testing narration".to_string()),
        now_ms(),
    );
    let (_, body) = service::render_digest_email(&report);
    assert!(body.contains("HUBSPOT DEALS"));
    assert!(body.contains("Close rate: 75.0%"));
    assert!(body.contains("Avg contact-to-close: 18 days"));
    assert!(body.contains("dealtype:commercial=3"));
    assert!(!body.contains("Close rate / contact-to-close: pending"));
}
