//! Content plans: local planning rows that hand off to the existing Queue →
//! Produce → content_drafts spine. This slice owns planning state and
//! advisory overlap checks; drafting and approval remain in content_drafts.

pub mod routes;
pub mod service;
pub mod store;

#[cfg(test)]
mod tests;

use crate::slices::{RouteSpec, SliceSpec};

pub const SOURCE_KIND_CONTENT_PLAN_ITEM: &str = "content_plan_item";

pub const SLICE: SliceSpec = SliceSpec {
    id: "content_plans",
    title: "Content plans",
    summary: "Local content plan items with deterministic duplicate/cannibalization warnings, manual published inventory, and a one-transaction handoff into the normal work_queue/content_draft produce spine.",
    routes: &[
        RouteSpec {
            method: "GET",
            path: "/api/content-plans/items",
            summary: "Content plan items with derived draft state",
        },
        RouteSpec {
            method: "POST",
            path: "/api/content-plans/items",
            summary: "Create a planned content item and run advisory overlap checks",
        },
        RouteSpec {
            method: "POST",
            path: "/api/content-plans/items/{plan_item_id}/update",
            summary: "Update a planned content item and rerun advisory overlap checks",
        },
        RouteSpec {
            method: "POST",
            path: "/api/content-plans/items/{plan_item_id}/queue",
            summary: "Queue a planned item as a normal content_draft work item",
        },
        RouteSpec {
            method: "GET",
            path: "/api/content-plans/items/{plan_item_id}/campaign",
            summary: "Unified campaign workspace over the plan, exact article/social revisions, destinations, and publication dependency",
        },
        RouteSpec {
            method: "POST",
            path: "/api/content-plans/items/{plan_item_id}/generate",
            summary: "Operator-accept and generate the grounded article through the normal content_drafts producer",
        },
        RouteSpec {
            method: "POST",
            path: "/api/content-plans/items/{plan_item_id}/publish-campaign",
            summary: "Approve the exact article/social/destination snapshot and enqueue blog-first publication",
        },
        RouteSpec {
            method: "POST",
            path: "/api/content-plans/items/{plan_item_id}/check",
            summary: "Rerun advisory duplicate/cannibalization checks",
        },
        RouteSpec {
            method: "POST",
            path: "/api/content-plans/items/{plan_item_id}/mark-published",
            summary: "Mark a planned or queued item as manually published and add it to inventory",
        },
        RouteSpec {
            method: "GET",
            path: "/api/content-plans/inventory",
            summary: "List local content inventory rows",
        },
        RouteSpec {
            method: "GET",
            path: "/api/content-plans/draft-overlap/{draft_id}",
            summary: "Advisory overlap warnings for a staged content draft",
        },
        RouteSpec {
            method: "POST",
            path: "/api/content-plans/inventory",
            summary: "Add a manual published inventory row",
        },
        RouteSpec {
            method: "POST",
            path: "/api/content-plans/inventory/refresh",
            summary: "Refresh local content inventory from cached local sources",
        },
        RouteSpec {
            method: "POST",
            path: "/api/content-plans/inventory/{inventory_id}/archive",
            summary: "Archive a local content inventory row",
        },
    ],
    tables: &[
        "content_plan_items",
        "content_inventory_items",
        "content_inventory_fts",
        "content_campaign_publications",
    ],
    env_vars: &[],
    read_models: &[
        "content_plan_items",
        "content_inventory_items",
        "content_campaign_workspace",
    ],
};
