use std::time::SystemTime;

use crate::{AppResult, CorrelationId, ExecutionContext, ExecutionId, IdempotencyKey, LeaseId};

use super::{ExecutionLease, ExecutionRecord, ExecutionRecordStatus};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionInspectionQuery {
    observed_at: SystemTime,
    limit: usize,
    status: Option<ExecutionRecordStatus>,
    operation: Option<String>,
    target: Option<String>,
}

impl ExecutionInspectionQuery {
    pub fn new(limit: usize) -> Self {
        Self {
            observed_at: SystemTime::now(),
            limit: limit.max(1),
            status: None,
            operation: None,
            target: None,
        }
    }

    pub fn observed_at(&self) -> SystemTime {
        self.observed_at
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    pub fn status(&self) -> Option<ExecutionRecordStatus> {
        self.status
    }

    pub fn operation(&self) -> Option<&str> {
        self.operation.as_deref()
    }

    pub fn target(&self) -> Option<&str> {
        self.target.as_deref()
    }

    pub fn at(mut self, observed_at: SystemTime) -> Self {
        self.observed_at = observed_at;
        self
    }

    pub fn with_status(mut self, status: ExecutionRecordStatus) -> Self {
        self.status = Some(status);
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

    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit.max(1);
        self
    }

    pub fn matches_record(&self, record: &ExecutionRecord) -> bool {
        self.status
            .map(|status| record.status() == status)
            .unwrap_or(true)
            && self
                .operation
                .as_deref()
                .map(|operation| record.operation() == operation)
                .unwrap_or(true)
            && self
                .target
                .as_deref()
                .map(|target| record.target() == target)
                .unwrap_or(true)
    }

    pub fn matches_inspection(&self, record: &ExecutionInspectionRecord) -> bool {
        self.matches_record(record.record())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionInspectionRecord {
    record: ExecutionRecord,
    lease: Option<ExecutionLease>,
}

impl ExecutionInspectionRecord {
    pub fn new(record: ExecutionRecord) -> Self {
        Self {
            record,
            lease: None,
        }
    }

    pub fn execution_id(&self) -> &ExecutionId {
        self.record.id()
    }

    pub fn operation(&self) -> &str {
        self.record.operation()
    }

    pub fn target(&self) -> &str {
        self.record.target()
    }

    pub fn correlation_id(&self) -> &CorrelationId {
        self.record.correlation_id()
    }

    pub fn idempotency_key(&self) -> &IdempotencyKey {
        self.record.idempotency_key()
    }

    pub fn status(&self) -> ExecutionRecordStatus {
        self.record.status()
    }

    pub fn attempt(&self) -> u32 {
        self.record.attempt()
    }

    pub fn recorded_at(&self) -> SystemTime {
        self.record.recorded_at()
    }

    pub fn available_at(&self) -> SystemTime {
        self.record.available_at()
    }

    pub fn last_attempted_at(&self) -> Option<SystemTime> {
        self.record.last_attempted_at()
    }

    pub fn finished_at(&self) -> Option<SystemTime> {
        self.record.finished_at()
    }

    pub fn last_error(&self) -> Option<&str> {
        self.record.last_error()
    }

    pub fn record(&self) -> &ExecutionRecord {
        &self.record
    }

    pub fn lease(&self) -> Option<&ExecutionLease> {
        self.lease.as_ref()
    }

    pub fn with_lease(mut self, lease: Option<ExecutionLease>) -> Self {
        self.lease = lease;
        self
    }

    pub fn lease_id(&self) -> Option<&LeaseId> {
        self.lease.as_ref().map(|lease| lease.lease_id())
    }

    pub fn leased_by(&self) -> Option<&str> {
        self.lease.as_ref().map(|lease| lease.leased_by())
    }

    pub fn lease_expires_at(&self) -> Option<SystemTime> {
        self.lease.as_ref().map(|lease| lease.lease_expires_at())
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status(),
            ExecutionRecordStatus::Succeeded | ExecutionRecordStatus::DeadLettered
        )
    }

    pub fn has_active_lease_at(&self, observed_at: SystemTime) -> bool {
        self.lease
            .as_ref()
            .is_some_and(|lease| !lease.is_expired_at(observed_at))
    }

    pub fn has_stale_lease_at(&self, observed_at: SystemTime) -> bool {
        self.lease
            .as_ref()
            .is_some_and(|lease| lease.is_expired_at(observed_at))
    }

    pub fn is_due_at(&self, observed_at: SystemTime) -> bool {
        self.record.available_at() <= observed_at
            && matches!(
                self.record.status(),
                ExecutionRecordStatus::Pending
                    | ExecutionRecordStatus::Failed
                    | ExecutionRecordStatus::InFlight
            )
            && !self.has_active_lease_at(observed_at)
    }

    pub fn is_retry_scheduled_at(&self, observed_at: SystemTime) -> bool {
        self.record.status() == ExecutionRecordStatus::Pending
            && self.record.attempt() > 1
            && self.record.available_at() > observed_at
    }

    pub fn snapshot_at(&self, observed_at: SystemTime) -> ExecutionInspectionSnapshot {
        ExecutionInspectionSnapshot {
            execution_id: self.record.id().clone(),
            operation: self.record.operation().to_string(),
            target: self.record.target().to_string(),
            correlation_id: self.record.correlation_id().clone(),
            idempotency_key: self.record.idempotency_key().clone(),
            status: self.record.status(),
            attempt: self.record.attempt(),
            recorded_at: self.record.recorded_at(),
            available_at: self.record.available_at(),
            last_attempted_at: self.record.last_attempted_at(),
            finished_at: self.record.finished_at(),
            last_error: self.record.last_error().map(str::to_string),
            lease_id: self.lease.as_ref().map(|lease| lease.lease_id().clone()),
            leased_by: self
                .lease
                .as_ref()
                .map(|lease| lease.leased_by().to_string()),
            lease_expires_at: self.lease.as_ref().map(|lease| lease.lease_expires_at()),
            active_lease: self.has_active_lease_at(observed_at),
            stale_lease: self.has_stale_lease_at(observed_at),
            due: self.is_due_at(observed_at),
            retry_scheduled: self.is_retry_scheduled_at(observed_at),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionInspectionSnapshot {
    execution_id: ExecutionId,
    operation: String,
    target: String,
    correlation_id: CorrelationId,
    idempotency_key: IdempotencyKey,
    status: ExecutionRecordStatus,
    attempt: u32,
    recorded_at: SystemTime,
    available_at: SystemTime,
    last_attempted_at: Option<SystemTime>,
    finished_at: Option<SystemTime>,
    last_error: Option<String>,
    lease_id: Option<LeaseId>,
    leased_by: Option<String>,
    lease_expires_at: Option<SystemTime>,
    active_lease: bool,
    stale_lease: bool,
    due: bool,
    retry_scheduled: bool,
}

impl ExecutionInspectionSnapshot {
    pub fn execution_id(&self) -> &ExecutionId {
        &self.execution_id
    }

    pub fn operation(&self) -> &str {
        &self.operation
    }

    pub fn target(&self) -> &str {
        &self.target
    }

    pub fn correlation_id(&self) -> &CorrelationId {
        &self.correlation_id
    }

    pub fn idempotency_key(&self) -> &IdempotencyKey {
        &self.idempotency_key
    }

    pub fn status(&self) -> ExecutionRecordStatus {
        self.status
    }

    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    pub fn recorded_at(&self) -> SystemTime {
        self.recorded_at
    }

    pub fn available_at(&self) -> SystemTime {
        self.available_at
    }

    pub fn last_attempted_at(&self) -> Option<SystemTime> {
        self.last_attempted_at
    }

    pub fn finished_at(&self) -> Option<SystemTime> {
        self.finished_at
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn lease_id(&self) -> Option<&LeaseId> {
        self.lease_id.as_ref()
    }

    pub fn leased_by(&self) -> Option<&str> {
        self.leased_by.as_deref()
    }

    pub fn lease_expires_at(&self) -> Option<SystemTime> {
        self.lease_expires_at
    }

    pub fn active_lease(&self) -> bool {
        self.active_lease
    }

    pub fn stale_lease(&self) -> bool {
        self.stale_lease
    }

    pub fn due(&self) -> bool {
        self.due
    }

    pub fn retry_scheduled(&self) -> bool {
        self.retry_scheduled
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status,
            ExecutionRecordStatus::Succeeded | ExecutionRecordStatus::DeadLettered
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExecutionInspectionSummary {
    total: usize,
    pending: usize,
    in_flight: usize,
    succeeded: usize,
    failed: usize,
    dead_lettered: usize,
    due: usize,
    leased: usize,
    retry_scheduled: usize,
    stale_leases: usize,
    oldest_due_at: Option<SystemTime>,
}

impl ExecutionInspectionSummary {
    pub fn total(&self) -> usize {
        self.total
    }

    pub fn pending(&self) -> usize {
        self.pending
    }

    pub fn in_flight(&self) -> usize {
        self.in_flight
    }

    pub fn succeeded(&self) -> usize {
        self.succeeded
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

    pub fn observe(
        self,
        record: &ExecutionRecord,
        lease: Option<&ExecutionLease>,
        observed_at: SystemTime,
    ) -> Self {
        self.observe_record(
            &ExecutionInspectionRecord::new(record.clone()).with_lease(lease.cloned()),
            observed_at,
        )
    }

    pub fn observe_record(
        mut self,
        record: &ExecutionInspectionRecord,
        observed_at: SystemTime,
    ) -> Self {
        self.total = self.total.saturating_add(1);
        match record.record().status() {
            ExecutionRecordStatus::Pending => self.pending = self.pending.saturating_add(1),
            ExecutionRecordStatus::InFlight => self.in_flight = self.in_flight.saturating_add(1),
            ExecutionRecordStatus::Succeeded => self.succeeded = self.succeeded.saturating_add(1),
            ExecutionRecordStatus::Failed => self.failed = self.failed.saturating_add(1),
            ExecutionRecordStatus::DeadLettered => {
                self.dead_lettered = self.dead_lettered.saturating_add(1)
            }
        }

        if record.has_active_lease_at(observed_at) {
            self.leased = self.leased.saturating_add(1);
        }

        if record.has_stale_lease_at(observed_at) {
            self.stale_leases = self.stale_leases.saturating_add(1);
        }

        if record.is_retry_scheduled_at(observed_at) {
            self.retry_scheduled = self.retry_scheduled.saturating_add(1);
        }

        if record.is_due_at(observed_at) {
            self.due = self.due.saturating_add(1);
            self.oldest_due_at = Some(
                self.oldest_due_at
                    .map(|oldest| oldest.min(record.record().available_at()))
                    .unwrap_or(record.record().available_at()),
            );
        }

        self
    }
}

pub trait ExecutionInspectionStore: Send + Sync {
    fn lookup_execution(
        &self,
        execution_id: &ExecutionId,
        ctx: &ExecutionContext,
    ) -> AppResult<Option<ExecutionRecord>>;

    fn lookup_execution_inspection(
        &self,
        execution_id: &ExecutionId,
        observed_at: SystemTime,
        ctx: &ExecutionContext,
    ) -> AppResult<Option<ExecutionInspectionRecord>>;

    fn list_executions(
        &self,
        query: ExecutionInspectionQuery,
        ctx: &ExecutionContext,
    ) -> AppResult<Vec<ExecutionRecord>>;

    fn list_execution_inspection(
        &self,
        query: ExecutionInspectionQuery,
        ctx: &ExecutionContext,
    ) -> AppResult<Vec<ExecutionInspectionRecord>>;

    fn summarize_executions(
        &self,
        query: ExecutionInspectionQuery,
        ctx: &ExecutionContext,
    ) -> AppResult<ExecutionInspectionSummary>;
}
