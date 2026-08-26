//! Shared enrichment waterfall contracts. Draft slices use these types to
//! describe field-scoped enrichment plans and durable diagnostics.

use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrichmentRunStatus {
    Started,
    Completed,
    Partial,
    Skipped,
    Failed,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrichmentTier {
    Local,
    Provider,
    WebSearch,
    Research,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrichmentMode {
    Standard,
    Research,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrichmentEligibility {
    MissingOnly,
    WeakPrefill,
    AlwaysCompare,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrichmentConfidence {
    Low,
    Medium,
    High,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrichmentFieldSpec {
    pub field_id: String,
    pub value_kind: String,
    pub eligibility: EnrichmentEligibility,
    pub min_confidence: EnrichmentConfidence,
    pub provenance_required: bool,
    pub operator_override: bool,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrichmentSeedEvidence {
    pub source_id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrichmentPlan {
    pub subject: String,
    pub fields: Vec<EnrichmentFieldSpec>,
    pub seed_evidence: Vec<EnrichmentSeedEvidence>,
    pub enabled_tiers: Vec<EnrichmentTier>,
    pub stop_policy: Vec<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrichmentTierEvent {
    pub event_type: String,
    pub tier: EnrichmentTier,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_remaining: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<EnrichmentConfidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_micros: Option<u64>,
}

impl Default for EnrichmentTierEvent {
    fn default() -> Self {
        Self {
            event_type: String::new(),
            tier: EnrichmentTier::Local,
            step: None,
            action_kind: None,
            budget_remaining: None,
            refusal_code: None,
            field_id: None,
            status: None,
            reason: None,
            source_id: None,
            url: None,
            final_url: None,
            query: None,
            rank: None,
            title: None,
            snippet: None,
            proposed_value: None,
            confidence: None,
            quote: None,
            latency_ms: None,
            bytes: None,
            cost_micros: None,
        }
    }
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrichmentFieldProposal {
    pub field_id: String,
    pub proposed_value: String,
    pub source_tier: EnrichmentTier,
    pub confidence: EnrichmentConfidence,
    pub provenance_refs: Vec<String>,
    pub accepted: bool,
    pub reason: String,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrichmentRun {
    pub run_id: String,
    pub slice_id: String,
    pub draft_id: String,
    pub item_id: String,
    pub subject: String,
    pub status: EnrichmentRunStatus,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub started_at_ms: u64,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub finished_at_ms: Option<u64>,
    pub plan: EnrichmentPlan,
    pub diagnostics: Vec<EnrichmentTierEvent>,
    pub proposals: Vec<EnrichmentFieldProposal>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub cost_micros: u64,
    pub created_by: String,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrichmentRunsResponse {
    pub runs: Vec<EnrichmentRun>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrichmentKickoffRequest {
    pub idempotency_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain_seed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<EnrichmentMode>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrichmentKickoffResponse {
    pub run_id: String,
    pub already_running: bool,
}
