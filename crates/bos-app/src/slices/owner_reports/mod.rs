//! Owner reporting digest (port #7, W16): a deterministic weekly +
//! month-to-date digest assembled entirely from the LOCAL caches — sales and
//! margin from the accounting snapshots (the §5 reporting surface; money is
//! READ, never AI-generated), call volume from a configured email-triage category,
//! follow-up completion from the tasks watchdog, order control from the
//! Stockforge order board, damage/claims from the damage snapshots + claim
//! drafts. ONE bounded narration transform writes prose over those metrics;
//! any dollar amount in the prose must literally appear in the input.
//! Deterministic period ids make regeneration an idempotent upsert; the
//! optional email delivery stages a gated Gmail draft to the owners.

pub mod routes;
pub mod service;
pub mod store;
pub mod worker;

#[cfg(test)]
mod tests;

use crate::env_registry;
use crate::slices::{RouteSpec, SliceSpec};

pub const SLICE: SliceSpec = SliceSpec {
    id: "owner_reports",
    title: "Owner reporting digest",
    summary: "Deterministic weekly + month-to-date owner digest assembled from cached operational data plus read-only HubSpot deal reporting when configured; generation is env-gated, scheduled delivery is separately gated/configured by overlay/env (recipients, weekly weekday, MTD day, metric ordering), and optional email delivery stages a gated Gmail draft. Calls are configurable email-derived metrics; site traffic remains a pending data-source decision.",
    routes: &[
        RouteSpec {
            method: "GET",
            path: "/api/owner-reports",
            summary: "Digest reports, newest period first (?period=weekly|mtd filters)",
        },
        RouteSpec {
            method: "POST",
            path: "/api/owner-reports/generate",
            summary: "Regenerate the current weekly + MTD digests now (202; 409 while generating/cooling down)",
        },
        RouteSpec {
            method: "POST",
            path: "/api/owner-reports/{report_id}/email",
            summary: "Stage the digest as a gated Gmail draft to configured owner-report recipients (422 when unset)",
        },
    ],
    tables: &["owner_reports"],
    env_vars: &[
        &env_registry::BOS_HUBSPOT_ACCESS_TOKEN,
        &env_registry::BOS_HUBSPOT_DEALS_CLOSED_DATE_PROPERTY,
        &env_registry::BOS_HUBSPOT_DEALS_LOST_STAGE_IDS,
        &env_registry::BOS_HUBSPOT_DEALS_OPEN_STAGE_IDS,
        &env_registry::BOS_HUBSPOT_DEALS_PIPELINE_ID,
        &env_registry::BOS_HUBSPOT_DEALS_SEGMENT_PROPERTIES,
        &env_registry::BOS_HUBSPOT_DEALS_STARTED_DATE_PROPERTY,
        &env_registry::BOS_HUBSPOT_DEALS_WON_STAGE_IDS,
        &env_registry::BOS_CRM_PROVIDER,
        &env_registry::BOS_REPORT_DIGEST_ENABLED,
        &env_registry::BOS_REPORT_DIGEST_INTERVAL_SECS,
        &env_registry::BOS_REPORT_DIGEST_DELIVERY_ENABLED,
        &env_registry::BOS_REPORT_DIGEST_TO_ADDR,
        &env_registry::BOS_REPORT_DIGEST_WEEKLY_WEEKDAY,
        &env_registry::BOS_REPORT_DIGEST_MTD_DAY,
        &env_registry::BOS_REPORT_DIGEST_METRICS,
        &env_registry::BOS_REPORT_DIGEST_REDACT_FINANCIALS_FOR,
        &env_registry::BOS_REPORT_DIGEST_SUBJECT_PREFIX,
        &env_registry::BOS_OWNER_REPORT_CALL_VOLUME_CATEGORY_ID,
        &env_registry::BOS_OWNER_REPORT_CALL_VOLUME_GMAIL_LABEL,
        &env_registry::BOS_OWNER_REPORT_CALL_VOLUME_GMAIL_QUERY,
        &env_registry::BOS_OWNER_REPORT_CALL_VOLUME_LABEL,
        &env_registry::BOS_OWNER_REPORT_CALL_VOLUME_SOURCE_LABEL,
        &env_registry::BOS_OWNER_REPORT_ALLOWED_OPERATOR_USER_IDS,
    ],
    read_models: &["owner_reports"],
};
