use std::time::{Duration, SystemTime};

use crate::{
    CorrelationId, DispatchClaimRequest, DispatchLeaseRenewal, DispatchQueue, DispatchResolution,
    DispatchSelectionRequest, ExecutionContext, ExecutionId, ExecutionInspectionQuery,
    ExecutionJournal, ExecutionLeaseRenewal, ExecutionRecord, ExecutionRecordStatus,
    IdempotencyKey, OutboxDeliveryLeaseRenewal, OutboxEnvelope, OutboxInspectionQuery,
    OutboxInspectionStore, OutboxMessage, OutboxMessageId, OutboxMessageStatus,
    VisibleWorkersQuery, WorkerDispatchSnapshotRef, WorkerDispatchSnapshotsQuery, WorkerHeartbeat,
    WorkerHeartbeatStore, WorkerVisibility, WorkerVisibilityRef,
};

use super::InMemoryExecutionWorkStore;

fn ctx() -> ExecutionContext {
    ExecutionContext::new(CorrelationId::new("corr_mem_store"))
}

fn record(id: &str, available_at: SystemTime) -> ExecutionRecord {
    ExecutionRecord::new(
        ExecutionId::new(id),
        "send_email",
        format!("target_{id}"),
        IdempotencyKey::new(format!("idem_{id}")),
        CorrelationId::new(format!("corr_{id}")),
    )
    .with_recorded_at(available_at)
}

fn outbox_message(id: &str, execution_id: &str, available_at: SystemTime) -> OutboxMessage {
    OutboxMessage::new(
        OutboxMessageId::new(id),
        "thread",
        format!("thread_{id}"),
        ExecutionId::new(execution_id),
        OutboxEnvelope::new("agent_bus", format!("thread_{id}"), "{\"ok\":true}"),
    )
    .schedule_at(available_at)
}

#[test]
fn due_selection_respects_availability_and_expired_leases() {
    let store = InMemoryExecutionWorkStore::new();
    let base = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
    let ctx = ctx();

    ExecutionJournal::append(&store, record("exec_1", base), &ctx).expect("append exec_1");
    ExecutionJournal::append(
        &store,
        record("exec_2", base + Duration::from_secs(10)),
        &ctx,
    )
    .expect("append exec_2");

    let initial_due = crate::DispatchWorkStore::select_due(
        &store,
        DispatchSelectionRequest::new(DispatchQueue::Execution, 10)
            .at(base + Duration::from_secs(11)),
        &ctx,
    )
    .expect("initial due");
    assert_eq!(initial_due.len(), 2);

    let claimed = crate::DispatchWorkStore::claim_due(
        &store,
        DispatchClaimRequest::new(
            DispatchQueue::Execution,
            "worker-a",
            1,
            Duration::from_secs(5),
        )
        .at(base + Duration::from_secs(11)),
        &ctx,
    )
    .expect("claim due");
    assert_eq!(claimed.len(), 1);

    let without_expired = crate::DispatchWorkStore::select_due(
        &store,
        DispatchSelectionRequest::new(DispatchQueue::Execution, 10)
            .at(base + Duration::from_secs(16))
            .with_expired_leases(false),
        &ctx,
    )
    .expect("due without expired");
    assert_eq!(without_expired.len(), 1);

    let with_expired = crate::DispatchWorkStore::select_due(
        &store,
        DispatchSelectionRequest::new(DispatchQueue::Execution, 10)
            .at(base + Duration::from_secs(17))
            .with_expired_leases(true),
        &ctx,
    )
    .expect("due with expired");
    assert_eq!(with_expired.len(), 2);

    match &with_expired[0] {
        crate::DueDispatchWork::Execution(record) => {
            assert_eq!(record.id().as_str(), "exec_1");
            assert_eq!(record.status(), ExecutionRecordStatus::Pending);
        }
        other => panic!("unexpected work item: {other:?}"),
    }
}

#[test]
fn dispatch_flow_supports_renew_retry_reclaim_and_complete() {
    let store = InMemoryExecutionWorkStore::new();
    let base = SystemTime::UNIX_EPOCH + Duration::from_secs(200);
    let ctx = ctx();

    ExecutionJournal::append(&store, record("exec_flow", base), &ctx).expect("append");

    let mut claimed = crate::DispatchWorkStore::claim_due(
        &store,
        DispatchClaimRequest::new(
            DispatchQueue::Execution,
            "worker-flow",
            1,
            Duration::from_secs(10),
        )
        .at(base),
        &ctx,
    )
    .expect("claim");
    let claimed = claimed.pop().expect("claimed execution");
    let lease = match claimed.lease() {
        crate::DispatchLease::Execution(lease) => lease,
        other => panic!("unexpected lease: {other:?}"),
    };

    let renewed = crate::DispatchWorkStore::renew_claim(
        &store,
        DispatchLeaseRenewal::Execution(
            ExecutionLeaseRenewal::new(
                ExecutionId::new("exec_flow"),
                lease.lease_id().clone(),
                "worker-flow",
                Duration::from_secs(15),
            )
            .at(base + Duration::from_secs(5)),
        ),
        &ctx,
    )
    .expect("renew");
    assert_eq!(renewed.lease_expires_at(), base + Duration::from_secs(20));

    let retry_resolution = claimed
        .retry(
            base + Duration::from_secs(6),
            base + Duration::from_secs(30),
            "temporary outage",
        )
        .expect("retry resolution");
    assert!(matches!(
        retry_resolution,
        DispatchResolution::ExecutionRetried { .. }
    ));
    crate::DispatchWorkStore::finalize(&store, retry_resolution, &ctx).expect("finalize retry");

    let not_yet_due = crate::DispatchWorkStore::select_due(
        &store,
        DispatchSelectionRequest::new(DispatchQueue::Execution, 10)
            .at(base + Duration::from_secs(29)),
        &ctx,
    )
    .expect("not yet due");
    assert!(not_yet_due.is_empty());

    let due_again = crate::DispatchWorkStore::select_due(
        &store,
        DispatchSelectionRequest::new(DispatchQueue::Execution, 10)
            .at(base + Duration::from_secs(31)),
        &ctx,
    )
    .expect("due again");
    assert_eq!(due_again.len(), 1);
    match &due_again[0] {
        crate::DueDispatchWork::Execution(record) => {
            assert_eq!(record.attempt(), 2);
            assert_eq!(record.last_error(), Some("temporary outage"));
        }
        other => panic!("unexpected due item: {other:?}"),
    }

    let mut reclaimed = crate::DispatchWorkStore::claim_due(
        &store,
        DispatchClaimRequest::new(
            DispatchQueue::Execution,
            "worker-flow",
            1,
            Duration::from_secs(10),
        )
        .at(base + Duration::from_secs(31)),
        &ctx,
    )
    .expect("reclaim");
    let reclaimed = reclaimed.pop().expect("reclaimed execution");
    let completion = reclaimed.complete(base + Duration::from_secs(32));
    crate::DispatchWorkStore::finalize(&store, completion, &ctx).expect("complete");

    let stored = store
        .record(&ExecutionId::new("exec_flow"))
        .expect("stored lookup")
        .expect("stored record");
    assert_eq!(stored.status(), ExecutionRecordStatus::Succeeded);
    assert_eq!(stored.attempt(), 2);
    assert!(store
        .lease(&ExecutionId::new("exec_flow"))
        .expect("lease lookup")
        .is_none());
}

#[test]
fn inspection_queries_summarize_execution_backlog() {
    let store = InMemoryExecutionWorkStore::new();
    let base = SystemTime::UNIX_EPOCH + Duration::from_secs(320);
    let ctx = ctx();

    ExecutionJournal::append(&store, record("exec_pending", base), &ctx).expect("append");
    ExecutionJournal::append(
        &store,
        record("exec_failed", base + Duration::from_secs(5))
            .mark_failed(base + Duration::from_secs(5), "temporary"),
        &ctx,
    )
    .expect("append failed");

    let summary = crate::ExecutionInspectionStore::summarize_executions(
        &store,
        ExecutionInspectionQuery::new(10).at(base + Duration::from_secs(10)),
        &ctx,
    )
    .expect("execution summary");
    assert_eq!(summary.total(), 2);
    assert_eq!(summary.pending(), 1);
    assert_eq!(summary.failed(), 1);
    assert_eq!(summary.due(), 2);
    assert_eq!(summary.retry_scheduled(), 0);
    assert_eq!(summary.stale_leases(), 0);

    let failed = crate::ExecutionInspectionStore::list_executions(
        &store,
        ExecutionInspectionQuery::new(10)
            .at(base + Duration::from_secs(10))
            .with_status(ExecutionRecordStatus::Failed),
        &ctx,
    )
    .expect("failed executions");
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].id().as_str(), "exec_failed");
}

#[test]
fn outbox_dispatch_flow_and_inspection_cover_retry_and_delivery() {
    let store = InMemoryExecutionWorkStore::new();
    let base = SystemTime::UNIX_EPOCH + Duration::from_secs(360);
    let ctx = ctx();

    store
        .append_outbox(outbox_message("msg_outbox", "exec_outbox", base), &ctx)
        .expect("append outbox");

    let mut claimed = crate::DispatchWorkStore::claim_due(
        &store,
        DispatchClaimRequest::new(
            DispatchQueue::Outbox,
            "worker-outbox",
            1,
            Duration::from_secs(10),
        )
        .at(base),
        &ctx,
    )
    .expect("claim outbox");
    let claimed = claimed.pop().expect("claimed outbox");
    let lease = match claimed.lease() {
        crate::DispatchLease::Outbox(lease) => lease,
        other => panic!("unexpected lease: {other:?}"),
    };

    let renewed = crate::DispatchWorkStore::renew_claim(
        &store,
        DispatchLeaseRenewal::Outbox(
            OutboxDeliveryLeaseRenewal::new(
                OutboxMessageId::new("msg_outbox"),
                lease.lease_id().clone(),
                "worker-outbox",
                Duration::from_secs(15),
            )
            .at(base + Duration::from_secs(3)),
        ),
        &ctx,
    )
    .expect("renew outbox");
    assert_eq!(renewed.lease_expires_at(), base + Duration::from_secs(18));

    let retry_resolution = claimed
        .retry(
            base + Duration::from_secs(4),
            base + Duration::from_secs(20),
            "transport down",
        )
        .expect("retry resolution");
    assert!(matches!(
        retry_resolution,
        DispatchResolution::OutboxRetried { .. }
    ));
    crate::DispatchWorkStore::finalize(&store, retry_resolution, &ctx).expect("finalize retry");

    let summary = crate::OutboxInspectionStore::summarize_outbox(
        &store,
        OutboxInspectionQuery::new(10).at(base + Duration::from_secs(10)),
        &ctx,
    )
    .expect("outbox summary after retry");
    assert_eq!(summary.pending(), 1);
    assert_eq!(summary.retry_scheduled(), 1);
    assert_eq!(summary.leased(), 0);
    assert_eq!(summary.stale_leases(), 0);

    let mut reclaimed = crate::DispatchWorkStore::claim_due(
        &store,
        DispatchClaimRequest::new(
            DispatchQueue::Outbox,
            "worker-outbox",
            1,
            Duration::from_secs(10),
        )
        .at(base + Duration::from_secs(21)),
        &ctx,
    )
    .expect("reclaim outbox");
    let reclaimed = reclaimed.pop().expect("reclaimed outbox");
    let completion = reclaimed.complete(base + Duration::from_secs(22));
    crate::DispatchWorkStore::finalize(&store, completion, &ctx).expect("complete outbox");

    let record = OutboxInspectionStore::lookup_outbox_message(
        &store,
        &OutboxMessageId::new("msg_outbox"),
        base + Duration::from_secs(22),
        &ExecutionContext::new(CorrelationId::new("corr_outbox_lookup"))
            .start_at(base + Duration::from_secs(22)),
    )
    .expect("lookup outbox")
    .expect("outbox record");
    assert_eq!(record.status(), OutboxMessageStatus::Delivered);
    assert_eq!(record.message().attempts(), 2);
}

#[test]
fn worker_visibility_and_snapshots_reflect_heartbeats_and_stale_claims() {
    let store = InMemoryExecutionWorkStore::new();
    let base = SystemTime::UNIX_EPOCH + Duration::from_secs(500);
    let ctx = ctx();

    ExecutionJournal::append(&store, record("exec_worker", base), &ctx).expect("append");
    let claimed = crate::DispatchWorkStore::claim_due(
        &store,
        DispatchClaimRequest::new(
            DispatchQueue::Execution,
            "worker-visible",
            1,
            Duration::from_secs(10),
        )
        .at(base),
        &ctx,
    )
    .expect("claim");
    assert_eq!(claimed.len(), 1);

    WorkerHeartbeatStore::record_heartbeat(
        &store,
        WorkerHeartbeat::from_ttl(
            "worker-visible",
            "scheduler",
            base + Duration::from_secs(1),
            Duration::from_secs(20),
        ),
        &ctx,
    )
    .expect("record heartbeat");

    let visible = WorkerHeartbeatStore::lookup_worker(
        &store,
        WorkerVisibilityRef::new("worker-visible", "scheduler").at(base + Duration::from_secs(5)),
        &ctx,
    )
    .expect("lookup visible")
    .expect("visible worker");
    assert_eq!(visible.active_leases(), 1);

    let snapshot = crate::DispatchStatusStore::lookup_worker_snapshot(
        &store,
        WorkerDispatchSnapshotRef::new("worker-visible", "scheduler")
            .at(base + Duration::from_secs(5)),
        &ctx,
    )
    .expect("snapshot lookup")
    .expect("worker snapshot");
    assert_eq!(snapshot.visibility(), WorkerVisibility::Visible);
    assert_eq!(snapshot.active_leases(), 1);
    assert_eq!(snapshot.claimed_work(), 1);
    assert_eq!(snapshot.stale_claims(), 0);
    assert_eq!(
        snapshot
            .queue(DispatchQueue::Outbox)
            .expect("outbox queue")
            .claimed_count(),
        0
    );

    let stale_snapshot = crate::DispatchStatusStore::lookup_worker_snapshot(
        &store,
        WorkerDispatchSnapshotRef::new("worker-visible", "scheduler")
            .at(base + Duration::from_secs(12)),
        &ctx,
    )
    .expect("stale snapshot lookup")
    .expect("stale snapshot");
    assert_eq!(stale_snapshot.visibility(), WorkerVisibility::Visible);
    assert_eq!(stale_snapshot.active_leases(), 0);
    assert_eq!(stale_snapshot.claimed_work(), 0);
    assert_eq!(stale_snapshot.stale_claims(), 1);

    let visible_workers = WorkerHeartbeatStore::list_visible_workers(
        &store,
        VisibleWorkersQuery::new("scheduler").at(base + Duration::from_secs(12)),
        &ctx,
    )
    .expect("list visible workers");
    assert_eq!(visible_workers.len(), 1);
    assert_eq!(visible_workers[0].active_leases(), 0);

    let snapshots = crate::DispatchStatusStore::list_worker_snapshots(
        &store,
        WorkerDispatchSnapshotsQuery::new("scheduler")
            .at(base + Duration::from_secs(25))
            .with_expired_workers(false),
        &ctx,
    )
    .expect("list worker snapshots");
    assert!(snapshots.is_empty());
}
