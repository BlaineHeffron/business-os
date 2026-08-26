use std::time::{Duration, SystemTime};

use crate::{
    AppError, AppResult, ClaimedExecution, ClaimedOutboxDelivery, DueExecutionRequest,
    DueOutboxMessageRequest, ErrorCode, ExecutionContext, ExecutionDeadLetter, ExecutionLease,
    ExecutionLeaseRenewal, ExecutionLeaseRequest, ExecutionRecord, ExecutionRetry,
    ExecutionSuccess, LeaseId, OutboxDeliveryDeadLetter, OutboxDeliveryLease,
    OutboxDeliveryLeaseRenewal, OutboxDeliveryLeaseRequest, OutboxDeliveryRetry,
    OutboxDeliverySuccess, OutboxMessage, WorkerHeartbeat, WorkerVisibility,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DispatchQueue {
    Execution,
    Outbox,
}

impl DispatchQueue {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Execution => "execution",
            Self::Outbox => "outbox",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchSelectionRequest {
    queue: DispatchQueue,
    due_before: SystemTime,
    limit: usize,
    include_expired_leases: bool,
}

impl DispatchSelectionRequest {
    pub fn new(queue: DispatchQueue, limit: usize) -> Self {
        Self {
            queue,
            due_before: SystemTime::now(),
            limit: limit.max(1),
            include_expired_leases: true,
        }
    }

    pub fn queue(&self) -> DispatchQueue {
        self.queue
    }

    pub fn due_before(&self) -> SystemTime {
        self.due_before
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    pub fn include_expired_leases(&self) -> bool {
        self.include_expired_leases
    }

    pub fn at(mut self, due_before: SystemTime) -> Self {
        self.due_before = due_before;
        self
    }

    pub fn with_expired_leases(mut self, include_expired_leases: bool) -> Self {
        self.include_expired_leases = include_expired_leases;
        self
    }

    pub fn as_execution_request(&self) -> Option<DueExecutionRequest> {
        (self.queue == DispatchQueue::Execution).then(|| {
            DueExecutionRequest::new(self.limit)
                .at(self.due_before)
                .with_expired_leases(self.include_expired_leases)
        })
    }

    pub fn as_outbox_request(&self) -> Option<DueOutboxMessageRequest> {
        (self.queue == DispatchQueue::Outbox).then(|| {
            DueOutboxMessageRequest::new(self.limit)
                .at(self.due_before)
                .with_expired_leases(self.include_expired_leases)
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchClaimRequest {
    queue: DispatchQueue,
    worker_id: String,
    batch_size: usize,
    lease_ttl: Duration,
    now: SystemTime,
}

impl DispatchClaimRequest {
    pub fn new(
        queue: DispatchQueue,
        worker_id: impl Into<String>,
        batch_size: usize,
        lease_ttl: Duration,
    ) -> Self {
        Self {
            queue,
            worker_id: worker_id.into(),
            batch_size: batch_size.max(1),
            lease_ttl,
            now: SystemTime::now(),
        }
    }

    pub fn queue(&self) -> DispatchQueue {
        self.queue
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

    pub fn now(&self) -> SystemTime {
        self.now
    }

    pub fn at(mut self, now: SystemTime) -> Self {
        self.now = now;
        self
    }

    pub fn as_execution_request(&self) -> Option<ExecutionLeaseRequest> {
        (self.queue == DispatchQueue::Execution).then(|| {
            ExecutionLeaseRequest::new(self.worker_id.clone(), self.batch_size, self.lease_ttl)
                .at(self.now)
        })
    }

    pub fn as_outbox_request(&self) -> Option<OutboxDeliveryLeaseRequest> {
        (self.queue == DispatchQueue::Outbox).then(|| {
            OutboxDeliveryLeaseRequest::new(self.worker_id.clone(), self.batch_size, self.lease_ttl)
                .at(self.now)
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DueDispatchWork {
    Execution(ExecutionRecord),
    Outbox(OutboxMessage),
}

impl DueDispatchWork {
    pub fn queue(&self) -> DispatchQueue {
        match self {
            Self::Execution(_) => DispatchQueue::Execution,
            Self::Outbox(_) => DispatchQueue::Outbox,
        }
    }

    pub fn available_at(&self) -> SystemTime {
        match self {
            Self::Execution(record) => record.available_at(),
            Self::Outbox(message) => message.available_at(),
        }
    }

    pub fn lease_id_hint(&self) -> Option<&LeaseId> {
        None
    }

    pub fn claim(self, lease: DispatchLease) -> AppResult<ClaimedDispatchWork> {
        match (self, lease) {
            (Self::Execution(record), DispatchLease::Execution(lease)) => {
                ClaimedExecution::claim(record, lease).map(ClaimedDispatchWork::Execution)
            }
            (Self::Outbox(message), DispatchLease::Outbox(lease)) => {
                ClaimedOutboxDelivery::claim(message, lease).map(ClaimedDispatchWork::Outbox)
            }
            (Self::Execution(record), DispatchLease::Outbox(_)) => Err(AppError::new(
                ErrorCode::InvalidInput,
                "dispatch_claim_queue_mismatch",
                "execution work requires an execution lease",
                record.correlation_id().clone(),
            )),
            (Self::Outbox(_), DispatchLease::Execution(_)) => Err(AppError::new(
                ErrorCode::InvalidInput,
                "dispatch_claim_queue_mismatch",
                "outbox work requires an outbox lease",
                crate::CorrelationId::generate(),
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchLease {
    Execution(ExecutionLease),
    Outbox(OutboxDeliveryLease),
}

impl DispatchLease {
    pub fn queue(&self) -> DispatchQueue {
        match self {
            Self::Execution(_) => DispatchQueue::Execution,
            Self::Outbox(_) => DispatchQueue::Outbox,
        }
    }

    pub fn lease_id(&self) -> &LeaseId {
        match self {
            Self::Execution(lease) => lease.lease_id(),
            Self::Outbox(lease) => lease.lease_id(),
        }
    }

    pub fn leased_by(&self) -> &str {
        match self {
            Self::Execution(lease) => lease.leased_by(),
            Self::Outbox(lease) => lease.leased_by(),
        }
    }

    pub fn leased_at(&self) -> SystemTime {
        match self {
            Self::Execution(lease) => lease.leased_at(),
            Self::Outbox(lease) => lease.leased_at(),
        }
    }

    pub fn lease_expires_at(&self) -> SystemTime {
        match self {
            Self::Execution(lease) => lease.lease_expires_at(),
            Self::Outbox(lease) => lease.lease_expires_at(),
        }
    }

    pub fn is_expired_at(&self, now: SystemTime) -> bool {
        match self {
            Self::Execution(lease) => lease.is_expired_at(now),
            Self::Outbox(lease) => lease.is_expired_at(now),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchLeaseRenewal {
    Execution(ExecutionLeaseRenewal),
    Outbox(OutboxDeliveryLeaseRenewal),
}

impl DispatchLeaseRenewal {
    pub fn queue(&self) -> DispatchQueue {
        match self {
            Self::Execution(_) => DispatchQueue::Execution,
            Self::Outbox(_) => DispatchQueue::Outbox,
        }
    }

    pub fn lease_id(&self) -> &LeaseId {
        match self {
            Self::Execution(renewal) => renewal.lease_id(),
            Self::Outbox(renewal) => renewal.lease_id(),
        }
    }

    pub fn leased_by(&self) -> &str {
        match self {
            Self::Execution(renewal) => renewal.leased_by(),
            Self::Outbox(renewal) => renewal.leased_by(),
        }
    }

    pub fn renewed_at(&self) -> SystemTime {
        match self {
            Self::Execution(renewal) => renewal.renewed_at(),
            Self::Outbox(renewal) => renewal.renewed_at(),
        }
    }

    pub fn lease_ttl(&self) -> Duration {
        match self {
            Self::Execution(renewal) => renewal.lease_ttl(),
            Self::Outbox(renewal) => renewal.lease_ttl(),
        }
    }

    pub fn validate(&self, ctx: &ExecutionContext) -> AppResult<()> {
        match self {
            Self::Execution(renewal) => renewal.validate(ctx),
            Self::Outbox(renewal) => renewal.validate(ctx),
        }
    }

    pub fn renewed_lease(&self) -> DispatchLease {
        match self {
            Self::Execution(renewal) => DispatchLease::Execution(renewal.renewed_lease()),
            Self::Outbox(renewal) => DispatchLease::Outbox(renewal.renewed_lease()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimedDispatchWork {
    Execution(ClaimedExecution),
    Outbox(ClaimedOutboxDelivery),
}

impl ClaimedDispatchWork {
    pub fn queue(&self) -> DispatchQueue {
        match self {
            Self::Execution(_) => DispatchQueue::Execution,
            Self::Outbox(_) => DispatchQueue::Outbox,
        }
    }

    pub fn lease(&self) -> DispatchLease {
        match self {
            Self::Execution(claimed) => DispatchLease::Execution(claimed.lease().clone()),
            Self::Outbox(claimed) => DispatchLease::Outbox(claimed.lease().clone()),
        }
    }

    pub fn complete(self, completed_at: SystemTime) -> DispatchResolution {
        match self {
            Self::Execution(claimed) => {
                let (record, success) = claimed.complete(completed_at);
                DispatchResolution::ExecutionCompleted { record, success }
            }
            Self::Outbox(claimed) => {
                let (message, success) = claimed.complete(completed_at);
                DispatchResolution::OutboxCompleted { message, success }
            }
        }
    }

    pub fn retry(
        self,
        attempted_at: SystemTime,
        next_available_at: SystemTime,
        error: impl Into<String>,
    ) -> AppResult<DispatchResolution> {
        match self {
            Self::Execution(claimed) => {
                let (record, retry) = claimed.retry(attempted_at, next_available_at, error)?;
                Ok(DispatchResolution::ExecutionRetried { record, retry })
            }
            Self::Outbox(claimed) => {
                let (message, retry) = claimed.retry(attempted_at, next_available_at, error)?;
                Ok(DispatchResolution::OutboxRetried { message, retry })
            }
        }
    }

    pub fn dead_letter(
        self,
        dead_lettered_at: SystemTime,
        error: impl Into<String>,
    ) -> AppResult<DispatchResolution> {
        match self {
            Self::Execution(claimed) => {
                let (record, dead_letter) = claimed.dead_letter(dead_lettered_at, error)?;
                Ok(DispatchResolution::ExecutionDeadLettered {
                    record,
                    dead_letter,
                })
            }
            Self::Outbox(claimed) => {
                let (message, dead_letter) = claimed.dead_letter(dead_lettered_at, error)?;
                Ok(DispatchResolution::OutboxDeadLettered {
                    message,
                    dead_letter,
                })
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchResolution {
    ExecutionCompleted {
        record: ExecutionRecord,
        success: ExecutionSuccess,
    },
    ExecutionRetried {
        record: ExecutionRecord,
        retry: ExecutionRetry,
    },
    ExecutionDeadLettered {
        record: ExecutionRecord,
        dead_letter: ExecutionDeadLetter,
    },
    OutboxCompleted {
        message: OutboxMessage,
        success: OutboxDeliverySuccess,
    },
    OutboxRetried {
        message: OutboxMessage,
        retry: OutboxDeliveryRetry,
    },
    OutboxDeadLettered {
        message: OutboxMessage,
        dead_letter: OutboxDeliveryDeadLetter,
    },
}

impl DispatchResolution {
    pub fn queue(&self) -> DispatchQueue {
        match self {
            Self::ExecutionCompleted { .. }
            | Self::ExecutionRetried { .. }
            | Self::ExecutionDeadLettered { .. } => DispatchQueue::Execution,
            Self::OutboxCompleted { .. }
            | Self::OutboxRetried { .. }
            | Self::OutboxDeadLettered { .. } => DispatchQueue::Outbox,
        }
    }

    pub fn lease_id(&self) -> &LeaseId {
        match self {
            Self::ExecutionCompleted { success, .. } => success.lease_id(),
            Self::ExecutionRetried { retry, .. } => retry.lease_id(),
            Self::ExecutionDeadLettered { dead_letter, .. } => dead_letter.lease_id(),
            Self::OutboxCompleted { success, .. } => success.lease_id(),
            Self::OutboxRetried { retry, .. } => retry.lease_id(),
            Self::OutboxDeadLettered { dead_letter, .. } => dead_letter.lease_id(),
        }
    }
}

pub trait DispatchWorkStore: Send + Sync {
    fn select_due(
        &self,
        request: DispatchSelectionRequest,
        ctx: &ExecutionContext,
    ) -> AppResult<Vec<DueDispatchWork>>;

    fn claim_due(
        &self,
        request: DispatchClaimRequest,
        ctx: &ExecutionContext,
    ) -> AppResult<Vec<ClaimedDispatchWork>>;

    fn renew_claim(
        &self,
        renewal: DispatchLeaseRenewal,
        ctx: &ExecutionContext,
    ) -> AppResult<DispatchLease>;

    fn finalize(&self, resolution: DispatchResolution, ctx: &ExecutionContext) -> AppResult<()>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpiredLeaseRecoveryRequest {
    queue: DispatchQueue,
    now: SystemTime,
    limit: usize,
    max_attempts: u32,
}

impl ExpiredLeaseRecoveryRequest {
    pub fn new(queue: DispatchQueue, limit: usize, max_attempts: u32) -> Self {
        Self {
            queue,
            now: SystemTime::now(),
            limit: limit.max(1),
            max_attempts: max_attempts.max(1),
        }
    }

    pub fn queue(&self) -> DispatchQueue {
        self.queue
    }

    pub fn now(&self) -> SystemTime {
        self.now
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    pub fn max_attempts(&self) -> u32 {
        self.max_attempts
    }

    pub fn at(mut self, now: SystemTime) -> Self {
        self.now = now;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ExpiredLeaseRecoverySummary {
    recovered: usize,
    dead_lettered: usize,
}

impl ExpiredLeaseRecoverySummary {
    pub fn recovered(&self) -> usize {
        self.recovered
    }

    pub fn dead_lettered(&self) -> usize {
        self.dead_lettered
    }

    pub fn observe_recovered(&mut self) {
        self.recovered = self.recovered.saturating_add(1);
    }

    pub fn observe_dead_lettered(&mut self) {
        self.dead_lettered = self.dead_lettered.saturating_add(1);
    }
}

pub trait ExpiredLeaseRecoveryStore: Send + Sync {
    fn recover_expired_leases(
        &self,
        request: ExpiredLeaseRecoveryRequest,
        ctx: &ExecutionContext,
    ) -> AppResult<ExpiredLeaseRecoverySummary>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerDispatchSnapshotRef {
    worker_id: String,
    scope: String,
    observed_at: SystemTime,
}

impl WorkerDispatchSnapshotRef {
    pub fn new(worker_id: impl Into<String>, scope: impl Into<String>) -> Self {
        Self {
            worker_id: worker_id.into(),
            scope: scope.into(),
            observed_at: SystemTime::now(),
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

    pub fn at(mut self, observed_at: SystemTime) -> Self {
        self.observed_at = observed_at;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerDispatchSnapshotsQuery {
    scope: String,
    observed_at: SystemTime,
    limit: usize,
    include_expired_workers: bool,
}

impl WorkerDispatchSnapshotsQuery {
    pub fn new(scope: impl Into<String>) -> Self {
        Self {
            scope: scope.into(),
            observed_at: SystemTime::now(),
            limit: usize::MAX,
            include_expired_workers: true,
        }
    }

    pub fn scope(&self) -> &str {
        &self.scope
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchQueueStatusSnapshot {
    queue: DispatchQueue,
    due_count: usize,
    claimed_count: usize,
    stale_claim_count: usize,
    oldest_due_at: Option<SystemTime>,
}

impl DispatchQueueStatusSnapshot {
    pub fn new(queue: DispatchQueue) -> Self {
        Self {
            queue,
            due_count: 0,
            claimed_count: 0,
            stale_claim_count: 0,
            oldest_due_at: None,
        }
    }

    pub fn queue(&self) -> DispatchQueue {
        self.queue
    }

    pub fn due_count(&self) -> usize {
        self.due_count
    }

    pub fn claimed_count(&self) -> usize {
        self.claimed_count
    }

    pub fn stale_claim_count(&self) -> usize {
        self.stale_claim_count
    }

    pub fn oldest_due_at(&self) -> Option<SystemTime> {
        self.oldest_due_at
    }

    pub fn with_due_count(mut self, due_count: usize) -> Self {
        self.due_count = due_count;
        self
    }

    pub fn with_claimed_count(mut self, claimed_count: usize) -> Self {
        self.claimed_count = claimed_count;
        self
    }

    pub fn with_stale_claim_count(mut self, stale_claim_count: usize) -> Self {
        self.stale_claim_count = stale_claim_count;
        self
    }

    pub fn with_oldest_due_at(mut self, oldest_due_at: Option<SystemTime>) -> Self {
        self.oldest_due_at = oldest_due_at;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerDispatchSnapshot {
    worker_id: String,
    scope: String,
    observed_at: SystemTime,
    heartbeat: Option<WorkerHeartbeat>,
    queues: Vec<DispatchQueueStatusSnapshot>,
}

impl WorkerDispatchSnapshot {
    pub fn new(
        worker_id: impl Into<String>,
        scope: impl Into<String>,
        observed_at: SystemTime,
    ) -> Self {
        Self {
            worker_id: worker_id.into(),
            scope: scope.into(),
            observed_at,
            heartbeat: None,
            queues: Vec::new(),
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

    pub fn heartbeat(&self) -> Option<&WorkerHeartbeat> {
        self.heartbeat.as_ref()
    }

    pub fn queues(&self) -> &[DispatchQueueStatusSnapshot] {
        &self.queues
    }

    pub fn visibility(&self) -> WorkerVisibility {
        self.heartbeat
            .as_ref()
            .map(|heartbeat| heartbeat.visibility_at(self.observed_at))
            .unwrap_or(WorkerVisibility::Expired)
    }

    pub fn active_leases(&self) -> usize {
        self.heartbeat
            .as_ref()
            .map(|heartbeat| heartbeat.active_leases())
            .unwrap_or_else(|| self.claimed_work())
    }

    pub fn claimed_work(&self) -> usize {
        self.queues.iter().map(|queue| queue.claimed_count()).sum()
    }

    pub fn stale_claims(&self) -> usize {
        self.queues
            .iter()
            .map(|queue| queue.stale_claim_count())
            .sum()
    }

    pub fn queue(&self, queue: DispatchQueue) -> Option<&DispatchQueueStatusSnapshot> {
        self.queues
            .iter()
            .find(|snapshot| snapshot.queue() == queue)
    }

    pub fn with_heartbeat(mut self, heartbeat: WorkerHeartbeat) -> Self {
        self.heartbeat = Some(heartbeat);
        self
    }

    pub fn push_queue(mut self, queue: DispatchQueueStatusSnapshot) -> Self {
        self.queues
            .retain(|existing| existing.queue() != queue.queue());
        self.queues.push(queue);
        self
    }
}

pub trait DispatchStatusStore: Send + Sync {
    fn lookup_worker_snapshot(
        &self,
        worker: WorkerDispatchSnapshotRef,
        ctx: &ExecutionContext,
    ) -> AppResult<Option<WorkerDispatchSnapshot>>;

    fn list_worker_snapshots(
        &self,
        query: WorkerDispatchSnapshotsQuery,
        ctx: &ExecutionContext,
    ) -> AppResult<Vec<WorkerDispatchSnapshot>>;
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use crate::{
        ClaimedExecution, ClaimedOutboxDelivery, CorrelationId, ExecutionId, ExecutionRecordStatus,
        IdempotencyKey, LeaseId, OutboxEnvelope, OutboxMessage, OutboxMessageId, WorkerHeartbeat,
        WorkerVisibility,
    };

    use super::{
        ClaimedDispatchWork, DispatchClaimRequest, DispatchLease, DispatchLeaseRenewal,
        DispatchQueue, DispatchQueueStatusSnapshot, DispatchResolution, DispatchSelectionRequest,
        DueDispatchWork, WorkerDispatchSnapshot, WorkerDispatchSnapshotsQuery,
    };

    #[test]
    fn dispatch_selection_request_converts_to_typed_requests() {
        let due_before = SystemTime::UNIX_EPOCH + Duration::from_secs(20);

        let execution = DispatchSelectionRequest::new(DispatchQueue::Execution, 0)
            .at(due_before)
            .with_expired_leases(false)
            .as_execution_request()
            .expect("execution request");
        assert_eq!(execution.limit(), 1);
        assert_eq!(execution.due_before(), due_before);
        assert!(!execution.include_expired_leases());

        let outbox = DispatchSelectionRequest::new(DispatchQueue::Outbox, 3)
            .at(due_before)
            .as_outbox_request()
            .expect("outbox request");
        assert_eq!(outbox.limit(), 3);
        assert_eq!(outbox.due_before(), due_before);
        assert!(outbox.include_expired_leases());
    }

    #[test]
    fn dispatch_claim_request_converts_to_queue_specific_lease_requests() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(50);
        let request = DispatchClaimRequest::new(
            DispatchQueue::Outbox,
            "worker-1",
            0,
            Duration::from_secs(15),
        )
        .at(now);

        let outbox = request.as_outbox_request().expect("outbox request");
        assert_eq!(outbox.leased_by(), "worker-1");
        assert_eq!(outbox.batch_size(), 1);
        assert_eq!(outbox.lease_ttl(), Duration::from_secs(15));
        assert_eq!(outbox.now(), now);
        assert!(request.as_execution_request().is_none());
    }

    #[test]
    fn due_dispatch_work_claim_rejects_queue_mismatch() {
        let record = crate::ExecutionRecord::new(
            ExecutionId::new("exec_1"),
            "dispatch",
            "task_1",
            IdempotencyKey::new("idem_1"),
            CorrelationId::new("corr_1"),
        );
        let lease = DispatchLease::Outbox(crate::OutboxDeliveryLease::from_now(
            LeaseId::new("lease_1"),
            "worker-1",
            Duration::from_secs(30),
            SystemTime::UNIX_EPOCH,
        ));

        let error = DueDispatchWork::Execution(record)
            .claim(lease)
            .expect_err("mismatched lease should fail");
        assert_eq!(error.code(), "dispatch_claim_queue_mismatch");
    }

    #[test]
    fn claimed_dispatch_work_completes_execution_and_outbox_flows() {
        let leased_at = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let execution = crate::ExecutionRecord::new(
            ExecutionId::new("exec_2"),
            "dispatch",
            "task_2",
            IdempotencyKey::new("idem_2"),
            CorrelationId::new("corr_2"),
        )
        .with_recorded_at(leased_at);
        let execution_lease = crate::ExecutionLease::from_now(
            LeaseId::new("lease_exec_2"),
            "worker-exec",
            Duration::from_secs(30),
            leased_at,
        );
        let claimed_execution = ClaimedExecution::claim(execution, execution_lease)
            .expect("execution claim should succeed");

        let completed_at = leased_at + Duration::from_secs(4);
        let execution_resolution =
            ClaimedDispatchWork::Execution(claimed_execution).complete(completed_at);
        match execution_resolution {
            DispatchResolution::ExecutionCompleted { record, success } => {
                assert_eq!(record.status(), ExecutionRecordStatus::Succeeded);
                assert_eq!(success.lease_id().as_str(), "lease_exec_2");
            }
            other => panic!("unexpected resolution: {other:?}"),
        }

        let message = OutboxMessage::new(
            OutboxMessageId::new("msg_1"),
            "thread",
            "thread_1",
            ExecutionId::new("exec_3"),
            OutboxEnvelope::new("agent_bus", "thread_1", "{\"ok\":true}"),
        )
        .schedule_at(leased_at);
        let outbox_lease = crate::OutboxDeliveryLease::from_now(
            LeaseId::new("lease_outbox_1"),
            "worker-outbox",
            Duration::from_secs(20),
            leased_at,
        );
        let claimed_outbox = ClaimedOutboxDelivery::claim(message, outbox_lease)
            .expect("outbox claim should succeed");

        let outbox_resolution = ClaimedDispatchWork::Outbox(claimed_outbox).complete(completed_at);
        match outbox_resolution {
            DispatchResolution::OutboxCompleted { message, success } => {
                assert_eq!(message.attempts(), 1);
                assert_eq!(success.lease_id().as_str(), "lease_outbox_1");
            }
            other => panic!("unexpected resolution: {other:?}"),
        }
    }

    #[test]
    fn claimed_dispatch_work_supports_retry_and_dead_letter() {
        let leased_at = SystemTime::UNIX_EPOCH + Duration::from_secs(200);
        let message = OutboxMessage::new(
            OutboxMessageId::new("msg_2"),
            "thread",
            "thread_2",
            ExecutionId::new("exec_4"),
            OutboxEnvelope::new("agent_bus", "thread_2", "{\"ok\":false}"),
        )
        .schedule_at(leased_at);
        let outbox_lease = crate::OutboxDeliveryLease::from_now(
            LeaseId::new("lease_outbox_2"),
            "worker-outbox-2",
            Duration::from_secs(20),
            leased_at,
        );
        let claimed_outbox = ClaimedOutboxDelivery::claim(message, outbox_lease)
            .expect("outbox claim should succeed");

        let retry = ClaimedDispatchWork::Outbox(claimed_outbox)
            .retry(
                leased_at + Duration::from_secs(1),
                leased_at + Duration::from_secs(10),
                "transport down",
            )
            .expect("retry should succeed");
        assert!(matches!(retry, DispatchResolution::OutboxRetried { .. }));

        let record = crate::ExecutionRecord::new(
            ExecutionId::new("exec_5"),
            "dispatch",
            "task_5",
            IdempotencyKey::new("idem_5"),
            CorrelationId::new("corr_5"),
        )
        .with_recorded_at(leased_at);
        let execution_lease = crate::ExecutionLease::from_now(
            LeaseId::new("lease_exec_5"),
            "worker-exec-5",
            Duration::from_secs(30),
            leased_at,
        );
        let claimed_execution = ClaimedExecution::claim(record, execution_lease)
            .expect("execution claim should succeed");

        let dead_letter = ClaimedDispatchWork::Execution(claimed_execution)
            .dead_letter(leased_at + Duration::from_secs(2), "permanent failure")
            .expect("dead letter should succeed");
        assert!(matches!(
            dead_letter,
            DispatchResolution::ExecutionDeadLettered { .. }
        ));
    }

    #[test]
    fn dispatch_lease_renewal_wraps_typed_leases() {
        let renewed_at = SystemTime::UNIX_EPOCH + Duration::from_secs(300);
        let renewal = DispatchLeaseRenewal::Execution(
            crate::ExecutionLeaseRenewal::new(
                ExecutionId::new("exec_6"),
                LeaseId::new("lease_exec_6"),
                "worker-6",
                Duration::from_secs(45),
            )
            .at(renewed_at),
        );
        let lease = renewal.renewed_lease();
        assert_eq!(lease.queue(), DispatchQueue::Execution);
        assert_eq!(lease.leased_by(), "worker-6");
        assert_eq!(
            lease.lease_expires_at(),
            renewed_at + Duration::from_secs(45)
        );
    }

    #[test]
    fn worker_dispatch_snapshots_track_visibility_and_claimed_counts() {
        let observed_at = SystemTime::UNIX_EPOCH + Duration::from_secs(400);
        let heartbeat = WorkerHeartbeat::from_ttl(
            "worker-7",
            "scheduler",
            observed_at - Duration::from_secs(5),
            Duration::from_secs(10),
        )
        .with_active_leases(4);

        let snapshot = WorkerDispatchSnapshot::new("worker-7", "scheduler", observed_at)
            .with_heartbeat(heartbeat)
            .push_queue(
                DispatchQueueStatusSnapshot::new(DispatchQueue::Execution)
                    .with_due_count(2)
                    .with_claimed_count(3)
                    .with_stale_claim_count(1),
            )
            .push_queue(
                DispatchQueueStatusSnapshot::new(DispatchQueue::Outbox)
                    .with_claimed_count(1)
                    .with_oldest_due_at(Some(observed_at - Duration::from_secs(20))),
            );

        assert_eq!(snapshot.visibility(), WorkerVisibility::Visible);
        assert_eq!(snapshot.active_leases(), 4);
        assert_eq!(snapshot.claimed_work(), 4);
        assert_eq!(snapshot.stale_claims(), 1);
        assert_eq!(
            snapshot
                .queue(DispatchQueue::Outbox)
                .expect("outbox snapshot")
                .claimed_count(),
            1
        );
    }

    #[test]
    fn worker_dispatch_snapshot_queries_clamp_limits() {
        let observed_at = SystemTime::UNIX_EPOCH + Duration::from_secs(500);
        let query = WorkerDispatchSnapshotsQuery::new("scheduler")
            .at(observed_at)
            .with_limit(0)
            .with_expired_workers(false);

        assert_eq!(query.scope(), "scheduler");
        assert_eq!(query.observed_at(), observed_at);
        assert_eq!(query.limit(), 1);
        assert!(!query.include_expired_workers());
    }
}
