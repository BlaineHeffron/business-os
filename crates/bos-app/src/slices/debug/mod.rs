//! Operator Debug surface: general backend diagnostics projected from existing
//! auditable sources plus the panic diagnostics table.

pub mod routes;
pub mod store;

#[cfg(test)]
mod tests;

use crate::env_registry;
use crate::slices::{RouteSpec, SliceSpec};

pub const SLICE: SliceSpec = SliceSpec {
    id: "debug",
    title: "Debug",
    summary: "Opt-in operator diagnostics over backend-surfaced errors: panics, failed/conflict receipts, outbox delivery failures, and failed LLM calls.",
    routes: &[
        RouteSpec {
            method: "GET",
            path: "/api/debug",
            summary: "Recent backend diagnostics (404 unless BOS_DEBUG_ENABLED is set)",
        },
        RouteSpec {
            method: "POST",
            path: "/api/debug/spawn-agent",
            summary: "Debug-only local monitor proxy: spawn a Codex agent with diagnostic context",
        },
    ],
    tables: &["panic_diagnostics"],
    env_vars: &[
        &env_registry::BOS_DEBUG_AGENT_MONITOR_TOKEN,
        &env_registry::BOS_DEBUG_AGENT_MONITOR_URL,
        &env_registry::BOS_DEBUG_ENABLED,
    ],
    read_models: &["debug_diagnostics"],
};
