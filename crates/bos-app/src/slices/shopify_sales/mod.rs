//! Shopify sales cached views. This slice is read-only: it syncs recent
//! Shopify orders/customers into local snapshots so operators and future AI
//! grounding tools can query sales context offline. Customer-tier writes stay
//! isolated in `customer_tier_sync`.

pub mod routes;
pub mod service;
pub mod store;
pub mod worker;

#[cfg(test)]
mod tests;

use crate::env_registry;
use crate::slices::{RouteSpec, SliceSpec};

pub const SLICE: SliceSpec = SliceSpec {
    id: "shopify_sales",
    title: "Shopify sales cached views",
    summary: "Read-only Shopify connector: env-configured Admin API token, a request-budgeted sync pump into local order/customer snapshot caches, and sales views served from sqlite only — the UI and grounding tools never hit Shopify directly.",
    routes: &[
        RouteSpec {
            method: "GET",
            path: "/api/shopify-sales/status",
            summary: "Connector status (configured, shop domain, has synced); blocked_reason when env is missing",
        },
        RouteSpec {
            method: "POST",
            path: "/api/shopify-sales/sync",
            summary: "Kick one budgeted sync cycle (202; 409 while one runs or during the cooldown)",
        },
        RouteSpec {
            method: "GET",
            path: "/api/shopify-sales/orders",
            summary: "Cached recent orders or ?email= customer order history; dollar fields redact for limited operators",
        },
        RouteSpec {
            method: "GET",
            path: "/api/shopify-sales/customers",
            summary: "Cached customer lookup by email; dollar fields redact for limited operators",
        },
    ],
    tables: &[
        "shopify_order_snapshots",
        "shopify_customer_snapshots",
        "shopify_sales_sync_state",
    ],
    env_vars: &[
        &env_registry::BOS_SHOPIFY_ACCESS_TOKEN,
        &env_registry::BOS_SHOPIFY_API_VERSION,
        &env_registry::BOS_SHOPIFY_CLIENT_ID,
        &env_registry::BOS_SHOPIFY_CLIENT_SECRET,
        &env_registry::BOS_SHOPIFY_READ_SYNC_ENABLED,
        &env_registry::BOS_SHOPIFY_READ_SYNC_INTERVAL_SECS,
        &env_registry::BOS_SHOPIFY_READ_SYNC_MAX_ORDERS_PER_CYCLE,
        &env_registry::BOS_SHOPIFY_SALES_VISIBILITY_POLICY,
        &env_registry::BOS_SHOPIFY_SHOP_DOMAIN,
    ],
    read_models: &[
        "shopify_sales_status",
        "shopify_sales_orders",
        "shopify_sales_customers",
    ],
};
