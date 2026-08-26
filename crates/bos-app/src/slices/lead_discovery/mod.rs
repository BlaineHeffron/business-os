//! Approved-source lead discovery: configured source list + review-only findings.

pub mod routes;
pub mod service;
pub mod store;
pub mod worker;

#[cfg(test)]
mod tests;

use crate::env_registry;
use crate::slices::{RouteSpec, SliceSpec};

pub const SOURCE_KIND_LEAD_FINDING: &str = "lead_finding";

pub const SLICE: SliceSpec = SliceSpec {
    id: "lead_discovery",
    title: "Lead discovery",
    summary: "Approved-source lead discovery workflow: sources are client-overlay configured, findings are staged for human review with provenance, and accepted findings become normal queue work. No broad scraping or outreach.",
    routes: &[
        RouteSpec {
            method: "GET",
            path: "/api/lead-discovery/status",
            summary: "Configured approved sources, criteria, and pending/disabled state",
        },
        RouteSpec {
            method: "GET",
            path: "/api/lead-discovery/findings",
            summary: "Lead findings newest-first (?status=staged|accepted|rejected)",
        },
        RouteSpec {
            method: "POST",
            path: "/api/lead-discovery/findings",
            summary: "Stage one finding from an approved configured source; provenance is required",
        },
        RouteSpec {
            method: "POST",
            path: "/api/lead-discovery/findings/{finding_id}/action",
            summary: "Accept a finding into the work queue or reject it",
        },
    ],
    tables: &["lead_findings"],
    env_vars: &[
        &env_registry::BOS_LEAD_DISCOVERY_AUTOSCRAPE_ENABLED,
        &env_registry::BOS_LEAD_DISCOVERY_AUTOSCRAPE_INTERVAL_SECS,
        &env_registry::BOS_LEAD_DISCOVERY_AUTOSCRAPE_MAX_FINDINGS_PER_CYCLE,
    ],
    read_models: &["lead_discovery_status", "lead_findings"],
};
