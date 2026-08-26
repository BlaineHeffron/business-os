//! Shopify sales cache contracts: connector status, sync state, and cached
//! order/customer read models. The browser and future grounding tools read
//! local snapshots only; Shopify is touched only by the sync path.

use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShopifySalesConnectorStatus {
    pub configured: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shop_domain: Option<String>,
    pub has_synced: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShopifySalesSyncInfo {
    pub sync_enabled: bool,
    pub in_flight: bool,
    pub backfill_complete: bool,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub last_synced_at_ms: Option<u64>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub order_count: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub customer_count: u64,
    pub last_requests_used: u32,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub next_sync_allowed_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShopifyOrderLineItemSummary {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sku: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub quantity: i64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShopifyOrderSnapshotRow {
    pub order_id: String,
    pub order_number: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer_email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub total_cents: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub financial_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fulfillment_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracking_number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carrier: Option<String>,
    pub line_items_summary: String,
    pub line_items: Vec<ShopifyOrderLineItemSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShopifyCustomerSnapshotRow {
    pub customer_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub total_spent_cents: Option<i64>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub orders_count: i64,
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShopifyOrdersResponse {
    pub orders: Vec<ShopifyOrderSnapshotRow>,
    pub sync: ShopifySalesSyncInfo,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShopifyCustomersResponse {
    pub customers: Vec<ShopifyCustomerSnapshotRow>,
    pub sync: ShopifySalesSyncInfo,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShopifySalesSyncNowResponse {
    pub accepted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub next_allowed_at_ms: u64,
}
