//! Email reply draft contracts: the produce → approve → provider-write
//! vertical for the `email_draft_reply` packet kind. Approval creates a
//! Gmail DRAFT in the operator's mailbox (never sends); sending stays a
//! human act in Gmail — the DRAFT→approver posture for customer-facing mail.

use serde::{Deserialize, Serialize};

use crate::calendar_drafts::{DraftFieldProvenance, OutboxJobSummary};
use crate::work_queue::WorkItemWithRevision;

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmailDraftStatus {
    Staged,
    Approved,
    Rejected,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailReplyDraft {
    /// "erd_<item_id>_<attempt>" — one active (non-rejected) draft per item.
    pub draft_id: String,
    pub item_id: String,
    pub source_kind: String,
    pub source_ref: String,
    /// Operator user this draft is bound to, inherited from the originating
    /// work item. Null = legacy rows / all-scope-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_user_id: Option<String>,
    pub status: EmailDraftStatus,
    /// Reply recipient — source-grounded on inbound replies, operator-authored
    /// for blank drafts, and never model-chosen.
    pub to_addr: String,
    /// Reply-all Cc recipients grounded from the source message's To/Cc,
    /// excluding the source mailbox when known.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cc_addrs: Vec<String>,
    /// Source-grounded on inbound replies or operator-authored for blank
    /// drafts; never model-chosen.
    pub subject: String,
    pub body_text: String,
    /// Gmail thread the draft attaches to (reply threading).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    /// RFC Message-ID of the source message. Used for Gmail reply drafts'
    /// In-Reply-To header; source-grounded and not operator-editable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_message_id: Option<String>,
    /// RFC References chain for the source conversation, with the source
    /// Message-ID appended. Source-grounded and not operator-editable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reference_message_ids: Vec<String>,
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
pub struct EmailDraftWithRevision {
    pub draft: EmailReplyDraft,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outbox_job: Option<OutboxJobSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up: Option<EmailOutboundFollowUpSummary>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailDraftsResponse {
    pub drafts: Vec<EmailDraftWithRevision>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailDraftProduceRequest {
    pub item_id: String,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailDraftProduceResponse {
    pub draft: EmailDraftWithRevision,
}

/// Stage an operator-authored email draft for an already accepted work item.
/// This is the blank/manual Output Composer path; no model call occurs.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailDraftManualStageRequest {
    pub item_id: String,
    pub to_addr: String,
    #[serde(default)]
    pub cc_addrs: Vec<String>,
    pub subject: String,
    pub body_text: String,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

/// Exact-revision AI rewrite of a staged email body. Recipient and subject
/// remain operator-owned typed fields; the model has no approval authority.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailDraftRewriteRequest {
    pub instructions: String,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailDraftRewriteResponse {
    pub draft: EmailDraftWithRevision,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmailDraftActionKind {
    Approve,
    Reject,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailDraftActionRequest {
    pub action: EmailDraftActionKind,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up: Option<EmailDraftFollowUpRequest>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailDraftFollowUpRequest {
    pub enabled: bool,
    /// ISO date (YYYY-MM-DD), computed by the frontend/overlay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_date: Option<String>,
    pub title: String,
    pub context: String,
    pub create_follow_up_draft: bool,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmailOutboundFollowUpStatus {
    Active,
    Resolved,
    Cancelled,
    Stale,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GmailThreadFollowUpState {
    DraftCreated,
    SentWaitingReply,
    RepliedAfterSend,
    StaleUnknown,
    NotApplicable,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailOutboundFollowUpSummary {
    pub follow_up_id: String,
    pub email_draft_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up_task_id: Option<String>,
    pub item_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    pub status: EmailOutboundFollowUpStatus,
    pub thread_state: GmailThreadFollowUpState,
    pub due_date: String,
    pub follow_up_title: String,
    pub create_follow_up_draft: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub sent_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub reply_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub last_checked_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_check_error: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailOutboundFollowUpsResponse {
    pub follow_ups: Vec<EmailOutboundFollowUpSummary>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailOutboundFollowUpActionRequest {
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailOutboundFollowUpCheckResponse {
    pub follow_up: EmailOutboundFollowUpSummary,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailOutboundFollowUpDraftResponse {
    pub item: WorkItemWithRevision,
}

/// Operator edit of a STAGED draft. Typed recipient, Cc, subject, and body
/// fields remain editable until approval; threading metadata stays grounded.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailDraftUpdateRequest {
    pub to_addr: String,
    #[serde(default)]
    pub cc_addrs: Vec<String>,
    pub subject: String,
    pub body_text: String,
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
    fn email_draft_round_trips() {
        let draft = EmailReplyDraft {
            draft_id: "erd_wi_email_m1_1".into(),
            item_id: "wi_email_m1".into(),
            source_kind: "email".into(),
            source_ref: "m1".into(),
            source_user_id: None,
            status: EmailDraftStatus::Staged,
            to_addr: "dana@example.test".into(),
            cc_addrs: vec!["alex@example.test".into()],
            subject: "Re: storefront quote".into(),
            body_text: "Hi Dana — happy to help with that.".into(),
            thread_id: Some("thread-9".into()),
            reply_message_id: Some("<m1@example.test>".into()),
            reference_message_ids: vec!["<root@example.test>".into(), "<m1@example.test>".into()],
            provenance: vec![DraftFieldProvenance {
                field: "body_text".into(),
                quote: "could you send me a quote".into(),
            }],
            model: "claude-sonnet-4-6".into(),
            confidence: "high".into(),
            outbox_job_id: None,
            created_at_ms: 1,
            updated_at_ms: 1,
        };
        let json = serde_json::to_string(&draft).expect("serialize");
        let back: EmailReplyDraft = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(draft, back);
    }

    #[test]
    fn follow_up_contract_round_trips() {
        let request = EmailDraftFollowUpRequest {
            enabled: true,
            due_date: Some("2026-06-26".into()),
            title: "Follow up: storefront quote".into(),
            context: "Dana asked for a quote.".into(),
            create_follow_up_draft: false,
        };
        let json = serde_json::to_string(&request).expect("serialize");
        let back: EmailDraftFollowUpRequest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(request, back);

        let summary = EmailOutboundFollowUpSummary {
            follow_up_id: "efuw_erd_wi_email_m1_1".into(),
            email_draft_id: "erd_wi_email_m1_1".into(),
            follow_up_task_id: Some("task_efuw_erd_wi_email_m1_1".into()),
            item_id: "wi_email_m1".into(),
            thread_id: Some("thread-9".into()),
            status: EmailOutboundFollowUpStatus::Active,
            thread_state: GmailThreadFollowUpState::SentWaitingReply,
            due_date: "2026-06-26".into(),
            follow_up_title: "Follow up: storefront quote".into(),
            create_follow_up_draft: false,
            sent_message_id: Some("sent-1".into()),
            sent_at_ms: Some(2),
            reply_message_id: None,
            reply_at_ms: None,
            resolution_reason: None,
            last_checked_at_ms: Some(3),
            last_check_error: None,
        };
        let json = serde_json::to_string(&summary).expect("serialize");
        let back: EmailOutboundFollowUpSummary = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(summary, back);
    }
}
