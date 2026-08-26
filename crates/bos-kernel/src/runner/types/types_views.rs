use std::time::SystemTime;

use crate::{DispatchQueue, LeaseId, WorkerVisibility};

use super::types_core::{
    ReferenceQueueInspectionRecord, ReferenceQueueInspectionSnapshot,
    ReferenceQueueInspectionSummary, ReferenceWorkClaim, ReferenceWorkerSnapshot,
};

pub struct ReferenceActiveClaimSnapshot {
    claim: ReferenceWorkClaim,
    snapshot: ReferenceQueueInspectionSnapshot,
}

impl ReferenceActiveClaimSnapshot {
    pub(crate) fn new(
        claim: ReferenceWorkClaim,
        snapshot: ReferenceQueueInspectionSnapshot,
    ) -> Self {
        Self { claim, snapshot }
    }

    pub fn claim(&self) -> &ReferenceWorkClaim {
        &self.claim
    }

    pub fn snapshot(&self) -> &ReferenceQueueInspectionSnapshot {
        &self.snapshot
    }

    pub fn queue(&self) -> DispatchQueue {
        self.claim.queue()
    }

    pub fn worker_id(&self) -> &str {
        self.claim.worker_id()
    }

    pub fn lease_id(&self) -> &LeaseId {
        self.claim.lease_id()
    }

    pub fn leased_at(&self) -> SystemTime {
        self.claim.leased_at()
    }

    pub fn lease_expires_at(&self) -> SystemTime {
        self.claim.lease_expires_at()
    }

    pub fn id(&self) -> &str {
        self.snapshot.id()
    }

    pub fn available_at(&self) -> SystemTime {
        self.snapshot.available_at()
    }

    pub fn leased_by(&self) -> Option<&str> {
        self.snapshot.leased_by()
    }

    pub fn last_error(&self) -> Option<&str> {
        self.snapshot.last_error()
    }

    pub fn active_lease(&self) -> bool {
        self.snapshot.active_lease()
    }

    pub fn stale_lease(&self) -> bool {
        self.snapshot.stale_lease()
    }

    pub fn due(&self) -> bool {
        self.snapshot.due()
    }

    pub fn retry_scheduled(&self) -> bool {
        self.snapshot.retry_scheduled()
    }

    pub fn execution(&self) -> Option<&crate::ExecutionInspectionSnapshot> {
        self.snapshot.execution()
    }

    pub fn outbox(&self) -> Option<&crate::OutboxInspectionSnapshot> {
        self.snapshot.outbox()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceActiveClaimSummary {
    queue: DispatchQueue,
    observed_at: SystemTime,
    total: usize,
    workers: usize,
    active_leases: usize,
    stale_leases: usize,
    due_work: usize,
    retry_scheduled: usize,
    oldest_leased_at: Option<SystemTime>,
    earliest_lease_expiry: Option<SystemTime>,
}

impl ReferenceActiveClaimSummary {
    pub(crate) fn new(
        queue: DispatchQueue,
        observed_at: SystemTime,
        claims: &[ReferenceActiveClaimSnapshot],
    ) -> Self {
        let mut workers = std::collections::BTreeSet::new();
        for claim in claims {
            workers.insert(claim.worker_id().to_string());
        }

        Self {
            queue,
            observed_at,
            total: claims.len(),
            workers: workers.len(),
            active_leases: claims.iter().filter(|claim| claim.active_lease()).count(),
            stale_leases: claims.iter().filter(|claim| claim.stale_lease()).count(),
            due_work: claims.iter().filter(|claim| claim.due()).count(),
            retry_scheduled: claims
                .iter()
                .filter(|claim| claim.retry_scheduled())
                .count(),
            oldest_leased_at: claims.iter().map(|claim| claim.leased_at()).min(),
            earliest_lease_expiry: claims.iter().map(|claim| claim.lease_expires_at()).min(),
        }
    }

    pub fn queue(&self) -> DispatchQueue {
        self.queue
    }

    pub fn observed_at(&self) -> SystemTime {
        self.observed_at
    }

    pub fn total(&self) -> usize {
        self.total
    }

    pub fn workers(&self) -> usize {
        self.workers
    }

    pub fn active_leases(&self) -> usize {
        self.active_leases
    }

    pub fn stale_leases(&self) -> usize {
        self.stale_leases
    }

    pub fn due_work(&self) -> usize {
        self.due_work
    }

    pub fn retry_scheduled(&self) -> usize {
        self.retry_scheduled
    }

    pub fn oldest_leased_at(&self) -> Option<SystemTime> {
        self.oldest_leased_at
    }

    pub fn earliest_lease_expiry(&self) -> Option<SystemTime> {
        self.earliest_lease_expiry
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceSchedulerStateSummary {
    queue: DispatchQueue,
    scope: String,
    observed_at: SystemTime,
    workers: usize,
    visible_workers: usize,
    expired_workers: usize,
    workers_with_claims: usize,
    active_claims: usize,
    stale_active_claims: usize,
    backlog: ReferenceQueueInspectionSummary,
}

impl ReferenceSchedulerStateSummary {
    pub(crate) fn new(
        queue: DispatchQueue,
        scope: impl Into<String>,
        observed_at: SystemTime,
        workers: &[ReferenceWorkerSnapshot],
        active_claims: &[ReferenceWorkClaim],
        backlog: ReferenceQueueInspectionSummary,
    ) -> Self {
        Self {
            queue,
            scope: scope.into(),
            observed_at,
            workers: workers.len(),
            visible_workers: workers
                .iter()
                .filter(|worker| worker.visibility() != crate::WorkerVisibility::Expired)
                .count(),
            expired_workers: workers
                .iter()
                .filter(|worker| worker.visibility() == crate::WorkerVisibility::Expired)
                .count(),
            workers_with_claims: workers
                .iter()
                .filter(|worker| worker.active_claim_count() > 0)
                .count(),
            active_claims: active_claims.len(),
            stale_active_claims: active_claims
                .iter()
                .filter(|claim| claim.lease_expires_at() < observed_at)
                .count(),
            backlog,
        }
    }

    pub fn queue(&self) -> DispatchQueue {
        self.queue
    }

    pub fn scope(&self) -> &str {
        &self.scope
    }

    pub fn observed_at(&self) -> SystemTime {
        self.observed_at
    }

    pub fn workers(&self) -> usize {
        self.workers
    }

    pub fn visible_workers(&self) -> usize {
        self.visible_workers
    }

    pub fn expired_workers(&self) -> usize {
        self.expired_workers
    }

    pub fn workers_with_claims(&self) -> usize {
        self.workers_with_claims
    }

    pub fn active_claims(&self) -> usize {
        self.active_claims
    }

    pub fn stale_active_claims(&self) -> usize {
        self.stale_active_claims
    }

    pub fn backlog(&self) -> &ReferenceQueueInspectionSummary {
        &self.backlog
    }

    pub fn queue_summary(&self) -> ReferenceQueueSummary {
        self.backlog.operator_summary(self.queue, self.observed_at)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceSchedulerState {
    summary: ReferenceSchedulerStateSummary,
    workers: Vec<ReferenceWorkerSnapshot>,
    active_claims: Vec<ReferenceWorkClaim>,
    backlog: Vec<ReferenceQueueInspectionRecord>,
}

impl ReferenceSchedulerState {
    pub(crate) fn new(
        summary: ReferenceSchedulerStateSummary,
        workers: Vec<ReferenceWorkerSnapshot>,
        active_claims: Vec<ReferenceWorkClaim>,
        backlog: Vec<ReferenceQueueInspectionRecord>,
    ) -> Self {
        Self {
            summary,
            workers,
            active_claims,
            backlog,
        }
    }

    pub fn summary(&self) -> &ReferenceSchedulerStateSummary {
        &self.summary
    }

    pub fn workers(&self) -> &[ReferenceWorkerSnapshot] {
        &self.workers
    }

    pub fn active_claims(&self) -> &[ReferenceWorkClaim] {
        &self.active_claims
    }

    pub fn backlog(&self) -> &[ReferenceQueueInspectionRecord] {
        &self.backlog
    }

    pub fn worker(&self, worker_id: &str) -> Option<&ReferenceWorkerSnapshot> {
        self.workers
            .iter()
            .find(|worker| worker.worker_id() == worker_id)
    }

    pub fn backlog_record(&self, id: &str) -> Option<&ReferenceQueueInspectionRecord> {
        self.backlog.iter().find(|record| record.id() == id)
    }

    pub fn backlog_snapshot(&self, id: &str) -> Option<ReferenceQueueInspectionSnapshot> {
        self.backlog_record(id)
            .map(|record| record.snapshot_at(self.summary.observed_at()))
    }

    pub fn operator_summary(&self) -> ReferenceOperatorSummary {
        ReferenceOperatorSummary::new(&self.summary, &self.workers)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceWorkerQueueSummary {
    queue: DispatchQueue,
    due: usize,
    claimed: usize,
    stale_claims: usize,
    oldest_due_at: Option<SystemTime>,
}

impl ReferenceWorkerQueueSummary {
    fn from_snapshot(snapshot: &crate::DispatchQueueStatusSnapshot) -> Self {
        Self {
            queue: snapshot.queue(),
            due: snapshot.due_count(),
            claimed: snapshot.claimed_count(),
            stale_claims: snapshot.stale_claim_count(),
            oldest_due_at: snapshot.oldest_due_at(),
        }
    }

    pub fn queue(&self) -> DispatchQueue {
        self.queue
    }

    pub fn due(&self) -> usize {
        self.due
    }

    pub fn claimed(&self) -> usize {
        self.claimed
    }

    pub fn stale_claims(&self) -> usize {
        self.stale_claims
    }

    pub fn oldest_due_at(&self) -> Option<SystemTime> {
        self.oldest_due_at
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceWorkerSummary {
    worker_id: String,
    scope: Option<String>,
    observed_at: Option<SystemTime>,
    visibility: WorkerVisibility,
    heartbeat_recorded_at: Option<SystemTime>,
    visible_until: Option<SystemTime>,
    active_leases: usize,
    active_claims: usize,
    claimed_work: usize,
    stale_claims: usize,
    due_work: usize,
    queues: Vec<ReferenceWorkerQueueSummary>,
}

impl ReferenceWorkerSummary {
    pub(crate) fn from_snapshot(snapshot: &ReferenceWorkerSnapshot) -> Self {
        let mut queues = snapshot
            .snapshot()
            .map(|worker| {
                let mut queues = worker
                    .queues()
                    .iter()
                    .map(ReferenceWorkerQueueSummary::from_snapshot)
                    .collect::<Vec<_>>();
                queues.sort_by_key(|queue| queue.queue());
                queues
            })
            .unwrap_or_default();

        if queues.is_empty() {
            queues.shrink_to_fit();
        }

        Self {
            worker_id: snapshot.worker_id().to_string(),
            scope: snapshot.snapshot().map(|worker| worker.scope().to_string()),
            observed_at: snapshot.snapshot().map(|worker| worker.observed_at()),
            visibility: snapshot.visibility(),
            heartbeat_recorded_at: snapshot
                .snapshot()
                .and_then(|worker| worker.heartbeat().map(|heartbeat| heartbeat.recorded_at())),
            visible_until: snapshot.snapshot().and_then(|worker| {
                worker
                    .heartbeat()
                    .map(|heartbeat| heartbeat.visible_until())
            }),
            active_leases: snapshot
                .snapshot()
                .map(|worker| worker.active_leases())
                .unwrap_or(snapshot.active_claim_count()),
            active_claims: snapshot.active_claim_count(),
            claimed_work: snapshot.claimed_work(),
            stale_claims: snapshot.stale_claims(),
            due_work: snapshot.due_work(),
            queues,
        }
    }

    pub fn worker_id(&self) -> &str {
        &self.worker_id
    }

    pub fn scope(&self) -> Option<&str> {
        self.scope.as_deref()
    }

    pub fn observed_at(&self) -> Option<SystemTime> {
        self.observed_at
    }

    pub fn visibility(&self) -> WorkerVisibility {
        self.visibility
    }

    pub fn heartbeat_recorded_at(&self) -> Option<SystemTime> {
        self.heartbeat_recorded_at
    }

    pub fn visible_until(&self) -> Option<SystemTime> {
        self.visible_until
    }

    pub fn active_leases(&self) -> usize {
        self.active_leases
    }

    pub fn active_claims(&self) -> usize {
        self.active_claims
    }

    pub fn claimed_work(&self) -> usize {
        self.claimed_work
    }

    pub fn stale_claims(&self) -> usize {
        self.stale_claims
    }

    pub fn due_work(&self) -> usize {
        self.due_work
    }

    pub fn queues(&self) -> &[ReferenceWorkerQueueSummary] {
        &self.queues
    }

    pub fn queue(&self, queue: DispatchQueue) -> Option<&ReferenceWorkerQueueSummary> {
        self.queues
            .iter()
            .find(|candidate| candidate.queue() == queue)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceQueueSummary {
    queue: DispatchQueue,
    observed_at: SystemTime,
    total: usize,
    pending: usize,
    in_flight: usize,
    completed: usize,
    failed: usize,
    dead_lettered: usize,
    due: usize,
    leased: usize,
    retry_scheduled: usize,
    stale_leases: usize,
    oldest_due_at: Option<SystemTime>,
}

impl ReferenceQueueSummary {
    pub(crate) fn new(
        queue: DispatchQueue,
        observed_at: SystemTime,
        summary: &ReferenceQueueInspectionSummary,
    ) -> Self {
        Self {
            queue,
            observed_at,
            total: summary.total(),
            pending: summary.pending(),
            in_flight: summary.in_flight(),
            completed: summary.completed(),
            failed: summary.failed(),
            dead_lettered: summary.dead_lettered(),
            due: summary.due(),
            leased: summary.leased(),
            retry_scheduled: summary.retry_scheduled(),
            stale_leases: summary.stale_leases(),
            oldest_due_at: summary.oldest_due_at(),
        }
    }

    pub fn queue(&self) -> DispatchQueue {
        self.queue
    }

    pub fn observed_at(&self) -> SystemTime {
        self.observed_at
    }

    pub fn total(&self) -> usize {
        self.total
    }

    pub fn pending(&self) -> usize {
        self.pending
    }

    pub fn in_flight(&self) -> usize {
        self.in_flight
    }

    pub fn completed(&self) -> usize {
        self.completed
    }

    pub fn failed(&self) -> usize {
        self.failed
    }

    pub fn dead_lettered(&self) -> usize {
        self.dead_lettered
    }

    pub fn due(&self) -> usize {
        self.due
    }

    pub fn leased(&self) -> usize {
        self.leased
    }

    pub fn retry_scheduled(&self) -> usize {
        self.retry_scheduled
    }

    pub fn stale_leases(&self) -> usize {
        self.stale_leases
    }

    pub fn oldest_due_at(&self) -> Option<SystemTime> {
        self.oldest_due_at
    }

    pub fn unfinished(&self) -> usize {
        self.total
            .saturating_sub(self.completed)
            .saturating_sub(self.dead_lettered)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceOperatorSummary {
    scheduler: ReferenceSchedulerStateSummary,
    queue: ReferenceQueueSummary,
    workers: Vec<ReferenceWorkerSummary>,
}

impl ReferenceOperatorSummary {
    pub(crate) fn new(
        scheduler: &ReferenceSchedulerStateSummary,
        workers: &[ReferenceWorkerSnapshot],
    ) -> Self {
        Self {
            scheduler: scheduler.clone(),
            queue: scheduler.queue_summary(),
            workers: workers
                .iter()
                .map(ReferenceWorkerSnapshot::operator_summary)
                .collect(),
        }
    }

    pub fn scheduler(&self) -> &ReferenceSchedulerStateSummary {
        &self.scheduler
    }

    pub fn queue(&self) -> &ReferenceQueueSummary {
        &self.queue
    }

    pub fn workers(&self) -> &[ReferenceWorkerSummary] {
        &self.workers
    }

    pub fn worker(&self, worker_id: &str) -> Option<&ReferenceWorkerSummary> {
        self.workers
            .iter()
            .find(|worker| worker.worker_id() == worker_id)
    }
}
