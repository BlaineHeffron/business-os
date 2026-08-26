use bos_integrations::stockforge_read::{
    FixtureStockforgeReadClient, SfAlertRecord, SfMaterialRecord, SfOrderCardRecord,
    SfPurchaseOrderRecord, SfReorderSuggestionRecord, StockforgeError,
};

use super::service::{self, StockforgeConnectorConfig};
use super::store;
use super::worker::{self, CycleSummary};
use crate::http::test_support::{test_state, EnvGuard};
use crate::http::AppState;

const CLIENT: &str = "test-client";

fn config() -> StockforgeConnectorConfig {
    StockforgeConnectorConfig {
        base_url: "https://sf.example.test".to_string(),
        api_key: "sfk_live_test".to_string(),
    }
}

fn material(id: &str, name: &str, quantity: f64) -> SfMaterialRecord {
    SfMaterialRecord {
        material_id: id.to_string(),
        name: name.to_string(),
        sku: Some(format!("SKU-{id}")),
        category: Some("LIQUID".to_string()),
        current_quantity: quantity,
        reserved_qty: None,
        incoming_qty: None,
        unit: Some("gal".to_string()),
        warning_threshold: Some(20.0),
        critical_threshold: Some(5.0),
        threshold_type: Some("ABSOLUTE".to_string()),
        unit_cost_cents: 5_000,
        lead_time_days: Some(14),
        vendor_name: Some("Champion".to_string()),
        is_active: true,
        is_purchasable: Some(true),
        replenishment_policy: Some("PURCHASE".to_string()),
        sale_depletion_policy: Some("STOCK".to_string()),
        updated_at: Some("2026-06-09T00:00:00Z".to_string()),
    }
}

fn material_row(id: &str, monitored: bool) -> store::MaterialSnapshotRow {
    store::MaterialSnapshotRow {
        material_id: id.to_string(),
        name: format!("Material {id}"),
        sku: Some(format!("SKU-{id}")),
        category: Some("DISCRETE".to_string()),
        quantity: 0.0,
        reserved_qty: None,
        incoming_qty: None,
        unit: Some("ea".to_string()),
        warning_threshold: Some(20.0),
        critical_threshold: Some(5.0),
        threshold_type: Some("ABSOLUTE".to_string()),
        unit_cost_cents: 100,
        lead_time_days: None,
        vendor_name: None,
        is_active: true,
        is_purchasable: Some(monitored),
        replenishment_policy: Some(if monitored { "PURCHASE" } else { "NONE" }.to_string()),
        sale_depletion_policy: Some(if monitored { "STOCK" } else { "NONE" }.to_string()),
    }
}

fn order_snapshot(material_ids: &[&str], complete: bool) -> store::OrderSnapshotRow {
    store::OrderSnapshotRow {
        order_id: "o1".to_string(),
        order_number: "#o1".to_string(),
        external_order_id: None,
        platform: Some("shopify".to_string()),
        board_status: "DELIVERED".to_string(),
        customer_name: None,
        customer_email: None,
        total_amount_cents: 1_000,
        order_date: Some("2026-06-01".to_string()),
        processed_at: None,
        item_count: material_ids.len() as i64,
        unit_count: material_ids.len() as i64,
        mapped_line_count: material_ids.len() as i64,
        line_material_ids: material_ids.iter().map(|id| (*id).to_string()).collect(),
        line_identity_complete: complete,
        carrier: None,
        tracking_number: None,
        shipment_refs: None,
        shipment_id: None,
        ship_date: None,
        photo_count: 0,
        pack_station_container_id: None,
        needs_mapping: false,
        blocked: false,
        deducted: true,
        deduction_failed: false,
        exception: false,
        depletion_total: 0,
        depletion_applied: 0,
        depletion_failed: 0,
        depletion_reversed: 0,
        blocked_reasons_json: "[]".to_string(),
    }
}

fn po_snapshot(material_ids: &[&str], complete: bool) -> store::PoSnapshotRow {
    store::PoSnapshotRow {
        po_id: "po1".to_string(),
        vendor_name: None,
        status: "SENT".to_string(),
        total_estimated_cost_cents: 10_000,
        freight_mode: None,
        line_count: material_ids.len() as i64,
        line_material_ids: material_ids.iter().map(|id| (*id).to_string()).collect(),
        line_identity_complete: complete,
        created_at: None,
        sent_at: None,
    }
}

fn alert(id: &str, material_id: &str, severity: &str) -> SfAlertRecord {
    SfAlertRecord {
        alert_id: id.to_string(),
        material_id: Some(material_id.to_string()),
        material_name: Some("example Blue".to_string()),
        material_sku: Some(format!("SKU-{material_id}")),
        severity: severity.to_string(),
        status: "ACTIVE".to_string(),
        current_quantity: Some(3.0),
        threshold_value: Some(5.0),
        percentage_remaining: Some(12.0),
        message: Some("low".to_string()),
        created_at: Some("2026-06-09T00:00:00Z".to_string()),
    }
}

fn suggestion(id: &str, status: &str) -> SfReorderSuggestionRecord {
    SfReorderSuggestionRecord {
        suggestion_id: id.to_string(),
        material_id: Some("m1".to_string()),
        material_name: Some("example Blue".to_string()),
        material_sku: Some("SKU-m1".to_string()),
        vendor_name: Some("Champion".to_string()),
        urgency: "HIGH".to_string(),
        status: status.to_string(),
        current_quantity: Some(3.0),
        suggested_quantity: Some(50.0),
        unit: Some("gal".to_string()),
        estimated_cost_cents: 250_000,
        days_until_stockout: Some(6.5),
        lead_time_days: Some(14),
        reasoning: Some("burn rate".to_string()),
        created_at: None,
    }
}

fn order(id: &str, status: &str, order_date: &str) -> SfOrderCardRecord {
    SfOrderCardRecord {
        order_id: id.to_string(),
        order_number: format!("#{id}"),
        external_order_id: Some(format!("shopify-{id}")),
        platform: Some("shopify".to_string()),
        board_status: status.to_string(),
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
        carrier: None,
        tracking_number: None,
        shipment_refs: None,
        shipment_id: None,
        ship_date: None,
        photo_count: 0,
        pack_station_container_id: None,
        needs_mapping: false,
        blocked: false,
        deducted: false,
        deduction_failed: false,
        label_needed: false,
        packed_missing_photo: false,
        exception: false,
        depletion_total: 0,
        depletion_applied: 0,
        depletion_failed: 0,
        depletion_reversed: 0,
        blocked_reasons_json: "[]".to_string(),
    }
}

fn purchase_order(id: &str, status: &str) -> SfPurchaseOrderRecord {
    SfPurchaseOrderRecord {
        po_id: id.to_string(),
        vendor_name: Some("INMARK".to_string()),
        status: status.to_string(),
        total_estimated_cost_cents: 100_000,
        freight_mode: Some("LTL".to_string()),
        line_count: 3,
        line_material_ids: vec!["m1".to_string()],
        line_identity_complete: true,
        created_at: Some("2026-06-01T00:00:00Z".to_string()),
        sent_at: None,
        received_at: None,
    }
}

fn receipt_count(state: &AppState) -> i64 {
    let persistence = state.persistence.lock();
    persistence
        .connection_ref()
        .query_row("SELECT COUNT(*) FROM receipts", [], |row| row.get(0))
        .expect("count")
}

#[test]
fn connector_status_uses_explicit_stockforge_app_url_for_board_link() {
    let _env = EnvGuard::set_many(&[
        (
            crate::env_registry::BOS_STOCKFORGE_BASE_URL.name,
            "https://api.stockforge.ai/",
        ),
        (
            crate::env_registry::BOS_STOCKFORGE_APP_URL.name,
            "https://app.stockforge.ai/",
        ),
        (
            crate::env_registry::BOS_STOCKFORGE_API_KEY.name,
            "sfk_live_test",
        ),
    ]);

    let status = service::connector_status(true);

    assert_eq!(
        status.base_url.as_deref(),
        Some("https://api.stockforge.ai/")
    );
    assert_eq!(
        status.order_board_url.as_deref(),
        Some("https://app.stockforge.ai/orders/board")
    );
}

#[test]
fn connector_status_maps_known_stockforge_api_host_to_app_board_link() {
    let _env = EnvGuard::set_many(&[
        (
            crate::env_registry::BOS_STOCKFORGE_BASE_URL.name,
            "https://api.stockforge.ai",
        ),
        (crate::env_registry::BOS_STOCKFORGE_APP_URL.name, ""),
        (
            crate::env_registry::BOS_STOCKFORGE_API_KEY.name,
            "sfk_live_test",
        ),
    ]);

    let status = service::connector_status(false);

    assert_eq!(
        status.order_board_url.as_deref(),
        Some("https://app.stockforge.ai/orders/board")
    );
}

#[test]
fn connector_status_does_not_rewrite_stockforge_lookalike_hosts() {
    let _env = EnvGuard::set_many(&[
        (
            crate::env_registry::BOS_STOCKFORGE_BASE_URL.name,
            "https://api.stockforge.ai.example.test",
        ),
        (crate::env_registry::BOS_STOCKFORGE_APP_URL.name, ""),
        (
            crate::env_registry::BOS_STOCKFORGE_API_KEY.name,
            "sfk_live_test",
        ),
    ]);

    let status = service::connector_status(false);

    assert_eq!(
        status.order_board_url.as_deref(),
        Some("https://api.stockforge.ai.example.test/orders/board")
    );
}

#[test]
fn connector_status_preserves_stockforge_api_path_when_mapping_app_link() {
    let _env = EnvGuard::set_many(&[
        (
            crate::env_registry::BOS_STOCKFORGE_BASE_URL.name,
            "https://api.stockforge.ai/client-a",
        ),
        (crate::env_registry::BOS_STOCKFORGE_APP_URL.name, ""),
        (
            crate::env_registry::BOS_STOCKFORGE_API_KEY.name,
            "sfk_live_test",
        ),
    ]);

    let status = service::connector_status(false);

    assert_eq!(
        status.order_board_url.as_deref(),
        Some("https://app.stockforge.ai/client-a/orders/board")
    );
}

fn run_cycle(
    state: &AppState,
    client: &FixtureStockforgeReadClient,
    budget: u32,
    now_ms: u64,
) -> CycleSummary {
    worker::run_sync_cycle(state, client, &config(), budget, now_ms).expect("cycle")
}

#[test]
fn backfill_walks_material_pages_across_budgeted_cycles() {
    let state = test_state();
    // 250 materials = 3 pages at take 100; the other entities are small.
    let fixture = FixtureStockforgeReadClient {
        materials: (0..250)
            .map(|n| material(&format!("m{n:03}"), &format!("Mat {n:03}"), 50.0))
            .collect(),
        ..Default::default()
    };

    // Budget 1: materials page 1 only. Walk parks at skip 100.
    let summary = run_cycle(&state, &fixture, 1, 1_000);
    assert_eq!(summary.requests_used, 1);
    {
        let persistence = state.persistence.lock();
        let conn = persistence.connection_ref();
        let cursor = store::get_cursor(conn, CLIENT, store::ENTITY_MATERIAL).expect("cursor");
        assert_eq!(cursor.next_skip, 100);
        assert!(!cursor.backfill_complete);
        let (materials, _) = store::snapshot_counts(conn, CLIENT).expect("counts");
        assert_eq!(materials, 100);
    }

    // Big budget: the walk completes and the remaining entities sync.
    let summary = run_cycle(&state, &fixture, 20, 2_000);
    // pages at skip 100, 200 (short page closes the walk) + alerts + reorders
    // + orders + POs = 6 requests.
    assert_eq!(summary.requests_used, 6);
    {
        let persistence = state.persistence.lock();
        let conn = persistence.connection_ref();
        let cursor = store::get_cursor(conn, CLIENT, store::ENTITY_MATERIAL).expect("cursor");
        assert!(cursor.backfill_complete);
        assert_eq!(cursor.next_skip, 0, "walk reset for the next cycle");
        let (materials, _) = store::snapshot_counts(conn, CLIENT).expect("counts");
        assert_eq!(materials, 250);
    }
}

#[test]
fn steady_state_cycles_write_zero_receipts_and_prune_stays_quiet() {
    let state = test_state();
    let fixture = FixtureStockforgeReadClient {
        materials: vec![material("m1", "example Blue", 3.0)],
        alerts: vec![alert("a1", "m1", "CRITICAL")],
        suggestions: vec![suggestion("s1", "PENDING")],
        order_cards: vec![order("o1", "NEW", "2026-06-08")],
        purchase_orders: vec![purchase_order("p1", "SENT")],
        ..Default::default()
    };
    run_cycle(&state, &fixture, 20, 1_000);
    let after_first = receipt_count(&state);

    // Identical data: full sets re-fetched, nothing changed, nothing pruned —
    // the load-bearing assertion is ZERO new receipts.
    let summary = run_cycle(&state, &fixture, 20, 2_000);
    assert_eq!(summary.written, 0);
    assert_eq!(summary.pruned, 0);
    assert_eq!(
        receipt_count(&state),
        after_first,
        "quiet cycle wrote receipts"
    );

    // The alert resolves in Stockforge (vanishes from the ACTIVE set) and the
    // order leaves the window: both prune, each as one receipt; the material
    // quantity change is one snapshot receipt + one... cursor stays put for
    // full-set entities, so: 1 material upsert + 2 prunes = 3 receipts.
    let mut changed = fixture.clone();
    changed.alerts.clear();
    changed.order_cards.clear();
    changed.materials[0].current_quantity = 2.0;
    let summary = run_cycle(&state, &changed, 20, 3_000);
    assert_eq!(summary.written, 1);
    assert_eq!(summary.pruned, 2);
    {
        let persistence = state.persistence.lock();
        let conn = persistence.connection_ref();
        assert!(store::list_alerts(conn, CLIENT).expect("alerts").is_empty());
        assert!(store::list_orders(conn, CLIENT).expect("orders").is_empty());
    }
}

#[test]
fn stock_view_classifies_against_thresholds_and_alerts() {
    let healthy = store::MaterialSnapshotRow {
        material_id: "m1".to_string(),
        name: "Healthy".to_string(),
        sku: None,
        category: None,
        quantity: 100.0,
        reserved_qty: None,
        incoming_qty: None,
        unit: None,
        warning_threshold: Some(20.0),
        critical_threshold: Some(5.0),
        threshold_type: Some("ABSOLUTE".to_string()),
        unit_cost_cents: 1_000,
        lead_time_days: None,
        vendor_name: None,
        is_active: true,
        is_purchasable: Some(true),
        replenishment_policy: Some("PURCHASE".to_string()),
        sale_depletion_policy: Some("STOCK".to_string()),
    };
    let mut warning = healthy.clone();
    warning.material_id = "m2".to_string();
    warning.name = "Warning".to_string();
    warning.quantity = 15.0;
    let mut critical = healthy.clone();
    critical.material_id = "m3".to_string();
    critical.name = "Critical".to_string();
    critical.quantity = 4.0;
    let mut out = healthy.clone();
    out.material_id = "m4".to_string();
    out.name = "Out".to_string();
    out.quantity = 0.0;
    // PERCENTAGE thresholds can't classify locally — the alert decides.
    let mut percent = healthy.clone();
    percent.material_id = "m5".to_string();
    percent.name = "Percent".to_string();
    percent.quantity = 40.0;
    percent.threshold_type = Some("PERCENTAGE".to_string());
    let mut inactive = healthy.clone();
    inactive.material_id = "m6".to_string();
    inactive.is_active = false;
    let mut built_to_order = healthy.clone();
    built_to_order.material_id = "m7".to_string();
    built_to_order.name = "Built to order".to_string();
    built_to_order.quantity = 0.0;
    built_to_order.is_purchasable = Some(false);
    built_to_order.replenishment_policy = Some("PRODUCTION".to_string());
    built_to_order.sale_depletion_policy = Some("COMPONENTS".to_string());
    let mut catalog_kit = healthy.clone();
    catalog_kit.material_id = "m8".to_string();
    catalog_kit.name = "Catalog kit".to_string();
    catalog_kit.quantity = 500.0;
    catalog_kit.unit_cost_cents = 1_000_000;
    catalog_kit.replenishment_policy = Some("NONE".to_string());
    catalog_kit.sale_depletion_policy = Some("COMPONENTS".to_string());

    let alerts = vec![store::AlertSnapshotRow {
        alert_id: "a1".to_string(),
        material_id: Some("m5".to_string()),
        material_name: None,
        material_sku: None,
        severity: "WARNING".to_string(),
        quantity: None,
        percentage_remaining: Some(15.0),
        message: None,
        created_at: None,
    }];
    let (kpis, rows) = service::compute_stock(
        &[
            healthy,
            warning,
            critical,
            out,
            percent,
            inactive,
            built_to_order,
            catalog_kit,
        ],
        &alerts,
    );
    assert_eq!(kpis.active_materials, 7, "inactive material hidden");
    assert_eq!(kpis.monitored_materials, 5);
    assert_eq!(kpis.not_monitored_count, 2);
    assert_eq!(kpis.warning_count, 2, "absolute warning + alert-driven");
    assert_eq!(kpis.critical_count, 1);
    assert_eq!(kpis.out_of_stock_count, 1);
    assert_eq!(
        kpis.stock_value_cents,
        (100.0_f64 + 15.0 + 4.0 + 0.0 + 40.0).round() as i64 * 1_000
    );
    // Problems sort first: out, then critical, then warnings.
    assert_eq!(rows[0].stock_status, "out");
    assert_eq!(rows[1].stock_status, "critical");
    assert_eq!(rows[2].stock_status, "warning");
    let percent_row = rows
        .iter()
        .find(|row| row.material_id == "m5")
        .expect("percent row");
    assert_eq!(percent_row.stock_status, "warning", "alert decided");
    let built_to_order_row = rows
        .iter()
        .find(|row| row.material_id == "m7")
        .expect("built-to-order row");
    assert_eq!(built_to_order_row.stock_status, "not_monitored");
    assert!(!built_to_order_row.is_stocked);
    let catalog_kit_row = rows
        .iter()
        .find(|row| row.material_id == "m8")
        .expect("catalog kit row");
    assert!(!catalog_kit_row.is_stocked);
    assert_eq!(catalog_kit_row.stock_status, "not_monitored");
    assert_eq!(
        kpis.catalog_value_cents,
        kpis.stock_value_cents + 500_000_000,
        "high-value catalog kit must inflate catalog value, not stocked value"
    );
}

// Same 12-cell table as Stockforge STOCKED_POLICY_CASES / docs/stocked-rule.md.
const STOCKED_POLICY_CASES: &[(&str, &str, bool)] = &[
    ("STOCK", "AUTO", true),
    ("STOCK", "PURCHASE", true),
    ("STOCK", "NONE", true),
    ("STOCK", "PRODUCTION", false),
    ("COMPONENTS", "AUTO", false),
    ("COMPONENTS", "PURCHASE", false),
    ("COMPONENTS", "NONE", false),
    ("COMPONENTS", "PRODUCTION", false),
    ("NONE", "AUTO", false),
    ("NONE", "PURCHASE", false),
    ("NONE", "NONE", false),
    ("NONE", "PRODUCTION", false),
];

#[test]
fn stocked_policy_cases_match_shared_table() {
    for (sale, replenishment, expected) in STOCKED_POLICY_CASES {
        let mut row = material_row("m1", true);
        row.sale_depletion_policy = Some((*sale).to_string());
        row.replenishment_policy = Some((*replenishment).to_string());
        let (kpis, rows) = service::compute_stock(std::slice::from_ref(&row), &[]);
        assert_eq!(rows[0].is_stocked, *expected, "{sale}+{replenishment}");
        if *expected {
            assert_eq!(
                kpis.monitored_materials, 1,
                "{sale}+{replenishment} counted"
            );
            assert_eq!(kpis.stock_value_cents, 0);
        } else {
            assert_eq!(
                kpis.monitored_materials, 0,
                "{sale}+{replenishment} not counted"
            );
            assert_eq!(kpis.not_monitored_count, 1);
            assert_eq!(kpis.stock_value_cents, 0);
        }
    }
}

#[test]
fn stock_plus_none_is_stocked_and_valued() {
    let mut row = material_row("m1", true);
    row.quantity = 10.0;
    row.unit_cost_cents = 250;
    row.sale_depletion_policy = Some("STOCK".to_string());
    row.replenishment_policy = Some("NONE".to_string());
    let (kpis, rows) = service::compute_stock(&[row], &[]);
    assert!(rows[0].is_stocked);
    assert_eq!(kpis.monitored_materials, 1);
    assert_eq!(kpis.stock_value_cents, 2_500);
}

#[test]
fn inactive_stock_policies_are_not_stocked() {
    let mut row = material_row("m1", true);
    row.is_active = false;
    let (kpis, rows) = service::compute_stock(&[row], &[]);
    assert!(rows.is_empty());
    assert_eq!(kpis.monitored_materials, 0);
}

#[test]
fn null_policies_are_not_stocked() {
    let mut row = material_row("m1", true);
    row.sale_depletion_policy = None;
    row.replenishment_policy = None;
    let (kpis, rows) = service::compute_stock(&[row], &[]);
    assert!(!rows[0].is_stocked);
    assert_eq!(kpis.monitored_materials, 0);
    assert_eq!(kpis.not_monitored_count, 1);
}

fn alert_snapshot(id: &str, material_id: &str, severity: &str) -> store::AlertSnapshotRow {
    store::AlertSnapshotRow {
        alert_id: id.to_string(),
        material_id: Some(material_id.to_string()),
        material_name: None,
        material_sku: None,
        severity: severity.to_string(),
        quantity: Some(0.0),
        percentage_remaining: Some(0.0),
        message: None,
        created_at: None,
    }
}

#[test]
fn stock_plus_production_alerts_but_is_not_stocked() {
    let mut make_to_stock = material_row("m-prod", true);
    make_to_stock.quantity = 50.0;
    make_to_stock.unit_cost_cents = 1_000;
    make_to_stock.sale_depletion_policy = Some("STOCK".to_string());
    make_to_stock.replenishment_policy = Some("PRODUCTION".to_string());
    let alerts = vec![alert_snapshot("a-prod", "m-prod", "CRITICAL")];
    let (kpis, rows) = service::compute_stock(&[make_to_stock.clone()], &alerts);
    assert!(!rows[0].is_stocked);
    assert_eq!(rows[0].stock_status, "critical");
    assert_eq!(kpis.stock_value_cents, 0);
    assert_eq!(kpis.critical_count, 1);
    assert_eq!(kpis.monitored_materials, 0);
    let surfaced = service::alert_rows(&alerts, &[make_to_stock]);
    assert_eq!(surfaced.len(), 1);
    assert_eq!(surfaced[0].alert_id, "a-prod");
}

#[test]
fn stock_plus_none_critical_alert_surfaces_and_is_stocked() {
    let mut shopify_default = material_row("m-none", true);
    shopify_default.quantity = 50.0;
    shopify_default.sale_depletion_policy = Some("STOCK".to_string());
    shopify_default.replenishment_policy = Some("NONE".to_string());
    let alerts = vec![alert_snapshot("a-none", "m-none", "CRITICAL")];
    let (kpis, rows) = service::compute_stock(&[shopify_default.clone()], &alerts);
    assert!(rows[0].is_stocked);
    assert_eq!(rows[0].stock_status, "critical");
    assert_eq!(kpis.critical_count, 1);
    assert_eq!(kpis.monitored_materials, 1);
    let surfaced = service::alert_rows(&alerts, &[shopify_default]);
    assert_eq!(surfaced.len(), 1);
    assert_eq!(surfaced[0].alert_id, "a-none");
}

#[test]
fn null_sale_depletion_still_surfaces_stockforge_alerts() {
    let mut pending_sync = material_row("m-null", true);
    pending_sync.sale_depletion_policy = None;
    pending_sync.replenishment_policy = None;
    pending_sync.quantity = 2.0;
    let alerts = vec![alert_snapshot("a-null", "m-null", "CRITICAL")];
    let (kpis, rows) = service::compute_stock(&[pending_sync.clone()], &alerts);
    assert!(!rows[0].is_stocked);
    assert_eq!(rows[0].stock_status, "critical");
    assert_eq!(kpis.critical_count, 1);
    let surfaced = service::alert_rows(&alerts, &[pending_sync]);
    assert_eq!(surfaced.len(), 1);
}

#[test]
fn null_sale_depletion_does_not_synthesize_local_stock_alerts() {
    let mut pending_sync = material_row("m-null", true);
    pending_sync.sale_depletion_policy = None;
    pending_sync.replenishment_policy = None;
    pending_sync.quantity = 0.0;
    pending_sync.threshold_type = Some("ABSOLUTE".to_string());
    pending_sync.warning_threshold = Some(10.0);
    pending_sync.critical_threshold = Some(5.0);

    let (kpis, rows) = service::compute_stock(&[pending_sync], &[]);

    assert_eq!(rows[0].stock_status, "not_monitored");
    assert_eq!(kpis.out_of_stock_count, 0);
    assert_eq!(kpis.critical_count, 0);
    assert_eq!(kpis.warning_count, 0);
}

#[test]
fn catalog_and_built_to_order_alerts_stay_suppressed() {
    let mut catalog = material_row("m-cat", false);
    catalog.quantity = 0.0;
    let mut built = material_row("m-bto", true);
    built.sale_depletion_policy = Some("COMPONENTS".to_string());
    built.replenishment_policy = Some("PRODUCTION".to_string());
    built.quantity = 0.0;
    let alerts = vec![
        alert_snapshot("a-cat", "m-cat", "CRITICAL"),
        alert_snapshot("a-bto", "m-bto", "CRITICAL"),
    ];
    let (kpis, rows) = service::compute_stock(&[catalog.clone(), built.clone()], &alerts);
    assert!(rows.iter().all(|row| !row.is_stocked));
    assert!(rows.iter().all(|row| row.stock_status == "not_monitored"));
    assert_eq!(kpis.out_of_stock_count, 0);
    assert_eq!(kpis.critical_count, 0);
    assert!(service::alert_rows(&alerts, &[catalog, built]).is_empty());
}

#[test]
fn policy_fields_change_material_hash_and_rewrite_row() {
    let state = test_state();
    let first = material("m1", "example Blue", 10.0);
    {
        let mut persistence = state.persistence.lock();
        store::upsert_material_snapshots(
            persistence.connection(),
            CLIENT,
            std::slice::from_ref(&first),
            1_000,
        )
        .expect("insert");
    }
    let mut second = first;
    second.sale_depletion_policy = Some("NONE".to_string());
    second.replenishment_policy = Some("NONE".to_string());
    let summary = {
        let mut persistence = state.persistence.lock();
        store::upsert_material_snapshots(persistence.connection(), CLIENT, &[second], 2_000)
            .expect("rewrite")
    };
    assert_eq!(summary.written, 1, "policy-only change must rewrite");
    assert_eq!(summary.unchanged, 0);
    let persistence = state.persistence.lock();
    let rows = store::list_materials(persistence.connection_ref(), CLIENT).expect("list");
    assert_eq!(rows[0].sale_depletion_policy.as_deref(), Some("NONE"));
}

#[test]
fn available_qty_is_on_hand_minus_reserved_and_ignores_incoming() {
    let mut on_hand = material_row("m1", true);
    on_hand.quantity = 10.0;
    on_hand.reserved_qty = Some(4.0);
    on_hand.incoming_qty = Some(50.0);
    let mut over_reserved = on_hand.clone();
    over_reserved.material_id = "m2".to_string();
    over_reserved.quantity = 3.0;
    over_reserved.reserved_qty = Some(8.0);
    over_reserved.incoming_qty = Some(20.0);
    let mut unknown = on_hand.clone();
    unknown.material_id = "m3".to_string();
    unknown.reserved_qty = None;
    unknown.incoming_qty = None;
    let (_kpis, rows) = service::compute_stock(&[on_hand, over_reserved, unknown], &[]);
    let first = rows.iter().find(|row| row.material_id == "m1").expect("m1");
    assert_eq!(first.quantity, 10.0);
    assert_eq!(first.reserved_qty, Some(4.0));
    assert_eq!(first.incoming_qty, Some(50.0));
    assert_eq!(
        first.available_qty,
        Some(6.0),
        "available must not add incoming"
    );
    let second = rows.iter().find(|row| row.material_id == "m2").expect("m2");
    assert_eq!(second.available_qty, Some(0.0), "available floors at 0");
    let third = rows.iter().find(|row| row.material_id == "m3").expect("m3");
    assert_eq!(third.available_qty, None, "unknown reserved stays unknown");
}

#[test]
fn stock_cover_uses_minimum_supplied_prediction_and_keeps_missing_unknown() {
    let mut first = material_row("m1", true);
    first.quantity = 10.0;
    let mut missing = material_row("m2", true);
    missing.quantity = 10.0;
    let (_, mut rows) = service::compute_stock(&[first, missing], &[]);
    let suggestion = |id: &str, material_id: &str, days_until_stockout| store::ReorderSnapshotRow {
        suggestion_id: id.to_string(),
        material_id: Some(material_id.to_string()),
        material_name: None,
        material_sku: None,
        vendor_name: None,
        urgency: "HIGH".to_string(),
        status: "PENDING".to_string(),
        days_until_stockout,
        suggested_quantity: None,
        unit: None,
        estimated_cost_cents: 0,
        lead_time_days: None,
        reasoning: None,
    };
    service::enrich_stock_rows(
        &mut rows,
        &[
            suggestion("s-null", "m1", None),
            suggestion("s-five", "m1", Some(5.0)),
            suggestion("s-zero", "m1", Some(0.0)),
            suggestion("s-missing", "m2", None),
        ],
        &service::StockHistoryEvidence::default(),
    );

    let first = rows.iter().find(|row| row.material_id == "m1").expect("m1");
    let missing = rows.iter().find(|row| row.material_id == "m2").expect("m2");
    assert_eq!(first.days_until_stockout, Some(0.0));
    assert_eq!(missing.days_until_stockout, None);
}

#[test]
fn dead_stock_requires_stocked_on_hand_complete_history_and_no_activity() {
    let mut dead = material_row("dead", true);
    dead.quantity = 10.0;
    let mut demanded = material_row("demanded", true);
    demanded.quantity = 10.0;
    let mut inbound = material_row("inbound", true);
    inbound.quantity = 10.0;
    let mut catalog = material_row("catalog", false);
    catalog.quantity = 10.0;
    let mut built_to_order = material_row("bto", false);
    built_to_order.quantity = 10.0;
    built_to_order.replenishment_policy = Some("PRODUCTION".to_string());
    built_to_order.sale_depletion_policy = Some("COMPONENTS".to_string());
    let mut empty = material_row("empty", true);
    empty.quantity = 0.0;
    let (_, mut rows) = service::compute_stock(
        &[dead, demanded, inbound, catalog, built_to_order, empty],
        &[],
    );
    let history = service::stock_history_evidence(
        &[order_snapshot(&["demanded"], true)],
        &[po_snapshot(&["inbound"], true)],
        true,
        true,
    );

    service::enrich_stock_rows(&mut rows, &[], &history);

    assert!(
        rows.iter()
            .find(|row| row.material_id == "dead")
            .expect("dead")
            .dead_stock
    );
    assert!(
        !rows
            .iter()
            .find(|row| row.material_id == "demanded")
            .expect("demanded")
            .dead_stock
    );
    assert!(
        !rows
            .iter()
            .find(|row| row.material_id == "inbound")
            .expect("inbound")
            .dead_stock
    );
    assert!(
        !rows
            .iter()
            .find(|row| row.material_id == "catalog")
            .expect("catalog")
            .dead_stock
    );
    assert!(
        !rows
            .iter()
            .find(|row| row.material_id == "bto")
            .expect("bto")
            .dead_stock
    );
    assert!(
        !rows
            .iter()
            .find(|row| row.material_id == "empty")
            .expect("empty")
            .dead_stock
    );

    let incomplete_order = service::stock_history_evidence(
        &[order_snapshot(&["demanded"], false)],
        &[po_snapshot(&["inbound"], true)],
        true,
        true,
    );
    service::enrich_stock_rows(&mut rows, &[], &incomplete_order);
    assert!(rows.iter().all(|row| !row.dead_stock));

    let incomplete_po = service::stock_history_evidence(
        &[order_snapshot(&["demanded"], true)],
        &[po_snapshot(&["inbound"], false)],
        true,
        true,
    );
    service::enrich_stock_rows(&mut rows, &[], &incomplete_po);
    assert!(rows.iter().all(|row| !row.dead_stock));

    let missing_board = service::stock_history_evidence(&[], &[], false, true);
    service::enrich_stock_rows(&mut rows, &[], &missing_board);
    assert!(rows.iter().all(|row| !row.dead_stock));
}

#[test]
fn stockout_prediction_suppresses_dead_stock_label() {
    let mut dead = material_row("dead", true);
    dead.quantity = 10.0;
    let (_, mut rows) = service::compute_stock(&[dead], &[]);
    let history = service::stock_history_evidence(&[], &[], true, true);
    let suggestion = store::ReorderSnapshotRow {
        suggestion_id: "s1".to_string(),
        material_id: Some("dead".to_string()),
        material_name: None,
        material_sku: None,
        vendor_name: None,
        urgency: "HIGH".to_string(),
        status: "PENDING".to_string(),
        days_until_stockout: Some(0.0),
        suggested_quantity: None,
        unit: None,
        estimated_cost_cents: 0,
        lead_time_days: None,
        reasoning: None,
    };

    service::enrich_stock_rows(&mut rows, &[suggestion], &history);

    let row = rows
        .iter()
        .find(|row| row.material_id == "dead")
        .expect("dead");
    assert_eq!(row.days_until_stockout, Some(0.0));
    assert!(!row.dead_stock);
}

#[test]
fn material_snapshots_persist_reserved_and_incoming_from_payload() {
    let state = test_state();
    let mut first = material("m1", "example Blue", 10.0);
    first.reserved_qty = Some(4.0);
    first.incoming_qty = Some(5.0);
    {
        let mut persistence = state.persistence.lock();
        store::upsert_material_snapshots(
            persistence.connection(),
            CLIENT,
            std::slice::from_ref(&first),
            1_000,
        )
        .expect("insert");
    }
    {
        let persistence = state.persistence.lock();
        let rows = store::list_materials(persistence.connection_ref(), CLIENT).expect("list");
        assert_eq!(rows[0].reserved_qty, Some(4.0));
        assert_eq!(rows[0].incoming_qty, Some(5.0));
    }
    first.reserved_qty = Some(0.0);
    first.incoming_qty = Some(0.0);
    let summary = {
        let mut persistence = state.persistence.lock();
        store::upsert_material_snapshots(
            persistence.connection(),
            CLIENT,
            std::slice::from_ref(&first),
            2_000,
        )
        .expect("rewrite")
    };
    assert_eq!(
        summary.written, 1,
        "zero reserved/incoming is a real change"
    );
    {
        let persistence = state.persistence.lock();
        let rows = store::list_materials(persistence.connection_ref(), CLIENT).expect("list");
        assert_eq!(rows[0].reserved_qty, Some(0.0));
        assert_eq!(rows[0].incoming_qty, Some(0.0));
    }
    let unchanged = {
        let mut persistence = state.persistence.lock();
        store::upsert_material_snapshots(
            persistence.connection(),
            CLIENT,
            std::slice::from_ref(&first),
            2_500,
        )
        .expect("unchanged")
    };
    assert_eq!(
        unchanged.written, 0,
        "unchanged reserved/incoming must not rewrite"
    );
    assert_eq!(unchanged.unchanged, 1);
    first.reserved_qty = Some(2.0);
    let reserved_only = {
        let mut persistence = state.persistence.lock();
        store::upsert_material_snapshots(persistence.connection(), CLIENT, &[first], 2_600)
            .expect("reserved-only")
    };
    assert_eq!(
        reserved_only.written, 1,
        "reserved-only change must rewrite"
    );
    {
        let persistence = state.persistence.lock();
        let rows = store::list_materials(persistence.connection_ref(), CLIENT).expect("list");
        let row = rows.iter().find(|row| row.material_id == "m1").expect("m1");
        assert_eq!(row.reserved_qty, Some(2.0));
        assert_eq!(row.incoming_qty, Some(0.0));
    }

    let unknown = material("m2", "Legacy Blue", 8.0);
    {
        let mut persistence = state.persistence.lock();
        store::upsert_material_snapshots(persistence.connection(), CLIENT, &[unknown], 3_000)
            .expect("insert unknown");
    }
    let persistence = state.persistence.lock();
    let rows = store::list_materials(persistence.connection_ref(), CLIENT).expect("list");
    let unknown_row = rows
        .iter()
        .find(|row| row.material_id == "m2")
        .expect("unknown row");
    assert_eq!(
        unknown_row.reserved_qty, None,
        "NULL reserved stays None, not 0"
    );
    assert_eq!(
        unknown_row.incoming_qty, None,
        "NULL incoming stays None, not 0"
    );
}

#[test]
fn alert_rows_include_alerts_without_material_id() {
    let monitored = material_row("m1", true);
    let snapshots = vec![store::AlertSnapshotRow {
        alert_id: "orphan".to_string(),
        material_id: None,
        material_name: Some("Unknown".to_string()),
        material_sku: None,
        severity: "WARNING".to_string(),
        quantity: None,
        percentage_remaining: None,
        message: None,
        created_at: None,
    }];
    let rows = service::alert_rows(&snapshots, &[monitored]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].alert_id, "orphan");
}

#[test]
fn connector_status_exposes_inventory_list_url() {
    let _env = EnvGuard::set_many(&[
        (
            crate::env_registry::BOS_STOCKFORGE_BASE_URL.name,
            "https://api.stockforge.ai/",
        ),
        (
            crate::env_registry::BOS_STOCKFORGE_APP_URL.name,
            "https://app.stockforge.ai/",
        ),
        (
            crate::env_registry::BOS_STOCKFORGE_API_KEY.name,
            "sfk_live_test",
        ),
    ]);
    let status = service::connector_status(true);
    assert_eq!(
        status.inventory_url.as_deref(),
        Some("https://app.stockforge.ai/inventory")
    );
}

#[test]
fn classify_stockforge_error_uses_enum_variant() {
    assert_eq!(
        service::classify_stockforge_error(&StockforgeError::RateLimited {
            retry_after_ms: Some(1_000),
            message: "slow down".to_string(),
        }),
        "rate_limited"
    );
    assert_eq!(
        service::classify_stockforge_error(&StockforgeError::AuthRejected {
            message: "bad key".to_string(),
        }),
        "auth"
    );
    assert_eq!(
        service::classify_stockforge_error(&StockforgeError::Retryable {
            code: "timeout".to_string(),
            message: "hung".to_string(),
        }),
        "timeout"
    );
    assert_eq!(
        service::classify_stockforge_error(&StockforgeError::Permanent {
            code: "stockforge_request_rejected".to_string(),
            message: "nope".to_string(),
        }),
        "error"
    );
    assert_eq!(
        service::classify_stockforge_error(&StockforgeError::Retryable {
            code: "stockforge_request_failed".to_string(),
            message: "error sending request for url: timed out".to_string(),
        }),
        "error",
        "timeout class comes from the error code, not display text"
    );
}

#[test]
fn stockforge_urls_percent_encode_path_and_query_components() {
    assert_eq!(
        service::stockforge_material_url("https://app.stockforge.ai", "mat/id?x=1"),
        "https://app.stockforge.ai/inventory/mat%2Fid%3Fx%3D1"
    );
    assert_eq!(
        service::stockforge_order_url("https://app.stockforge.ai/", "SO 100#1"),
        "https://app.stockforge.ai/orders/board?search=SO%20100%231"
    );
}

#[test]
fn quiet_successful_cycle_stamps_in_memory_last_success() {
    let state = test_state();
    let fixture = FixtureStockforgeReadClient {
        materials: vec![material("m1", "example Blue", 3.0)],
        ..Default::default()
    };
    run_cycle(&state, &fixture, 10, 1_000);
    run_cycle(&state, &fixture, 10, 5_000);
    let last_advanced = {
        let persistence = state.persistence.lock();
        store::get_cursor(persistence.connection_ref(), CLIENT, store::ENTITY_MATERIAL)
            .expect("cursor")
            .last_advanced_at_ms
    };
    assert_eq!(
        last_advanced,
        Some(1_000),
        "quiet cycle must not rewrite cursor"
    );
    let last_success = state
        .sync_guards
        .guard(crate::http::Pump::Stockforge)
        .lock()
        .last_success_ms;
    assert_eq!(last_success, Some(5_000));
}

#[test]
fn total_outage_cycle_does_not_advance_last_success() {
    use bos_integrations::stockforge_read::{SfPage, StockforgeReadClient};
    struct AllFailClient;
    impl StockforgeReadClient for AllFailClient {
        fn fetch_materials(
            &self,
            _: &str,
            _: u32,
            _: u32,
        ) -> Result<SfPage<SfMaterialRecord>, StockforgeError> {
            Err(StockforgeError::Retryable {
                code: "stockforge_request_failed".to_string(),
                message: "network down".to_string(),
            })
        }
        fn fetch_active_alerts(&self, _: &str) -> Result<Vec<SfAlertRecord>, StockforgeError> {
            Err(StockforgeError::Retryable {
                code: "stockforge_request_failed".to_string(),
                message: "network down".to_string(),
            })
        }
        fn fetch_reorder_suggestions(
            &self,
            _: &str,
        ) -> Result<Vec<SfReorderSuggestionRecord>, StockforgeError> {
            Err(StockforgeError::Retryable {
                code: "stockforge_request_failed".to_string(),
                message: "network down".to_string(),
            })
        }
        fn fetch_order_board(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<Vec<SfOrderCardRecord>, StockforgeError> {
            Err(StockforgeError::Retryable {
                code: "stockforge_request_failed".to_string(),
                message: "network down".to_string(),
            })
        }
        fn fetch_purchase_orders(
            &self,
            _: &str,
            _: u32,
            _: u32,
        ) -> Result<SfPage<SfPurchaseOrderRecord>, StockforgeError> {
            Err(StockforgeError::Retryable {
                code: "stockforge_request_failed".to_string(),
                message: "network down".to_string(),
            })
        }
        fn fetch_damage_events(
            &self,
            _: &str,
            _: &str,
            _: u32,
            _: u32,
        ) -> Result<SfPage<bos_integrations::stockforge_read::SfDamageEventRecord>, StockforgeError>
        {
            Err(StockforgeError::Retryable {
                code: "stockforge_request_failed".to_string(),
                message: "network down".to_string(),
            })
        }
        fn fetch_container_photos(
            &self,
            _: &str,
            _: &str,
        ) -> Result<
            Option<Vec<bos_integrations::stockforge_read::SfPackPhotoRecord>>,
            StockforgeError,
        > {
            Err(StockforgeError::Retryable {
                code: "stockforge_request_failed".to_string(),
                message: "network down".to_string(),
            })
        }
    }

    let state = test_state();
    let fixture = FixtureStockforgeReadClient {
        materials: vec![material("m1", "example Blue", 3.0)],
        ..Default::default()
    };
    run_cycle(&state, &fixture, 10, 1_000);
    worker::run_sync_cycle(&state, &AllFailClient, &config(), 10, 9_000).expect("outage cycle");
    let last_success = state
        .sync_guards
        .guard(crate::http::Pump::Stockforge)
        .lock()
        .last_success_ms;
    assert_eq!(
        last_success,
        Some(1_000),
        "total outage must not look fresh"
    );
}

#[test]
fn alert_rows_enrich_names_and_exclude_non_monitored_stock() {
    let monitored = material_row("m1", true);
    let catalog = material_row("m2", false);
    let snapshots = vec![
        store::AlertSnapshotRow {
            alert_id: "a1".to_string(),
            material_id: Some("m1".to_string()),
            material_name: None,
            material_sku: None,
            severity: "WARNING".to_string(),
            quantity: Some(3.0),
            percentage_remaining: Some(10.0),
            message: None,
            created_at: None,
        },
        store::AlertSnapshotRow {
            alert_id: "a2".to_string(),
            material_id: Some("m2".to_string()),
            material_name: Some("Catalog only".to_string()),
            material_sku: None,
            severity: "CRITICAL".to_string(),
            quantity: Some(0.0),
            percentage_remaining: Some(0.0),
            message: None,
            created_at: None,
        },
    ];

    let rows = service::alert_rows(&snapshots, &[monitored, catalog]);

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].material_name.as_deref(), Some("Material m1"));
    assert_eq!(rows[0].material_sku.as_deref(), Some("SKU-m1"));
}

#[test]
fn orders_view_rolls_up_pipeline_controls_and_attention_order() {
    let today = "2026-06-10";
    let plain = |id: &str, status: &str, date: &str| store::OrderSnapshotRow {
        order_id: id.to_string(),
        order_number: format!("#{id}"),
        external_order_id: Some(format!("shopify-{id}")),
        platform: Some("shopify".to_string()),
        board_status: status.to_string(),
        customer_name: None,
        customer_email: None,
        total_amount_cents: 1_000,
        order_date: Some(date.to_string()),
        processed_at: None,
        item_count: 1,
        unit_count: 1,
        mapped_line_count: 1,
        line_material_ids: vec!["m1".to_string()],
        line_identity_complete: true,
        carrier: None,
        tracking_number: None,
        shipment_refs: None,
        shipment_id: None,
        ship_date: None,
        photo_count: 0,
        pack_station_container_id: None,
        needs_mapping: false,
        blocked: false,
        deducted: false,
        deduction_failed: false,
        exception: false,
        depletion_total: 0,
        depletion_applied: 0,
        depletion_failed: 0,
        depletion_reversed: 0,
        blocked_reasons_json: "[]".to_string(),
    };
    let fresh_new = plain("o1", "NEW", "2026-06-09");
    let stale_picking = plain("o2", "PICKING", "2026-06-01"); // 9 days old
    let mut unmapped = plain("o3", "NEW", "2026-06-10");
    unmapped.needs_mapping = true;
    unmapped.blocked = true;
    unmapped.blocked_reasons_json = "[\"2 lines unmapped\"]".to_string();
    let shipped_old = plain("o4", "SHIPPED", "2026-05-20"); // age irrelevant
    let mut failed_deduction = plain("o5", "DELIVERED", "2026-06-05");
    failed_deduction.deduction_failed = true;
    failed_deduction.depletion_total = 1;
    failed_deduction.depletion_failed = 1;
    let exception = {
        let mut row = plain("o6", "EXCEPTION", "2026-06-07");
        row.exception = true;
        row
    };
    let mut depleted = plain("o7", "PACKED", "2026-06-10");
    depleted.depletion_total = 1;
    depleted.depletion_applied = 1;
    depleted.deducted = true;
    let mut awaiting = plain("o8", "PACKED", "2026-06-10");
    awaiting.depletion_total = 0;
    let mut reversed = plain("o9", "PACKED", "2026-06-10");
    reversed.depletion_total = 1;
    reversed.depletion_applied = 1;
    reversed.depletion_reversed = 1;
    let mut summary_failed = plain("o10", "PACKED", "2026-06-10");
    summary_failed.depletion_total = 1;
    summary_failed.depletion_applied = 1;
    summary_failed.depletion_failed = 1;
    let mut non_shopify_mapped = plain("o10", "PACKED", "2026-06-10");
    non_shopify_mapped.order_id = "o11".to_string();
    non_shopify_mapped.order_number = "#o11".to_string();
    non_shopify_mapped.external_order_id = Some("shopify-o11".to_string());
    non_shopify_mapped.platform = Some("woocommerce".to_string());
    non_shopify_mapped.depletion_total = 1;
    non_shopify_mapped.depletion_applied = 1;

    let (pipeline, controls, rows) = service::compute_orders(
        &[
            fresh_new,
            stale_picking,
            unmapped,
            shipped_old,
            failed_deduction,
            exception,
            depleted,
            awaiting,
            reversed,
            summary_failed,
            non_shopify_mapped,
        ],
        today,
    );
    assert_eq!(pipeline.new_count, 2);
    assert_eq!(pipeline.picking_count, 1);
    assert_eq!(pipeline.packed_count, 5);
    assert_eq!(pipeline.shipped_count, 1);
    assert_eq!(pipeline.delivered_count, 1);
    assert_eq!(pipeline.exception_count, 1);
    assert_eq!(controls.needs_mapping_count, 1);
    assert_eq!(controls.shopify_order_count, 10);
    assert_eq!(controls.mapped_count, 9);
    assert_eq!(controls.depleted_count, 1);
    assert_eq!(controls.awaiting_depletion_count, 4);
    assert_eq!(controls.blocked_count, 1);
    assert_eq!(controls.deduction_failed_count, 1);
    assert_eq!(controls.stale_count, 1, "9-day-old PICKING order is stale");
    // Attention rows lead; the fresh NEW and SHIPPED orders trail.
    let leading: Vec<&str> = rows[..6].iter().map(|row| row.order_id.as_str()).collect();
    assert!(leading.contains(&"o2") && leading.contains(&"o3"));
    assert!(leading.contains(&"o5") && leading.contains(&"o6"));
    assert!(
        leading.contains(&"o9"),
        "reversed depletion needs attention"
    );
    assert!(
        leading.contains(&"o10"),
        "failed depletion summary needs attention"
    );
    let unmapped_row = rows.iter().find(|row| row.order_id == "o3").expect("row");
    assert_eq!(unmapped_row.blocked_reasons, vec!["2 lines unmapped"]);
    let stale_row = rows.iter().find(|row| row.order_id == "o2").expect("row");
    assert_eq!(stale_row.age_days, 9);
    let shipped_row = rows.iter().find(|row| row.order_id == "o4").expect("row");
    assert_eq!(shipped_row.age_days, 0, "shipped orders don't age");
}

#[test]
fn reorder_rows_surface_pending_only_and_pos_filter_open() {
    let pending = store::ReorderSnapshotRow {
        suggestion_id: "s1".to_string(),
        material_id: Some("m1".to_string()),
        material_name: Some("example Blue".to_string()),
        material_sku: None,
        vendor_name: Some("Champion".to_string()),
        urgency: "HIGH".to_string(),
        status: "PENDING".to_string(),
        days_until_stockout: Some(6.5),
        suggested_quantity: Some(50.0),
        unit: Some("gal".to_string()),
        estimated_cost_cents: 250_000,
        lead_time_days: Some(14),
        reasoning: None,
    };
    let mut accepted = pending.clone();
    accepted.suggestion_id = "s2".to_string();
    accepted.status = "ACCEPTED".to_string();
    let mut non_replenished = pending.clone();
    non_replenished.suggestion_id = "s3".to_string();
    non_replenished.material_id = Some("m2".to_string());
    let rows = service::reorder_rows(
        &[pending, accepted, non_replenished],
        &[material_row("m1", true), material_row("m2", false)],
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].suggestion_id, "s1");

    let open = store::PoSnapshotRow {
        po_id: "p1".to_string(),
        vendor_name: None,
        status: "SENT".to_string(),
        total_estimated_cost_cents: 100_000,
        freight_mode: None,
        line_count: 1,
        line_material_ids: vec!["m1".to_string()],
        line_identity_complete: true,
        created_at: None,
        sent_at: None,
    };
    let mut received = open.clone();
    received.po_id = "p2".to_string();
    received.status = "RECEIVED".to_string();
    let mut cancelled = open.clone();
    cancelled.po_id = "p3".to_string();
    cancelled.status = "CANCELLED".to_string();
    let (rows, total) = service::open_purchase_orders(&[open, received, cancelled]);
    assert_eq!(rows.len(), 1);
    assert_eq!(total, 100_000);
}

#[test]
fn webhook_signature_verification_accepts_genuine_and_rejects_forgeries() {
    // Sign the way Stockforge's webhook.service.ts does:
    // sha256=hex(hmac_sha256(secret, "{unix_secs}.{body}")).
    let secret = "whsec_test";
    let body = r#"{"event":"stock.critical","data":{"materialId":"m1"}}"#;
    let timestamp = "1781120000";
    let now_ms: u64 = 1_781_120_000_500; // half a second after the stamp
                                         // Computed independently (python hmac) — also pins our local HMAC impl.
    let genuine = "sha256=6636e594a2ae3235e720e1bd37f45865ea1a9dc550d3d9f97285fdd4e0413b62";
    assert_eq!(
        service::verify_webhook_signature(secret, timestamp, body, genuine, now_ms),
        Ok(())
    );
    // Wrong secret / tampered body → mismatch.
    assert_eq!(
        service::verify_webhook_signature("other", timestamp, body, genuine, now_ms),
        Err("webhook_signature_mismatch")
    );
    assert_eq!(
        service::verify_webhook_signature(secret, timestamp, "{}", genuine, now_ms),
        Err("webhook_signature_mismatch")
    );
    // Replayed delivery from 10 minutes ago → stale.
    assert_eq!(
        service::verify_webhook_signature(secret, timestamp, body, genuine, now_ms + 600_000),
        Err("webhook_timestamp_stale")
    );
    // Garbage timestamp header → invalid, never panics.
    assert_eq!(
        service::verify_webhook_signature(secret, "soon", body, genuine, now_ms),
        Err("webhook_timestamp_invalid")
    );
}

#[test]
fn order_window_reaches_back_thirty_days() {
    let (start, end) = service::order_window("2026-06-10");
    assert_eq!(end, "2026-06-10");
    assert_eq!(start, "2026-05-12", "30 days inclusive");
}

#[test]
fn rate_limit_stamps_backoff_and_next_cycle_stands_down() {
    use bos_integrations::stockforge_read::{SfPage, StockforgeError, StockforgeReadClient};
    struct RateLimitedClient;
    impl StockforgeReadClient for RateLimitedClient {
        fn fetch_materials(
            &self,
            _token: &str,
            _skip: u32,
            _take: u32,
        ) -> Result<SfPage<SfMaterialRecord>, StockforgeError> {
            Err(StockforgeError::RateLimited {
                retry_after_ms: Some(120_000),
                message: "429".to_string(),
            })
        }
        fn fetch_active_alerts(&self, _: &str) -> Result<Vec<SfAlertRecord>, StockforgeError> {
            panic!("cycle must stop at the first 429");
        }
        fn fetch_reorder_suggestions(
            &self,
            _: &str,
        ) -> Result<Vec<SfReorderSuggestionRecord>, StockforgeError> {
            panic!("cycle must stop at the first 429");
        }
        fn fetch_order_board(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<Vec<SfOrderCardRecord>, StockforgeError> {
            panic!("cycle must stop at the first 429");
        }
        fn fetch_purchase_orders(
            &self,
            _: &str,
            _: u32,
            _: u32,
        ) -> Result<SfPage<SfPurchaseOrderRecord>, StockforgeError> {
            panic!("cycle must stop at the first 429");
        }
        fn fetch_damage_events(
            &self,
            _: &str,
            _: &str,
            _: u32,
            _: u32,
        ) -> Result<SfPage<bos_integrations::stockforge_read::SfDamageEventRecord>, StockforgeError>
        {
            panic!("cycle must stop at the first 429");
        }
        fn fetch_container_photos(
            &self,
            _: &str,
            _: &str,
        ) -> Result<
            Option<Vec<bos_integrations::stockforge_read::SfPackPhotoRecord>>,
            StockforgeError,
        > {
            panic!("cycle must stop at the first 429");
        }
    }

    let state = test_state();
    let summary =
        worker::run_sync_cycle(&state, &RateLimitedClient, &config(), 10, 1_000).expect("cycle");
    assert!(summary.rate_limited);
    {
        let persistence = state.persistence.lock();
        let cursor =
            store::get_cursor(persistence.connection_ref(), CLIENT, store::ENTITY_MATERIAL)
                .expect("cursor");
        assert_eq!(cursor.rate_limited_until_ms, 1_000 + 120_000);
    }
    // Within the backoff window the whole cycle stands down: zero requests.
    let summary =
        worker::run_sync_cycle(&state, &RateLimitedClient, &config(), 10, 2_000).expect("cycle");
    assert_eq!(summary.requests_used, 0);
}

#[test]
fn rejected_api_key_records_the_error_and_ends_the_cycle() {
    use bos_integrations::stockforge_read::{SfPage, StockforgeError, StockforgeReadClient};
    struct RevokedKeyClient;
    impl StockforgeReadClient for RevokedKeyClient {
        fn fetch_materials(
            &self,
            _token: &str,
            _skip: u32,
            _take: u32,
        ) -> Result<SfPage<SfMaterialRecord>, StockforgeError> {
            Err(StockforgeError::AuthRejected {
                message: "stockforge 401 API_KEY_REVOKED on /api/v1/materials".to_string(),
            })
        }
        fn fetch_active_alerts(&self, _: &str) -> Result<Vec<SfAlertRecord>, StockforgeError> {
            panic!("a rejected key must end the cycle — no retry can fix it");
        }
        fn fetch_reorder_suggestions(
            &self,
            _: &str,
        ) -> Result<Vec<SfReorderSuggestionRecord>, StockforgeError> {
            panic!("a rejected key must end the cycle — no retry can fix it");
        }
        fn fetch_order_board(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<Vec<SfOrderCardRecord>, StockforgeError> {
            panic!("a rejected key must end the cycle — no retry can fix it");
        }
        fn fetch_purchase_orders(
            &self,
            _: &str,
            _: u32,
            _: u32,
        ) -> Result<SfPage<SfPurchaseOrderRecord>, StockforgeError> {
            panic!("a rejected key must end the cycle — no retry can fix it");
        }
        fn fetch_damage_events(
            &self,
            _: &str,
            _: &str,
            _: u32,
            _: u32,
        ) -> Result<SfPage<bos_integrations::stockforge_read::SfDamageEventRecord>, StockforgeError>
        {
            panic!("a rejected key must end the cycle — no retry can fix it");
        }
        fn fetch_container_photos(
            &self,
            _: &str,
            _: &str,
        ) -> Result<
            Option<Vec<bos_integrations::stockforge_read::SfPackPhotoRecord>>,
            StockforgeError,
        > {
            panic!("a rejected key must end the cycle — no retry can fix it");
        }
    }

    let state = test_state();
    let err = worker::run_sync_cycle(&state, &RevokedKeyClient, &config(), 10, 1_000)
        .expect_err("rejected key is a cycle error");
    assert!(err.contains("API_KEY_REVOKED"), "{err}");
    // The Stockforge code lands on the cursor, which the status view reads —
    // the operator learns WHICH key problem they have.
    let persistence = state.persistence.lock();
    let cursor = store::get_cursor(persistence.connection_ref(), CLIENT, store::ENTITY_MATERIAL)
        .expect("cursor");
    assert!(cursor
        .last_error
        .expect("error")
        .contains("API_KEY_REVOKED"));
}
