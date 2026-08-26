//! Shared enrichment waterfall diagnostics. Participating draft slices own the
//! domain decision, while this slice owns the durable run table and read model.

pub(crate) mod research;
pub(crate) mod research_finalize;
pub mod routes;
pub mod service;
pub mod store;
pub mod web_tier;
pub mod worker;

#[cfg(test)]
mod tests;

use crate::env_registry;
use crate::slices::{RouteSpec, SliceSpec};

pub const SLICE: SliceSpec = SliceSpec {
    id: "enrichment",
    title: "Enrichment diagnostics",
    summary: "Shared field-scoped enrichment waterfall diagnostics: draft slices write durable tier events and proposals through store_core; operators can inspect recent runs by draft or item.",
    routes: &[RouteSpec {
        method: "GET",
        path: "/api/enrichment/runs",
        summary: "Recent enrichment runs (?slice_id=&draft_id= or ?item_id= filters)",
    }],
    tables: &[store::RUN_ENTITY_KIND],
    env_vars: &[
        &env_registry::BOS_AGENTIC_WEB_RESEARCH_COST_BUDGET_MICROS,
        &env_registry::BOS_AGENTIC_WEB_RESEARCH_ENABLED,
        &env_registry::BOS_AGENTIC_WEB_RESEARCH_MAX_CONCURRENT_RUNS,
        &env_registry::BOS_AGENTIC_WEB_RESEARCH_MAX_FETCHED_PAGES,
        &env_registry::BOS_AGENTIC_WEB_RESEARCH_MAX_OUTPUT_TOKENS,
        &env_registry::BOS_AGENTIC_WEB_RESEARCH_MAX_PAGE_BYTES,
        &env_registry::BOS_AGENTIC_WEB_RESEARCH_MAX_RESULTS,
        &env_registry::BOS_AGENTIC_WEB_RESEARCH_MAX_SEARCHES,
        &env_registry::BOS_AGENTIC_WEB_RESEARCH_MAX_STEPS,
        &env_registry::BOS_AGENTIC_WEB_RESEARCH_TIMEOUT_MS,
        &env_registry::BOS_ENRICHMENT_FRESHNESS_ENABLED,
        &env_registry::BOS_ENRICHMENT_FRESHNESS_INTERVAL_SECS,
        &env_registry::BOS_ENRICHMENT_FRESHNESS_MAX_ENRICHMENTS_PER_CYCLE,
        &env_registry::BOS_ENRICHMENT_FRESHNESS_STALE_AFTER_SECS,
    ],
    read_models: &["enrichment_runs"],
};
