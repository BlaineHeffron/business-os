//! Slice registry. Every feature registers a `SliceSpec` here — the single
//! source of truth for routes, migrations, env keys, and per-client enablement.
//! REPO_MAP.md is generated from this registry (`just repo-map`).

use crate::env_registry::EnvVar;

pub mod accounting;
pub mod admin_settings;
pub mod agent_mcp;
pub mod ai_usage;
// Shared helper for slice-owned async kickoffs. Use this instead of hand-rolling
// idempotency replay + duplicate/capacity guards around background threads.
pub(crate) mod async_kickoff;
pub mod calendar_drafts;
pub mod call_inputs;
pub mod claim_drafts;
pub mod client_profile;
pub mod content_drafts;
pub mod content_plans;
pub mod crm_cache;
pub mod crm_drafts;
pub mod crm_record_drafts;
pub mod crm_sales_intent;
pub mod customer_tier_sync;
pub mod data_retention;
pub(crate) mod datetime_input;
pub mod debug;
pub(crate) mod draft_store;
pub mod drive_corpus;
pub mod email_drafts;
pub mod email_triage;
pub mod enrichment;
pub mod follow_up_tasks;
pub mod google_connector;
pub mod grounding;
pub mod home_dashboard;
pub mod instance_diagnostics;
pub mod inventory;
pub mod invoice_drafts;
pub mod lead_discovery;
pub mod ledger_drafts;
pub(crate) mod mutation_context;
pub(crate) mod oauth_state;
pub mod operator_notes;
pub mod operator_users;
pub mod owner_reports;
pub mod packet_proposals;
pub mod quote_workflows;
pub mod release_notes;
pub mod search_console;
pub(crate) mod shipment_refs;
pub mod shopify_sales;
pub mod social_publishing;

pub mod work_queue;

#[derive(Debug, Clone, Copy)]
pub struct RouteSpec {
    pub method: &'static str,
    pub path: &'static str,
    pub summary: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct SliceSpec {
    /// Stable id; client overlays enable slices by this id.
    pub id: &'static str,
    pub title: &'static str,
    pub summary: &'static str,
    pub routes: &'static [RouteSpec],
    /// Tables owned by this slice (documentation + REPO_MAP).
    pub tables: &'static [&'static str],
    pub env_vars: &'static [&'static EnvVar],
    /// Read models this slice serves to the frontend.
    pub read_models: &'static [&'static str],
}

/// All registered slices, sorted by id. The registry test asserts ordering and
/// id uniqueness; code-shape asserts every `slices/<dir>` has an entry.
pub fn registry() -> &'static [SliceSpec] {
    &[
        accounting::SLICE,
        admin_settings::SLICE,
        agent_mcp::SLICE,
        ai_usage::SLICE,
        calendar_drafts::SLICE,
        call_inputs::SLICE,
        claim_drafts::SLICE,
        client_profile::SLICE,
        content_drafts::SLICE,
        content_plans::SLICE,
        crm_cache::SLICE,
        crm_drafts::SLICE,
        crm_record_drafts::SLICE,
        crm_sales_intent::SLICE,
        customer_tier_sync::SLICE,
        data_retention::SLICE,
        debug::SLICE,
        drive_corpus::SLICE,
        email_drafts::SLICE,
        email_triage::SLICE,
        enrichment::SLICE,
        follow_up_tasks::SLICE,
        google_connector::SLICE,
        home_dashboard::SLICE,
        instance_diagnostics::SLICE,
        inventory::SLICE,
        invoice_drafts::SLICE,
        lead_discovery::SLICE,
        ledger_drafts::SLICE,
        operator_notes::SLICE,
        operator_users::SLICE,
        owner_reports::SLICE,
        packet_proposals::SLICE,
        quote_workflows::SLICE,
        release_notes::SLICE,
        search_console::SLICE,
        shopify_sales::SLICE,
        social_publishing::SLICE,
        work_queue::SLICE,
    ]
}

/// Markdown section per slice, for REPO_MAP generation.
pub fn markdown() -> String {
    let slices = registry();
    if slices.is_empty() {
        return "_No slices registered yet._\n".to_string();
    }
    let mut out = String::new();
    for slice in slices {
        out.push_str(&format!(
            "### `{}` — {}\n\n{}\n\n",
            slice.id, slice.title, slice.summary
        ));
        if !slice.routes.is_empty() {
            out.push_str("| Method | Path | Summary |\n| --- | --- | --- |\n");
            for route in slice.routes {
                out.push_str(&format!(
                    "| {} | `{}` | {} |\n",
                    route.method, route.path, route.summary
                ));
            }
            out.push('\n');
        }
        if !slice.tables.is_empty() {
            out.push_str(&format!("Tables: {}\n\n", slice.tables.join(", ")));
        }
        if !slice.read_models.is_empty() {
            out.push_str(&format!(
                "Read models: {}\n\n",
                slice.read_models.join(", ")
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_sorted_and_unique() {
        let ids: Vec<&str> = registry().iter().map(|s| s.id).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            ids, sorted,
            "slice registry must be sorted by id, no duplicates"
        );
    }
}
