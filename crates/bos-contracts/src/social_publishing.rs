//! Approval-gated social publishing contracts. A staged proposal owns one
//! editable target per configured Buffer channel. Approval snapshots the exact
//! current revision and fans it out into independent outbox jobs.

use crate::calendar_drafts::OutboxJobSummary;
use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SocialProposalStatus {
    Staged,
    Approved,
    Rejected,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SocialScheduleMode {
    Queue,
    Scheduled,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SocialUtmParameters {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub medium: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub campaign: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SocialPublishingChannel {
    pub channel_id: String,
    pub name: String,
    /// Buffer service id, for example linkedin, twitter, or facebook.
    pub platform: String,
}

/// Editable input for one configured channel. Channel display metadata is
/// resolved server-side so agents never get to redirect an approved write.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SocialProposalTargetInput {
    pub channel_id: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
    #[serde(default)]
    pub utm: SocialUtmParameters,
    pub schedule_mode: SocialScheduleMode,
    /// Required for scheduled mode; absent for queue mode. RFC3339 with offset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_at: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SocialProposalTarget {
    pub target_id: String,
    pub channel_id: String,
    pub channel_name: String,
    pub platform: String,
    /// Exact text handed to Buffer. The tracked URL is normalized into this
    /// text before staging, so the operator approves the provider payload.
    pub text: String,
    pub tracked_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
    pub utm: SocialUtmParameters,
    pub schedule_mode: SocialScheduleMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outbox_job_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outbox_job: Option<OutboxJobSummary>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SocialPostProposal {
    pub proposal_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_content_draft_id: Option<String>,
    /// Exact article revision used to ground a pre-publication proposal.
    /// Absent for external/published-only ingress.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub source_content_draft_revision: Option<u64>,
    pub canonical_url: String,
    pub status: SocialProposalStatus,
    pub targets: Vec<SocialProposalTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_by: Option<String>,
    /// Revision whose exact payload was approved. The approval mutation itself
    /// advances the entity to the next revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub approved_revision: Option<u64>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub created_at_ms: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub updated_at_ms: u64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SocialPostProposalWithRevision {
    pub proposal: SocialPostProposal,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub revision: u64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SocialPublishedSource {
    pub source_id: String,
    pub source_kind: String,
    pub external_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_content_draft_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub source_content_draft_revision: Option<u64>,
    pub title: String,
    pub canonical_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,
    pub generation_status: SocialSourceGenerationStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal_id: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub revision: u64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SocialSourceGenerationStatus {
    Ready,
    Generating,
    ProposalStaged,
    GenerationFailed,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SocialPublishingResponse {
    pub proposals: Vec<SocialPostProposalWithRevision>,
    pub channels: Vec<SocialPublishingChannel>,
    pub published_sources: Vec<SocialPublishedSource>,
    pub buffer_configured: bool,
    pub buffer_live_enabled: bool,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SocialProposalStageRequest {
    #[serde(default)]
    pub source_id: Option<String>,
    #[serde(default)]
    pub source_content_draft_id: Option<String>,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub source_content_draft_revision: Option<u64>,
    pub canonical_url: String,
    pub targets: Vec<SocialProposalTargetInput>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

/// Narrow CMS/OpenClaw boundary: published-content identity and metadata only.
/// Social copy and provider-write fields are intentionally absent.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SocialPublishedContentIngressRequest {
    pub source_kind: String,
    pub external_id: String,
    #[serde(default)]
    pub source_content_draft_id: Option<String>,
    pub canonical_url: String,
    pub title: String,
    #[serde(default)]
    pub excerpt: Option<String>,
    #[serde(default)]
    pub published_at: Option<String>,
    pub idempotency_key: String,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SocialProposalGenerateRequest {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub expected_revision: u64,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

/// Create a durable preview source from an editable BusinessOS article and
/// run the normal bounded social drafting transform before the article is live.
/// The operator-previewed URL is later compared byte-for-normalized-byte with
/// the blog adapter's returned canonical URL before any Buffer job exists.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SocialDraftPreviewGenerateRequest {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub expected_content_draft_revision: u64,
    pub expected_canonical_url: String,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SocialGenerationResponse {
    pub source: SocialPublishedSource,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SocialProposalUpdateRequest {
    pub canonical_url: String,
    pub targets: Vec<SocialProposalTargetInput>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub expected_revision: u64,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SocialProposalActionKind {
    Approve,
    Reject,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SocialProposalActionRequest {
    pub action: SocialProposalActionKind,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub expected_revision: u64,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}
