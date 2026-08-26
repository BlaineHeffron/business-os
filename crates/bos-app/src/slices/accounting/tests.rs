use std::sync::Mutex;

use bos_integrations::accounting_read::{
    AccountingError, AccountingReadClient, BalanceSheetSummary, BillRecord, CustomerRecord,
    FixtureAccountingReadClient, InvoiceRecord, Page, PageRequest, PnlDailySummary, PnlReport,
    PnlReportRequest, PnlSummary, TierSource,
};
use bos_integrations::qbo_oauth::{QboTokenGrant, QboTokenRefresher};

use super::service;
use super::store::{self, QboSyncCursor};
use super::worker::{self, AuthRecovery, CycleSummary, NoAuthRecovery};
use crate::http::test_support::test_state;
use crate::http::AppState;
use crate::overlay::AccountingVisibilityPolicy;

const CLIENT: &str = "test-client";

fn grant(suffix: &str, now_ms: u64) -> QboTokenGrant {
    QboTokenGrant {
        access_token: format!("at-{suffix}"),
        access_token_expires_at_ms: now_ms + 3_600_000,
        refresh_token: format!("rt-{suffix}"),
        refresh_token_expires_at_ms: now_ms + 8_640_000_000,
    }
}

fn connect(state: &AppState, now_ms: u64) {
    let mut persistence = state.persistence.lock();
    store::store_credential(
        persistence.connection(),
        CLIENT,
        "realm-1",
        "sandbox",
        &grant("0", now_ms),
        "user_example",
        now_ms,
    )
    .expect("connect");
}

#[test]
fn qbo_financial_visibility_is_limited_to_authorizer_or_all_scope() {
    let state = test_state();
    connect(&state, 1_000);
    let persistence = state.persistence.lock();
    let conn = persistence.connection_ref();

    assert!(service::financial_visibility_allowed(
        conn,
        CLIENT,
        &crate::http::OperatorScope::All,
        AccountingVisibilityPolicy::AuthorizerOnly,
    )
    .expect("all scope"));
    assert!(service::financial_visibility_allowed(
        conn,
        CLIENT,
        &crate::http::OperatorScope::User("user_example".to_string()),
        AccountingVisibilityPolicy::AuthorizerOnly,
    )
    .expect("authorizer"));
    assert!(!service::financial_visibility_allowed(
        conn,
        CLIENT,
        &crate::http::OperatorScope::User("user_casey".to_string()),
        AccountingVisibilityPolicy::AuthorizerOnly,
    )
    .expect("other user"));
}

#[test]
fn shared_financial_visibility_allows_non_authorizer() {
    let state = test_state();
    connect(&state, 1_000);
    let persistence = state.persistence.lock();
    let conn = persistence.connection_ref();

    assert!(service::financial_visibility_allowed(
        conn,
        CLIENT,
        &crate::http::OperatorScope::User("user_casey".to_string()),
        AccountingVisibilityPolicy::Shared,
    )
    .expect("shared policy"));
}

#[test]
fn cached_qbo_financial_visibility_blocks_stale_cache_after_disconnect() {
    let state = test_state();
    connect(&state, 1_000);
    let mut fixture = FixtureAccountingReadClient::with_pnl_support();
    fixture.invoices = vec![invoice("i1", "2026-05-01T00:00:00-07:00")];
    fixture.customers = vec![customer("c1", "Dana")];
    run_cycle(&state, &fixture, &NoAuthRecovery, 25, 1_781_092_800_000);

    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    store::delete_credential(conn, CLIENT, "user_example", false, 2_000).expect("disconnect");

    assert!(service::cached_financial_visibility_allowed(
        conn,
        CLIENT,
        &crate::http::OperatorScope::All,
        AccountingVisibilityPolicy::AuthorizerOnly,
    )
    .expect("all scope"));
    assert!(!service::cached_financial_visibility_allowed(
        conn,
        CLIENT,
        &crate::http::OperatorScope::User("user_casey".to_string()),
        AccountingVisibilityPolicy::AuthorizerOnly,
    )
    .expect("other user"));
}

fn invoice(id: &str, updated_at: &str) -> InvoiceRecord {
    InvoiceRecord {
        invoice_id: id.to_string(),
        doc_number: Some(format!("DOC-{id}")),
        customer_id: Some("c1".to_string()),
        customer_name: Some("Dana".to_string()),
        txn_date: Some("2026-06-01".to_string()),
        due_date: Some("2026-07-01".to_string()),
        total_amt_cents: 10_000,
        balance_cents: 10_000,
        voided: false,
        updated_at: updated_at.to_string(),
    }
}

fn bill(id: &str, updated_at: &str) -> BillRecord {
    BillRecord {
        bill_id: id.to_string(),
        vendor_id: Some("v1".to_string()),
        vendor_name: Some("Champion".to_string()),
        txn_date: Some("2026-06-01".to_string()),
        due_date: Some("2026-07-01".to_string()),
        total_amt_cents: 7_500,
        balance_cents: 2_500,
        voided: false,
        updated_at: updated_at.to_string(),
    }
}

fn customer(id: &str, name: &str) -> CustomerRecord {
    CustomerRecord {
        customer_id: id.to_string(),
        display_name: name.to_string(),
        company_name: None,
        email: None,
        phone: None,
        active: true,
        tier_raw: Some("Tier A".to_string()),
        tier_source: TierSource::CustomerTypeRefName,
        updated_at: Some("2026-06-01T00:00:00-07:00".to_string()),
    }
}

struct FakeRefresher {
    grants: Mutex<Vec<QboTokenGrant>>,
    calls: Mutex<u32>,
}

impl FakeRefresher {
    fn with(grants: Vec<QboTokenGrant>) -> Self {
        Self {
            grants: Mutex::new(grants),
            calls: Mutex::new(0),
        }
    }

    fn call_count(&self) -> u32 {
        *self.calls.lock().expect("lock")
    }
}

impl QboTokenRefresher for FakeRefresher {
    fn refresh(
        &self,
        _refresh_token: &str,
        _now_ms: u64,
    ) -> Result<QboTokenGrant, AccountingError> {
        *self.calls.lock().expect("lock") += 1;
        let mut grants = self.grants.lock().expect("lock");
        if grants.is_empty() {
            panic!("unexpected token refresh");
        }
        Ok(grants.remove(0))
    }
}

fn receipt_count(state: &AppState) -> i64 {
    let persistence = state.persistence.lock();
    persistence
        .connection_ref()
        .query_row("SELECT COUNT(*) FROM receipts", [], |row| row.get(0))
        .expect("count")
}

/// Recording AuthRecovery fake: Ok for the first `allowed` calls, then Err.
struct FakeRecovery {
    calls: Mutex<u32>,
    allowed: u32,
}

impl FakeRecovery {
    fn allowing(allowed: u32) -> Self {
        Self {
            calls: Mutex::new(0),
            allowed,
        }
    }

    fn call_count(&self) -> u32 {
        *self.calls.lock().expect("lock")
    }
}

impl AuthRecovery for FakeRecovery {
    fn recover(&self, _state: &AppState, _now: u64) -> Result<(), String> {
        let mut calls = self.calls.lock().expect("lock");
        *calls += 1;
        if *calls <= self.allowed {
            Ok(())
        } else {
            Err("recovery exhausted".to_string())
        }
    }
}

fn run_cycle(
    state: &AppState,
    client: &dyn AccountingReadClient,
    auth: &dyn AuthRecovery,
    budget: u32,
    now_ms: u64,
) -> CycleSummary {
    worker::run_sync_cycle(state, client, auth, budget, now_ms).expect("cycle")
}

#[test]
fn backfill_walks_pages_across_budgeted_cycles_then_goes_incremental() {
    let state = test_state();
    connect(&state, 1_000);
    // 250 invoices = 3 pages at MAXRESULTS 100; one customer page.
    let fixture = FixtureAccountingReadClient {
        invoices: (0..250)
            .map(|n| {
                invoice(
                    &format!("i{n:03}"),
                    &format!("2026-05-01T{:02}:{:02}:00-07:00", n / 60, n % 60),
                )
            })
            .collect(),
        customers: vec![customer("c1", "Dana")],
        pnl: Default::default(),
        pnl_supported: false,
        ..Default::default()
    };
    // Cycle 1, budget 2: customers complete (1 req), invoices page 1 (1 req).
    let summary = run_cycle(&state, &fixture, &NoAuthRecovery, 2, 2_000);
    assert_eq!(summary.requests_used, 2);
    {
        let persistence = state.persistence.lock();
        let conn = persistence.connection_ref();
        let cursor = store::get_cursor(conn, CLIENT, store::ENTITY_INVOICE).expect("cursor");
        assert_eq!(
            cursor.next_start_position, 101,
            "invoice walk parked at page 2"
        );
        assert!(!cursor.backfill_complete);
        let customer_cursor =
            store::get_cursor(conn, CLIENT, store::ENTITY_CUSTOMER).expect("cursor");
        assert!(customer_cursor.backfill_complete, "customers finished");
        let (invoices, customers) = store::snapshot_counts(conn, CLIENT).expect("counts");
        assert_eq!((invoices, customers), (100, 1));
    }

    // Cycle 2, budget 2: customer check spends one request (incremental walk
    // over an already-complete entity), invoice page 2 the other.
    let summary = run_cycle(&state, &fixture, &NoAuthRecovery, 2, 3_000);
    assert_eq!(summary.requests_used, 2);
    {
        let persistence = state.persistence.lock();
        let conn = persistence.connection_ref();
        let cursor = store::get_cursor(conn, CLIENT, store::ENTITY_INVOICE).expect("cursor");
        assert_eq!(cursor.next_start_position, 201, "page 3 still pending");
        let (invoices, _) = store::snapshot_counts(conn, CLIENT).expect("counts");
        assert_eq!(invoices, 200);
    }

    // Cycle 3: the walk completes and promotes the high-water mark.
    run_cycle(&state, &fixture, &NoAuthRecovery, 4, 4_000);
    {
        let persistence = state.persistence.lock();
        let conn = persistence.connection_ref();
        let cursor = store::get_cursor(conn, CLIENT, store::ENTITY_INVOICE).expect("cursor");
        assert!(cursor.backfill_complete, "third cycle finishes the walk");
        assert_eq!(cursor.next_start_position, 1);
        assert!(cursor.high_water_updated_at.is_some());
        let (invoices, _) = store::snapshot_counts(conn, CLIENT).expect("counts");
        assert_eq!(invoices, 250);
    }
}

#[test]
fn steady_state_cycles_write_zero_receipts() {
    let state = test_state();
    connect(&state, 1_000);
    let fixture = FixtureAccountingReadClient {
        invoices: vec![
            invoice("i1", "2026-05-01T00:00:00-07:00"),
            invoice("i2", "2026-05-02T00:00:00-07:00"),
        ],
        customers: vec![customer("c1", "Dana")],
        pnl: Default::default(),
        pnl_supported: false,
        ..Default::default()
    };
    // Budget covers the entities (P&L is exercised separately), so the
    // second cycle is true steady state.
    run_cycle(&state, &fixture, &NoAuthRecovery, 25, 2_000);
    let after_first = receipt_count(&state);

    // Identical data: the boundary rows are re-fetched (inclusive filter) but
    // nothing changed — the load-bearing assertion is ZERO new receipts.
    let summary = run_cycle(&state, &fixture, &NoAuthRecovery, 25, 3_000);
    assert_eq!(
        receipt_count(&state),
        after_first,
        "quiet cycle wrote receipts"
    );
    assert_eq!(summary.written, 0);

    // One real change: exactly one snapshot receipt + one cursor advance.
    let mut changed = fixture.clone();
    changed.invoices[1].balance_cents = 0;
    changed.invoices[1].updated_at = "2026-05-03T00:00:00-07:00".to_string();
    let summary = run_cycle(&state, &changed, &NoAuthRecovery, 25, 4_000);
    assert_eq!(summary.written, 1);
    let delta = receipt_count(&state) - after_first;
    assert_eq!(
        delta, 2,
        "one snapshot receipt + one cursor receipt, got {delta}"
    );
}

#[test]
fn bill_sync_uses_the_budgeted_entity_walk_and_receipt_quiet_upserts() {
    let state = test_state();
    connect(&state, 1_000);
    let fixture = FixtureAccountingReadClient {
        bills: vec![bill("b1", "2026-05-01T00:00:00-07:00")],
        bills_supported: true,
        pnl_supported: false,
        ..Default::default()
    };
    let summary = run_cycle(&state, &fixture, &NoAuthRecovery, 8, 2_000);
    assert_eq!(
        summary.requests_used, 3,
        "customer, bill, and invoice entity pages"
    );
    assert_eq!(summary.written, 1);
    let after_first = receipt_count(&state);
    {
        let persistence = state.persistence.lock();
        let conn = persistence.connection_ref();
        let bills = store::list_bills(conn, CLIENT, 10).expect("bills");
        assert_eq!(bills.len(), 1);
        assert_eq!(bills[0].balance_cents, 2_500);
        let cursor = store::get_cursor(conn, CLIENT, store::ENTITY_BILL).expect("cursor");
        assert!(cursor.backfill_complete);
    }

    let summary = run_cycle(&state, &fixture, &NoAuthRecovery, 8, 3_000);
    assert_eq!(summary.written, 0);
    assert_eq!(
        receipt_count(&state),
        after_first,
        "quiet bill cycle wrote receipts"
    );
}

struct ScriptedClient {
    /// One scripted result per INVOICE fetch, in call order. An empty script
    /// panics on fetch — used to prove an entity was skipped.
    script: Mutex<Vec<Result<Vec<InvoiceRecord>, AccountingError>>>,
}

impl AccountingReadClient for ScriptedClient {
    fn fetch_invoices(&self, page: &PageRequest) -> Result<Page<InvoiceRecord>, AccountingError> {
        let mut script = self.script.lock().expect("lock");
        if script.is_empty() {
            panic!("unexpected invoice fetch");
        }
        script.remove(0).map(|records| Page {
            records,
            requested_page_size: page.page_size.clamp(1, 100),
        })
    }

    fn fetch_customers(&self, page: &PageRequest) -> Result<Page<CustomerRecord>, AccountingError> {
        Ok(Page {
            records: Vec::new(),
            requested_page_size: page.page_size.clamp(1, 100),
        })
    }

    fn supports_pnl(&self) -> bool {
        false
    }

    fn fetch_profit_and_loss(
        &self,
        _request: &PnlReportRequest<'_>,
    ) -> Result<PnlReport, AccountingError> {
        Ok(Default::default())
    }
}

#[test]
fn rate_limit_stops_the_cycle_and_stamps_a_backoff_deadline() {
    let state = test_state();
    connect(&state, 1_000);
    let client = ScriptedClient {
        script: Mutex::new(vec![Err(AccountingError::RateLimited {
            retry_after_ms: Some(45_000),
            message: "429".to_string(),
        })]),
    };
    let summary = run_cycle(&state, &client, &NoAuthRecovery, 8, 10_000);
    assert!(summary.rate_limited);
    assert_eq!(
        summary.requests_used, 2,
        "customer page + the rate-limited call"
    );
    {
        let persistence = state.persistence.lock();
        let cursor = store::get_cursor(persistence.connection_ref(), CLIENT, store::ENTITY_INVOICE)
            .expect("cursor");
        assert_eq!(
            cursor.rate_limited_until_ms, 55_000,
            "deadline = now + Retry-After"
        );
        assert_eq!(
            cursor.next_start_position, 1,
            "cursor did not regress or advance"
        );
    }

    // Before the deadline the WHOLE cycle stands down (the 429 throttles
    // the realm, not one entity): zero fetches of any kind — an unexpected
    // invoice fetch would panic the scripted client.
    let client = ScriptedClient {
        script: Mutex::new(Vec::new()),
    };
    let summary = run_cycle(&state, &client, &NoAuthRecovery, 8, 20_000);
    assert_eq!(summary.requests_used, 0, "realm-wide standdown");
    // After the deadline, fetching resumes.
    let client = ScriptedClient {
        script: Mutex::new(vec![Ok(Vec::new())]),
    };
    let summary = run_cycle(&state, &client, &NoAuthRecovery, 4, 60_000);
    assert!(summary.requests_used >= 2, "resumes after the deadline");
}

#[test]
fn failure_mid_walk_resumes_at_the_same_page() {
    let state = test_state();
    connect(&state, 1_000);
    // Page 1 (100 records) then a retryable failure on page 2.
    let full_page: Vec<InvoiceRecord> = (0..100)
        .map(|n| invoice(&format!("i{n:03}"), "2026-05-01T00:00:00-07:00"))
        .collect();
    let client = ScriptedClient {
        script: Mutex::new(vec![
            Ok(full_page),
            Err(AccountingError::Retryable {
                code: "qbo_server_error".to_string(),
                message: "502".to_string(),
            }),
        ]),
    };
    run_cycle(&state, &client, &NoAuthRecovery, 8, 2_000);
    let persistence = state.persistence.lock();
    let cursor = store::get_cursor(persistence.connection_ref(), CLIENT, store::ENTITY_INVOICE)
        .expect("cursor");
    assert_eq!(cursor.next_start_position, 101, "resume exactly at page 2");
    assert!(cursor.last_error.is_some());
    assert!(!cursor.backfill_complete);
}

#[test]
fn expired_access_token_refreshes_and_persists_the_rotated_grant() {
    let state = test_state();
    {
        // Connect with an ALREADY-EXPIRED access token.
        let mut persistence = state.persistence.lock();
        let mut stale = grant("0", 0);
        stale.access_token_expires_at_ms = 10;
        store::store_credential(
            persistence.connection(),
            CLIENT,
            "realm-1",
            "sandbox",
            &stale,
            "user_example",
            1_000,
        )
        .expect("connect");
    }
    let refresher = FakeRefresher::with(vec![grant("rotated", 50_000)]);
    let mut budget = 8;
    let prepared = worker::prepare_qbo_credentials(&state, &refresher, &mut budget, 50_000)
        .expect("prepare")
        .expect("connected");
    assert_eq!(refresher.call_count(), 1);
    assert_eq!(prepared.1, "at-rotated", "fresh access token returned");
    assert_eq!(prepared.2, 1, "one request spent on the refresh");
    assert_eq!(budget, 7);

    let persistence = state.persistence.lock();
    let credential = store::get_credential(persistence.connection_ref(), CLIENT)
        .expect("get")
        .expect("present");
    assert_eq!(
        credential.refresh_token, "rt-rotated",
        "rotated token persisted"
    );
    assert_eq!(credential.access_token.as_deref(), Some("at-rotated"));
    // Tokens never leak into the receipt spine.
    let leaked: i64 = persistence
        .connection_ref()
        .query_row(
            "SELECT COUNT(*) FROM receipts WHERE after_json LIKE '%rt-rotated%' \
             OR after_json LIKE '%at-rotated%' OR after_json LIKE '%rt-0%'",
            [],
            |row| row.get(0),
        )
        .expect("scan");
    assert_eq!(leaked, 0, "token leaked into receipts");
}

#[test]
fn mid_cycle_401_refreshes_once_then_a_second_401_fails_the_cycle() {
    let state = test_state();
    connect(&state, 1_000);
    let client = ScriptedClient {
        script: Mutex::new(vec![
            Err(AccountingError::AuthExpired {
                message: "401".to_string(),
            }),
            Err(AccountingError::AuthExpired {
                message: "401 again".to_string(),
            }),
        ]),
    };
    let recovery = FakeRecovery::allowing(5);
    let result = worker::run_sync_cycle(&state, &client, &recovery, 8, 2_000);
    assert!(
        result.is_err(),
        "second expiry with a fresh credential ends the cycle"
    );
    assert_eq!(recovery.call_count(), 1, "exactly one recovery attempt");
}

#[test]
fn realm_change_on_reconnect_wipes_the_snapshot_cache() {
    let state = test_state();
    connect(&state, 1_000);
    let fixture = FixtureAccountingReadClient {
        invoices: vec![invoice("i1", "2026-05-01T00:00:00-07:00")],
        bills: vec![bill("b1", "2026-05-01T00:00:00-07:00")],
        bills_supported: true,
        customers: vec![customer("c1", "Dana")],
        pnl: Default::default(),
        pnl_supported: false,
        ..Default::default()
    };
    run_cycle(&state, &fixture, &NoAuthRecovery, 8, 2_000);
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    store::upsert_balance_sheet_snapshot(
        conn,
        CLIENT,
        "2026-06-10",
        BalanceSheetSummary {
            cash_on_hand_cents: 55_000,
        },
        2_500,
    )
    .expect("cash");
    assert_eq!(
        store::snapshot_counts(conn, CLIENT).expect("counts"),
        (1, 1)
    );
    assert_eq!(store::bill_snapshot_count(conn, CLIENT).expect("bills"), 1);
    assert!(store::get_latest_balance_sheet_snapshot(conn, CLIENT)
        .expect("cash")
        .is_some());

    // Same realm reconnect: cache stays.
    store::store_credential(
        conn,
        CLIENT,
        "realm-1",
        "sandbox",
        &grant("1", 3_000),
        "op",
        3_000,
    )
    .expect("reconnect");
    assert_eq!(
        store::snapshot_counts(conn, CLIENT).expect("counts"),
        (1, 1)
    );
    assert_eq!(store::bill_snapshot_count(conn, CLIENT).expect("bills"), 1);
    assert!(store::get_latest_balance_sheet_snapshot(conn, CLIENT)
        .expect("cash")
        .is_some());

    // DIFFERENT realm: another company's books — wipe snapshots + cursors.
    store::store_credential(
        conn,
        CLIENT,
        "realm-2",
        "sandbox",
        &grant("2", 4_000),
        "op",
        4_000,
    )
    .expect("reconnect");
    assert_eq!(
        store::snapshot_counts(conn, CLIENT).expect("counts"),
        (0, 0)
    );
    assert_eq!(store::bill_snapshot_count(conn, CLIENT).expect("bills"), 0);
    assert!(store::get_latest_balance_sheet_snapshot(conn, CLIENT)
        .expect("cash")
        .is_none());
    let cursor = store::get_cursor(conn, CLIENT, store::ENTITY_INVOICE).expect("cursor");
    assert_eq!(cursor, QboSyncCursor::initial());
}

#[test]
fn sync_now_guard_serializes_and_cools_down() {
    let state = test_state();
    assert!(worker::try_begin_sync(&state, 1_000).is_ok());
    assert_eq!(
        worker::try_begin_sync(&state, 1_001).unwrap_err(),
        "sync_in_flight"
    );
    {
        let mut status = state
            .sync_guards
            .guard(crate::http::Pump::Accounting)
            .lock();
        status.in_flight = false;
        status.next_allowed_at_ms = 5_000;
    }
    assert_eq!(
        worker::try_begin_sync(&state, 4_999).unwrap_err(),
        "sync_cooldown"
    );
    assert!(worker::try_begin_sync(&state, 5_000).is_ok());
}

fn snapshot(
    id: &str,
    due: Option<&str>,
    txn: Option<&str>,
    total: i64,
    balance: i64,
    voided: bool,
) -> store::InvoiceSnapshotRow {
    store::InvoiceSnapshotRow {
        invoice_id: id.to_string(),
        doc_number: None,
        customer_name: None,
        txn_date: txn.map(str::to_string),
        due_date: due.map(str::to_string),
        total_amt_cents: total,
        balance_cents: balance,
        voided,
    }
}

#[test]
fn aging_buckets_classify_open_invoices_exactly() {
    // Fixed today: 2026-06-10.
    let today = "2026-06-10";
    let invoices = vec![
        snapshot("not-due", Some("2026-06-20"), None, 100, 100, false), // current
        snapshot("due-today", Some("2026-06-10"), None, 100, 50, false), // current (0 days)
        snapshot("d16", Some("2026-05-25"), None, 100, 100, false),     // 1-30
        snapshot("d45", Some("2026-04-26"), None, 100, 200, false),     // 31-60
        snapshot("d75", Some("2026-03-27"), None, 100, 300, false),     // 61-90
        snapshot("d101", Some("2026-03-01"), None, 100, 400, false),    // 90+
        snapshot("no-due", None, None, 100, 500, false),                // no_due_date
        snapshot("paid", Some("2026-03-01"), None, 100, 0, false),      // excluded
        snapshot("void", Some("2026-03-01"), None, 0, 0, true),         // excluded
    ];
    let buckets = service::compute_aging(&invoices, today);
    let by_bucket: Vec<(&str, u32, i64)> = buckets
        .iter()
        .map(|bucket| {
            (
                bucket.bucket.as_str(),
                bucket.invoice_count,
                bucket.balance_cents,
            )
        })
        .collect();
    assert_eq!(
        by_bucket,
        vec![
            ("current", 2, 150),
            ("days_1_30", 1, 100),
            ("days_31_60", 1, 200),
            ("days_61_90", 1, 300),
            ("days_90_plus", 1, 400),
            ("no_due_date", 1, 500),
        ]
    );
}

fn empty_sync_info() -> bos_contracts::accounting::AccountingSyncInfo {
    bos_contracts::accounting::AccountingSyncInfo {
        sync_enabled: false,
        in_flight: false,
        backfill_complete: true,
        last_synced_at_ms: None,
        invoice_count: 0,
        customer_count: 0,
        last_requests_used: 0,
        next_sync_allowed_at_ms: 0,
        last_error: None,
    }
}

fn pnl_row(
    kind: &str,
    start: &str,
    end: &str,
    income: i64,
    cogs: i64,
    complete: bool,
) -> store::PnlSnapshotRow {
    store::PnlSnapshotRow {
        period_kind: kind.to_string(),
        period_start: start.to_string(),
        period_end: end.to_string(),
        total_income_cents: income,
        total_cogs_cents: cogs,
        gross_profit_cents: income - cogs,
        is_complete: complete,
    }
}

#[test]
fn needed_pnl_periods_cover_four_baseline_quarters_plus_current_periods() {
    // Today 2026-06-10: current quarter starts 2026-04-01, so the baseline
    // window is 2025-04-01..2026-03-31 (the previous FOUR completed
    // quarters). Months needed: 2025-04 .. 2026-06 (15), plus two weeks.
    let periods = service::needed_pnl_periods("2026-06-10");
    let months: Vec<_> = periods.iter().filter(|p| p.kind == "month").collect();
    assert_eq!(months.len(), 15);
    assert_eq!(months[0].start, "2025-04-01");
    assert_eq!(months[0].end, "2025-04-30");
    assert!(months[0].is_complete);
    let current = months.last().expect("current month");
    assert_eq!(current.start, "2026-06-01");
    assert_eq!(current.end, "2026-06-10");
    assert!(!current.is_complete);

    let weeks: Vec<_> = periods.iter().filter(|p| p.kind == "week").collect();
    assert_eq!(weeks.len(), 2);
    assert_eq!(
        (weeks[0].start.as_str(), weeks[0].end.as_str()),
        ("2026-06-01", "2026-06-07")
    );
    assert!(weeks[0].is_complete);
    assert_eq!(
        (weeks[1].start.as_str(), weeks[1].end.as_str()),
        ("2026-06-08", "2026-06-10")
    );
    assert!(!weeks[1].is_complete);

    assert_eq!(
        service::baseline_window("2026-06-10"),
        Some(("2025-04-01".to_string(), "2026-03-31".to_string()))
    );
    // Quarter boundary: Jan 1 looks back at Q1..Q4 of the prior year.
    assert_eq!(
        service::baseline_window("2026-01-01"),
        Some(("2025-01-01".to_string(), "2025-12-31".to_string()))
    );
}

#[test]
fn financials_compute_baseline_margin_and_sales_pace() {
    let today = "2026-06-10";
    // 12 baseline months (2025-04..2026-03): margin 10_000 each except one
    // at 22_000 -> baseline avg = (11*10_000 + 22_000)/12 = 11_000.
    let mut months = Vec::new();
    let baseline_starts = [
        "2025-04-01",
        "2025-05-01",
        "2025-06-01",
        "2025-07-01",
        "2025-08-01",
        "2025-09-01",
        "2025-10-01",
        "2025-11-01",
        "2025-12-01",
        "2026-01-01",
        "2026-02-01",
        "2026-03-01",
    ];
    for (index, start) in baseline_starts.iter().enumerate() {
        let income = if index == 0 { 50_000 } else { 30_000 };
        let cogs = if index == 0 { 28_000 } else { 20_000 };
        months.push(pnl_row("month", start, start, income, cogs, true));
    }
    // Post-baseline months + the in-progress June.
    months.push(pnl_row(
        "month",
        "2026-04-01",
        "2026-04-30",
        40_000,
        24_000,
        true,
    ));
    months.push(pnl_row(
        "month",
        "2026-05-01",
        "2026-05-31",
        44_000,
        26_000,
        true,
    ));
    months.push(pnl_row(
        "month",
        "2026-06-01",
        "2026-06-10",
        25_000,
        11_000,
        false,
    ));
    let weeks = vec![
        pnl_row("week", "2026-06-01", "2026-06-07", 9_000, 4_000, true),
        pnl_row("week", "2026-06-08", "2026-06-10", 5_000, 2_000, false),
    ];
    let financials = service::compute_financials(&months, &weeks, today, empty_sync_info());
    assert_eq!(financials.week_to_date_cents, 5_000);
    assert_eq!(financials.prior_week_cents, Some(9_000));
    assert_eq!(financials.month_to_date_cents, 25_000);
    assert_eq!(financials.prior_month_cents, Some(44_000));
    assert_eq!(financials.mtd_gross_profit_cents, Some(14_000));
    assert_eq!(financials.baseline_months_cached, 12);
    assert_eq!(financials.baseline_monthly_margin_cents, Some(11_000));
    // Payment metric: June margin 14_000 - baseline 11_000.
    assert_eq!(financials.margin_above_baseline_cents, Some(3_000));
    assert_eq!(financials.months.len(), 15);

    // Missing baseline months => no baseline, no payment metric, count shown.
    let partial: Vec<_> = months
        .iter()
        .filter(|row| row.period_start.as_str() > "2025-06-01")
        .cloned()
        .collect();
    let financials = service::compute_financials(&partial, &weeks, today, empty_sync_info());
    assert_eq!(financials.baseline_months_cached, 9);
    assert!(financials.baseline_monthly_margin_cents.is_none());
    assert!(financials.margin_above_baseline_cents.is_none());
}

#[test]
fn adjusted_gross_sales_metric_uses_configured_deductions_and_baseline() {
    let today = "2026-06-10";
    let months = vec![pnl_row(
        "month",
        "2026-06-01",
        "2026-06-10",
        100_000,
        20_000,
        false,
    )];
    let weeks = vec![pnl_row(
        "week",
        "2026-06-08",
        "2026-06-10",
        40_000,
        8_000,
        false,
    )];
    let config = service::AccountingMetricBasisConfig {
        basis: service::AccountingMetricBasisKind::AdjustedGrossSales,
        label: "Adjusted gross sales".to_string(),
        baseline_cents: Some(70_000),
        freight_cents: Some(4_000),
        taxes_cents: Some(3_000),
        insurance_cents: Some(2_000),
        configured: true,
        ..service::AccountingMetricBasisConfig::default()
    };

    let financials =
        service::compute_financials_with_basis(&months, &weeks, today, empty_sync_info(), &config);

    assert_eq!(financials.metric_basis, "adjusted_gross_sales");
    assert_eq!(financials.metric_basis_label, "Adjusted gross sales");
    assert_eq!(financials.month_to_date_cents, 100_000);
    assert_eq!(financials.metric_value_cents, Some(91_000));
    assert_eq!(financials.metric_baseline_cents, Some(70_000));
    assert_eq!(financials.metric_above_baseline_cents, Some(21_000));
    assert_eq!(financials.metric_pending_reason, None);
    // Legacy gross-margin fields remain available, but are not the configured metric.
    assert_eq!(financials.mtd_gross_profit_cents, Some(80_000));
}

#[test]
fn adjusted_gross_sales_metric_is_pending_when_formula_inputs_are_missing() {
    let today = "2026-06-10";
    let months = vec![pnl_row(
        "month",
        "2026-06-01",
        "2026-06-10",
        100_000,
        20_000,
        false,
    )];
    let config = service::AccountingMetricBasisConfig {
        basis: service::AccountingMetricBasisKind::AdjustedGrossSales,
        label: "Adjusted gross sales".to_string(),
        baseline_cents: Some(70_000),
        freight_cents: Some(4_000),
        taxes_cents: None,
        insurance_cents: Some(2_000),
        configured: true,
        ..service::AccountingMetricBasisConfig::default()
    };

    let financials =
        service::compute_financials_with_basis(&months, &[], today, empty_sync_info(), &config);

    assert_eq!(financials.metric_value_cents, None);
    assert_eq!(financials.metric_above_baseline_cents, None);
    assert!(financials
        .metric_pending_reason
        .as_deref()
        .unwrap_or_default()
        .contains("taxes"));
}

#[test]
fn non_pnl_provider_defaults_unconfigured_metric_to_invoice_totals() {
    let effective = service::effective_metric_config_for_provider(
        "invoice_ninja",
        &service::AccountingMetricBasisConfig::default(),
    );

    assert_eq!(
        effective.basis,
        service::AccountingMetricBasisKind::InvoiceTotals
    );
    assert_eq!(effective.label, "Invoice totals");
    assert!(!effective.configured);

    let today = "2026-06-10";
    let invoices = vec![snapshot("wtd", None, Some("2026-06-09"), 1_000, 0, false)];
    let financials = service::compute_financials_from_invoices(&invoices, today, empty_sync_info());
    assert_eq!(financials.metric_basis, "invoice_totals");
    assert_eq!(financials.metric_value_cents, Some(1_000));
    assert_eq!(financials.metric_baseline_cents, None);
    assert_eq!(financials.metric_above_baseline_cents, None);
    assert_eq!(financials.metric_pending_reason, None);
}

#[test]
fn configured_invoice_totals_metric_reports_delta_or_pending_baseline() {
    let today = "2026-06-10";
    let invoices = vec![
        snapshot("wtd", None, Some("2026-06-09"), 1_000, 0, false),
        snapshot("mtd", None, Some("2026-06-01"), 4_000, 0, false),
    ];
    let base = service::compute_financials_from_invoices(&invoices, today, empty_sync_info());
    let with_baseline = service::apply_metric_basis(
        base.clone(),
        &service::AccountingMetricBasisConfig {
            basis: service::AccountingMetricBasisKind::InvoiceTotals,
            label: "Invoice totals".to_string(),
            baseline_cents: Some(3_000),
            configured: true,
            basis_explicit: true,
            ..service::AccountingMetricBasisConfig::default()
        },
    );

    assert_eq!(with_baseline.metric_value_cents, Some(5_000));
    assert_eq!(with_baseline.metric_baseline_cents, Some(3_000));
    assert_eq!(with_baseline.metric_above_baseline_cents, Some(2_000));
    assert_eq!(with_baseline.metric_pending_reason, None);

    let pending = service::apply_metric_basis(
        base,
        &service::AccountingMetricBasisConfig {
            basis: service::AccountingMetricBasisKind::InvoiceTotals,
            label: "Invoice totals".to_string(),
            configured: true,
            basis_explicit: true,
            ..service::AccountingMetricBasisConfig::default()
        },
    );
    assert_eq!(
        pending.metric_pending_reason.as_deref(),
        Some("Invoice-total baseline is not configured.")
    );
}

#[test]
fn pnl_sync_fetches_complete_periods_once_and_stays_receipt_quiet() {
    let state = test_state();
    // 2026-06-10 12:00 UTC; connect just before so the access token is fresh.
    let now: u64 = 1_781_092_800_000;
    connect(&state, now - 1_000);
    let mut fixture = FixtureAccountingReadClient::with_pnl_support();
    for period in service::needed_pnl_periods("2026-06-10") {
        fixture.pnl.insert(
            (period.start.clone(), period.end.clone()),
            PnlSummary {
                total_income_cents: 10_000,
                total_cogs_cents: 4_000,
                gross_profit_cents: 6_000,
            },
        );
    }
    // 17 periods (15 months + 2 weeks) + daily-revenue report at budget 20
    // → all fetched.
    let summary = run_cycle(&state, &fixture, &NoAuthRecovery, 20, now);
    assert_eq!(
        summary.requests_used,
        17 + 1 + 2,
        "17 P&L + daily revenue + 2 empty entity pages"
    );
    assert_eq!(summary.written, 17);
    let after_first = receipt_count(&state);

    // Next cycle: only the 2 current periods re-fetch; nothing changed, so
    // zero receipts.
    let summary = run_cycle(&state, &fixture, &NoAuthRecovery, 20, now + 60_000);
    assert_eq!(
        summary.requests_used,
        2 + 1 + 2,
        "current month + week + daily revenue + 2 entity pages"
    );
    assert_eq!(summary.written, 0);
    assert_eq!(
        receipt_count(&state),
        after_first,
        "quiet P&L cycle wrote receipts"
    );

    // The financials read model assembles from the cache.
    let persistence = state.persistence.lock();
    let conn = persistence.connection_ref();
    let months = store::list_pnl_snapshots(conn, CLIENT, "month").expect("months");
    assert_eq!(months.len(), 15);
    let financials = service::compute_financials(
        &months,
        &store::list_pnl_snapshots(conn, CLIENT, "week").expect("weeks"),
        "2026-06-10",
        empty_sync_info(),
    );
    assert_eq!(financials.baseline_monthly_margin_cents, Some(6_000));
    assert_eq!(financials.margin_above_baseline_cents, Some(0));
}

#[test]
fn daily_revenue_sync_stores_day_pnl_rows() {
    let state = test_state();
    let now: u64 = 1_781_092_800_000; // 2026-06-10 12:00 UTC.
    connect(&state, now - 1_000);
    let mut fixture = FixtureAccountingReadClient::with_pnl_support();
    for period in service::needed_pnl_periods("2026-06-10") {
        fixture.pnl.insert(
            (period.start.clone(), period.end.clone()),
            PnlSummary::default(),
        );
    }
    fixture.daily_pnl.insert(
        ("2026-05-01".to_string(), "2026-06-10".to_string()),
        vec![
            PnlDailySummary {
                date: "2026-06-09".to_string(),
                total_income_cents: 12_000,
            },
            PnlDailySummary {
                date: "2026-06-10".to_string(),
                total_income_cents: 34_000,
            },
        ],
    );

    let summary = run_cycle(&state, &fixture, &NoAuthRecovery, 30, now);
    assert_eq!(summary.written, 19, "17 regular P&L + 2 day rows");
    let persistence = state.persistence.lock();
    let rows =
        service::daily_revenue_from_store(persistence.connection_ref(), CLIENT, "2026-06-10")
            .expect("daily revenue");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].total_income_cents, 12_000);
    assert_eq!(rows[1].total_income_cents, 34_000);
}

#[test]
fn balance_sheet_sync_caches_cash_on_hand_receipt_quietly() {
    let state = test_state();
    connect(&state, 1_000);
    let fixture = FixtureAccountingReadClient {
        balance_sheet_supported: true,
        balance_sheet: Some(BalanceSheetSummary {
            cash_on_hand_cents: 123_456,
        }),
        pnl_supported: false,
        ..Default::default()
    };
    let summary = run_cycle(&state, &fixture, &NoAuthRecovery, 8, 1_781_092_800_000);
    assert_eq!(summary.requests_used, 3, "customer, invoice, balance sheet");
    assert_eq!(summary.written, 1);
    let after_first = receipt_count(&state);
    {
        let persistence = state.persistence.lock();
        let row = store::get_latest_balance_sheet_snapshot(persistence.connection_ref(), CLIENT)
            .expect("cash")
            .expect("cash row");
        assert_eq!(row.cash_on_hand_cents, 123_456);
    }
    let summary = run_cycle(&state, &fixture, &NoAuthRecovery, 8, 1_781_092_860_000);
    assert_eq!(summary.written, 0);
    assert_eq!(receipt_count(&state), after_first);
}

#[test]
fn invoice_row_status_and_days_overdue() {
    let today = "2026-06-10";
    let row = service::invoice_row(
        &snapshot("x", Some("2026-05-31"), None, 100, 100, false),
        today,
    );
    assert_eq!(row.status, "overdue");
    assert_eq!(row.days_overdue, 10);
    let row = service::invoice_row(
        &snapshot("y", Some("2026-06-20"), None, 100, 100, false),
        today,
    );
    assert_eq!(row.status, "open");
    assert_eq!(row.days_overdue, 0);
    let row = service::invoice_row(&snapshot("z", None, None, 100, 0, false), today);
    assert_eq!(row.status, "paid");
    let row = service::invoice_row(&snapshot("v", None, None, 0, 0, true), today);
    assert_eq!(row.status, "voided");
}

#[test]
fn today_string_converts_epoch_to_civil_date() {
    // 2026-06-10 00:00:00 UTC.
    assert_eq!(service::today_string(1_781_049_600_000), "2026-06-10");
    assert_eq!(service::today_string(0), "1970-01-01");
}

#[test]
fn pnl_step_is_skipped_for_providers_without_pnl() {
    let state = test_state();
    connect(&state, 1_000);
    let fixture = FixtureAccountingReadClient {
        invoices: vec![invoice("i1", "2026-05-01T00:00:00-07:00")],
        customers: vec![customer("c1", "Dana")],
        pnl: Default::default(),
        pnl_supported: false,
        ..Default::default()
    };
    let summary = run_cycle(&state, &fixture, &NoAuthRecovery, 20, 2_000);
    assert_eq!(summary.requests_used, 2, "entities only — zero P&L fetches");
    let persistence = state.persistence.lock();
    let conn = persistence.connection_ref();
    let months = store::list_pnl_snapshots(conn, CLIENT, "month").expect("months");
    assert!(months.is_empty(), "no P&L rows for a no-P&L provider");
    let pnl_cursor: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM accounting_sync_cursors WHERE entity = 'pnl'",
            [],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(pnl_cursor, 0, "no pnl cursor row is ever created");
}

#[test]
fn purge_disconnect_drops_every_cached_row_in_one_receipted_mutation() {
    let state = test_state();
    connect(&state, 1_000);
    let mut fixture = FixtureAccountingReadClient::with_pnl_support();
    fixture.invoices = vec![invoice("i1", "2026-05-01T00:00:00-07:00")];
    fixture.bills = vec![bill("b1", "2026-05-01T00:00:00-07:00")];
    fixture.bills_supported = true;
    fixture.customers = vec![customer("c1", "Dana")];
    run_cycle(&state, &fixture, &NoAuthRecovery, 25, 1_781_092_800_000);
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    store::upsert_balance_sheet_snapshot(
        conn,
        CLIENT,
        "2026-06-10",
        BalanceSheetSummary {
            cash_on_hand_cents: 44_000,
        },
        1_781_092_800_001,
    )
    .expect("cash");
    assert_eq!(
        store::snapshot_counts(conn, CLIENT).expect("counts"),
        (1, 1)
    );
    assert_eq!(store::bill_snapshot_count(conn, CLIENT).expect("bills"), 1);
    assert!(store::get_latest_balance_sheet_snapshot(conn, CLIENT)
        .expect("cash")
        .is_some());
    let before_receipts: i64 = conn
        .query_row("SELECT COUNT(*) FROM receipts", [], |row| row.get(0))
        .expect("count");

    // Plain disconnect keeps the cache.
    store::delete_credential(conn, CLIENT, "user_example", false, 2_000).expect("disconnect");
    assert_eq!(
        store::snapshot_counts(conn, CLIENT).expect("counts"),
        (1, 1)
    );
    assert_eq!(store::bill_snapshot_count(conn, CLIENT).expect("bills"), 1);

    // Purge disconnect drops snapshots, P&L periods, and cursors — exactly
    // one receipt for the whole operation.
    store::delete_credential(conn, CLIENT, "user_example", true, 3_000).expect("purge");
    assert_eq!(
        store::snapshot_counts(conn, CLIENT).expect("counts"),
        (0, 0)
    );
    assert_eq!(store::bill_snapshot_count(conn, CLIENT).expect("bills"), 0);
    let pnl_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM accounting_pnl_snapshots", [], |row| {
            row.get(0)
        })
        .expect("count");
    let balance_sheet_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM accounting_balance_sheet_snapshots",
            [],
            |row| row.get(0),
        )
        .expect("count");
    let cursor_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM accounting_sync_cursors", [], |row| {
            row.get(0)
        })
        .expect("count");
    assert_eq!((pnl_rows, balance_sheet_rows, cursor_rows), (0, 0, 0));
    let after_receipts: i64 = conn
        .query_row("SELECT COUNT(*) FROM receipts", [], |row| row.get(0))
        .expect("count");
    assert_eq!(
        after_receipts - before_receipts,
        2,
        "one receipt per disconnect call, nothing per purged table"
    );
}

#[test]
fn invoice_totals_financials_sum_sales_without_margin_fields() {
    // Today 2026-06-10 (Wednesday): WTD [06-08..], prior week [06-01..06-07].
    let today = "2026-06-10";
    let invoices = vec![
        snapshot("wtd", None, Some("2026-06-09"), 1_000, 0, false),
        snapshot("prior-week", None, Some("2026-06-03"), 2_000, 0, false),
        snapshot("mtd-other", None, Some("2026-06-02"), 4_000, 0, false),
        snapshot("prior-week-tail", None, Some("2026-06-05"), 5_000, 0, false),
        snapshot("prior-mtd", None, Some("2026-05-03"), 3_000, 0, false),
        snapshot("prior-month", None, Some("2026-05-20"), 8_000, 0, false),
        snapshot("older", None, Some("2026-04-10"), 16_000, 0, false),
        snapshot("void", None, Some("2026-06-09"), 32_000, 0, true),
        snapshot("future", None, Some("2026-06-12"), 64_000, 0, false),
    ];
    let financials = service::compute_financials_from_invoices(&invoices, today, empty_sync_info());
    assert_eq!(financials.basis, "invoice_totals");
    assert_eq!(financials.week_to_date_cents, 1_000);
    assert_eq!(financials.prior_week_cents, Some(2_000 + 4_000 + 5_000));
    assert_eq!(financials.prior_week_to_date_cents, Some(2_000 + 4_000));
    assert_eq!(
        financials.month_to_date_cents,
        1_000 + 2_000 + 4_000 + 5_000
    );
    assert_eq!(financials.prior_month_cents, Some(3_000 + 8_000));
    assert_eq!(financials.prior_month_to_date_cents, Some(3_000));
    assert!(financials.mtd_gross_profit_cents.is_none());
    assert!(financials.baseline_monthly_margin_cents.is_none());
    assert!(financials.margin_above_baseline_cents.is_none());
    // Monthly trend: Apr, May, June (current month incomplete).
    let months: Vec<(&str, i64, bool)> = financials
        .months
        .iter()
        .map(|month| {
            (
                month.month_start.as_str(),
                month.total_income_cents,
                month.is_complete,
            )
        })
        .collect();
    assert_eq!(
        months,
        vec![
            ("2026-04-01", 16_000, true),
            ("2026-05-01", 11_000, true),
            ("2026-06-01", 12_000, false),
        ]
    );
    assert!(financials
        .months
        .iter()
        .all(|m| m.gross_profit_cents.is_none()));
}
