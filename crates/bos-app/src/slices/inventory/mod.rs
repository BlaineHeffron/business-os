//! Read-only inventory connector + cached views. Stockforge is the first
//! (and for Demo, the only) inventory source; the slice id stays generic
//! because the read models — stock on hand, low-stock alerts, reorder
//! suggestions, order pipeline, inbound POs — are what any inventory system
//! would feed this tab.
//!
//! Division of labor with Stockforge itself: the full interactive order
//! board (drag/drop, packing flow, barcode scans) lives in Stockforge; this
//! slice caches a read-only window of it so the dashboard can show pipeline
//! counts, exceptions, and order-control gaps (unmapped SKUs, failed
//! deductions, stale unshipped orders) next to the rest of the business.
//!
//! KNOWN SEAMS: materials and POs are upsert-only — a hard delete in
//! Stockforge lingers here until a wipe/backfill (same posture as qbo_views
//! invoices). Alerts/reorders/orders are full-set syncs and self-heal.

pub mod routes;
pub mod service;
pub mod store;
pub mod worker;

#[cfg(test)]
mod tests;

use crate::env_registry;
use crate::slices::{RouteSpec, SliceSpec};

pub const SLICE: SliceSpec = SliceSpec {
    id: "inventory",
    title: "Inventory cached views (Stockforge)",
    summary: "Read-only Stockforge connector: env-configured org API key (VIEWER role, sfk_live_…), a request-budgeted sync pump into local snapshot caches (webhook events kick it early), and stock/alert/order-board/PO views served from sqlite only — the UI never hits the Stockforge API.",
    routes: &[
        RouteSpec {
            method: "GET",
            path: "/api/connectors/stockforge/status",
            summary: "Connector status (configured, base URL, has synced); blocked_reason when env is missing",
        },
        RouteSpec {
            method: "POST",
            path: "/api/inventory/sync",
            summary: "Kick one budgeted sync cycle (202; 409 while one runs or during the cooldown)",
        },
        RouteSpec {
            method: "GET",
            path: "/api/inventory/stock",
            summary: "Cached stock on hand with low-stock classification + KPI rollup — local cache only, never Stockforge",
        },
        RouteSpec {
            method: "GET",
            path: "/api/inventory/alerts",
            summary: "Active low-stock alerts + pending reorder suggestions (burn-rate / lead-time aware)",
        },
        RouteSpec {
            method: "GET",
            path: "/api/inventory/orders",
            summary: "Order-board summary over the cached 30-day window: pipeline counts, exceptions, order-control gaps, attention-first cards",
        },
        RouteSpec {
            method: "GET",
            path: "/api/inventory/purchase-orders",
            summary: "Open purchase orders (inbound stock) from the cached snapshot",
        },
        RouteSpec {
            method: "POST",
            path: "/api/webhooks/stockforge",
            summary: "Inbound Stockforge webhook (HMAC-verified, replay-bounded); a verified event kicks one guarded sync cycle — payloads are never trusted as data",
        },
    ],
    tables: &[
        "stockforge_sync_cursors",
        "stockforge_material_snapshots",
        "stockforge_alert_snapshots",
        "stockforge_reorder_snapshots",
        "stockforge_order_snapshots",
        "stockforge_po_snapshots",
    ],
    env_vars: &[
        &env_registry::BOS_STOCKFORGE_BASE_URL,
        &env_registry::BOS_STOCKFORGE_APP_URL,
        &env_registry::BOS_STOCKFORGE_API_KEY,
        &env_registry::BOS_STOCKFORGE_SYNC_ENABLED,
        &env_registry::BOS_STOCKFORGE_SYNC_INTERVAL_SECS,
        &env_registry::BOS_STOCKFORGE_MAX_REQUESTS_PER_CYCLE,
        &env_registry::BOS_STOCKFORGE_WEBHOOK_SECRET,
    ],
    read_models: &[
        "stockforge_connector_status",
        "inventory_stock",
        "inventory_alerts",
        "inventory_orders",
        "inventory_purchase_orders",
    ],
};
