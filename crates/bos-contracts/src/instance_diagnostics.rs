//! Instance diagnostics contracts: the structured health surface every
//! instance exposes for the support hub — `/readyz` (unauthenticated
//! liveness) and `/api/diagnostics/health` (operator-gated full signal).

use serde::{Deserialize, Serialize};

/// `GET /readyz` — unauthenticated structured liveness.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadyzResponse {
    pub client_id: String,
    /// Client-facing brand name from the overlay identity ("Example Company");
    /// the SPA titles itself with it. Empty = pre-branding payload.
    #[serde(default)]
    pub display_name: String,
    /// "ok" | "degraded".
    pub status: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub schema_version: u32,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub uptime_ms: u64,
    // Slice ids enabled for this client (from the overlay). The SPA uses this
    // to hide UI for slices a client doesn't run (e.g. Settings sections). Not
    // sensitive: feature names only, no secrets.
    #[serde(default)]
    pub enabled_slices: Vec<String>,
    /// True when accepting a work item may kick automatic draft production.
    #[serde(default)]
    pub auto_produce_enabled: bool,
    /// True when unmatched email can be examined by the bounded AI triage pass.
    #[serde(default)]
    pub ai_triage_enabled: bool,
    /// True when the operator may launch a Agent Monitor agent session from a
    /// work item (operator power tool; off on client instances).
    #[serde(default)]
    pub agent_launch_enabled: bool,
}

/// One background pump's in-memory guard state.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PumpStatusDto {
    /// "accounting_sync" | "stockforge_sync" | "drive_sync" | "claims_sync"
    /// | "report_generate".
    pub pump: String,
    pub in_flight: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub last_attempt_ms: Option<u64>,
    /// "ok" / "ok (...)" / "rate-limited ..." / "error: ..."; None = never ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_outcome: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub next_allowed_at_ms: u64,
}

/// LLM failures within the window, grouped by purpose + error code.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmErrorRollupDto {
    pub purpose: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub count: u64,
}

/// Error counts over one trailing window, computed from existing tables
/// (receipts, ai_usage_log) — nothing is stored for this.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorRollupDto {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub window_ms: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub failed_receipts: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub conflict_receipts: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub llm_failures: u64,
    pub llm_errors: Vec<LlmErrorRollupDto>,
}

/// Point-in-time outbox backlog (terminal jobs persist, so no window).
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxBacklogDto {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub pending_jobs: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub terminal_jobs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_terminal_error: Option<String>,
}

/// `GET /api/diagnostics/health` — the full operator-gated signal the
/// support hub polls.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceHealth {
    pub client_id: String,
    /// Client-facing brand name from the overlay identity.
    #[serde(default)]
    pub display_name: String,
    /// Server crate version (build identity).
    pub version: String,
    /// Git sha of the deployed build (BOS_BUILD_SHA, stamped by the deploy
    /// image). None = local/unstamped build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_sha: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub started_at_ms: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub uptime_ms: u64,
    /// Instance clock at response time (lets the hub spot skew).
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub now_ms: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub schema_version: u32,
    /// "ok" | "degraded" — the instance's own cheap readiness; deeper
    /// judgement (down/unreachable, trends) is the hub's job.
    pub status: String,
    pub pumps: Vec<PumpStatusDto>,
    pub outbox: OutboxBacklogDto,
    pub errors_1h: ErrorRollupDto,
    pub errors_24h: ErrorRollupDto,
    pub enabled_slices: Vec<String>,
    /// Enabled slices after applying the authenticated operator's visibility
    /// policy. The SPA uses this to hide tabs whose routes would reject this
    /// user.
    #[serde(default)]
    pub visible_slices: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_health_round_trips() {
        let health = InstanceHealth {
            client_id: "Example Company".into(),
            display_name: "Example Company".into(),
            version: "0.1.0".into(),
            build_sha: Some("e53debe0".into()),
            started_at_ms: 1_000,
            uptime_ms: 60_000,
            now_ms: 61_000,
            schema_version: 34,
            status: "degraded".into(),
            pumps: vec![PumpStatusDto {
                pump: "accounting_sync".into(),
                in_flight: false,
                last_attempt_ms: Some(50_000),
                last_outcome: Some("error: provider timeout".into()),
                next_allowed_at_ms: 70_000,
            }],
            outbox: OutboxBacklogDto {
                pending_jobs: 2,
                terminal_jobs: 1,
                last_terminal_error: Some("permanent_rejection".into()),
            },
            errors_1h: ErrorRollupDto {
                window_ms: 3_600_000,
                failed_receipts: 1,
                conflict_receipts: 0,
                llm_failures: 2,
                llm_errors: vec![LlmErrorRollupDto {
                    purpose: "email_ai_triage".into(),
                    error_code: Some("llm_timeout".into()),
                    count: 2,
                }],
            },
            errors_24h: ErrorRollupDto {
                window_ms: 86_400_000,
                failed_receipts: 3,
                conflict_receipts: 1,
                llm_failures: 2,
                llm_errors: vec![],
            },
            enabled_slices: vec!["accounting".into(), "work_queue".into()],
            visible_slices: vec!["work_queue".into()],
        };
        let json = serde_json::to_string(&health).expect("serialize");
        let back: InstanceHealth = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(health, back);
    }

    #[test]
    fn readyz_round_trips() {
        let readyz = ReadyzResponse {
            client_id: "dev".into(),
            display_name: "BusinessOS".into(),
            status: "ok".into(),
            schema_version: 34,
            uptime_ms: 5_000,
            enabled_slices: vec!["work_queue".into(), "ai_usage".into()],
            auto_produce_enabled: true,
            ai_triage_enabled: true,
            agent_launch_enabled: false,
        };
        let json = serde_json::to_string(&readyz).expect("serialize");
        let back: ReadyzResponse = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(readyz, back);
    }
}
