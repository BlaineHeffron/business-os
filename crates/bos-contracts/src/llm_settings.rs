//! Operator-configurable typed-LLM routing settings.

use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmRouteSettingsResponse {
    pub revision: Option<u64>,
    pub api_provider: String,
    /// Whether the local Claude CLI harness backend is available on this
    /// instance. Availability is derived from the configured default backend
    /// or a per-purpose route override selecting harness. When false the SPA
    /// hides the api/harness backend selector entirely — a deployed client
    /// instance has no harness, so "api" is the only valid backend.
    #[serde(default)]
    pub harness_available: bool,
    pub global: LlmGlobalRouteSettings,
    pub purposes: Vec<LlmPurposeRouteSettings>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmGlobalRouteSettings {
    pub backend: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub max_tokens: u32,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub timeout_ms: u64,
    pub source: String,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmPurposeRouteSettings {
    pub purpose: String,
    pub label: String,
    pub description: String,
    pub effective_backend: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub override_backend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub override_model: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmRouteSettingsUpdateRequest {
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    pub actor_id: Option<String>,
    pub global: LlmGlobalRouteSettingsUpdate,
    pub overrides: Vec<LlmPurposeRouteOverrideUpdate>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmGlobalRouteSettingsUpdate {
    pub backend: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub max_tokens: u32,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub timeout_ms: u64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmPurposeRouteOverrideUpdate {
    pub purpose: String,
    pub backend: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Claude Code subscription credential state for the configured harness.
/// Tokens and provider identifiers are intentionally never returned.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeSubscriptionStatus {
    /// The configured harness program can be invoked on this instance.
    pub available: bool,
    /// Claude Code reports a live claude.ai subscription credential.
    pub connected: bool,
    /// Safe provider-reported authentication label, normally "claude.ai".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_method: Option<String>,
    /// Safe plan label such as "pro" or "max"; no account email is exposed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription_type: Option<String>,
    /// An attended OAuth flow is currently waiting for its authorization code.
    pub authorization_pending: bool,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeSubscriptionAuthStartRequest {
    pub idempotency_key: String,
    pub actor_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeSubscriptionAuthStartResponse {
    pub flow_id: String,
    pub authorization_url: String,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeSubscriptionAuthCompleteRequest {
    pub flow_id: String,
    /// One-time code returned by Claude's authorization page. It is sent
    /// directly to the waiting CLI process and is never persisted or logged.
    pub authorization_code: String,
    pub idempotency_key: String,
    pub actor_id: Option<String>,
}
