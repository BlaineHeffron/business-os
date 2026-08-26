use std::time::SystemTime;

use crate::{AppResult, ExecutionContext, ExecutionId, LeaseId, OutboxMessageId};

use super::{OutboxDeliveryLease, OutboxMessage};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboxMessageStatus {
    Pending,
    InFlight,
    Delivered,
    DeadLettered,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxInspectionRecord {
    message: OutboxMessage,
    status: OutboxMessageStatus,
    lease: Option<OutboxDeliveryLease>,
    delivered_at: Option<SystemTime>,
    dead_lettered_at: Option<SystemTime>,
    last_error: Option<String>,
}

impl OutboxInspectionRecord {
    pub fn new(message: OutboxMessage, status: OutboxMessageStatus) -> Self {
        Self {
            message,
            status,
            lease: None,
            delivered_at: None,
            dead_lettered_at: None,
            last_error: None,
        }
    }

    pub fn message_id(&self) -> &OutboxMessageId {
        self.message.id()
    }

    pub fn aggregate(&self) -> &str {
        self.message.aggregate()
    }

    pub fn aggregate_id(&self) -> &str {
        self.message.aggregate_id()
    }

    pub fn execution_id(&self) -> &ExecutionId {
        self.message.execution_id()
    }

    pub fn topic(&self) -> &str {
        self.message.envelope().topic()
    }

    pub fn key(&self) -> &str {
        self.message.envelope().key()
    }

    pub fn content_type(&self) -> &str {
        self.message.envelope().content_type()
    }

    pub fn attempts(&self) -> u32 {
        self.message.attempts()
    }

    pub fn available_at(&self) -> SystemTime {
        self.message.available_at()
    }

    pub fn last_attempted_at(&self) -> Option<SystemTime> {
        self.message.last_attempted_at()
    }

    pub fn message(&self) -> &OutboxMessage {
        &self.message
    }

    pub fn status(&self) -> OutboxMessageStatus {
        self.status
    }

    pub fn lease(&self) -> Option<&OutboxDeliveryLease> {
        self.lease.as_ref()
    }

    pub fn delivered_at(&self) -> Option<SystemTime> {
        self.delivered_at
    }

    pub fn dead_lettered_at(&self) -> Option<SystemTime> {
        self.dead_lettered_at
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn with_lease(mut self, lease: Option<OutboxDeliveryLease>) -> Self {
        self.lease = lease;
        self
    }

    pub fn with_delivered_at(mut self, delivered_at: Option<SystemTime>) -> Self {
        self.delivered_at = delivered_at;
        self
    }

    pub fn with_dead_lettered_at(mut self, dead_lettered_at: Option<SystemTime>) -> Self {
        self.dead_lettered_at = dead_lettered_at;
        self
    }

    pub fn with_last_error(mut self, last_error: Option<String>) -> Self {
        self.last_error = last_error;
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
            self.status,
            OutboxMessageStatus::Delivered | OutboxMessageStatus::DeadLettered
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
        self.status == OutboxMessageStatus::Pending && self.message.available_at() <= observed_at
    }

    pub fn is_retry_scheduled_at(&self, observed_at: SystemTime) -> bool {
        self.status == OutboxMessageStatus::Pending
            && self.message.attempts() > 0
            && self.message.available_at() > observed_at
    }

    pub fn snapshot_at(&self, observed_at: SystemTime) -> OutboxInspectionSnapshot {
        OutboxInspectionSnapshot {
            message_id: self.message.id().clone(),
            aggregate: self.message.aggregate().to_string(),
            aggregate_id: self.message.aggregate_id().to_string(),
            execution_id: self.message.execution_id().clone(),
            topic: self.message.envelope().topic().to_string(),
            key: self.message.envelope().key().to_string(),
            content_type: self.message.envelope().content_type().to_string(),
            status: self.status,
            attempts: self.message.attempts(),
            available_at: self.message.available_at(),
            last_attempted_at: self.message.last_attempted_at(),
            delivered_at: self.delivered_at,
            dead_lettered_at: self.dead_lettered_at,
            last_error: self.last_error.clone(),
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
pub struct OutboxInspectionQuery {
    observed_at: SystemTime,
    limit: usize,
    status: Option<OutboxMessageStatus>,
    aggregate: Option<String>,
    topic: Option<String>,
}

impl OutboxInspectionQuery {
    pub fn new(limit: usize) -> Self {
        Self {
            observed_at: SystemTime::now(),
            limit: limit.max(1),
            status: None,
            aggregate: None,
            topic: None,
        }
    }

    pub fn observed_at(&self) -> SystemTime {
        self.observed_at
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    pub fn status(&self) -> Option<OutboxMessageStatus> {
        self.status
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

    pub fn with_status(mut self, status: OutboxMessageStatus) -> Self {
        self.status = Some(status);
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

    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit.max(1);
        self
    }

    pub fn matches_message(&self, message: &OutboxMessage, status: OutboxMessageStatus) -> bool {
        self.status
            .map(|expected| status == expected)
            .unwrap_or(true)
            && self
                .aggregate
                .as_deref()
                .map(|aggregate| message.aggregate() == aggregate)
                .unwrap_or(true)
            && self
                .topic
                .as_deref()
                .map(|topic| message.envelope().topic() == topic)
                .unwrap_or(true)
    }

    pub fn matches_inspection(&self, record: &OutboxInspectionRecord) -> bool {
        self.matches_message(record.message(), record.status())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxInspectionSnapshot {
    message_id: OutboxMessageId,
    aggregate: String,
    aggregate_id: String,
    execution_id: ExecutionId,
    topic: String,
    key: String,
    content_type: String,
    status: OutboxMessageStatus,
    attempts: u32,
    available_at: SystemTime,
    last_attempted_at: Option<SystemTime>,
    delivered_at: Option<SystemTime>,
    dead_lettered_at: Option<SystemTime>,
    last_error: Option<String>,
    lease_id: Option<LeaseId>,
    leased_by: Option<String>,
    lease_expires_at: Option<SystemTime>,
    active_lease: bool,
    stale_lease: bool,
    due: bool,
    retry_scheduled: bool,
}

impl OutboxInspectionSnapshot {
    pub fn message_id(&self) -> &OutboxMessageId {
        &self.message_id
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

    pub fn topic(&self) -> &str {
        &self.topic
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn content_type(&self) -> &str {
        &self.content_type
    }

    pub fn status(&self) -> OutboxMessageStatus {
        self.status
    }

    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    pub fn available_at(&self) -> SystemTime {
        self.available_at
    }

    pub fn last_attempted_at(&self) -> Option<SystemTime> {
        self.last_attempted_at
    }

    pub fn delivered_at(&self) -> Option<SystemTime> {
        self.delivered_at
    }

    pub fn dead_lettered_at(&self) -> Option<SystemTime> {
        self.dead_lettered_at
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
            OutboxMessageStatus::Delivered | OutboxMessageStatus::DeadLettered
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OutboxInspectionSummary {
    total: usize,
    pending: usize,
    in_flight: usize,
    delivered: usize,
    dead_lettered: usize,
    due: usize,
    retry_scheduled: usize,
    leased: usize,
    stale_leases: usize,
    oldest_due_at: Option<SystemTime>,
}

impl OutboxInspectionSummary {
    pub fn total(&self) -> usize {
        self.total
    }

    pub fn pending(&self) -> usize {
        self.pending
    }

    pub fn in_flight(&self) -> usize {
        self.in_flight
    }

    pub fn delivered(&self) -> usize {
        self.delivered
    }

    pub fn dead_lettered(&self) -> usize {
        self.dead_lettered
    }

    pub fn due(&self) -> usize {
        self.due
    }

    pub fn retry_scheduled(&self) -> usize {
        self.retry_scheduled
    }

    pub fn leased(&self) -> usize {
        self.leased
    }

    pub fn stale_leases(&self) -> usize {
        self.stale_leases
    }

    pub fn oldest_due_at(&self) -> Option<SystemTime> {
        self.oldest_due_at
    }

    pub fn observe(mut self, record: &OutboxInspectionRecord, observed_at: SystemTime) -> Self {
        self.total = self.total.saturating_add(1);
        match record.status() {
            OutboxMessageStatus::Pending => self.pending = self.pending.saturating_add(1),
            OutboxMessageStatus::InFlight => self.in_flight = self.in_flight.saturating_add(1),
            OutboxMessageStatus::Delivered => self.delivered = self.delivered.saturating_add(1),
            OutboxMessageStatus::DeadLettered => {
                self.dead_lettered = self.dead_lettered.saturating_add(1)
            }
        }

        if record.has_active_lease_at(observed_at) {
            self.leased = self.leased.saturating_add(1);
        }

        if record.has_stale_lease_at(observed_at) {
            self.stale_leases = self.stale_leases.saturating_add(1);
        }

        if record.is_due_at(observed_at) {
            self.due = self.due.saturating_add(1);
            self.oldest_due_at = Some(
                self.oldest_due_at
                    .map(|oldest| oldest.min(record.message().available_at()))
                    .unwrap_or(record.message().available_at()),
            );
        }

        if record.is_retry_scheduled_at(observed_at) {
            self.retry_scheduled = self.retry_scheduled.saturating_add(1);
        }

        self
    }
}

pub trait OutboxInspectionStore: Send + Sync {
    fn lookup_outbox_message(
        &self,
        message_id: &OutboxMessageId,
        observed_at: SystemTime,
        ctx: &ExecutionContext,
    ) -> AppResult<Option<OutboxInspectionRecord>>;

    fn list_outbox_messages(
        &self,
        query: OutboxInspectionQuery,
        ctx: &ExecutionContext,
    ) -> AppResult<Vec<OutboxInspectionRecord>>;

    fn summarize_outbox(
        &self,
        query: OutboxInspectionQuery,
        ctx: &ExecutionContext,
    ) -> AppResult<OutboxInspectionSummary>;
}
