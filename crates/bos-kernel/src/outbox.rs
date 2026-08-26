use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime};

use crate::execution::validate_scope;
use crate::{
    AppError, AppResult, ExecutionContext, ExecutionId, ExecutionRecord, IdempotencyClaim, LeaseId,
    OutboxMessageId, RevisionToken,
};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OutboxEnvelope {
    topic: String,
    key: String,
    payload: String,
    content_type: String,
    headers: Vec<(String, String)>,
}

impl OutboxEnvelope {
    pub fn new(
        topic: impl Into<String>,
        key: impl Into<String>,
        payload: impl Into<String>,
    ) -> Self {
        Self {
            topic: topic.into(),
            key: key.into(),
            payload: payload.into(),
            content_type: "application/json".to_string(),
            headers: Vec::new(),
        }
    }

    pub fn with_content_type(mut self, content_type: impl Into<String>) -> Self {
        self.content_type = content_type.into();
        self
    }

    pub fn with_header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((key.into(), value.into()));
        self
    }

    pub fn topic(&self) -> &str {
        &self.topic
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn payload(&self) -> &str {
        &self.payload
    }

    pub fn content_type(&self) -> &str {
        &self.content_type
    }

    pub fn headers(&self) -> &[(String, String)] {
        &self.headers
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxMessage {
    id: OutboxMessageId,
    aggregate: String,
    aggregate_id: String,
    execution_id: ExecutionId,
    envelope: OutboxEnvelope,
    available_at: SystemTime,
    attempts: u32,
    last_attempted_at: Option<SystemTime>,
    revision: RevisionToken,
}

impl OutboxMessage {
    pub fn new(
        id: OutboxMessageId,
        aggregate: impl Into<String>,
        aggregate_id: impl Into<String>,
        execution_id: ExecutionId,
        envelope: OutboxEnvelope,
    ) -> Self {
        Self {
            id,
            aggregate: aggregate.into(),
            aggregate_id: aggregate_id.into(),
            execution_id,
            envelope,
            available_at: SystemTime::now(),
            attempts: 0,
            last_attempted_at: None,
            revision: RevisionToken::initial(),
        }
    }

    pub fn id(&self) -> &OutboxMessageId {
        &self.id
    }

    pub fn aggregate(&self) -> &str {
        &self.aggregate
    }

    pub fn aggregate_id(&self) -> &str {
        &self.aggregate_id
    }

    pub fn execution_id(&self) -> &ExecutionId {
        &self.execution_id
    }

    pub fn envelope(&self) -> &OutboxEnvelope {
        &self.envelope
    }

    pub fn available_at(&self) -> SystemTime {
        self.available_at
    }

    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    pub fn last_attempted_at(&self) -> Option<SystemTime> {
        self.last_attempted_at
    }

    pub fn revision(&self) -> RevisionToken {
        self.revision
    }

    pub fn schedule_at(mut self, available_at: SystemTime) -> Self {
        self.available_at = available_at;
        self
    }

    pub fn with_revision(mut self, revision: RevisionToken) -> Self {
        self.revision = revision;
        self
    }

    pub fn mark_attempted(
        mut self,
        attempted_at: SystemTime,
        available_at: Option<SystemTime>,
    ) -> Self {
        self.attempts = self.attempts.saturating_add(1);
        self.last_attempted_at = Some(attempted_at);
        if let Some(available_at) = available_at {
            self.available_at = available_at;
        }
        self.revision = self.revision.next();
        self
    }

    pub fn mark_claimed(mut self) -> Self {
        self.revision = self.revision.next();
        self
    }

    pub fn release_claim(mut self, available_at: SystemTime) -> Self {
        self.available_at = available_at;
        self.revision = self.revision.next();
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxDeliveryLease {
    lease_id: LeaseId,
    leased_by: String,
    leased_at: SystemTime,
    lease_expires_at: SystemTime,
}

impl OutboxDeliveryLease {
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
pub struct OutboxDeliveryLeaseRequest {
    leased_by: String,
    batch_size: usize,
    lease_ttl: Duration,
    now: SystemTime,
}

impl OutboxDeliveryLeaseRequest {
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
pub struct DueOutboxMessageRequest {
    due_before: SystemTime,
    limit: usize,
    include_expired_leases: bool,
}

impl DueOutboxMessageRequest {
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
pub struct OutboxDeliveryLeaseRenewal {
    message_id: OutboxMessageId,
    lease_id: LeaseId,
    leased_by: String,
    renewed_at: SystemTime,
    lease_ttl: Duration,
}

impl OutboxDeliveryLeaseRenewal {
    pub fn new(
        message_id: OutboxMessageId,
        lease_id: LeaseId,
        leased_by: impl Into<String>,
        lease_ttl: Duration,
    ) -> Self {
        Self {
            message_id,
            lease_id,
            leased_by: leased_by.into(),
            renewed_at: SystemTime::now(),
            lease_ttl,
        }
    }

    pub fn message_id(&self) -> &OutboxMessageId {
        &self.message_id
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

    pub fn renewed_lease(&self) -> OutboxDeliveryLease {
        OutboxDeliveryLease::from_now(
            self.lease_id.clone(),
            self.leased_by.clone(),
            self.lease_ttl,
            self.renewed_at,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxDeliverySuccess {
    message_id: OutboxMessageId,
    lease_id: LeaseId,
    delivered_at: SystemTime,
    expected_revision: Option<RevisionToken>,
}

impl OutboxDeliverySuccess {
    pub fn new(message_id: OutboxMessageId, lease_id: LeaseId, delivered_at: SystemTime) -> Self {
        Self {
            message_id,
            lease_id,
            delivered_at,
            expected_revision: None,
        }
    }

    pub fn message_id(&self) -> &OutboxMessageId {
        &self.message_id
    }

    pub fn lease_id(&self) -> &LeaseId {
        &self.lease_id
    }

    pub fn delivered_at(&self) -> SystemTime {
        self.delivered_at
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
pub struct OutboxDeliveryRetry {
    message_id: OutboxMessageId,
    lease_id: LeaseId,
    attempted_at: SystemTime,
    next_available_at: SystemTime,
    error: String,
    expected_revision: Option<RevisionToken>,
}

impl OutboxDeliveryRetry {
    pub fn new(
        message_id: OutboxMessageId,
        lease_id: LeaseId,
        attempted_at: SystemTime,
        next_available_at: SystemTime,
        error: impl Into<String>,
    ) -> Self {
        Self {
            message_id,
            lease_id,
            attempted_at,
            next_available_at,
            error: error.into(),
            expected_revision: None,
        }
    }

    pub fn message_id(&self) -> &OutboxMessageId {
        &self.message_id
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
pub struct OutboxDeliveryDeadLetter {
    message_id: OutboxMessageId,
    lease_id: LeaseId,
    dead_lettered_at: SystemTime,
    error: String,
    expected_revision: Option<RevisionToken>,
}

impl OutboxDeliveryDeadLetter {
    pub fn new(
        message_id: OutboxMessageId,
        lease_id: LeaseId,
        dead_lettered_at: SystemTime,
        error: impl Into<String>,
    ) -> Self {
        Self {
            message_id,
            lease_id,
            dead_lettered_at,
            error: error.into(),
            expected_revision: None,
        }
    }

    pub fn message_id(&self) -> &OutboxMessageId {
        &self.message_id
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
pub struct ClaimedOutboxDelivery {
    lease: OutboxDeliveryLease,
    message: OutboxMessage,
}

impl ClaimedOutboxDelivery {
    pub fn claim(message: OutboxMessage, lease: OutboxDeliveryLease) -> AppResult<Self> {
        if lease.leased_by().trim().is_empty() {
            return Err(AppError::new(
                crate::ErrorCode::InvalidInput,
                "outbox_lease_owner_required",
                "outbox leases require a non-empty worker identity",
                crate::CorrelationId::generate(),
            ));
        }

        if message.available_at() > lease.leased_at() {
            return Err(AppError::new(
                crate::ErrorCode::InvalidState,
                "outbox_not_available_for_claim",
                "outbox messages cannot be claimed before they are available",
                crate::CorrelationId::generate(),
            ));
        }

        Ok(Self {
            lease,
            message: message.mark_claimed(),
        })
    }

    pub fn lease(&self) -> &OutboxDeliveryLease {
        &self.lease
    }

    pub fn message(&self) -> &OutboxMessage {
        &self.message
    }

    pub fn into_parts(self) -> (OutboxDeliveryLease, OutboxMessage) {
        (self.lease, self.message)
    }

    pub fn replace_lease(mut self, lease: OutboxDeliveryLease) -> AppResult<Self> {
        if lease.leased_by().trim().is_empty() {
            return Err(AppError::new(
                crate::ErrorCode::InvalidInput,
                "outbox_lease_owner_required",
                "outbox leases require a non-empty worker identity",
                crate::CorrelationId::generate(),
            ));
        }
        self.lease = lease;
        Ok(self)
    }

    pub fn complete(self, delivered_at: SystemTime) -> (OutboxMessage, OutboxDeliverySuccess) {
        let success = OutboxDeliverySuccess::new(
            self.message.id().clone(),
            self.lease.lease_id().clone(),
            delivered_at,
        )
        .with_expected_revision(self.message.revision());
        let message = self.message.mark_attempted(delivered_at, None);
        (message, success)
    }

    pub fn retry(
        self,
        attempted_at: SystemTime,
        next_available_at: SystemTime,
        error: impl Into<String>,
    ) -> AppResult<(OutboxMessage, OutboxDeliveryRetry)> {
        if next_available_at < attempted_at {
            return Err(AppError::new(
                crate::ErrorCode::InvalidInput,
                "outbox_retry_before_attempt",
                "outbox retries require next availability at or after the attempt time",
                crate::CorrelationId::generate(),
            ));
        }

        let error = error.into();
        if error.trim().is_empty() {
            return Err(AppError::new(
                crate::ErrorCode::InvalidInput,
                "outbox_retry_error_required",
                "outbox retries require a failure reason",
                crate::CorrelationId::generate(),
            ));
        }

        let retry = OutboxDeliveryRetry::new(
            self.message.id().clone(),
            self.lease.lease_id().clone(),
            attempted_at,
            next_available_at,
            error,
        )
        .with_expected_revision(self.message.revision());
        let message = self
            .message
            .mark_attempted(attempted_at, Some(next_available_at));
        Ok((message, retry))
    }

    pub fn dead_letter(
        self,
        dead_lettered_at: SystemTime,
        error: impl Into<String>,
    ) -> AppResult<(OutboxMessage, OutboxDeliveryDeadLetter)> {
        let error = error.into();
        if error.trim().is_empty() {
            return Err(AppError::new(
                crate::ErrorCode::InvalidInput,
                "outbox_dead_letter_error_required",
                "dead-lettering outbox work requires a failure reason",
                crate::CorrelationId::generate(),
            ));
        }

        let dead_letter = OutboxDeliveryDeadLetter::new(
            self.message.id().clone(),
            self.lease.lease_id().clone(),
            dead_lettered_at,
            error,
        )
        .with_expected_revision(self.message.revision());
        let message = self.message.mark_attempted(dead_lettered_at, None);
        Ok((message, dead_letter))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PendingOutbox {
    messages: Vec<OutboxMessage>,
}

impl PendingOutbox {
    pub fn push(&mut self, message: OutboxMessage) {
        self.messages.push(message);
    }

    pub fn drain(&mut self) -> Vec<OutboxMessage> {
        std::mem::take(&mut self.messages)
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    pub fn messages(&self) -> &[OutboxMessage] {
        &self.messages
    }
}

pub trait OutboxPublisher: Send + Sync {
    fn publish(&self, batch: Vec<OutboxMessage>) -> AppResult<()>;
}

pub trait LeasedOutbox: Send + Sync {
    fn select_due(
        &self,
        request: DueOutboxMessageRequest,
        ctx: &ExecutionContext,
    ) -> AppResult<Vec<OutboxMessage>>;

    fn lease_available(
        &self,
        request: OutboxDeliveryLeaseRequest,
        ctx: &ExecutionContext,
    ) -> AppResult<Vec<ClaimedOutboxDelivery>>;

    fn renew_lease(
        &self,
        renewal: OutboxDeliveryLeaseRenewal,
        ctx: &ExecutionContext,
    ) -> AppResult<OutboxDeliveryLease>;

    fn acknowledge_delivery(
        &self,
        success: OutboxDeliverySuccess,
        ctx: &ExecutionContext,
    ) -> AppResult<()>;

    fn retry_delivery(&self, retry: OutboxDeliveryRetry, ctx: &ExecutionContext) -> AppResult<()>;

    fn dead_letter_delivery(
        &self,
        dead_letter: OutboxDeliveryDeadLetter,
        ctx: &ExecutionContext,
    ) -> AppResult<()>;
}

mod inspection;

pub use inspection::{
    OutboxInspectionQuery, OutboxInspectionRecord, OutboxInspectionSnapshot, OutboxInspectionStore,
    OutboxInspectionSummary, OutboxMessageStatus,
};

impl OutboxDeliveryLeaseRenewal {
    pub fn validate(&self, ctx: &ExecutionContext) -> AppResult<()> {
        if self.leased_by.trim().is_empty() {
            return Err(AppError::from_context(
                crate::ErrorCode::InvalidInput,
                "outbox_lease_renewal_owner_required",
                "outbox lease renewals require a non-empty worker identity",
                ctx,
            ));
        }

        if self.lease_ttl.is_zero() {
            return Err(AppError::from_context(
                crate::ErrorCode::InvalidInput,
                "outbox_lease_renewal_ttl_required",
                "outbox lease renewals require a positive lease ttl",
                ctx,
            ));
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TransactionalWrite<E, U = String> {
    aggregate_updates: Vec<U>,
    domain_events: Vec<E>,
    idempotency_claims: Vec<IdempotencyClaim>,
    execution_records: Vec<ExecutionRecord>,
    outbox_messages: Vec<OutboxMessage>,
}

impl<E, U> TransactionalWrite<E, U> {
    pub fn push_aggregate_update(&mut self, update: U) {
        self.aggregate_updates.push(update);
    }

    pub fn push_domain_event(&mut self, event: E) {
        self.domain_events.push(event);
    }

    pub fn push_idempotency_claim(&mut self, claim: IdempotencyClaim) {
        self.idempotency_claims.push(claim);
    }

    pub fn push_execution_record(&mut self, record: ExecutionRecord) {
        self.execution_records.push(record);
    }

    pub fn push_outbox_message(&mut self, message: OutboxMessage) {
        self.outbox_messages.push(message);
    }

    pub fn is_empty(&self) -> bool {
        self.aggregate_updates.is_empty()
            && self.domain_events.is_empty()
            && self.idempotency_claims.is_empty()
            && self.execution_records.is_empty()
            && self.outbox_messages.is_empty()
    }

    pub fn validate(self, ctx: &ExecutionContext) -> AppResult<ValidatedTransactionalWrite<E, U>> {
        if !self.outbox_messages.is_empty() && self.execution_records.is_empty() {
            return Err(AppError::from_context(
                crate::ErrorCode::InvalidState,
                "outbox_requires_execution_record",
                "outbox messages require at least one execution record",
                ctx,
            ));
        }

        let mut claim_keys = HashSet::new();
        let mut execution_ids = HashSet::new();
        let mut idempotency_keys = HashSet::new();
        let mut execution_by_id = HashMap::new();
        let mut outbox_message_ids = HashSet::new();

        for claim in &self.idempotency_claims {
            validate_scope(claim.scope(), ctx)?;
            if !claim_keys.insert(claim.key().clone()) {
                return Err(AppError::from_context(
                    crate::ErrorCode::Conflict,
                    "duplicate_idempotency_claim",
                    "transactional writes cannot contain duplicate idempotency claims",
                    ctx,
                ));
            }
        }

        for record in &self.execution_records {
            require_non_empty(
                record.operation(),
                "execution operation",
                "execution_operation_required",
                ctx,
            )?;
            require_non_empty(
                record.target(),
                "execution target",
                "execution_target_required",
                ctx,
            )?;

            if record.correlation_id() != &ctx.correlation_id {
                return Err(AppError::from_context(
                    crate::ErrorCode::InvalidState,
                    "execution_record_correlation_mismatch",
                    "execution records must use the current execution context correlation id",
                    ctx,
                ));
            }

            if !execution_ids.insert(record.id().clone()) {
                return Err(AppError::from_context(
                    crate::ErrorCode::Conflict,
                    "duplicate_execution_record",
                    "transactional writes cannot contain duplicate execution record ids",
                    ctx,
                ));
            }

            if !idempotency_keys.insert(record.idempotency_key().clone()) {
                return Err(AppError::from_context(
                    crate::ErrorCode::Conflict,
                    "duplicate_execution_idempotency_key",
                    "transactional writes cannot contain multiple execution records for one idempotency key",
                    ctx,
                ));
            }

            let has_claim = self
                .idempotency_claims
                .iter()
                .any(|claim| claim.key() == record.idempotency_key());
            let matches_context = ctx
                .idempotency_key()
                .is_some_and(|key| key == record.idempotency_key());

            if !has_claim && !matches_context {
                return Err(AppError::from_context(
                    crate::ErrorCode::InvalidState,
                    "execution_record_missing_idempotency_claim",
                    "execution records require a matching idempotency claim or execution context key",
                    ctx,
                ));
            }

            execution_by_id.insert(record.id().clone(), record);
        }

        let mut outbox_execution_ids = HashSet::new();
        for message in &self.outbox_messages {
            if !outbox_message_ids.insert(message.id().clone()) {
                return Err(AppError::from_context(
                    crate::ErrorCode::Conflict,
                    "duplicate_outbox_message",
                    "transactional writes cannot contain duplicate outbox message ids",
                    ctx,
                ));
            }

            require_non_empty(
                message.aggregate(),
                "outbox aggregate",
                "outbox_aggregate_required",
                ctx,
            )?;
            require_non_empty(
                message.aggregate_id(),
                "outbox aggregate id",
                "outbox_aggregate_id_required",
                ctx,
            )?;
            require_non_empty(
                message.envelope().topic(),
                "outbox topic",
                "outbox_topic_required",
                ctx,
            )?;
            require_non_empty(
                message.envelope().key(),
                "outbox key",
                "outbox_key_required",
                ctx,
            )?;
            require_non_empty(
                message.envelope().payload(),
                "outbox payload",
                "outbox_payload_required",
                ctx,
            )?;

            let Some(record) = execution_by_id.get(message.execution_id()) else {
                return Err(AppError::from_context(
                    crate::ErrorCode::InvalidState,
                    "outbox_execution_record_missing",
                    "outbox messages require a matching execution record",
                    ctx,
                ));
            };

            if !outbox_execution_ids.insert(message.execution_id().clone()) {
                return Err(AppError::from_context(
                    crate::ErrorCode::Conflict,
                    "outbox_execution_record_reused",
                    "each outbox message requires its own execution record",
                    ctx,
                ));
            }

            let has_claim = self
                .idempotency_claims
                .iter()
                .any(|claim| claim.key() == record.idempotency_key());
            let matches_context = ctx
                .idempotency_key()
                .is_some_and(|key| key == record.idempotency_key());

            if !has_claim && !matches_context {
                return Err(AppError::from_context(
                    crate::ErrorCode::InvalidState,
                    "outbox_missing_idempotency_claim",
                    "outbox messages require execution records backed by an idempotency claim or context key",
                    ctx,
                ));
            }
        }

        Ok(ValidatedTransactionalWrite(self))
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
            crate::ErrorCode::InvalidInput,
            code,
            format!("{field_name} must not be empty"),
            ctx,
        ));
    }

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedTransactionalWrite<E, U = String>(TransactionalWrite<E, U>);

impl<E, U> ValidatedTransactionalWrite<E, U> {
    pub fn aggregate_updates(&self) -> &[U] {
        &self.0.aggregate_updates
    }

    pub fn domain_events(&self) -> &[E] {
        &self.0.domain_events
    }

    pub fn idempotency_claims(&self) -> &[IdempotencyClaim] {
        &self.0.idempotency_claims
    }

    pub fn execution_records(&self) -> &[ExecutionRecord] {
        &self.0.execution_records
    }

    pub fn outbox_messages(&self) -> &[OutboxMessage] {
        &self.0.outbox_messages
    }

    pub fn into_inner(self) -> TransactionalWrite<E, U> {
        self.0
    }
}

pub trait TransactionalOutbox: Send + Sync {
    type Event;
    type AggregateUpdate;

    fn commit(
        &self,
        write: ValidatedTransactionalWrite<Self::Event, Self::AggregateUpdate>,
        ctx: &ExecutionContext,
    ) -> AppResult<()>;
}

#[cfg(test)]
mod tests;
