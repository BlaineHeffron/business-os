use std::time::{Duration, SystemTime};

use crate::{CorrelationId, ExecutionContext, ExecutionId, IdempotencyKey, LeaseId};

use super::{
    ClaimedExecution, DueExecutionRequest, ExecutionInspectionRecord, ExecutionLease,
    ExecutionLeaseRenewal, ExecutionLeaseRequest, ExecutionRecord, ExecutionRecordStatus,
    IdempotencyClaim, IdempotencyClaimStatus,
};

#[test]
fn execution_record_from_context_uses_attempt_and_started_at() {
    let started_at = SystemTime::UNIX_EPOCH + Duration::from_secs(42);
    let ctx = ExecutionContext::new(CorrelationId::new("corr_1"))
        .with_idempotency_key(IdempotencyKey::new("idem_1"))
        .with_attempt(3)
        .start_at(started_at);

    let record =
        ExecutionRecord::from_context(ExecutionId::new("exec_1"), "send_email", "gmail", &ctx)
            .expect("record should build");

    assert_eq!(record.attempt(), 3);
    assert_eq!(record.recorded_at(), started_at);
    assert_eq!(record.available_at(), started_at);
}

#[test]
fn execution_record_tracks_failures() {
    let finished_at = SystemTime::UNIX_EPOCH + Duration::from_secs(99);
    let record = ExecutionRecord::new(
        ExecutionId::new("exec_2"),
        "send_email",
        "gmail",
        IdempotencyKey::new("idem_2"),
        CorrelationId::new("corr_2"),
    )
    .mark_in_flight()
    .mark_failed(finished_at, "smtp unavailable");

    assert_eq!(record.status(), ExecutionRecordStatus::Failed);
    assert_eq!(record.finished_at(), Some(finished_at));
    assert_eq!(record.last_error(), Some("smtp unavailable"));
}

#[test]
fn idempotency_claim_from_context_tracks_attempt_and_execution() {
    let started_at = SystemTime::UNIX_EPOCH + Duration::from_secs(7);
    let ctx = ExecutionContext::new(CorrelationId::new("corr_3"))
        .with_attempt(2)
        .start_at(started_at);

    let claim = IdempotencyClaim::from_context(
        IdempotencyKey::new("idem_3"),
        "send_quote",
        ExecutionId::new("exec_3"),
        &ctx,
    )
    .expect("claim should build");

    assert_eq!(claim.attempt(), 2);
    assert_eq!(claim.execution_id().expect("execution").as_str(), "exec_3");
    assert_eq!(
        claim.correlation_id().expect("correlation").as_str(),
        "corr_3"
    );
    assert_eq!(claim.last_claimed_at(), started_at);
}

#[test]
fn idempotency_claim_tracks_duplicates_and_completion() {
    let duplicate_at = SystemTime::UNIX_EPOCH + Duration::from_secs(10);
    let completed_at = SystemTime::UNIX_EPOCH + Duration::from_secs(11);
    let claim = IdempotencyClaim::new(IdempotencyKey::new("idem_4"), "publish_event")
        .record_duplicate(duplicate_at)
        .mark_succeeded(completed_at);

    assert_eq!(claim.duplicate_count(), 1);
    assert_eq!(claim.status(), IdempotencyClaimStatus::Succeeded);
    assert_eq!(claim.completed_at(), Some(completed_at));
}

#[test]
fn execution_lease_request_clamps_batch_size() {
    let request = ExecutionLeaseRequest::new("worker-1", 0, Duration::from_secs(10));
    assert_eq!(request.batch_size(), 1);
}

#[test]
fn execution_inspection_record_tracks_due_retry_and_stale_lease_state() {
    let base = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
    let lease = ExecutionLease::from_now(
        LeaseId::new("lease_1"),
        "worker-1",
        Duration::from_secs(10),
        base,
    );

    let leased = ExecutionInspectionRecord::new(
        ExecutionRecord::new(
            ExecutionId::new("exec_due"),
            "send_email",
            "gmail",
            IdempotencyKey::new("idem_due"),
            CorrelationId::new("corr_due"),
        )
        .with_recorded_at(base),
    )
    .with_lease(Some(lease));
    assert!(!leased.is_due_at(base + Duration::from_secs(5)));
    assert!(leased.is_due_at(base + Duration::from_secs(11)));
    assert!(leased.has_stale_lease_at(base + Duration::from_secs(11)));

    let retry = ExecutionInspectionRecord::new(
        ExecutionRecord::new(
            ExecutionId::new("exec_retry"),
            "send_email",
            "gmail",
            IdempotencyKey::new("idem_retry"),
            CorrelationId::new("corr_retry"),
        )
        .with_recorded_at(base)
        .schedule_retry(
            base + Duration::from_secs(1),
            base + Duration::from_secs(20),
            "retry",
        ),
    );
    assert!(retry.is_retry_scheduled_at(base + Duration::from_secs(10)));
}

#[test]
fn execution_inspection_queries_match_records_and_snapshots() {
    let base = SystemTime::UNIX_EPOCH + Duration::from_secs(130);
    let record = ExecutionRecord::new(
        ExecutionId::new("exec_query"),
        "send_email",
        "gmail",
        IdempotencyKey::new("idem_query"),
        CorrelationId::new("corr_query"),
    )
    .with_recorded_at(base);
    let inspection = ExecutionInspectionRecord::new(record.clone());

    let query = crate::ExecutionInspectionQuery::new(0)
        .with_status(ExecutionRecordStatus::Pending)
        .with_operation("send_email")
        .with_target("gmail");

    assert_eq!(query.limit(), 1);
    assert!(query.matches_record(&record));
    assert!(query.matches_inspection(&inspection));
    assert!(!query.with_target("smtp").matches_record(&record));
}

#[test]
fn execution_inspection_snapshot_shapes_stable_operator_fields() {
    let base = SystemTime::UNIX_EPOCH + Duration::from_secs(160);
    let lease = ExecutionLease::from_now(
        LeaseId::new("lease_snapshot"),
        "worker-snapshot",
        Duration::from_secs(10),
        base,
    );
    let inspection = ExecutionInspectionRecord::new(
        ExecutionRecord::new(
            ExecutionId::new("exec_snapshot"),
            "send_quote",
            "hubspot",
            IdempotencyKey::new("idem_snapshot"),
            CorrelationId::new("corr_snapshot"),
        )
        .with_recorded_at(base)
        .schedule_retry(
            base + Duration::from_secs(1),
            base + Duration::from_secs(20),
            "temporary",
        ),
    )
    .with_lease(Some(lease));

    let snapshot = inspection.snapshot_at(base + Duration::from_secs(12));
    assert_eq!(snapshot.execution_id().as_str(), "exec_snapshot");
    assert_eq!(snapshot.operation(), "send_quote");
    assert_eq!(snapshot.target(), "hubspot");
    assert_eq!(snapshot.status(), ExecutionRecordStatus::Pending);
    assert_eq!(snapshot.attempt(), 2);
    assert_eq!(snapshot.leased_by(), Some("worker-snapshot"));
    assert!(snapshot.stale_lease());
    assert!(!snapshot.active_lease());
    assert!(!snapshot.due());
    assert!(snapshot.retry_scheduled());
    assert_eq!(snapshot.last_error(), Some("temporary"));
}

#[test]
fn due_execution_request_clamps_limit() {
    let due_before = SystemTime::UNIX_EPOCH + Duration::from_secs(12);
    let request = DueExecutionRequest::new(0).at(due_before);

    assert_eq!(request.limit(), 1);
    assert_eq!(request.due_before(), due_before);
    assert!(request.include_expired_leases());
}

#[test]
fn execution_lease_can_be_renewed() {
    let renewed_at = SystemTime::UNIX_EPOCH + Duration::from_secs(20);
    let lease = ExecutionLease::from_now(
        LeaseId::new("lease_exec_renew_1"),
        "worker-renew",
        Duration::from_secs(10),
        SystemTime::UNIX_EPOCH + Duration::from_secs(5),
    )
    .renew(renewed_at, Duration::from_secs(30));

    assert_eq!(lease.lease_id().as_str(), "lease_exec_renew_1");
    assert_eq!(lease.leased_by(), "worker-renew");
    assert_eq!(lease.leased_at(), renewed_at);
    assert_eq!(
        lease.lease_expires_at(),
        renewed_at + Duration::from_secs(30)
    );
    assert!(!lease.is_expired_at(renewed_at + Duration::from_secs(29)));
    assert!(lease.is_expired_at(renewed_at + Duration::from_secs(31)));
}

#[test]
fn execution_lease_renewal_builds_next_lease_window() {
    let renewed_at = SystemTime::UNIX_EPOCH + Duration::from_secs(25);
    let renewal = ExecutionLeaseRenewal::new(
        ExecutionId::new("exec_renew_1"),
        LeaseId::new("lease_exec_renew_2"),
        "worker-renew-2",
        Duration::from_secs(45),
    )
    .at(renewed_at);
    let lease = renewal.renewed_lease();

    assert_eq!(renewal.execution_id().as_str(), "exec_renew_1");
    assert_eq!(renewal.lease_id().as_str(), "lease_exec_renew_2");
    assert_eq!(renewal.leased_by(), "worker-renew-2");
    assert_eq!(renewal.renewed_at(), renewed_at);
    assert_eq!(renewal.lease_ttl(), Duration::from_secs(45));
    assert_eq!(lease.leased_at(), renewed_at);
    assert_eq!(
        lease.lease_expires_at(),
        renewed_at + Duration::from_secs(45)
    );
}

#[test]
fn execution_lease_renewal_requires_owner_and_ttl() {
    let ctx = ExecutionContext::new(CorrelationId::new("corr_renew_1"));
    let missing_owner = ExecutionLeaseRenewal::new(
        ExecutionId::new("exec_renew_2"),
        LeaseId::new("lease_exec_renew_3"),
        "  ",
        Duration::from_secs(10),
    );
    let missing_ttl = ExecutionLeaseRenewal::new(
        ExecutionId::new("exec_renew_3"),
        LeaseId::new("lease_exec_renew_4"),
        "worker-renew-3",
        Duration::ZERO,
    );

    assert_eq!(
        missing_owner
            .validate(&ctx)
            .expect_err("owner should be required")
            .code(),
        "execution_lease_renewal_owner_required"
    );
    assert_eq!(
        missing_ttl
            .validate(&ctx)
            .expect_err("ttl should be required")
            .code(),
        "execution_lease_renewal_ttl_required"
    );
}

#[test]
fn claimed_execution_can_complete_work() {
    let leased_at = SystemTime::UNIX_EPOCH + Duration::from_secs(30);
    let finished_at = SystemTime::UNIX_EPOCH + Duration::from_secs(31);
    let record = ExecutionRecord::new(
        ExecutionId::new("exec_4"),
        "send_email",
        "gmail",
        IdempotencyKey::new("idem_4"),
        CorrelationId::new("corr_4"),
    )
    .with_recorded_at(SystemTime::UNIX_EPOCH + Duration::from_secs(10));
    let lease = ExecutionLease::from_now(
        LeaseId::new("lease_exec_1"),
        "worker-1",
        Duration::from_secs(30),
        leased_at,
    );

    let claimed = ClaimedExecution::claim(record, lease).expect("claim should succeed");
    assert_eq!(claimed.record().status(), ExecutionRecordStatus::InFlight);
    assert_eq!(claimed.record().last_attempted_at(), Some(leased_at));

    let (record, success) = claimed.complete(finished_at);
    assert_eq!(record.status(), ExecutionRecordStatus::Succeeded);
    assert_eq!(success.execution_id().as_str(), "exec_4");
    assert_eq!(success.lease_id().as_str(), "lease_exec_1");
}

#[test]
fn claimed_execution_can_schedule_retry() {
    let leased_at = SystemTime::UNIX_EPOCH + Duration::from_secs(50);
    let attempted_at = leased_at + Duration::from_secs(2);
    let next_available_at = attempted_at + Duration::from_secs(20);
    let record = ExecutionRecord::new(
        ExecutionId::new("exec_5"),
        "send_email",
        "gmail",
        IdempotencyKey::new("idem_5"),
        CorrelationId::new("corr_5"),
    )
    .with_recorded_at(SystemTime::UNIX_EPOCH + Duration::from_secs(10));
    let lease = ExecutionLease::from_now(
        LeaseId::new("lease_exec_2"),
        "worker-2",
        Duration::from_secs(30),
        leased_at,
    );

    let claimed = ClaimedExecution::claim(record, lease).expect("claim should succeed");
    let (record, retry) = claimed
        .retry(attempted_at, next_available_at, "smtp unavailable")
        .expect("retry should succeed");

    assert_eq!(record.status(), ExecutionRecordStatus::Pending);
    assert_eq!(record.attempt(), 2);
    assert_eq!(record.available_at(), next_available_at);
    assert_eq!(record.last_error(), Some("smtp unavailable"));
    assert_eq!(retry.error(), "smtp unavailable");
}

#[test]
fn claimed_execution_can_dead_letter_work() {
    let leased_at = SystemTime::UNIX_EPOCH + Duration::from_secs(70);
    let dead_lettered_at = leased_at + Duration::from_secs(5);
    let record = ExecutionRecord::new(
        ExecutionId::new("exec_6"),
        "send_email",
        "gmail",
        IdempotencyKey::new("idem_6"),
        CorrelationId::new("corr_6"),
    )
    .with_recorded_at(SystemTime::UNIX_EPOCH + Duration::from_secs(10));
    let lease = ExecutionLease::from_now(
        LeaseId::new("lease_exec_3"),
        "worker-3",
        Duration::from_secs(30),
        leased_at,
    );

    let claimed = ClaimedExecution::claim(record, lease).expect("claim should succeed");
    let (record, dead_letter) = claimed
        .dead_letter(dead_lettered_at, "retry budget exhausted")
        .expect("dead-letter should succeed");

    assert_eq!(record.status(), ExecutionRecordStatus::DeadLettered);
    assert_eq!(record.finished_at(), Some(dead_lettered_at));
    assert_eq!(dead_letter.error(), "retry budget exhausted");
}

#[test]
fn claimed_execution_rejects_early_claims() {
    let available_at = SystemTime::UNIX_EPOCH + Duration::from_secs(200);
    let record = ExecutionRecord::new(
        ExecutionId::new("exec_7"),
        "send_email",
        "gmail",
        IdempotencyKey::new("idem_7"),
        CorrelationId::new("corr_7"),
    )
    .with_available_at(available_at);
    let lease = ExecutionLease::from_now(
        LeaseId::new("lease_exec_4"),
        "worker-4",
        Duration::from_secs(10),
        available_at - Duration::from_secs(1),
    );

    let error = ClaimedExecution::claim(record, lease).expect_err("claim should fail");
    assert_eq!(error.code(), "execution_not_available_for_claim");
}
