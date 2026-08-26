//! Owner-digest pump: regenerates the current weekly + month-to-date
//! reports when missing or stale (assembled for a previous day), then stages
//! configured scheduled deliveries as Gmail drafts. Off unless
//! BOS_REPORT_DIGEST_ENABLED; delivery additionally requires the explicit
//! delivery gate plus recipients and a due schedule. Generate-now shares the
//! same guarded generation cycle with force=true but does not auto-send.

use std::time::Duration;

use bos_contracts::owner_reports::OwnerDigestMetrics;
use bos_contracts::receipt::ActorKindDto;

use super::service::{self, DigestNarration, DigestPeriod};
use crate::env_registry;
use crate::http::{now_ms, AppState};
use crate::store_core::StoreError;

/// Minimum gap between digest generation cycles (manual or pump).
pub const GENERATE_COOLDOWN_MS: u64 = 60_000;

pub struct DigestPumpConfig {
    pub enabled: bool,
    pub interval: Duration,
}

pub fn config_from_settings(
    conn: &rusqlite::Connection,
    client_id: &str,
) -> Result<DigestPumpConfig, StoreError> {
    Ok(DigestPumpConfig {
        enabled: crate::slices::admin_settings::service::flag(
            conn,
            client_id,
            &env_registry::BOS_REPORT_DIGEST_ENABLED,
        )?,
        interval: Duration::from_secs(
            crate::slices::admin_settings::service::usize_or(
                conn,
                client_id,
                &env_registry::BOS_REPORT_DIGEST_INTERVAL_SECS,
                21_600,
            )?
            .max(600) as u64,
        ),
    })
}

pub fn spawn(state: AppState) {
    if !state.slice_enabled(super::SLICE.id) {
        tracing::info!("owner-digest pump not started (owner_reports disabled by client overlay)");
        return;
    }
    std::thread::Builder::new()
        .name("owner-digest-pump".to_string())
        .spawn(move || {
            tracing::info!("owner-digest pump started");
            loop {
                let config = {
                    let persistence = state.persistence.lock();
                    match config_from_settings(persistence.connection_ref(), &state.client_id) {
                        Ok(config) => config,
                        Err(err) => {
                            tracing::warn!(error = %err, "owner-digest config read failed");
                            DigestPumpConfig {
                                enabled: false,
                                interval: Duration::from_secs(21_600),
                            }
                        }
                    }
                };
                if config.enabled && try_begin_generate(&state, now_ms()).is_ok() {
                    match run_guarded_generate(&state, false) {
                        Ok(summary) if summary.generated > 0 => tracing::info!(
                            generated = summary.generated,
                            skipped = summary.skipped,
                            narration_failures = summary.narration_failures,
                            "owner-digest cycle complete"
                        ),
                        Ok(_) => {}
                        Err(err) => tracing::warn!(error = %err, "owner-digest cycle failed"),
                    }
                }
                std::thread::sleep(config.interval);
            }
        })
        .expect("spawn owner-digest-pump thread");
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct GenerateSummary {
    pub generated: usize,
    pub skipped: usize,
    pub narration_failures: usize,
    pub delivered: usize,
    pub delivery_skipped: usize,
}

/// Claim the generation slot. Err = a generation is running or cooling down.
pub fn try_begin_generate(state: &AppState, now: u64) -> Result<(), &'static str> {
    let mut status = state
        .sync_guards
        .guard(crate::http::Pump::ReportGenerate)
        .lock();
    if status.in_flight {
        return Err("generate_in_flight");
    }
    if now < status.next_allowed_at_ms {
        return Err("generate_cooldown");
    }
    status.in_flight = true;
    status.last_attempt_ms = Some(now);
    Ok(())
}

/// Run one metric-only cycle and release the slot. Caller must
/// hold the slot via [`try_begin_generate`]. `force` regenerates every
/// current period (Generate-now); the pump only refreshes stale ones.
pub fn run_guarded_generate(state: &AppState, force: bool) -> Result<GenerateSummary, String> {
    let result = run_cycle_with(state, force, now_ms(), &disabled_narrator());
    let mut status = state
        .sync_guards
        .guard(crate::http::Pump::ReportGenerate)
        .lock();
    status.in_flight = false;
    status.next_allowed_at_ms = now_ms() + GENERATE_COOLDOWN_MS;
    status.last_outcome = Some(match &result {
        Ok(summary) if summary.narration_failures > 0 => {
            format!("ok ({} narration failed)", summary.narration_failures)
        }
        Ok(_) => "ok".to_string(),
        Err(err) => format!("error: {err}"),
    });
    result
}

/// The narration seam: period + metrics in, validated narration + model out.
/// The live arm is the ONE LLM call; tests inject a deterministic one.
pub type Narrator<'a> = dyn Fn(&DigestPeriod, &OwnerDigestMetrics, &str) -> Result<(DigestNarration, String), String>
    + 'a;

fn disabled_narrator(
) -> impl Fn(&DigestPeriod, &OwnerDigestMetrics, &str) -> Result<(DigestNarration, String), String>
{
    |_, _, _| {
        Ok((
            DigestNarration {
                headline: String::new(),
                narrative: String::new(),
                callouts: Vec::new(),
                confidence: String::new(),
            },
            String::new(),
        ))
    }
}

/// The testable cycle core: assemble each current period's metrics from the
/// local caches, narrate through the injected seam, upsert the row, then
/// optionally stage scheduled delivery. Metrics assembly holds the persistence
/// lock; the narration call does NOT.
pub fn run_cycle_with(
    state: &AppState,
    force: bool,
    now: u64,
    narrator: &Narrator<'_>,
) -> Result<GenerateSummary, String> {
    let today = crate::slices::accounting::service::today_string(now);
    let report_config = service::config_from_sources(state.owner_reports_overlay.as_ref().as_ref());
    let call_volume_config = service::CallVolumeMetricConfig::from_overlay(
        state.owner_reports_overlay.as_ref().as_ref(),
    );
    let accounting_metric_config =
        crate::slices::accounting::service::metric_basis_config_from_sources(Some(
            &state.accounting_overlay.metric_basis,
        ))
        .map_err(|err| err.to_string())?;
    let mut summary = GenerateSummary::default();
    for period in service::current_periods(&today) {
        let report_id = service::report_id_for(period.kind, &period.start);
        let mut generated_report = None;
        let local_metrics = {
            let accounting_status = state
                .sync_guards
                .guard(crate::http::Pump::Accounting)
                .lock()
                .clone();
            let persistence = state.persistence.lock();
            let conn = persistence.connection_ref();
            if !force {
                let as_of = super::store::report_as_of(conn, &state.client_id, &report_id)
                    .map_err(|err| err.to_string())?;
                if as_of.as_deref() == Some(today.as_str()) {
                    summary.skipped += 1;
                    None
                } else {
                    Some(
                        service::assemble_local_metrics(
                            conn,
                            &state.client_id,
                            &accounting_status,
                            service::MetricAssemblyConfig {
                                report: &report_config,
                                call_volume: &call_volume_config,
                                accounting_metric: &accounting_metric_config,
                                search_console_overlay: state
                                    .search_console_overlay
                                    .as_ref()
                                    .as_ref(),
                            },
                            &period,
                            &today,
                        )
                        .map_err(|err| err.to_string())?,
                    )
                }
            } else {
                Some(
                    service::assemble_local_metrics(
                        conn,
                        &state.client_id,
                        &accounting_status,
                        service::MetricAssemblyConfig {
                            report: &report_config,
                            call_volume: &call_volume_config,
                            accounting_metric: &accounting_metric_config,
                            search_console_overlay: state.search_console_overlay.as_ref().as_ref(),
                        },
                        &period,
                        &today,
                    )
                    .map_err(|err| err.to_string())?,
                )
            }
        };
        if let Some(mut metrics) = local_metrics {
            let (start_ms, end_ms) =
                service::period_bounds_ms(&period).map_err(|err| err.to_string())?;
            if report_config
                .metrics
                .contains(&service::ReportMetricSection::CloseRate)
            {
                let deal_config = {
                    let persistence = state.persistence.lock();
                    service::hubspot_deal_config_for_client(
                        persistence.connection_ref(),
                        &state.client_id,
                    )
                };
                metrics.deals = service::assemble_hubspot_deal_metrics_with_config(
                    deal_config,
                    start_ms,
                    end_ms,
                );
            }
            let narration = narrator(&period, &metrics, &report_id);
            if let Err(error) = &narration {
                summary.narration_failures += 1;
                tracing::warn!(
                    report_id = %report_id,
                    error = %error,
                    "owner-digest narration failed (metrics stored without prose)"
                );
            }
            let report = service::report_from_parts(&period, metrics, narration, now);
            {
                let mut persistence = state.persistence.lock();
                super::store::upsert_report(
                    persistence.connection(),
                    &state.client_id,
                    super::store::PUMP_ACTOR,
                    &report,
                )
                .map_err(|err| err.to_string())?;
            }
            generated_report = Some(report);
            summary.generated += 1;
        }
        if !force && service::due_for_scheduled_delivery(&period, &today, &report_config) {
            match stage_scheduled_delivery(
                state,
                &report_id,
                generated_report.as_ref(),
                &report_config,
                now,
                &today,
            )? {
                true => summary.delivered += 1,
                false => summary.delivery_skipped += 1,
            }
        }
    }
    Ok(summary)
}

fn stage_scheduled_delivery(
    state: &AppState,
    report_id: &str,
    generated_report: Option<&bos_contracts::owner_reports::OwnerReport>,
    config: &service::OwnerReportConfig,
    now: u64,
    today: &str,
) -> Result<bool, String> {
    let Some(to_addr) = service::recipients_line(config) else {
        tracing::warn!("owner-report scheduled delivery due but recipients are unset");
        return Ok(false);
    };
    let today_start = crate::slices::accounting::service::date_to_epoch_ms(today)
        .ok_or_else(|| "owner_report_bad_today".to_string())?;
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    if super::store::email_job_count_since(conn, &state.client_id, report_id, today_start)
        .map_err(|err| err.to_string())?
        > 0
    {
        return Ok(false);
    }
    let report = match generated_report {
        Some(report) => report.clone(),
        None => super::store::get_report(conn, &state.client_id, report_id)
            .map_err(|err| err.to_string())?
            .map(|entry| entry.report)
            .ok_or_else(|| "owner_report_not_found".to_string())?,
    };
    let job = service::build_email_job_with_config(
        &report,
        &to_addr,
        None,
        super::store::PUMP_ACTOR,
        now,
        config,
    )?;
    let idempotency_key = format!("scheduled:{}:{}", report.report_id, report.as_of_date);
    let ctx = super::store::EmailActionContext {
        client_id: &state.client_id,
        actor_id: super::store::PUMP_ACTOR,
        actor_kind: ActorKindDto::System,
        expected_revision: None,
        idempotency_key: &idempotency_key,
        now_ms: now,
    };
    match super::store::stage_email(conn, ctx, &report.report_id, &job) {
        Ok(_) => Ok(true),
        Err(err)
            if err
                .to_string()
                .contains("owner_report_email_already_staged") =>
        {
            Ok(false)
        }
        Err(err) => Err(err.to_string()),
    }
}
