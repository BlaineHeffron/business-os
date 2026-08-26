//! Cached CRM contact/deal read models. These are local snapshot views only;
//! sync is the only path that talks to the CRM provider.

use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrmCacheSyncInfo {
    pub provider: String,
    pub sync_enabled: bool,
    pub in_flight: bool,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub contact_count: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub deal_count: u64,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub last_synced_at_ms: Option<u64>,
    pub last_requests_used: u32,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub next_sync_allowed_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrmCacheSyncNowResponse {
    pub accepted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub next_allowed_at_ms: u64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrmContactSnapshotsResponse {
    pub contacts: Vec<CrmContactSnapshot>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrmDealSnapshotsResponse {
    pub deals: Vec<CrmDealSnapshot>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrmContextResponse {
    pub contacts: Vec<CrmContactSnapshot>,
    pub deals: Vec<CrmDealSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lookup_email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped_reason: Option<String>,
    pub hubspot_links_configured: bool,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrmContactSnapshot {
    pub provider: String,
    pub provider_contact_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_stage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity_at: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrmDealSnapshot {
    pub provider: String,
    pub provider_deal_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deal_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub amount_cents: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    pub amount_visible: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub associated_contact_email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub associated_contact_company: Option<String>,
}
