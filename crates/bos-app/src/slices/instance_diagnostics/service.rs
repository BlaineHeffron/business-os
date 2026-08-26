//! Health assembly: pump guard snapshots + windowed error rollups. The
//! status derivation is pure; the clock is always passed in.

use bos_contracts::instance_diagnostics::{InstanceHealth, PumpStatusDto, ReadyzResponse};

use super::store;
use crate::http::{AppState, OperatorScope};
use crate::store_core::StoreError;

pub const HOUR_MS: u64 = 60 * 60 * 1000;
pub const DAY_MS: u64 = 24 * HOUR_MS;

/// Snapshot every pump guard. Locks are taken one at a time and dropped
/// immediately — this must never hold up a sync cycle.
pub fn pump_statuses(state: &AppState) -> Vec<PumpStatusDto> {
    let accounting = state
        .sync_guards
        .guard(crate::http::Pump::Accounting)
        .lock()
        .clone();
    let stockforge = state
        .sync_guards
        .guard(crate::http::Pump::Stockforge)
        .lock()
        .clone();
    let crm_cache = state
        .sync_guards
        .guard(crate::http::Pump::CrmCache)
        .lock()
        .clone();
    let drive = state
        .sync_guards
        .guard(crate::http::Pump::Drive)
        .lock()
        .clone();
    let claims = state
        .sync_guards
        .guard(crate::http::Pump::Claims)
        .lock()
        .clone();
    let call_input_transcription = state
        .sync_guards
        .guard(crate::http::Pump::CallInputTranscription)
        .lock()
        .clone();
    let data_retention = state
        .sync_guards
        .guard(crate::http::Pump::DataRetention)
        .lock()
        .clone();
    let enrichment_freshness = state
        .sync_guards
        .guard(crate::http::Pump::EnrichmentFreshness)
        .lock()
        .clone();
    let report = state
        .sync_guards
        .guard(crate::http::Pump::ReportGenerate)
        .lock()
        .clone();
    let shopify_sales = state
        .sync_guards
        .guard(crate::http::Pump::ShopifySales)
        .lock()
        .clone();
    vec![
        PumpStatusDto {
            pump: "accounting_sync".to_string(),
            in_flight: accounting.in_flight,
            last_attempt_ms: accounting.last_attempt_ms,
            last_outcome: accounting.last_outcome,
            next_allowed_at_ms: accounting.next_allowed_at_ms,
        },
        PumpStatusDto {
            pump: "stockforge_sync".to_string(),
            in_flight: stockforge.in_flight,
            last_attempt_ms: stockforge.last_attempt_ms,
            last_outcome: stockforge.last_outcome,
            next_allowed_at_ms: stockforge.next_allowed_at_ms,
        },
        PumpStatusDto {
            pump: "crm_cache_sync".to_string(),
            in_flight: crm_cache.in_flight,
            last_attempt_ms: crm_cache.last_attempt_ms,
            last_outcome: crm_cache.last_outcome,
            next_allowed_at_ms: crm_cache.next_allowed_at_ms,
        },
        PumpStatusDto {
            pump: "drive_sync".to_string(),
            in_flight: drive.in_flight,
            last_attempt_ms: drive.last_attempt_ms,
            last_outcome: drive.last_outcome,
            next_allowed_at_ms: drive.next_allowed_at_ms,
        },
        PumpStatusDto {
            pump: "claims_sync".to_string(),
            in_flight: claims.in_flight,
            last_attempt_ms: claims.last_attempt_ms,
            last_outcome: claims.last_outcome,
            next_allowed_at_ms: claims.next_allowed_at_ms,
        },
        PumpStatusDto {
            pump: "call_input_transcription".to_string(),
            in_flight: call_input_transcription.in_flight,
            last_attempt_ms: call_input_transcription.last_attempt_ms,
            last_outcome: call_input_transcription.last_outcome,
            next_allowed_at_ms: call_input_transcription.next_allowed_at_ms,
        },
        PumpStatusDto {
            pump: "data_retention".to_string(),
            in_flight: data_retention.in_flight,
            last_attempt_ms: data_retention.last_attempt_ms,
            last_outcome: data_retention.last_outcome,
            next_allowed_at_ms: data_retention.next_allowed_at_ms,
        },
        PumpStatusDto {
            pump: "enrichment_freshness".to_string(),
            in_flight: enrichment_freshness.in_flight,
            last_attempt_ms: enrichment_freshness.last_attempt_ms,
            last_outcome: enrichment_freshness.last_outcome,
            next_allowed_at_ms: enrichment_freshness.next_allowed_at_ms,
        },
        PumpStatusDto {
            pump: "report_generate".to_string(),
            in_flight: report.in_flight,
            last_attempt_ms: report.last_attempt_ms,
            last_outcome: report.last_outcome,
            next_allowed_at_ms: report.next_allowed_at_ms,
        },
        PumpStatusDto {
            pump: "shopify_sales_sync".to_string(),
            in_flight: shopify_sales.in_flight,
            last_attempt_ms: shopify_sales.last_attempt_ms,
            last_outcome: shopify_sales.last_outcome,
            next_allowed_at_ms: shopify_sales.next_allowed_at_ms,
        },
    ]
}

/// "degraded" when any pump's last outcome reports an error, or terminal
/// outbox jobs exist. Deliberately cheap and local — trend judgement
/// (down/unreachable, spikes) belongs to the hub's anomaly detection.
pub fn derive_status(pumps: &[PumpStatusDto], terminal_outbox_jobs: u64) -> &'static str {
    let pump_error = pumps.iter().any(|pump| {
        pump.last_outcome
            .as_deref()
            .is_some_and(|outcome| outcome.starts_with("error"))
    });
    if pump_error || terminal_outbox_jobs > 0 {
        "degraded"
    } else {
        "ok"
    }
}

/// Cheap liveness: pump guards + schema version, no rollup queries (so the
/// outbox does not factor in here — only `/api/diagnostics/health` does).
pub fn readyz(state: &AppState, now_ms: u64) -> Result<ReadyzResponse, StoreError> {
    let pumps = pump_statuses(state);
    let persistence = state.persistence.lock();
    let conn = persistence.connection_ref();
    Ok(ReadyzResponse {
        client_id: state.client_id.to_string(),
        display_name: state.display_name.to_string(),
        status: derive_status(&pumps, 0).to_string(),
        schema_version: state.schema_version,
        uptime_ms: now_ms.saturating_sub(state.started_at_ms),
        enabled_slices: state.enabled_slice_ids(),
        auto_produce_enabled: crate::slices::admin_settings::service::flag(
            conn,
            &state.client_id,
            &crate::env_registry::BOS_AUTO_PRODUCE_ENABLED,
        )?,
        ai_triage_enabled: crate::slices::admin_settings::service::flag(
            conn,
            &state.client_id,
            &crate::env_registry::BOS_AI_TRIAGE_ENABLED,
        )?,
        agent_launch_enabled: crate::env_registry::flag(
            &crate::env_registry::BOS_AGENT_LAUNCH_ENABLED,
        ),
    })
}

/// The full signal: identity, pumps, outbox backlog, 1h/24h error rollups.
pub fn health(
    state: &AppState,
    scope: &OperatorScope,
    actor_id: &str,
    now_ms: u64,
) -> Result<InstanceHealth, StoreError> {
    let pumps = pump_statuses(state);
    let (schema_version, outbox, errors_1h, errors_24h) = {
        let persistence = state.persistence.lock();
        let conn = persistence.connection_ref();
        (
            store::schema_version(conn)?,
            store::outbox_backlog(conn, &state.client_id)?,
            store::error_rollup(conn, &state.client_id, now_ms, HOUR_MS)?,
            store::error_rollup(conn, &state.client_id, now_ms, DAY_MS)?,
        )
    };
    let enabled_slices = state.enabled_slice_ids();
    let visible_slices = crate::operator_visibility::visible_slice_ids(state, scope, actor_id)?;
    Ok(InstanceHealth {
        client_id: state.client_id.to_string(),
        display_name: state.display_name.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        build_sha: crate::env_registry::string(&crate::env_registry::BOS_BUILD_SHA),
        started_at_ms: state.started_at_ms,
        uptime_ms: now_ms.saturating_sub(state.started_at_ms),
        now_ms,
        schema_version,
        status: derive_status(&pumps, outbox.terminal_jobs).to_string(),
        pumps,
        outbox,
        errors_1h,
        errors_24h,
        enabled_slices,
        visible_slices,
    })
}
