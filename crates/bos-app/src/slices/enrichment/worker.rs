//! Enrichment freshness pump: budgeted re-enrichment for stale critical fields.
//!
//! This pump is env-gated OFF by default. It only targets still-STAGED drafts
//! because the existing slice graft stores require staged status and only
//! fill missing / replace weak AI-prefill values. Approved/provider records are
//! out of scope for this PR; refreshing them needs a separate re-staging
//! mechanism and attended product decision. For stale-but-populated fields, the
//! run records proposals and diagnostics, while the owning slice store leaves
//! the draft unchanged.
//!
//! The freshness trigger epoch is bucketed by stale_after_ms. A failed/skipped
//! freshness run in a bucket is replay-stable and will not retry with a new
//! run_id until the bucket rolls. That limitation is acceptable for v1 because
//! this pump is disabled by default and operator on-demand enrichment remains
//! available.

use std::time::Duration;

use super::service::{self, FreshnessCandidate};
use crate::env_registry;
use crate::http::{now_ms, AppState};
use crate::store_core::StoreError;

pub const FRESHNESS_COOLDOWN_MS: u64 = 60_000;

pub struct FreshnessPumpConfig {
    pub enabled: bool,
    pub interval: Duration,
    pub stale_after_ms: u64,
    pub max_enrichments_per_cycle: u32,
}

pub fn config_from_settings(
    conn: &rusqlite::Connection,
    client_id: &str,
) -> Result<FreshnessPumpConfig, StoreError> {
    Ok(FreshnessPumpConfig {
        enabled: crate::slices::admin_settings::service::flag(
            conn,
            client_id,
            &env_registry::BOS_ENRICHMENT_FRESHNESS_ENABLED,
        )?,
        interval: Duration::from_secs(
            crate::slices::admin_settings::service::usize_or(
                conn,
                client_id,
                &env_registry::BOS_ENRICHMENT_FRESHNESS_INTERVAL_SECS,
                1800,
            )?
            .max(300) as u64,
        ),
        stale_after_ms: (crate::slices::admin_settings::service::usize_or(
            conn,
            client_id,
            &env_registry::BOS_ENRICHMENT_FRESHNESS_STALE_AFTER_SECS,
            30 * 24 * 60 * 60,
        )? as u64)
            .max(3600)
            * 1000,
        max_enrichments_per_cycle: max_enrichments_from_settings(conn, client_id)?,
    })
}

pub fn max_enrichments_from_settings(
    conn: &rusqlite::Connection,
    client_id: &str,
) -> Result<u32, StoreError> {
    Ok(crate::slices::admin_settings::service::usize_or(
        conn,
        client_id,
        &env_registry::BOS_ENRICHMENT_FRESHNESS_MAX_ENRICHMENTS_PER_CYCLE,
        3,
    )?
    .clamp(1, 20) as u32)
}

pub fn spawn(state: AppState) {
    if !state.slice_enabled(super::SLICE.id) {
        tracing::info!(
            "enrichment freshness pump not started (enrichment disabled by client overlay)"
        );
        return;
    }
    std::thread::Builder::new()
        .name("enrichment-freshness-pump".to_string())
        .spawn(move || {
            tracing::info!("enrichment freshness pump started");
            loop {
                let config = {
                    let persistence = state.persistence.lock();
                    match config_from_settings(persistence.connection_ref(), &state.client_id) {
                        Ok(config) => config,
                        Err(err) => {
                            tracing::warn!(
                                error = %err,
                                "enrichment freshness config read failed"
                            );
                            FreshnessPumpConfig {
                                enabled: false,
                                interval: Duration::from_secs(1800),
                                stale_after_ms: 30 * 24 * 60 * 60 * 1000,
                                max_enrichments_per_cycle: 3,
                            }
                        }
                    }
                };
                if config.enabled && try_begin_sync(&state, now_ms()).is_ok() {
                    let summary = run_guarded_cycle(
                        &state,
                        config.stale_after_ms,
                        config.max_enrichments_per_cycle,
                    );
                    match summary {
                        Ok(summary) if summary.enrichments_attempted > 0 => tracing::info!(
                            attempted = summary.enrichments_attempted,
                            completed = summary.completed,
                            skipped = summary.skipped,
                            failed = summary.failed,
                            "enrichment freshness cycle complete"
                        ),
                        Ok(_) => {}
                        Err(err) => {
                            tracing::warn!(error = %err, "enrichment freshness cycle failed")
                        }
                    }
                }
                std::thread::sleep(config.interval);
            }
        })
        .expect("spawn enrichment-freshness-pump thread");
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CycleSummary {
    pub enrichments_attempted: u32,
    pub completed: usize,
    pub skipped: usize,
    pub failed: usize,
}

pub fn try_begin_sync(state: &AppState, now: u64) -> Result<(), &'static str> {
    let mut status = state
        .sync_guards
        .guard(crate::http::Pump::EnrichmentFreshness)
        .lock();
    if status.in_flight {
        return Err("sync_in_flight");
    }
    if now < status.next_allowed_at_ms {
        return Err("sync_cooldown");
    }
    status.in_flight = true;
    status.last_attempt_ms = Some(now);
    Ok(())
}

pub fn run_guarded_cycle(
    state: &AppState,
    stale_after_ms: u64,
    max_enrichments: u32,
) -> Result<CycleSummary, String> {
    let result = run_cycle(state, stale_after_ms, max_enrichments, now_ms());
    let mut status = state
        .sync_guards
        .guard(crate::http::Pump::EnrichmentFreshness)
        .lock();
    status.in_flight = false;
    status.next_allowed_at_ms = now_ms() + FRESHNESS_COOLDOWN_MS;
    match &result {
        Ok(summary) => {
            status.units_used = summary.enrichments_attempted;
            status.last_outcome = Some("ok".to_string());
        }
        Err(err) => status.last_outcome = Some(format!("error: {err}")),
    }
    result
}

pub fn run_cycle(
    state: &AppState,
    stale_after_ms: u64,
    max_enrichments: u32,
    now: u64,
) -> Result<CycleSummary, String> {
    let mut summary = CycleSummary::default();
    let mut remaining = max_enrichments;
    let epoch = service::freshness_epoch(stale_after_ms, now);
    for adapter in service::registered_freshness_adapters() {
        if remaining == 0 {
            break;
        }
        if !state.slice_enabled(adapter.slice_id) {
            continue;
        }
        let candidates =
            (adapter.collect_candidates)(state, adapter, stale_after_ms, now, remaining as usize)?;
        for candidate in candidates {
            if remaining == 0 {
                break;
            }
            if candidate.subject_id != adapter.subject_id || candidate.slice_id != adapter.slice_id
            {
                continue;
            }
            let Some(_guard) = begin_freshness_candidate(&candidate) else {
                summary.skipped += 1;
                continue;
            };
            remaining -= 1;
            summary.enrichments_attempted += 1;
            let outcome = (adapter.run_candidate)(state, &candidate, &epoch);
            match outcome.status {
                bos_contracts::enrichment::EnrichmentRunStatus::Completed
                | bos_contracts::enrichment::EnrichmentRunStatus::Partial => {
                    summary.completed += 1;
                }
                bos_contracts::enrichment::EnrichmentRunStatus::Skipped => {
                    summary.skipped += 1;
                }
                bos_contracts::enrichment::EnrichmentRunStatus::Started
                | bos_contracts::enrichment::EnrichmentRunStatus::Failed => {
                    summary.failed += 1;
                }
            }
        }
    }
    Ok(summary)
}

fn begin_freshness_candidate(
    candidate: &FreshnessCandidate,
) -> Option<crate::slices::async_kickoff::KickoffGuard> {
    match crate::slices::async_kickoff::begin(
        crate::slices::async_kickoff::KickoffSpec {
            slice_id: candidate.slice_id,
            draft_id: &candidate.draft_id,
            planned_run_id: &candidate.run_id,
            capacity: crate::slices::async_kickoff::KickoffCapacity::Unbounded,
        },
        || {
            Ok::<_, crate::store_core::StoreError>(crate::slices::async_kickoff::RecordedKickoff {
                run_id: candidate.run_id.clone(),
                replayed: false,
            })
        },
    ) {
        Ok(crate::slices::async_kickoff::KickoffDecision::Spawn { guard, .. }) => Some(guard),
        Ok(crate::slices::async_kickoff::KickoffDecision::AlreadyRunning { run_id }) => {
            tracing::info!(
                slice_id = candidate.slice_id,
                draft_id = %candidate.draft_id,
                active_run_id = %run_id,
                "enrichment freshness skipped already-running draft"
            );
            None
        }
        Ok(crate::slices::async_kickoff::KickoffDecision::Replayed { .. })
        | Ok(crate::slices::async_kickoff::KickoffDecision::CapacityExceeded)
        | Err(_) => None,
    }
}
