//! Ledger entry drafts: record received payments (e.g. Stripe receipt
//! emails) into the accounting provider through the produce → approve →
//! outbox vertical. Money is grounded — the amount must carry a literal
//! provenance quote — and each provider arm is write-gated (dry-run by
//! default): Invoice Ninja behind BOS_INVOICE_NINJA_WRITE_ENABLED
//! (record_receipt ensure-chain), QBO behind BOS_QBO_WRITE_ENABLED
//! (record_payment against the snapshot invoice whose open balance equals
//! the amount — the agent_monitor amount-must-match guard, port #3).

pub mod routes;
pub mod service;
pub mod store;

#[cfg(test)]
mod tests;

use crate::env_registry;
use crate::slices::{RouteSpec, SliceSpec};

pub const SLICE: SliceSpec = SliceSpec {
    id: "ledger_drafts",
    title: "Ledger entry drafts",
    summary: "Produce + approval vertical for ledger_entry work items: typed fill stages a received-payment draft (payer/amount/date grounded with literal provenance — money is never invented); approval enqueues the provider write as an outbox job — Invoice Ninja record_receipt (client + invoice + applied payment) or QBO record_payment (applied to the snapshot invoice whose open balance matches the amount) — dry-run while the provider's write gate is closed.",
    routes: &[
        RouteSpec {
            method: "GET",
            path: "/api/ledger-drafts",
            summary: "Drafts newest-first (?item_id= scopes to one work item); includes outbox delivery state",
        },
        RouteSpec {
            method: "POST",
            path: "/api/ledger-drafts/produce",
            summary: "Produce a receipt draft from an accepted work item (typed fill; returns the existing active draft when one exists)",
        },
        RouteSpec {
            method: "POST",
            path: "/api/ledger-drafts/{draft_id}/action",
            summary: "Approve (stages the accounting write as an outbox job; requires a writable provider) or reject a staged draft",
        },
        RouteSpec {
            method: "POST",
            path: "/api/ledger-drafts/{draft_id}/update",
            summary: "Edit a staged draft's AI-filled receipt fields (payer/amount/date/description) before approval",
        },
    ],
    tables: &["ledger_entry_drafts"],
    env_vars: &[
        &env_registry::BOS_INVOICE_NINJA_BASE_URL,
        &env_registry::BOS_INVOICE_NINJA_API_TOKEN,
        &env_registry::BOS_INVOICE_NINJA_WRITE_ENABLED,
        &env_registry::BOS_QBO_WRITE_ENABLED,
    ],
    read_models: &["ledger_drafts"],
};
