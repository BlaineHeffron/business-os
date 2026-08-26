use std::time::{Duration, SystemTime};

use bos_kernel::{
    CorrelationId, DispatchQueue, ExecutionId, ExecutionInspectionQuery, ExecutionJournal,
    ExecutionRecord, IdempotencyKey, InMemoryExecutionWorkStore, NoopTelemetry, OutboxEnvelope,
    OutboxInspectionQuery, OutboxMessage, OutboxMessageId, OutboxMessageStatus,
    ReferenceActiveClaimRef, ReferenceDispatchInspector, ReferenceDispatchRunner,
    ReferenceQueueInspectionRecord, ReferenceSchedulerStateQuery, WorkerDispatchCoordinatorConfig,
    WorkerDispatchCycleRequest, WorkerVisibility,
};

fn ctx() -> bos_kernel::ExecutionContext {
    bos_kernel::ExecutionContext::new(CorrelationId::new("corr_operator_views"))
}

fn execution_record(id: &str, available_at: SystemTime) -> ExecutionRecord {
    ExecutionRecord::new(
        ExecutionId::new(id),
        "send_email",
        format!("target_{id}"),
        IdempotencyKey::new(format!("idem_{id}")),
        CorrelationId::new(format!("corr_{id}")),
    )
    .with_recorded_at(available_at)
}

fn execution_runner<'a>(store: &'a InMemoryExecutionWorkStore) -> ReferenceDispatchRunner<'a> {
    ReferenceDispatchRunner::new(
        store,
        store,
        store,
        WorkerDispatchCoordinatorConfig::new(
            DispatchQueue::Execution,
            "scheduler",
            Duration::from_secs(10),
        ),
    )
    .with_telemetry(&NoopTelemetry)
}

fn outbox_runner<'a>(store: &'a InMemoryExecutionWorkStore) -> ReferenceDispatchRunner<'a> {
    ReferenceDispatchRunner::new(
        store,
        store,
        store,
        WorkerDispatchCoordinatorConfig::new(
            DispatchQueue::Outbox,
            "scheduler",
            Duration::from_secs(10),
        ),
    )
    .with_telemetry(&NoopTelemetry)
}

#[test]
fn execution_operator_views_preserve_summary_and_snapshot_shape() {
    let store = InMemoryExecutionWorkStore::new();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_200);
    let ctx = ctx();
    let runner = execution_runner(&store);

    ExecutionJournal::append(&store, execution_record("exec_ready", now), &ctx).expect("append");
    ExecutionJournal::append(
        &store,
        execution_record("exec_retry", now).schedule_retry(
            now + Duration::from_secs(1),
            now + Duration::from_secs(30),
            "temporary",
        ),
        &ctx,
    )
    .expect("append retry");

    runner
        .claim(
            WorkerDispatchCycleRequest::new("worker-exec", 1, Duration::from_secs(20)).at(now),
            &ctx,
        )
        .expect("claim");

    let observed_at = now + Duration::from_secs(12);
    let inspector = ReferenceDispatchInspector::for_executions(&runner, &store);
    let state = inspector
        .inspect(
            ReferenceSchedulerStateQuery::new()
                .at(observed_at)
                .with_backlog_limit(10),
            &ctx,
        )
        .expect("inspect");
    let summary = state.operator_summary();

    assert_eq!(summary.scheduler().queue(), DispatchQueue::Execution);
    assert_eq!(summary.scheduler().workers(), 1);
    assert_eq!(summary.scheduler().active_claims(), 1);
    assert_eq!(summary.queue().total(), 2);
    assert_eq!(summary.queue().in_flight(), 1);
    assert_eq!(summary.queue().pending(), 1);
    assert_eq!(summary.queue().retry_scheduled(), 1);
    assert_eq!(summary.workers()[0].worker_id(), "worker-exec");
    assert_eq!(summary.workers()[0].visibility(), WorkerVisibility::Expired);
    assert_eq!(summary.workers()[0].active_claims(), 1);

    let filter = ExecutionInspectionQuery::new(10)
        .at(observed_at)
        .with_operation("send_email");
    assert!(state.backlog().iter().all(|record| match record {
        ReferenceQueueInspectionRecord::Execution(record) => filter.matches_inspection(record),
        ReferenceQueueInspectionRecord::Outbox(_) => false,
    }));
    let snapshots = state
        .backlog()
        .iter()
        .map(|record| match record {
            ReferenceQueueInspectionRecord::Execution(record) => record.snapshot_at(observed_at),
            ReferenceQueueInspectionRecord::Outbox(_) => panic!("expected execution backlog"),
        })
        .collect::<Vec<_>>();

    assert_eq!(snapshots.len(), 2);
    assert!(snapshots.iter().any(|snapshot| {
        snapshot.execution_id().as_str() == "exec_ready" && snapshot.active_lease()
    }));
    assert!(snapshots.iter().any(|snapshot| {
        snapshot.execution_id().as_str() == "exec_retry"
            && snapshot.retry_scheduled()
            && snapshot.last_error() == Some("temporary")
    }));
}

#[test]
fn execution_operator_helpers_expose_direct_record_and_state_lookups() {
    let store = InMemoryExecutionWorkStore::new();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_320);
    let ctx = ctx();
    let runner = execution_runner(&store);

    ExecutionJournal::append(&store, execution_record("exec_lookup_ready", now), &ctx)
        .expect("append ready");
    ExecutionJournal::append(
        &store,
        execution_record("exec_lookup_done", now).mark_succeeded(now + Duration::from_secs(4)),
        &ctx,
    )
    .expect("append done");

    runner
        .claim(
            WorkerDispatchCycleRequest::new("worker-lookup", 1, Duration::from_secs(30)).at(now),
            &ctx,
        )
        .expect("claim");

    let observed_at = now + Duration::from_secs(5);
    let inspector = ReferenceDispatchInspector::for_executions(&runner, &store);
    let state = inspector
        .inspect(
            ReferenceSchedulerStateQuery::new()
                .at(observed_at)
                .with_backlog_limit(10)
                .with_worker_limit(10),
            &ctx,
        )
        .expect("inspect");

    let ready = state
        .backlog_record("exec_lookup_ready")
        .expect("ready backlog record");
    let execution = ready.execution().expect("execution record");
    assert_eq!(execution.execution_id().as_str(), "exec_lookup_ready");
    assert_eq!(execution.operation(), "send_email");
    assert_eq!(execution.target(), "target_exec_lookup_ready");
    assert_eq!(
        execution.status(),
        bos_kernel::ExecutionRecordStatus::InFlight
    );
    assert_eq!(ready.leased_by(), Some("worker-lookup"));
    assert!(ready.lease_id().is_some());
    assert!(!ready.is_terminal());

    let done = state
        .backlog_snapshot("exec_lookup_done")
        .expect("done backlog snapshot");
    let done = done.execution().expect("done execution snapshot");
    assert_eq!(done.execution_id().as_str(), "exec_lookup_done");
    assert!(done.is_terminal());
    assert_eq!(done.last_error(), None);

    let worker = state.worker("worker-lookup").expect("worker state");
    assert_eq!(worker.worker_id(), "worker-lookup");

    let summary = state.operator_summary();
    let worker = summary.worker("worker-lookup").expect("worker summary");
    let queue = worker
        .queue(DispatchQueue::Execution)
        .expect("worker execution queue");
    assert_eq!(queue.claimed(), 1);
    assert_eq!(summary.queue().unfinished(), 1);
}

#[test]
fn outbox_operator_views_preserve_summary_and_snapshot_shape() {
    let store = InMemoryExecutionWorkStore::new();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_260);
    let ctx = ctx();
    let runner = outbox_runner(&store);

    store
        .append_outbox(
            OutboxMessage::new(
                OutboxMessageId::new("msg_due"),
                "thread",
                "thread_1",
                ExecutionId::new("exec_due"),
                OutboxEnvelope::new("agent_bus", "thread_1", "{\"ok\":true}"),
            )
            .schedule_at(now),
            &ctx,
        )
        .expect("append due outbox");
    store
        .append_outbox(
            OutboxMessage::new(
                OutboxMessageId::new("msg_retry"),
                "approval",
                "approval_1",
                ExecutionId::new("exec_retry"),
                OutboxEnvelope::new("audit_log", "approval_1", "{\"ok\":true}"),
            )
            .schedule_at(now + Duration::from_secs(30))
            .mark_attempted(now + Duration::from_secs(1), None),
            &ctx,
        )
        .expect("append retry outbox");

    runner
        .claim(
            WorkerDispatchCycleRequest::new("worker-outbox", 1, Duration::from_secs(20)).at(now),
            &ctx,
        )
        .expect("claim");

    let observed_at = now + Duration::from_secs(5);
    let inspector = ReferenceDispatchInspector::for_outbox(&runner, &store);
    let state = inspector
        .inspect(
            ReferenceSchedulerStateQuery::new()
                .at(observed_at)
                .with_backlog_limit(10),
            &ctx,
        )
        .expect("inspect");
    let summary = state.operator_summary();

    assert_eq!(summary.scheduler().queue(), DispatchQueue::Outbox);
    assert_eq!(summary.scheduler().workers(), 1);
    assert_eq!(summary.queue().total(), 2);
    assert_eq!(summary.queue().pending(), 1);
    assert_eq!(summary.queue().in_flight(), 1);
    assert_eq!(summary.queue().leased(), 1);
    assert_eq!(summary.queue().due(), 0);
    assert_eq!(summary.queue().retry_scheduled(), 1);

    let filter = OutboxInspectionQuery::new(10)
        .at(observed_at)
        .with_status(OutboxMessageStatus::Pending);
    assert!(state.backlog().iter().any(|record| match record {
        ReferenceQueueInspectionRecord::Outbox(record) => filter.matches_inspection(record),
        ReferenceQueueInspectionRecord::Execution(_) => false,
    }));
    let snapshots = state
        .backlog()
        .iter()
        .map(|record| match record {
            ReferenceQueueInspectionRecord::Outbox(record) => record.snapshot_at(observed_at),
            ReferenceQueueInspectionRecord::Execution(_) => panic!("expected outbox backlog"),
        })
        .collect::<Vec<_>>();

    assert_eq!(snapshots.len(), 2);
    assert!(snapshots.iter().any(|snapshot| {
        snapshot.message_id().as_str() == "msg_due"
            && snapshot.active_lease()
            && snapshot.status() == OutboxMessageStatus::InFlight
    }));
    assert!(snapshots.iter().any(|snapshot| {
        snapshot.message_id().as_str() == "msg_retry"
            && snapshot.retry_scheduled()
            && snapshot.status() == OutboxMessageStatus::Pending
    }));
}

#[test]
fn outbox_operator_helpers_expose_direct_record_and_state_lookups() {
    let store = InMemoryExecutionWorkStore::new();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_380);
    let ctx = ctx();
    let runner = outbox_runner(&store);

    store
        .append_outbox(
            OutboxMessage::new(
                OutboxMessageId::new("msg_lookup_due"),
                "thread",
                "thread_lookup",
                ExecutionId::new("exec_lookup_due"),
                OutboxEnvelope::new("agent_bus", "thread_lookup", "{\"ok\":true}"),
            )
            .schedule_at(now),
            &ctx,
        )
        .expect("append due");
    store
        .append_outbox(
            OutboxMessage::new(
                OutboxMessageId::new("msg_lookup_done"),
                "approval",
                "approval_lookup",
                ExecutionId::new("exec_lookup_done"),
                OutboxEnvelope::new("audit_log", "approval_lookup", "{\"ok\":true}"),
            )
            .schedule_at(now),
            &ctx,
        )
        .expect("append done");

    let claim = runner
        .claim(
            WorkerDispatchCycleRequest::new("worker-outbox-lookup", 1, Duration::from_secs(20))
                .at(now),
            &ctx,
        )
        .expect("claim outbox");
    let due_claim = claim.claims().first().expect("due claim");
    assert_eq!(due_claim.work_id(), "msg_lookup_done");

    runner
        .complete(
            ReferenceActiveClaimRef::new("worker-outbox-lookup", due_claim.lease_id().clone()),
            now + Duration::from_secs(2),
            &ctx,
        )
        .expect("complete");

    runner
        .claim(
            WorkerDispatchCycleRequest::new("worker-outbox-lookup", 1, Duration::from_secs(20))
                .at(now + Duration::from_secs(3)),
            &ctx,
        )
        .expect("claim due");

    let observed_at = now + Duration::from_secs(4);
    let inspector = ReferenceDispatchInspector::for_outbox(&runner, &store);
    let state = inspector
        .inspect(
            ReferenceSchedulerStateQuery::new()
                .at(observed_at)
                .with_backlog_limit(10)
                .with_worker_limit(10),
            &ctx,
        )
        .expect("inspect");

    let due = state
        .backlog_record("msg_lookup_due")
        .expect("due backlog record");
    let outbox = due.outbox().expect("outbox record");
    assert_eq!(outbox.message_id().as_str(), "msg_lookup_due");
    assert_eq!(outbox.aggregate(), "thread");
    assert_eq!(outbox.aggregate_id(), "thread_lookup");
    assert_eq!(outbox.topic(), "agent_bus");
    assert_eq!(outbox.status(), OutboxMessageStatus::InFlight);
    assert_eq!(due.leased_by(), Some("worker-outbox-lookup"));
    assert!(!due.is_terminal());

    let done = state
        .backlog_snapshot("msg_lookup_done")
        .expect("done backlog snapshot");
    let done = done.outbox().expect("done outbox snapshot");
    assert_eq!(done.message_id().as_str(), "msg_lookup_done");
    assert_eq!(done.status(), OutboxMessageStatus::Delivered);
    assert!(done.is_terminal());

    let summary = state.operator_summary();
    let worker = summary
        .worker("worker-outbox-lookup")
        .expect("worker summary");
    let queue = worker
        .queue(DispatchQueue::Outbox)
        .expect("worker outbox queue");
    assert_eq!(queue.claimed(), 1);
    assert_eq!(summary.queue().unfinished(), 1);
}
