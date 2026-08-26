//! CRM note draft contracts: the produce → approve → provider-write vertical
//! for the `crm_activity` packet kind (HubSpot note logging the source
//! call/email). Write-gated like the calendar vertical: approval enqueues an
//! outbox job; the HubSpot client dry-runs until the gate opens.

use serde::{Deserialize, Serialize};

use crate::calendar_drafts::{DraftFieldProvenance, OutboxJobSummary};

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrmDraftStatus {
    Staged,
    Approved,
    Rejected,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrmNoteDraft {
    /// "cnd_<item_id>_<attempt>" — one active (non-rejected) draft per item.
    pub draft_id: String,
    pub item_id: String,
    pub source_kind: String,
    pub source_ref: String,
    /// Operator user this draft is bound to, inherited from the originating
    /// work item. Null = legacy rows / all-scope-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_user_id: Option<String>,
    pub status: CrmDraftStatus,
    /// The CRM-ready note text.
    pub note_body: String,
    /// Contact the note concerns, when determinable (rides in the note body
    /// on write — the free-tier posture avoids the associations API).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact_email: Option<String>,
    /// RFC3339 timestamp the note is logged at (grounded from the source
    /// email's date, never model-invented).
    pub occurred_at: String,
    pub provenance: Vec<DraftFieldProvenance>,
    pub model: String,
    pub confidence: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outbox_job_id: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub created_at_ms: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub updated_at_ms: u64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrmDraftWithRevision {
    pub draft: CrmNoteDraft,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outbox_job: Option<OutboxJobSummary>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrmDraftsResponse {
    pub drafts: Vec<CrmDraftWithRevision>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrmDraftProduceRequest {
    pub item_id: String,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrmDraftProduceResponse {
    pub draft: CrmDraftWithRevision,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrmDraftActionKind {
    Approve,
    Reject,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrmDraftActionRequest {
    pub action: CrmDraftActionKind,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

/// Operator edit of a STAGED draft's AI-filled fields ("AI-produced fields
/// remain editable until accepted"). occurred_at stays grounded from the
/// source email's date — not editable.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrmDraftUpdateRequest {
    pub note_body: String,
    #[serde(default)]
    pub contact_email: Option<String>,
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
    fn crm_draft_round_trips() {
        let draft = CrmNoteDraft {
            draft_id: "cnd_wi_email_m1_1".into(),
            item_id: "wi_email_m1".into(),
            source_kind: "email".into(),
            source_ref: "m1".into(),
            source_user_id: None,
            status: CrmDraftStatus::Staged,
            note_body: "Call from Dana: wants the storefront repaint quote by Monday.".into(),
            contact_email: Some("dana@example.test".into()),
            occurred_at: "2026-06-10T11:45:00Z".into(),
            provenance: vec![DraftFieldProvenance {
                field: "note_body".into(),
                quote: "Dana called about the storefront".into(),
            }],
            model: "claude-sonnet-4-6".into(),
            confidence: "high".into(),
            outbox_job_id: None,
            created_at_ms: 1,
            updated_at_ms: 1,
        };
        let json = serde_json::to_string(&draft).expect("serialize");
        let back: CrmNoteDraft = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(draft, back);
    }
}
