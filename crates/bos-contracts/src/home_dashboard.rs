//! Home dashboard contracts: per-operator widget preferences plus the
//! server-assembled, authorization-filtered widget data rendered by Home.

use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum HomeDashboardWidgetKind {
    BusinessSummary,
    SalesPipeline,
    SystemHealth,
    HelpShortcuts,
    SystemDiagnostics,
    OpenTasks,
    ImportantEmails,
    WorkQueueEvents,
    RecentOrders,
    #[serde(rename = "financial_overview")]
    #[cfg_attr(
        not(feature = "ts"),
        serde(alias = "financials", alias = "outstanding_invoices")
    )]
    FinancialOverview,
    InventoryAlerts,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HomeDashboardWidgetState {
    Ready,
    Unavailable,
    PendingSetup,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomeDashboardWidgetPreference {
    pub kind: HomeDashboardWidgetKind,
    pub enabled: bool,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomeDashboardPreference {
    pub widgets: Vec<HomeDashboardWidgetPreference>,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub revision: Option<u64>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomeDashboardMetric {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub cents: Option<i64>,
    /// When set, this metric is a clickable count that deep-links into the
    /// named view (e.g. KPI-ribbon figures). Reuses the 7317398 target
    /// contract; absent for plain read-only metrics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<HomeDashboardTarget>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomeDashboardWidgetChartPoint {
    pub label: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub value: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<HomeDashboardTarget>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HomeDashboardWidgetChart {
    Donut {
        segments: Vec<HomeDashboardWidgetChartPoint>,
    },
    Bar {
        items: Vec<HomeDashboardWidgetChartPoint>,
    },
    Sparkline {
        points: Vec<HomeDashboardWidgetChartPoint>,
    },
    Funnel {
        stages: Vec<HomeDashboardWidgetChartPoint>,
    },
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HomeDashboardTargetView {
    Queue,
    Inbox,
    Tasks,
    Leads,
    Inventory,
    Accounting,
    Reports,
    Settings,
    Debug,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomeDashboardTarget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<HomeDashboardTargetView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_url: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomeDashboardAction {
    pub label: String,
    pub target: HomeDashboardTarget,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomeDashboardWidgetItem {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<HomeDashboardTarget>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomeDashboardWidget {
    pub kind: HomeDashboardWidgetKind,
    pub title: String,
    pub state: HomeDashboardWidgetState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub metrics: Vec<HomeDashboardMetric>,
    pub items: Vec<HomeDashboardWidgetItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<HomeDashboardAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chart: Option<HomeDashboardWidgetChart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomeDashboardResponse {
    pub preferences: HomeDashboardPreference,
    pub available_widgets: Vec<HomeDashboardWidgetKind>,
    pub widgets: Vec<HomeDashboardWidget>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomeDashboardPreferencesUpdateRequest {
    pub widgets: Vec<HomeDashboardWidgetPreference>,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HubSpotDealStageOption {
    pub stage_id: String,
    pub label: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub display_order: i32,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub probability: Option<f64>,
    pub archived: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HubSpotDealPipelineOption {
    pub pipeline_id: String,
    pub label: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub display_order: i32,
    pub archived: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub stages: Vec<HubSpotDealStageOption>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubSpotDealDatePropertyOption {
    pub name: String,
    pub label: String,
    pub field_type: String,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HubSpotDealDiscoveryResponse {
    pub configured: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub pipelines: Vec<HubSpotDealPipelineOption>,
    pub date_properties: Vec<HubSpotDealDatePropertyOption>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubSpotDealStageMapping {
    pub stage_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub status: HubSpotDealMappedStatus,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HubSpotDealMappedStatus {
    Open,
    Won,
    Lost,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubSpotDealPipelineMapping {
    pub pipeline_id: String,
    pub stage_mappings: Vec<HubSpotDealStageMapping>,
    pub started_date_property: String,
    pub closed_date_property: String,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubSpotDealPipelineMappingResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mapping: Option<HubSpotDealPipelineMapping>,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub revision: Option<u64>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubSpotDealPipelineMappingSaveRequest {
    pub mapping: HubSpotDealPipelineMapping,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}
