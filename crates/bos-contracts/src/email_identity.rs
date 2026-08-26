//! Generic inbound-email identity enrichment contracts.
//!
//! Client-specific parsers may suggest who an email represents and whether it
//! carries attention/routing signals. Field names stay provider-neutral: the
//! parser id and opaque reason codes carry client/provider specificity.

use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityConfidence {
    Low,
    #[default]
    Medium,
    High,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepresentedPartyCandidate {
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub phone: Option<String>,
    #[serde(default)]
    pub company: Option<String>,
    /// Opaque parser-local source label, e.g. "field:email" or "form:name".
    pub provenance: String,
    pub confidence: IdentityConfidence,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionLevel {
    Lower,
    #[default]
    Normal,
    Higher,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttentionSignal {
    pub level: AttentionLevel,
    /// Opaque parser-local reason code. Generic code must not match on values.
    pub reason_code: String,
    /// Parser-supplied operator-facing summary. This lets client-specific
    /// parsers explain the signal without teaching generic UI about reason
    /// codes.
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
    pub provenance: String,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedInbound {
    #[serde(default)]
    pub represented_parties: Vec<RepresentedPartyCandidate>,
    #[serde(default)]
    pub attention_signals: Vec<AttentionSignal>,
    /// Optional neutral title/summary hints for operator-facing work items.
    #[serde(default)]
    pub title_hint: Option<String>,
    #[serde(default)]
    pub summary_hint: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboundParserResult {
    pub parser_id: String,
    pub parsed: ParsedInbound,
}
