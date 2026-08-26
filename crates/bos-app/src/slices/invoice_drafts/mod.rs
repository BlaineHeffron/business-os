//! Invoice drafts (packet kind `invoice_draft`): bill-this-work items from
//! notes/emails become provider invoice drafts on the ProduceFlavor spine.
//! Avery's own invoicing vertical (not a Demo workflow): the typed fill
//! extracts customer + line items with every amount provenance-grounded
//! (money is never invented; line/total math is recomputed server-side),
//! the operator edits/approves in the Queue, and approval enqueues the
//! create-invoice-draft outbox job for BOS_ACCOUNTING_PROVIDER's arm —
//! Invoice Ninja (the chosen production path: ACH/check payments, gated by
//! BOS_INVOICE_NINJA_WRITE_ENABLED) or Stripe (gated by
//! BOS_STRIPE_WRITE_ENABLED). Either way the invoice stays a provider
//! DRAFT — reviewing and sending it is a human action in the provider's UI.

pub mod routes;
pub mod service;
pub mod store;

#[cfg(test)]
mod tests;

use crate::env_registry;
use crate::slices::{RouteSpec, SliceSpec};

pub const SLICE: SliceSpec = SliceSpec {
    id: "invoice_drafts",
    title: "Invoice drafts",
    summary: "Produce + approval vertical for invoice_draft work items (suggested from notes/emails describing billable work): typed fill stages an invoice draft — customer, line items with provenance-grounded amounts (totals recomputed server-side, never model math); approval (requires a customer email) enqueues the create-invoice-draft outbox job for the configured provider — Invoice Ninja (find-or-create client, DRAFT invoice by unique number, dry-run until BOS_INVOICE_NINJA_WRITE_ENABLED) or Stripe (find-or-create customer, invoice with auto_advance=false, dry-run until BOS_STRIPE_WRITE_ENABLED). Either way the invoice stays a provider DRAFT — review and send stay human in the provider's UI.",
    routes: &[
        RouteSpec {
            method: "GET",
            path: "/api/invoice-drafts",
            summary: "Drafts newest-first (?item_id= scopes to one work item); includes outbox delivery state",
        },
        RouteSpec {
            method: "POST",
            path: "/api/invoice-drafts/produce",
            summary: "Produce an invoice draft from an accepted work item (typed fill; returns the existing active draft when one exists)",
        },
        RouteSpec {
            method: "POST",
            path: "/api/invoice-drafts/{draft_id}/action",
            summary: "Approve (requires customer email + non-zero total; stages the provider draft-invoice write) or reject",
        },
        RouteSpec {
            method: "POST",
            path: "/api/invoice-drafts/{draft_id}/update",
            summary: "Edit a staged draft (customer/email/due date/memo/line items; totals recomputed server-side)",
        },
        RouteSpec {
            method: "POST",
            path: "/api/invoice-drafts/{draft_id}/enrich",
            summary: "Kick off operator-directed customer web enrichment for a staged invoice draft; returns the enrichment run id immediately",
        },
        RouteSpec {
            method: "GET",
            path: "/api/invoice-drafts/settings",
            summary: "Invoicing defaults (default due-date term)",
        },
        RouteSpec {
            method: "POST",
            path: "/api/invoice-drafts/settings",
            summary: "Update invoicing defaults — default due-date Net N, applied at produce when the source states no explicit date or term (1..=365 days; revision-checked)",
        },
    ],
    tables: &["invoice_drafts", "invoice_settings"],
    env_vars: &[
        &env_registry::BOS_INVOICE_NINJA_BASE_URL,
        &env_registry::BOS_INVOICE_NINJA_API_TOKEN,
        &env_registry::BOS_INVOICE_NINJA_WRITE_ENABLED,
        &env_registry::BOS_STRIPE_SECRET_KEY,
        &env_registry::BOS_STRIPE_WRITE_ENABLED,
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
    ],
    read_models: &["invoice_drafts", "invoice_settings"],
};
