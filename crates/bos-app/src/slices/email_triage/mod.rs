//! Email triage slice: deterministic rule-based classification of inbound
//! email. Rules are operator-managed (CRUD + dry-run); the resolver pins a
//! category when a rule matches, else the fallback applies. AI classification,
//! when it arrives, runs AFTER these rules, never instead of them.

pub mod catalog;
pub mod facts;
pub mod legacy;
pub mod routes;
pub mod service;
pub mod store;
pub mod subjects;
pub mod worker;

#[cfg(test)]
mod tests;

use crate::env_registry;
use crate::slices::{RouteSpec, SliceSpec};

pub const SLICE: SliceSpec = SliceSpec {
    id: "email_triage",
    title: "Email triage rules",
    summary: "Operator-managed deterministic rules that classify inbound email into input categories; dry-run endpoint for testing rules against sample or live messages.",
    routes: &[
        RouteSpec {
            method: "GET",
            path: "/api/email-triage/rules",
            summary: "List active rules",
        },
        RouteSpec {
            method: "POST",
            path: "/api/email-triage/rules",
            summary: "Create or update a rule (idempotent, revision-checked)",
        },
        RouteSpec {
            method: "POST",
            path: "/api/email-triage/rules/{rule_id}/action",
            summary: "Enable, disable, or delete a rule",
        },
        RouteSpec {
            method: "POST",
            path: "/api/email-triage/dry-run",
            summary: "Classify sample messages against current + proposed rules",
        },
        RouteSpec {
            method: "GET",
            path: "/api/email-triage/condition-catalog",
            summary: "Catalog of supported email triage rule conditions",
        },
        RouteSpec {
            method: "GET",
            path: "/api/email-triage/inbox",
            summary: "Recent ingested + classified inbound messages, optionally filtered by Gmail category, label, mailbox, and limit",
        },
        RouteSpec {
            method: "GET",
            path: "/api/email-triage/inbox/options",
            summary: "Available inbox Gmail categories, labels, mailboxes, and configured dashboard defaults",
        },
        RouteSpec {
            method: "GET",
            path: "/api/email-triage/inbox/settings",
            summary: "Operator-configurable inbox Gmail tab visibility settings",
        },
        RouteSpec {
            method: "POST",
            path: "/api/email-triage/inbox/settings",
            summary: "Replace inbox Gmail tab visibility settings",
        },
        RouteSpec {
            method: "POST",
            path: "/api/email-triage/inbox/{message_id}/follow-up",
            summary: "Manually add a follow-up task packet kind for one inbound email",
        },
        RouteSpec {
            method: "POST",
            path: "/api/email-triage/inbox/{message_id}/trash",
            summary: "Explicitly dismiss local work and enqueue a gated Gmail Move to Trash effect",
        },
        RouteSpec {
            method: "POST",
            path: "/api/email-triage/inbox/{message_id}/attachments/{attachment_id}/evidence",
            summary: "Stage one inbound email attachment into a per-session agent evidence directory",
        },
        RouteSpec {
            method: "POST",
            path: "/api/email-triage/reclassify",
            summary: "Re-run rules over all stored mail + backfill work items",
        },
        RouteSpec {
            method: "POST",
            path: "/api/email-triage/ai-retriage-reset",
            summary: "Clear AI-triage verdicts (per message, stale, or all) so the pump re-examines old mail",
        },
        RouteSpec {
            method: "GET",
            path: "/api/email-triage/categories",
            summary: "Operator-defined input categories (lazy-seeds defaults)",
        },
        RouteSpec {
            method: "POST",
            path: "/api/email-triage/categories",
            summary: "Create or update a category",
        },
        RouteSpec {
            method: "POST",
            path: "/api/email-triage/categories/{category_id}/delete",
            summary: "Delete a category (refused while rules pin it)",
        },
    ],
    tables: &[
        "email_triage_rules",
        "email_inbound_messages",
        "email_inbound_enrichments",
        "email_triage_categories",
        "email_triage_fact_cache",
        "email_triage_inbox_settings",
        "agent_evidence_files",
        "gmail_ingest_cursors",
    ],
    env_vars: &[
        &env_registry::BOS_GMAIL_INGEST_ENABLED,
        &env_registry::BOS_GMAIL_INGEST_INTERVAL_SECS,
        &env_registry::BOS_GMAIL_INGEST_QUERY,
        &env_registry::BOS_GMAIL_OAUTH_CLIENT_ID,
        &env_registry::BOS_GMAIL_OAUTH_CLIENT_SECRET,
        &env_registry::BOS_GMAIL_OAUTH_REFRESH_TOKEN,
        &env_registry::BOS_GMAIL_OAUTH_SCOPES,
        &env_registry::BOS_GMAIL_TRASH_ENABLED,
        &env_registry::BOS_AI_TRIAGE_ENABLED,
        &env_registry::BOS_AI_TRIAGE_MAX_LLM_CALLS_PER_CYCLE,
        &env_registry::BOS_AI_TRIAGE_MIN_CONFIDENCE,
        &env_registry::BOS_AI_TRIAGE_PACKET_PROPOSALS_ENABLED,
        &env_registry::BOS_EMAIL_TRIAGE_FACT_CACHE_TTL_SECS,
        &env_registry::BOS_EMAIL_TRIAGE_FACT_PROVIDER_BUDGET_PER_MESSAGE,
        &env_registry::BOS_CRM_CONTEXT_NEUTRAL_SENDER_DOMAINS,
        &env_registry::BOS_CRM_PROVIDER,
        &env_registry::BOS_HUBSPOT_ACCESS_TOKEN,
        &env_registry::BOS_HUBSPOT_WRITE_ENABLED,
        &env_registry::BOS_ESPOCRM_BASE_URL,
        &env_registry::BOS_ESPOCRM_API_KEY,
        &env_registry::BOS_ESPOCRM_WRITE_ENABLED,
        &env_registry::BOS_AGENT_EVIDENCE_CLEANUP_ENABLED,
        &env_registry::BOS_AGENT_EVIDENCE_CLEANUP_INTERVAL_SECS,
        &env_registry::BOS_AGENT_EVIDENCE_MAX_BYTES,
        &env_registry::BOS_AGENT_EVIDENCE_RETENTION_DAYS,
        &env_registry::BOS_AGENT_EVIDENCE_ROOT_DIR,
    ],
    read_models: &[
        "email_triage_rules_list",
        "email_triage_inbox",
        "email_triage_inbox_options",
        "email_triage_inbox_settings",
        "email_triage_categories",
    ],
};
