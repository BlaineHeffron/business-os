//! Optional BusinessOS MCP surface for AgentMonitor/Fleet agents.
//!
//! This is an operator-controlled bridge into existing BOS APIs. It is off by
//! default, uses normal operator authentication, and exposes only read/stage/
//! note-ingest tools. Provider writes still require the existing human approval
//! and outbox gates.

pub mod routes;
pub mod service;
pub mod store;

#[cfg(test)]
mod tests;

use crate::env_registry;
use crate::slices::{RouteSpec, SliceSpec};

pub const SLICE: SliceSpec = SliceSpec {
    id: "agent_mcp",
    title: "Agent MCP",
    summary: "Optional MCP endpoint for AgentMonitor/Fleet agents explicitly launched with BusinessOS context. Tools are operator-authenticated and limited to safe reads, note/work-queue artifact creation, and staged draft production; no tool sends email or writes to providers.",
    routes: &[
        RouteSpec {
            method: "GET",
            path: "/api/agent-mcp",
            summary: "Discover the optional BusinessOS MCP server and its safe tool posture",
        },
        RouteSpec {
            method: "POST",
            path: "/api/agent-mcp",
            summary: "Stateless streamable-HTTP MCP endpoint for explicitly BOS-contexted agents",
        },
    ],
    tables: &[],
    env_vars: &[&env_registry::BOS_AGENT_MCP_ENABLED],
    read_models: &[],
};
