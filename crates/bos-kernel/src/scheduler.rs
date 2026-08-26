use std::time::{Duration, SystemTime};

use crate::{
    AppError, AppResult, ClaimedDispatchWork, DispatchClaimRequest, DispatchLease,
    DispatchLeaseRenewal, DispatchQueue, DispatchResolution, DispatchStatusStore,
    DispatchWorkStore, ErrorCode, ExecutionContext, ExecutionLeaseRenewal,
    OutboxDeliveryLeaseRenewal, TelemetryEvent, TelemetrySink, VisibleWorkersQuery,
    WorkerDispatchSnapshot, WorkerDispatchSnapshotRef, WorkerDispatchSnapshotsQuery,
    WorkerHeartbeat, WorkerHeartbeatStore, WorkerVisibilityRef,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerDispatchCoordinatorConfig {
    queue: DispatchQueue,
    scope: String,
    visibility_ttl: Duration,
}

impl WorkerDispatchCoordinatorConfig {
    pub fn new(queue: DispatchQueue, scope: impl Into<String>, visibility_ttl: Duration) -> Self {
        Self {
            queue,
            scope: scope.into(),
            visibility_ttl,
        }
    }

    pub fn queue(&self) -> DispatchQueue {
        self.queue
    }

    pub fn scope(&self) -> &str {
        &self.scope
    }

    pub fn visibility_ttl(&self) -> Duration {
        self.visibility_ttl
    }

    pub fn validate(&self, ctx: &ExecutionContext) -> AppResult<()> {
        require_non_empty(
            &self.scope,
            "worker dispatch scope",
            "worker_dispatch_scope_required",
            ctx,
        )?;
        require_positive_duration(
            self.visibility_ttl,
            "worker dispatch visibility ttl",
            "worker_dispatch_visibility_ttl_required",
            ctx,
        )?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerHeartbeatRequest {
    worker_id: String,
    observed_at: SystemTime,
}

impl WorkerHeartbeatRequest {
    pub fn new(worker_id: impl Into<String>) -> Self {
        Self {
            worker_id: worker_id.into(),
            observed_at: SystemTime::now(),
        }
    }

    pub fn worker_id(&self) -> &str {
        &self.worker_id
    }

    pub fn observed_at(&self) -> SystemTime {
        self.observed_at
    }

    pub fn at(mut self, observed_at: SystemTime) -> Self {
        self.observed_at = observed_at;
        self
    }

    pub fn validate(&self, ctx: &ExecutionContext) -> AppResult<()> {
        require_non_empty(
            &self.worker_id,
            "worker id",
            "worker_dispatch_worker_id_required",
            ctx,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerDispatchCycleRequest {
    worker_id: String,
    batch_size: usize,
    lease_ttl: Duration,
    observed_at: SystemTime,
}

impl WorkerDispatchCycleRequest {
    pub fn new(worker_id: impl Into<String>, batch_size: usize, lease_ttl: Duration) -> Self {
        Self {
            worker_id: worker_id.into(),
            batch_size: batch_size.max(1),
            lease_ttl,
            observed_at: SystemTime::now(),
        }
    }

    pub fn worker_id(&self) -> &str {
        &self.worker_id
    }

    pub fn batch_size(&self) -> usize {
        self.batch_size
    }

    pub fn lease_ttl(&self) -> Duration {
        self.lease_ttl
    }

    pub fn observed_at(&self) -> SystemTime {
        self.observed_at
    }

    pub fn at(mut self, observed_at: SystemTime) -> Self {
        self.observed_at = observed_at;
        self
    }

    pub fn validate(&self, ctx: &ExecutionContext) -> AppResult<()> {
        require_non_empty(
            &self.worker_id,
            "worker id",
            "worker_dispatch_worker_id_required",
            ctx,
        )?;
        require_positive_duration(
            self.lease_ttl,
            "worker lease ttl",
            "worker_dispatch_lease_ttl_required",
            ctx,
        )?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerSnapshotQuery {
    worker_id: String,
    observed_at: SystemTime,
}

impl WorkerSnapshotQuery {
    pub fn new(worker_id: impl Into<String>) -> Self {
        Self {
            worker_id: worker_id.into(),
            observed_at: SystemTime::now(),
        }
    }

    pub fn worker_id(&self) -> &str {
        &self.worker_id
    }

    pub fn observed_at(&self) -> SystemTime {
        self.observed_at
    }

    pub fn at(mut self, observed_at: SystemTime) -> Self {
        self.observed_at = observed_at;
        self
    }

    pub fn validate(&self, ctx: &ExecutionContext) -> AppResult<()> {
        require_non_empty(
            &self.worker_id,
            "worker id",
            "worker_dispatch_worker_id_required",
            ctx,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerSnapshotListQuery {
    observed_at: SystemTime,
    limit: usize,
    include_expired_workers: bool,
}

impl WorkerSnapshotListQuery {
    pub fn new() -> Self {
        Self {
            observed_at: SystemTime::now(),
            limit: usize::MAX,
            include_expired_workers: true,
        }
    }

    pub fn observed_at(&self) -> SystemTime {
        self.observed_at
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    pub fn include_expired_workers(&self) -> bool {
        self.include_expired_workers
    }

    pub fn at(mut self, observed_at: SystemTime) -> Self {
        self.observed_at = observed_at;
        self
    }

    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit.max(1);
        self
    }

    pub fn with_expired_workers(mut self, include_expired_workers: bool) -> Self {
        self.include_expired_workers = include_expired_workers;
        self
    }
}

impl Default for WorkerSnapshotListQuery {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleWorkerListQuery {
    observed_at: SystemTime,
    limit: usize,
}

impl VisibleWorkerListQuery {
    pub fn new() -> Self {
        Self {
            observed_at: SystemTime::now(),
            limit: usize::MAX,
        }
    }

    pub fn observed_at(&self) -> SystemTime {
        self.observed_at
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    pub fn at(mut self, observed_at: SystemTime) -> Self {
        self.observed_at = observed_at;
        self
    }

    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit.max(1);
        self
    }
}

impl Default for VisibleWorkerListQuery {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerObservation {
    worker_id: String,
    scope: String,
    observed_at: SystemTime,
    heartbeat: WorkerHeartbeat,
    snapshot: Option<WorkerDispatchSnapshot>,
}

impl WorkerObservation {
    pub fn new(
        worker_id: impl Into<String>,
        scope: impl Into<String>,
        observed_at: SystemTime,
        heartbeat: WorkerHeartbeat,
        snapshot: Option<WorkerDispatchSnapshot>,
    ) -> Self {
        Self {
            worker_id: worker_id.into(),
            scope: scope.into(),
            observed_at,
            heartbeat,
            snapshot,
        }
    }

    pub fn worker_id(&self) -> &str {
        &self.worker_id
    }

    pub fn scope(&self) -> &str {
        &self.scope
    }

    pub fn observed_at(&self) -> SystemTime {
        self.observed_at
    }

    pub fn heartbeat(&self) -> &WorkerHeartbeat {
        &self.heartbeat
    }

    pub fn snapshot(&self) -> Option<&WorkerDispatchSnapshot> {
        self.snapshot.as_ref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerDispatchCycle {
    observation: WorkerObservation,
    claimed: Vec<ClaimedDispatchWork>,
}

impl WorkerDispatchCycle {
    pub fn new(observation: WorkerObservation, claimed: Vec<ClaimedDispatchWork>) -> Self {
        Self {
            observation,
            claimed,
        }
    }

    pub fn observation(&self) -> &WorkerObservation {
        &self.observation
    }

    pub fn claimed(&self) -> &[ClaimedDispatchWork] {
        &self.claimed
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerStoredLeaseRenewal {
    observation: WorkerObservation,
    lease: DispatchLease,
}

impl WorkerStoredLeaseRenewal {
    pub fn new(observation: WorkerObservation, lease: DispatchLease) -> Self {
        Self { observation, lease }
    }

    pub fn observation(&self) -> &WorkerObservation {
        &self.observation
    }

    pub fn lease(&self) -> &DispatchLease {
        &self.lease
    }
}

pub struct WorkerDispatchCoordinator<'a> {
    dispatch: &'a dyn DispatchWorkStore,
    heartbeats: &'a dyn WorkerHeartbeatStore,
    status: &'a dyn DispatchStatusStore,
    telemetry: Option<&'a dyn TelemetrySink>,
    config: WorkerDispatchCoordinatorConfig,
}

impl<'a> WorkerDispatchCoordinator<'a> {
    pub fn new(
        dispatch: &'a dyn DispatchWorkStore,
        heartbeats: &'a dyn WorkerHeartbeatStore,
        status: &'a dyn DispatchStatusStore,
        config: WorkerDispatchCoordinatorConfig,
    ) -> Self {
        Self {
            dispatch,
            heartbeats,
            status,
            telemetry: None,
            config,
        }
    }

    pub fn with_telemetry(mut self, telemetry: &'a dyn TelemetrySink) -> Self {
        self.telemetry = Some(telemetry);
        self
    }

    pub fn config(&self) -> &WorkerDispatchCoordinatorConfig {
        &self.config
    }

    pub fn record_heartbeat(
        &self,
        request: WorkerHeartbeatRequest,
        ctx: &ExecutionContext,
    ) -> AppResult<WorkerObservation> {
        self.config.validate(ctx)?;
        request.validate(ctx)?;
        let observation = self.observe_worker(request.worker_id(), request.observed_at(), ctx)?;
        self.record_event(
            ctx,
            TelemetryEvent::new("worker.heartbeat")
                .with_attribute("scope", self.config.scope())
                .with_attribute("queue", self.config.queue().as_str())
                .with_attribute("worker_id", request.worker_id()),
        );
        Ok(observation)
    }

    pub fn dispatch_cycle(
        &self,
        request: WorkerDispatchCycleRequest,
        ctx: &ExecutionContext,
    ) -> AppResult<WorkerDispatchCycle> {
        self.config.validate(ctx)?;
        request.validate(ctx)?;

        let claimed = self.dispatch.claim_due(
            DispatchClaimRequest::new(
                self.config.queue(),
                request.worker_id().to_string(),
                request.batch_size(),
                request.lease_ttl(),
            )
            .at(request.observed_at()),
            ctx,
        )?;

        let observation = self.observe_worker(request.worker_id(), request.observed_at(), ctx)?;
        self.record_event(
            ctx,
            TelemetryEvent::new("worker.dispatch_cycle")
                .with_attribute("scope", self.config.scope())
                .with_attribute("queue", self.config.queue().as_str())
                .with_attribute("worker_id", request.worker_id())
                .with_attribute("claimed_count", claimed.len().to_string()),
        );
        Ok(WorkerDispatchCycle::new(observation, claimed))
    }

    pub fn renew_claim(
        &self,
        claimed: &ClaimedDispatchWork,
        renewed_at: SystemTime,
        lease_ttl: Duration,
        ctx: &ExecutionContext,
    ) -> AppResult<WorkerStoredLeaseRenewal> {
        self.config.validate(ctx)?;
        require_positive_duration(
            lease_ttl,
            "worker lease ttl",
            "worker_dispatch_lease_ttl_required",
            ctx,
        )?;

        let lease = claimed.lease();
        let worker_id = lease.leased_by().to_string();
        let renewal = match claimed {
            ClaimedDispatchWork::Execution(claimed) => DispatchLeaseRenewal::Execution(
                ExecutionLeaseRenewal::new(
                    claimed.record().id().clone(),
                    claimed.lease().lease_id().clone(),
                    worker_id.clone(),
                    lease_ttl,
                )
                .at(renewed_at),
            ),
            ClaimedDispatchWork::Outbox(claimed) => DispatchLeaseRenewal::Outbox(
                OutboxDeliveryLeaseRenewal::new(
                    claimed.message().id().clone(),
                    claimed.lease().lease_id().clone(),
                    worker_id.clone(),
                    lease_ttl,
                )
                .at(renewed_at),
            ),
        };

        let lease = self.dispatch.renew_claim(renewal, ctx)?;
        let observation = self.observe_worker(&worker_id, renewed_at, ctx)?;
        self.record_event(
            ctx,
            TelemetryEvent::new("worker.lease_renewed")
                .with_attribute("scope", self.config.scope())
                .with_attribute("queue", self.config.queue().as_str())
                .with_attribute("worker_id", &worker_id)
                .with_attribute("lease_id", lease.lease_id().as_str()),
        );
        Ok(WorkerStoredLeaseRenewal::new(observation, lease))
    }

    pub fn complete_claim(
        &self,
        claimed: ClaimedDispatchWork,
        completed_at: SystemTime,
        ctx: &ExecutionContext,
    ) -> AppResult<WorkerObservation> {
        let worker_id = claimed.lease().leased_by().to_string();
        self.finalize_resolution(worker_id, claimed.complete(completed_at), completed_at, ctx)
    }

    pub fn retry_claim(
        &self,
        claimed: ClaimedDispatchWork,
        attempted_at: SystemTime,
        next_available_at: SystemTime,
        error: impl Into<String>,
        ctx: &ExecutionContext,
    ) -> AppResult<WorkerObservation> {
        let worker_id = claimed.lease().leased_by().to_string();
        let resolution = claimed.retry(attempted_at, next_available_at, error)?;
        self.finalize_resolution(worker_id, resolution, attempted_at, ctx)
    }

    pub fn dead_letter_claim(
        &self,
        claimed: ClaimedDispatchWork,
        dead_lettered_at: SystemTime,
        error: impl Into<String>,
        ctx: &ExecutionContext,
    ) -> AppResult<WorkerObservation> {
        let worker_id = claimed.lease().leased_by().to_string();
        let resolution = claimed.dead_letter(dead_lettered_at, error)?;
        self.finalize_resolution(worker_id, resolution, dead_lettered_at, ctx)
    }

    pub fn snapshot_worker(
        &self,
        query: WorkerSnapshotQuery,
        ctx: &ExecutionContext,
    ) -> AppResult<Option<WorkerDispatchSnapshot>> {
        self.config.validate(ctx)?;
        query.validate(ctx)?;
        self.status.lookup_worker_snapshot(
            WorkerDispatchSnapshotRef::new(query.worker_id().to_string(), self.config.scope())
                .at(query.observed_at()),
            ctx,
        )
    }

    pub fn list_worker_snapshots(
        &self,
        query: WorkerSnapshotListQuery,
        ctx: &ExecutionContext,
    ) -> AppResult<Vec<WorkerDispatchSnapshot>> {
        self.config.validate(ctx)?;
        self.status.list_worker_snapshots(
            WorkerDispatchSnapshotsQuery::new(self.config.scope())
                .at(query.observed_at())
                .with_limit(query.limit())
                .with_expired_workers(query.include_expired_workers()),
            ctx,
        )
    }

    pub fn list_visible_workers(
        &self,
        query: VisibleWorkerListQuery,
        ctx: &ExecutionContext,
    ) -> AppResult<Vec<WorkerHeartbeat>> {
        self.config.validate(ctx)?;
        self.heartbeats.list_visible_workers(
            VisibleWorkersQuery::new(self.config.scope())
                .at(query.observed_at())
                .with_limit(query.limit()),
            ctx,
        )
    }

    fn finalize_resolution(
        &self,
        worker_id: String,
        resolution: DispatchResolution,
        observed_at: SystemTime,
        ctx: &ExecutionContext,
    ) -> AppResult<WorkerObservation> {
        self.config.validate(ctx)?;
        self.dispatch.finalize(resolution.clone(), ctx)?;
        let observation = self.observe_worker(&worker_id, observed_at, ctx)?;
        self.record_event(
            ctx,
            TelemetryEvent::new("worker.dispatch_finalized")
                .with_attribute("scope", self.config.scope())
                .with_attribute("queue", self.config.queue().as_str())
                .with_attribute("worker_id", worker_id)
                .with_attribute("resolution_queue", resolution.queue().as_str())
                .with_attribute("lease_id", resolution.lease_id().as_str()),
        );
        Ok(observation)
    }

    fn observe_worker(
        &self,
        worker_id: &str,
        observed_at: SystemTime,
        ctx: &ExecutionContext,
    ) -> AppResult<WorkerObservation> {
        let heartbeat = WorkerHeartbeat::from_ttl(
            worker_id.to_string(),
            self.config.scope().to_string(),
            observed_at,
            self.config.visibility_ttl(),
        );
        self.heartbeats.record_heartbeat(heartbeat, ctx)?;

        let heartbeat = self
            .heartbeats
            .lookup_worker(
                WorkerVisibilityRef::new(worker_id.to_string(), self.config.scope())
                    .at(observed_at),
                ctx,
            )?
            .ok_or_else(|| {
                AppError::from_context(
                    ErrorCode::Unexpected,
                    "worker_heartbeat_lookup_missing",
                    "worker heartbeat store could not read back a recorded heartbeat",
                    ctx,
                )
            })?;

        let snapshot = self.status.lookup_worker_snapshot(
            WorkerDispatchSnapshotRef::new(worker_id.to_string(), self.config.scope())
                .at(observed_at),
            ctx,
        )?;

        Ok(WorkerObservation::new(
            worker_id.to_string(),
            self.config.scope().to_string(),
            observed_at,
            heartbeat,
            snapshot,
        ))
    }

    fn record_event(&self, ctx: &ExecutionContext, event: TelemetryEvent) {
        if let Some(telemetry) = self.telemetry {
            telemetry.record(ctx, event);
        }
    }
}

fn require_non_empty(
    value: &str,
    field_name: &'static str,
    code: &'static str,
    ctx: &ExecutionContext,
) -> AppResult<()> {
    if value.trim().is_empty() {
        return Err(AppError::from_context(
            ErrorCode::InvalidInput,
            code,
            format!("{field_name} must not be empty"),
            ctx,
        ));
    }

    Ok(())
}

fn require_positive_duration(
    duration: Duration,
    field_name: &'static str,
    code: &'static str,
    ctx: &ExecutionContext,
) -> AppResult<()> {
    if duration.is_zero() {
        return Err(AppError::from_context(
            ErrorCode::InvalidInput,
            code,
            format!("{field_name} must be positive"),
            ctx,
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::{Duration, SystemTime};

    use crate::{
        CorrelationId, DispatchQueue, ExecutionContext, ExecutionId, ExecutionJournal,
        ExecutionRecord, ExecutionRecordStatus, IdempotencyKey, InMemoryExecutionWorkStore,
        NoopTelemetry, TelemetryEvent, TelemetrySink, WorkerVisibility,
    };

    use super::{
        VisibleWorkerListQuery, WorkerDispatchCoordinator, WorkerDispatchCoordinatorConfig,
        WorkerDispatchCycleRequest, WorkerHeartbeatRequest, WorkerSnapshotListQuery,
        WorkerSnapshotQuery,
    };

    fn ctx() -> ExecutionContext {
        ExecutionContext::new(CorrelationId::new("corr_scheduler"))
    }

    fn record(id: &str, available_at: SystemTime) -> ExecutionRecord {
        ExecutionRecord::new(
            ExecutionId::new(id),
            "send_email",
            format!("target_{id}"),
            IdempotencyKey::new(format!("idem_{id}")),
            CorrelationId::new(format!("corr_{id}")),
        )
        .with_recorded_at(available_at)
    }

    fn coordinator<'a>(store: &'a InMemoryExecutionWorkStore) -> WorkerDispatchCoordinator<'a> {
        WorkerDispatchCoordinator::new(
            store,
            store,
            store,
            WorkerDispatchCoordinatorConfig::new(
                DispatchQueue::Execution,
                "scheduler",
                Duration::from_secs(10),
            ),
        )
    }

    #[derive(Default)]
    struct RecordingTelemetry {
        events: Mutex<Vec<TelemetryEvent>>,
    }

    impl RecordingTelemetry {
        fn names(&self) -> Vec<String> {
            self.events
                .lock()
                .expect("telemetry lock")
                .iter()
                .map(|event| event.name.clone())
                .collect()
        }
    }

    impl TelemetrySink for RecordingTelemetry {
        fn record(&self, _ctx: &ExecutionContext, event: TelemetryEvent) {
            self.events.lock().expect("telemetry lock").push(event);
        }
    }

    #[test]
    fn dispatch_cycle_claims_due_work_and_updates_worker_snapshot() {
        let store = InMemoryExecutionWorkStore::new();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let ctx = ctx();

        ExecutionJournal::append(&store, record("exec_1", now), &ctx).expect("append exec_1");
        ExecutionJournal::append(&store, record("exec_2", now), &ctx).expect("append exec_2");

        let cycle = coordinator(&store)
            .dispatch_cycle(
                WorkerDispatchCycleRequest::new("worker-1", 2, Duration::from_secs(30)).at(now),
                &ctx,
            )
            .expect("dispatch cycle");

        assert_eq!(cycle.claimed().len(), 2);
        assert_eq!(cycle.observation().heartbeat().worker_id(), "worker-1");
        assert_eq!(cycle.observation().heartbeat().active_leases(), 2);

        let snapshot = cycle
            .observation()
            .snapshot()
            .expect("worker snapshot should exist");
        assert_eq!(snapshot.visibility(), WorkerVisibility::Visible);
        assert_eq!(snapshot.active_leases(), 2);
        assert_eq!(
            snapshot
                .queue(DispatchQueue::Execution)
                .expect("execution queue")
                .claimed_count(),
            2
        );
        assert_eq!(
            snapshot
                .queue(DispatchQueue::Execution)
                .expect("execution queue")
                .due_count(),
            0
        );
    }

    #[test]
    fn renew_claim_extends_the_lease_and_refreshes_visibility() {
        let store = InMemoryExecutionWorkStore::new();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(200);
        let ctx = ctx();

        ExecutionJournal::append(&store, record("exec_3", now), &ctx).expect("append exec_3");

        let cycle = coordinator(&store)
            .dispatch_cycle(
                WorkerDispatchCycleRequest::new("worker-2", 1, Duration::from_secs(5)).at(now),
                &ctx,
            )
            .expect("dispatch cycle");

        let renewed = coordinator(&store)
            .renew_claim(
                &cycle.claimed()[0],
                now + Duration::from_secs(3),
                Duration::from_secs(15),
                &ctx,
            )
            .expect("renew claim");

        assert_eq!(renewed.lease().leased_by(), "worker-2");
        assert_eq!(
            renewed.lease().lease_expires_at(),
            now + Duration::from_secs(18)
        );
        assert_eq!(renewed.observation().heartbeat().active_leases(), 1);
        assert_eq!(
            renewed.observation().heartbeat().visible_until(),
            now + Duration::from_secs(13)
        );
    }

    #[test]
    fn completing_claim_clears_active_leases_and_marks_record_succeeded() {
        let store = InMemoryExecutionWorkStore::new();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(300);
        let ctx = ctx();

        ExecutionJournal::append(&store, record("exec_4", now), &ctx).expect("append exec_4");

        let cycle = coordinator(&store)
            .dispatch_cycle(
                WorkerDispatchCycleRequest::new("worker-3", 1, Duration::from_secs(20)).at(now),
                &ctx,
            )
            .expect("dispatch cycle");

        let observation = coordinator(&store)
            .complete_claim(cycle.claimed().first().expect("claimed").clone(), now, &ctx)
            .expect("complete claim");

        assert_eq!(observation.heartbeat().active_leases(), 0);
        assert_eq!(
            observation
                .snapshot()
                .expect("snapshot")
                .queue(DispatchQueue::Execution)
                .expect("execution queue")
                .claimed_count(),
            0
        );
        assert_eq!(
            store
                .record(&ExecutionId::new("exec_4"))
                .expect("stored record")
                .expect("record exists")
                .status(),
            ExecutionRecordStatus::Succeeded
        );
    }

    #[test]
    fn retry_claim_requeues_work_and_updates_snapshot() {
        let store = InMemoryExecutionWorkStore::new();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(400);
        let ctx = ctx();

        ExecutionJournal::append(&store, record("exec_5", now), &ctx).expect("append exec_5");

        let cycle = coordinator(&store)
            .dispatch_cycle(
                WorkerDispatchCycleRequest::new("worker-4", 1, Duration::from_secs(20)).at(now),
                &ctx,
            )
            .expect("dispatch cycle");

        let observation = coordinator(&store)
            .retry_claim(
                cycle.claimed().first().expect("claimed").clone(),
                now + Duration::from_secs(2),
                now + Duration::from_secs(20),
                "temporary failure",
                &ctx,
            )
            .expect("retry claim");

        assert_eq!(observation.heartbeat().active_leases(), 0);
        assert_eq!(
            store
                .record(&ExecutionId::new("exec_5"))
                .expect("stored record")
                .expect("record exists")
                .status(),
            ExecutionRecordStatus::Pending
        );
        assert_eq!(
            store
                .record(&ExecutionId::new("exec_5"))
                .expect("stored record")
                .expect("record exists")
                .attempt(),
            2
        );
    }

    #[test]
    fn snapshot_queries_distinguish_visible_and_expired_workers() {
        let store = InMemoryExecutionWorkStore::new();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(500);
        let ctx = ctx();

        ExecutionJournal::append(&store, record("exec_6", now), &ctx).expect("append exec_6");

        coordinator(&store)
            .dispatch_cycle(
                WorkerDispatchCycleRequest::new("worker-5", 1, Duration::from_secs(30)).at(now),
                &ctx,
            )
            .expect("dispatch cycle");
        coordinator(&store)
            .record_heartbeat(WorkerHeartbeatRequest::new("worker-6").at(now), &ctx)
            .expect("idle heartbeat");

        let visible = coordinator(&store)
            .list_visible_workers(
                VisibleWorkerListQuery::new().at(now + Duration::from_secs(5)),
                &ctx,
            )
            .expect("visible workers");
        assert_eq!(visible.len(), 2);

        let snapshots = coordinator(&store)
            .list_worker_snapshots(
                WorkerSnapshotListQuery::new()
                    .at(now + Duration::from_secs(11))
                    .with_expired_workers(false),
                &ctx,
            )
            .expect("worker snapshots");
        assert!(snapshots.is_empty());

        let expired = coordinator(&store)
            .snapshot_worker(
                WorkerSnapshotQuery::new("worker-5").at(now + Duration::from_secs(11)),
                &ctx,
            )
            .expect("snapshot lookup")
            .expect("expired snapshot");
        assert_eq!(expired.visibility(), WorkerVisibility::Expired);
        assert_eq!(expired.active_leases(), 1);
    }

    #[test]
    fn invalid_requests_are_rejected_before_store_mutation() {
        let store = InMemoryExecutionWorkStore::new();
        let ctx = ctx();
        let error = coordinator(&store)
            .dispatch_cycle(
                WorkerDispatchCycleRequest::new("   ", 1, Duration::from_secs(0)),
                &ctx,
            )
            .expect_err("invalid cycle should fail");
        assert_eq!(error.code(), "worker_dispatch_worker_id_required");

        let config = WorkerDispatchCoordinatorConfig::new(
            DispatchQueue::Execution,
            "   ",
            Duration::from_secs(10),
        );
        let error = WorkerDispatchCoordinator::new(&store, &store, &store, config)
            .record_heartbeat(WorkerHeartbeatRequest::new("worker-7"), &ctx)
            .expect_err("invalid config should fail");
        assert_eq!(error.code(), "worker_dispatch_scope_required");
    }

    #[test]
    fn telemetry_is_emitted_for_worker_lifecycle_steps() {
        let store = InMemoryExecutionWorkStore::new();
        let telemetry = RecordingTelemetry::default();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(600);
        let ctx = ctx();

        ExecutionJournal::append(&store, record("exec_7", now), &ctx).expect("append exec_7");

        let scheduler = WorkerDispatchCoordinator::new(
            &store,
            &store,
            &store,
            WorkerDispatchCoordinatorConfig::new(
                DispatchQueue::Execution,
                "scheduler",
                Duration::from_secs(10),
            ),
        )
        .with_telemetry(&telemetry);

        let cycle = scheduler
            .dispatch_cycle(
                WorkerDispatchCycleRequest::new("worker-8", 1, Duration::from_secs(30)).at(now),
                &ctx,
            )
            .expect("dispatch cycle");
        scheduler
            .renew_claim(
                &cycle.claimed()[0],
                now + Duration::from_secs(2),
                Duration::from_secs(10),
                &ctx,
            )
            .expect("renew claim");
        scheduler
            .complete_claim(
                cycle.claimed()[0].clone(),
                now + Duration::from_secs(3),
                &ctx,
            )
            .expect("complete claim");

        assert_eq!(
            telemetry.names(),
            vec![
                "worker.dispatch_cycle".to_string(),
                "worker.lease_renewed".to_string(),
                "worker.dispatch_finalized".to_string(),
            ]
        );

        let _ = NoopTelemetry;
    }
}
