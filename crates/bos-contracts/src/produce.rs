//! Shared produce-stage wire types (every packet kind's produce route uses
//! the same kickoff shape).

use serde::{Deserialize, Serialize};

/// 202 body when a produce was kicked off in the background: the draft is
/// being generated and will appear in the drafts list / queue feed — poll,
/// don't wait on the request. Also returned when a produce for the same
/// item+kind is already running (the second click is a no-op).
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProduceKickoffResponse {
    pub producing: bool,
}

/// Polled status for a background produce kickoff.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ProduceStatusResponse {
    Producing,
    Failed {
        error_code: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        receipt_id: String,
        created_at_ms: u64,
    },
    Idle,
}
