use std::time::{Duration, SystemTime};

use crate::{
    AppError, AppResult, CorrelationId, ErrorCode, ExecutionContext, ExecutionId, IdempotencyKey,
    LeaseId, RevisionToken,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionRecordStatus {
    Pending,
    InFlight,
    Succeeded,
    Failed,
    DeadLettered,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdempotencyClaimStatus {
    InProgress,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdempotencyClaim {
    key: IdempotencyKey,
    scope: String,
    status: IdempotencyClaimStatus,
    attempt: u32,
    duplicate_count: u32,
    execution_id: Option<ExecutionId>,
    correlation_id: Option<CorrelationId>,
    created_at: SystemTime,
    first_claimed_at: SystemTime,
    last_claimed_at: SystemTime,
    completed_at: Option<SystemTime>,
    last_error: Option<String>,
}

impl IdempotencyClaim {
    pub fn new(key: IdempotencyKey, scope: impl Into<String>) -> Self {
        let created_at = SystemTime::now();
        Self {
            key,
            scope: scope.into(),
            status: IdempotencyClaimStatus::InProgress,
            attempt: 1,
            duplicate_count: 0,
            execution_id: None,
            correlation_id: None,
            created_at,
            first_claimed_at: created_at,
            last_claimed_at: created_at,
            completed_at: None,
            last_error: None,
        }
    }

    pub fn from_context(
        key: IdempotencyKey,
        scope: impl Into<String>,
        execution_id: ExecutionId,
        ctx: &ExecutionContext,
    ) -> AppResult<Self> {
        let mut claim = Self::new(key, scope);
        claim = claim
            .attach_execution(execution_id)
            .with_correlation_id(ctx.correlation_id.clone())
            .with_attempt(ctx.attempt)
            .touch(ctx.started_at);
        validate_scope(claim.scope(), ctx)?;
        Ok(claim)
    }

    pub fn key(&self) -> &IdempotencyKey {
        &self.key
    }

    pub fn scope(&self) -> &str {
        &self.scope
    }

    pub fn status(&self) -> IdempotencyClaimStatus {
        self.status
    }

    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    pub fn duplicate_count(&self) -> u32 {
        self.duplicate_count
    }

    pub fn execution_id(&self) -> Option<&ExecutionId> {
        self.execution_id.as_ref()
    }

    pub fn correlation_id(&self) -> Option<&CorrelationId> {
        self.correlation_id.as_ref()
    }

    pub fn created_at(&self) -> SystemTime {
        self.created_at
    }

    pub fn first_claimed_at(&self) -> SystemTime {
        self.first_claimed_at
    }

    pub fn last_claimed_at(&self) -> SystemTime {
        self.last_claimed_at
    }

    pub fn completed_at(&self) -> Option<SystemTime> {
        self.completed_at
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn restore(
        key: IdempotencyKey,
        scope: impl Into<String>,
        status: IdempotencyClaimStatus,
        attempt: u32,
        duplicate_count: u32,
        execution_id: Option<ExecutionId>,
        correlation_id: Option<CorrelationId>,
        created_at: SystemTime,
        first_claimed_at: SystemTime,
        last_claimed_at: SystemTime,
        completed_at: Option<SystemTime>,
        last_error: Option<String>,
    ) -> Self {
        Self {
            key,
            scope: scope.into(),
            status,
            attempt: attempt.max(1),
            duplicate_count,
            execution_id,
            correlation_id,
            created_at,
            first_claimed_at,
            last_claimed_at,
            completed_at,
            last_error,
        }
    }

    pub fn attach_execution(mut self, execution_id: ExecutionId) -> Self {
        self.execution_id = Some(execution_id);
        self
    }

    pub fn with_correlation_id(mut self, correlation_id: CorrelationId) -> Self {
        self.correlation_id = Some(correlation_id);
        self
    }

    pub fn with_attempt(mut self, attempt: u32) -> Self {
        self.attempt = attempt.max(1);
        self
    }

    pub fn touch(mut self, claimed_at: SystemTime) -> Self {
        self.last_claimed_at = claimed_at;
        self
    }

    pub fn record_duplicate(mut self, duplicate_at: SystemTime) -> Self {
        self.duplicate_count = self.duplicate_count.saturating_add(1);
        self.last_claimed_at = duplicate_at;
        self
    }

    pub fn mark_succeeded(mut self, completed_at: SystemTime) -> Self {
        self.status = IdempotencyClaimStatus::Succeeded;
        self.completed_at = Some(completed_at);
        self.last_claimed_at = completed_at;
        self.last_error = None;
        self
    }

    pub fn mark_failed(mut self, failed_at: SystemTime, error: impl Into<String>) -> Self {
        self.status = IdempotencyClaimStatus::Failed;
        self.completed_at = Some(failed_at);
        self.last_claimed_at = failed_at;
        self.last_error = Some(error.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionRecord {
    id: ExecutionId,
    operation: String,
    target: String,
    idempotency_key: IdempotencyKey,
    correlation_id: CorrelationId,
    status: ExecutionRecordStatus,
    attempt: u32,
    recorded_at: SystemTime,
    available_at: SystemTime,
    last_attempted_at: Option<SystemTime>,
    finished_at: Option<SystemTime>,
    last_error: Option<String>,
    revision: RevisionToken,
}

impl ExecutionRecord {
    pub fn new(
        id: ExecutionId,
        operation: impl Into<String>,
        target: impl Into<String>,
        idempotency_key: IdempotencyKey,
        correlation_id: CorrelationId,
    ) -> Self {
        Self {
            id,
            operation: operation.into(),
            target: target.into(),
            idempotency_key,
            correlation_id,
            status: ExecutionRecordStatus::Pending,
            attempt: 1,
            recorded_at: SystemTime::now(),
            available_at: SystemTime::now(),
            last_attempted_at: None,
            finished_at: None,
            last_error: None,
            revision: RevisionToken::initial(),
        }
    }

    pub fn from_context(
        id: ExecutionId,
        operation: impl Into<String>,
        target: impl Into<String>,
        ctx: &ExecutionContext,
    ) -> AppResult<Self> {
        let idempotency_key = ctx.idempotency_key().cloned().ok_or_else(|| {
            AppError::from_context(
                ErrorCode::InvalidState,
                "execution_context_missing_idempotency_key",
                "execution records require an execution context with an idempotency key",
                ctx,
            )
        })?;

        Ok(Self::new(
            id,
            operation,
            target,
            idempotency_key,
            ctx.correlation_id.clone(),
        )
        .with_attempt(ctx.attempt)
        .with_recorded_at(ctx.started_at))
    }

    pub fn id(&self) -> &ExecutionId {
        &self.id
    }

    pub fn operation(&self) -> &str {
        &self.operation
    }

    pub fn target(&self) -> &str {
        &self.target
    }

    pub fn idempotency_key(&self) -> &IdempotencyKey {
        &self.idempotency_key
    }

    pub fn correlation_id(&self) -> &CorrelationId {
        &self.correlation_id
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

    pub fn revision(&self) -> RevisionToken {
        self.revision
    }

    pub fn with_status(mut self, status: ExecutionRecordStatus) -> Self {
        self.status = status;
        self
    }

    pub fn with_attempt(mut self, attempt: u32) -> Self {
        self.attempt = attempt.max(1);
        self
    }

    pub fn with_recorded_at(mut self, recorded_at: SystemTime) -> Self {
        self.recorded_at = recorded_at;
        self.available_at = recorded_at;
        self
    }

    pub fn with_available_at(mut self, available_at: SystemTime) -> Self {
        self.available_at = available_at;
        self
    }

    pub fn with_revision(mut self, revision: RevisionToken) -> Self {
        self.revision = revision;
        self
    }

    pub fn mark_in_flight(mut self) -> Self {
        self.last_attempted_at = Some(SystemTime::now());
        self.status = ExecutionRecordStatus::InFlight;
        self.finished_at = None;
        self.last_error = None;
        self.revision = self.revision.next();
        self
    }

    pub fn mark_claimed(mut self, claimed_at: SystemTime) -> Self {
        self.status = ExecutionRecordStatus::InFlight;
        self.last_attempted_at = Some(claimed_at);
        self.finished_at = None;
        self.last_error = None;
        self.revision = self.revision.next();
        self
    }

    pub fn mark_succeeded(mut self, finished_at: SystemTime) -> Self {
        self.status = ExecutionRecordStatus::Succeeded;
        self.last_attempted_at = Some(finished_at);
        self.finished_at = Some(finished_at);
        self.last_error = None;
        self.revision = self.revision.next();
        self
    }

    pub fn mark_failed(mut self, finished_at: SystemTime, error: impl Into<String>) -> Self {
        self.status = ExecutionRecordStatus::Failed;
        self.last_attempted_at = Some(finished_at);
        self.finished_at = Some(finished_at);
        self.last_error = Some(error.into());
        self.revision = self.revision.next();
        self
    }

    pub fn schedule_retry(
        mut self,
        attempted_at: SystemTime,
        next_available_at: SystemTime,
        error: impl Into<String>,
    ) -> Self {
        self.status = ExecutionRecordStatus::Pending;
        self.attempt = self.attempt.saturating_add(1);
        self.available_at = next_available_at;
        self.last_attempted_at = Some(attempted_at);
        self.finished_at = None;
        self.last_error = Some(error.into());
        self.revision = self.revision.next();
        self
    }

    pub fn mark_dead_lettered(
        mut self,
        dead_lettered_at: SystemTime,
        error: impl Into<String>,
    ) -> Self {
        self.status = ExecutionRecordStatus::DeadLettered;
        self.last_attempted_at = Some(dead_lettered_at);
        self.finished_at = Some(dead_lettered_at);
        self.last_error = Some(error.into());
        self.revision = self.revision.next();
        self
    }

    pub fn release_claim(mut self, released_at: SystemTime) -> Self {
        self.status = ExecutionRecordStatus::Pending;
        self.last_attempted_at = Some(released_at);
        self.finished_at = None;
        self.revision = self.revision.next();
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionLease {
    lease_id: LeaseId,
    leased_by: String,
    leased_at: SystemTime,
    lease_expires_at: SystemTime,
}

impl ExecutionLease {
    pub fn new(
        lease_id: LeaseId,
        leased_by: impl Into<String>,
        leased_at: SystemTime,
        lease_expires_at: SystemTime,
    ) -> Self {
        Self {
            lease_id,
            leased_by: leased_by.into(),
            leased_at,
            lease_expires_at,
        }
    }

    pub fn from_now(
        lease_id: LeaseId,
        leased_by: impl Into<String>,
        lease_ttl: Duration,
        now: SystemTime,
    ) -> Self {
        Self::new(
            lease_id,
            leased_by,
            now,
            now.checked_add(lease_ttl).unwrap_or(now),
        )
    }

    pub fn lease_id(&self) -> &LeaseId {
        &self.lease_id
    }

    pub fn leased_by(&self) -> &str {
        &self.leased_by
    }

    pub fn leased_at(&self) -> SystemTime {
        self.leased_at
    }

    pub fn lease_expires_at(&self) -> SystemTime {
        self.lease_expires_at
    }

    pub fn is_expired_at(&self, now: SystemTime) -> bool {
        self.lease_expires_at < now
    }

    pub fn renew(self, renewed_at: SystemTime, lease_ttl: Duration) -> Self {
        Self::from_now(self.lease_id, self.leased_by, lease_ttl, renewed_at)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionLeaseRequest {
    leased_by: String,
    batch_size: usize,
    lease_ttl: Duration,
    now: SystemTime,
}

impl ExecutionLeaseRequest {
    pub fn new(leased_by: impl Into<String>, batch_size: usize, lease_ttl: Duration) -> Self {
        Self {
            leased_by: leased_by.into(),
            batch_size: batch_size.max(1),
            lease_ttl,
            now: SystemTime::now(),
        }
    }

    pub fn leased_by(&self) -> &str {
        &self.leased_by
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DueExecutionRequest {
    due_before: SystemTime,
    limit: usize,
    include_expired_leases: bool,
}

impl DueExecutionRequest {
    pub fn new(limit: usize) -> Self {
        Self {
            due_before: SystemTime::now(),
            limit: limit.max(1),
            include_expired_leases: true,
        }
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionLeaseRenewal {
    execution_id: ExecutionId,
    lease_id: LeaseId,
    leased_by: String,
    renewed_at: SystemTime,
    lease_ttl: Duration,
}

impl ExecutionLeaseRenewal {
    pub fn new(
        execution_id: ExecutionId,
        lease_id: LeaseId,
        leased_by: impl Into<String>,
        lease_ttl: Duration,
    ) -> Self {
        Self {
            execution_id,
            lease_id,
            leased_by: leased_by.into(),
            renewed_at: SystemTime::now(),
            lease_ttl,
        }
    }

    pub fn execution_id(&self) -> &ExecutionId {
        &self.execution_id
    }

    pub fn lease_id(&self) -> &LeaseId {
        &self.lease_id
    }

    pub fn leased_by(&self) -> &str {
        &self.leased_by
    }

    pub fn renewed_at(&self) -> SystemTime {
        self.renewed_at
    }

    pub fn lease_ttl(&self) -> Duration {
        self.lease_ttl
    }

    pub fn at(mut self, renewed_at: SystemTime) -> Self {
        self.renewed_at = renewed_at;
        self
    }

    pub fn renewed_lease(&self) -> ExecutionLease {
        ExecutionLease::from_now(
            self.lease_id.clone(),
            self.leased_by.clone(),
            self.lease_ttl,
            self.renewed_at,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionSuccess {
    execution_id: ExecutionId,
    lease_id: LeaseId,
    finished_at: SystemTime,
    expected_revision: Option<RevisionToken>,
}

impl ExecutionSuccess {
    pub fn new(execution_id: ExecutionId, lease_id: LeaseId, finished_at: SystemTime) -> Self {
        Self {
            execution_id,
            lease_id,
            finished_at,
            expected_revision: None,
        }
    }

    pub fn execution_id(&self) -> &ExecutionId {
        &self.execution_id
    }

    pub fn lease_id(&self) -> &LeaseId {
        &self.lease_id
    }

    pub fn finished_at(&self) -> SystemTime {
        self.finished_at
    }

    pub fn expected_revision(&self) -> Option<RevisionToken> {
        self.expected_revision
    }

    pub fn with_expected_revision(mut self, expected_revision: RevisionToken) -> Self {
        self.expected_revision = Some(expected_revision);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionRetry {
    execution_id: ExecutionId,
    lease_id: LeaseId,
    attempted_at: SystemTime,
    next_available_at: SystemTime,
    error: String,
    expected_revision: Option<RevisionToken>,
}

impl ExecutionRetry {
    pub fn new(
        execution_id: ExecutionId,
        lease_id: LeaseId,
        attempted_at: SystemTime,
        next_available_at: SystemTime,
        error: impl Into<String>,
    ) -> Self {
        Self {
            execution_id,
            lease_id,
            attempted_at,
            next_available_at,
            error: error.into(),
            expected_revision: None,
        }
    }

    pub fn execution_id(&self) -> &ExecutionId {
        &self.execution_id
    }

    pub fn lease_id(&self) -> &LeaseId {
        &self.lease_id
    }

    pub fn attempted_at(&self) -> SystemTime {
        self.attempted_at
    }

    pub fn next_available_at(&self) -> SystemTime {
        self.next_available_at
    }

    pub fn error(&self) -> &str {
        &self.error
    }

    pub fn expected_revision(&self) -> Option<RevisionToken> {
        self.expected_revision
    }

    pub fn with_expected_revision(mut self, expected_revision: RevisionToken) -> Self {
        self.expected_revision = Some(expected_revision);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionDeadLetter {
    execution_id: ExecutionId,
    lease_id: LeaseId,
    dead_lettered_at: SystemTime,
    error: String,
    expected_revision: Option<RevisionToken>,
}

impl ExecutionDeadLetter {
    pub fn new(
        execution_id: ExecutionId,
        lease_id: LeaseId,
        dead_lettered_at: SystemTime,
        error: impl Into<String>,
    ) -> Self {
        Self {
            execution_id,
            lease_id,
            dead_lettered_at,
            error: error.into(),
            expected_revision: None,
        }
    }

    pub fn execution_id(&self) -> &ExecutionId {
        &self.execution_id
    }

    pub fn lease_id(&self) -> &LeaseId {
        &self.lease_id
    }

    pub fn dead_lettered_at(&self) -> SystemTime {
        self.dead_lettered_at
    }

    pub fn error(&self) -> &str {
        &self.error
    }

    pub fn expected_revision(&self) -> Option<RevisionToken> {
        self.expected_revision
    }

    pub fn with_expected_revision(mut self, expected_revision: RevisionToken) -> Self {
        self.expected_revision = Some(expected_revision);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedExecution {
    record: ExecutionRecord,
    lease: ExecutionLease,
}

impl ClaimedExecution {
    pub fn claim(record: ExecutionRecord, lease: ExecutionLease) -> AppResult<Self> {
        if lease.leased_by().trim().is_empty() {
            return Err(AppError::new(
                ErrorCode::InvalidInput,
                "execution_lease_owner_required",
                "execution leases require a non-empty worker identity",
                record.correlation_id().clone(),
            ));
        }

        if record.available_at() > lease.leased_at() {
            return Err(AppError::new(
                ErrorCode::InvalidState,
                "execution_not_available_for_claim",
                "execution work cannot be claimed before it is available",
                record.correlation_id().clone(),
            ));
        }

        match record.status() {
            ExecutionRecordStatus::Pending | ExecutionRecordStatus::Failed => Ok(Self {
                record: record.mark_claimed(lease.leased_at()),
                lease,
            }),
            ExecutionRecordStatus::InFlight => Err(AppError::new(
                ErrorCode::Conflict,
                "execution_already_claimed",
                "execution work is already in flight",
                record.correlation_id().clone(),
            )),
            ExecutionRecordStatus::Succeeded | ExecutionRecordStatus::DeadLettered => {
                Err(AppError::new(
                    ErrorCode::InvalidState,
                    "execution_not_claimable",
                    "completed execution work cannot be claimed again",
                    record.correlation_id().clone(),
                ))
            }
        }
    }

    pub fn record(&self) -> &ExecutionRecord {
        &self.record
    }

    pub fn lease(&self) -> &ExecutionLease {
        &self.lease
    }

    pub fn into_parts(self) -> (ExecutionRecord, ExecutionLease) {
        (self.record, self.lease)
    }

    pub fn replace_lease(mut self, lease: ExecutionLease) -> AppResult<Self> {
        if lease.leased_by().trim().is_empty() {
            return Err(AppError::new(
                ErrorCode::InvalidInput,
                "execution_lease_owner_required",
                "execution leases require a non-empty worker identity",
                self.record.correlation_id().clone(),
            ));
        }
        self.lease = lease;
        Ok(self)
    }

    pub fn complete(self, finished_at: SystemTime) -> (ExecutionRecord, ExecutionSuccess) {
        let success = ExecutionSuccess::new(
            self.record.id().clone(),
            self.lease.lease_id().clone(),
            finished_at,
        )
        .with_expected_revision(self.record.revision());
        let record = self.record.mark_succeeded(finished_at);
        (record, success)
    }

    pub fn retry(
        self,
        attempted_at: SystemTime,
        next_available_at: SystemTime,
        error: impl Into<String>,
    ) -> AppResult<(ExecutionRecord, ExecutionRetry)> {
        if next_available_at < attempted_at {
            return Err(AppError::new(
                ErrorCode::InvalidInput,
                "execution_retry_before_attempt",
                "execution retries require next availability at or after the attempt time",
                self.record.correlation_id().clone(),
            ));
        }

        let error = error.into();
        if error.trim().is_empty() {
            return Err(AppError::new(
                ErrorCode::InvalidInput,
                "execution_retry_error_required",
                "execution retries require a failure reason",
                self.record.correlation_id().clone(),
            ));
        }

        let retry = ExecutionRetry::new(
            self.record.id().clone(),
            self.lease.lease_id().clone(),
            attempted_at,
            next_available_at,
            error.clone(),
        )
        .with_expected_revision(self.record.revision());
        let record = self
            .record
            .schedule_retry(attempted_at, next_available_at, error);
        Ok((record, retry))
    }

    pub fn dead_letter(
        self,
        dead_lettered_at: SystemTime,
        error: impl Into<String>,
    ) -> AppResult<(ExecutionRecord, ExecutionDeadLetter)> {
        let error = error.into();
        if error.trim().is_empty() {
            return Err(AppError::new(
                ErrorCode::InvalidInput,
                "execution_dead_letter_error_required",
                "dead-lettering execution work requires a failure reason",
                self.record.correlation_id().clone(),
            ));
        }

        let dead_letter = ExecutionDeadLetter::new(
            self.record.id().clone(),
            self.lease.lease_id().clone(),
            dead_lettered_at,
            error.clone(),
        )
        .with_expected_revision(self.record.revision());
        let record = self.record.mark_dead_lettered(dead_lettered_at, error);
        Ok((record, dead_letter))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdempotencyClaimed {
    Claimed(IdempotencyClaim),
    Duplicate(IdempotencyClaim),
    Busy(IdempotencyClaim),
}

pub trait IdempotencyClaimStore: Send + Sync {
    fn claim(
        &self,
        claim: IdempotencyClaim,
        ctx: &ExecutionContext,
    ) -> AppResult<IdempotencyClaimed>;

    fn mark_succeeded(
        &self,
        key: &IdempotencyKey,
        completed_at: SystemTime,
        ctx: &ExecutionContext,
    ) -> AppResult<()>;

    fn mark_failed(
        &self,
        key: &IdempotencyKey,
        failed_at: SystemTime,
        error: &str,
        ctx: &ExecutionContext,
    ) -> AppResult<()>;
}

pub trait ExecutionJournal: Send + Sync {
    fn append(&self, record: ExecutionRecord, ctx: &ExecutionContext) -> AppResult<()>;

    fn update(&self, record: ExecutionRecord, ctx: &ExecutionContext) -> AppResult<()>;
}

pub trait LeasedExecutions: Send + Sync {
    fn select_due(
        &self,
        request: DueExecutionRequest,
        ctx: &ExecutionContext,
    ) -> AppResult<Vec<ExecutionRecord>>;

    fn claim_available(
        &self,
        request: ExecutionLeaseRequest,
        ctx: &ExecutionContext,
    ) -> AppResult<Vec<ClaimedExecution>>;

    fn renew_lease(
        &self,
        renewal: ExecutionLeaseRenewal,
        ctx: &ExecutionContext,
    ) -> AppResult<ExecutionLease>;

    fn acknowledge_completion(
        &self,
        success: ExecutionSuccess,
        ctx: &ExecutionContext,
    ) -> AppResult<()>;

    fn retry_execution(&self, retry: ExecutionRetry, ctx: &ExecutionContext) -> AppResult<()>;

    fn dead_letter_execution(
        &self,
        dead_letter: ExecutionDeadLetter,
        ctx: &ExecutionContext,
    ) -> AppResult<()>;
}

mod inspection;

pub use inspection::{
    ExecutionInspectionQuery, ExecutionInspectionRecord, ExecutionInspectionSnapshot,
    ExecutionInspectionStore, ExecutionInspectionSummary,
};

impl ExecutionLeaseRenewal {
    pub fn validate(&self, ctx: &ExecutionContext) -> AppResult<()> {
        if self.leased_by.trim().is_empty() {
            return Err(AppError::from_context(
                ErrorCode::InvalidInput,
                "execution_lease_renewal_owner_required",
                "execution lease renewals require a non-empty worker identity",
                ctx,
            ));
        }

        if self.lease_ttl.is_zero() {
            return Err(AppError::from_context(
                ErrorCode::InvalidInput,
                "execution_lease_renewal_ttl_required",
                "execution lease renewals require a positive lease ttl",
                ctx,
            ));
        }

        Ok(())
    }
}

pub(crate) fn validate_scope(scope: &str, ctx: &ExecutionContext) -> AppResult<()> {
    if scope.trim().is_empty() {
        return Err(AppError::from_context(
            ErrorCode::InvalidInput,
            "idempotency_scope_required",
            "idempotency claims require a non-empty scope",
            ctx,
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests;
