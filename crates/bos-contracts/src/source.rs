//! Generic source and origin-provenance contracts.
//!
//! These are pure wire/domain types. Source-owning slices still own their
//! stores, sync budgets, connector config, and operator routes.

use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Forum,
    Reddit,
    GoogleAlert,
    FacebookGroup,
    Web,
    Drive,
    Crm,
    Accounting,
    Inventory,
    Email,
    Call,
    Manual,
    Other,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceSourceRef {
    pub source_id: String,
    pub kind: SourceKind,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceAccessMode {
    OperatorSupplied,
    ProviderSync,
    GuardedRead,
    ApprovedSourceImport,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceUsagePolicy {
    pub access_mode: EvidenceAccessMode,
    #[serde(default)]
    pub broad_access_allowed: bool,
    #[serde(default)]
    pub automated_outreach_allowed: bool,
}

impl EvidenceUsagePolicy {
    pub fn approved_source_import() -> Self {
        Self {
            access_mode: EvidenceAccessMode::ApprovedSourceImport,
            broad_access_allowed: false,
            automated_outreach_allowed: false,
        }
    }
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRecord {
    pub evidence_id: String,
    pub source: EvidenceSourceRef,
    pub policy: EvidenceUsagePolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub captured_at_ms: Option<u64>,
    pub evidence_quote: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
}

impl EvidenceRecord {
    pub fn validate_for_ai_consumption(&self) -> Result<(), &'static str> {
        if self.evidence_id.trim().is_empty() {
            return Err("evidence_id_required");
        }
        if self.source.source_id.trim().is_empty() {
            return Err("evidence_source_id_required");
        }
        if self.source.display_name.trim().is_empty() {
            return Err("evidence_source_display_name_required");
        }
        if self.evidence_quote.trim().is_empty() {
            return Err("evidence_quote_required");
        }
        if self.policy.automated_outreach_allowed {
            return Err("automated_outreach_not_allowed");
        }
        Ok(())
    }
}
