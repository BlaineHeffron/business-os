//! Quote workflow slice: the first bounded agentic workflow implementation.
//! The workflow runner and Trace recorder intentionally stay slice-local until
//! a second workflow proves the abstraction boundary.

pub mod profiles;
pub mod routes;
pub mod service;
pub mod store;

#[cfg(test)]
mod tests;

use crate::slices::{RouteSpec, SliceSpec};

pub const SLICE: SliceSpec = SliceSpec {
    id: "quote_workflows",
    title: "Quote workflows",
    summary: "Bounded quote-builder workflow with slice-local Trace persistence: start a run, inspect its causal trace, and approve or reject the staged quote draft. Approval enqueues the provider-write outbox job with the workflow run id as correlation id.",
    routes: &[
        RouteSpec {
            method: "POST",
            path: "/api/quote-workflows/run",
            summary: "Start the quote_builder.v1 workflow and stage a quote draft when grounding and policy checks pass",
        },
        RouteSpec {
            method: "GET",
            path: "/api/quote-workflows/{run_id}",
            summary: "Inspect a workflow run with steps, by-correlation receipts, outbox jobs, and staged quote draft",
        },
        RouteSpec {
            method: "POST",
            path: "/api/quote-drafts/{draft_id}/action",
            summary: "Approve (enqueue quote provider draft) or reject a staged quote draft",
        },
    ],
    tables: &["workflow_runs", "workflow_steps", "quote_drafts", "outbox_jobs"],
    env_vars: &[],
    read_models: &["quote_workflow_inspection"],
};
