//! Smart draft packet proposals: a planner/filler run that decides and drafts
//! packet kinds, then delegates all draft persistence to existing produce
//! flavors.

pub mod routes;
pub mod service;
pub mod store;

#[cfg(test)]
mod tests;

use crate::slices::{RouteSpec, SliceSpec};

pub const SLICE: SliceSpec = SliceSpec {
    id: "packet_proposals",
    title: "Packet proposals",
    summary: "Smart draft runs one bounded typed AI proposal over a normalized source, records the run, accepts the queue item, and stages existing packet-kind drafts through their normal gates.",
    routes: &[
        RouteSpec {
            method: "POST",
            path: "/api/packet-proposals/smart-draft",
            summary:
                "Create or accept a work item for a source and stage Smart draft packet proposals",
        },
        RouteSpec {
            method: "POST",
            path: "/api/packet-proposals/smart-draft/source-state",
            summary: "Read Smart draft source state and current queue item revision",
        },
    ],
    tables: &["packet_proposal_runs", "packet_proposal_run_evidence"],
    env_vars: &[
        &crate::env_registry::BOS_PACKET_PROPOSAL_RUNNING_STALE_AFTER_MS,
        &crate::env_registry::BOS_PACKET_PROPOSAL_TOOL_LOOP_ENABLED,
    ],
    read_models: &["packet_proposal_runs"],
};
