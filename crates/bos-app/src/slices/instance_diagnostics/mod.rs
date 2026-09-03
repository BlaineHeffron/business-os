//! Instance diagnostics slice: the structured health surface the support hub
//! polls. `/readyz` is mounted in the core router (liveness must outlive
//! slice enablement); `/api/diagnostics/health` is the operator-gated full
//! signal. Read-only over existing tables — no tables, no workers, no env.

pub mod routes;
pub mod service;
pub mod store;

#[cfg(test)]
mod tests;

use crate::slices::{RouteSpec, SliceSpec};

pub const SLICE: SliceSpec = SliceSpec {
    id: "instance_diagnostics",
    title: "Instance diagnostics",
    summary: "Structured health for cross-instance support monitoring: identity, pump guard states, and error rollups computed from receipts, outbox_jobs, and ai_usage_log. Read-only; the support hub (agent-monitor) polls these endpoints.",
    routes: &[
        RouteSpec {
            method: "GET",
            path: "/livez",
            summary: "Unauthenticated liveness (`ok` text/plain). Aliases /health and /healthz (and trailing slashes, HEAD) are registered in http.rs, not as extra RouteSpecs",
        },
        RouteSpec {
            method: "GET",
            path: "/readyz",
            summary: "Unauthenticated structured liveness JSON (mounted core, including /readyz/; serves even when the slice is disabled)",
        },
        RouteSpec {
            method: "GET",
            path: "/api/diagnostics/health",
            summary: "Operator-gated health: identity, pump statuses, outbox backlog, windowed error rollups, enabled slices",
        },
    ],
    tables: &[],
    env_vars: &[&crate::env_registry::BOS_BUILD_SHA],
    read_models: &[],
};
