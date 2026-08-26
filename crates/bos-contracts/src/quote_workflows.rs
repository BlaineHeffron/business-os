use serde::{Deserialize, Serialize};

use crate::calendar_drafts::OutboxJobSummary;
use crate::receipt::ReceiptDto;

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowRunStatus {
    Running,
    Staged,
    NeedsOperatorInput,
    Failed,
    Approved,
    Rejected,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuoteDraftStatus {
    Staged,
    Approved,
    Rejected,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuoteLineItem {
    pub sku: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_line: Option<String>,
    pub description: String,
    pub quantity: u32,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub unit_cents: i64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub total_cents: i64,
    pub source_quote: String,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuoteGuardrailStatus {
    WithinGuardrails,
    NeedsApproval,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuoteGuardrailSeverity {
    Info,
    Review,
    ApprovalRequired,
    Major,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuoteGuardrailFinding {
    pub code: String,
    pub severity: QuoteGuardrailSeverity,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_sku: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_line: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer_tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub list_unit_cents: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub quoted_unit_cents: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta_bps: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_approver_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuoteApprovalRoute {
    pub approver_id: String,
    pub reason: String,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuoteGuardrailEvaluation {
    pub status: QuoteGuardrailStatus,
    pub config_hash: String,
    #[serde(default)]
    pub findings: Vec<QuoteGuardrailFinding>,
    #[serde(default)]
    pub approval_routes: Vec<QuoteApprovalRoute>,
    #[cfg_attr(feature = "ts", ts(type = "unknown"))]
    pub config_snapshot_json: serde_json::Value,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuoteDraft {
    pub draft_id: String,
    pub run_id: String,
    pub source_kind: String,
    pub source_ref: String,
    pub status: QuoteDraftStatus,
    pub customer_name: String,
    pub summary: String,
    pub line_items: Vec<QuoteLineItem>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub subtotal_cents: i64,
    pub guardrails: QuoteGuardrailEvaluation,
    pub policy_notes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outbox_job_id: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub created_at_ms: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub updated_at_ms: u64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuoteDraftWithRevision {
    pub draft: QuoteDraft,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outbox_job: Option<OutboxJobSummary>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowRun {
    pub run_id: String,
    pub workflow: String,
    pub version: String,
    pub profile_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_sha: Option<String>,
    pub status: WorkflowRunStatus,
    #[cfg_attr(feature = "ts", ts(type = "unknown"))]
    pub input_snapshot_json: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "unknown | null"))]
    pub terminal_json: Option<serde_json::Value>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub started_at_ms: u64,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub finished_at_ms: Option<u64>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub updated_at_ms: u64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowStep {
    pub run_id: String,
    pub step_index: u32,
    pub node: String,
    pub node_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    #[serde(default)]
    pub inputs: Vec<WorkflowTraceValue>,
    #[serde(default)]
    pub outputs: Vec<WorkflowTraceValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "unknown | null"))]
    pub llm_usage: Option<serde_json::Value>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub latency_ms: u64,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    pub receipt_id: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub created_at_ms: u64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowTraceValue {
    pub label: String,
    #[cfg_attr(feature = "ts", ts(type = "unknown"))]
    pub value: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub formula: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuoteWorkflowRunRequest {
    pub source_kind: String,
    pub source_ref: String,
    pub customer_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer_tier: Option<String>,
    pub request_text: String,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuoteWorkflowRunResponse {
    pub run: WorkflowRun,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<QuoteDraftWithRevision>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuoteDraftActionKind {
    Approve,
    Reject,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuoteDraftActionRequest {
    pub action: QuoteDraftActionKind,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuoteWorkflowInspection {
    pub run: WorkflowRun,
    pub steps: Vec<WorkflowStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<QuoteDraftWithRevision>,
    pub receipts: Vec<ReceiptDto>,
    pub outbox_jobs: Vec<OutboxJobSummary>,
}
