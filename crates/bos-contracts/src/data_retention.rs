//! Automatic SQLite retention and storage-maintenance contracts.

use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SqliteAutoVacuumMode {
    None,
    Full,
    Incremental,
    Unknown,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataRetentionStatus {
    pub enabled: bool,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub interval_secs: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub email_body_retention_days: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub receipt_payload_retention_days: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub batch_size: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub max_rows_per_cycle: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub incremental_vacuum_pages: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub email_body_cutoff_ms: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub receipt_payload_cutoff_ms: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub eligible_email_bodies: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub eligible_receipt_payloads: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub database_bytes: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub page_size_bytes: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub page_count: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub freelist_pages: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub freelist_bytes: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub wal_bytes: u64,
    pub auto_vacuum_mode: SqliteAutoVacuumMode,
    pub attended_full_vacuum_required: bool,
    pub in_flight: bool,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub last_attempt_ms: Option<u64>,
    pub last_outcome: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub last_duration_ms: Option<u64>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub last_units_compacted: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub next_allowed_at_ms: u64,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub last_retention_receipt_at_ms: Option<u64>,
    pub last_retention_receipt_outcome: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataRetentionRunRequest {
    #[serde(default)]
    pub actor_id: Option<String>,
    pub idempotency_key: String,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataRetentionRunStatus {
    Spawned,
    Replayed,
    AlreadyRunning,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataRetentionRunResponse {
    pub status: DataRetentionRunStatus,
    pub run_id: Option<String>,
    pub reason: Option<String>,
}
