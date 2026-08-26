//! Inventory view contracts (Stockforge connector): connector status, sync
//! state, and the cached stock / low-stock / order-pipeline / inbound-PO read
//! models. Every response that renders inventory data carries
//! [`InventorySyncInfo`] so the UI can say how fresh the numbers are — the
//! browser never talks to Stockforge, only to the local cache.

use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StockforgeConnectorStatus {
    /// True when the env credential is present (base URL + service account).
    pub configured: bool,
    /// API base URL used by the read-only connector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// User-facing Stockforge order board deep link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_board_url: Option<String>,
    /// User-facing Stockforge inventory list (full report / export).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inventory_url: Option<String>,
    /// Set after the first successful sync; the practical "connected" signal.
    pub has_synced: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
}

/// Sync freshness attached to every inventory view response.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventorySyncInfo {
    /// The background pump's env gate (manual sync works regardless).
    pub sync_enabled: bool,
    pub in_flight: bool,
    /// False until the initial full material walk completes.
    pub backfill_complete: bool,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub last_synced_at_ms: Option<u64>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub material_count: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub order_count: u64,
    /// Stockforge API requests spent by the most recent cycle.
    pub last_requests_used: u32,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub next_sync_allowed_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// rate_limited | auth | timeout | error. Set from the StockforgeError
    /// variant at record time, not parsed from display text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error_class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub last_error_at_ms: Option<u64>,
}

/// One material with stock state classified against its thresholds.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InventoryStockRow {
    pub material_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sku: Option<String>,
    /// LIQUID | FABRIC | DISCRETE (raw Stockforge category).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    pub quantity: f64,
    /// Allocated to open orders (Stockforge reservedQty). None if the payload omitted it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reserved_qty: Option<f64>,
    /// Stockforge onOrderQty (SENT|CONFIRMED, stock-unit). None if omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incoming_qty: Option<f64>,
    /// on hand minus reserved, floored at 0. None when reserved is unknown.
    /// Incoming is never added.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_qty: Option<f64>,
    /// Stockforge prediction-service days until stockout, used as days of
    /// cover. None means the cached reorder payload supplied no burn result;
    /// callers must render that as unknown rather than zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub days_until_stockout: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// ok | warning | critical | out | not_monitored.
    pub stock_status: String,
    /// Stockforge explicitly permits purchasing this item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_purchasable: Option<bool>,
    /// AUTO | PURCHASE | PRODUCTION | NONE.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replenishment_policy: Option<String>,
    /// STOCK | COMPONENTS | NONE.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sale_depletion_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning_threshold: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub critical_threshold: Option<f64>,
    /// quantity × unit cost, integer cents.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub stock_value_cents: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub lead_time_days: Option<i64>,
    /// True when this active material is stocked/monitored (STOCK +
    /// AUTO|PURCHASE|PRODUCTION).
    pub is_stocked: bool,
    /// Stocked on-hand inventory with a complete cached 30-day demand and
    /// open-PO line history, but no demand or inbound line for this material.
    /// False also covers unknown history; only true is operator-labeled.
    #[serde(default)]
    pub dead_stock: bool,
    /// Stockforge material page when the app URL is configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_url: Option<String>,
}

/// Headline numbers for the stock view's KPI cards.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventoryStockKpis {
    pub active_materials: u32,
    /// Active items eligible for independent low-stock alerts.
    pub monitored_materials: u32,
    /// Active built-to-order/non-replenished/legacy items excluded from alerts.
    pub not_monitored_count: u32,
    pub warning_count: u32,
    pub critical_count: u32,
    pub out_of_stock_count: u32,
    /// Stocked/monitored materials only — not raw catalog value.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub stock_value_cents: i64,
    /// All active catalog materials at cost, including non-stocked.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub catalog_value_cents: i64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InventoryStockResponse {
    pub kpis: InventoryStockKpis,
    pub materials: Vec<InventoryStockRow>,
    pub sync: InventorySyncInfo,
}

/// One active low-stock alert (lifecycle owned by Stockforge).
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InventoryAlertRow {
    pub alert_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material_sku: Option<String>,
    /// WARNING | CRITICAL.
    pub severity: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantity: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percentage_remaining: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_url: Option<String>,
}

/// One pending reorder suggestion (burn-rate + lead-time aware).
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InventoryReorderRow {
    pub suggestion_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material_sku: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor_name: Option<String>,
    /// LOW | MEDIUM | HIGH | CRITICAL.
    pub urgency: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub days_until_stockout: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_quantity: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub estimated_cost_cents: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub lead_time_days: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_url: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InventoryAlertsResponse {
    pub alerts: Vec<InventoryAlertRow>,
    pub reorder_suggestions: Vec<InventoryReorderRow>,
    pub sync: InventorySyncInfo,
}

/// One order card from the cached live board window.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventoryOrderRow {
    pub order_id: String,
    pub order_number: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_order_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    /// NEW | PICKING | PACKED | SHIPPED | DELIVERED | EXCEPTION.
    pub board_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer_email: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub total_cents: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub processed_at: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub item_count: i64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub unit_count: i64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub mapped_line_count: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carrier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracking_number: Option<String>,
    /// Days the order has sat in a pre-shipment column (0 once shipped).
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub age_days: i64,
    pub needs_mapping: bool,
    pub blocked: bool,
    pub deducted: bool,
    pub deduction_failed: bool,
    pub exception: bool,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub depletion_total: i64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub depletion_applied: i64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub depletion_failed: i64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub depletion_reversed: i64,
    /// Why the order can't advance (empty when not blocked).
    pub blocked_reasons: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_url: Option<String>,
}

/// Pipeline counts for the board summary strip (one per column).
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventoryOrderPipeline {
    pub new_count: u32,
    pub picking_count: u32,
    pub packed_count: u32,
    pub shipped_count: u32,
    pub delivered_count: u32,
    pub exception_count: u32,
}

/// Order-controls rollup: the work-agreement "paid orders are visible,
/// fulfilled, and deducted" visibility numbers.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventoryOrderControls {
    /// Shopify-origin orders in the cached board window.
    pub shopify_order_count: u32,
    /// Orders with every reported line mapped and no mapping backlog.
    pub mapped_count: u32,
    /// Orders with all reported depletion rows applied.
    pub depleted_count: u32,
    /// Orders ready to deduct once their configured Stockforge trigger runs.
    pub awaiting_depletion_count: u32,
    /// Orders with at least one unmapped SKU line (reconciliation backlog).
    pub needs_mapping_count: u32,
    /// Orders whose inventory deduction failed (reconcile in Stockforge).
    pub deduction_failed_count: u32,
    /// Orders blocked from advancing for any reason.
    pub blocked_count: u32,
    /// Unshipped orders older than the stale threshold.
    pub stale_count: u32,
    /// Days before an unshipped order counts as stale.
    pub stale_after_days: u32,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventoryOrdersResponse {
    pub pipeline: InventoryOrderPipeline,
    pub controls: InventoryOrderControls,
    /// Orders needing operator attention first, then the rest of the window,
    /// newest first.
    pub orders: Vec<InventoryOrderRow>,
    /// How many days back the cached board window reaches.
    pub window_days: u32,
    pub sync: InventorySyncInfo,
}

/// One inbound purchase order.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventoryPurchaseOrderRow {
    pub po_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor_name: Option<String>,
    /// DRAFT | PENDING_APPROVAL | SENT | CONFIRMED | RECEIVED | CANCELLED.
    pub status: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub total_cents: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freight_mode: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub line_count: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_url: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventoryPurchaseOrdersResponse {
    /// Open (not RECEIVED/CANCELLED) POs, newest first.
    pub purchase_orders: Vec<InventoryPurchaseOrderRow>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub open_total_cents: i64,
    pub sync: InventorySyncInfo,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventorySyncNowResponse {
    pub accepted: bool,
    /// Refusal reason when not accepted: sync_in_flight | sync_cooldown |
    /// stockforge_not_configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub next_allowed_at_ms: u64,
}
