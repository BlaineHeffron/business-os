use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use bos_integrations::crm_read::{
    CrmContactRecord, CrmDealRecord, CrmPage, CrmPageRequest, CrmReadClient, CrmReadError,
};

use super::{service, store, worker};
use crate::http::test_support::{test_state, EnvGuard};
use crate::http::OperatorScope;
use crate::slices::email_triage;

const CLIENT: &str = "test-client";

fn contact(id: &str, email: &str, company: &str) -> CrmContactRecord {
    CrmContactRecord {
        provider_contact_id: id.to_string(),
        email: Some(email.to_string()),
        name: Some("Dana Customer".to_string()),
        company: Some(company.to_string()),
        phone: Some("555-0100".to_string()),
        lifecycle_stage: Some("lead".to_string()),
        owner: Some("owner-1".to_string()),
        last_activity_at: Some("2026-06-01T00:00:00Z".to_string()),
    }
}

fn deal(id: &str, email: &str) -> CrmDealRecord {
    CrmDealRecord {
        provider_deal_id: id.to_string(),
        name: Some("renovation project".to_string()),
        stage: Some("qualified".to_string()),
        amount_cents: Some(12_345),
        currency: Some("USD".to_string()),
        pipeline: Some("sales".to_string()),
        close_date: Some("2026-07-01".to_string()),
        associated_contact_ids: vec!["c1".to_string()],
        associated_contact_email: Some(email.to_string()),
        associated_contact_company: Some("Acme Co".to_string()),
    }
}

fn inbound(id: &str, from_addr: &str) -> email_triage::store::InboundMessageRecord {
    email_triage::store::InboundMessageRecord {
        source_key: id.to_string(),
        message_id: id.to_string(),
        thread_id: Some(format!("thread-{id}")),
        internal_date_ms: Some(1_000),
        from_addr: Some(from_addr.to_string()),
        to_addr: Some("ops@example.test".to_string()),
        subject: Some("Customer message".to_string()),
        body_excerpt: "Body".to_string(),
        body_full: "Body".to_string(),
        labels: vec!["INBOX".to_string()],
        headers: Vec::new(),
        resolved_category: bos_contracts::email_triage::FALLBACK_CATEGORY_ID.to_string(),
        matched_rule_id: None,
        ingested_at_ms: 1_000,
        ai_triage_status: None,
        ai_triage_rationale: None,
        attachments: Vec::new(),
        source_user_id: None,
    }
}

struct FakeCrmReadClient {
    contact_pages: Mutex<VecDeque<Vec<CrmContactRecord>>>,
    contact_calls: AtomicUsize,
}

impl FakeCrmReadClient {
    fn new(contact_pages: Vec<Vec<CrmContactRecord>>) -> Self {
        Self {
            contact_pages: Mutex::new(contact_pages.into()),
            contact_calls: AtomicUsize::new(0),
        }
    }

    fn contact_calls(&self) -> usize {
        self.contact_calls.load(Ordering::SeqCst)
    }
}

impl CrmReadClient for FakeCrmReadClient {
    fn list_contacts_page(
        &self,
        _request: &CrmPageRequest,
    ) -> Result<CrmPage<CrmContactRecord>, CrmReadError> {
        self.contact_calls.fetch_add(1, Ordering::SeqCst);
        let records = self
            .contact_pages
            .lock()
            .expect("contact pages")
            .pop_front()
            .unwrap_or_default();
        Ok(CrmPage {
            records,
            next_cursor: None,
        })
    }

    fn list_deals_page(
        &self,
        _request: &CrmPageRequest,
    ) -> Result<CrmPage<CrmDealRecord>, CrmReadError> {
        Ok(CrmPage {
            records: Vec::new(),
            next_cursor: None,
        })
    }
}

#[test]
fn contact_snapshot_upsert_is_receipt_quiet_when_unchanged() {
    let state = test_state();
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let records = vec![contact("c1", "dana@example.com", "Acme Co")];

    let first = store::upsert_contact_snapshots(conn, CLIENT, &records, 1_000).expect("first");
    let second = store::upsert_contact_snapshots(conn, CLIENT, &records, 1_000).expect("second");

    assert_eq!(first.written, 1);
    assert_eq!(second.unchanged, 1);
    let receipts =
        crate::store_core::receipts_for_entity(conn, CLIENT, "crm_contact_snapshot", "c1", 10)
            .expect("receipts");
    assert_eq!(receipts.len(), 1);
}

#[test]
fn completed_refresh_tombstones_contacts_not_seen_again() {
    let state = test_state();
    let client = FakeCrmReadClient::new(vec![
        vec![
            contact("c1", "dana@example.com", "Acme Co"),
            contact("c2", "old@example.com", "Old Co"),
        ],
        vec![contact("c1", "dana@example.com", "Acme Co")],
    ]);

    let first = worker::run_sync_cycle(
        &state,
        &client,
        false,
        10,
        1_000,
        false,
        Duration::from_secs(300),
    )
    .expect("first sync");
    assert_eq!(first.written, 2);

    let second = worker::run_sync_cycle(
        &state,
        &client,
        false,
        10,
        2_000,
        true,
        Duration::from_secs(300),
    )
    .expect("second sync");
    assert_eq!(second.unchanged, 1);
    assert_eq!(second.tombstoned, 1);

    let persistence = state.persistence.lock();
    let conn = persistence.connection_ref();
    let active = service::contacts_by_email(
        conn,
        &state.client_id,
        &OperatorScope::All,
        "dana@example.com",
    )
    .expect("active contact");
    let tombstoned = service::contacts_by_email(
        conn,
        &state.client_id,
        &OperatorScope::All,
        "old@example.com",
    )
    .expect("old contact");
    let counts = store::snapshot_counts(conn, &state.client_id).expect("counts");
    assert_eq!(active.len(), 1);
    assert!(tombstoned.is_empty());
    assert_eq!(counts.contacts, 1);
}

#[test]
fn empty_successful_sync_updates_status_freshness() {
    let state = test_state();
    let client = FakeCrmReadClient::new(vec![Vec::new(), Vec::new()]);

    let summary = worker::run_sync_cycle(
        &state,
        &client,
        false,
        10,
        1_000,
        false,
        Duration::from_secs(300),
    )
    .expect("empty sync");
    assert_eq!(summary.requests_used, 1);
    let second = worker::run_sync_cycle(
        &state,
        &client,
        false,
        10,
        2_000,
        true,
        Duration::from_secs(300),
    )
    .expect("second empty sync");
    assert_eq!(second.requests_used, 1);

    let persistence = state.persistence.lock();
    let guard = state
        .sync_guards
        .guard(crate::http::Pump::CrmCache)
        .lock()
        .clone();
    let info = service::sync_info(persistence.connection_ref(), &state.client_id, &guard)
        .expect("sync info");
    assert_eq!(info.contact_count, 0);
    assert_eq!(info.last_synced_at_ms, Some(2_000));
}

#[test]
fn deal_amounts_redact_for_user_when_cache_has_deals() {
    let state = test_state();
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let deals = vec![deal("d1", "dana@example.com")];
    store::upsert_deal_snapshots(conn, CLIENT, &deals, 1_000).expect("deal");

    let all = service::deals_by_contact(conn, CLIENT, &OperatorScope::All, "dana@example.com")
        .expect("all");
    let user = service::deals_by_contact(
        conn,
        CLIENT,
        &OperatorScope::User("user_casey".to_string()),
        "dana@example.com",
    )
    .expect("user");

    assert_eq!(all[0].amount_cents, Some(12_345));
    assert_eq!(all[0].currency.as_deref(), Some("USD"));
    assert_eq!(user[0].amount_cents, None);
    assert_eq!(user[0].currency, None);
    assert_eq!(user[0].name.as_deref(), Some("renovation project"));
}

#[test]
fn hubspot_cache_rows_include_portal_aware_deep_links() {
    let _env = EnvGuard::set_many(&[
        ("BOS_CRM_PROVIDER", "hubspot"),
        ("BOS_HUBSPOT_PORTAL_ID", "123456"),
    ]);
    let state = test_state();
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    store::upsert_contact_snapshots(
        conn,
        CLIENT,
        &[contact("101", "dana@example.com", "Acme Co")],
        1_000,
    )
    .expect("contact");
    store::upsert_deal_snapshots(conn, CLIENT, &[deal("202", "dana@example.com")], 1_000)
        .expect("deal");

    let contacts =
        service::contacts_by_email(conn, CLIENT, &OperatorScope::All, "dana@example.com")
            .expect("contacts");
    let deals = service::deals_by_contact(conn, CLIENT, &OperatorScope::All, "dana@example.com")
        .expect("deals");

    assert_eq!(contacts[0].provider, "hubspot");
    assert_eq!(
        contacts[0].contact_url.as_deref(),
        Some("https://app.hubspot.com/contacts/123456/record/0-1/101")
    );
    assert_eq!(
        deals[0].deal_url.as_deref(),
        Some("https://app.hubspot.com/contacts/123456/record/0-3/202")
    );
}

#[test]
fn hubspot_cache_rows_survive_missing_portal_id_without_links() {
    let _env = EnvGuard::set_many(&[
        ("BOS_CRM_PROVIDER", "hubspot"),
        ("BOS_HUBSPOT_PORTAL_ID", ""),
    ]);
    let state = test_state();
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    store::upsert_contact_snapshots(
        conn,
        CLIENT,
        &[contact("101", "dana@example.com", "Acme Co")],
        1_000,
    )
    .expect("contact");

    let contacts =
        service::contacts_by_email(conn, CLIENT, &OperatorScope::All, "dana@example.com")
            .expect("contacts");

    assert_eq!(contacts.len(), 1);
    assert_eq!(contacts[0].provider, "hubspot");
    assert_eq!(contacts[0].contact_url, None);
}

#[test]
fn source_context_skips_shopify_platform_sender() {
    let _env = EnvGuard::set_many(&[
        ("BOS_CRM_PROVIDER", "hubspot"),
        ("BOS_HUBSPOT_PORTAL_ID", "123456"),
    ]);
    let state = test_state();
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    store::upsert_contact_snapshots(
        conn,
        CLIENT,
        &[contact(
            "101",
            "mailer@shopify.com",
            "America's Best Varnish",
        )],
        1_000,
    )
    .expect("contact");
    email_triage::store::record_inbound_message(
        conn,
        CLIENT,
        &inbound(
            "msg-shopify",
            "Shopify <mailer@bounce.notifications.shopify.com>",
        ),
    )
    .expect("inbound");

    let context = service::context_for_source(conn, CLIENT, &OperatorScope::All, "msg-shopify")
        .expect("context");

    assert!(context.contacts.is_empty());
    assert!(context.deals.is_empty());
    assert_eq!(
        context.skipped_reason.as_deref(),
        Some("sender_is_platform_or_automation")
    );
}

#[test]
fn source_context_allows_real_person_at_neutral_domain() {
    let _env = EnvGuard::set_many(&[
        ("BOS_CRM_PROVIDER", "hubspot"),
        ("BOS_HUBSPOT_PORTAL_ID", "123456"),
    ]);
    let state = test_state();
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    store::upsert_contact_snapshots(
        conn,
        CLIENT,
        &[contact("101", "dana@shopify.com", "Shopify")],
        1_000,
    )
    .expect("contact");
    email_triage::store::record_inbound_message(
        conn,
        CLIENT,
        &inbound("msg-person", "Dana <dana@shopify.com>"),
    )
    .expect("inbound");

    let context = service::context_for_source(conn, CLIENT, &OperatorScope::All, "msg-person")
        .expect("context");

    assert_eq!(context.lookup_email.as_deref(), Some("dana@shopify.com"));
    assert_eq!(context.contacts.len(), 1);
}

#[test]
fn source_context_returns_hubspot_customer_link_for_real_sender() {
    let _env = EnvGuard::set_many(&[
        ("BOS_CRM_PROVIDER", "hubspot"),
        ("BOS_HUBSPOT_PORTAL_ID", "123456"),
    ]);
    let state = test_state();
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    store::upsert_contact_snapshots(
        conn,
        CLIENT,
        &[contact("101", "dana@example.com", "Acme Co")],
        1_000,
    )
    .expect("contact");
    store::upsert_deal_snapshots(conn, CLIENT, &[deal("202", "dana@example.com")], 1_000)
        .expect("deal");
    email_triage::store::record_inbound_message(
        conn,
        CLIENT,
        &inbound("msg-customer", "Dana <dana@example.com>"),
    )
    .expect("inbound");

    let context = service::context_for_source(conn, CLIENT, &OperatorScope::All, "msg-customer")
        .expect("context");

    assert_eq!(context.lookup_email.as_deref(), Some("dana@example.com"));
    assert_eq!(context.contacts.len(), 1);
    assert_eq!(context.deals.len(), 1);
    assert_eq!(
        context.contacts[0].contact_url.as_deref(),
        Some("https://app.hubspot.com/contacts/123456/record/0-1/101")
    );
    assert!(context.hubspot_links_configured);
}

#[test]
fn espocrm_cache_rows_include_base_url_deep_links() {
    let _env = EnvGuard::set_many(&[
        ("BOS_CRM_PROVIDER", "espocrm"),
        ("BOS_ESPOCRM_BASE_URL", "https://crm.example.test/"),
    ]);
    let state = test_state();
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    store::upsert_contact_snapshots(
        conn,
        CLIENT,
        &[contact("con-1", "dana@example.com", "Acme Co")],
        1_000,
    )
    .expect("contact");

    let contacts =
        service::contacts_by_email(conn, CLIENT, &OperatorScope::All, "dana@example.com")
            .expect("contacts");

    assert_eq!(contacts[0].provider, "espocrm");
    assert_eq!(
        contacts[0].contact_url.as_deref(),
        Some("https://crm.example.test/#Contact/view/con-1")
    );
}

#[test]
fn espocrm_cache_links_reject_invalid_base_url() {
    let _env = EnvGuard::set_many(&[
        ("BOS_CRM_PROVIDER", "espocrm"),
        ("BOS_ESPOCRM_BASE_URL", "crm.example.test"),
    ]);
    let state = test_state();
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    store::upsert_contact_snapshots(
        conn,
        CLIENT,
        &[contact("con-1", "dana@example.com", "Acme Co")],
        1_000,
    )
    .expect("contact");

    let contacts =
        service::contacts_by_email(conn, CLIENT, &OperatorScope::All, "dana@example.com")
            .expect("contacts");

    assert_eq!(contacts[0].provider, "espocrm");
    assert_eq!(contacts[0].contact_url, None);
}

#[test]
fn force_refresh_rewalks_after_backfill_complete() {
    let state = test_state();
    let mut refreshed = contact("c1", "dana@example.com", "Acme Co");
    refreshed.name = Some("Dana Refreshed".to_string());
    let client = FakeCrmReadClient::new(vec![
        vec![contact("c1", "dana@example.com", "Acme Co")],
        vec![refreshed],
    ]);

    let first = worker::run_sync_cycle(
        &state,
        &client,
        false,
        10,
        1_000,
        false,
        Duration::from_secs(300),
    )
    .expect("first sync");
    assert_eq!(first.requests_used, 1);
    assert_eq!(client.contact_calls(), 1);
    {
        let persistence = state.persistence.lock();
        let cursor = store::get_cursor(
            persistence.connection_ref(),
            &state.client_id,
            store::ENTITY_CONTACT,
        )
        .expect("cursor");
        assert!(cursor.backfill_complete);
    }

    let too_soon = worker::run_sync_cycle(
        &state,
        &client,
        false,
        10,
        2_000,
        false,
        Duration::from_secs(300),
    )
    .expect("too soon sync");
    assert_eq!(too_soon.requests_used, 0);
    assert_eq!(client.contact_calls(), 1);

    let forced = worker::run_sync_cycle(
        &state,
        &client,
        false,
        10,
        3_000,
        true,
        Duration::from_secs(300),
    )
    .expect("forced sync");
    assert_eq!(forced.requests_used, 1);
    assert_eq!(client.contact_calls(), 2);

    let persistence = state.persistence.lock();
    let contacts = service::contacts_by_email(
        persistence.connection_ref(),
        &state.client_id,
        &OperatorScope::All,
        "dana@example.com",
    )
    .expect("contacts");
    assert_eq!(contacts[0].name.as_deref(), Some("Dana Refreshed"));
}
