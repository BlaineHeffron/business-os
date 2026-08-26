//! CRM record-create drafts (packet kind `crm_record_create`): a note that
//! references a company and/or people who are NOT yet in the CRM becomes one or
//! more drafts proposing the missing records, on the ProduceFlavor spine. The
//! produce stage grounds the names (an invented name is dropped) and runs a
//! bounded LIVE CRM search to propose ONLY what is missing. Each approval runs
//! one deterministic ensure-chain write behind the configured provider's write
//! gate: EspoCRM (account → contact) or HubSpot (company → contact + default
//! association).

pub mod routes;
pub mod service;
pub mod store;

#[cfg(test)]
mod tests;

use crate::env_registry;
use crate::slices::{RouteSpec, SliceSpec};

pub const SLICE: SliceSpec = SliceSpec {
    id: "crm_record_drafts",
    title: "CRM record-create drafts",
    summary: "Produce + approval vertical for crm_record_create work items: a typed fill extracts the company/contacts a note references (names grounded — an invented name is dropped), a bounded LIVE CRM search decides which already exist, and one draft per missing contact proposes ONLY the missing records. Each approval enqueues the create-records outbox job for the configured CRM provider: EspoCRM creates Account then Contact, HubSpot creates Company then Contact with the default association. Writes are idempotent on redelivery and dry-run until their provider write gate is enabled.",
    routes: &[
        RouteSpec {
            method: "GET",
            path: "/api/crm-record-drafts",
            summary: "Drafts newest-first (?item_id= scopes to one work item); includes outbox delivery state",
        },
        RouteSpec {
            method: "POST",
            path: "/api/crm-record-drafts/produce",
            summary: "Produce record-create draft(s) from an accepted work item (typed fill + live CRM search; returns one existing active draft when any exists)",
        },
        RouteSpec {
            method: "POST",
            path: "/api/crm-record-drafts/{draft_id}/action",
            summary: "Approve (≥1 record proposed with a name; stages the configured CRM ensure-chain write) or reject",
        },
        RouteSpec {
            method: "POST",
            path: "/api/crm-record-drafts/{draft_id}/update",
            summary: "Edit a staged draft (which records to create + their fields; names re-validated)",
        },
        RouteSpec {
            method: "POST",
            path: "/api/crm-record-drafts/{draft_id}/enrich",
            summary: "Kick off operator-directed web enrichment or gated research mode for a staged record-create draft; returns the enrichment run id immediately",
        },
    ],
    tables: &["crm_record_drafts"],
    env_vars: &[
        &env_registry::BOS_CRM_PROVIDER,
        &env_registry::BOS_ESPOCRM_BASE_URL,
        &env_registry::BOS_ESPOCRM_API_KEY,
        &env_registry::BOS_ESPOCRM_WRITE_ENABLED,
        &env_registry::BOS_HUBSPOT_ACCESS_TOKEN,
        &env_registry::BOS_HUBSPOT_WRITE_ENABLED,
        &env_registry::BOS_WEB_ENRICHMENT_ENABLED,
        &env_registry::BOS_WEB_SEARCH_ENRICHMENT_API_KEY,
        &env_registry::BOS_WEB_SEARCH_ENRICHMENT_COST_BUDGET_MICROS,
        &env_registry::BOS_WEB_SEARCH_ENRICHMENT_ENABLED,
        &env_registry::BOS_WEB_SEARCH_ENRICHMENT_ENDPOINT,
        &env_registry::BOS_WEB_SEARCH_ENRICHMENT_FALLBACK_ENDPOINT,
        &env_registry::BOS_WEB_SEARCH_ENRICHMENT_MAX_FETCHED_PAGES,
        &env_registry::BOS_WEB_SEARCH_ENRICHMENT_MAX_QUERIES,
        &env_registry::BOS_WEB_SEARCH_ENRICHMENT_MAX_RESULTS,
        &env_registry::BOS_WEB_SEARCH_ENRICHMENT_PROVIDER,
        &env_registry::BOS_WEB_SEARCH_ENRICHMENT_TIMEOUT_MS,
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
    ],
    read_models: &["crm_record_drafts"],
};
