//! AI usage slice: per-call accounting for every typed LLM execution.
//! [`service::execute_recorded`] is the ONE seam the app calls LLMs through —
//! API calls record from the output envelope, harness calls record per
//! attempt via the kernel usage sink. Needed so produce-stage volume is
//! visible (and billable) before it grows.

pub mod routes;
pub mod service;
pub mod store;

#[cfg(test)]
mod tests;

use crate::{
    env_registry,
    slices::{RouteSpec, SliceSpec},
};

pub const SLICE: SliceSpec = SliceSpec {
    id: "ai_usage",
    title: "AI usage log",
    summary: "Per-call usage accounting for typed LLM executions (tokens, latency, cost, outcome) across both the API and harness routes. All LLM call sites flow through this slice's recording seam.",
    routes: &[
        RouteSpec {
            method: "GET",
            path: "/api/ai-usage",
            summary: "Recent usage rows plus all-time and last-24h totals",
        },
        RouteSpec {
            method: "GET",
            path: "/api/llm-settings",
            summary: "Effective typed-LLM routing settings and known task purposes",
        },
        RouteSpec {
            method: "POST",
            path: "/api/llm-settings",
            summary: "Replace global typed-LLM defaults and per-purpose route overrides",
        },
        RouteSpec {
            method: "GET",
            path: "/api/llm-settings/claude-subscription",
            summary: "Read Claude Code subscription availability and connection status",
        },
        RouteSpec {
            method: "POST",
            path: "/api/llm-settings/claude-subscription/start",
            summary: "Start an attended Claude subscription OAuth authorization",
        },
        RouteSpec {
            method: "POST",
            path: "/api/llm-settings/claude-subscription/complete",
            summary: "Submit the one-time Claude authorization code to the waiting CLI",
        },
    ],
    tables: &["ai_usage_log", "llm_route_settings", "llm_route_overrides"],
    env_vars: &[
        &env_registry::BOS_LLM_API_ENDPOINT,
        &env_registry::BOS_LLM_API_KEY,
        &env_registry::BOS_LLM_API_MODEL,
        &env_registry::BOS_LLM_API_PROVIDER,
        &env_registry::BOS_LLM_DEFAULT_BACKEND,
        &env_registry::BOS_LLM_DEFAULT_MODEL,
        &env_registry::BOS_LLM_HARNESS_MODEL,
        &env_registry::BOS_LLM_HARNESS_THINKING_LEVEL,
        &env_registry::BOS_LLM_MAX_TOKENS,
        &env_registry::BOS_LLM_ROUTE_OVERRIDES,
        &env_registry::BOS_LLM_TIMEOUT_MS,
        &env_registry::BOS_STATE_DIR,
    ],
    read_models: &["ai_usage_log", "llm_route_settings"],
};
