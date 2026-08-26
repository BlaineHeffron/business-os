//! Lead discovery contracts: approved-source findings staged for human review.

use crate::source::{EvidenceRecord, SourceKind};
use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeadDiscoverySourceKind {
    Forum,
    Reddit,
    GoogleAlert,
    FacebookGroup,
    Other,
}

impl From<LeadDiscoverySourceKind> for SourceKind {
    fn from(kind: LeadDiscoverySourceKind) -> Self {
        match kind {
            LeadDiscoverySourceKind::Forum => SourceKind::Forum,
            LeadDiscoverySourceKind::Reddit => SourceKind::Reddit,
            LeadDiscoverySourceKind::GoogleAlert => SourceKind::GoogleAlert,
            LeadDiscoverySourceKind::FacebookGroup => SourceKind::FacebookGroup,
            LeadDiscoverySourceKind::Other => SourceKind::Other,
        }
    }
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeadDiscoverySourceConfig {
    pub source_id: String,
    pub display_name: String,
    pub kind: LeadDiscoverySourceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default)]
    pub approved: bool,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub auto_poll: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feed_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_note: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeadDiscoveryCriteria {
    #[serde(default)]
    pub lead_markets: Vec<String>,
    #[serde(default)]
    pub intent_terms: Vec<String>,
    #[serde(default)]
    pub prohibited_sources: Vec<String>,
    #[serde(default)]
    pub routing_packet_kinds: Vec<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeadDiscoveryStatusResponse {
    pub configured: bool,
    pub enabled_sources: usize,
    pub pending_sources: usize,
    pub sources: Vec<LeadDiscoverySourceConfig>,
    pub criteria: LeadDiscoveryCriteria,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub auto_poll_last_checked_at_ms: Option<u64>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeadFindingStatus {
    Staged,
    Accepted,
    Rejected,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeadFinding {
    pub finding_id: String,
    pub source_id: String,
    pub status: LeadFindingStatus,
    pub title: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact_hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_hint: Option<String>,
    #[serde(default)]
    pub matched_terms: Vec<String>,
    pub evidence: EvidenceRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub created_at_ms: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub updated_at_ms: u64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeadFindingWithRevision {
    pub finding: LeadFinding,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub revision: u64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeadFindingsResponse {
    pub findings: Vec<LeadFindingWithRevision>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeadFindingStageRequest {
    pub source_id: String,
    pub title: String,
    pub summary: String,
    #[serde(default)]
    pub contact_hint: Option<String>,
    #[serde(default)]
    pub company_hint: Option<String>,
    #[serde(default)]
    pub matched_terms: Vec<String>,
    #[serde(default)]
    pub item_url: Option<String>,
    pub evidence_quote: String,
    #[serde(default)]
    pub captured_at_ms: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeadFindingActionKind {
    Accept,
    Reject,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeadFindingActionRequest {
    pub action: LeadFindingActionKind,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}
