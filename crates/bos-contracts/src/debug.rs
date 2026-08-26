//! Operator debug diagnostics. This is a general backend-error projection over
//! auditable sources; individual rows identify their source table/type.

use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DebugDiagnosticRow {
    pub diagnostic_id: String,
    /// "receipt" | "outbox" | "llm" | "sync" | "drive".
    pub source: String,
    /// "error" | "warning".
    pub severity: String,
    /// Broad subsystem: mutation, provider_delivery, llm, sync, document_index.
    pub category: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    pub error_code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_id: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub occurred_at_ms: u64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DebugDiagnosticsResponse {
    pub rows: Vec<DebugDiagnosticRow>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DebugSpawnAgentRequest {
    pub diagnostic_id: String,
    pub idempotency_key: String,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DebugSpawnAgentResponse {
    pub session_id: String,
    pub thread_id: Option<String>,
    pub monitor_url: Option<String>,
}
