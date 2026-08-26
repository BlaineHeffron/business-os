//! AI usage log contracts: per-call token/latency accounting for every typed
//! LLM execution (API and harness routes), surfaced to the operator.

use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiUsageRow {
    pub usage_id: String,
    /// What the call was for ("email_ai_triage", "calendar_event_extract", ...).
    pub purpose: String,
    /// "api" | "harness".
    pub route: String,
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub tokens_in: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub tokens_out: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub total_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub cost_micros: Option<u64>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub latency_ms: u64,
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    pub correlation_id: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub recorded_at_ms: u64,
}

/// Aggregates over a window of usage rows.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiUsageTotals {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub calls: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub failures: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub tokens_in: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub tokens_out: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub cost_micros: u64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiUsageResponse {
    pub rows: Vec<AiUsageRow>,
    pub totals_all_time: AiUsageTotals,
    pub totals_last_24h: AiUsageTotals,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_row_round_trips() {
        let row = AiUsageRow {
            usage_id: "aiu_1".into(),
            purpose: "calendar_event_extract".into(),
            route: "api".into(),
            provider: "anthropic".into(),
            model: "claude-sonnet-4-6".into(),
            tokens_in: Some(1200),
            tokens_out: Some(300),
            total_tokens: Some(1500),
            cost_micros: None,
            latency_ms: 2400,
            success: true,
            error_code: None,
            correlation_id: "wi_email_m1".into(),
            recorded_at_ms: 1,
        };
        let json = serde_json::to_string(&row).expect("serialize");
        let back: AiUsageRow = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(row, back);
    }
}
