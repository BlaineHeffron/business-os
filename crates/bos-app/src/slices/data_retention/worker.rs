//! Automatic retention pump and guarded manual kickoff.

use std::time::{Duration, Instant};

use bos_contracts::data_retention::{DataRetentionRunResponse, DataRetentionRunStatus};
use bos_contracts::receipt::ActorKindDto;

use super::{service, store};
use crate::http::{now_ms, AppState, Pump};
use crate::slices::async_kickoff::{
    self, KickoffCapacity, KickoffDecision, KickoffSpec, RecordedKickoff,
};
use crate::store_core::{MutationOutcome, StoreError};

const MANUAL_DRAFT_ID: &str = "data_retention_manual";
const MANUAL_CAPACITY_GROUP: &str = "data_retention";

pub fn spawn(state: AppState) {
    if !state.slice_enabled(super::SLICE.id) {
        tracing::info!("data retention pump not started because the slice is disabled");
        return;
    }
    std::thread::Builder::new()
        .name("data-retention-pump".to_string())
        .spawn(move || {
            tracing::info!("data retention pump started");
            loop {
                let config = read_config(&state).unwrap_or_else(|err| {
                    tracing::warn!(error = %err, "data retention config read failed");
                    service::RetentionConfig {
                        enabled: false,
                        interval: Duration::from_secs(21_600),
                        email_body_days: 90,
                        receipt_payload_days: 90,
                        batch_size: 200,
                        max_rows_per_cycle: 5_000,
                        incremental_vacuum_pages: 256,
                    }
                });
                let started_at_ms = now_ms();
                if config.enabled && try_begin_run(&state, started_at_ms).is_ok() {
                    let run_id = format!("retention_cycle_{started_at_ms}");
                    let report = run_guarded_cycle(
                        &state,
                        &config,
                        &service::RunActor::system(),
                        &run_id,
                        started_at_ms,
                    );
                    log_report(&report, "automatic data retention cycle complete");
                }
                std::thread::sleep(config.interval);
            }
        })
        .expect("spawn data-retention-pump thread");
}

pub fn start_manual_run(
    state: &AppState,
    actor_id: String,
    idempotency_key: &str,
    started_at_ms: u64,
) -> Result<DataRetentionRunResponse, StoreError> {
    if try_begin_run(state, started_at_ms).is_err() {
        return Ok(DataRetentionRunResponse {
            status: DataRetentionRunStatus::AlreadyRunning,
            run_id: None,
            reason: Some("retention_in_flight".to_string()),
        });
    }
    let config = match read_config(state) {
        Ok(config) => config,
        Err(err) => {
            release_without_run(state, Some(format!("error: {err}")));
            return Err(err);
        }
    };
    let planned_run_id = service::manual_run_id(idempotency_key);
    let decision = async_kickoff::begin(
        KickoffSpec {
            slice_id: super::SLICE.id,
            draft_id: MANUAL_DRAFT_ID,
            planned_run_id: &planned_run_id,
            capacity: KickoffCapacity::Limited {
                group: MANUAL_CAPACITY_GROUP,
                max_concurrent: 1,
            },
        },
        || {
            let mut persistence = state.persistence.lock();
            let outcome = store::record_manual_kickoff(
                persistence.connection(),
                &state.client_id,
                store::ManualKickoff {
                    run_id: &planned_run_id,
                    actor_id: &actor_id,
                    idempotency_key,
                    now_ms: started_at_ms,
                },
            )?;
            Ok::<RecordedKickoff, StoreError>(RecordedKickoff {
                run_id: outcome.run_id,
                replayed: matches!(outcome.mutation, MutationOutcome::ReplayedIdempotent { .. }),
            })
        },
    );
    match decision {
        Ok(KickoffDecision::Spawn { run_id, guard }) => {
            let task_state = state.clone();
            let task_run_id = run_id.clone();
            let task_actor = service::RunActor {
                actor_id,
                actor_kind: ActorKindDto::Operator,
            };
            let spawn_result = std::thread::Builder::new()
                .name("data-retention-manual".to_string())
                .spawn(move || {
                    let _kickoff_guard = guard;
                    let report = run_guarded_cycle(
                        &task_state,
                        &config,
                        &task_actor,
                        &task_run_id,
                        started_at_ms,
                    );
                    log_report(&report, "manual data retention cycle complete");
                });
            if let Err(err) = spawn_result {
                release_without_run(state, Some(format!("error: thread_spawn:{err}")));
                return Err(StoreError::Domain(format!(
                    "data_retention_thread_spawn:{err}"
                )));
            }
            Ok(DataRetentionRunResponse {
                status: DataRetentionRunStatus::Spawned,
                run_id: Some(run_id),
                reason: None,
            })
        }
        Ok(KickoffDecision::Replayed { run_id }) => {
            release_without_run(state, Some("replayed".to_string()));
            Ok(DataRetentionRunResponse {
                status: DataRetentionRunStatus::Replayed,
                run_id: Some(run_id),
                reason: None,
            })
        }
        Ok(KickoffDecision::AlreadyRunning { run_id }) => {
            release_without_run(state, None);
            Ok(DataRetentionRunResponse {
                status: DataRetentionRunStatus::AlreadyRunning,
                run_id: Some(run_id),
                reason: Some("retention_in_flight".to_string()),
            })
        }
        Ok(KickoffDecision::CapacityExceeded) => {
            release_without_run(state, None);
            Ok(DataRetentionRunResponse {
                status: DataRetentionRunStatus::AlreadyRunning,
                run_id: None,
                reason: Some("retention_capacity".to_string()),
            })
        }
        Err(err) => {
            release_without_run(state, Some(format!("error: {err}")));
            Err(err)
        }
    }
}

pub fn try_begin_run(state: &AppState, started_at_ms: u64) -> Result<(), &'static str> {
    let mut guard = state.sync_guards.guard(Pump::DataRetention).lock();
    if guard.in_flight {
        return Err("retention_in_flight");
    }
    guard.in_flight = true;
    guard.last_attempt_ms = Some(started_at_ms);
    Ok(())
}

pub fn run_guarded_cycle(
    state: &AppState,
    config: &service::RetentionConfig,
    actor: &service::RunActor,
    run_id: &str,
    started_at_ms: u64,
) -> service::CycleReport {
    let started = Instant::now();
    let report = service::run_cycle(state, config, actor, run_id, started_at_ms);
    let finished_at_ms = now_ms();
    let mut guard = state.sync_guards.guard(Pump::DataRetention).lock();
    guard.in_flight = false;
    // The automatic thread sleeps for `config.interval`; manual recovery can
    // start another bounded cycle as soon as this one releases the guard.
    guard.next_allowed_at_ms = finished_at_ms;
    guard.last_duration_ms = Some(started.elapsed().as_millis().min(u64::MAX as u128) as u64);
    guard.units_used = report.summary.total().min(u32::MAX as usize) as u32;
    guard.last_outcome = Some(if report.errors.is_empty() {
        format!(
            "ok: email_bodies={}, receipt_payloads={}",
            report.summary.email_bodies_compacted, report.summary.receipt_payloads_compacted
        )
    } else {
        format!("error: {}", report.errors.join(" | "))
    });
    report
}

fn read_config(state: &AppState) -> Result<service::RetentionConfig, StoreError> {
    let persistence = state.persistence.lock();
    service::config_from_settings(persistence.connection_ref(), &state.client_id)
}

fn release_without_run(state: &AppState, outcome: Option<String>) {
    let mut guard = state.sync_guards.guard(Pump::DataRetention).lock();
    guard.in_flight = false;
    guard.last_duration_ms = Some(0);
    if outcome.is_some() {
        guard.last_outcome = outcome;
    }
}

fn log_report(report: &service::CycleReport, message: &'static str) {
    if report.errors.is_empty() {
        tracing::info!(
            email_bodies = report.summary.email_bodies_compacted,
            receipt_payloads = report.summary.receipt_payloads_compacted,
            "{message}"
        );
    } else {
        tracing::warn!(
            email_bodies = report.summary.email_bodies_compacted,
            receipt_payloads = report.summary.receipt_payloads_compacted,
            errors = ?report.errors,
            "{message}"
        );
    }
}
