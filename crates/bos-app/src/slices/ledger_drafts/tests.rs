use bos_contracts::email_triage::InboundMessageRecord;
use bos_contracts::ledger_drafts::LedgerDraftStatus;
use bos_contracts::work_queue::{WorkItem, WorkItemStatus};
use bos_integrations::invoice_ninja::InvoiceNinjaWriteConfig;
use bos_integrations::qbo_payment_write::{
    QboPaymentExecutionClient, QboPaymentOutboxPayload, QboPaymentResponse, QboWriteError,
};
use serde_json::json;

use super::service;
use super::store::{self, DraftActionContext};
use crate::outbox;
use crate::persistence::Persistence;

const CLIENT: &str = "test-client";

fn item() -> WorkItem {
    WorkItem {
        item_id: "wi_email_m1".to_string(),
        source_kind: "email".to_string(),
        source_ref: "m1".to_string(),
        category_id: "payment_received".to_string(),
        title: "Stripe receipt".to_string(),
        summary: String::new(),
        packet_kinds: vec![service::PACKET_KIND.to_string()],
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

fn message() -> InboundMessageRecord {
    InboundMessageRecord {
        source_key: "m1".to_string(),
        message_id: "m1".to_string(),
        thread_id: None,
        // 2026-06-09 12:00 UTC.
        internal_date_ms: Some(1_781_006_400_000),
        from_addr: Some("receipts@business-b9783733db.test".to_string()),
        to_addr: Some("user@business-3cbd99b604.test".to_string()),
        subject: Some("Payment of $1,500.00 from Acme LLC".to_string()),
        body_excerpt:
            "You received a payment of $1,500.00 from Acme LLC (jane@business-86b318398f.test) \
                       for June consulting."
                .to_string(),
        body_full: String::new(),
        headers: Vec::new(),
        labels: Vec::new(),
        resolved_category: "payment_received".to_string(),
        matched_rule_id: None,
        ingested_at_ms: 1_781_006_500_000,
        ai_triage_status: None,
        ai_triage_rationale: None,
        attachments: Vec::new(),
        source_user_id: None,
    }
}

fn valid_fill() -> serde_json::Value {
    json!({
        "payer_name": "Acme LLC",
        "payer_email": "jane@business-86b318398f.test",
        "amount_cents": 150_000,
        "paid_date": null,
        "description": "June consulting",
        "confidence": "high",
        "provenance": [
            { "field": "payer_name", "quote": "from Acme LLC" },
            { "field": "amount_cents", "quote": "a payment of $1,500.00" }
        ]
    })
}

#[test]
fn fill_parsing_requires_a_literal_amount_quote() {
    let fill = service::parse_receipt_fill_response(&valid_fill()).expect("valid");
    assert_eq!(fill.payer_name, "Acme LLC");
    assert_eq!(fill.amount_cents, 150_000);
    assert_eq!(fill.paid_date, None);

    // No provenance for the amount: refused — money is never invented.
    let mut no_quote = valid_fill();
    no_quote["provenance"] = json!([{ "field": "payer_name", "quote": "from Acme LLC" }]);
    assert!(service::parse_receipt_fill_response(&no_quote)
        .unwrap_err()
        .contains("amount_cents"));

    // A quote that does NOT contain the amount: refused.
    let mut wrong_quote = valid_fill();
    wrong_quote["provenance"] = json!([{ "field": "amount_cents", "quote": "thanks for paying" }]);
    assert!(service::parse_receipt_fill_response(&wrong_quote).is_err());

    // Whole-dollar amounts match without cents ("$1500" quote, 150000 cents).
    let mut whole = valid_fill();
    whole["provenance"] = json!([{ "field": "amount_cents", "quote": "charged $1500 total" }]);
    assert!(service::parse_receipt_fill_response(&whole).is_ok());

    // Non-positive amounts are refused outright.
    let mut zero = valid_fill();
    zero["amount_cents"] = json!(0);
    assert!(service::parse_receipt_fill_response(&zero).is_err());

    // A contextual paid_date normalizes to canonical YYYY-MM-DD.
    let mut human_date = valid_fill();
    human_date["paid_date"] = json!("June 9th");
    let date_context = crate::slices::datetime_input::context_from_email(&message());
    assert_eq!(
        service::parse_receipt_fill_response_with_context(&human_date, Some(&date_context))
            .expect("valid")
            .paid_date,
        Some("2026-06-09".to_string())
    );

    let mut bad_date = valid_fill();
    bad_date["paid_date"] = json!("next week");
    assert!(
        service::parse_receipt_fill_response_with_context(&bad_date, Some(&date_context)).is_err()
    );
}

#[test]
fn draft_grounds_paid_date_from_the_source_email() {
    let fill = service::parse_receipt_fill_response(&valid_fill()).expect("valid");
    let draft = service::draft_from_fill(&item(), &message(), &fill, 1, "test-model", 2_000);
    assert_eq!(
        draft.paid_date, "2026-06-09",
        "source email date, not invented"
    );
    assert_eq!(draft.draft_id, "led_wi_email_m1_1");
    assert_eq!(draft.status, LedgerDraftStatus::Staged);

    // An explicit, valid date from the fill wins.
    let mut dated = valid_fill();
    dated["paid_date"] = json!("2026-06-08");
    let fill = service::parse_receipt_fill_response(&dated).expect("valid");
    let draft = service::draft_from_fill(&item(), &message(), &fill, 2, "test-model", 2_000);
    assert_eq!(draft.paid_date, "2026-06-08");
}

fn staged_draft(conn: &mut rusqlite::Connection) -> String {
    let fill = service::parse_receipt_fill_response(&valid_fill()).expect("valid");
    let draft = service::draft_from_fill(&item(), &message(), &fill, 1, "test-model", 2_000);
    store::insert_draft(conn, CLIENT, "op_test", &draft, "stage_1").expect("insert");
    draft.draft_id
}

fn ctx<'a>(key: &'a str, revision: Option<u64>) -> DraftActionContext<'a> {
    DraftActionContext {
        client_id: CLIENT,
        actor_id: "user_example",
        expected_revision: revision,
        idempotency_key: key,
        now_ms: 5_000,
    }
}

#[test]
fn one_active_draft_per_item_and_edit_validation() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let draft_id = staged_draft(conn);

    // Second active draft for the same item refuses.
    let fill = service::parse_receipt_fill_response(&valid_fill()).expect("valid");
    let dupe = service::draft_from_fill(&item(), &message(), &fill, 2, "test-model", 3_000);
    let err = store::insert_draft(conn, CLIENT, "op_test", &dupe, "stage_2").unwrap_err();
    assert!(err.to_string().contains("ledger_draft_already_active"));

    // Edits validate: payer required, positive amount, civil date.
    assert!(store::update_draft(
        conn,
        ctx("e1", None),
        &draft_id,
        "",
        None,
        100,
        "2026-06-09",
        "x"
    )
    .unwrap_err()
    .to_string()
    .contains("ledger_draft_payer_required"));
    assert!(store::update_draft(
        conn,
        ctx("e2", None),
        &draft_id,
        "Acme",
        None,
        0,
        "2026-06-09",
        "x"
    )
    .unwrap_err()
    .to_string()
    .contains("ledger_draft_amount_invalid"));
    assert!(store::update_draft(
        conn,
        ctx("e3", None),
        &draft_id,
        "Acme",
        None,
        100,
        "13/09/2026",
        "x"
    )
    .unwrap_err()
    .to_string()
    .contains("ledger_draft_date_invalid"));

    store::update_draft(
        conn,
        ctx("e4", Some(1)),
        &draft_id,
        "Acme Holdings",
        Some("jane@business-86b318398f.test"),
        200_000,
        "2026-06-08",
        "June consulting (corrected)",
    )
    .expect("edit");
    let updated = store::get_draft(conn, CLIENT, &draft_id)
        .expect("get")
        .expect("present");
    assert_eq!(updated.draft.payer_name, "Acme Holdings");
    assert_eq!(updated.draft.amount_cents, 200_000);
}

#[test]
fn approval_enqueues_the_record_receipt_job_atomically() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let draft_id = staged_draft(conn);
    let draft = store::get_draft(conn, CLIENT, &draft_id)
        .expect("get")
        .expect("present");
    let job = service::build_approval_job(&draft.draft, "user_example", 5_000).expect("job");
    assert_eq!(job.provider, "invoice_ninja");
    assert_eq!(job.capability, "record_receipt");
    let payload: bos_integrations::invoice_ninja::InvoiceNinjaReceiptOutboxPayload =
        serde_json::from_str(&job.payload_json).expect("payload");
    assert_eq!(payload.amount_cents, 150_000);
    assert_eq!(payload.invoice_number, format!("BOS-{draft_id}"));
    assert_eq!(payload.approval.approved_by, "user_example");

    store::approve_draft(conn, ctx("a1", Some(1)), &draft_id, &job).expect("approve");
    let approved = store::get_draft(conn, CLIENT, &draft_id)
        .expect("get")
        .expect("present");
    assert_eq!(approved.draft.status, LedgerDraftStatus::Approved);
    assert_eq!(
        approved.draft.outbox_job_id.as_deref(),
        Some(job.job_id.as_str())
    );
    let summary = approved
        .outbox_job
        .expect("job enqueued in the same transaction");
    assert_eq!(summary.status, "pending");

    // Approving twice refuses (no longer staged).
    let err = store::approve_draft(conn, ctx("a2", Some(2)), &draft_id, &job).unwrap_err();
    assert!(err.to_string().contains("ledger_draft_not_staged"));
}

#[test]
fn execute_job_dry_runs_behind_the_gate_and_rejects_foreign_jobs() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let draft_id = staged_draft(conn);
    let draft = store::get_draft(conn, CLIENT, &draft_id)
        .expect("get")
        .expect("present");
    let new_job = service::build_approval_job(&draft.draft, "user_example", 5_000).expect("job");
    store::approve_draft(conn, ctx("a1", Some(1)), &draft_id, &new_job).expect("approve");
    let claimed = outbox::claim_due_jobs(conn, CLIENT, Some("invoice_ninja"), 60_000, 10, 6_000)
        .expect("claim");
    let job = claimed.first().expect("claimed job");

    // Gate closed: dry-run delivery, clearly labeled.
    let config = InvoiceNinjaWriteConfig {
        base_url: Some("https://in.example".to_string()),
        api_token: Some("token".to_string()),
        write_enabled: false,
    };
    match service::execute_job(job, &config, 10_000) {
        outbox::AttemptOutcome::Delivered { result_json } => {
            let result: serde_json::Value = serde_json::from_str(&result_json).expect("json");
            assert_eq!(result["dry_run"], serde_json::json!(true));
        }
        other => panic!("expected dry-run delivery, got {other:?}"),
    }

    // A job for another provider/capability is terminal, never retried.
    let mut foreign = job.clone();
    foreign.provider = "hubspot".to_string();
    assert!(matches!(
        service::execute_job(&foreign, &config, 10_000),
        outbox::AttemptOutcome::Terminal { .. }
    ));
}

/// QBO arm (port #3): amount-must-match invoice link at approval + dry-run
/// delivery through the gate.
mod qbo_arm {
    use super::*;
    use bos_integrations::accounting_read::{CustomerRecord, InvoiceRecord, TierSource};
    use bos_integrations::qbo_payment_write::DryRunQboPaymentClient;

    fn invoice(id: &str, customer: Option<(&str, &str)>, balance_cents: i64) -> InvoiceRecord {
        InvoiceRecord {
            invoice_id: id.to_string(),
            doc_number: Some(format!("INV-{id}")),
            customer_id: customer.map(|(cid, _)| cid.to_string()),
            customer_name: customer.map(|(_, name)| name.to_string()),
            txn_date: Some("2026-05-20".to_string()),
            due_date: Some("2026-06-20".to_string()),
            total_amt_cents: balance_cents,
            balance_cents,
            voided: false,
            updated_at: "2026-06-01T00:00:00Z".to_string(),
        }
    }

    fn seed_invoices(conn: &mut rusqlite::Connection, records: &[InvoiceRecord]) {
        crate::slices::accounting::store::upsert_invoice_snapshots(conn, CLIENT, records, 1_000)
            .expect("seed snapshots");
    }

    fn seed_customers(conn: &mut rusqlite::Connection, records: &[CustomerRecord]) {
        crate::slices::accounting::store::upsert_customer_snapshots(conn, CLIENT, records, 1_000)
            .expect("seed customer snapshots");
    }

    fn customer(id: &str, name: &str, active: bool) -> CustomerRecord {
        CustomerRecord {
            customer_id: id.to_string(),
            display_name: name.to_string(),
            company_name: None,
            email: None,
            phone: None,
            active,
            tier_raw: None,
            tier_source: TierSource::NotProvided,
            updated_at: Some("2026-06-01T00:00:00Z".to_string()),
        }
    }

    fn draft(conn: &mut rusqlite::Connection) -> bos_contracts::ledger_drafts::LedgerEntryDraft {
        let draft_id = staged_draft(conn);
        store::get_draft(conn, CLIENT, &draft_id)
            .expect("get")
            .expect("present")
            .draft
    }

    #[test]
    fn approval_links_the_single_invoice_whose_balance_matches() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        seed_invoices(
            conn,
            &[
                invoice("inv-1", Some(("cust-1", "Acme LLC")), 150_000),
                invoice("inv-2", Some(("cust-2", "Other Co")), 99_000),
            ],
        );
        let draft = draft(conn);

        let job = service::build_qbo_approval_job(conn, CLIENT, &draft, "user_example", 5_000)
            .expect("job");
        assert_eq!(job.provider, service::PROVIDER_QBO);
        assert_eq!(job.capability, service::CAPABILITY_RECORD_PAYMENT);
        let payload: serde_json::Value = serde_json::from_str(&job.payload_json).expect("json");
        assert_eq!(payload["provider_invoice_id"], "inv-1");
        assert_eq!(payload["provider_customer_id"], "cust-1");
        assert_eq!(payload["amount_cents"], 150_000);
    }

    #[test]
    fn approval_refuses_when_no_or_ambiguous_balance_match() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        let draft = draft(conn);

        // Empty snapshot: nothing to link.
        let err = service::build_qbo_approval_job(conn, CLIENT, &draft, "user_example", 5_000)
            .expect_err("no match");
        assert_eq!(err, "qbo_payment_no_invoice_with_matching_balance");

        // Two same-balance invoices, neither customer resembling the payer.
        seed_invoices(
            conn,
            &[
                invoice("inv-1", Some(("cust-1", "Globex")), 150_000),
                invoice("inv-2", Some(("cust-2", "Initech")), 150_000),
            ],
        );
        let err = service::build_qbo_approval_job(conn, CLIENT, &draft, "user_example", 5_000)
            .expect_err("ambiguous");
        assert_eq!(err, "qbo_payment_invoice_match_ambiguous");
    }

    #[test]
    fn approval_narrows_same_balance_matches_by_payer_name() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        seed_invoices(
            conn,
            &[
                invoice("inv-1", Some(("cust-1", "Globex")), 150_000),
                // Fill's payer is "Acme LLC"; snapshot name "Acme" is contained.
                invoice("inv-2", Some(("cust-2", "Acme")), 150_000),
            ],
        );
        let draft = draft(conn);

        let job = service::build_qbo_approval_job(conn, CLIENT, &draft, "user_example", 5_000)
            .expect("narrowed");
        let payload: serde_json::Value = serde_json::from_str(&job.payload_json).expect("json");
        assert_eq!(payload["provider_invoice_id"], "inv-2");
    }

    #[test]
    fn approval_refuses_a_match_without_a_customer_reference() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        seed_invoices(conn, &[invoice("inv-1", None, 150_000)]);
        let draft = draft(conn);

        let err = service::build_qbo_approval_job(conn, CLIENT, &draft, "user_example", 5_000)
            .expect_err("no customer");
        assert_eq!(err, "qbo_payment_invoice_missing_customer");
    }

    #[test]
    fn approval_refuses_a_cached_inactive_customer_reference() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        seed_invoices(
            conn,
            &[invoice("inv-1", Some(("cust-1", "Acme LLC")), 150_000)],
        );
        seed_customers(conn, &[customer("cust-1", "Acme LLC", false)]);
        let draft = draft(conn);

        let err = service::build_qbo_approval_job(conn, CLIENT, &draft, "user_example", 5_000)
            .expect_err("inactive customer");
        assert_eq!(err, "qbo_payment_customer_inactive");
    }

    #[test]
    fn qbo_delivery_dry_runs_behind_the_gate() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        seed_invoices(
            conn,
            &[invoice("inv-1", Some(("cust-1", "Acme LLC")), 150_000)],
        );
        let draft = draft(conn);
        let new_job = service::build_qbo_approval_job(conn, CLIENT, &draft, "user_example", 5_000)
            .expect("job");
        store::approve_draft(conn, ctx("a1", Some(1)), &draft.draft_id, &new_job).expect("approve");
        let claimed =
            outbox::claim_due_jobs(conn, CLIENT, Some("qbo"), 60_000, 10, 6_000).expect("claim");
        let job = claimed.first().expect("claimed job");

        match service::execute_qbo_job(job, &DryRunQboPaymentClient, 10_000) {
            outbox::AttemptOutcome::Delivered { result_json } => {
                let result: serde_json::Value = serde_json::from_str(&result_json).expect("json");
                assert_eq!(result["dry_run"], serde_json::json!(true));
                assert_eq!(result["linked_invoice_id"], "inv-1");
            }
            other => panic!("expected dry-run delivery, got {other:?}"),
        }

        // A corrupt payload is terminal, never retried.
        let mut corrupt = job.clone();
        corrupt.payload_json = "{not json".to_string();
        assert!(matches!(
            service::execute_qbo_job(&corrupt, &DryRunQboPaymentClient, 10_000),
            outbox::AttemptOutcome::Terminal { .. }
        ));
    }

    struct RejectingQboPaymentClient;

    impl QboPaymentExecutionClient for RejectingQboPaymentClient {
        fn record_payment(
            &self,
            _payload: &QboPaymentOutboxPayload,
        ) -> Result<QboPaymentResponse, QboWriteError> {
            Err(QboWriteError::Permanent {
                code: "qbo_payment_rejected".to_string(),
                message: "provider returned 400: invalid customer reference".to_string(),
            })
        }
    }

    #[test]
    fn qbo_permanent_provider_message_is_stored_in_outbox_error() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        seed_invoices(
            conn,
            &[invoice("inv-1", Some(("cust-1", "Acme LLC")), 150_000)],
        );
        let draft = draft(conn);
        let new_job = service::build_qbo_approval_job(conn, CLIENT, &draft, "user_example", 5_000)
            .expect("job");
        store::approve_draft(conn, ctx("a1", Some(1)), &draft.draft_id, &new_job).expect("approve");
        let claimed =
            outbox::claim_due_jobs(conn, CLIENT, Some("qbo"), 60_000, 10, 6_000).expect("claim");
        let job = claimed.first().expect("claimed job");

        let outcome = service::execute_qbo_job(job, &RejectingQboPaymentClient, 10_000);
        assert_eq!(
            outcome,
            outbox::AttemptOutcome::Terminal {
                error: "qbo_payment_rejected: provider returned 400: invalid customer reference"
                    .to_string(),
                result_json: Some(
                    "{\"message\":\"provider returned 400: invalid customer reference\"}"
                        .to_string()
                ),
            }
        );

        outbox::record_attempt(conn, CLIENT, job, &outcome, 10_000).expect("record attempt");
        let summary = outbox::job_summary(conn, CLIENT, &job.job_id)
            .expect("summary")
            .expect("job");
        assert_eq!(
            summary.last_error.as_deref(),
            Some("qbo_payment_rejected: provider returned 400: invalid customer reference")
        );
    }
}
