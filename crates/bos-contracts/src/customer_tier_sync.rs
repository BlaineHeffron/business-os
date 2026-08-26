//! QBO customer-tier to Shopify customer-segment sync contracts. QBO remains
//! source of truth; Shopify writes are staged as operator-approved outbox jobs
//! and dry-run until the Shopify write gate opens.

use serde::{Deserialize, Serialize};

use crate::calendar_drafts::OutboxJobSummary;

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomerTierSyncPreviewRequest {
    pub idempotency_key: String,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomerTierSyncApproveRequest {
    pub idempotency_key: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub expected_revision: u64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomerTierSyncRun {
    pub run_id: String,
    pub status: CustomerTierSyncStatus,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub revision: u64,
    pub plan: CustomerTierSyncPlan,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outbox_job_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outbox_job: Option<OutboxJobSummary>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub created_at_ms: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub updated_at_ms: u64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CustomerTierSyncStatus {
    Staged,
    Approved,
    Rejected,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomerTierSyncPlan {
    pub source_provider: String,
    pub target_provider: String,
    pub mapping_version: String,
    pub actions: Vec<CustomerTierSyncAction>,
    pub skipped: Vec<CustomerTierSyncSkippedCustomer>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomerTierSyncAction {
    pub qbo_customer_id: String,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    pub qbo_tier: String,
    pub shopify: ShopifyTierTarget,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShopifyTierTarget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclusive_tag_prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metafield_namespace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metafield_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metafield_value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segment_query: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomerTierSyncSkippedCustomer {
    pub qbo_customer_id: String,
    pub display_name: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qbo_tier: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomerTierSyncListResponse {
    pub runs: Vec<CustomerTierSyncRun>,
}
