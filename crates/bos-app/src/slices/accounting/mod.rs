//! Accounting provider connector + cached views (QuickBooks | Invoice
//! Ninja, selected by BOS_ACCOUNTING_PROVIDER). Provider read limits are the
//! design driver: the browser only ever reads the local snapshot cache; the
//! provider is touched exclusively by a request-budgeted, incremental sync
//! cycle (env-gated pump or the guarded Sync-now route), one request in
//! flight at most.
//!
//! KNOWN SEAM: hard-deleted QBO invoices linger in the cache — the query API
//! never returns deletes (voided invoices DO appear and are flagged/excluded
//! from view math). Fix later with the CDC endpoint or a periodic forced
//! backfill if Demo turns out to actually delete invoices.

pub mod routes;
pub mod service;
pub mod store;
pub mod worker;

#[cfg(test)]
mod tests;

use crate::env_registry;
use crate::slices::{RouteSpec, SliceSpec};

pub const SLICE: SliceSpec = SliceSpec {
    id: "accounting",
    title: "Accounting cached views",
    summary: "Accounting provider connector (QuickBooks OAuth or a self-hosted provider) feeding request-budgeted incremental sync into local snapshot caches; invoice/aging/financials/customer views serve from sqlite only — the UI never hits the provider API.",
    routes: &[
        RouteSpec {
            method: "GET",
            path: "/api/accounting/status",
            summary: "Provider + connection status; connect_url when QBO is disconnected",
        },
        RouteSpec {
            method: "GET",
            path: "/api/connectors/qbo/connect",
            summary: "Redirect to the Intuit consent screen",
        },
        RouteSpec {
            method: "GET",
            path: "/api/connectors/qbo/callback",
            summary: "OAuth redirect target; stores the realm-bound credential (realmId arrives here)",
        },
        RouteSpec {
            method: "POST",
            path: "/api/connectors/qbo/disconnect",
            summary: "Remove the stored QBO credential; body {purge:true} also deletes every cached snapshot/cursor row",
        },
        RouteSpec {
            method: "POST",
            path: "/api/accounting/sync",
            summary: "Kick one budgeted sync cycle (202; 409 while one runs or during the cooldown)",
        },
        RouteSpec {
            method: "GET",
            path: "/api/accounting/invoices",
            summary: "Cached invoice table (?filter=open|overdue|all) — local cache only, never QBO",
        },
        RouteSpec {
            method: "GET",
            path: "/api/accounting/aging",
            summary: "AR aging buckets over cached open invoices",
        },
        RouteSpec {
            method: "GET",
            path: "/api/accounting/financials",
            summary: "Owner financials from cached P&L reports: weekly/MTD sales and gross margin vs the four-quarter baseline (the pilot payment metric)",
        },
        RouteSpec {
            method: "GET",
            path: "/api/accounting/customers",
            summary: "Cached customer list with tiers (QBO is the tier source of truth)",
        },
    ],
    tables: &[
        "qbo_credentials",
        "accounting_sync_cursors",
        "accounting_invoice_snapshots",
        "accounting_bill_snapshots",
        "accounting_customer_snapshots",
        "accounting_pnl_snapshots",
        "accounting_balance_sheet_snapshots",
    ],
    env_vars: &[
        &env_registry::BOS_ACCOUNTING_PROVIDER,
        &env_registry::BOS_ACCOUNTING_METRIC_BASIS,
        &env_registry::BOS_ACCOUNTING_METRIC_LABEL,
        &env_registry::BOS_ACCOUNTING_METRIC_BASELINE_CENTS,
        &env_registry::BOS_ACCOUNTING_METRIC_ADJUSTED_FREIGHT_CENTS,
        &env_registry::BOS_ACCOUNTING_METRIC_ADJUSTED_TAXES_CENTS,
        &env_registry::BOS_ACCOUNTING_METRIC_ADJUSTED_INSURANCE_CENTS,
        &env_registry::BOS_ACCOUNTING_SYNC_ENABLED,
        &env_registry::BOS_ACCOUNTING_SYNC_INTERVAL_SECS,
        &env_registry::BOS_ACCOUNTING_MAX_REQUESTS_PER_CYCLE,
        &env_registry::BOS_ACCOUNTING_VISIBILITY_POLICY,
        &env_registry::BOS_QBO_CLIENT_ID,
        &env_registry::BOS_QBO_CLIENT_SECRET,
        &env_registry::BOS_QBO_ENVIRONMENT,
        &env_registry::BOS_STRIPE_SECRET_KEY,
        &env_registry::BOS_INVOICE_NINJA_BASE_URL,
        &env_registry::BOS_INVOICE_NINJA_API_TOKEN,
        &env_registry::BOS_PUBLIC_BASE_URL,
    ],
    read_models: &[
        "accounting_status",
        "accounting_invoices",
        "accounting_aging",
        "accounting_financials",
        "accounting_customers",
    ],
};
