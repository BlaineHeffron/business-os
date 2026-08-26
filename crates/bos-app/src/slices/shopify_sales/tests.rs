use bos_integrations::shopify_sales_read::{
    FixtureShopifySalesReadClient, ShopifyCustomerRecord, ShopifyLineItemRecord, ShopifyMoney,
    ShopifyOrderRecord,
};

use super::{service, store, worker};
use crate::http::test_support::{test_state, EnvGuard};
use crate::http::OperatorScope;

fn money(cents: i64) -> ShopifyMoney {
    ShopifyMoney {
        cents,
        currency: Some("USD".to_string()),
    }
}

fn order(id: &str, email: &str, total_cents: i64) -> ShopifyOrderRecord {
    ShopifyOrderRecord {
        order_id: id.to_string(),
        order_number: format!("#{id}"),
        customer_email: Some(email.to_string()),
        customer_name: Some("Ada Buyer".to_string()),
        total: money(total_cents),
        financial_status: Some("PAID".to_string()),
        fulfillment_status: Some("FULFILLED".to_string()),
        tracking_number: Some("1Z".to_string()),
        tracking_carrier: Some("UPS".to_string()),
        tracking_url: Some("https://track.test/1Z".to_string()),
        line_items: vec![ShopifyLineItemRecord {
            title: "Widget".to_string(),
            sku: Some("W1".to_string()),
            quantity: 3,
        }],
        created_at: Some("2026-06-01T00:00:00Z".to_string()),
        updated_at: Some("2026-06-02T00:00:00Z".to_string()),
    }
}

fn customer(id: &str, email: &str, total_cents: i64) -> ShopifyCustomerRecord {
    ShopifyCustomerRecord {
        customer_id: id.to_string(),
        email: Some(email.to_string()),
        name: Some("Ada Buyer".to_string()),
        phone: Some("+15551234567".to_string()),
        total_spent: money(total_cents),
        orders_count: 4,
        tags: vec!["tier:Gold".to_string(), "vip".to_string()],
        tier: Some("Gold".to_string()),
        updated_at: Some("2026-06-02T00:00:00Z".to_string()),
    }
}

#[test]
fn connector_status_accepts_static_access_token() {
    let _env = EnvGuard::set_many(&[
        ("BOS_SHOPIFY_SHOP_DOMAIN", "demo.myshopify.com"),
        ("BOS_SHOPIFY_ACCESS_TOKEN", "shpat_static"),
        ("BOS_SHOPIFY_CLIENT_ID", ""),
        ("BOS_SHOPIFY_CLIENT_SECRET", ""),
    ]);

    let status = service::connector_status(false);

    assert!(status.configured);
    assert_eq!(status.shop_domain.as_deref(), Some("demo.myshopify.com"));
    assert_eq!(status.blocked_reason, None);
}

#[test]
fn connector_status_accepts_client_credentials_without_fetching_token() {
    let _env = EnvGuard::set_many(&[
        ("BOS_SHOPIFY_SHOP_DOMAIN", "demo.myshopify.com"),
        ("BOS_SHOPIFY_ACCESS_TOKEN", ""),
        ("BOS_SHOPIFY_CLIENT_ID", "client-id"),
        ("BOS_SHOPIFY_CLIENT_SECRET", "client-secret"),
    ]);

    let status = service::connector_status(false);

    assert!(status.configured);
    assert_eq!(status.shop_domain.as_deref(), Some("demo.myshopify.com"));
    assert_eq!(status.blocked_reason, None);
    assert!(service::connector_config_present_from_env());
}

#[test]
fn upserts_are_receipt_quiet_when_snapshot_unchanged() {
    let state = test_state();
    let now = 1_000;
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let records = vec![order("1001", "ada@example.com", 4250)];

    let first =
        store::upsert_order_snapshots(conn, &state.client_id, &records, now).expect("first upsert");
    let second = store::upsert_order_snapshots(conn, &state.client_id, &records, now + 1)
        .expect("second upsert");

    assert_eq!(first.written, 1);
    assert_eq!(second.written, 0);
    assert_eq!(second.unchanged, 1);
}

#[test]
fn store_money_fields_redact_when_visibility_flag_is_false() {
    let state = test_state();
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    store::upsert_order_snapshots(
        conn,
        &state.client_id,
        &[order("1001", "ada@example.com", 4250)],
        1_000,
    )
    .expect("order upsert");
    store::upsert_customer_snapshots(
        conn,
        &state.client_id,
        &[customer("c1", "ada@example.com", 99_00)],
        1_000,
    )
    .expect("customer upsert");

    let named = OperatorScope::User("casey".to_string());
    let orders =
        store::orders_by_customer(conn, &state.client_id, &named, false, "ADA@example.com", 10)
            .expect("orders");
    let customers =
        store::customers_by_email(conn, &state.client_id, &named, false, "ada@example.com", 10)
            .expect("customers");

    assert_eq!(orders.len(), 1);
    assert_eq!(orders[0].total_cents, None);
    assert_eq!(orders[0].financial_status.as_deref(), Some("PAID"));
    assert_eq!(orders[0].tracking_number.as_deref(), Some("1Z"));
    assert_eq!(customers[0].total_spent_cents, None);
    assert_eq!(customers[0].orders_count, 4);

    let all = store::orders_by_customer(
        conn,
        &state.client_id,
        &OperatorScope::All,
        true,
        "ada@example.com",
        10,
    )
    .expect("all orders");
    assert_eq!(all[0].total_cents, Some(4250));
}

#[test]
fn default_visibility_policy_redacts_money_for_named_users() {
    let _env = EnvGuard::unset("BOS_SHOPIFY_SALES_VISIBILITY_POLICY");
    let state = test_state();
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    store::upsert_order_snapshots(
        conn,
        &state.client_id,
        &[order("1001", "ada@example.com", 4250)],
        1_000,
    )
    .expect("order upsert");
    store::upsert_customer_snapshots(
        conn,
        &state.client_id,
        &[customer("c1", "ada@example.com", 99_00)],
        1_000,
    )
    .expect("customer upsert");

    let named = OperatorScope::User("casey".to_string());
    let orders =
        service::orders_for_customer(conn, &state.client_id, &named, "ADA@example.com", 10)
            .expect("orders");
    let customers =
        service::customers_for_email(conn, &state.client_id, &named, "ada@example.com", 10)
            .expect("customers");

    assert_eq!(orders[0].total_cents, None);
    assert_eq!(customers[0].total_spent_cents, None);
}

#[test]
fn shared_visibility_policy_shares_money_for_named_users() {
    let _env = EnvGuard::set("BOS_SHOPIFY_SALES_VISIBILITY_POLICY", "shared");
    let state = test_state();
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    store::upsert_order_snapshots(
        conn,
        &state.client_id,
        &[order("1001", "ada@example.com", 4250)],
        1_000,
    )
    .expect("order upsert");
    store::upsert_customer_snapshots(
        conn,
        &state.client_id,
        &[customer("c1", "ada@example.com", 99_00)],
        1_000,
    )
    .expect("customer upsert");

    let named = OperatorScope::User("casey".to_string());
    let orders =
        service::orders_for_customer(conn, &state.client_id, &named, "ADA@example.com", 10)
            .expect("orders");
    let customers =
        service::customers_for_email(conn, &state.client_id, &named, "ada@example.com", 10)
            .expect("customers");

    assert_eq!(orders[0].total_cents, Some(4250));
    assert_eq!(customers[0].total_spent_cents, Some(99_00));
}

#[test]
fn shop_domain_change_resets_snapshot_cache_before_sync() {
    let state = test_state();
    let first_client = FixtureShopifySalesReadClient {
        orders: vec![order("1001", "ada@example.com", 4250)],
        customers: vec![customer("c1", "ada@example.com", 99_00)],
    };
    let second_client = FixtureShopifySalesReadClient {
        orders: vec![order("2001", "ben@example.com", 5000)],
        customers: vec![customer("c2", "ben@example.com", 50_00)],
    };

    let first = worker::run_sync_cycle(&state, &first_client, "first.myshopify.com", 250, 1_000)
        .expect("first sync");
    let second = worker::run_sync_cycle(&state, &second_client, "second.myshopify.com", 250, 2_000)
        .expect("second sync");

    assert_eq!(first.written, 2);
    assert_eq!(second.written, 2);
    let persistence = state.persistence.lock();
    let conn = persistence.connection_ref();
    let recent = store::list_recent_orders(conn, &state.client_id, &OperatorScope::All, true, 10)
        .expect("recent");
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].order_id, "2001");
}

#[test]
fn sync_state_timestamp_advances_when_counts_are_unchanged() {
    let state = test_state();
    let client = FixtureShopifySalesReadClient {
        orders: vec![order("1001", "ada@example.com", 4250)],
        customers: vec![customer("c1", "ada@example.com", 99_00)],
    };

    worker::run_sync_cycle(&state, &client, "first.myshopify.com", 250, 1_000).expect("first sync");
    worker::run_sync_cycle(&state, &client, "first.myshopify.com", 250, 2_000)
        .expect("second sync");

    let persistence = state.persistence.lock();
    let conn = persistence.connection_ref();
    let sync_state = store::get_sync_state(conn, &state.client_id).expect("sync state");
    assert_eq!(sync_state.last_advanced_at_ms, Some(2_000));
}

#[test]
fn paged_backfill_does_not_mark_complete_after_first_page() {
    let state = test_state();
    let client = FixtureShopifySalesReadClient {
        orders: vec![
            order("1001", "ada@example.com", 4250),
            order("1002", "ben@example.com", 5000),
        ],
        customers: vec![
            customer("c1", "ada@example.com", 99_00),
            customer("c2", "ben@example.com", 50_00),
        ],
    };

    worker::run_sync_cycle(&state, &client, "first.myshopify.com", 1, 1_000).expect("first page");
    {
        let persistence = state.persistence.lock();
        let conn = persistence.connection_ref();
        let sync_state = store::get_sync_state(conn, &state.client_id).expect("sync state");
        assert!(!sync_state.backfill_complete);
        assert!(!sync_state.order_backfill_complete);
        assert!(!sync_state.customer_backfill_complete);
    }

    worker::run_sync_cycle(&state, &client, "first.myshopify.com", 1, 2_000).expect("second page");
    let persistence = state.persistence.lock();
    let conn = persistence.connection_ref();
    let sync_state = store::get_sync_state(conn, &state.client_id).expect("sync state");
    let recent = store::list_recent_orders(conn, &state.client_id, &OperatorScope::All, true, 10)
        .expect("recent");
    assert!(sync_state.backfill_complete);
    assert_eq!(recent.len(), 2);
}

#[test]
fn visibility_policy_authorizer_only_matches_admin_only_for_env_credentials() {
    assert!(service::financial_visible(
        &OperatorScope::All,
        service::ShopifySalesVisibilityPolicy::AuthorizerOnly
    ));
    assert!(!service::financial_visible(
        &OperatorScope::User("casey".to_string()),
        service::ShopifySalesVisibilityPolicy::AuthorizerOnly
    ));
}
