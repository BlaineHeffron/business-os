//! Slice tests: provenance-grounded line amounts (one ungrounded line
//! refuses the whole fill), server-side total math, the staged → approved
//! lifecycle (provider outbox job in the same tx; approval requires a
//! customer email + non-zero total), edits recomputing totals, and the
//! dry-run delivery path. No live LLM, no network.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use bos_contracts::enrichment::EnrichmentPlan;
use bos_contracts::invoice_drafts::{
    InvoiceDraftLineItem, InvoiceDraftStatus, InvoiceSettingsUpdateRequest,
};
use bos_contracts::operator_notes::OperatorNote;
use bos_contracts::work_queue::{WorkItem, WorkItemStatus};
use bos_integrations::accounting_read::{CustomerRecord, InvoiceRecord, TierSource};
use bos_integrations::invoice_ninja::InvoiceNinjaInvoiceDraftOutboxPayload;
use bos_integrations::llm_typed_tasks::{TypedLlmRawOutputRetention, TypedLlmRedactionPolicy};
use bos_integrations::stripe::{StripeInvoiceDraftOutboxPayload, StripeWriteConfig};
use serde_json::json;
use tower::ServiceExt;

use super::service;
use super::store::{self, DraftActionContext};
use crate::http::{build_router, test_support::test_state};
use crate::outbox::{AttemptOutcome, ClaimedJob};
use crate::slices::enrichment::store as enrichment_store;

const CLIENT: &str = "test-client";

#[test]
fn invoice_fill_request_includes_background_when_present() {
    use bos_integrations::llm_typed_tasks::TypedLlmTextBlock;
    let item = work_item("wi_bg");
    let message = bos_contracts::email_triage::InboundMessageRecord {
        source_key: "m_bg".to_string(),
        message_id: "m_bg".to_string(),
        thread_id: None,
        internal_date_ms: Some(1_781_000_000_000),
        from_addr: Some("a@test".to_string()),
        to_addr: Some("b@test".to_string()),
        subject: Some("Invoice please".to_string()),
        body_excerpt: "Please bill us $500 for the repaint.".to_string(),
        body_full: String::new(),
        headers: Vec::new(),
        labels: Vec::new(),
        resolved_category: "operator_note".to_string(),
        matched_rule_id: None,
        ingested_at_ms: 1,
        ai_triage_status: None,
        ai_triage_rationale: None,
        attachments: Vec::new(),
        source_user_id: None,
    };

    let plain = service::build_invoice_fill_request(
        CLIENT,
        &item,
        &message,
        &json!({ "background": null }),
        1,
    );
    assert!(!plain
        .input
        .text_blocks
        .iter()
        .any(|b| b.block_id == "background"));

    let block = TypedLlmTextBlock {
        block_id: "background".to_string(),
        text: "Company: Example Company".to_string(),
    };
    let context = json!({ "background": serde_json::to_value(&block).unwrap() });
    let grounded = service::build_invoice_fill_request(CLIENT, &item, &message, &context, 1);
    let backgrounds: Vec<_> = grounded
        .input
        .text_blocks
        .iter()
        .filter(|b| b.block_id == "background")
        .collect();
    assert_eq!(backgrounds.len(), 1);
    assert_eq!(backgrounds[0].text, "Company: Example Company");
}

#[test]
fn invoice_grounding_exact_email_grafts_customer_and_records_evidence() {
    use crate::produce::ProduceFlavor;

    let state = test_state();
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let mut item = work_item("wi_grounded_invoice");
    item.source_kind = "email".to_string();
    item.source_ref = "m_grounded_invoice".to_string();
    crate::slices::work_queue::store::insert_item(conn, CLIENT, &item).expect("work item");
    crate::slices::accounting::store::upsert_customer_snapshots(
        conn,
        CLIENT,
        &[CustomerRecord {
            customer_id: "cust_1".to_string(),
            display_name: "Acme Ops".to_string(),
            company_name: Some("Acme Repair".to_string()),
            email: Some("ap@business-86b318398f.test".to_string()),
            phone: None,
            active: true,
            tier_raw: None,
            tier_source: TierSource::NotProvided,
            updated_at: Some("2026-06-01T00:00:00Z".to_string()),
        }],
        1_000,
    )
    .expect("customer");
    crate::slices::accounting::store::upsert_invoice_snapshots(
        conn,
        CLIENT,
        &[InvoiceRecord {
            invoice_id: "inv_cache_1".to_string(),
            doc_number: Some("INV-100".to_string()),
            customer_id: Some("cust_1".to_string()),
            customer_name: Some("Acme Repair".to_string()),
            txn_date: Some("2026-06-01".to_string()),
            due_date: Some("2026-06-15".to_string()),
            total_amt_cents: 50_000,
            balance_cents: 25_000,
            voided: false,
            updated_at: "2026-06-01T00:00:00Z".to_string(),
        }],
        1_000,
    )
    .expect("invoice");
    let message = bos_contracts::email_triage::InboundMessageRecord {
        source_key: "m_grounded_invoice".to_string(),
        message_id: "m_grounded_invoice".to_string(),
        thread_id: None,
        internal_date_ms: Some(2_000),
        from_addr: Some("ap@business-86b318398f.test".to_string()),
        to_addr: Some("billing@example.test".to_string()),
        subject: Some("Please invoice us".to_string()),
        body_excerpt: "Please invoice us $500 for the work.".to_string(),
        body_full: String::new(),
        headers: Vec::new(),
        labels: Vec::new(),
        resolved_category: "billing".to_string(),
        matched_rule_id: None,
        ingested_at_ms: 2_000,
        ai_triage_status: None,
        ai_triage_rationale: None,
        attachments: Vec::new(),
        source_user_id: None,
    };
    let mut response = grounded_response();
    response["customer_email"] = serde_json::Value::Null;
    response["customer_name"] = serde_json::Value::String("Acme".to_string());
    let context = service::Produce
        .prepare_context(
            conn,
            CLIENT,
            &item,
            &message,
            &crate::http::OperatorScope::All,
            "operator",
        )
        .expect("context");
    drop(persistence);
    let context = service::Produce.enrich_context_unlocked(crate::produce::EnrichContext {
        state: &state,
        item: &item,
        message: &message,
        scope: &crate::http::OperatorScope::All,
        actor_id: "operator",
        actor_kind: bos_contracts::receipt::ActorKindDto::Operator,
        context,
        attempt: 1,
        now_ms: 1_000,
    });
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    service::Produce
        .stage(crate::produce::StageContext {
            conn: &mut *conn,
            client_id: CLIENT,
            actor_id: "operator",
            item: &item,
            message: &message,
            response: &response,
            context: &context,
            model: "model",
            attempt: 1,
            idempotency_key: "grounded_stage",
            now_ms: 3_000,
        })
        .expect("stage");
    let draft = store::active_draft_for_item(conn, CLIENT, &item.item_id)
        .expect("draft")
        .expect("present")
        .draft;
    assert_eq!(
        draft.customer_email.as_deref(),
        Some("ap@business-86b318398f.test")
    );
    assert_eq!(draft.customer_name, "Acme Repair");
    let evidence =
        crate::slices::grounding::grounding_evidence_for_item(conn, CLIENT, &item.item_id)
            .expect("evidence");
    assert!(evidence
        .iter()
        .any(|row| row.tool_name == crate::slices::grounding::TOOL_RESOLVE_PARTY));
    assert!(evidence
        .iter()
        .any(|row| row.tool_name == crate::slices::grounding::TOOL_CUSTOMER_INVOICE_HISTORY));
}

fn work_item(item_id: &str) -> WorkItem {
    WorkItem {
        item_id: item_id.to_string(),
        source_kind: "operator_note".to_string(),
        source_ref: format!("note_{item_id}"),
        category_id: "billing".to_string(),
        title: "Bill Dana for June".to_string(),
        summary: String::new(),
        packet_kinds: vec!["invoice_draft".to_string()],
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

fn grounded_response() -> serde_json::Value {
    json!({
        "customer_name": "Dana Co",
        "customer_email": "dana@example.com",
        "line_items": [
            {
                "line_number": 1,
                "label": "Consulting",
                "description": "June engagement",
                "quantity": 2,
                "unit_amount_cents": 50_000,
            },
            {
                "line_number": 2,
                "label": "Materials",
                "quantity": 1,
                "unit_amount_cents": 25_000,
            },
        ],
        "due_date": "2026-07-01",
        "memo": "June consulting",
        "confidence": "high",
        "provenance": [
            { "field": "customer_name", "quote": "bill Dana Co" },
            { "field": "line_1_amount", "quote": "$500 per session, two sessions" },
            { "field": "line_2_amount", "quote": "materials were $250" },
        ],
    })
}

#[test]
fn fill_parses_grounded_lines_and_recomputes_totals() {
    let fill = service::parse_invoice_fill_response(&grounded_response()).expect("grounded");
    assert_eq!(fill.customer_name, "Dana Co");
    assert_eq!(fill.customer_email.as_deref(), Some("dana@example.com"));
    assert_eq!(fill.line_items.len(), 2);
    // Totals are server math, never the model's.
    assert_eq!(fill.line_items[0].line_total_cents, 100_000);
    assert_eq!(fill.line_items[1].line_total_cents, 25_000);
    let draft = service::draft_from_fill(&work_item("itm_1"), &fill, 1, "test-model", 1_000);
    assert_eq!(draft.subtotal_cents, 125_000);
    assert_eq!(draft.total_cents, 125_000);
    assert_eq!(draft.currency, "usd");
    assert_eq!(draft.status, InvoiceDraftStatus::Staged);
}

#[test]
fn one_ungrounded_line_refuses_the_whole_fill() {
    let mut response = grounded_response();
    // Remove line 2's provenance: the entire fill must be refused — dropping
    // the line silently would under-invoice.
    response["provenance"] = json!([
        { "field": "customer_name", "quote": "bill Dana Co" },
        { "field": "line_1_amount", "quote": "$500 per session, two sessions" },
    ]);
    let err = service::parse_invoice_fill_response(&response).unwrap_err();
    assert!(err.contains("line 2"), "unexpected error: {err}");

    // A quote that does not contain the amount is equally ungrounded.
    let mut response = grounded_response();
    response["provenance"][2] = json!({ "field": "line_2_amount", "quote": "some materials" });
    assert!(service::parse_invoice_fill_response(&response).is_err());
}

#[test]
fn fill_requires_lines_and_valid_fields() {
    let mut response = grounded_response();
    response["line_items"] = json!([]);
    assert!(service::parse_invoice_fill_response(&response).is_err());
    let mut response = grounded_response();
    response["customer_name"] = json!("");
    assert!(service::parse_invoice_fill_response(&response).is_err());
    // Bad email degrades to None (operator fills it before approval).
    let mut response = grounded_response();
    response["customer_email"] = json!("not-an-email");
    let fill = service::parse_invoice_fill_response(&response).expect("parses");
    assert!(fill.customer_email.is_none());
    // Human due dates require explicit context, then normalize to canonical.
    let mut response = grounded_response();
    response["due_date"] = json!("July 1st");
    assert!(service::parse_invoice_fill_response(&response).is_err());
    let date_context =
        crate::slices::datetime_input::DateInputContext::from_epoch_ms(1_781_000_000_000, "UTC");
    let fill = service::parse_invoice_fill_response_with_context(&response, Some(&date_context))
        .expect("parses with source context");
    assert_eq!(fill.due_date.as_deref(), Some("2026-07-01"));
}

fn staged_draft(state: &crate::http::AppState, item_id: &str) -> String {
    let fill = service::parse_invoice_fill_response(&grounded_response()).expect("grounded");
    let draft = service::draft_from_fill(&work_item(item_id), &fill, 1, "test-model", 1_000);
    let mut persistence = state.persistence.lock();
    store::insert_draft(
        persistence.connection(),
        CLIENT,
        "operator",
        &draft,
        &format!("stage:{}", draft.draft_id),
    )
    .expect("insert");
    draft.draft_id
}

fn insert_invoice_freshness_source(conn: &mut rusqlite::Connection, item: &WorkItem) {
    crate::slices::operator_notes::store::insert_note(
        conn,
        CLIENT,
        &OperatorNote {
            note_id: item.source_ref.clone(),
            body: "Please invoice Dana Co for June work. Website business-e743230c8d.test."
                .to_string(),
            category_id: "billing".to_string(),
            created_by: "operator".to_string(),
            created_at_ms: 1,
        },
        &format!("note:{}", item.source_ref),
    )
    .expect("note");
    crate::slices::work_queue::store::insert_item(conn, CLIENT, item).expect("item");
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
async fn enrich_route_rejects_research_mode_until_runner_exists() {
    let router = build_router(test_state());
    let response = router
        .oneshot(
            Request::post("/api/invoice-drafts/draft_1/enrich")
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
    assert_eq!(response_error(response).await, "research_mode_unavailable");
}

#[test]
fn invoice_freshness_candidates_require_missing_email_and_skip_current_bucket() {
    let state = test_state();
    let adapter = crate::slices::enrichment::service::registered_freshness_adapters()
        .iter()
        .find(|adapter| adapter.subject_id == "invoice_customer")
        .expect("invoice adapter");
    let stale_after_ms = 30 * 24 * 60 * 60 * 1000;
    let now_ms = stale_after_ms * 2;
    let item = work_item("wi_invoice_freshness");
    let mut fill = service::parse_invoice_fill_response(&grounded_response()).expect("fill");
    fill.customer_email = None;
    let draft = service::draft_from_fill(&item, &fill, 1, "test-model", 1_000);

    {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        insert_invoice_freshness_source(conn, &item);
        store::insert_draft(conn, CLIENT, "operator", &draft, "stage:freshness").expect("insert");
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
                slice_id: "invoice_drafts",
                draft_id: &candidates[0].draft_id,
                item_id: &candidates[0].item_id,
                plan: &EnrichmentPlan {
                    subject: "invoice_customer".to_string(),
                    fields: Vec::new(),
                    seed_evidence: Vec::new(),
                    enabled_tiers: Vec::new(),
                    stop_policy: Vec::new(),
                },
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

    let state = test_state();
    let item = work_item("wi_invoice_filled");
    let fill = service::parse_invoice_fill_response(&grounded_response()).expect("fill");
    let draft = service::draft_from_fill(&item, &fill, 1, "test-model", 1_000);
    {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        insert_invoice_freshness_source(conn, &item);
        store::insert_draft(conn, CLIENT, "operator", &draft, "stage:filled").expect("insert");
    }
    assert!(
        service::freshness_candidates(&state, adapter, stale_after_ms, now_ms, 10)
            .expect("candidates")
            .is_empty()
    );
}

#[test]
fn invoice_freshness_candidates_include_weak_customer_name() {
    let state = test_state();
    let adapter = crate::slices::enrichment::service::registered_freshness_adapters()
        .iter()
        .find(|adapter| adapter.subject_id == "invoice_customer")
        .expect("invoice adapter");
    let stale_after_ms = 30 * 24 * 60 * 60 * 1000;
    let now_ms = stale_after_ms * 2;
    let item = work_item("wi_invoice_weak_name");
    let mut fill = service::parse_invoice_fill_response(&grounded_response()).expect("fill");
    fill.customer_name = "business-e743230c8d.test".to_string();
    fill.customer_email = Some("billing@business-e743230c8d.test".to_string());
    fill.provenance.retain(|p| p.field != "customer_name");
    fill.provenance
        .push(bos_contracts::calendar_drafts::DraftFieldProvenance {
            field: "customer_name".to_string(),
            quote: "Please bill business-e743230c8d.test".to_string(),
        });
    let draft = service::draft_from_fill(&item, &fill, 1, "test-model", 1_000);

    {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        insert_invoice_freshness_source(conn, &item);
        store::insert_draft(conn, CLIENT, "operator", &draft, "stage:weak-name").expect("insert");
    }

    let candidates = service::freshness_candidates(&state, adapter, stale_after_ms, now_ms, 10)
        .expect("candidates");
    assert_eq!(candidates.len(), 1);
}

#[test]
fn approve_enqueues_the_stripe_job_in_one_tx() {
    let state = test_state();
    let draft_id = staged_draft(&state, "itm_1");
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let draft = store::get_draft(conn, CLIENT, &draft_id)
        .expect("get")
        .expect("exists")
        .draft;
    let job = service::build_approval_job(&draft, "avery", 2_000).expect("job");
    assert_eq!(job.provider, "stripe");
    assert_eq!(job.capability, "create_invoice_draft");
    // The payload round-trips and passes the write client's validation.
    let payload: StripeInvoiceDraftOutboxPayload =
        serde_json::from_str(&job.payload_json).expect("payload");
    assert_eq!(payload.total_cents, 125_000);
    assert_eq!(payload.due_date_epoch_seconds, Some(1_782_864_000));
    store::approve_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "avery",
            expected_revision: None,
            idempotency_key: "approve:1",
            now_ms: 2_000,
        },
        &draft_id,
        &job,
    )
    .expect("approve");
    let approved = store::get_draft(conn, CLIENT, &draft_id)
        .expect("get")
        .expect("exists");
    assert_eq!(approved.draft.status, InvoiceDraftStatus::Approved);
    assert_eq!(
        approved.draft.outbox_job_id.as_deref(),
        Some(job.job_id.as_str())
    );
    // The enqueued job is visible through the outbox summary join.
    assert!(approved.outbox_job.is_some());
}

#[test]
fn approve_enqueues_the_invoice_ninja_job_in_one_tx() {
    let state = test_state();
    let draft_id = staged_draft(&state, "itm_in_1");
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let draft = store::get_draft(conn, CLIENT, &draft_id)
        .expect("get")
        .expect("exists")
        .draft;
    let job = service::build_invoice_ninja_approval_job(&draft, "avery", 2_000).expect("job");
    assert_eq!(job.provider, "invoice_ninja");
    assert_eq!(job.capability, "create_invoice_draft");
    let payload: InvoiceNinjaInvoiceDraftOutboxPayload =
        serde_json::from_str(&job.payload_json).expect("payload");
    // The NUMBER is Invoice Ninja's to assign (Generated Numbers pattern);
    // the payload carries only the dedupe ref.
    assert_eq!(payload.draft_ref, draft_id);
    assert_eq!(payload.total_cents, 125_000);
    store::approve_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "avery",
            expected_revision: None,
            idempotency_key: "approve:invoice-ninja",
            now_ms: 2_000,
        },
        &draft_id,
        &job,
    )
    .expect("approve");
    let approved = store::get_draft(conn, CLIENT, &draft_id)
        .expect("get")
        .expect("exists");
    assert_eq!(approved.draft.status, InvoiceDraftStatus::Approved);
    assert_eq!(
        approved.draft.outbox_job_id.as_deref(),
        Some(job.job_id.as_str())
    );
    assert!(approved.outbox_job.is_some());
}

#[test]
fn approval_requires_a_customer_email() {
    let state = test_state();
    let draft_id = staged_draft(&state, "itm_1");
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    // Blank the email (allowed at staging/edit time)…
    store::update_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "operator",
            expected_revision: None,
            idempotency_key: "edit:1",
            now_ms: 1_500,
        },
        &draft_id,
        "Dana Co",
        None,
        Some("2026-07-01"),
        "June consulting",
        &[InvoiceDraftLineItem {
            line_number: 1,
            label: "Consulting".to_string(),
            description: None,
            quantity: 2,
            unit_amount_cents: 50_000,
            line_total_cents: 0,
        }],
    )
    .expect("edit");
    // …then approval refuses at both gates (job build + store).
    let draft = store::get_draft(conn, CLIENT, &draft_id)
        .expect("get")
        .expect("exists")
        .draft;
    assert_eq!(
        service::build_approval_job(&draft, "avery", 2_000).unwrap_err(),
        "invoice_draft_email_required"
    );
}

#[test]
fn edits_recompute_totals_and_reject_bad_lines() {
    let state = test_state();
    let draft_id = staged_draft(&state, "itm_1");
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let ctx = |key: &'static str| DraftActionContext {
        client_id: CLIENT,
        actor_id: "operator",
        expected_revision: None,
        idempotency_key: key,
        now_ms: 1_500,
    };
    store::update_draft(
        conn,
        ctx("edit:1"),
        &draft_id,
        "Dana Co",
        Some("dana@example.com"),
        None,
        "",
        &[InvoiceDraftLineItem {
            line_number: 9, // renumbered server-side
            label: "Consulting".to_string(),
            description: None,
            quantity: 3,
            unit_amount_cents: 40_000,
            line_total_cents: 999, // ignored: recomputed
        }],
    )
    .expect("edit");
    let edited = store::get_draft(conn, CLIENT, &draft_id)
        .expect("get")
        .expect("exists")
        .draft;
    assert_eq!(edited.total_cents, 120_000);
    assert_eq!(edited.line_items[0].line_number, 1);
    assert_eq!(edited.line_items[0].line_total_cents, 120_000);
    assert!(edited.due_date.is_none());

    // Zero-amount and empty edits are refused.
    let err = store::update_draft(
        conn,
        ctx("edit:2"),
        &draft_id,
        "Dana Co",
        Some("dana@example.com"),
        None,
        "",
        &[InvoiceDraftLineItem {
            line_number: 1,
            label: "Consulting".to_string(),
            description: None,
            quantity: 1,
            unit_amount_cents: 0,
            line_total_cents: 0,
        }],
    )
    .unwrap_err();
    assert!(err.to_string().contains("invoice_draft_line_items_invalid"));
    assert!(store::update_draft(
        conn,
        ctx("edit:3"),
        &draft_id,
        "Dana Co",
        Some("dana@example.com"),
        None,
        "",
        &[],
    )
    .is_err());
}

#[test]
fn delivery_dry_runs_while_the_gate_is_closed() {
    let state = test_state();
    let draft_id = staged_draft(&state, "itm_1");
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let draft = store::get_draft(conn, CLIENT, &draft_id)
        .expect("get")
        .expect("exists")
        .draft;
    let job = service::build_approval_job(&draft, "avery", 2_000).expect("job");
    let claimed = ClaimedJob {
        job_id: job.job_id.clone(),
        provider: job.provider.clone(),
        capability: job.capability.clone(),
        payload_json: job.payload_json.clone(),
        attempts: 1,
        source_entity_kind: job.source_entity_kind.clone(),
        source_entity_id: job.source_entity_id.clone(),
        correlation_id: job.correlation_id.clone(),
        idempotency_key: job.idempotency_key.clone(),
    };
    let outcome = service::execute_job(
        &claimed,
        &StripeWriteConfig {
            secret_key: Some("sk_test".to_string()),
            write_enabled: false,
        },
        3_000,
    );
    match outcome {
        AttemptOutcome::Delivered { result_json } => {
            let result: serde_json::Value = serde_json::from_str(&result_json).expect("json");
            assert_eq!(result["dry_run"], json!(true));
            assert_eq!(result["provider_object_id"], json!("dry-run"));
        }
        other => panic!("expected delivered dry-run, got {other:?}"),
    }
    // Wrong provider/capability is terminal, never retried.
    let mut wrong = claimed.clone();
    wrong.capability = "send_invoice".to_string();
    assert!(matches!(
        service::execute_job(
            &wrong,
            &StripeWriteConfig {
                secret_key: None,
                write_enabled: false
            },
            3_000
        ),
        AttemptOutcome::Terminal { .. }
    ));
}

#[test]
fn invoice_ninja_arm_builds_a_draft_job_and_dry_runs() {
    use bos_integrations::invoice_ninja::InvoiceNinjaWriteConfig;
    let state = test_state();
    let draft_id = staged_draft(&state, "itm_1");
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let draft = store::get_draft(conn, CLIENT, &draft_id)
        .expect("get")
        .expect("exists")
        .draft;
    let job = service::build_invoice_ninja_approval_job(&draft, "avery", 2_000).expect("job");
    assert_eq!(job.provider, "invoice_ninja");
    assert_eq!(job.capability, "create_invoice_draft");
    let payload: InvoiceNinjaInvoiceDraftOutboxPayload =
        serde_json::from_str(&job.payload_json).expect("payload");
    // The NUMBER is Invoice Ninja's to assign (Generated Numbers pattern);
    // the payload carries only the dedupe ref.
    assert_eq!(payload.draft_ref, draft_id);
    assert_eq!(payload.total_cents, 125_000);
    assert_eq!(payload.due_date.as_deref(), Some("2026-07-01"));

    // Gate closed => dry-run delivery (validates end-to-end, no network).
    let claimed = ClaimedJob {
        job_id: job.job_id.clone(),
        provider: job.provider.clone(),
        capability: job.capability.clone(),
        payload_json: job.payload_json.clone(),
        attempts: 1,
        source_entity_kind: job.source_entity_kind.clone(),
        source_entity_id: job.source_entity_id.clone(),
        correlation_id: job.correlation_id.clone(),
        idempotency_key: job.idempotency_key.clone(),
    };
    let outcome = service::execute_invoice_ninja_job(
        &claimed,
        &InvoiceNinjaWriteConfig {
            base_url: Some("http://localhost:8003".to_string()),
            api_token: Some("token".to_string()),
            write_enabled: false,
        },
        3_000,
    );
    match outcome {
        AttemptOutcome::Delivered { result_json } => {
            let result: serde_json::Value = serde_json::from_str(&result_json).expect("json");
            assert_eq!(result["dry_run"], json!(true));
            // No number until Invoice Ninja assigns one on the live write.
            assert_eq!(result["invoice_number"], json!(null));
            assert_eq!(result["provider_object_id"], json!("dry-run"));
        }
        other => panic!("expected delivered dry-run, got {other:?}"),
    }

    // The email gate holds on this arm too.
    let mut emailless = draft.clone();
    emailless.customer_email = None;
    assert_eq!(
        service::build_invoice_ninja_approval_job(&emailless, "avery", 2_000).unwrap_err(),
        "invoice_draft_email_required"
    );
}

#[test]
fn net_terms_derive_a_due_date_from_the_draft_date() {
    // 1_780_963_200_000 ms = 2026-06-09T00:00:00Z. Net 30 → +30 days.
    let now_ms = 1_780_963_200_000u64;
    let (date, term) = service::due_date_from_net_terms("SEO audit for $200, terms Net 30", now_ms)
        .expect("net term parsed");
    assert_eq!(date, "2026-07-09");
    assert_eq!(term, "Net 30");

    // Hyphen + no space variants, case-insensitive, with literal provenance.
    assert_eq!(
        service::due_date_from_net_terms("paid NET-15 please", now_ms)
            .expect("net-15")
            .1,
        "NET-15"
    );
    assert_eq!(
        service::due_date_from_net_terms("paid NET15 please", now_ms)
            .expect("net15")
            .0,
        "2026-06-24"
    );
    // "network" must NOT match (word-boundary + digits required).
    assert!(service::due_date_from_net_terms("set up the network today", now_ms).is_none());
    // Alphanumeric suffixes are not payment terms.
    assert!(service::due_date_from_net_terms("net 30k revenue", now_ms).is_none());
    // No term, no date.
    assert!(service::due_date_from_net_terms("invoice him $200", now_ms).is_none());
    // Out-of-range day counts are ignored.
    assert!(service::due_date_from_net_terms("net 9999", now_ms).is_none());
}

#[test]
fn stage_persists_due_date_derived_from_net_terms() {
    use crate::produce::ProduceFlavor;

    let state = test_state();
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let item_id = "wi_invoice_net_terms";
    let item = work_item(item_id);
    let message = bos_contracts::email_triage::InboundMessageRecord {
        source_key: "note_net_terms".to_string(),
        message_id: "note_net_terms".to_string(),
        thread_id: None,
        internal_date_ms: Some(1),
        from_addr: None,
        to_addr: None,
        subject: Some("Bill Dana - NET-15".to_string()),
        body_excerpt: "did an SEO audit, invoice $200".to_string(),
        body_full: String::new(),
        headers: Vec::new(),
        labels: Vec::new(),
        resolved_category: "operator_note".to_string(),
        matched_rule_id: None,
        ingested_at_ms: 1,
        ai_triage_status: None,
        ai_triage_rationale: None,
        attachments: Vec::new(),
        source_user_id: None,
    };
    let mut response = grounded_response();
    response["due_date"] = serde_json::Value::Null;

    service::Produce
        .stage(crate::produce::StageContext {
            conn: &mut *conn,
            client_id: CLIENT,
            actor_id: "op",
            item: &item,
            message: &message,
            response: &response,
            context: &serde_json::Value::Null,
            model: "model",
            attempt: 1,
            idempotency_key: "inv_net_terms",
            now_ms: 1_780_963_200_000,
        })
        .expect("stage");

    let draft = store::active_draft_for_item(conn, CLIENT, item_id)
        .expect("get")
        .expect("present");
    assert_eq!(draft.draft.due_date.as_deref(), Some("2026-06-24"));
    assert!(
        draft
            .draft
            .provenance
            .iter()
            .any(|p| p.field == "due_date" && p.quote == "NET-15"),
        "the derived due date carries the literal matched term"
    );
}

#[test]
fn invoice_settings_replace_is_receipted_and_revision_checked() {
    let state = test_state();
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let request = InvoiceSettingsUpdateRequest {
        expected_revision: None,
        idempotency_key: "invoice-settings-1".to_string(),
        actor_id: None,
        default_due_days: Some(30),
    };

    let outcome = store::replace_invoice_settings(conn, CLIENT, "operator", &request, 10_000)
        .expect("replace settings");
    let revision = match outcome {
        crate::store_core::MutationOutcome::Applied { revision, .. } => revision,
        other => panic!("unexpected outcome: {other:?}"),
    };
    assert_eq!(revision, 1);

    let stored = store::get_invoice_settings(conn, CLIENT)
        .expect("load settings")
        .expect("settings row");
    assert_eq!(stored.default_due_days, Some(30));
    assert_eq!(stored.revision, Some(1));

    let mut stale = request.clone();
    stale.expected_revision = Some(0);
    stale.idempotency_key = "invoice-settings-stale".to_string();
    let conflict = store::replace_invoice_settings(conn, CLIENT, "operator", &stale, 10_100)
        .expect("conflict outcome");
    assert!(matches!(
        conflict,
        crate::store_core::MutationOutcome::RevisionConflict {
            current_revision: Some(1),
            ..
        }
    ));
}

#[test]
fn invoice_settings_default_due_days_apply_when_source_has_no_due_date() {
    use crate::produce::ProduceFlavor;

    let state = test_state();
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let settings = InvoiceSettingsUpdateRequest {
        expected_revision: None,
        idempotency_key: "invoice-settings-default".to_string(),
        actor_id: None,
        default_due_days: Some(7),
    };
    store::replace_invoice_settings(conn, CLIENT, "operator", &settings, 10_000).expect("settings");
    let item_id = "wi_invoice_default_terms";
    let item = work_item(item_id);
    let message = bos_contracts::email_triage::InboundMessageRecord {
        source_key: "note_default_terms".to_string(),
        message_id: "note_default_terms".to_string(),
        thread_id: None,
        internal_date_ms: Some(1),
        from_addr: None,
        to_addr: None,
        subject: Some("Bill Dana".to_string()),
        body_excerpt: "did an SEO audit, invoice $200".to_string(),
        body_full: String::new(),
        headers: Vec::new(),
        labels: Vec::new(),
        resolved_category: "operator_note".to_string(),
        matched_rule_id: None,
        ingested_at_ms: 1,
        ai_triage_status: None,
        ai_triage_rationale: None,
        attachments: Vec::new(),
        source_user_id: None,
    };
    let mut response = grounded_response();
    response["due_date"] = serde_json::Value::Null;

    service::Produce
        .stage(crate::produce::StageContext {
            conn: &mut *conn,
            client_id: CLIENT,
            actor_id: "op",
            item: &item,
            message: &message,
            response: &response,
            context: &serde_json::Value::Null,
            model: "model",
            attempt: 1,
            idempotency_key: "inv_default_terms",
            now_ms: 1_780_963_200_000,
        })
        .expect("stage");

    let draft = store::active_draft_for_item(conn, CLIENT, item_id)
        .expect("get")
        .expect("present");
    assert_eq!(draft.draft.due_date.as_deref(), Some("2026-06-16"));
    assert!(
        draft
            .draft
            .provenance
            .iter()
            .any(|p| p.field == "due_date" && p.quote == "default Net 7"),
        "the defaulted due date records its configured term"
    );
}

#[test]
fn invoice_settings_default_due_days_do_not_override_source_net_terms() {
    use crate::produce::ProduceFlavor;

    let state = test_state();
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let settings = InvoiceSettingsUpdateRequest {
        expected_revision: None,
        idempotency_key: "invoice-settings-precedence".to_string(),
        actor_id: None,
        default_due_days: Some(30),
    };
    store::replace_invoice_settings(conn, CLIENT, "operator", &settings, 10_000).expect("settings");
    let item_id = "wi_invoice_terms_precedence";
    let item = work_item(item_id);
    let message = bos_contracts::email_triage::InboundMessageRecord {
        source_key: "note_terms_precedence".to_string(),
        message_id: "note_terms_precedence".to_string(),
        thread_id: None,
        internal_date_ms: Some(1),
        from_addr: None,
        to_addr: None,
        subject: Some("Bill Dana - NET-15".to_string()),
        body_excerpt: "did an SEO audit, invoice $200".to_string(),
        body_full: String::new(),
        headers: Vec::new(),
        labels: Vec::new(),
        resolved_category: "operator_note".to_string(),
        matched_rule_id: None,
        ingested_at_ms: 1,
        ai_triage_status: None,
        ai_triage_rationale: None,
        attachments: Vec::new(),
        source_user_id: None,
    };
    let mut response = grounded_response();
    response["due_date"] = serde_json::Value::Null;

    service::Produce
        .stage(crate::produce::StageContext {
            conn: &mut *conn,
            client_id: CLIENT,
            actor_id: "op",
            item: &item,
            message: &message,
            response: &response,
            context: &serde_json::Value::Null,
            model: "model",
            attempt: 1,
            idempotency_key: "inv_terms_precedence",
            now_ms: 1_780_963_200_000,
        })
        .expect("stage");

    let draft = store::active_draft_for_item(conn, CLIENT, item_id)
        .expect("get")
        .expect("present");
    assert_eq!(draft.draft.due_date.as_deref(), Some("2026-06-24"));
    assert!(draft
        .draft
        .provenance
        .iter()
        .any(|p| p.field == "due_date" && p.quote == "NET-15"));
    assert!(!draft
        .draft
        .provenance
        .iter()
        .any(|p| p.field == "due_date" && p.quote == "default Net 30"));
}

/// Increment D/E: when the same work item carries a CRM record draft, invoice
/// produce grafts its billing identity into the invoice. The email stops
/// approval from blocking, and the customer name stays aligned with the CRM
/// record the operator reviewed rather than a separate model guess.
#[test]
fn customer_email_prefilled_from_the_items_crm_draft() {
    use crate::produce::ProduceFlavor;

    let state = test_state();
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let item_id = "wi_operator_note_note_d";

    // An active crm_record_create draft for the item, carrying the billing
    // identity the operator reviewed.
    let crm_draft = bos_contracts::crm_record_drafts::CrmRecordDraft {
        draft_id: format!("crd_{item_id}_1"),
        item_id: item_id.to_string(),
        source_kind: "operator_note".to_string(),
        source_ref: "note_d".to_string(),
        status: bos_contracts::crm_record_drafts::CrmRecordDraftStatus::Staged,
        create_company: false,
        company_name: Some("Example Company".to_string()),
        company_website: None,
        company_phone: None,
        company_address: None,
        company_description: None,
        create_contact: true,
        contact_first_name: Some("Casey".to_string()),
        contact_last_name: Some("Sullivan".to_string()),
        contact_email: Some("casey@example.test".to_string()),
        contact_phone: None,
        contact_title: None,
        provider_ids: Default::default(),
        provenance: Vec::new(),
        enrichment_trace: None,
        research_annotations: Vec::new(),
        model: "m".to_string(),
        confidence: "high".to_string(),
        outbox_job_id: None,
        created_at_ms: 1,
        updated_at_ms: 1,
    };
    crate::slices::crm_record_drafts::store::insert_draft(conn, CLIENT, "op", &crm_draft, "crd_1")
        .expect("crm draft");

    // A billable-work fill that states NO customer email.
    let mut response = grounded_response();
    response["customer_email"] = serde_json::Value::Null;
    response["customer_name"] = serde_json::Value::String("example.test".to_string());

    let message = bos_contracts::email_triage::InboundMessageRecord {
        source_key: "note_d".to_string(),
        message_id: "note_d".to_string(),
        thread_id: None,
        internal_date_ms: Some(1),
        from_addr: None,
        to_addr: None,
        subject: Some("Bill Dana".to_string()),
        body_excerpt: "did an SEO audit, invoice $200".to_string(),
        body_full: String::new(),
        headers: Vec::new(),
        labels: Vec::new(),
        resolved_category: "operator_note".to_string(),
        matched_rule_id: None,
        ingested_at_ms: 1,
        ai_triage_status: None,
        ai_triage_rationale: None,
        attachments: Vec::new(),
        source_user_id: None,
    };
    let item = work_item(item_id);

    let context = service::Produce
        .prepare_context(
            conn,
            CLIENT,
            &item,
            &message,
            &crate::http::OperatorScope::All,
            "operator",
        )
        .expect("context");
    assert_eq!(context["crm_billing"]["email"], "casey@example.test");
    assert_eq!(context["crm_billing"]["customer_name"], "Example Company");

    service::Produce
        .stage(crate::produce::StageContext {
            conn: &mut *conn,
            client_id: CLIENT,
            actor_id: "op",
            item: &item,
            message: &message,
            response: &response,
            context: &context,
            model: "model",
            attempt: 1,
            idempotency_key: "inv_1",
            now_ms: 2_000,
        })
        .expect("stage");

    let draft = store::active_draft_for_item(conn, CLIENT, item_id)
        .expect("get")
        .expect("present");
    assert_eq!(
        draft.draft.customer_email.as_deref(),
        Some("casey@example.test"),
        "the CRM contact email prefilled the invoice"
    );
    assert_eq!(
        draft.draft.customer_name, "Example Company",
        "the CRM company name replaced the model's customer name"
    );
    assert!(
        draft
            .draft
            .provenance
            .iter()
            .any(|p| p.field == "customer_email" && p.quote == "crm_match"),
        "the prefill is provenance-tagged crm_match"
    );
    assert!(
        draft
            .draft
            .provenance
            .iter()
            .any(|p| p.field == "customer_name" && p.quote == "crm_match"),
        "the CRM name override is provenance-tagged crm_match"
    );
}

#[test]
fn customer_enrichment_request_and_parse_are_grounded() {
    let item = work_item("wi_customer_enrich");
    let fill = service::InvoiceFill {
        customer_name: "example.test".to_string(),
        customer_email: None,
        line_items: vec![InvoiceDraftLineItem {
            line_number: 1,
            label: "Audit".to_string(),
            description: None,
            quantity: 1,
            unit_amount_cents: 20_000,
            line_total_cents: 20_000,
        }],
        due_date: None,
        memo: String::new(),
        confidence: "high".to_string(),
        provenance: vec![bos_contracts::calendar_drafts::DraftFieldProvenance {
            field: "customer_name".to_string(),
            quote: "Please bill example.test".to_string(),
        }],
    };
    let draft = service::draft_from_fill(&item, &fill, 1, "model", 2_000);
    let fields = vec!["customer_name".to_string(), "customer_email".to_string()];
    let page_text = "About Example Company. Contact billing@example.test for billing.";
    let pages = vec![bos_integrations::web_page_read::EnrichedPageText {
        url: "https://example.test".to_string(),
        text: page_text.to_string(),
    }];
    let request =
        service::build_customer_enrichment_request(CLIENT, &item, &draft, &fields, &pages);
    assert_eq!(request.spec.schema_ref, service::CUSTOMER_ENRICH_SCHEMA_REF);
    assert_eq!(
        request.safety_policy.redaction_policy,
        TypedLlmRedactionPolicy::PreSubmit
    );
    assert_eq!(
        request.safety_policy.raw_output_retention,
        TypedLlmRawOutputRetention::None
    );

    let html = r#"<html><head><style>.x{}</style><script>bad()</script></head>
        <body><h1>Example&nbsp;Stays</h1><p>Contact billing@example.test &amp; finance.</p></body></html>"#;
    let flat = bos_integrations::web_page_read::normalize_page_text(html, None, 8_000).flat_text;
    assert_eq!(
        flat,
        bos_integrations::web_page_read::strip_to_text(html, 8_000)
    );
    let migration_pages = vec![bos_integrations::web_page_read::EnrichedPageText {
        url: "https://example.test/billing".to_string(),
        text: flat,
    }];
    let migration_request = service::build_customer_enrichment_request(
        CLIENT,
        &item,
        &draft,
        &fields,
        &migration_pages,
    );
    assert_eq!(
        migration_request.input.text_blocks[0].text,
        format!(
            "URL: https://example.test/billing\n{}",
            bos_integrations::web_page_read::strip_to_text(html, 8_000)
        )
    );

    let response = json!({
        "customer_name": "Example Company",
        "customer_email": "billing@example.test",
        "confidence": "high",
        "provenance": [
            { "field": "customer_name", "quote": "About Example Company" },
            { "field": "customer_email", "quote": "Contact billing@example.test for billing" }
        ]
    });
    let apply = service::parse_customer_enrichment_response(&response, page_text, &fields);
    assert_eq!(
        apply
            .customer_name
            .as_ref()
            .map(|value| value.value.as_str()),
        Some("Example Company")
    );
    assert_eq!(
        apply
            .customer_email
            .as_ref()
            .map(|value| value.value.as_str()),
        Some("billing@example.test")
    );

    let ungrounded = json!({
        "customer_name": "Example Company",
        "customer_email": "billing at example dot com",
        "confidence": "high",
        "provenance": [
            { "field": "customer_name", "quote": "About a different company" },
            { "field": "customer_email", "quote": "Contact billing at example dot com" }
        ]
    });
    let apply = service::parse_customer_enrichment_response(&ungrounded, page_text, &fields);
    assert!(apply.customer_name.is_none());
    assert!(apply.customer_email.is_none());
}

#[test]
fn customer_enrichment_grafts_missing_values_without_overwriting_existing_context() {
    let state = test_state();
    let item = work_item("wi_customer_graft");
    let mut fill = service::parse_invoice_fill_response(&grounded_response()).expect("grounded");
    fill.customer_name = "example.test".to_string();
    fill.customer_email = None;
    fill.provenance.retain(|p| p.field != "customer_name");
    fill.provenance
        .push(bos_contracts::calendar_drafts::DraftFieldProvenance {
            field: "customer_name".to_string(),
            quote: "Please bill example.test".to_string(),
        });
    let draft = service::draft_from_fill(&item, &fill, 1, "model", 2_000);
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    store::insert_draft(conn, CLIENT, "operator", &draft, "stage:customer:graft").expect("insert");

    let apply = store::CustomerEnrichmentApply {
        customer_name: Some(store::CustomerEnrichedValue {
            value: "Example Company".to_string(),
            provenance_quote: "About Example Company".to_string(),
        }),
        customer_email: Some(store::CustomerEnrichedValue {
            value: "billing@example.test".to_string(),
            provenance_quote: "Contact billing@example.test".to_string(),
        }),
    };
    let outcome = store::apply_customer_enrichment(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: service::CUSTOMER_ENRICHMENT_ACTOR,
            expected_revision: None,
            idempotency_key: "enrich:customer:graft",
            now_ms: 3_000,
        },
        &draft.draft_id,
        &apply,
    )
    .expect("apply");
    assert!(outcome.is_some());
    let enriched = store::get_draft(conn, CLIENT, &draft.draft_id)
        .expect("get")
        .expect("exists")
        .draft;
    assert_eq!(enriched.customer_name, "Example Company");
    assert_eq!(
        enriched.customer_email.as_deref(),
        Some("billing@example.test")
    );
    assert!(enriched
        .provenance
        .iter()
        .any(|p| p.field == "customer_name" && p.quote == "About Example Company"));
    assert!(enriched
        .provenance
        .iter()
        .any(|p| p.field == "customer_email" && p.quote == "Contact billing@example.test"));

    let locked_item = work_item("wi_customer_locked");
    let mut locked_fill = service::parse_invoice_fill_response(&grounded_response()).expect("fill");
    locked_fill.customer_name = "example.test".to_string();
    locked_fill.customer_email = Some("crm@example.test".to_string());
    locked_fill
        .provenance
        .retain(|p| p.field != "customer_name" && p.field != "customer_email");
    locked_fill
        .provenance
        .push(bos_contracts::calendar_drafts::DraftFieldProvenance {
            field: "customer_name".to_string(),
            quote: "crm_match".to_string(),
        });
    locked_fill
        .provenance
        .push(bos_contracts::calendar_drafts::DraftFieldProvenance {
            field: "customer_email".to_string(),
            quote: "crm_match".to_string(),
        });
    let locked = service::draft_from_fill(&locked_item, &locked_fill, 1, "model", 2_000);
    store::insert_draft(conn, CLIENT, "operator", &locked, "stage:customer:locked")
        .expect("locked insert");
    let outcome = store::apply_customer_enrichment(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: service::CUSTOMER_ENRICHMENT_ACTOR,
            expected_revision: None,
            idempotency_key: "enrich:customer:locked",
            now_ms: 3_000,
        },
        &locked.draft_id,
        &apply,
    )
    .expect("locked apply");
    assert!(
        outcome.is_none(),
        "enrichment should not mutate when CRM/operator context wins"
    );
    let unchanged = store::get_draft(conn, CLIENT, &locked.draft_id)
        .expect("get")
        .expect("exists")
        .draft;
    assert_eq!(unchanged.customer_name, "example.test");
    assert_eq!(
        unchanged.customer_email.as_deref(),
        Some("crm@example.test")
    );
}

#[test]
fn invoice_declares_search_eligibility_for_weak_customer_name() {
    let item = work_item("wi_search_eligible");
    let fill = service::InvoiceFill {
        customer_name: "example.test".to_string(),
        customer_email: Some("billing@example.test".to_string()),
        line_items: vec![InvoiceDraftLineItem {
            line_number: 1,
            label: "Audit".to_string(),
            description: None,
            quantity: 1,
            unit_amount_cents: 20_000,
            line_total_cents: 20_000,
        }],
        due_date: None,
        memo: String::new(),
        confidence: "high".to_string(),
        provenance: vec![bos_contracts::calendar_drafts::DraftFieldProvenance {
            field: "customer_name".to_string(),
            quote: "Please bill example.test".to_string(),
        }],
    };
    let draft = service::draft_from_fill(&item, &fill, 1, "model", 2_000);
    assert_eq!(
        service::missing_customer_enrich_fields(&draft, &store::CustomerEnrichmentApply::default()),
        vec!["customer_name".to_string()]
    );
}

#[test]
fn invoice_customer_enrichment_skips_when_customer_context_is_complete() {
    let item = work_item("wi_search_complete");
    let fill = service::InvoiceFill {
        customer_name: "Example Company".to_string(),
        customer_email: Some("billing@example.test".to_string()),
        line_items: vec![InvoiceDraftLineItem {
            line_number: 1,
            label: "Audit".to_string(),
            description: None,
            quantity: 1,
            unit_amount_cents: 20_000,
            line_total_cents: 20_000,
        }],
        due_date: None,
        memo: String::new(),
        confidence: "high".to_string(),
        provenance: vec![
            bos_contracts::calendar_drafts::DraftFieldProvenance {
                field: "customer_name".to_string(),
                quote: "Example Company".to_string(),
            },
            bos_contracts::calendar_drafts::DraftFieldProvenance {
                field: "customer_email".to_string(),
                quote: "billing@example.test".to_string(),
            },
        ],
    };
    let draft = service::draft_from_fill(&item, &fill, 1, "model", 2_000);
    assert!(service::missing_customer_enrich_fields(
        &draft,
        &store::CustomerEnrichmentApply::default()
    )
    .is_empty());
}
