use std::time::SystemTime;

use super::types_views::{ReferenceQueueSummary, ReferenceWorkerSummary};
use crate::{
    ClaimedDispatchWork, DispatchQueue, ExecutionInspectionQuery, ExecutionInspectionRecord,
    ExecutionInspectionSummary, ExecutionRecordStatus, LeaseId, OutboxInspectionQuery,
    OutboxInspectionRecord, OutboxInspectionSummary, OutboxMessageStatus, WorkerDispatchSnapshot,
    WorkerObservation,
};

pub struct ReferenceActiveClaimRef {
    worker_id: String,
    lease_id: LeaseId,
}

impl ReferenceActiveClaimRef {
    pub fn new(worker_id: impl Into<String>, lease_id: LeaseId) -> Self {
        Self {
            worker_id: worker_id.into(),
            lease_id,
        }
    }

    pub fn worker_id(&self) -> &str {
        &self.worker_id
    }

    pub fn lease_id(&self) -> &LeaseId {
        &self.lease_id
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceWorkClaim {
    queue: DispatchQueue,
    worker_id: String,
    lease_id: LeaseId,
    leased_at: SystemTime,
    lease_expires_at: SystemTime,
    work_id: String,
    available_at: SystemTime,
}

impl ReferenceWorkClaim {
    pub(crate) fn from_claimed(claimed: &ClaimedDispatchWork) -> Self {
        match claimed {
            ClaimedDispatchWork::Execution(claimed) => Self {
                queue: DispatchQueue::Execution,
                worker_id: claimed.lease().leased_by().to_string(),
                lease_id: claimed.lease().lease_id().clone(),
                leased_at: claimed.lease().leased_at(),
                lease_expires_at: claimed.lease().lease_expires_at(),
                work_id: claimed.record().id().as_str().to_string(),
                available_at: claimed.record().available_at(),
            },
            ClaimedDispatchWork::Outbox(claimed) => Self {
                queue: DispatchQueue::Outbox,
                worker_id: claimed.lease().leased_by().to_string(),
                lease_id: claimed.lease().lease_id().clone(),
                leased_at: claimed.lease().leased_at(),
                lease_expires_at: claimed.lease().lease_expires_at(),
                work_id: claimed.message().id().as_str().to_string(),
                available_at: claimed.message().available_at(),
            },
        }
    }

    pub fn queue(&self) -> DispatchQueue {
        self.queue
    }

    pub fn worker_id(&self) -> &str {
        &self.worker_id
    }

    pub fn lease_id(&self) -> &LeaseId {
        &self.lease_id
    }

    pub fn leased_at(&self) -> SystemTime {
        self.leased_at
    }

    pub fn lease_expires_at(&self) -> SystemTime {
        self.lease_expires_at
    }

    pub fn work_id(&self) -> &str {
        &self.work_id
    }

    pub fn available_at(&self) -> SystemTime {
        self.available_at
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceDispatchCycle {
    observation: WorkerObservation,
    claims: Vec<ReferenceWorkClaim>,
}

impl ReferenceDispatchCycle {
    pub(crate) fn new(observation: WorkerObservation, claims: Vec<ReferenceWorkClaim>) -> Self {
        Self {
            observation,
            claims,
        }
    }

    pub fn observation(&self) -> &WorkerObservation {
        &self.observation
    }

    pub fn claims(&self) -> &[ReferenceWorkClaim] {
        &self.claims
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceLeaseRenewal {
    observation: WorkerObservation,
    previous_claim: ReferenceWorkClaim,
    renewed_claim: ReferenceWorkClaim,
}

impl ReferenceLeaseRenewal {
    pub(crate) fn new(
        observation: WorkerObservation,
        previous_claim: ReferenceWorkClaim,
        renewed_claim: ReferenceWorkClaim,
    ) -> Self {
        Self {
            observation,
            previous_claim,
            renewed_claim,
        }
    }

    pub fn observation(&self) -> &WorkerObservation {
        &self.observation
    }

    pub fn previous_claim(&self) -> &ReferenceWorkClaim {
        &self.previous_claim
    }

    pub fn renewed_claim(&self) -> &ReferenceWorkClaim {
        &self.renewed_claim
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceClaimDisposition {
    Completed,
    Retried,
    DeadLettered,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceClaimOutcome {
    observation: WorkerObservation,
    claim: ReferenceWorkClaim,
    disposition: ReferenceClaimDisposition,
}

impl ReferenceClaimOutcome {
    pub(crate) fn new(
        observation: WorkerObservation,
        claim: ReferenceWorkClaim,
        disposition: ReferenceClaimDisposition,
    ) -> Self {
        Self {
            observation,
            claim,
            disposition,
        }
    }

    pub fn observation(&self) -> &WorkerObservation {
        &self.observation
    }

    pub fn claim(&self) -> &ReferenceWorkClaim {
        &self.claim
    }

    pub fn disposition(&self) -> ReferenceClaimDisposition {
        self.disposition
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceWorkerSnapshot {
    worker_id: String,
    snapshot: Option<WorkerDispatchSnapshot>,
    active_claims: Vec<ReferenceWorkClaim>,
}

impl ReferenceWorkerSnapshot {
    pub(crate) fn new(
        worker_id: impl Into<String>,
        snapshot: Option<WorkerDispatchSnapshot>,
        active_claims: Vec<ReferenceWorkClaim>,
    ) -> Self {
        Self {
            worker_id: worker_id.into(),
            snapshot,
            active_claims,
        }
    }

    pub fn worker_id(&self) -> &str {
        &self.worker_id
    }

    pub fn snapshot(&self) -> Option<&WorkerDispatchSnapshot> {
        self.snapshot.as_ref()
    }

    pub fn active_claims(&self) -> &[ReferenceWorkClaim] {
        &self.active_claims
    }

    pub fn visibility(&self) -> crate::WorkerVisibility {
        self.snapshot
            .as_ref()
            .map(|snapshot| snapshot.visibility())
            .unwrap_or(crate::WorkerVisibility::Expired)
    }

    pub fn active_claim_count(&self) -> usize {
        self.active_claims.len()
    }

    pub fn claimed_work(&self) -> usize {
        self.snapshot
            .as_ref()
            .map(|snapshot| snapshot.claimed_work())
            .unwrap_or(self.active_claims.len())
    }

    pub fn stale_claims(&self) -> usize {
        self.snapshot
            .as_ref()
            .map(|snapshot| snapshot.stale_claims())
            .unwrap_or(0)
    }

    pub fn due_work(&self) -> usize {
        self.snapshot
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .queues()
                    .iter()
                    .map(|queue| queue.due_count())
                    .sum::<usize>()
            })
            .unwrap_or(0)
    }

    pub fn queue(&self, queue: DispatchQueue) -> Option<&crate::DispatchQueueStatusSnapshot> {
        self.snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.queue(queue))
    }

    pub fn operator_summary(&self) -> ReferenceWorkerSummary {
        ReferenceWorkerSummary::from_snapshot(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceSchedulerStateQuery {
    observed_at: SystemTime,
    worker_limit: usize,
    backlog_limit: usize,
    active_claim_limit: usize,
    include_expired_workers: bool,
    execution_status: Option<ExecutionRecordStatus>,
    outbox_status: Option<OutboxMessageStatus>,
    operation: Option<String>,
    target: Option<String>,
    aggregate: Option<String>,
    topic: Option<String>,
}

impl ReferenceSchedulerStateQuery {
    pub fn new() -> Self {
        Self {
            observed_at: SystemTime::now(),
            worker_limit: usize::MAX,
            backlog_limit: 50,
            active_claim_limit: 50,
            include_expired_workers: true,
            execution_status: None,
            outbox_status: None,
            operation: None,
            target: None,
            aggregate: None,
            topic: None,
        }
    }

    pub fn observed_at(&self) -> SystemTime {
        self.observed_at
    }

    pub fn worker_limit(&self) -> usize {
        self.worker_limit
    }

    pub fn backlog_limit(&self) -> usize {
        self.backlog_limit
    }

    pub fn active_claim_limit(&self) -> usize {
        self.active_claim_limit
    }

    pub fn include_expired_workers(&self) -> bool {
        self.include_expired_workers
    }

    pub fn execution_status(&self) -> Option<ExecutionRecordStatus> {
        self.execution_status
    }

    pub fn outbox_status(&self) -> Option<OutboxMessageStatus> {
        self.outbox_status
    }

    pub fn operation(&self) -> Option<&str> {
        self.operation.as_deref()
    }

    pub fn target(&self) -> Option<&str> {
        self.target.as_deref()
    }

    pub fn aggregate(&self) -> Option<&str> {
        self.aggregate.as_deref()
    }

    pub fn topic(&self) -> Option<&str> {
        self.topic.as_deref()
    }

    pub fn at(mut self, observed_at: SystemTime) -> Self {
        self.observed_at = observed_at;
        self
    }

    pub fn with_worker_limit(mut self, worker_limit: usize) -> Self {
        self.worker_limit = worker_limit.max(1);
        self
    }

    pub fn with_backlog_limit(mut self, backlog_limit: usize) -> Self {
        self.backlog_limit = backlog_limit.max(1);
        self
    }

    pub fn with_active_claim_limit(mut self, active_claim_limit: usize) -> Self {
        self.active_claim_limit = active_claim_limit.max(1);
        self
    }

    pub fn with_expired_workers(mut self, include_expired_workers: bool) -> Self {
        self.include_expired_workers = include_expired_workers;
        self
    }

    pub fn with_execution_status(mut self, status: ExecutionRecordStatus) -> Self {
        self.execution_status = Some(status);
        self
    }

    pub fn with_outbox_status(mut self, status: OutboxMessageStatus) -> Self {
        self.outbox_status = Some(status);
        self
    }

    pub fn with_operation(mut self, operation: impl Into<String>) -> Self {
        self.operation = Some(operation.into());
        self
    }

    pub fn with_target(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }

    pub fn with_aggregate(mut self, aggregate: impl Into<String>) -> Self {
        self.aggregate = Some(aggregate.into());
        self
    }

    pub fn with_topic(mut self, topic: impl Into<String>) -> Self {
        self.topic = Some(topic.into());
        self
    }

    pub fn execution_query(&self, limit: usize) -> ExecutionInspectionQuery {
        let mut inspection = ExecutionInspectionQuery::new(limit).at(self.observed_at());
        if let Some(status) = self.execution_status() {
            inspection = inspection.with_status(status);
        }
        if let Some(operation) = self.operation() {
            inspection = inspection.with_operation(operation.to_string());
        }
        if let Some(target) = self.target() {
            inspection = inspection.with_target(target.to_string());
        }
        inspection
    }

    pub fn outbox_query(&self, limit: usize) -> OutboxInspectionQuery {
        let mut inspection = OutboxInspectionQuery::new(limit).at(self.observed_at());
        if let Some(status) = self.outbox_status() {
            inspection = inspection.with_status(status);
        }
        if let Some(aggregate) = self.aggregate() {
            inspection = inspection.with_aggregate(aggregate.to_string());
        }
        if let Some(topic) = self.topic() {
            inspection = inspection.with_topic(topic.to_string());
        }
        inspection
    }
}

impl Default for ReferenceSchedulerStateQuery {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReferenceQueueInspectionSummary {
    Execution(ExecutionInspectionSummary),
    Outbox(OutboxInspectionSummary),
}

impl ReferenceQueueInspectionSummary {
    pub fn total(&self) -> usize {
        match self {
            Self::Execution(summary) => summary.total(),
            Self::Outbox(summary) => summary.total(),
        }
    }

    pub fn due(&self) -> usize {
        match self {
            Self::Execution(summary) => summary.due(),
            Self::Outbox(summary) => summary.due(),
        }
    }

    pub fn leased(&self) -> usize {
        match self {
            Self::Execution(summary) => summary.leased(),
            Self::Outbox(summary) => summary.leased(),
        }
    }

    pub fn retry_scheduled(&self) -> usize {
        match self {
            Self::Execution(summary) => summary.retry_scheduled(),
            Self::Outbox(summary) => summary.retry_scheduled(),
        }
    }

    pub fn stale_leases(&self) -> usize {
        match self {
            Self::Execution(summary) => summary.stale_leases(),
            Self::Outbox(summary) => summary.stale_leases(),
        }
    }

    pub fn oldest_due_at(&self) -> Option<SystemTime> {
        match self {
            Self::Execution(summary) => summary.oldest_due_at(),
            Self::Outbox(summary) => summary.oldest_due_at(),
        }
    }

    pub fn pending(&self) -> usize {
        match self {
            Self::Execution(summary) => summary.pending(),
            Self::Outbox(summary) => summary.pending(),
        }
    }

    pub fn in_flight(&self) -> usize {
        match self {
            Self::Execution(summary) => summary.in_flight(),
            Self::Outbox(summary) => summary.in_flight(),
        }
    }

    pub fn completed(&self) -> usize {
        match self {
            Self::Execution(summary) => summary.succeeded(),
            Self::Outbox(summary) => summary.delivered(),
        }
    }

    pub fn failed(&self) -> usize {
        match self {
            Self::Execution(summary) => summary.failed(),
            Self::Outbox(_) => 0,
        }
    }

    pub fn dead_lettered(&self) -> usize {
        match self {
            Self::Execution(summary) => summary.dead_lettered(),
            Self::Outbox(summary) => summary.dead_lettered(),
        }
    }

    pub fn operator_summary(
        &self,
        queue: DispatchQueue,
        observed_at: SystemTime,
    ) -> ReferenceQueueSummary {
        ReferenceQueueSummary::new(queue, observed_at, self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReferenceQueueInspectionRecord {
    Execution(ExecutionInspectionRecord),
    Outbox(OutboxInspectionRecord),
}

impl ReferenceQueueInspectionRecord {
    pub fn queue(&self) -> DispatchQueue {
        match self {
            Self::Execution(_) => DispatchQueue::Execution,
            Self::Outbox(_) => DispatchQueue::Outbox,
        }
    }

    pub fn execution(&self) -> Option<&ExecutionInspectionRecord> {
        match self {
            Self::Execution(record) => Some(record),
            Self::Outbox(_) => None,
        }
    }

    pub fn outbox(&self) -> Option<&OutboxInspectionRecord> {
        match self {
            Self::Execution(_) => None,
            Self::Outbox(record) => Some(record),
        }
    }

    pub fn id(&self) -> &str {
        match self {
            Self::Execution(record) => record.record().id().as_str(),
            Self::Outbox(record) => record.message().id().as_str(),
        }
    }

    pub fn available_at(&self) -> SystemTime {
        match self {
            Self::Execution(record) => record.record().available_at(),
            Self::Outbox(record) => record.message().available_at(),
        }
    }

    pub fn lease_id(&self) -> Option<&LeaseId> {
        match self {
            Self::Execution(record) => record.lease_id(),
            Self::Outbox(record) => record.lease_id(),
        }
    }

    pub fn leased_by(&self) -> Option<&str> {
        match self {
            Self::Execution(record) => record.leased_by(),
            Self::Outbox(record) => record.leased_by(),
        }
    }

    pub fn lease_expires_at(&self) -> Option<SystemTime> {
        match self {
            Self::Execution(record) => record.lease_expires_at(),
            Self::Outbox(record) => record.lease_expires_at(),
        }
    }

    pub fn last_error(&self) -> Option<&str> {
        match self {
            Self::Execution(record) => record.last_error(),
            Self::Outbox(record) => record.last_error(),
        }
    }

    pub fn is_terminal(&self) -> bool {
        match self {
            Self::Execution(record) => record.is_terminal(),
            Self::Outbox(record) => record.is_terminal(),
        }
    }

    pub fn is_due_at(&self, observed_at: SystemTime) -> bool {
        match self {
            Self::Execution(record) => record.is_due_at(observed_at),
            Self::Outbox(record) => record.is_due_at(observed_at),
        }
    }

    pub fn is_retry_scheduled_at(&self, observed_at: SystemTime) -> bool {
        match self {
            Self::Execution(record) => record.is_retry_scheduled_at(observed_at),
            Self::Outbox(record) => record.is_retry_scheduled_at(observed_at),
        }
    }

    pub fn has_active_lease_at(&self, observed_at: SystemTime) -> bool {
        match self {
            Self::Execution(record) => record.has_active_lease_at(observed_at),
            Self::Outbox(record) => record.has_active_lease_at(observed_at),
        }
    }

    pub fn has_stale_lease_at(&self, observed_at: SystemTime) -> bool {
        match self {
            Self::Execution(record) => record.has_stale_lease_at(observed_at),
            Self::Outbox(record) => record.has_stale_lease_at(observed_at),
        }
    }

    pub fn snapshot_at(&self, observed_at: SystemTime) -> ReferenceQueueInspectionSnapshot {
        match self {
            Self::Execution(record) => {
                ReferenceQueueInspectionSnapshot::Execution(record.snapshot_at(observed_at))
            }
            Self::Outbox(record) => {
                ReferenceQueueInspectionSnapshot::Outbox(record.snapshot_at(observed_at))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReferenceQueueInspectionSnapshot {
    Execution(crate::ExecutionInspectionSnapshot),
    Outbox(crate::OutboxInspectionSnapshot),
}

impl ReferenceQueueInspectionSnapshot {
    pub fn queue(&self) -> DispatchQueue {
        match self {
            Self::Execution(_) => DispatchQueue::Execution,
            Self::Outbox(_) => DispatchQueue::Outbox,
        }
    }

    pub fn id(&self) -> &str {
        match self {
            Self::Execution(snapshot) => snapshot.execution_id().as_str(),
            Self::Outbox(snapshot) => snapshot.message_id().as_str(),
        }
    }

    pub fn available_at(&self) -> SystemTime {
        match self {
            Self::Execution(snapshot) => snapshot.available_at(),
            Self::Outbox(snapshot) => snapshot.available_at(),
        }
    }

    pub fn leased_by(&self) -> Option<&str> {
        match self {
            Self::Execution(snapshot) => snapshot.leased_by(),
            Self::Outbox(snapshot) => snapshot.leased_by(),
        }
    }

    pub fn lease_expires_at(&self) -> Option<SystemTime> {
        match self {
            Self::Execution(snapshot) => snapshot.lease_expires_at(),
            Self::Outbox(snapshot) => snapshot.lease_expires_at(),
        }
    }

    pub fn last_error(&self) -> Option<&str> {
        match self {
            Self::Execution(snapshot) => snapshot.last_error(),
            Self::Outbox(snapshot) => snapshot.last_error(),
        }
    }

    pub fn active_lease(&self) -> bool {
        match self {
            Self::Execution(snapshot) => snapshot.active_lease(),
            Self::Outbox(snapshot) => snapshot.active_lease(),
        }
    }

    pub fn stale_lease(&self) -> bool {
        match self {
            Self::Execution(snapshot) => snapshot.stale_lease(),
            Self::Outbox(snapshot) => snapshot.stale_lease(),
        }
    }

    pub fn due(&self) -> bool {
        match self {
            Self::Execution(snapshot) => snapshot.due(),
            Self::Outbox(snapshot) => snapshot.due(),
        }
    }

    pub fn retry_scheduled(&self) -> bool {
        match self {
            Self::Execution(snapshot) => snapshot.retry_scheduled(),
            Self::Outbox(snapshot) => snapshot.retry_scheduled(),
        }
    }

    pub fn execution(&self) -> Option<&crate::ExecutionInspectionSnapshot> {
        match self {
            Self::Execution(snapshot) => Some(snapshot),
            Self::Outbox(_) => None,
        }
    }

    pub fn outbox(&self) -> Option<&crate::OutboxInspectionSnapshot> {
        match self {
            Self::Execution(_) => None,
            Self::Outbox(snapshot) => Some(snapshot),
        }
    }
}
