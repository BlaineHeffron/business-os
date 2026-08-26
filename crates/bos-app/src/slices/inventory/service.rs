//! Stockforge connector config + the pure inventory view computations (stock
//! classification, pipeline rollups, order-controls). All view math runs over
//! the local snapshot cache — nothing here talks to Stockforge.

use std::collections::{HashMap, HashSet};

use bos_contracts::inventory::{
    InventoryAlertRow, InventoryOrderControls, InventoryOrderPipeline, InventoryOrderRow,
    InventoryPurchaseOrderRow, InventoryReorderRow, InventoryStockKpis, InventoryStockRow,
    StockforgeConnectorStatus,
};

use super::store::{
    AlertSnapshotRow, MaterialSnapshotRow, OrderSnapshotRow, PoSnapshotRow, ReorderSnapshotRow,
};
use crate::env_registry;

/// How many days back the cached order-board window reaches. Wider than
/// Stockforge's default week view so stale unshipped orders can't age out of
/// sight — catching those is the missed-order-prevention metric's whole job.
pub const ORDER_WINDOW_DAYS: u32 = 30;

/// An unshipped order older than this counts as stale (order controls).
pub const STALE_AFTER_DAYS: u32 = 3;

/// Env-provided Stockforge connector credential: a static org API key
/// (VIEWER role) — no login flow, no session state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StockforgeConnectorConfig {
    pub base_url: String,
    pub api_key: String,
}

pub fn connector_config_from_env() -> Option<StockforgeConnectorConfig> {
    match (
        env_registry::string(&env_registry::BOS_STOCKFORGE_BASE_URL),
        env_registry::string(&env_registry::BOS_STOCKFORGE_API_KEY),
    ) {
        (Some(base_url), Some(api_key)) => Some(StockforgeConnectorConfig { base_url, api_key }),
        _ => None,
    }
}

pub fn webhook_secret_from_env() -> Option<String> {
    env_registry::string(&env_registry::BOS_STOCKFORGE_WEBHOOK_SECRET)
}

/// Reject webhook timestamps further than this from now (replay window).
/// Stockforge stamps each delivery attempt at send time, so honest retries
/// always carry a fresh timestamp.
pub const WEBHOOK_REPLAY_WINDOW_SECS: u64 = 300;

/// HMAC-SHA256 (RFC 2104) over sha2 — small enough that a dedicated crate
/// isn't warranted. SHA-256 block size is 64 bytes.
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut block_key = [0u8; 64];
    if key.len() > 64 {
        block_key[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block_key[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(block_key.map(|byte| byte ^ 0x36));
    inner.update(message);
    let inner_digest = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(block_key.map(|byte| byte ^ 0x5c));
    outer.update(inner_digest);
    outer.finalize().into()
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Verify one Stockforge webhook delivery: the signature header is
/// `sha256=<hmac_sha256_hex(secret, "{timestamp}.{body}")>` with the
/// timestamp in unix seconds (replay-bounded). Constant-time comparison.
pub fn verify_webhook_signature(
    secret: &str,
    timestamp_header: &str,
    body: &str,
    signature_header: &str,
    now_ms: u64,
) -> Result<(), &'static str> {
    let timestamp: u64 = timestamp_header
        .trim()
        .parse()
        .map_err(|_| "webhook_timestamp_invalid")?;
    let now_secs = now_ms / 1000;
    if timestamp.abs_diff(now_secs) > WEBHOOK_REPLAY_WINDOW_SECS {
        return Err("webhook_timestamp_stale");
    }
    let expected = format!(
        "sha256={}",
        hex(&hmac_sha256(
            secret.as_bytes(),
            format!("{}.{body}", timestamp_header.trim()).as_bytes(),
        ))
    );
    let provided = signature_header.trim().as_bytes();
    let expected_bytes = expected.as_bytes();
    if provided.len() != expected_bytes.len() {
        return Err("webhook_signature_mismatch");
    }
    let mut diff = 0u8;
    for (left, right) in provided.iter().zip(expected_bytes) {
        diff |= left ^ right;
    }
    if diff != 0 {
        return Err("webhook_signature_mismatch");
    }
    Ok(())
}

fn stockforge_app_base_from_api(api_base_url: &str) -> Option<String> {
    let app_base_url = env_registry::string(&env_registry::BOS_STOCKFORGE_APP_URL)
        .unwrap_or_else(|| stockforge_app_url_from_api_url(api_base_url));
    let app_base_url = app_base_url.trim().trim_end_matches('/');
    if app_base_url.is_empty() {
        None
    } else {
        Some(app_base_url.to_string())
    }
}

fn stockforge_order_board_url(api_base_url: &str) -> Option<String> {
    stockforge_app_base_from_api(api_base_url).map(|app| format!("{app}/orders/board"))
}

fn stockforge_inventory_list_url(api_base_url: &str) -> Option<String> {
    stockforge_app_base_from_api(api_base_url).map(|app| format!("{app}/inventory"))
}

pub fn stockforge_app_base() -> Option<String> {
    connector_config_from_env().and_then(|config| stockforge_app_base_from_api(&config.base_url))
}

fn encode_stockforge_component(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    encoded
}

pub fn stockforge_material_url(app_base: &str, material_id: &str) -> String {
    format!(
        "{}/inventory/{}",
        app_base.trim_end_matches('/'),
        encode_stockforge_component(material_id)
    )
}

pub fn stockforge_order_url(app_base: &str, order_number: &str) -> String {
    // Inventorium PackStation already deep-links the board with ?search=.
    format!(
        "{}/orders/board?search={}",
        app_base.trim_end_matches('/'),
        encode_stockforge_component(order_number)
    )
}

pub fn classify_stockforge_error(
    error: &bos_integrations::stockforge_read::StockforgeError,
) -> &'static str {
    use bos_integrations::stockforge_read::StockforgeError;
    match error {
        StockforgeError::RateLimited { .. } => "rate_limited",
        StockforgeError::AuthRejected { .. } => "auth",
        StockforgeError::Retryable { code, .. } | StockforgeError::Permanent { code, .. } => {
            if code.eq_ignore_ascii_case("timeout") {
                "timeout"
            } else {
                "error"
            }
        }
    }
}

fn stockforge_app_url_from_api_url(api_base_url: &str) -> String {
    let trimmed = api_base_url.trim();
    let Ok(mut url) = url::Url::parse(trimmed) else {
        return trimmed.to_string();
    };
    if url.host_str() == Some("api.stockforge.ai") {
        let _ = url.set_host(Some("app.stockforge.ai"));
        url.to_string().trim_end_matches('/').to_string()
    } else {
        trimmed.to_string()
    }
}

pub fn connector_status(has_synced: bool) -> StockforgeConnectorStatus {
    match connector_config_from_env() {
        Some(config) => {
            let order_board_url = stockforge_order_board_url(&config.base_url);
            let inventory_url = stockforge_inventory_list_url(&config.base_url);
            StockforgeConnectorStatus {
                configured: true,
                base_url: Some(config.base_url),
                order_board_url,
                inventory_url,
                has_synced,
                blocked_reason: None,
            }
        }
        None => StockforgeConnectorStatus {
            configured: false,
            base_url: None,
            order_board_url: None,
            inventory_url: None,
            has_synced,
            blocked_reason: Some(
                "stockforge_unconfigured: set BOS_STOCKFORGE_BASE_URL and \
                 BOS_STOCKFORGE_API_KEY (an ADMIN creates a VIEWER-role key in \
                 Stockforge Settings → API Keys)"
                    .to_string(),
            ),
        },
    }
}

// ---- Civil date helpers (Howard Hinnant algorithms, same as qbo_views) ----

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let adjusted_year = if month <= 2 { year - 1 } else { year };
    let era = if adjusted_year >= 0 {
        adjusted_year
    } else {
        adjusted_year - 399
    } / 400;
    let year_of_era = adjusted_year - era * 400;
    let month_shift = if month > 2 { month - 3 } else { month + 9 } as i64;
    let day_of_year = (153 * month_shift + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_shift = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_shift + 2) / 5 + 1) as u32;
    let month = if month_shift < 10 {
        month_shift + 3
    } else {
        month_shift - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

fn format_days(days: i64) -> String {
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Parse a leading YYYY-MM-DD (RFC3339 timestamps qualify) → days since
/// epoch. None for malformed dates.
fn parse_date_days(date: &str) -> Option<i64> {
    let bytes = date.as_bytes();
    if bytes.len() < 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let year: i64 = date.get(0..4)?.parse().ok()?;
    let month: u32 = date.get(5..7)?.parse().ok()?;
    let day: u32 = date.get(8..10)?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(days_from_civil(year, month, day))
}

/// Today's civil date as YYYY-MM-DD (UTC; inventory views tolerate the offset).
pub fn today_string(now_ms: u64) -> String {
    format_days((now_ms / 86_400_000) as i64)
}

/// The board window [start, end] (inclusive YYYY-MM-DD) ending today.
pub fn order_window(today: &str) -> (String, String) {
    let today_days = parse_date_days(today).unwrap_or(0);
    (
        format_days(today_days - (ORDER_WINDOW_DAYS as i64 - 1)),
        today.to_string(),
    )
}

// ---- Stock view ----

/// quantity × unit cost, rounded to integer cents.
fn stock_value_cents(quantity: f64, unit_cost_cents: i64) -> i64 {
    (quantity * unit_cost_cents as f64).round() as i64
}

/// Stocked bucket. Same 12-cell table as Stockforge `isStockedPolicies`
/// and docs/stocked-rule.md: sale depletion STOCK and replenishment in
/// AUTO / PURCHASE / NONE. STOCK+PRODUCTION is custom, not stocked.
/// Inactive and null policies fail closed on value (pending sync).
fn monitors_stock(material: &MaterialSnapshotRow) -> bool {
    material.is_active
        && material.sale_depletion_policy.as_deref() == Some("STOCK")
        && matches!(
            material.replenishment_policy.as_deref(),
            Some("AUTO" | "PURCHASE" | "NONE")
        )
}

/// Alert gate. Mirror of Stockforge `stockAlertsEnabledForSaleDepletion`:
/// never hide an alert Stockforge raised. Null sale depletion fails open.
/// Catalog (NONE) and built-to-order (COMPONENTS) stay suppressed.
fn depletes_on_hand(material: &MaterialSnapshotRow) -> bool {
    material.is_active
        && !matches!(
            material.sale_depletion_policy.as_deref(),
            Some("COMPONENTS") | Some("NONE")
        )
}

fn permits_purchase_reorder(material: &MaterialSnapshotRow) -> bool {
    monitors_stock(material)
        && material.is_purchasable == Some(true)
        && matches!(
            material.replenishment_policy.as_deref(),
            Some("AUTO" | "PURCHASE")
        )
}

/// Classify one material: out | critical | warning | ok. ABSOLUTE thresholds
/// compare directly; PERCENTAGE thresholds can't be computed from the
/// snapshot alone (no base quantity), so Stockforge's own active alert for
/// the material is the authority — it computed the percentage server-side.
fn stock_status(material: &MaterialSnapshotRow, alert_severity: Option<&str>) -> &'static str {
    if !depletes_on_hand(material) {
        return "not_monitored";
    }
    // Null/unknown policies are pending sync. Keep an alert Stockforge actually
    // raised visible, but do not synthesize local quantity/threshold alerts for
    // a row whose depletion behavior is not yet known.
    if material.sale_depletion_policy.as_deref() != Some("STOCK") {
        return match alert_severity {
            Some("CRITICAL") => "critical",
            Some("WARNING") => "warning",
            _ => "not_monitored",
        };
    }
    if material.quantity <= 0.0 {
        return "out";
    }
    if alert_severity == Some("CRITICAL") {
        return "critical";
    }
    let absolute = material.threshold_type.as_deref() == Some("ABSOLUTE");
    if absolute {
        if let Some(critical) = material.critical_threshold {
            if material.quantity <= critical {
                return "critical";
            }
        }
    }
    if alert_severity == Some("WARNING") {
        return "warning";
    }
    if absolute {
        if let Some(warning) = material.warning_threshold {
            if material.quantity <= warning {
                return "warning";
            }
        }
    }
    "ok"
}

/// Stock rows + KPI rollup over ACTIVE materials (inactive ones are hidden —
/// they're discontinued SKUs, not stockouts). Alert severities join by
/// material id so PERCENTAGE-threshold materials classify correctly.
pub fn compute_stock(
    materials: &[MaterialSnapshotRow],
    alerts: &[AlertSnapshotRow],
) -> (InventoryStockKpis, Vec<InventoryStockRow>) {
    let severity_rank = |severity: &str| match severity {
        "CRITICAL" => 2,
        "" => 0,
        _ => 1,
    };
    let mut alert_by_material: HashMap<&str, &str> = HashMap::new();
    for alert in alerts {
        let Some(material_id) = alert.material_id.as_deref() else {
            continue;
        };
        let entry = alert_by_material.entry(material_id).or_insert("");
        if severity_rank(&alert.severity) > severity_rank(entry) {
            *entry = alert.severity.as_str();
        }
    }
    let app_base = stockforge_app_base();
    let mut kpis = InventoryStockKpis {
        active_materials: 0,
        monitored_materials: 0,
        not_monitored_count: 0,
        warning_count: 0,
        critical_count: 0,
        out_of_stock_count: 0,
        stock_value_cents: 0,
        catalog_value_cents: 0,
    };
    let mut rows = Vec::new();
    for material in materials.iter().filter(|material| material.is_active) {
        let alert_severity = alert_by_material
            .get(material.material_id.as_str())
            .copied()
            .filter(|severity| !severity.is_empty());
        let status = stock_status(material, alert_severity);
        let value = stock_value_cents(material.quantity, material.unit_cost_cents);
        let is_stocked = monitors_stock(material);
        let reserved_qty = material.reserved_qty;
        let incoming_qty = material.incoming_qty;
        let available_qty = reserved_qty.map(|reserved| (material.quantity - reserved).max(0.0));
        kpis.active_materials += 1;
        kpis.catalog_value_cents += value;
        if is_stocked {
            kpis.monitored_materials += 1;
            kpis.stock_value_cents += value;
        } else {
            kpis.not_monitored_count += 1;
        }
        match status {
            "out" => kpis.out_of_stock_count += 1,
            "critical" => kpis.critical_count += 1,
            "warning" => kpis.warning_count += 1,
            _ => {}
        }
        rows.push(InventoryStockRow {
            material_id: material.material_id.clone(),
            name: material.name.clone(),
            sku: material.sku.clone(),
            category: material.category.clone(),
            quantity: material.quantity,
            reserved_qty,
            incoming_qty,
            available_qty,
            days_until_stockout: None,
            unit: material.unit.clone(),
            stock_status: status.to_string(),
            is_purchasable: material.is_purchasable,
            replenishment_policy: material.replenishment_policy.clone(),
            sale_depletion_policy: material.sale_depletion_policy.clone(),
            warning_threshold: material.warning_threshold,
            critical_threshold: material.critical_threshold,
            stock_value_cents: value,
            vendor_name: material.vendor_name.clone(),
            lead_time_days: material.lead_time_days,
            is_stocked,
            dead_stock: false,
            external_url: app_base
                .as_deref()
                .map(|app| stockforge_material_url(app, &material.material_id)),
        });
    }
    // Problems first (out → critical → warning → ok → not monitored), then by name (the list
    // arrives name-sorted from the store, and the sort is stable).
    let rank = |status: &str| match status {
        "out" => 0,
        "critical" => 1,
        "warning" => 2,
        "ok" => 3,
        _ => 4,
    };
    rows.sort_by_key(|row| rank(&row.stock_status));
    (kpis, rows)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StockHistoryEvidence {
    pub demand_history_complete: bool,
    pub recent_demand_material_ids: HashSet<String>,
    pub inbound_history_complete: bool,
    pub inbound_material_ids: HashSet<String>,
}

/// Assemble per-material demand/inbound evidence from the already-cached
/// 30-day order board and purchase-order snapshots. A single incomplete line
/// makes the applicable history incomplete so dead-stock classification fails
/// closed for every material.
pub fn stock_history_evidence(
    orders: &[OrderSnapshotRow],
    purchase_orders: &[PoSnapshotRow],
    order_snapshot_available: bool,
    po_snapshot_available: bool,
) -> StockHistoryEvidence {
    let open_pos: Vec<&PoSnapshotRow> = purchase_orders
        .iter()
        .filter(|po| is_open_purchase_order(&po.status))
        .collect();
    StockHistoryEvidence {
        demand_history_complete: order_snapshot_available
            && orders.iter().all(|order| order.line_identity_complete),
        recent_demand_material_ids: orders
            .iter()
            .flat_map(|order| order.line_material_ids.iter().cloned())
            .collect(),
        inbound_history_complete: po_snapshot_available
            && open_pos.iter().all(|po| po.line_identity_complete),
        inbound_material_ids: open_pos
            .iter()
            .flat_map(|po| po.line_material_ids.iter().cloned())
            .collect(),
    }
}

/// Add prediction-service cover plus conservative dead-stock classification
/// to computed stock rows. Null cover stays null; duplicate prediction rows
/// choose the smallest supplied value deterministically.
pub fn enrich_stock_rows(
    rows: &mut [InventoryStockRow],
    reorders: &[ReorderSnapshotRow],
    history: &StockHistoryEvidence,
) {
    let mut cover_by_material: HashMap<&str, f64> = HashMap::new();
    for reorder in reorders.iter().filter(|row| row.status == "PENDING") {
        let (Some(material_id), Some(days)) =
            (reorder.material_id.as_deref(), reorder.days_until_stockout)
        else {
            continue;
        };
        cover_by_material
            .entry(material_id)
            .and_modify(|current| *current = current.min(days))
            .or_insert(days);
    }
    for row in rows {
        if !row.is_stocked {
            continue;
        }
        row.days_until_stockout = cover_by_material.get(row.material_id.as_str()).copied();
        // A stockout prediction is modeled demand; do not also label dead stock.
        row.dead_stock = row.days_until_stockout.is_none()
            && row.quantity > 0.0
            && history.demand_history_complete
            && history.inbound_history_complete
            && !history
                .recent_demand_material_ids
                .contains(&row.material_id)
            && !history.inbound_material_ids.contains(&row.material_id);
    }
}

fn alert_row(
    snapshot: &AlertSnapshotRow,
    material: Option<&MaterialSnapshotRow>,
    app_base: Option<&str>,
) -> InventoryAlertRow {
    let material_id = snapshot
        .material_id
        .clone()
        .or_else(|| material.map(|row| row.material_id.clone()));
    let external_url = app_base.and_then(|app| {
        material_id
            .as_deref()
            .map(|id| stockforge_material_url(app, id))
    });
    InventoryAlertRow {
        alert_id: snapshot.alert_id.clone(),
        material_id,
        material_name: snapshot
            .material_name
            .clone()
            .or_else(|| material.map(|row| row.name.clone())),
        material_sku: snapshot
            .material_sku
            .clone()
            .or_else(|| material.and_then(|row| row.sku.clone())),
        severity: snapshot.severity.clone(),
        quantity: snapshot.quantity,
        percentage_remaining: snapshot.percentage_remaining,
        message: snapshot.message.clone(),
        created_at: snapshot.created_at.clone(),
        external_url,
    }
}

/// Active Stockforge alerts restricted to independently monitored stock.
/// This also suppresses stale alerts for built-to-order/non-replenished
/// items and enriches alert identity from the material snapshot.
pub fn alert_rows(
    snapshots: &[AlertSnapshotRow],
    materials: &[MaterialSnapshotRow],
) -> Vec<InventoryAlertRow> {
    let material_by_id: HashMap<&str, &MaterialSnapshotRow> = materials
        .iter()
        .filter(|material| depletes_on_hand(material))
        .map(|material| (material.material_id.as_str(), material))
        .collect();
    let app_base = stockforge_app_base();
    snapshots
        .iter()
        .filter_map(|snapshot| match snapshot.material_id.as_deref() {
            None => Some(alert_row(snapshot, None, app_base.as_deref())),
            Some(material_id) => material_by_id
                .get(material_id)
                .map(|material| alert_row(snapshot, Some(material), app_base.as_deref())),
        })
        .collect()
}

/// Only PENDING suggestions surface — accepted/rejected/expired ones are
/// Stockforge history, not dashboard work.
pub fn reorder_rows(
    snapshots: &[ReorderSnapshotRow],
    materials: &[MaterialSnapshotRow],
) -> Vec<InventoryReorderRow> {
    let eligible_ids: std::collections::HashSet<&str> = materials
        .iter()
        .filter(|material| permits_purchase_reorder(material))
        .map(|material| material.material_id.as_str())
        .collect();
    let app_base = stockforge_app_base();
    snapshots
        .iter()
        .filter(|snapshot| {
            snapshot.status == "PENDING"
                && snapshot
                    .material_id
                    .as_deref()
                    .is_some_and(|id| eligible_ids.contains(id))
        })
        .map(|snapshot| {
            let external_url = app_base.as_deref().and_then(|app| {
                snapshot
                    .material_id
                    .as_deref()
                    .map(|id| stockforge_material_url(app, id))
            });
            InventoryReorderRow {
                suggestion_id: snapshot.suggestion_id.clone(),
                material_id: snapshot.material_id.clone(),
                material_name: snapshot.material_name.clone(),
                material_sku: snapshot.material_sku.clone(),
                vendor_name: snapshot.vendor_name.clone(),
                urgency: snapshot.urgency.clone(),
                days_until_stockout: snapshot.days_until_stockout,
                suggested_quantity: snapshot.suggested_quantity,
                unit: snapshot.unit.clone(),
                estimated_cost_cents: snapshot.estimated_cost_cents,
                lead_time_days: snapshot.lead_time_days,
                reasoning: snapshot.reasoning.clone(),
                external_url,
            }
        })
        .collect()
}

// ---- Order board view ----

/// Pre-shipment pipeline columns (the ones where an order can go stale).
fn pre_shipment(status: &str) -> bool {
    matches!(status, "NEW" | "PICKING" | "PACKED")
}

fn order_age_days(order_date: Option<&str>, today_days: i64) -> i64 {
    order_date
        .and_then(parse_date_days)
        .map(|ordered| (today_days - ordered).max(0))
        .unwrap_or(0)
}

/// True when the operator should look at this order before the rest.
fn needs_attention(row: &InventoryOrderRow) -> bool {
    row.exception
        || row.deduction_failed
        || row.depletion_failed > 0
        || row.depletion_reversed > 0
        || row.blocked
        || row.needs_mapping
        || (pre_shipment(&row.board_status) && row.age_days > STALE_AFTER_DAYS as i64)
}

fn is_shopify(platform: Option<&str>) -> bool {
    platform
        .map(|platform| platform.eq_ignore_ascii_case("shopify"))
        .unwrap_or(false)
}

fn all_reported_lines_mapped(snapshot: &OrderSnapshotRow) -> bool {
    snapshot.item_count > 0
        && snapshot.mapped_line_count >= snapshot.item_count
        && !snapshot.needs_mapping
}

fn depletion_applied(snapshot: &OrderSnapshotRow) -> bool {
    snapshot.depletion_failed == 0
        && snapshot.depletion_reversed == 0
        && (snapshot.deducted
            || (snapshot.depletion_total > 0
                && snapshot.depletion_applied >= snapshot.depletion_total))
}

fn awaiting_depletion(snapshot: &OrderSnapshotRow) -> bool {
    all_reported_lines_mapped(snapshot)
        && !snapshot.blocked
        && !snapshot.exception
        && !snapshot.deduction_failed
        && snapshot.depletion_failed == 0
        && snapshot.depletion_reversed == 0
        && !depletion_applied(snapshot)
}

/// Assemble the order view: pipeline counts, order-controls rollup, and the
/// card list with attention-first ordering.
pub fn compute_orders(
    snapshots: &[OrderSnapshotRow],
    today: &str,
) -> (
    InventoryOrderPipeline,
    InventoryOrderControls,
    Vec<InventoryOrderRow>,
) {
    let today_days = parse_date_days(today).unwrap_or(0);
    let mut pipeline = InventoryOrderPipeline {
        new_count: 0,
        picking_count: 0,
        packed_count: 0,
        shipped_count: 0,
        delivered_count: 0,
        exception_count: 0,
    };
    let mut controls = InventoryOrderControls {
        shopify_order_count: 0,
        mapped_count: 0,
        depleted_count: 0,
        awaiting_depletion_count: 0,
        needs_mapping_count: 0,
        deduction_failed_count: 0,
        blocked_count: 0,
        stale_count: 0,
        stale_after_days: STALE_AFTER_DAYS,
    };
    let app_base = stockforge_app_base();
    let mut rows = Vec::new();
    for snapshot in snapshots {
        match snapshot.board_status.as_str() {
            "NEW" => pipeline.new_count += 1,
            "PICKING" => pipeline.picking_count += 1,
            "PACKED" => pipeline.packed_count += 1,
            "SHIPPED" => pipeline.shipped_count += 1,
            "DELIVERED" => pipeline.delivered_count += 1,
            _ => pipeline.exception_count += 1,
        }
        let age_days = if pre_shipment(&snapshot.board_status) {
            order_age_days(snapshot.order_date.as_deref(), today_days)
        } else {
            0
        };
        let blocked_reasons: Vec<String> =
            serde_json::from_str(&snapshot.blocked_reasons_json).unwrap_or_default();
        let row = InventoryOrderRow {
            order_id: snapshot.order_id.clone(),
            order_number: snapshot.order_number.clone(),
            external_order_id: snapshot.external_order_id.clone(),
            platform: snapshot.platform.clone(),
            board_status: snapshot.board_status.clone(),
            customer_name: snapshot.customer_name.clone(),
            customer_email: snapshot.customer_email.clone(),
            total_cents: snapshot.total_amount_cents,
            order_date: snapshot.order_date.clone(),
            processed_at: snapshot.processed_at.clone(),
            item_count: snapshot.item_count,
            unit_count: snapshot.unit_count,
            mapped_line_count: snapshot.mapped_line_count,
            carrier: snapshot.carrier.clone(),
            tracking_number: snapshot.tracking_number.clone(),
            age_days,
            needs_mapping: snapshot.needs_mapping,
            blocked: snapshot.blocked,
            deducted: snapshot.deducted,
            deduction_failed: snapshot.deduction_failed,
            exception: snapshot.exception,
            depletion_total: snapshot.depletion_total,
            depletion_applied: snapshot.depletion_applied,
            depletion_failed: snapshot.depletion_failed,
            depletion_reversed: snapshot.depletion_reversed,
            blocked_reasons,
            external_url: app_base
                .as_deref()
                .map(|app| stockforge_order_url(app, &snapshot.order_number)),
        };
        let shopify_order = is_shopify(snapshot.platform.as_deref());
        if shopify_order {
            controls.shopify_order_count += 1;
            if all_reported_lines_mapped(snapshot) {
                controls.mapped_count += 1;
            }
            if depletion_applied(snapshot) {
                controls.depleted_count += 1;
            }
            if awaiting_depletion(snapshot) {
                controls.awaiting_depletion_count += 1;
            }
        }
        if row.needs_mapping {
            controls.needs_mapping_count += 1;
        }
        if row.deduction_failed {
            controls.deduction_failed_count += 1;
        }
        if row.blocked {
            controls.blocked_count += 1;
        }
        if pre_shipment(&row.board_status) && row.age_days > STALE_AFTER_DAYS as i64 {
            controls.stale_count += 1;
        }
        rows.push(row);
    }
    // Attention first; within each group keep store order (newest first).
    rows.sort_by_key(|row| !needs_attention(row) as u8);
    (pipeline, controls, rows)
}

// ---- Purchase orders view ----

/// Open = inbound stock still expected (not RECEIVED/CANCELLED).
pub fn is_open_purchase_order(status: &str) -> bool {
    !matches!(status, "RECEIVED" | "CANCELLED")
}

pub fn open_purchase_orders(snapshots: &[PoSnapshotRow]) -> (Vec<InventoryPurchaseOrderRow>, i64) {
    let mut total = 0;
    // No per-PO web route exists; link to the Stockforge inventory root.
    let po_url =
        stockforge_app_base().map(|app| format!("{}/inventory", app.trim_end_matches('/')));
    let rows: Vec<InventoryPurchaseOrderRow> = snapshots
        .iter()
        .filter(|snapshot| is_open_purchase_order(&snapshot.status))
        .map(|snapshot| {
            total += snapshot.total_estimated_cost_cents;
            InventoryPurchaseOrderRow {
                po_id: snapshot.po_id.clone(),
                vendor_name: snapshot.vendor_name.clone(),
                status: snapshot.status.clone(),
                total_cents: snapshot.total_estimated_cost_cents,
                freight_mode: snapshot.freight_mode.clone(),
                line_count: snapshot.line_count,
                created_at: snapshot.created_at.clone(),
                sent_at: snapshot.sent_at.clone(),
                external_url: po_url.clone(),
            }
        })
        .collect();
    (rows, total)
}
