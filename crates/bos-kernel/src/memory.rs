use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;
use std::time::SystemTime;

use crate::{
    AppError, AppResult, ClaimedDispatchWork, ClaimedExecution, ClaimedOutboxDelivery,
    DispatchClaimRequest, DispatchLease, DispatchLeaseRenewal, DispatchQueue,
    DispatchQueueStatusSnapshot, DispatchResolution, DispatchSelectionRequest, DispatchStatusStore,
    DispatchWorkStore, DueDispatchWork, DueExecutionRequest, DueOutboxMessageRequest,
    ExecutionContext, ExecutionDeadLetter, ExecutionId, ExecutionInspectionQuery,
    ExecutionInspectionRecord, ExecutionInspectionStore, ExecutionInspectionSummary,
    ExecutionJournal, ExecutionLease, ExecutionLeaseRenewal, ExecutionLeaseRequest,
    ExecutionRecord, ExecutionRecordStatus, ExecutionRetry, ExecutionSuccess,
    ExpiredLeaseRecoveryRequest, ExpiredLeaseRecoveryStore, ExpiredLeaseRecoverySummary, LeaseId,
    LeasedExecutions, LeasedOutbox, OutboxDeliveryDeadLetter, OutboxDeliveryLease,
    OutboxDeliveryLeaseRenewal, OutboxDeliveryLeaseRequest, OutboxDeliveryRetry,
    OutboxDeliverySuccess, OutboxInspectionQuery, OutboxInspectionRecord, OutboxInspectionStore,
    OutboxInspectionSummary, OutboxMessage, OutboxMessageId, OutboxMessageStatus, RevisionToken,
    VisibleWorkersQuery, WorkerDispatchSnapshot, WorkerDispatchSnapshotRef,
    WorkerDispatchSnapshotsQuery, WorkerHeartbeat, WorkerHeartbeatStore, WorkerVisibilityRef,
};

#[derive(Debug, Default)]
pub struct InMemoryExecutionWorkStore {
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    records: HashMap<ExecutionId, StoredExecution>,
    outbox: HashMap<OutboxMessageId, StoredOutboxMessage>,
    heartbeats: HashMap<(String, String), WorkerHeartbeat>,
    next_lease_sequence: u64,
}

#[derive(Debug, Clone)]
struct StoredExecution {
    record: ExecutionRecord,
    lease: Option<ExecutionLease>,
}

#[derive(Debug, Clone)]
struct StoredOutboxMessage {
    message: OutboxMessage,
    lease: Option<OutboxDeliveryLease>,
    status: OutboxMessageStatus,
    delivered_at: Option<SystemTime>,
    dead_lettered_at: Option<SystemTime>,
    last_error: Option<String>,
}

impl InMemoryExecutionWorkStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&self, execution_id: &ExecutionId) -> AppResult<Option<ExecutionRecord>> {
        let state = self.lock_state()?;
        Ok(state
            .records
            .get(execution_id)
            .map(|entry| entry.record.clone()))
    }

    pub fn lease(&self, execution_id: &ExecutionId) -> AppResult<Option<ExecutionLease>> {
        let state = self.lock_state()?;
        Ok(state
            .records
            .get(execution_id)
            .and_then(|entry| entry.lease.clone()))
    }

    pub fn append_outbox(&self, message: OutboxMessage, ctx: &ExecutionContext) -> AppResult<()> {
        let mut state = self.lock_state()?;
        if state.outbox.contains_key(message.id()) {
            return Err(AppError::conflict(
                "outbox_message_exists",
                format!("outbox message {} already exists", message.id()),
                ctx.correlation_id.clone(),
            ));
        }

        state.outbox.insert(
            message.id().clone(),
            StoredOutboxMessage {
                message,
                lease: None,
                status: OutboxMessageStatus::Pending,
                delivered_at: None,
                dead_lettered_at: None,
                last_error: None,
            },
        );
        Ok(())
    }

    fn lock_state(&self) -> AppResult<std::sync::MutexGuard<'_, State>> {
        self.state.lock().map_err(|_| {
            AppError::unexpected(
                "in_memory_execution_store_poisoned",
                "in-memory execution store mutex is poisoned",
                crate::CorrelationId::generate(),
            )
        })
    }

    fn require_queue_supported(queue: DispatchQueue, _ctx: &ExecutionContext) -> AppResult<()> {
        match queue {
            DispatchQueue::Execution | DispatchQueue::Outbox => Ok(()),
        }
    }

    fn require_revision(
        expected: Option<RevisionToken>,
        actual: RevisionToken,
        ctx: &ExecutionContext,
        code: &'static str,
    ) -> AppResult<()> {
        if expected.is_some_and(|expected| expected != actual) {
            return Err(AppError::concurrent_modification(
                code,
                "state transition revision did not match current persisted revision",
                ctx.correlation_id.clone(),
            ));
        }
        Ok(())
    }
}

impl State {
    fn next_lease_id(&mut self) -> LeaseId {
        self.next_lease_sequence = self.next_lease_sequence.saturating_add(1);
        LeaseId::new(format!("lease_exec_mem_{:x}", self.next_lease_sequence))
    }

    fn active_leases_for_worker(&self, worker_id: &str, now: SystemTime) -> usize {
        self.active_execution_leases_for_worker(worker_id, now)
            + self.active_outbox_leases_for_worker(worker_id, now)
    }

    fn active_execution_leases_for_worker(&self, worker_id: &str, now: SystemTime) -> usize {
        self.records
            .values()
            .filter(|entry| {
                entry.lease.as_ref().is_some_and(|lease| {
                    lease.leased_by() == worker_id && !lease.is_expired_at(now)
                })
            })
            .count()
    }

    fn active_outbox_leases_for_worker(&self, worker_id: &str, now: SystemTime) -> usize {
        self.outbox
            .values()
            .filter(|entry| {
                entry.lease.as_ref().is_some_and(|lease| {
                    lease.leased_by() == worker_id && !lease.is_expired_at(now)
                })
            })
            .count()
    }

    fn stale_execution_claim_count_for_worker(&self, worker_id: &str, now: SystemTime) -> usize {
        self.records
            .values()
            .filter(|entry| {
                entry
                    .lease
                    .as_ref()
                    .is_some_and(|lease| lease.leased_by() == worker_id && lease.is_expired_at(now))
            })
            .count()
    }

    fn stale_outbox_claim_count_for_worker(&self, worker_id: &str, now: SystemTime) -> usize {
        self.outbox
            .values()
            .filter(|entry| {
                entry
                    .lease
                    .as_ref()
                    .is_some_and(|lease| lease.leased_by() == worker_id && lease.is_expired_at(now))
            })
            .count()
    }

    fn due_execution_count(&self, now: SystemTime) -> usize {
        self.records
            .values()
            .filter(|entry| entry.is_due_at(now, true))
            .count()
    }

    fn due_outbox_count(&self, now: SystemTime) -> usize {
        self.outbox
            .values()
            .filter(|entry| entry.is_due_at(now, true))
            .count()
    }

    fn oldest_due_execution_at(&self, now: SystemTime) -> Option<SystemTime> {
        self.records
            .values()
            .filter(|entry| entry.is_due_at(now, true))
            .map(|entry| entry.record.available_at())
            .min()
    }

    fn oldest_due_outbox_at(&self, now: SystemTime) -> Option<SystemTime> {
        self.outbox
            .values()
            .filter(|entry| entry.is_due_at(now, true))
            .map(|entry| entry.message.available_at())
            .min()
    }

    fn selectable_due_records(&self, request: &DueExecutionRequest) -> Vec<ExecutionRecord> {
        let mut due = self
            .records
            .values()
            .filter(|entry| entry.is_due_at(request.due_before(), request.include_expired_leases()))
            .map(|entry| entry.materialize_due_record(request.due_before()))
            .collect::<Vec<_>>();

        due.sort_by_key(|record| {
            (
                record.available_at(),
                record.recorded_at(),
                record.id().clone(),
            )
        });
        due.truncate(request.limit());
        due
    }

    fn find_claimable_ids(&self, request: &ExecutionLeaseRequest) -> Vec<ExecutionId> {
        let selection = DueExecutionRequest::new(request.batch_size())
            .at(request.now())
            .with_expired_leases(true);

        self.selectable_due_records(&selection)
            .into_iter()
            .map(|record| record.id().clone())
            .collect()
    }

    fn selectable_due_messages(&self, request: &DueOutboxMessageRequest) -> Vec<OutboxMessage> {
        let mut due = self
            .outbox
            .values()
            .filter(|entry| entry.is_due_at(request.due_before(), request.include_expired_leases()))
            .map(|entry| entry.materialize_due_message())
            .collect::<Vec<_>>();

        due.sort_by_key(|message| {
            (
                message.available_at(),
                message.last_attempted_at(),
                message.id().clone(),
            )
        });
        due.truncate(request.limit());
        due
    }

    fn find_claimable_message_ids(
        &self,
        request: &OutboxDeliveryLeaseRequest,
    ) -> Vec<OutboxMessageId> {
        let selection = DueOutboxMessageRequest::new(request.batch_size())
            .at(request.now())
            .with_expired_leases(true);

        self.selectable_due_messages(&selection)
            .into_iter()
            .map(|message| message.id().clone())
            .collect()
    }

    fn snapshot_heartbeat(
        &self,
        worker_id: &str,
        scope: &str,
        observed_at: SystemTime,
    ) -> Option<WorkerHeartbeat> {
        self.heartbeats
            .get(&(scope.to_string(), worker_id.to_string()))
            .cloned()
            .map(|heartbeat| {
                heartbeat.with_active_leases(self.active_leases_for_worker(worker_id, observed_at))
            })
    }

    fn visible_heartbeat(
        &self,
        worker_id: &str,
        scope: &str,
        visible_at: SystemTime,
    ) -> Option<WorkerHeartbeat> {
        self.snapshot_heartbeat(worker_id, scope, visible_at)
            .filter(|heartbeat| heartbeat.visible_until() >= visible_at)
    }

    fn queue_snapshot_for_worker(
        &self,
        queue: DispatchQueue,
        worker_id: &str,
        observed_at: SystemTime,
    ) -> DispatchQueueStatusSnapshot {
        match queue {
            DispatchQueue::Execution => DispatchQueueStatusSnapshot::new(DispatchQueue::Execution)
                .with_due_count(self.due_execution_count(observed_at))
                .with_claimed_count(self.active_execution_leases_for_worker(worker_id, observed_at))
                .with_stale_claim_count(
                    self.stale_execution_claim_count_for_worker(worker_id, observed_at),
                )
                .with_oldest_due_at(self.oldest_due_execution_at(observed_at)),
            DispatchQueue::Outbox => DispatchQueueStatusSnapshot::new(DispatchQueue::Outbox)
                .with_due_count(self.due_outbox_count(observed_at))
                .with_claimed_count(self.active_outbox_leases_for_worker(worker_id, observed_at))
                .with_stale_claim_count(
                    self.stale_outbox_claim_count_for_worker(worker_id, observed_at),
                )
                .with_oldest_due_at(self.oldest_due_outbox_at(observed_at)),
        }
    }

    fn worker_ids_for_scope(&self, scope: &str) -> Vec<String> {
        let mut worker_ids = BTreeSet::new();
        for heartbeat in self
            .heartbeats
            .values()
            .filter(|heartbeat| heartbeat.scope() == scope)
        {
            worker_ids.insert(heartbeat.worker_id().to_string());
        }
        for worker_id in self.records.values().filter_map(|entry| {
            entry
                .lease
                .as_ref()
                .map(|lease| lease.leased_by().to_string())
        }) {
            worker_ids.insert(worker_id);
        }
        for worker_id in self.outbox.values().filter_map(|entry| {
            entry
                .lease
                .as_ref()
                .map(|lease| lease.leased_by().to_string())
        }) {
            worker_ids.insert(worker_id);
        }

        worker_ids.into_iter().collect()
    }
}

impl StoredExecution {
    fn is_due_at(&self, now: SystemTime, include_expired_leases: bool) -> bool {
        if !matches!(
            self.record.status(),
            ExecutionRecordStatus::Pending
                | ExecutionRecordStatus::Failed
                | ExecutionRecordStatus::InFlight
        ) {
            return false;
        }

        if self.record.available_at() > now {
            return false;
        }

        match self.lease.as_ref() {
            None => true,
            Some(lease) if lease.is_expired_at(now) => include_expired_leases,
            Some(_) => false,
        }
    }

    fn materialize_due_record(&self, now: SystemTime) -> ExecutionRecord {
        match self.lease.as_ref() {
            Some(lease)
                if lease.is_expired_at(now)
                    && self.record.status() == ExecutionRecordStatus::InFlight =>
            {
                self.record
                    .clone()
                    .with_status(ExecutionRecordStatus::Pending)
            }
            _ => self.record.clone(),
        }
    }

    fn inspection_record(&self) -> ExecutionInspectionRecord {
        ExecutionInspectionRecord::new(self.record.clone()).with_lease(self.lease.clone())
    }
}

impl StoredOutboxMessage {
    fn is_due_at(&self, now: SystemTime, include_expired_leases: bool) -> bool {
        if !matches!(
            self.status,
            OutboxMessageStatus::Pending | OutboxMessageStatus::InFlight
        ) {
            return false;
        }

        if self.message.available_at() > now {
            return false;
        }

        match self.lease.as_ref() {
            None => true,
            Some(lease) if lease.is_expired_at(now) => include_expired_leases,
            Some(_) => false,
        }
    }

    fn materialize_due_message(&self) -> OutboxMessage {
        self.message.clone()
    }

    fn inspection_status_at(&self, now: SystemTime) -> OutboxMessageStatus {
        match self.status {
            OutboxMessageStatus::Pending => OutboxMessageStatus::Pending,
            OutboxMessageStatus::Delivered => OutboxMessageStatus::Delivered,
            OutboxMessageStatus::DeadLettered => OutboxMessageStatus::DeadLettered,
            OutboxMessageStatus::InFlight => self
                .lease
                .as_ref()
                .filter(|lease| !lease.is_expired_at(now))
                .map(|_| OutboxMessageStatus::InFlight)
                .unwrap_or(OutboxMessageStatus::Pending),
        }
    }

    fn inspection_record(&self, now: SystemTime) -> OutboxInspectionRecord {
        OutboxInspectionRecord::new(self.message.clone(), self.inspection_status_at(now))
            .with_lease(self.lease.clone())
            .with_delivered_at(self.delivered_at)
            .with_dead_lettered_at(self.dead_lettered_at)
            .with_last_error(self.last_error.clone())
    }
}

impl ExecutionJournal for InMemoryExecutionWorkStore {
    fn append(&self, record: ExecutionRecord, ctx: &ExecutionContext) -> AppResult<()> {
        let mut state = self.lock_state()?;
        if state.records.contains_key(record.id()) {
            return Err(AppError::conflict(
                "execution_record_exists",
                format!("execution record {} already exists", record.id()),
                ctx.correlation_id.clone(),
            ));
        }

        state.records.insert(
            record.id().clone(),
            StoredExecution {
                record,
                lease: None,
            },
        );
        Ok(())
    }

    fn update(&self, record: ExecutionRecord, ctx: &ExecutionContext) -> AppResult<()> {
        let mut state = self.lock_state()?;
        let entry = state.records.get_mut(record.id()).ok_or_else(|| {
            AppError::not_found(
                "execution_record_not_found",
                format!("execution record {} was not found", record.id()),
                ctx.correlation_id.clone(),
            )
        })?;

        entry.record = record;
        if !matches!(entry.record.status(), ExecutionRecordStatus::InFlight) {
            entry.lease = None;
        }

        Ok(())
    }
}

impl ExecutionInspectionStore for InMemoryExecutionWorkStore {
    fn lookup_execution(
        &self,
        execution_id: &ExecutionId,
        _ctx: &ExecutionContext,
    ) -> AppResult<Option<ExecutionRecord>> {
        self.record(execution_id)
    }

    fn lookup_execution_inspection(
        &self,
        execution_id: &ExecutionId,
        _observed_at: SystemTime,
        _ctx: &ExecutionContext,
    ) -> AppResult<Option<ExecutionInspectionRecord>> {
        let state = self.lock_state()?;
        Ok(state
            .records
            .get(execution_id)
            .map(StoredExecution::inspection_record))
    }

    fn list_executions(
        &self,
        query: ExecutionInspectionQuery,
        _ctx: &ExecutionContext,
    ) -> AppResult<Vec<ExecutionRecord>> {
        let state = self.lock_state()?;
        let mut records = state
            .records
            .values()
            .filter(|entry| {
                query
                    .status()
                    .map(|status| entry.record.status() == status)
                    .unwrap_or(true)
            })
            .filter(|entry| {
                query
                    .operation()
                    .map(|operation| entry.record.operation() == operation)
                    .unwrap_or(true)
            })
            .filter(|entry| {
                query
                    .target()
                    .map(|target| entry.record.target() == target)
                    .unwrap_or(true)
            })
            .map(|entry| entry.record.clone())
            .collect::<Vec<_>>();

        records.sort_by_key(|record| {
            (
                record.available_at(),
                record.recorded_at(),
                record.id().clone(),
            )
        });
        records.truncate(query.limit());
        Ok(records)
    }

    fn list_execution_inspection(
        &self,
        query: ExecutionInspectionQuery,
        _ctx: &ExecutionContext,
    ) -> AppResult<Vec<ExecutionInspectionRecord>> {
        let state = self.lock_state()?;
        let mut records = state
            .records
            .values()
            .filter(|entry| {
                query
                    .status()
                    .map(|status| entry.record.status() == status)
                    .unwrap_or(true)
            })
            .filter(|entry| {
                query
                    .operation()
                    .map(|operation| entry.record.operation() == operation)
                    .unwrap_or(true)
            })
            .filter(|entry| {
                query
                    .target()
                    .map(|target| entry.record.target() == target)
                    .unwrap_or(true)
            })
            .map(StoredExecution::inspection_record)
            .collect::<Vec<_>>();

        records.sort_by_key(|record| {
            (
                record.record().available_at(),
                record.record().recorded_at(),
                record.record().id().clone(),
            )
        });
        records.truncate(query.limit());
        Ok(records)
    }

    fn summarize_executions(
        &self,
        query: ExecutionInspectionQuery,
        _ctx: &ExecutionContext,
    ) -> AppResult<ExecutionInspectionSummary> {
        let state = self.lock_state()?;
        Ok(state
            .records
            .values()
            .filter(|entry| {
                query
                    .status()
                    .map(|status| entry.record.status() == status)
                    .unwrap_or(true)
            })
            .filter(|entry| {
                query
                    .operation()
                    .map(|operation| entry.record.operation() == operation)
                    .unwrap_or(true)
            })
            .filter(|entry| {
                query
                    .target()
                    .map(|target| entry.record.target() == target)
                    .unwrap_or(true)
            })
            .fold(ExecutionInspectionSummary::default(), |summary, entry| {
                summary.observe_record(&entry.inspection_record(), query.observed_at())
            }))
    }
}

impl LeasedExecutions for InMemoryExecutionWorkStore {
    fn select_due(
        &self,
        request: DueExecutionRequest,
        _ctx: &ExecutionContext,
    ) -> AppResult<Vec<ExecutionRecord>> {
        let state = self.lock_state()?;
        Ok(state.selectable_due_records(&request))
    }

    fn claim_available(
        &self,
        request: ExecutionLeaseRequest,
        ctx: &ExecutionContext,
    ) -> AppResult<Vec<ClaimedExecution>> {
        tracing::info!(
            event = "dispatch.claim_attempted",
            queue = DispatchQueue::Execution.as_str(),
            trace_id = ctx.correlation_id.as_str(),
            worker_id = request.leased_by(),
            batch_size = request.batch_size()
        );
        let mut state = self.lock_state()?;
        let claimable_ids = state.find_claimable_ids(&request);
        let mut claimed = Vec::with_capacity(claimable_ids.len());

        for execution_id in claimable_ids {
            let lease = ExecutionLease::from_now(
                state.next_lease_id(),
                request.leased_by().to_string(),
                request.lease_ttl(),
                request.now(),
            );
            let Some(entry) = state.records.get_mut(&execution_id) else {
                tracing::warn!(
                    event = "dispatch.claim_conflicted",
                    queue = DispatchQueue::Execution.as_str(),
                    trace_id = ctx.correlation_id.as_str(),
                    worker_id = request.leased_by(),
                    entity_id = execution_id.as_str(),
                    error_class = "not_found"
                );
                return Err(AppError::not_found(
                    "execution_record_not_found",
                    format!("execution record {execution_id} was not found"),
                    ctx.correlation_id.clone(),
                ));
            };

            let claimable_record = entry.materialize_due_record(request.now());
            let next_claim = ClaimedExecution::claim(claimable_record, lease.clone())?;
            entry.record = next_claim.record().clone();
            entry.lease = Some(lease);
            claimed.push(next_claim);
        }

        tracing::info!(
            event = "dispatch.claim_succeeded",
            queue = DispatchQueue::Execution.as_str(),
            trace_id = ctx.correlation_id.as_str(),
            worker_id = request.leased_by(),
            claimed = claimed.len()
        );
        Ok(claimed)
    }

    fn renew_lease(
        &self,
        renewal: ExecutionLeaseRenewal,
        ctx: &ExecutionContext,
    ) -> AppResult<ExecutionLease> {
        renewal.validate(ctx)?;

        let mut state = self.lock_state()?;
        let entry = state
            .records
            .get_mut(renewal.execution_id())
            .ok_or_else(|| {
                AppError::not_found(
                    "execution_record_not_found",
                    format!("execution record {} was not found", renewal.execution_id()),
                    ctx.correlation_id.clone(),
                )
            })?;
        let lease = entry.lease.as_ref().ok_or_else(|| {
            AppError::conflict(
                "execution_lease_missing",
                "execution work is not currently leased",
                ctx.correlation_id.clone(),
            )
        })?;

        if lease.lease_id() != renewal.lease_id() || lease.leased_by() != renewal.leased_by() {
            return Err(AppError::concurrent_modification(
                "execution_lease_mismatch",
                "execution lease renewal did not match the active lease owner or lease id",
                ctx.correlation_id.clone(),
            ));
        }

        if lease.is_expired_at(renewal.renewed_at()) {
            return Err(AppError::conflict(
                "execution_lease_expired",
                "expired execution leases cannot be renewed",
                ctx.correlation_id.clone(),
            ));
        }

        let renewed = renewal.renewed_lease();
        entry.lease = Some(renewed.clone());
        Ok(renewed)
    }

    fn acknowledge_completion(
        &self,
        success: ExecutionSuccess,
        ctx: &ExecutionContext,
    ) -> AppResult<()> {
        let mut state = self.lock_state()?;
        let entry = state
            .records
            .get_mut(success.execution_id())
            .ok_or_else(|| {
                AppError::not_found(
                    "execution_record_not_found",
                    format!("execution record {} was not found", success.execution_id()),
                    ctx.correlation_id.clone(),
                )
            })?;

        match entry.lease.as_ref() {
            Some(lease) if lease.lease_id() == success.lease_id() => {
                Self::require_revision(
                    success.expected_revision(),
                    entry.record.revision(),
                    ctx,
                    "execution_completion_revision_mismatch",
                )?;
                entry.record = entry.record.clone().mark_succeeded(success.finished_at());
                entry.lease = None;
                Ok(())
            }
            Some(_) => Err(AppError::concurrent_modification(
                "execution_completion_lease_mismatch",
                "execution completion did not match the active lease",
                ctx.correlation_id.clone(),
            )),
            None => Err(AppError::conflict(
                "execution_completion_lease_missing",
                "execution completion requires an active lease",
                ctx.correlation_id.clone(),
            )),
        }
    }

    fn retry_execution(&self, retry: ExecutionRetry, ctx: &ExecutionContext) -> AppResult<()> {
        let mut state = self.lock_state()?;
        let entry = state.records.get_mut(retry.execution_id()).ok_or_else(|| {
            AppError::not_found(
                "execution_record_not_found",
                format!("execution record {} was not found", retry.execution_id()),
                ctx.correlation_id.clone(),
            )
        })?;

        match entry.lease.as_ref() {
            Some(lease) if lease.lease_id() == retry.lease_id() => {
                Self::require_revision(
                    retry.expected_revision(),
                    entry.record.revision(),
                    ctx,
                    "execution_retry_revision_mismatch",
                )?;
                entry.record = entry.record.clone().schedule_retry(
                    retry.attempted_at(),
                    retry.next_available_at(),
                    retry.error().to_string(),
                );
                entry.lease = None;
                Ok(())
            }
            Some(_) => Err(AppError::concurrent_modification(
                "execution_retry_lease_mismatch",
                "execution retry did not match the active lease",
                ctx.correlation_id.clone(),
            )),
            None => Err(AppError::conflict(
                "execution_retry_lease_missing",
                "execution retry requires an active lease",
                ctx.correlation_id.clone(),
            )),
        }
    }

    fn dead_letter_execution(
        &self,
        dead_letter: ExecutionDeadLetter,
        ctx: &ExecutionContext,
    ) -> AppResult<()> {
        let mut state = self.lock_state()?;
        let entry = state
            .records
            .get_mut(dead_letter.execution_id())
            .ok_or_else(|| {
                AppError::not_found(
                    "execution_record_not_found",
                    format!(
                        "execution record {} was not found",
                        dead_letter.execution_id()
                    ),
                    ctx.correlation_id.clone(),
                )
            })?;

        match entry.lease.as_ref() {
            Some(lease) if lease.lease_id() == dead_letter.lease_id() => {
                Self::require_revision(
                    dead_letter.expected_revision(),
                    entry.record.revision(),
                    ctx,
                    "execution_dead_letter_revision_mismatch",
                )?;
                entry.record = entry.record.clone().mark_dead_lettered(
                    dead_letter.dead_lettered_at(),
                    dead_letter.error().to_string(),
                );
                entry.lease = None;
                Ok(())
            }
            Some(_) => Err(AppError::concurrent_modification(
                "execution_dead_letter_lease_mismatch",
                "execution dead-letter acknowledgement did not match the active lease",
                ctx.correlation_id.clone(),
            )),
            None => Err(AppError::conflict(
                "execution_dead_letter_lease_missing",
                "execution dead-letter acknowledgement requires an active lease",
                ctx.correlation_id.clone(),
            )),
        }
    }
}

impl OutboxInspectionStore for InMemoryExecutionWorkStore {
    fn lookup_outbox_message(
        &self,
        message_id: &OutboxMessageId,
        observed_at: SystemTime,
        _ctx: &ExecutionContext,
    ) -> AppResult<Option<OutboxInspectionRecord>> {
        let state = self.lock_state()?;
        Ok(state
            .outbox
            .get(message_id)
            .map(|entry| entry.inspection_record(observed_at)))
    }

    fn list_outbox_messages(
        &self,
        query: OutboxInspectionQuery,
        _ctx: &ExecutionContext,
    ) -> AppResult<Vec<OutboxInspectionRecord>> {
        let state = self.lock_state()?;
        let mut records = state
            .outbox
            .values()
            .filter(|entry| {
                query
                    .status()
                    .map(|status| entry.inspection_status_at(query.observed_at()) == status)
                    .unwrap_or(true)
            })
            .filter(|entry| {
                query
                    .aggregate()
                    .map(|aggregate| entry.message.aggregate() == aggregate)
                    .unwrap_or(true)
            })
            .filter(|entry| {
                query
                    .topic()
                    .map(|topic| entry.message.envelope().topic() == topic)
                    .unwrap_or(true)
            })
            .map(|entry| entry.inspection_record(query.observed_at()))
            .collect::<Vec<_>>();

        records.sort_by_key(|record| {
            (
                record.message().available_at(),
                record.message().last_attempted_at(),
                record.message().id().clone(),
            )
        });
        records.truncate(query.limit());
        Ok(records)
    }

    fn summarize_outbox(
        &self,
        query: OutboxInspectionQuery,
        _ctx: &ExecutionContext,
    ) -> AppResult<OutboxInspectionSummary> {
        let state = self.lock_state()?;
        Ok(state
            .outbox
            .values()
            .filter(|entry| {
                query
                    .status()
                    .map(|status| entry.inspection_status_at(query.observed_at()) == status)
                    .unwrap_or(true)
            })
            .filter(|entry| {
                query
                    .aggregate()
                    .map(|aggregate| entry.message.aggregate() == aggregate)
                    .unwrap_or(true)
            })
            .filter(|entry| {
                query
                    .topic()
                    .map(|topic| entry.message.envelope().topic() == topic)
                    .unwrap_or(true)
            })
            .fold(OutboxInspectionSummary::default(), |summary, entry| {
                summary.observe(
                    &entry.inspection_record(query.observed_at()),
                    query.observed_at(),
                )
            }))
    }
}

impl LeasedOutbox for InMemoryExecutionWorkStore {
    fn select_due(
        &self,
        request: DueOutboxMessageRequest,
        _ctx: &ExecutionContext,
    ) -> AppResult<Vec<OutboxMessage>> {
        let state = self.lock_state()?;
        Ok(state.selectable_due_messages(&request))
    }

    fn lease_available(
        &self,
        request: OutboxDeliveryLeaseRequest,
        ctx: &ExecutionContext,
    ) -> AppResult<Vec<ClaimedOutboxDelivery>> {
        tracing::info!(
            event = "dispatch.claim_attempted",
            queue = DispatchQueue::Outbox.as_str(),
            trace_id = ctx.correlation_id.as_str(),
            worker_id = request.leased_by(),
            batch_size = request.batch_size()
        );
        let mut state = self.lock_state()?;
        let claimable_ids = state.find_claimable_message_ids(&request);
        let mut claimed = Vec::with_capacity(claimable_ids.len());

        for message_id in claimable_ids {
            let lease = OutboxDeliveryLease::from_now(
                state.next_lease_id(),
                request.leased_by().to_string(),
                request.lease_ttl(),
                request.now(),
            );
            let Some(entry) = state.outbox.get_mut(&message_id) else {
                tracing::warn!(
                    event = "dispatch.claim_conflicted",
                    queue = DispatchQueue::Outbox.as_str(),
                    trace_id = ctx.correlation_id.as_str(),
                    worker_id = request.leased_by(),
                    entity_id = message_id.as_str(),
                    error_class = "not_found"
                );
                return Err(AppError::not_found(
                    "outbox_message_not_found",
                    format!("outbox message {message_id} was not found"),
                    ctx.correlation_id.clone(),
                ));
            };

            let claimable_message = entry.materialize_due_message();
            let next_claim = ClaimedOutboxDelivery::claim(claimable_message, lease.clone())?;
            entry.message = next_claim.message().clone();
            entry.lease = Some(lease);
            entry.status = OutboxMessageStatus::InFlight;
            claimed.push(next_claim);
        }

        tracing::info!(
            event = "dispatch.claim_succeeded",
            queue = DispatchQueue::Outbox.as_str(),
            trace_id = ctx.correlation_id.as_str(),
            worker_id = request.leased_by(),
            claimed = claimed.len()
        );
        Ok(claimed)
    }

    fn renew_lease(
        &self,
        renewal: OutboxDeliveryLeaseRenewal,
        ctx: &ExecutionContext,
    ) -> AppResult<OutboxDeliveryLease> {
        renewal.validate(ctx)?;

        let mut state = self.lock_state()?;
        let entry = state.outbox.get_mut(renewal.message_id()).ok_or_else(|| {
            AppError::not_found(
                "outbox_message_not_found",
                format!("outbox message {} was not found", renewal.message_id()),
                ctx.correlation_id.clone(),
            )
        })?;
        let lease = entry.lease.as_ref().ok_or_else(|| {
            AppError::conflict(
                "outbox_lease_missing",
                "outbox work is not currently leased",
                ctx.correlation_id.clone(),
            )
        })?;

        if lease.lease_id() != renewal.lease_id() || lease.leased_by() != renewal.leased_by() {
            return Err(AppError::concurrent_modification(
                "outbox_lease_mismatch",
                "outbox lease renewal did not match the active lease owner or lease id",
                ctx.correlation_id.clone(),
            ));
        }

        if lease.is_expired_at(renewal.renewed_at()) {
            return Err(AppError::conflict(
                "outbox_lease_expired",
                "expired outbox leases cannot be renewed",
                ctx.correlation_id.clone(),
            ));
        }

        let renewed = renewal.renewed_lease();
        entry.lease = Some(renewed.clone());
        Ok(renewed)
    }

    fn acknowledge_delivery(
        &self,
        success: OutboxDeliverySuccess,
        ctx: &ExecutionContext,
    ) -> AppResult<()> {
        let mut state = self.lock_state()?;
        let entry = state.outbox.get_mut(success.message_id()).ok_or_else(|| {
            AppError::not_found(
                "outbox_message_not_found",
                format!("outbox message {} was not found", success.message_id()),
                ctx.correlation_id.clone(),
            )
        })?;

        match entry.lease.as_ref() {
            Some(lease) if lease.lease_id() == success.lease_id() => {
                Self::require_revision(
                    success.expected_revision(),
                    entry.message.revision(),
                    ctx,
                    "outbox_delivery_revision_mismatch",
                )?;
                entry.message = entry
                    .message
                    .clone()
                    .mark_attempted(success.delivered_at(), None);
                entry.lease = None;
                entry.status = OutboxMessageStatus::Delivered;
                entry.delivered_at = Some(success.delivered_at());
                entry.last_error = None;
                Ok(())
            }
            Some(_) => Err(AppError::concurrent_modification(
                "outbox_delivery_lease_mismatch",
                "outbox delivery acknowledgement did not match the active lease",
                ctx.correlation_id.clone(),
            )),
            None => Err(AppError::conflict(
                "outbox_delivery_lease_missing",
                "outbox delivery acknowledgement requires an active lease",
                ctx.correlation_id.clone(),
            )),
        }
    }

    fn retry_delivery(&self, retry: OutboxDeliveryRetry, ctx: &ExecutionContext) -> AppResult<()> {
        let mut state = self.lock_state()?;
        let entry = state.outbox.get_mut(retry.message_id()).ok_or_else(|| {
            AppError::not_found(
                "outbox_message_not_found",
                format!("outbox message {} was not found", retry.message_id()),
                ctx.correlation_id.clone(),
            )
        })?;

        match entry.lease.as_ref() {
            Some(lease) if lease.lease_id() == retry.lease_id() => {
                Self::require_revision(
                    retry.expected_revision(),
                    entry.message.revision(),
                    ctx,
                    "outbox_retry_revision_mismatch",
                )?;
                entry.message = entry
                    .message
                    .clone()
                    .mark_attempted(retry.attempted_at(), Some(retry.next_available_at()));
                entry.lease = None;
                entry.status = OutboxMessageStatus::Pending;
                entry.last_error = Some(retry.error().to_string());
                Ok(())
            }
            Some(_) => Err(AppError::concurrent_modification(
                "outbox_retry_lease_mismatch",
                "outbox retry did not match the active lease",
                ctx.correlation_id.clone(),
            )),
            None => Err(AppError::conflict(
                "outbox_retry_lease_missing",
                "outbox retry requires an active lease",
                ctx.correlation_id.clone(),
            )),
        }
    }

    fn dead_letter_delivery(
        &self,
        dead_letter: OutboxDeliveryDeadLetter,
        ctx: &ExecutionContext,
    ) -> AppResult<()> {
        let mut state = self.lock_state()?;
        let entry = state
            .outbox
            .get_mut(dead_letter.message_id())
            .ok_or_else(|| {
                AppError::not_found(
                    "outbox_message_not_found",
                    format!("outbox message {} was not found", dead_letter.message_id()),
                    ctx.correlation_id.clone(),
                )
            })?;

        match entry.lease.as_ref() {
            Some(lease) if lease.lease_id() == dead_letter.lease_id() => {
                Self::require_revision(
                    dead_letter.expected_revision(),
                    entry.message.revision(),
                    ctx,
                    "outbox_dead_letter_revision_mismatch",
                )?;
                entry.message = entry
                    .message
                    .clone()
                    .mark_attempted(dead_letter.dead_lettered_at(), None);
                entry.lease = None;
                entry.status = OutboxMessageStatus::DeadLettered;
                entry.dead_lettered_at = Some(dead_letter.dead_lettered_at());
                entry.last_error = Some(dead_letter.error().to_string());
                Ok(())
            }
            Some(_) => Err(AppError::concurrent_modification(
                "outbox_dead_letter_lease_mismatch",
                "outbox dead-letter acknowledgement did not match the active lease",
                ctx.correlation_id.clone(),
            )),
            None => Err(AppError::conflict(
                "outbox_dead_letter_lease_missing",
                "outbox dead-letter acknowledgement requires an active lease",
                ctx.correlation_id.clone(),
            )),
        }
    }
}

impl DispatchWorkStore for InMemoryExecutionWorkStore {
    fn select_due(
        &self,
        request: DispatchSelectionRequest,
        ctx: &ExecutionContext,
    ) -> AppResult<Vec<DueDispatchWork>> {
        Self::require_queue_supported(request.queue(), ctx)?;
        match request.queue() {
            DispatchQueue::Execution => {
                let request = request.as_execution_request().ok_or_else(|| {
                    AppError::unexpected(
                        "dispatch_selection_queue_mismatch",
                        "execution dispatch request did not convert",
                        ctx.correlation_id.clone(),
                    )
                })?;
                LeasedExecutions::select_due(self, request, ctx).map(|records| {
                    records
                        .into_iter()
                        .map(DueDispatchWork::Execution)
                        .collect()
                })
            }
            DispatchQueue::Outbox => {
                let request = request.as_outbox_request().ok_or_else(|| {
                    AppError::unexpected(
                        "dispatch_selection_queue_mismatch",
                        "outbox dispatch request did not convert",
                        ctx.correlation_id.clone(),
                    )
                })?;
                LeasedOutbox::select_due(self, request, ctx)
                    .map(|messages| messages.into_iter().map(DueDispatchWork::Outbox).collect())
            }
        }
    }

    fn claim_due(
        &self,
        request: DispatchClaimRequest,
        ctx: &ExecutionContext,
    ) -> AppResult<Vec<ClaimedDispatchWork>> {
        Self::require_queue_supported(request.queue(), ctx)?;
        match request.queue() {
            DispatchQueue::Execution => {
                let request = request.as_execution_request().ok_or_else(|| {
                    AppError::unexpected(
                        "dispatch_claim_queue_mismatch",
                        "execution dispatch claim request did not convert",
                        ctx.correlation_id.clone(),
                    )
                })?;
                LeasedExecutions::claim_available(self, request, ctx).map(|claimed| {
                    claimed
                        .into_iter()
                        .map(ClaimedDispatchWork::Execution)
                        .collect()
                })
            }
            DispatchQueue::Outbox => {
                let request = request.as_outbox_request().ok_or_else(|| {
                    AppError::unexpected(
                        "dispatch_claim_queue_mismatch",
                        "outbox dispatch claim request did not convert",
                        ctx.correlation_id.clone(),
                    )
                })?;
                LeasedOutbox::lease_available(self, request, ctx).map(|claimed| {
                    claimed
                        .into_iter()
                        .map(ClaimedDispatchWork::Outbox)
                        .collect()
                })
            }
        }
    }

    fn renew_claim(
        &self,
        renewal: DispatchLeaseRenewal,
        ctx: &ExecutionContext,
    ) -> AppResult<DispatchLease> {
        match renewal {
            DispatchLeaseRenewal::Execution(renewal) => {
                LeasedExecutions::renew_lease(self, renewal, ctx).map(DispatchLease::Execution)
            }
            DispatchLeaseRenewal::Outbox(renewal) => {
                LeasedOutbox::renew_lease(self, renewal, ctx).map(DispatchLease::Outbox)
            }
        }
    }

    fn finalize(&self, resolution: DispatchResolution, ctx: &ExecutionContext) -> AppResult<()> {
        match resolution {
            DispatchResolution::ExecutionCompleted { success, .. } => {
                LeasedExecutions::acknowledge_completion(self, success, ctx)
            }
            DispatchResolution::ExecutionRetried { retry, .. } => {
                LeasedExecutions::retry_execution(self, retry, ctx)
            }
            DispatchResolution::ExecutionDeadLettered { dead_letter, .. } => {
                LeasedExecutions::dead_letter_execution(self, dead_letter, ctx)
            }
            DispatchResolution::OutboxCompleted { success, .. } => {
                LeasedOutbox::acknowledge_delivery(self, success, ctx)
            }
            DispatchResolution::OutboxRetried { retry, .. } => {
                LeasedOutbox::retry_delivery(self, retry, ctx)
            }
            DispatchResolution::OutboxDeadLettered { dead_letter, .. } => {
                LeasedOutbox::dead_letter_delivery(self, dead_letter, ctx)
            }
        }
    }
}

impl ExpiredLeaseRecoveryStore for InMemoryExecutionWorkStore {
    fn recover_expired_leases(
        &self,
        request: ExpiredLeaseRecoveryRequest,
        ctx: &ExecutionContext,
    ) -> AppResult<ExpiredLeaseRecoverySummary> {
        Self::require_queue_supported(request.queue(), ctx)?;

        let mut state = self.lock_state()?;
        let mut summary = ExpiredLeaseRecoverySummary::default();
        match request.queue() {
            DispatchQueue::Execution => {
                let mut ids = state
                    .records
                    .iter()
                    .filter(|(_, entry)| {
                        entry
                            .lease
                            .as_ref()
                            .is_some_and(|lease| lease.is_expired_at(request.now()))
                    })
                    .map(|(id, entry)| (entry.record.available_at(), id.clone()))
                    .collect::<Vec<_>>();
                ids.sort_by_key(|(available_at, id)| (*available_at, id.clone()));
                ids.truncate(request.limit());

                for (_, id) in ids {
                    let Some(entry) = state.records.get_mut(&id) else {
                        continue;
                    };
                    if entry.record.attempt() >= request.max_attempts() {
                        entry.record = entry
                            .record
                            .clone()
                            .mark_dead_lettered(request.now(), "lease expired beyond retry limit");
                        summary.observe_dead_lettered();
                    } else {
                        entry.record = entry.record.clone().release_claim(request.now());
                        summary.observe_recovered();
                    }
                    entry.lease = None;
                }
            }
            DispatchQueue::Outbox => {
                let mut ids = state
                    .outbox
                    .iter()
                    .filter(|(_, entry)| {
                        entry
                            .lease
                            .as_ref()
                            .is_some_and(|lease| lease.is_expired_at(request.now()))
                    })
                    .map(|(id, entry)| (entry.message.available_at(), id.clone()))
                    .collect::<Vec<_>>();
                ids.sort_by_key(|(available_at, id)| (*available_at, id.clone()));
                ids.truncate(request.limit());

                for (_, id) in ids {
                    let Some(entry) = state.outbox.get_mut(&id) else {
                        continue;
                    };
                    if entry.message.attempts() >= request.max_attempts() {
                        entry.status = OutboxMessageStatus::DeadLettered;
                        entry.dead_lettered_at = Some(request.now());
                        entry.last_error = Some("lease expired beyond retry limit".to_string());
                        entry.message = entry.message.clone().mark_attempted(request.now(), None);
                        summary.observe_dead_lettered();
                    } else {
                        entry.status = OutboxMessageStatus::Pending;
                        entry.message = entry.message.clone().release_claim(request.now());
                        summary.observe_recovered();
                    }
                    entry.lease = None;
                }
            }
        }

        tracing::info!(
            event = "dispatch.lease_recovery",
            queue = request.queue().as_str(),
            trace_id = ctx.correlation_id.as_str(),
            recovered = summary.recovered(),
            dead_lettered = summary.dead_lettered()
        );

        Ok(summary)
    }
}

impl WorkerHeartbeatStore for InMemoryExecutionWorkStore {
    fn record_heartbeat(
        &self,
        heartbeat: WorkerHeartbeat,
        ctx: &ExecutionContext,
    ) -> AppResult<()> {
        heartbeat.validate(ctx)?;

        let mut state = self.lock_state()?;
        let active_leases =
            state.active_leases_for_worker(heartbeat.worker_id(), heartbeat.recorded_at());
        let heartbeat = heartbeat.with_active_leases(active_leases);
        state.heartbeats.insert(
            (
                heartbeat.scope().to_string(),
                heartbeat.worker_id().to_string(),
            ),
            heartbeat,
        );
        Ok(())
    }

    fn lookup_worker(
        &self,
        worker: WorkerVisibilityRef,
        _ctx: &ExecutionContext,
    ) -> AppResult<Option<WorkerHeartbeat>> {
        let state = self.lock_state()?;
        Ok(state.visible_heartbeat(worker.worker_id(), worker.scope(), worker.visible_at()))
    }

    fn list_visible_workers(
        &self,
        query: VisibleWorkersQuery,
        _ctx: &ExecutionContext,
    ) -> AppResult<Vec<WorkerHeartbeat>> {
        let state = self.lock_state()?;
        let mut workers = state
            .heartbeats
            .values()
            .filter(|heartbeat| heartbeat.scope() == query.scope())
            .filter_map(|heartbeat| {
                state.visible_heartbeat(
                    heartbeat.worker_id(),
                    heartbeat.scope(),
                    query.visible_at(),
                )
            })
            .collect::<Vec<_>>();

        workers.sort_by_key(|heartbeat| {
            (
                std::cmp::Reverse(heartbeat.recorded_at()),
                heartbeat.worker_id().to_string(),
            )
        });
        workers.truncate(query.limit());
        Ok(workers)
    }
}

impl DispatchStatusStore for InMemoryExecutionWorkStore {
    fn lookup_worker_snapshot(
        &self,
        worker: WorkerDispatchSnapshotRef,
        _ctx: &ExecutionContext,
    ) -> AppResult<Option<WorkerDispatchSnapshot>> {
        let state = self.lock_state()?;
        let heartbeat =
            state.snapshot_heartbeat(worker.worker_id(), worker.scope(), worker.observed_at());
        let has_queue_activity = state
            .active_execution_leases_for_worker(worker.worker_id(), worker.observed_at())
            > 0
            || state.active_outbox_leases_for_worker(worker.worker_id(), worker.observed_at()) > 0
            || state
                .stale_execution_claim_count_for_worker(worker.worker_id(), worker.observed_at())
                > 0
            || state.stale_outbox_claim_count_for_worker(worker.worker_id(), worker.observed_at())
                > 0
            || state
                .heartbeats
                .contains_key(&(worker.scope().to_string(), worker.worker_id().to_string()));

        if !has_queue_activity && heartbeat.is_none() {
            return Ok(None);
        }

        Ok(Some(
            WorkerDispatchSnapshot::new(
                worker.worker_id().to_string(),
                worker.scope().to_string(),
                worker.observed_at(),
            )
            .push_queue(state.queue_snapshot_for_worker(
                DispatchQueue::Execution,
                worker.worker_id(),
                worker.observed_at(),
            ))
            .push_queue(state.queue_snapshot_for_worker(
                DispatchQueue::Outbox,
                worker.worker_id(),
                worker.observed_at(),
            ))
            .with_optional_heartbeat(heartbeat),
        ))
    }

    fn list_worker_snapshots(
        &self,
        query: WorkerDispatchSnapshotsQuery,
        _ctx: &ExecutionContext,
    ) -> AppResult<Vec<WorkerDispatchSnapshot>> {
        let state = self.lock_state()?;
        let mut snapshots = state
            .worker_ids_for_scope(query.scope())
            .into_iter()
            .filter_map(|worker_id| {
                let heartbeat =
                    state.snapshot_heartbeat(&worker_id, query.scope(), query.observed_at());
                let snapshot = WorkerDispatchSnapshot::new(
                    worker_id.clone(),
                    query.scope().to_string(),
                    query.observed_at(),
                )
                .push_queue(state.queue_snapshot_for_worker(
                    DispatchQueue::Execution,
                    &worker_id,
                    query.observed_at(),
                ))
                .push_queue(state.queue_snapshot_for_worker(
                    DispatchQueue::Outbox,
                    &worker_id,
                    query.observed_at(),
                ))
                .with_optional_heartbeat(heartbeat);

                if !query.include_expired_workers()
                    && snapshot.visibility() == crate::WorkerVisibility::Expired
                {
                    None
                } else {
                    Some(snapshot)
                }
            })
            .collect::<Vec<_>>();

        snapshots.sort_by_key(|snapshot| {
            (
                std::cmp::Reverse(snapshot.observed_at()),
                snapshot.worker_id().to_string(),
            )
        });
        snapshots.truncate(query.limit());
        Ok(snapshots)
    }
}

trait WorkerDispatchSnapshotExt {
    fn with_optional_heartbeat(self, heartbeat: Option<WorkerHeartbeat>) -> Self;
}

impl WorkerDispatchSnapshotExt for WorkerDispatchSnapshot {
    fn with_optional_heartbeat(mut self, heartbeat: Option<WorkerHeartbeat>) -> Self {
        if let Some(heartbeat) = heartbeat {
            self = self.with_heartbeat(heartbeat);
        }
        self
    }
}

#[cfg(test)]
mod tests;
