//! Follow-up task contracts: the produce → approve → LOCAL write vertical for
//! the `follow_up_task` packet kind.
//!
//! Pipeline position: work item accepted → **produce** (typed fill stages a
//! draft) → operator approves → row in the local tasks table. No provider is
//! touched — approval is the write, executed in the same receipted mutation.

use serde::{Deserialize, Serialize};

use crate::calendar_drafts::DraftFieldProvenance;
use crate::email_drafts::EmailOutboundFollowUpSummary;

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FollowUpDraftStatus {
    Staged,
    Approved,
    Rejected,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FollowUpDraft {
    /// "fud_<item_id>_<attempt>" — one active (non-rejected) draft per item.
    pub draft_id: String,
    pub item_id: String,
    pub source_kind: String,
    pub source_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_user_id: Option<String>,
    pub status: FollowUpDraftStatus,
    pub title: String,
    /// ISO date (YYYY-MM-DD); None when the source gives no deadline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_date: Option<String>,
    /// One or two sentences of operator-useful context.
    pub context: String,
    pub provenance: Vec<DraftFieldProvenance>,
    pub model: String,
    /// Extractor's own confidence: "high" | "medium" | "low".
    pub confidence: String,
    /// Set when approved: the local task the approval created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub created_at_ms: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub updated_at_ms: u64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FollowUpDraftWithRevision {
    pub draft: FollowUpDraft,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub revision: u64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FollowUpDraftsResponse {
    pub drafts: Vec<FollowUpDraftWithRevision>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FollowUpDraftProduceRequest {
    pub item_id: String,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FollowUpDraftProduceResponse {
    pub draft: FollowUpDraftWithRevision,
}

/// Stage an operator-authored task draft for an accepted work item without a
/// model call. Approval still creates the local task through the normal gate.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FollowUpDraftManualStageRequest {
    pub item_id: String,
    pub title: String,
    #[serde(default)]
    pub due_date: Option<String>,
    pub context: String,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FollowUpDraftActionKind {
    Approve,
    Reject,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FollowUpDraftActionRequest {
    pub action: FollowUpDraftActionKind,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

// --- the local task itself ---

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Open,
    Done,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRecord {
    pub task_id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_date: Option<String>,
    pub context: String,
    /// Where the task came from ("email" + message id via the draft).
    pub source_kind: String,
    pub source_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_user_id: Option<String>,
    /// Originating work item when the task was created from an approved
    /// follow-up draft. Null for tracking tasks created directly by other
    /// slices.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_item_id: Option<String>,
    pub status: TaskStatus,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub created_at_ms: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub updated_at_ms: u64,
}

/// Due lane for the Tasks view, computed server-side against the operator's
/// local date (`?today=YYYY-MM-DD` on GET /api/tasks).
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskDueLane {
    Overdue,
    DueToday,
    Upcoming,
    NoDueDate,
}

/// Watchdog escalation level for an overdue task. Classification and
/// thresholds ported from agent-monitor-rust's customer_follow_up watchdog:
/// overdue below the escalation threshold is "missed", at/after it
/// "escalated", at/after the critical threshold "critical".
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskEscalationLevel {
    None,
    Missed,
    Escalated,
    Critical,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskEscalation {
    pub lane: TaskDueLane,
    pub level: TaskEscalationLevel,
    /// Days past due (0 unless overdue).
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub days_overdue: i64,
    /// Days until due (0 unless upcoming).
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub days_until_due: i64,
    /// Operator-facing escalation reason (None when not escalated).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskWithRevision {
    pub task: TaskRecord,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub revision: u64,
    /// Present on open tasks when the request supplied `today`; done tasks
    /// and undated requests carry None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation: Option<TaskEscalation>,
    /// Present for tasks spawned by an approved outbound email draft's
    /// "follow up if no reply" workflow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up: Option<EmailOutboundFollowUpSummary>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TasksResponse {
    pub tasks: Vec<TaskWithRevision>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskActionKind {
    Complete,
    Reopen,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskActionRequest {
    pub action: TaskActionKind,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

/// Operator edit of a STAGED draft's AI-filled fields ("AI-produced fields
/// remain editable until accepted"). Full replacement of the editable set.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FollowUpDraftUpdateRequest {
    pub title: String,
    /// ISO date (YYYY-MM-DD) or null for no deadline.
    #[serde(default)]
    pub due_date: Option<String>,
    pub context: String,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_and_task_round_trip() {
        let draft = FollowUpDraft {
            draft_id: "fud_wi_email_m1_1".into(),
            item_id: "wi_email_m1".into(),
            source_kind: "email".into(),
            source_ref: "m1".into(),
            source_user_id: Some("user_jordan".into()),
            status: FollowUpDraftStatus::Staged,
            title: "Reply to vendor about quote".into(),
            due_date: Some("2026-06-15".into()),
            context: "Vendor asked for a decision by Monday.".into(),
            provenance: vec![DraftFieldProvenance {
                field: "due_date".into(),
                quote: "by Monday".into(),
            }],
            model: "claude-sonnet-4-6".into(),
            confidence: "high".into(),
            task_id: None,
            created_at_ms: 1,
            updated_at_ms: 1,
        };
        let json = serde_json::to_string(&draft).expect("serialize");
        let back: FollowUpDraft = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(draft, back);

        let task = TaskRecord {
            task_id: "task_fud_wi_email_m1_1".into(),
            title: draft.title.clone(),
            due_date: draft.due_date.clone(),
            context: draft.context.clone(),
            source_kind: "email".into(),
            source_ref: "m1".into(),
            source_user_id: draft.source_user_id.clone(),
            source_item_id: Some("wi_email_m1".into()),
            status: TaskStatus::Open,
            created_at_ms: 2,
            updated_at_ms: 2,
        };
        let json = serde_json::to_string(&task).expect("serialize");
        let back: TaskRecord = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(task, back);
    }
}
