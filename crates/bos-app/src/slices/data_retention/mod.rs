//! Bounded automatic SQLite retention and storage maintenance.

pub mod routes;
pub mod service;
pub mod store;
pub mod worker;

#[cfg(test)]
mod tests;

use crate::env_registry;
use crate::slices::{RouteSpec, SliceSpec};

pub const SLICE: SliceSpec = SliceSpec {
    id: "data_retention",
    title: "Data retention",
    summary: "Bounded, receipted compaction of old email bodies and explicitly allowlisted applied receipt payloads, plus safe SQLite checkpoint/optimize/incremental-vacuum maintenance. Receipt rows and idempotency history are permanent; full VACUUM is never automatic.",
    routes: &[
        RouteSpec {
            method: "GET",
            path: "/api/data-retention/status",
            summary: "Retention policy, eligible rows, SQLite allocation, reusable pages, WAL size, and last-run state",
        },
        RouteSpec {
            method: "POST",
            path: "/api/data-retention/run",
            summary: "Start one idempotent, overlap-guarded retention cycle",
        },
    ],
    tables: &[],
    env_vars: &[
        &env_registry::BOS_DATA_RETENTION_BATCH_SIZE,
        &env_registry::BOS_DATA_RETENTION_EMAIL_BODY_DAYS,
        &env_registry::BOS_DATA_RETENTION_ENABLED,
        &env_registry::BOS_DATA_RETENTION_INCREMENTAL_VACUUM_PAGES,
        &env_registry::BOS_DATA_RETENTION_INTERVAL_SECS,
        &env_registry::BOS_DATA_RETENTION_MAX_ROWS_PER_CYCLE,
        &env_registry::BOS_DATA_RETENTION_RECEIPT_PAYLOAD_DAYS,
    ],
    read_models: &["data_retention_status"],
};
