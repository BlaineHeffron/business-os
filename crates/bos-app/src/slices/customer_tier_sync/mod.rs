//! Customer tier sync: build an operator-reviewed plan from cached QBO
//! customer tiers, then push the approved tier markers to Shopify through the
//! outbox. QBO is source of truth; Shopify writes are dry-run until gated.

pub mod routes;
pub mod service;
pub mod store;

#[cfg(test)]
mod tests;

use crate::env_registry;
use crate::slices::{RouteSpec, SliceSpec};

pub const SLICE: SliceSpec = SliceSpec {
    id: "customer_tier_sync",
    title: "Customer tier sync",
    summary: "Generic gated/dry-run QBO-to-Shopify customer tier sync: previews read cached QBO customer tiers, operator approval enqueues a Shopify outbox job, and live writes require BOS_SHOPIFY_WRITE_ENABLED. Shopify targets can copy the QBO tier value into a customer metafield/tag, with explicit mapping overrides from config/env.",
    routes: &[
        RouteSpec {
            method: "GET",
            path: "/api/customer-tier-sync/runs",
            summary: "Recent staged/approved customer-tier sync runs with outbox delivery state",
        },
        RouteSpec {
            method: "POST",
            path: "/api/customer-tier-sync/preview",
            summary: "Build a dry-run sync plan from cached QBO customer tiers and configured Shopify targets",
        },
        RouteSpec {
            method: "POST",
            path: "/api/customer-tier-sync/runs/{run_id}/approve",
            summary: "Approve a staged run, enqueueing the gated Shopify customer-tier write outbox job",
        },
        RouteSpec {
            method: "POST",
            path: "/api/customer-tier-sync/runs/{run_id}/reject",
            summary: "Reject a staged sync run without enqueueing provider writes",
        },
    ],
    tables: &["customer_tier_sync_runs", "outbox_jobs"],
    env_vars: &[
        &env_registry::BOS_SHOPIFY_ACCESS_TOKEN,
        &env_registry::BOS_SHOPIFY_API_VERSION,
        &env_registry::BOS_SHOPIFY_CLIENT_ID,
        &env_registry::BOS_SHOPIFY_CLIENT_SECRET,
        &env_registry::BOS_SHOPIFY_SHOP_DOMAIN,
        &env_registry::BOS_SHOPIFY_TIER_MAPPING_JSON,
        &env_registry::BOS_SHOPIFY_WRITE_ENABLED,
    ],
    read_models: &["customer_tier_sync_runs"],
};
