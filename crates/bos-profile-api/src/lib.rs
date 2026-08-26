//! Contracts-tier API for statically linked BusinessOS client profiles.
//!
//! Profile crates depend on this crate, not on `bos-app`. The host owns
//! persistence, receipts, routes, approval, and outbox delivery.

pub use bos_contracts::email_identity::{
    AttentionLevel, AttentionSignal, IdentityConfidence, InboundParserResult, ParsedInbound,
    RepresentedPartyCandidate,
};
use serde::{Deserialize, Serialize};

pub trait InboundMessageParser: Send + Sync {
    fn parser_id(&self) -> &'static str;

    fn parser_version(&self) -> &'static str {
        "1"
    }

    fn parse(&self, input: &InboundParserInput) -> Option<ParsedInbound>;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboundParserInput {
    pub source_key: String,
    pub message_id: String,
    #[serde(default)]
    pub source_user_id: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    /// Safe, bounded headers selected by the host. Full/raw provider headers
    /// remain provider-side and are not exposed to profiles.
    #[serde(default)]
    pub headers: Vec<(String, String)>,
}

/// Neutral call-outcome buckets for the owner-report call-volume KPI. The host
/// owns reading enrichment rows and counting; the client report profile owns the
/// mapping from its parser's opaque `reason_code` vocabulary into these buckets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallOutcome {
    TransferSuccessful,
    CallbackNeeded,
    NoCallbackNeeded,
    Unknown,
}

/// Client-owned owner-report assembly. v1 surface: classify a parser-owned
/// attention `reason_code` into a neutral [`CallOutcome`]. Keeps client-specific
/// reporting vocabulary out of generic `owner_reports`. Future revisions will
/// grow this trait to own more of the per-client report assembly.
pub trait ClientReportProfile: Send + Sync {
    fn profile_id(&self) -> &'static str;

    fn classify_call_reason(&self, reason_code: Option<&str>) -> CallOutcome;
}

pub trait QuoteWorkflowProfile: Send + Sync {
    fn profile_id(&self) -> &'static str;

    fn parse_config(
        &self,
        raw: serde_json::Value,
    ) -> Result<QuoteProfileConfig, QuoteProfileError> {
        Ok(QuoteProfileConfig {
            profile_id: self.profile_id().to_string(),
            settings: raw,
        })
    }

    fn run(
        &self,
        input: QuoteProfileInput,
        config: QuoteProfileConfig,
    ) -> Result<QuoteProfileRun, QuoteProfileError>;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuoteProfileConfig {
    pub profile_id: String,
    #[serde(default)]
    pub settings: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuoteProfileInput {
    pub source_kind: String,
    pub source_ref: String,
    pub customer_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer_tier: Option<String>,
    pub request_text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuoteProfileRun {
    pub steps: Vec<QuoteProfileStep>,
    pub draft: QuoteProfileDraft,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuoteProfileStep {
    pub node: String,
    pub kind: QuoteProfileStepKind,
    #[serde(default)]
    pub inputs: Vec<TracedValue>,
    #[serde(default)]
    pub outputs: Vec<TracedValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuoteProfileStepKind {
    Read,
    Deterministic,
    Grounding,
    Policy,
    Stage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TracedValue {
    pub label: String,
    pub value: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub formula: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuoteProfileDraft {
    pub summary: String,
    pub line_items: Vec<QuoteProfileLineItem>,
    #[serde(default)]
    pub policy_notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuoteProfileLineItem {
    pub sku: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_line: Option<String>,
    pub description: String,
    pub quantity: u32,
    pub unit_cents: i64,
    pub total_cents: i64,
    pub source_quote: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuoteProfileError {
    pub code: String,
    pub message: String,
}

impl QuoteProfileError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for QuoteProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for QuoteProfileError {}
