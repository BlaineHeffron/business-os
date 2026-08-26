//! Content planning contracts: local plan items that queue normal
//! `content_draft` work, plus advisory duplicate/cannibalization summaries.
//! Publishing remains manual. Inventory rows are local projection/manual rows
//! used for advisory duplicate/cannibalization warnings.

use serde::{Deserialize, Serialize};

use crate::calendar_drafts::OutboxJobSummary;
use crate::content_drafts::ContentDraftWithRevision;
use crate::social_publishing::{
    SocialPostProposalWithRevision, SocialProposalTarget, SocialPublishingChannel,
    SocialSourceGenerationStatus,
};

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentPlanStatus {
    Planned,
    Queued,
    Published,
    Cancelled,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentPlanDraftState {
    None,
    Staged,
    Approved,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentInventorySourceKind {
    PlanItem,
    SearchConsolePage,
    Manual,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentInventoryStatus {
    Pipeline,
    Published,
    Archived,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentCollisionMatch {
    /// Stable row key for the compared local content surface. PR2 uses
    /// synthetic keys such as `plan:<id>` and `draft:<id>`; PR3 inventory rows
    /// use their `inventory_id`.
    pub inventory_id: String,
    pub source_kind: String,
    pub source_ref: String,
    pub title: String,
    pub reason: String,
    pub score: f64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentCollisionSummary {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub checked_at_ms: u64,
    pub matches: Vec<ContentCollisionMatch>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentPlanItem {
    pub plan_item_id: String,
    pub status: ContentPlanStatus,
    pub topic: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub angle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collision_summary: Option<ContentCollisionSummary>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub created_at_ms: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub updated_at_ms: u64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentPlanItemWithRevision {
    pub item: ContentPlanItem,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub revision: u64,
    pub draft_state: ContentPlanDraftState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_draft_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentPlanItemsResponse {
    pub items: Vec<ContentPlanItemWithRevision>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentInventoryItem {
    pub inventory_id: String,
    pub source_kind: ContentInventorySourceKind,
    pub source_ref: String,
    pub status: ContentInventoryStatus,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub canonical_key: String,
    pub metrics_json: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub last_seen_at_ms: Option<u64>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub created_at_ms: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub updated_at_ms: u64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentInventoryItemWithRevision {
    pub item: ContentInventoryItem,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub revision: u64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentInventoryResponse {
    pub items: Vec<ContentInventoryItemWithRevision>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentDraftOverlapResponse {
    pub summary: ContentCollisionSummary,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentPlanItemCreateRequest {
    pub topic: String,
    #[serde(default)]
    pub angle: Option<String>,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub target_query: Option<String>,
    #[serde(default)]
    pub audience: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentPlanItemUpdateRequest {
    pub topic: String,
    #[serde(default)]
    pub angle: Option<String>,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub target_query: Option<String>,
    #[serde(default)]
    pub audience: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentPlanItemQueueRequest {
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentPlanItemCheckRequest {
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentPlanItemMarkPublishedRequest {
    pub published_url: String,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentInventoryManualAddRequest {
    pub title: String,
    #[serde(default)]
    pub target_query: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentInventoryArchiveRequest {
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentInventoryRefreshRequest {
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

/// Operator intent for the blog adapter. Both variants authorize the external
/// blog write; `schedule` leaves timing to the adapter's `published_at` field.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentCampaignLaunchMode {
    PublishNow,
    Schedule,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentCampaignPublicationStatus {
    AwaitingBlog,
    BlogDryRun,
    SocialEnqueued,
    Completed,
    RequiresReview,
}

/// Immutable, operator-approved campaign snapshot. This is coordination state,
/// not a second article/social state machine: the editable bodies remain owned
/// by content_drafts and social_publishing.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentCampaignPublication {
    pub publication_id: String,
    pub plan_item_id: String,
    pub content_draft_id: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub content_draft_revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub social_proposal_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub social_proposal_revision: Option<u64>,
    pub expected_canonical_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual_canonical_url: Option<String>,
    pub launch_mode: ContentCampaignLaunchMode,
    pub selected_channel_ids: Vec<String>,
    pub approved_social_targets: Vec<SocialProposalTarget>,
    pub status: ContentCampaignPublicationStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_reason: Option<String>,
    pub approved_by: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub approved_at_ms: u64,
    pub blog_outbox_job: OutboxJobSummary,
    pub social_outbox_jobs: Vec<OutboxJobSummary>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentCampaignPublicationWithRevision {
    pub publication: ContentCampaignPublication,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub revision: u64,
}

/// One read model for the Linear-style campaign workspace.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentCampaignWorkspaceResponse {
    pub plan: ContentPlanItemWithRevision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_draft: Option<ContentDraftWithRevision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub social_proposal: Option<SocialPostProposalWithRevision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub social_generation_status: Option<SocialSourceGenerationStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub social_generation_error: Option<String>,
    pub publications: Vec<ContentCampaignPublicationWithRevision>,
    pub channels: Vec<SocialPublishingChannel>,
    pub blog_publishing_available: bool,
    pub blog_live_enabled: bool,
    pub social_configured: bool,
    pub social_live_enabled: bool,
}

/// Generate/continue the article half of one plan in-place. A planned item is
/// queued as operator-accepted in the same receipted mutation before the normal
/// content_drafts producer runs.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentCampaignGenerateRequest {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub expected_revision: u64,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

/// Explicit human approval of the exact article revision, social revision,
/// selected destinations, expected live URL, and launch timing.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentCampaignPublishRequest {
    pub content_draft_id: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub expected_content_draft_revision: u64,
    #[serde(default)]
    pub social_proposal_id: Option<String>,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_social_proposal_revision: Option<u64>,
    #[serde(default)]
    pub selected_channel_ids: Vec<String>,
    pub slug: String,
    pub published_at: String,
    pub expected_canonical_url: String,
    pub launch_mode: ContentCampaignLaunchMode,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}
