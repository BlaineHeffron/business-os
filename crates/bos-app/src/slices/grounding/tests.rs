use super::*;
use bos_integrations::accounting_read::{CustomerRecord, InvoiceRecord, TierSource};
use bos_integrations::crm_read::{CrmContactRecord, CrmDealRecord};
use bos_integrations::shopify_sales_read::{
    ShopifyCustomerRecord, ShopifyLineItemRecord, ShopifyMoney, ShopifyOrderRecord,
};
use bos_integrations::stockforge_read::SfOrderCardRecord;
use rusqlite::Connection;

use crate::http::test_support::EnvGuard;
use crate::http::OperatorScope;
use crate::overlay::AccountingVisibilityPolicy;

const CLIENT: &str = "test-client";

#[test]
fn resolve_party_selects_unambiguous_exact_email() {
    let mut persistence = crate::persistence::Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    seed_customer(
        conn,
        "c1",
        "Acme Buyer",
        Some("Acme Co"),
        Some("buyer@business-86b318398f.test"),
    );

    let result = resolve_party(
        conn,
        CLIENT,
        &OperatorScope::All,
        Some("BUYER@business-86b318398f.test"),
        None,
    )
    .expect("resolve");

    assert_eq!(result.confidence, "high");
    assert_eq!(result.reason, "exact_email");
    assert_eq!(
        result.selected.as_ref().and_then(|p| p.email.as_deref()),
        Some("buyer@business-86b318398f.test")
    );
}

#[test]
fn resolve_party_never_auto_selects_name_match() {
    let mut persistence = crate::persistence::Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    seed_customer(
        conn,
        "c1",
        "Acme Buyer",
        Some("Acme Co"),
        Some("buyer@business-86b318398f.test"),
    );

    let result =
        resolve_party(conn, CLIENT, &OperatorScope::All, None, Some("Acme Co")).expect("resolve");

    assert!(result.selected.is_none());
    assert_eq!(result.reason, "name_candidates_only");
    assert_eq!(result.candidates.len(), 1);
}

#[test]
fn resolve_party_returns_no_match_without_candidates() {
    let persistence = crate::persistence::Persistence::open_in_memory().expect("db");
    let conn = persistence.connection_ref();

    let result = resolve_party(
        conn,
        CLIENT,
        &OperatorScope::All,
        Some("missing@example.test"),
        Some("Missing"),
    )
    .expect("resolve");

    assert!(result.selected.is_none());
    assert_eq!(result.confidence, "none");
    assert_eq!(result.reason, "no_match");
}

#[test]
fn resolve_party_treats_duplicate_email_as_ambiguous() {
    let mut persistence = crate::persistence::Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    seed_customer(
        conn,
        "c1",
        "Acme One",
        Some("Acme One"),
        Some("ops@business-86b318398f.test"),
    );
    seed_order(
        conn,
        "o1",
        "#1001",
        Some("Acme Two"),
        Some("ops@business-86b318398f.test"),
    );

    let result = resolve_party(
        conn,
        CLIENT,
        &OperatorScope::All,
        Some("ops@business-86b318398f.test"),
        None,
    )
    .expect("resolve");

    assert!(result.selected.is_none());
    assert_eq!(result.confidence, "ambiguous");
    assert_eq!(result.reason, "ambiguous_email");
    assert_eq!(result.candidates.len(), 2);
}

#[test]
fn customer_invoice_history_denies_without_financial_visibility() {
    let persistence = crate::persistence::Persistence::open_in_memory().expect("db");
    let conn = persistence.connection_ref();
    let party = PartyCandidate {
        source: "accounting_customer".to_string(),
        source_id: "c1".to_string(),
        display_name: Some("Acme Buyer".to_string()),
        company_name: Some("Acme Co".to_string()),
        email: Some("buyer@business-86b318398f.test".to_string()),
    };

    let history = customer_invoice_history(
        conn,
        CLIENT,
        &OperatorScope::User("user_scoped".to_string()),
        AccountingVisibilityPolicy::AdminOnly,
        Some(&party),
        1_800_000_000_000,
    )
    .expect("history");

    assert!(!history.allowed);
    assert_eq!(
        history.denied_reason.as_deref(),
        Some("accounting_visibility_denied")
    );
    assert!(history.invoices.is_empty());
    assert_eq!(history.open_balance_cents, 0);
}

#[test]
fn customer_invoice_history_balances_use_full_match_set_before_display_cap() {
    let mut persistence = crate::persistence::Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let party = PartyCandidate {
        source: "accounting_customer".to_string(),
        source_id: "c1".to_string(),
        display_name: Some("Acme Buyer".to_string()),
        company_name: Some("Acme Co".to_string()),
        email: Some("buyer@business-86b318398f.test".to_string()),
    };
    let invoices = (0..10)
        .map(|index| invoice_record(index, "Acme Co", 100))
        .collect::<Vec<_>>();
    crate::slices::accounting::store::upsert_invoice_snapshots(conn, CLIENT, &invoices, 1_000)
        .expect("invoices");

    let history = customer_invoice_history(
        conn,
        CLIENT,
        &OperatorScope::All,
        AccountingVisibilityPolicy::Shared,
        Some(&party),
        1_800_000_000_000,
    )
    .expect("history");

    assert!(history.allowed);
    assert_eq!(history.invoices.len(), MAX_INVOICES);
    assert_eq!(history.open_balance_cents, 1_000);
    assert_eq!(history.overdue_balance_cents, 1_000);
}

#[test]
fn call_transcript_lookup_denies_user_scope() {
    let persistence = crate::persistence::Persistence::open_in_memory().expect("db");
    let conn = persistence.connection_ref();

    let result = call_transcript_lookup(
        conn,
        CLIENT,
        &OperatorScope::User("user_scoped".to_string()),
        "Acme",
    )
    .expect("call lookup");

    assert!(!result.allowed);
    assert_eq!(
        result.denied_reason.as_deref(),
        Some("call_transcript_scope_denied")
    );
    assert!(result.calls.is_empty());
}

#[test]
fn order_status_lookup_merges_shopify_source_and_renders_shopify_money_only() {
    let _env = EnvGuard::set("BOS_SHOPIFY_SALES_VISIBILITY_POLICY", "shared");
    let mut persistence = crate::persistence::Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    seed_order(
        conn,
        "o1",
        "#1001",
        Some("Acme Buyer"),
        Some("buyer@business-86b318398f.test"),
    );
    seed_shopify_order(
        conn,
        "gid://shopify/Order/1001",
        "#1001",
        "buyer@business-86b318398f.test",
        42_50,
    );

    let lookup = order_status_lookup(
        conn,
        CLIENT,
        &OperatorScope::User("user_scoped".to_string()),
        "#1001",
    )
    .expect("lookup");

    assert!(lookup
        .orders
        .iter()
        .any(|order| order.source == "inventory"));
    let shopify = lookup
        .orders
        .iter()
        .find(|order| order.source == "shopify")
        .expect("shopify order");
    assert_eq!(shopify.total_amount_cents, Some(42_50));
    let rendered = render_orders(&lookup).expect("rendered");
    assert!(rendered.contains("[inventory]"));
    assert!(rendered.contains("[shopify]"));
    assert!(rendered.contains("order_total=$42.50"));
    let inventory_line = rendered
        .lines()
        .find(|line| line.contains("[inventory]"))
        .expect("inventory line");
    assert!(
        !inventory_line.contains('$'),
        "inventory rows should stay dollar-free: {inventory_line}"
    );
}

#[test]
fn shopify_order_grounding_renders_money_only_when_visible() {
    {
        let _env = EnvGuard::set("BOS_SHOPIFY_SALES_VISIBILITY_POLICY", "shared");
        let mut persistence = crate::persistence::Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        seed_shopify_order(
            conn,
            "gid://shopify/Order/1001",
            "#1001",
            "buyer@business-86b318398f.test",
            42_50,
        );
        seed_shopify_customer(
            conn,
            "gid://shopify/Customer/1",
            "buyer@business-86b318398f.test",
            99_00,
        );

        let lookup = shopify_order_grounding(
            conn,
            CLIENT,
            &OperatorScope::User("user_scoped".to_string()),
            None,
            Some("buyer@business-86b318398f.test"),
        )
        .expect("shopify lookup");

        assert_eq!(lookup.orders[0].total_cents, Some(42_50));
        assert_eq!(lookup.customers[0].total_spent_cents, Some(99_00));
        let rendered = render_shopify_order_grounding(&lookup).expect("rendered");
        assert!(rendered.contains("order_total=USD $42.50"));
        assert!(rendered.contains("total_spent=$99.00"));
    }

    {
        let _env = EnvGuard::set("BOS_SHOPIFY_SALES_VISIBILITY_POLICY", "authorizer_only");
        let mut persistence = crate::persistence::Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        seed_shopify_order(
            conn,
            "gid://shopify/Order/1001",
            "#1001",
            "buyer@business-86b318398f.test",
            42_50,
        );
        seed_shopify_customer(
            conn,
            "gid://shopify/Customer/1",
            "buyer@business-86b318398f.test",
            99_00,
        );

        let lookup = shopify_order_grounding(
            conn,
            CLIENT,
            &OperatorScope::User("user_scoped".to_string()),
            None,
            Some("buyer@business-86b318398f.test"),
        )
        .expect("shopify lookup");

        assert_eq!(lookup.orders[0].total_cents, None);
        assert_eq!(lookup.customers[0].total_spent_cents, None);
        let rendered = render_shopify_order_grounding(&lookup).expect("rendered");
        assert!(rendered.contains("[shopify]"));
        assert!(rendered.contains("Customer:"));
        assert!(!rendered.contains("order_total="));
        assert!(!rendered.contains("total_spent="));
        assert!(!rendered.contains('$'));
    }
}

#[test]
fn crm_contact_lookup_redacts_deal_amounts_for_user_scope() {
    let mut persistence = crate::persistence::Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    seed_crm_contact(conn, "c1", "buyer@business-86b318398f.test", "Acme Co");
    seed_crm_deal(
        conn,
        "d1",
        "buyer@business-86b318398f.test",
        "Acme Co",
        12_345,
    );

    let lookup = crm_contact_lookup(
        conn,
        CLIENT,
        &OperatorScope::User("user_scoped".to_string()),
        Some("buyer@business-86b318398f.test"),
        None,
    )
    .expect("crm lookup");

    assert_eq!(lookup.contacts.len(), 1);
    assert_eq!(lookup.deals.len(), 1);
    assert_eq!(lookup.deals[0].amount_cents, None);
    assert_eq!(lookup.deals[0].currency, None);
    let rendered = render_crm_contact(&lookup).expect("rendered");
    assert!(rendered.contains("redacted"));
    assert!(!rendered.contains("123.45"));
}

fn seed_customer(
    conn: &mut Connection,
    id: &str,
    display_name: &str,
    company_name: Option<&str>,
    email: Option<&str>,
) {
    crate::slices::accounting::store::upsert_customer_snapshots(
        conn,
        CLIENT,
        &[CustomerRecord {
            customer_id: id.to_string(),
            display_name: display_name.to_string(),
            company_name: company_name.map(str::to_string),
            email: email.map(str::to_string),
            phone: None,
            active: true,
            tier_raw: None,
            tier_source: TierSource::NotProvided,
            updated_at: Some("2026-06-01T00:00:00Z".to_string()),
        }],
        1_000,
    )
    .expect("customer");
}

fn seed_order(
    conn: &mut Connection,
    id: &str,
    order_number: &str,
    customer_name: Option<&str>,
    customer_email: Option<&str>,
) {
    crate::slices::inventory::store::upsert_order_snapshots(
        conn,
        CLIENT,
        &[SfOrderCardRecord {
            order_id: id.to_string(),
            order_number: order_number.to_string(),
            external_order_id: None,
            platform: None,
            board_status: "PACKED".to_string(),
            raw_status: None,
            customer_name: customer_name.map(str::to_string),
            customer_email: customer_email.map(str::to_string),
            total_amount_cents: 1_200,
            currency: Some("USD".to_string()),
            order_date: Some("2026-06-01".to_string()),
            processed_at: None,
            item_count: 1,
            unit_count: 1,
            mapped_line_count: 1,
            line_material_ids: vec!["m1".to_string()],
            line_identity_complete: true,
            carrier: Some("UPS".to_string()),
            tracking_number: Some("1ZTEST".to_string()),
            shipment_refs: None,
            shipment_id: None,
            ship_date: Some("2026-06-02".to_string()),
            photo_count: 0,
            pack_station_container_id: None,
            needs_mapping: false,
            blocked: false,
            deducted: true,
            deduction_failed: false,
            label_needed: false,
            packed_missing_photo: false,
            exception: false,
            depletion_total: 0,
            depletion_applied: 0,
            depletion_failed: 0,
            depletion_reversed: 0,
            blocked_reasons_json: "[]".to_string(),
        }],
        1_000,
    )
    .expect("order");
}

fn seed_shopify_order(
    conn: &mut Connection,
    id: &str,
    order_number: &str,
    customer_email: &str,
    total_cents: i64,
) {
    crate::slices::shopify_sales::store::upsert_order_snapshots(
        conn,
        CLIENT,
        &[ShopifyOrderRecord {
            order_id: id.to_string(),
            order_number: order_number.to_string(),
            customer_email: Some(customer_email.to_string()),
            customer_name: Some("Acme Buyer".to_string()),
            total: ShopifyMoney {
                cents: total_cents,
                currency: Some("USD".to_string()),
            },
            financial_status: Some("paid".to_string()),
            fulfillment_status: Some("fulfilled".to_string()),
            tracking_number: Some("1ZSHOPIFY".to_string()),
            tracking_carrier: Some("UPS".to_string()),
            tracking_url: Some("https://example.test/track".to_string()),
            line_items: vec![ShopifyLineItemRecord {
                title: "Blue product".to_string(),
                sku: Some("MUG-BLUE".to_string()),
                quantity: 2,
            }],
            created_at: Some("2026-06-01T00:00:00Z".to_string()),
            updated_at: Some("2026-06-01T00:00:00Z".to_string()),
        }],
        1_000,
    )
    .expect("shopify order");
}

fn seed_shopify_customer(
    conn: &mut Connection,
    id: &str,
    customer_email: &str,
    total_spent_cents: i64,
) {
    crate::slices::shopify_sales::store::upsert_customer_snapshots(
        conn,
        CLIENT,
        &[ShopifyCustomerRecord {
            customer_id: id.to_string(),
            email: Some(customer_email.to_string()),
            name: Some("Acme Buyer".to_string()),
            phone: None,
            total_spent: ShopifyMoney {
                cents: total_spent_cents,
                currency: Some("USD".to_string()),
            },
            orders_count: 3,
            tags: vec!["vip".to_string()],
            tier: Some("Gold".to_string()),
            updated_at: Some("2026-06-01T00:00:00Z".to_string()),
        }],
        1_000,
    )
    .expect("shopify customer");
}

fn seed_crm_contact(conn: &mut Connection, id: &str, email: &str, company: &str) {
    crate::slices::crm_cache::store::upsert_contact_snapshots(
        conn,
        CLIENT,
        &[CrmContactRecord {
            provider_contact_id: id.to_string(),
            email: Some(email.to_string()),
            name: Some("Acme Buyer".to_string()),
            company: Some(company.to_string()),
            phone: None,
            lifecycle_stage: Some("customer".to_string()),
            owner: Some("Jordan".to_string()),
            last_activity_at: Some("2026-06-01T00:00:00Z".to_string()),
        }],
        1_000,
    )
    .expect("crm contact");
}

fn seed_crm_deal(conn: &mut Connection, id: &str, email: &str, company: &str, amount_cents: i64) {
    crate::slices::crm_cache::store::upsert_deal_snapshots(
        conn,
        CLIENT,
        &[CrmDealRecord {
            provider_deal_id: id.to_string(),
            name: Some("renovation project".to_string()),
            stage: Some("open".to_string()),
            amount_cents: Some(amount_cents),
            currency: Some("USD".to_string()),
            pipeline: Some("sales".to_string()),
            close_date: Some("2026-07-01".to_string()),
            associated_contact_ids: vec!["c1".to_string()],
            associated_contact_email: Some(email.to_string()),
            associated_contact_company: Some(company.to_string()),
        }],
        1_000,
    )
    .expect("crm deal");
}

fn invoice_record(index: usize, customer_name: &str, balance_cents: i64) -> InvoiceRecord {
    InvoiceRecord {
        invoice_id: format!("inv_{index}"),
        doc_number: Some(format!("INV-{index}")),
        customer_id: Some("c1".to_string()),
        customer_name: Some(customer_name.to_string()),
        txn_date: Some("2026-01-01".to_string()),
        due_date: Some("2026-01-15".to_string()),
        total_amt_cents: balance_cents,
        balance_cents,
        voided: false,
        updated_at: format!("2026-01-{day:02}T00:00:00Z", day = index + 1),
    }
}
