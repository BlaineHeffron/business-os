//! Claim packets (port #6, packet kind `claim_draft`): provider-neutral
//! shipping-damage packets on the ProduceFlavor spine. The claims pump polls
//! the currently configured damage source into the work queue (source systems
//! store the evidence — BusinessOS assembles); produce builds a deterministic
//! packet (shipment/order/evidence from local caches, claim amount grounded)
//! with ONE narrative transform; required evidence roles gate approval-
//! readiness. Approval stages a Gmail draft to the filing mailbox
//! (HUMAN-CLAIM) and creates the claim-tracking follow-up task.

pub mod routes;
pub mod service;
pub mod store;
pub mod worker;

#[cfg(test)]
mod tests;

use crate::env_registry;
use crate::slices::{RouteSpec, SliceSpec};

/// Category id stamped on damage work items (policy-overridable per client).
pub const DAMAGE_CATEGORY: &str = "shipping_damage";

pub const SLICE: SliceSpec = SliceSpec {
    id: "claim_drafts",
    title: "Shipping damage claims",
    summary: "Shipping damage events become queue items (claims pump, request-budgeted, env-gated OFF); produce assembles a deterministic provider-neutral claim packet from local caches (order ref, packing proof, tracking ref, damage photos — completeness gates approval) with one grounded narrative transform; approval stages a gated Gmail draft for manual provider filing plus a claim-tracking follow-up task.",
    routes: &[
        RouteSpec {
            method: "GET",
            path: "/api/claim-drafts",
            summary: "Claim drafts, newest first (?item_id= filters)",
        },
        RouteSpec {
            method: "POST",
            path: "/api/claim-drafts/produce",
            summary: "Produce a claim packet for an accepted damage item (202, panel polls)",
        },
        RouteSpec {
            method: "POST",
            path: "/api/claim-drafts/{draft_id}/action",
            summary: "Approve (packet must be complete; stages the Gmail draft + tracking task) or reject",
        },
        RouteSpec {
            method: "POST",
            path: "/api/claim-drafts/{draft_id}/update",
            summary: "Edit a staged draft's narrative/item/amount (shipment + evidence immutable)",
        },
        RouteSpec {
            method: "POST",
            path: "/api/claim-drafts/sync",
            summary: "Kick one claims sync cycle (202; 409 while syncing/cooling down)",
        },
    ],
    tables: &[
        "stockforge_damage_snapshots",
        "claims_sync_cursors",
        "claim_drafts",
    ],
    env_vars: &[
        &env_registry::BOS_CLAIMS_MAX_REQUESTS_PER_CYCLE,
        &env_registry::BOS_CLAIMS_SYNC_ENABLED,
        &env_registry::BOS_CLAIMS_SYNC_INTERVAL_SECS,
        &env_registry::BOS_CLAIM_DRAFT_TO_ADDR,
    ],
    read_models: &["claim_drafts"],
};
