use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use crate::{
    AppError, AppResult, ClaimedDispatchWork, DispatchLease, DispatchStatusStore,
    DispatchWorkStore, ErrorCode, ExecutionContext, LeaseId, TelemetrySink, VisibleWorkerListQuery,
    WorkerDispatchCoordinator, WorkerDispatchCoordinatorConfig, WorkerDispatchCycleRequest,
    WorkerDispatchSnapshot, WorkerHeartbeat, WorkerHeartbeatStore, WorkerObservation,
    WorkerSnapshotListQuery, WorkerSnapshotQuery,
};

use super::{
    ReferenceActiveClaimRef, ReferenceClaimDisposition, ReferenceClaimOutcome,
    ReferenceDispatchCycle, ReferenceLeaseRenewal, ReferenceWorkClaim, ReferenceWorkerSnapshot,
};

#[derive(Debug, Clone)]
pub(super) struct RegisteredClaim {
    pub(super) claimed: ClaimedDispatchWork,
}

pub struct ReferenceDispatchRunner<'a> {
    coordinator: WorkerDispatchCoordinator<'a>,
    active_claims: Mutex<HashMap<LeaseId, RegisteredClaim>>,
}

impl<'a> ReferenceDispatchRunner<'a> {
    pub fn new(
        dispatch: &'a dyn DispatchWorkStore,
        heartbeats: &'a dyn WorkerHeartbeatStore,
        status: &'a dyn DispatchStatusStore,
        config: WorkerDispatchCoordinatorConfig,
    ) -> Self {
        Self {
            coordinator: WorkerDispatchCoordinator::new(dispatch, heartbeats, status, config),
            active_claims: Mutex::new(HashMap::new()),
        }
    }

    pub fn with_telemetry(mut self, telemetry: &'a dyn TelemetrySink) -> Self {
        self.coordinator = self.coordinator.with_telemetry(telemetry);
        self
    }

    pub fn config(&self) -> &WorkerDispatchCoordinatorConfig {
        self.coordinator.config()
    }

    pub fn heartbeat(
        &self,
        request: crate::WorkerHeartbeatRequest,
        ctx: &ExecutionContext,
    ) -> AppResult<WorkerObservation> {
        self.coordinator.record_heartbeat(request, ctx)
    }

    pub fn claim(
        &self,
        request: WorkerDispatchCycleRequest,
        ctx: &ExecutionContext,
    ) -> AppResult<ReferenceDispatchCycle> {
        let cycle = self.coordinator.dispatch_cycle(request, ctx)?;
        let claims = cycle
            .claimed()
            .iter()
            .map(ReferenceWorkClaim::from_claimed)
            .collect::<Vec<_>>();

        let mut active_claims = self.lock_active_claims(ctx)?;
        for claimed in cycle.claimed() {
            active_claims.insert(
                claimed.lease().lease_id().clone(),
                RegisteredClaim {
                    claimed: claimed.clone(),
                },
            );
        }

        Ok(ReferenceDispatchCycle::new(
            cycle.observation().clone(),
            claims,
        ))
    }

    pub fn renew(
        &self,
        claim: ReferenceActiveClaimRef,
        renewed_at: SystemTime,
        lease_ttl: Duration,
        ctx: &ExecutionContext,
    ) -> AppResult<ReferenceLeaseRenewal> {
        let previous = self.lookup_claim(&claim, ctx)?;
        let previous_claim = ReferenceWorkClaim::from_claimed(&previous.claimed);
        let renewal =
            self.coordinator
                .renew_claim(&previous.claimed, renewed_at, lease_ttl, ctx)?;
        let renewed_claim = rebuild_claimed_with_lease(previous.claimed, renewal.lease().clone())?;
        let renewed_summary = ReferenceWorkClaim::from_claimed(&renewed_claim);

        let mut active_claims = self.lock_active_claims(ctx)?;
        active_claims.remove(claim.lease_id());
        active_claims.insert(
            renewed_summary.lease_id().clone(),
            RegisteredClaim {
                claimed: renewed_claim,
            },
        );

        Ok(ReferenceLeaseRenewal::new(
            renewal.observation().clone(),
            previous_claim,
            renewed_summary,
        ))
    }

    pub fn complete(
        &self,
        claim: ReferenceActiveClaimRef,
        completed_at: SystemTime,
        ctx: &ExecutionContext,
    ) -> AppResult<ReferenceClaimOutcome> {
        let registered = self.lookup_claim(&claim, ctx)?;
        let claim_summary = ReferenceWorkClaim::from_claimed(&registered.claimed);
        let observation = self
            .coordinator
            .complete_claim(registered.claimed, completed_at, ctx)?;

        self.lock_active_claims(ctx)?.remove(claim.lease_id());
        Ok(ReferenceClaimOutcome::new(
            observation,
            claim_summary,
            ReferenceClaimDisposition::Completed,
        ))
    }

    pub fn retry(
        &self,
        claim: ReferenceActiveClaimRef,
        attempted_at: SystemTime,
        next_available_at: SystemTime,
        error: impl Into<String>,
        ctx: &ExecutionContext,
    ) -> AppResult<ReferenceClaimOutcome> {
        let registered = self.lookup_claim(&claim, ctx)?;
        let claim_summary = ReferenceWorkClaim::from_claimed(&registered.claimed);
        let observation = self.coordinator.retry_claim(
            registered.claimed,
            attempted_at,
            next_available_at,
            error,
            ctx,
        )?;

        self.lock_active_claims(ctx)?.remove(claim.lease_id());
        Ok(ReferenceClaimOutcome::new(
            observation,
            claim_summary,
            ReferenceClaimDisposition::Retried,
        ))
    }

    pub fn dead_letter(
        &self,
        claim: ReferenceActiveClaimRef,
        dead_lettered_at: SystemTime,
        error: impl Into<String>,
        ctx: &ExecutionContext,
    ) -> AppResult<ReferenceClaimOutcome> {
        let registered = self.lookup_claim(&claim, ctx)?;
        let claim_summary = ReferenceWorkClaim::from_claimed(&registered.claimed);
        let observation =
            self.coordinator
                .dead_letter_claim(registered.claimed, dead_lettered_at, error, ctx)?;

        self.lock_active_claims(ctx)?.remove(claim.lease_id());
        Ok(ReferenceClaimOutcome::new(
            observation,
            claim_summary,
            ReferenceClaimDisposition::DeadLettered,
        ))
    }

    pub fn snapshot_worker(
        &self,
        query: WorkerSnapshotQuery,
        ctx: &ExecutionContext,
    ) -> AppResult<Option<ReferenceWorkerSnapshot>> {
        let worker_id = query.worker_id().to_string();
        let snapshot = self.coordinator.snapshot_worker(query, ctx)?;
        let active_claims = self.list_active_claims_for_worker(&worker_id, ctx)?;

        if snapshot.is_none() && active_claims.is_empty() {
            return Ok(None);
        }

        Ok(Some(ReferenceWorkerSnapshot::new(
            worker_id,
            snapshot,
            active_claims,
        )))
    }

    pub fn list_worker_snapshots(
        &self,
        query: WorkerSnapshotListQuery,
        ctx: &ExecutionContext,
    ) -> AppResult<Vec<WorkerDispatchSnapshot>> {
        self.coordinator.list_worker_snapshots(query, ctx)
    }

    pub fn list_worker_details(
        &self,
        query: WorkerSnapshotListQuery,
        ctx: &ExecutionContext,
    ) -> AppResult<Vec<ReferenceWorkerSnapshot>> {
        let snapshots = self.coordinator.list_worker_snapshots(query, ctx)?;
        snapshots
            .into_iter()
            .map(|snapshot| {
                let worker_id = snapshot.worker_id().to_string();
                let active_claims = self.list_active_claims_for_worker(&worker_id, ctx)?;
                Ok(ReferenceWorkerSnapshot::new(
                    worker_id,
                    Some(snapshot),
                    active_claims,
                ))
            })
            .collect()
    }

    pub fn list_visible_workers(
        &self,
        query: VisibleWorkerListQuery,
        ctx: &ExecutionContext,
    ) -> AppResult<Vec<WorkerHeartbeat>> {
        self.coordinator.list_visible_workers(query, ctx)
    }

    pub fn list_active_claims(&self, ctx: &ExecutionContext) -> AppResult<Vec<ReferenceWorkClaim>> {
        self.list_active_claims_inner(None, ctx)
    }

    pub fn list_active_claims_for_worker(
        &self,
        worker_id: &str,
        ctx: &ExecutionContext,
    ) -> AppResult<Vec<ReferenceWorkClaim>> {
        self.list_active_claims_inner(Some(worker_id), ctx)
    }

    pub(super) fn lookup_claim(
        &self,
        claim: &ReferenceActiveClaimRef,
        ctx: &ExecutionContext,
    ) -> AppResult<RegisteredClaim> {
        let active_claims = self.lock_active_claims(ctx)?;
        let registered = active_claims
            .get(claim.lease_id())
            .cloned()
            .ok_or_else(|| {
                AppError::not_found(
                    "reference_runner_claim_not_found",
                    format!("active claim {} was not found", claim.lease_id()),
                    ctx.correlation_id.clone(),
                )
            })?;

        if registered.claimed.lease().leased_by() != claim.worker_id() {
            return Err(AppError::new(
                ErrorCode::ConcurrentModification,
                "reference_runner_claim_owner_mismatch",
                "active claim does not belong to the requested worker",
                ctx.correlation_id.clone(),
            ));
        }

        Ok(registered)
    }

    fn list_active_claims_inner(
        &self,
        worker_id: Option<&str>,
        ctx: &ExecutionContext,
    ) -> AppResult<Vec<ReferenceWorkClaim>> {
        let active_claims = self.lock_active_claims(ctx)?;
        let mut claims = active_claims
            .values()
            .filter(|registered| {
                worker_id
                    .map(|worker_id| registered.claimed.lease().leased_by() == worker_id)
                    .unwrap_or(true)
            })
            .map(|registered| ReferenceWorkClaim::from_claimed(&registered.claimed))
            .collect::<Vec<_>>();

        claims.sort_by_key(|claim| (claim.leased_at(), claim.lease_id().clone()));
        Ok(claims)
    }

    fn lock_active_claims(
        &self,
        ctx: &ExecutionContext,
    ) -> AppResult<MutexGuard<'_, HashMap<LeaseId, RegisteredClaim>>> {
        self.active_claims.lock().map_err(|_| {
            AppError::unexpected(
                "reference_runner_state_poisoned",
                "reference dispatch runner state mutex is poisoned",
                ctx.correlation_id.clone(),
            )
        })
    }
}

fn rebuild_claimed_with_lease(
    claimed: ClaimedDispatchWork,
    renewed_lease: DispatchLease,
) -> AppResult<ClaimedDispatchWork> {
    match (claimed, renewed_lease) {
        (ClaimedDispatchWork::Execution(claimed), DispatchLease::Execution(lease)) => claimed
            .replace_lease(lease)
            .map(ClaimedDispatchWork::Execution),
        (ClaimedDispatchWork::Outbox(claimed), DispatchLease::Outbox(lease)) => claimed
            .replace_lease(lease)
            .map(ClaimedDispatchWork::Outbox),
        (ClaimedDispatchWork::Execution(_), DispatchLease::Outbox(_)) => Err(AppError::new(
            ErrorCode::InvalidInput,
            "reference_runner_lease_queue_mismatch",
            "execution claims require execution lease renewals",
            crate::CorrelationId::generate(),
        )),
        (ClaimedDispatchWork::Outbox(_), DispatchLease::Execution(_)) => Err(AppError::new(
            ErrorCode::InvalidInput,
            "reference_runner_lease_queue_mismatch",
            "outbox claims require outbox lease renewals",
            crate::CorrelationId::generate(),
        )),
    }
}
