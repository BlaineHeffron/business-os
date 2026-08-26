use std::time::{Duration, SystemTime};

use crate::{
    CorrelationId, ExecutionContext, ExecutionId, IdempotencyClaim, IdempotencyKey, LeaseId,
    OutboxMessageId, OutboxMessageStatus,
};

use super::{
    ClaimedOutboxDelivery, DueOutboxMessageRequest, OutboxDeliveryLease,
    OutboxDeliveryLeaseRenewal, OutboxDeliveryLeaseRequest, OutboxDeliveryRetry, OutboxEnvelope,
    OutboxInspectionQuery, OutboxInspectionRecord, OutboxMessage, PendingOutbox,
    TransactionalWrite,
};

#[test]
fn pending_outbox_drains_messages() {
    let mut outbox = PendingOutbox::default();
    outbox.push(OutboxMessage::new(
        OutboxMessageId::new("msg_1"),
        "thread",
        "thr_1",
        ExecutionId::new("exec_1"),
        OutboxEnvelope::new("agent_bus.message_created", "thr_1", "{}"),
    ));

    assert_eq!(outbox.len(), 1);
    assert_eq!(outbox.drain().len(), 1);
    assert!(outbox.is_empty());
}

#[test]
fn outbox_message_tracks_attempt_timestamps() {
    let attempted_at = SystemTime::UNIX_EPOCH + Duration::from_secs(5);
    let available_at = SystemTime::UNIX_EPOCH + Duration::from_secs(30);
    let message = OutboxMessage::new(
        OutboxMessageId::new("msg_2"),
        "thread",
        "thr_2",
        ExecutionId::new("exec_2"),
        OutboxEnvelope::new("agent_bus.message_created", "thr_2", "{}"),
    )
    .mark_attempted(attempted_at, Some(available_at));

    assert_eq!(message.attempts(), 1);
    assert_eq!(message.last_attempted_at(), Some(attempted_at));
    assert_eq!(message.available_at(), available_at);
}

#[test]
fn delivery_lease_request_clamps_batch_size() {
    let request = OutboxDeliveryLeaseRequest::new("worker-1", 0, Duration::from_secs(15));
    assert_eq!(request.batch_size(), 1);
}

#[test]
fn due_outbox_request_clamps_limit() {
    let due_before = SystemTime::UNIX_EPOCH + Duration::from_secs(18);
    let request = DueOutboxMessageRequest::new(0).at(due_before);

    assert_eq!(request.limit(), 1);
    assert_eq!(request.due_before(), due_before);
    assert!(request.include_expired_leases());
}

#[test]
fn delivery_lease_computes_expiration_from_now() {
    let leased_at = SystemTime::UNIX_EPOCH + Duration::from_secs(20);
    let lease = OutboxDeliveryLease::from_now(
        LeaseId::new("lease_1"),
        "worker-1",
        Duration::from_secs(30),
        leased_at,
    );

    assert_eq!(lease.leased_at(), leased_at);
    assert_eq!(
        lease.lease_expires_at(),
        SystemTime::UNIX_EPOCH + Duration::from_secs(50)
    );
}

#[test]
fn delivery_lease_can_be_renewed() {
    let renewed_at = SystemTime::UNIX_EPOCH + Duration::from_secs(35);
    let lease = OutboxDeliveryLease::from_now(
        LeaseId::new("lease_outbox_renew_1"),
        "worker-outbox-renew",
        Duration::from_secs(10),
        SystemTime::UNIX_EPOCH + Duration::from_secs(5),
    )
    .renew(renewed_at, Duration::from_secs(20));

    assert_eq!(lease.leased_at(), renewed_at);
    assert_eq!(
        lease.lease_expires_at(),
        renewed_at + Duration::from_secs(20)
    );
    assert!(!lease.is_expired_at(renewed_at + Duration::from_secs(19)));
    assert!(lease.is_expired_at(renewed_at + Duration::from_secs(21)));
}

#[test]
fn delivery_lease_renewal_builds_next_lease_window() {
    let renewed_at = SystemTime::UNIX_EPOCH + Duration::from_secs(40);
    let renewal = OutboxDeliveryLeaseRenewal::new(
        OutboxMessageId::new("msg_renew_1"),
        LeaseId::new("lease_outbox_renew_2"),
        "worker-outbox-renew-2",
        Duration::from_secs(15),
    )
    .at(renewed_at);
    let lease = renewal.renewed_lease();

    assert_eq!(renewal.message_id().as_str(), "msg_renew_1");
    assert_eq!(renewal.lease_id().as_str(), "lease_outbox_renew_2");
    assert_eq!(renewal.leased_by(), "worker-outbox-renew-2");
    assert_eq!(renewal.renewed_at(), renewed_at);
    assert_eq!(renewal.lease_ttl(), Duration::from_secs(15));
    assert_eq!(lease.leased_at(), renewed_at);
    assert_eq!(
        lease.lease_expires_at(),
        renewed_at + Duration::from_secs(15)
    );
}

#[test]
fn delivery_lease_renewal_requires_owner_and_ttl() {
    let ctx = ExecutionContext::new(CorrelationId::new("corr_outbox_renew_1"));
    let missing_owner = OutboxDeliveryLeaseRenewal::new(
        OutboxMessageId::new("msg_renew_2"),
        LeaseId::new("lease_outbox_renew_3"),
        " ",
        Duration::from_secs(10),
    );
    let missing_ttl = OutboxDeliveryLeaseRenewal::new(
        OutboxMessageId::new("msg_renew_3"),
        LeaseId::new("lease_outbox_renew_4"),
        "worker-outbox-renew-3",
        Duration::ZERO,
    );

    assert_eq!(
        missing_owner
            .validate(&ctx)
            .expect_err("owner should be required")
            .code(),
        "outbox_lease_renewal_owner_required"
    );
    assert_eq!(
        missing_ttl
            .validate(&ctx)
            .expect_err("ttl should be required")
            .code(),
        "outbox_lease_renewal_ttl_required"
    );
}

#[test]
fn delivery_retry_exposes_backoff_details() {
    let attempted_at = SystemTime::UNIX_EPOCH + Duration::from_secs(40);
    let retry = OutboxDeliveryRetry::new(
        OutboxMessageId::new("msg_3"),
        LeaseId::new("lease_2"),
        attempted_at,
        attempted_at + Duration::from_secs(60),
        "timeout",
    );

    assert_eq!(retry.error(), "timeout");
    assert_eq!(retry.attempted_at(), attempted_at);
}

#[test]
fn outbox_inspection_queries_match_records_and_snapshots() {
    let base = SystemTime::UNIX_EPOCH + Duration::from_secs(90);
    let record = OutboxInspectionRecord::new(
        OutboxMessage::new(
            OutboxMessageId::new("msg_query"),
            "thread",
            "thread_1",
            ExecutionId::new("exec_query"),
            OutboxEnvelope::new("agent_bus", "thread_1", "{\"ok\":true}"),
        )
        .schedule_at(base),
        OutboxMessageStatus::Pending,
    );

    let query = OutboxInspectionQuery::new(0)
        .with_status(OutboxMessageStatus::Pending)
        .with_aggregate("thread")
        .with_topic("agent_bus");

    assert_eq!(query.limit(), 1);
    assert!(query.matches_inspection(&record));
    assert!(query.matches_message(record.message(), record.status()));
    assert!(!query.with_topic("audit_log").matches_inspection(&record));
}

#[test]
fn outbox_inspection_snapshot_shapes_stable_operator_fields() {
    let base = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
    let lease = OutboxDeliveryLease::from_now(
        LeaseId::new("lease_outbox_snapshot"),
        "worker-outbox",
        Duration::from_secs(10),
        base,
    );
    let record = OutboxInspectionRecord::new(
        OutboxMessage::new(
            OutboxMessageId::new("msg_snapshot"),
            "approval",
            "approval_1",
            ExecutionId::new("exec_snapshot"),
            OutboxEnvelope::new("audit_log", "approval_1", "{\"ok\":true}"),
        )
        .schedule_at(base)
        .mark_attempted(
            base + Duration::from_secs(1),
            Some(base + Duration::from_secs(25)),
        ),
        OutboxMessageStatus::Pending,
    )
    .with_lease(Some(lease))
    .with_last_error(Some("transport down".to_string()));

    let snapshot = record.snapshot_at(base + Duration::from_secs(12));
    assert_eq!(snapshot.message_id().as_str(), "msg_snapshot");
    assert_eq!(snapshot.aggregate(), "approval");
    assert_eq!(snapshot.aggregate_id(), "approval_1");
    assert_eq!(snapshot.topic(), "audit_log");
    assert_eq!(snapshot.status(), OutboxMessageStatus::Pending);
    assert_eq!(snapshot.attempts(), 1);
    assert_eq!(snapshot.leased_by(), Some("worker-outbox"));
    assert!(snapshot.stale_lease());
    assert!(!snapshot.active_lease());
    assert!(!snapshot.due());
    assert!(snapshot.retry_scheduled());
    assert_eq!(snapshot.last_error(), Some("transport down"));
}

#[test]
fn claimed_outbox_delivery_can_complete_work() {
    let leased_at = SystemTime::UNIX_EPOCH + Duration::from_secs(12);
    let delivered_at = leased_at + Duration::from_secs(3);
    let lease = OutboxDeliveryLease::from_now(
        LeaseId::new("lease_3"),
        "worker-1",
        Duration::from_secs(30),
        leased_at,
    );
    let message = OutboxMessage::new(
        OutboxMessageId::new("msg_4"),
        "thread",
        "thr_3",
        ExecutionId::new("exec_3"),
        OutboxEnvelope::new("agent_bus.message_created", "thr_3", "{}"),
    )
    .schedule_at(SystemTime::UNIX_EPOCH + Duration::from_secs(1));

    let claimed = ClaimedOutboxDelivery::claim(message, lease).expect("claim should succeed");
    let (message, success) = claimed.complete(delivered_at);

    assert_eq!(message.attempts(), 1);
    assert_eq!(message.last_attempted_at(), Some(delivered_at));
    assert_eq!(success.message_id().as_str(), "msg_4");
}

#[test]
fn claimed_outbox_delivery_can_retry_work() {
    let leased_at = SystemTime::UNIX_EPOCH + Duration::from_secs(20);
    let attempted_at = leased_at + Duration::from_secs(5);
    let next_available_at = attempted_at + Duration::from_secs(25);
    let lease = OutboxDeliveryLease::from_now(
        LeaseId::new("lease_4"),
        "worker-2",
        Duration::from_secs(30),
        leased_at,
    );
    let message = OutboxMessage::new(
        OutboxMessageId::new("msg_5"),
        "thread",
        "thr_4",
        ExecutionId::new("exec_4"),
        OutboxEnvelope::new("agent_bus.message_created", "thr_4", "{}"),
    )
    .schedule_at(SystemTime::UNIX_EPOCH + Duration::from_secs(1));

    let claimed = ClaimedOutboxDelivery::claim(message, lease).expect("claim should succeed");
    let (message, retry) = claimed
        .retry(attempted_at, next_available_at, "timeout")
        .expect("retry should succeed");

    assert_eq!(message.attempts(), 1);
    assert_eq!(message.available_at(), next_available_at);
    assert_eq!(retry.error(), "timeout");
}

#[test]
fn claimed_outbox_delivery_can_dead_letter_work() {
    let leased_at = SystemTime::UNIX_EPOCH + Duration::from_secs(40);
    let dead_lettered_at = leased_at + Duration::from_secs(1);
    let lease = OutboxDeliveryLease::from_now(
        LeaseId::new("lease_5"),
        "worker-3",
        Duration::from_secs(30),
        leased_at,
    );
    let message = OutboxMessage::new(
        OutboxMessageId::new("msg_6"),
        "thread",
        "thr_5",
        ExecutionId::new("exec_5"),
        OutboxEnvelope::new("agent_bus.message_created", "thr_5", "{}"),
    )
    .schedule_at(SystemTime::UNIX_EPOCH + Duration::from_secs(1));

    let claimed = ClaimedOutboxDelivery::claim(message, lease).expect("claim should succeed");
    let (message, dead_letter) = claimed
        .dead_letter(dead_lettered_at, "retry budget exhausted")
        .expect("dead-letter should succeed");

    assert_eq!(message.attempts(), 1);
    assert_eq!(dead_letter.error(), "retry budget exhausted");
}

#[test]
fn claimed_outbox_delivery_rejects_early_claims() {
    let available_at = SystemTime::UNIX_EPOCH + Duration::from_secs(80);
    let lease = OutboxDeliveryLease::from_now(
        LeaseId::new("lease_6"),
        "worker-4",
        Duration::from_secs(30),
        available_at - Duration::from_secs(1),
    );
    let message = OutboxMessage::new(
        OutboxMessageId::new("msg_7"),
        "thread",
        "thr_6",
        ExecutionId::new("exec_6"),
        OutboxEnvelope::new("agent_bus.message_created", "thr_6", "{}"),
    )
    .schedule_at(available_at);

    let error = ClaimedOutboxDelivery::claim(message, lease).expect_err("claim should fail");
    assert_eq!(error.code(), "outbox_not_available_for_claim");
}

#[test]
fn transactional_write_requires_execution_record_for_outbox_messages() {
    let ctx = ExecutionContext::new(CorrelationId::new("corr_1"));
    let mut write = TransactionalWrite::<String>::default();
    write.push_outbox_message(OutboxMessage::new(
        OutboxMessageId::new("msg_4"),
        "thread",
        "thr_1",
        ExecutionId::new("exec_missing"),
        OutboxEnvelope::new("agent_bus.message_created", "thr_1", "{}"),
    ));

    let error = write.validate(&ctx).expect_err("validation should fail");
    assert_eq!(error.code(), "outbox_requires_execution_record");
}

#[test]
fn transactional_write_accepts_matching_execution_record_and_claim() {
    let ctx = ExecutionContext::new(CorrelationId::new("corr_2"));
    let key = IdempotencyKey::new("idem_1");
    let execution_id = ExecutionId::new("exec_3");
    let mut write = TransactionalWrite::<String>::default();
    write.push_idempotency_claim(IdempotencyClaim::new(key.clone(), "send_email"));
    write.push_execution_record(crate::ExecutionRecord::new(
        execution_id.clone(),
        "send_email",
        "gmail",
        key,
        CorrelationId::new("corr_2"),
    ));
    write.push_outbox_message(OutboxMessage::new(
        OutboxMessageId::new("msg_5"),
        "thread",
        "thr_2",
        execution_id,
        OutboxEnvelope::new("agent_bus.message_created", "thr_2", "{}"),
    ));

    let validated = write.validate(&ctx).expect("validation should pass");
    assert_eq!(validated.execution_records().len(), 1);
    assert_eq!(validated.idempotency_claims().len(), 1);
}

#[test]
fn transactional_write_rejects_duplicate_idempotency_claims() {
    let ctx = ExecutionContext::new(CorrelationId::new("corr_3"));
    let key = IdempotencyKey::new("idem_3");
    let mut write = TransactionalWrite::<String>::default();
    write.push_idempotency_claim(IdempotencyClaim::new(key.clone(), "publish_event"));
    write.push_idempotency_claim(IdempotencyClaim::new(key, "publish_event"));

    let error = write.validate(&ctx).expect_err("validation should fail");
    assert_eq!(error.code(), "duplicate_idempotency_claim");
}

#[test]
fn transactional_write_rejects_outbox_without_matching_execution_record() {
    let ctx = ExecutionContext::new(CorrelationId::new("corr_4"))
        .with_idempotency_key(IdempotencyKey::new("idem_4"));
    let mut write = TransactionalWrite::<String>::default();
    write.push_execution_record(
        crate::ExecutionRecord::from_context(
            ExecutionId::new("exec_4"),
            "publish_event",
            "agent_bus",
            &ctx,
        )
        .expect("execution record should build from context"),
    );
    write.push_outbox_message(OutboxMessage::new(
        OutboxMessageId::new("msg_6"),
        "thread",
        "thr_4",
        ExecutionId::new("exec_404"),
        OutboxEnvelope::new("agent_bus.message_created", "thr_4", "{}"),
    ));

    let error = write.validate(&ctx).expect_err("validation should fail");
    assert_eq!(error.code(), "outbox_execution_record_missing");
}

#[test]
fn transactional_write_rejects_blank_outbox_payloads() {
    let ctx = ExecutionContext::new(CorrelationId::new("corr_5"))
        .with_idempotency_key(IdempotencyKey::new("idem_5"));
    let execution_id = ExecutionId::new("exec_5");
    let mut write = TransactionalWrite::<String>::default();
    write.push_execution_record(
        crate::ExecutionRecord::from_context(
            execution_id.clone(),
            "publish_event",
            "agent_bus",
            &ctx,
        )
        .expect("execution record should build from context"),
    );
    write.push_outbox_message(OutboxMessage::new(
        OutboxMessageId::new("msg_7"),
        "thread",
        "thr_5",
        execution_id,
        OutboxEnvelope::new("agent_bus.message_created", "thr_5", "   "),
    ));

    let error = write.validate(&ctx).expect_err("validation should fail");
    assert_eq!(error.code(), "outbox_payload_required");
}

#[test]
fn transactional_write_rejects_duplicate_execution_idempotency_keys() {
    let ctx = ExecutionContext::new(CorrelationId::new("corr_6"));
    let key = IdempotencyKey::new("idem_6");
    let mut write = TransactionalWrite::<String>::default();
    write.push_idempotency_claim(IdempotencyClaim::new(key.clone(), "publish_event"));
    write.push_execution_record(crate::ExecutionRecord::new(
        ExecutionId::new("exec_6_a"),
        "publish_event",
        "agent_bus",
        key.clone(),
        CorrelationId::new("corr_6"),
    ));
    write.push_execution_record(crate::ExecutionRecord::new(
        ExecutionId::new("exec_6_b"),
        "publish_event",
        "agent_bus",
        key,
        CorrelationId::new("corr_6"),
    ));

    let error = write.validate(&ctx).expect_err("validation should fail");
    assert_eq!(error.code(), "duplicate_execution_idempotency_key");
}

#[test]
fn transactional_write_rejects_duplicate_outbox_message_ids() {
    let ctx = ExecutionContext::new(CorrelationId::new("corr_7"))
        .with_idempotency_key(IdempotencyKey::new("idem_7"));
    let execution_id = ExecutionId::new("exec_7");
    let message = OutboxMessage::new(
        OutboxMessageId::new("msg_8"),
        "thread",
        "thr_7",
        execution_id.clone(),
        OutboxEnvelope::new("agent_bus.message_created", "thr_7", "{}"),
    );
    let mut write = TransactionalWrite::<String>::default();
    write.push_execution_record(
        crate::ExecutionRecord::from_context(execution_id, "publish_event", "agent_bus", &ctx)
            .expect("execution record should build from context"),
    );
    write.push_outbox_message(message.clone());
    write.push_outbox_message(message);

    let error = write.validate(&ctx).expect_err("validation should fail");
    assert_eq!(error.code(), "duplicate_outbox_message");
}
