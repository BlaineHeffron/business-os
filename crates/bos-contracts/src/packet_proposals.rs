//! Packet proposal contracts: one bounded AI call proposes which packet kinds
//! are warranted and returns typed draft payloads for the existing produce
//! gates to stage.

use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PacketProposalDecisionMode {
    AiDecides,
    FillFixed,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PacketProposalExecutionMode {
    BoundedTyped,
    ToolLoopAgentic,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PacketProposalRunStatus {
    Running,
    Completed,
    Failed,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PacketProposalKindOutcomeStatus {
    Drafted,
    Unavailable,
    RejectedByGate,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PacketProposalReasonCode {
    ActiveDraftExists,
    CategoryInvalid,
    ContextUnavailable,
    GateRejected,
    KindNotEnabled,
    KindNotRequested,
    LowConfidence,
    ModelOutputInvalid,
    SourceMissing,
    SourceUnsupported,
    StageFailed,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PacketProposalKindOutcome {
    pub packet_kind: String,
    pub status: PacketProposalKindOutcomeStatus,
    #[serde(default)]
    pub reason_code: Option<PacketProposalReasonCode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default)]
    pub draft_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PacketProposalRun {
    pub run_id: String,
    pub source_kind: String,
    pub source_ref: String,
    #[serde(default)]
    pub item_id: Option<String>,
    pub resolved_decision_mode: PacketProposalDecisionMode,
    pub execution_mode: PacketProposalExecutionMode,
    pub status: PacketProposalRunStatus,
    pub candidate_packet_kinds: Vec<String>,
    pub outcomes: Vec<PacketProposalKindOutcome>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub confidence: Option<String>,
    #[serde(default)]
    pub error_code: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub created_at_ms: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub updated_at_ms: u64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmartDraftRequest {
    pub source_kind: String,
    pub source_ref: String,
    pub idempotency_key: String,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmartDraftSourceStateRequest {
    pub source_kind: String,
    pub source_ref: String,
    #[serde(default)]
    pub run_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmartDraftSourceStateResponse {
    #[serde(default)]
    pub item: Option<crate::work_queue::WorkItemWithRevision>,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    #[serde(default)]
    pub run: Option<PacketProposalRun>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmartDraftResponse {
    pub run: PacketProposalRun,
    #[serde(default)]
    pub item: Option<crate::work_queue::WorkItemWithRevision>,
}
