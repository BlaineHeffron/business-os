use std::time::{Duration, SystemTime};

use crate::{
    CorrelationId, DispatchQueue, ExecutionId, ExecutionJournal, ExecutionRecord,
    ExecutionRecordStatus, IdempotencyKey, InMemoryExecutionWorkStore, NoopTelemetry,
    OutboxEnvelope, OutboxMessage, OutboxMessageId, OutboxMessageStatus,
    WorkerDispatchCoordinatorConfig, WorkerDispatchCycleRequest, WorkerHeartbeatRequest,
    WorkerVisibility,
};

use super::{
    ReferenceActiveClaimRef, ReferenceClaimDisposition, ReferenceDispatchInspector,
    ReferenceDispatchRunner, ReferenceQueueInspectionRecord, ReferenceQueueInspectionSnapshot,
    ReferenceQueueInspectionSummary, ReferenceSchedulerStateQuery,
};

fn ctx() -> crate::ExecutionContext {
    crate::ExecutionContext::new(CorrelationId::new("corr_reference_runner"))
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

fn runner<'a>(store: &'a InMemoryExecutionWorkStore) -> ReferenceDispatchRunner<'a> {
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
fn scheduler_state_query_builds_execution_and_outbox_filters() {
    let observed_at = SystemTime::UNIX_EPOCH + Duration::from_secs(850);
    let query = ReferenceSchedulerStateQuery::new()
        .at(observed_at)
        .with_execution_status(ExecutionRecordStatus::Failed)
        .with_operation("send_email")
        .with_target("smtp")
        .with_outbox_status(OutboxMessageStatus::Pending)
        .with_aggregate("thread")
        .with_topic("agent_bus");

    let execution = query.execution_query(0);
    assert_eq!(execution.limit(), 1);
    assert_eq!(execution.observed_at(), observed_at);
    assert_eq!(execution.status(), Some(ExecutionRecordStatus::Failed));
    assert_eq!(execution.operation(), Some("send_email"));
    assert_eq!(execution.target(), Some("smtp"));

    let outbox = query.outbox_query(0);
    assert_eq!(outbox.limit(), 1);
    assert_eq!(outbox.observed_at(), observed_at);
    assert_eq!(outbox.status(), Some(OutboxMessageStatus::Pending));
    assert_eq!(outbox.aggregate(), Some("thread"));
    assert_eq!(outbox.topic(), Some("agent_bus"));
}

#[test]
fn claim_cycle_registers_active_work_and_snapshot_state() {
    let store = InMemoryExecutionWorkStore::new();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
    let ctx = ctx();
    let runner = runner(&store);

    ExecutionJournal::append(&store, execution_record("exec_1", now), &ctx).expect("append");
    ExecutionJournal::append(&store, execution_record("exec_2", now), &ctx).expect("append");

    let cycle = runner
        .claim(
            WorkerDispatchCycleRequest::new("worker-1", 2, Duration::from_secs(30)).at(now),
            &ctx,
        )
        .expect("claim cycle");

    assert_eq!(cycle.claims().len(), 2);
    assert_eq!(cycle.observation().heartbeat().active_leases(), 2);

    let active = runner.list_active_claims(&ctx).expect("active claims");
    assert_eq!(active.len(), 2);
    assert_eq!(active[0].worker_id(), "worker-1");

    let snapshot = runner
        .snapshot_worker(crate::WorkerSnapshotQuery::new("worker-1").at(now), &ctx)
        .expect("snapshot")
        .expect("worker snapshot");
    assert_eq!(snapshot.active_claims().len(), 2);
    assert_eq!(
        snapshot.snapshot().expect("dispatch snapshot").visibility(),
        WorkerVisibility::Visible
    );
    assert_eq!(
        snapshot
            .snapshot()
            .expect("dispatch snapshot")
            .queue(DispatchQueue::Execution)
            .expect("execution queue")
            .claimed_count(),
        2
    );
}

#[test]
fn heartbeat_exposes_idle_worker_visibility() {
    let store = InMemoryExecutionWorkStore::new();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(200);
    let ctx = ctx();
    let runner = runner(&store);

    let observation = runner
        .heartbeat(WorkerHeartbeatRequest::new("worker-idle").at(now), &ctx)
        .expect("heartbeat");
    assert_eq!(observation.heartbeat().worker_id(), "worker-idle");
    assert_eq!(observation.heartbeat().active_leases(), 0);

    let visible = runner
        .list_visible_workers(crate::VisibleWorkerListQuery::new().at(now), &ctx)
        .expect("visible workers");
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].worker_id(), "worker-idle");
}

#[test]
fn renew_replaces_the_cached_lease_and_complete_uses_the_new_handle() {
    let store = InMemoryExecutionWorkStore::new();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(300);
    let ctx = ctx();
    let runner = runner(&store);

    ExecutionJournal::append(&store, execution_record("exec_renew", now), &ctx).expect("append");

    let cycle = runner
        .claim(
            WorkerDispatchCycleRequest::new("worker-2", 1, Duration::from_secs(20)).at(now),
            &ctx,
        )
        .expect("claim cycle");
    let original = cycle.claims().first().expect("claim").clone();

    let renewal = runner
        .renew(
            ReferenceActiveClaimRef::new("worker-2", original.lease_id().clone()),
            now + Duration::from_secs(5),
            Duration::from_secs(25),
            &ctx,
        )
        .expect("renew");

    assert_eq!(
        renewal.renewed_claim().lease_expires_at(),
        now + Duration::from_secs(30)
    );
    assert_eq!(renewal.previous_claim().lease_id(), original.lease_id());
    assert_eq!(renewal.renewed_claim().lease_id(), original.lease_id());
    assert_eq!(
        runner
            .list_active_claims(&ctx)
            .expect("active claims")
            .len(),
        1
    );

    let outcome = runner
        .complete(
            ReferenceActiveClaimRef::new("worker-2", renewal.renewed_claim().lease_id().clone()),
            now + Duration::from_secs(7),
            &ctx,
        )
        .expect("complete");
    assert_eq!(outcome.disposition(), ReferenceClaimDisposition::Completed);
    assert!(runner
        .list_active_claims(&ctx)
        .expect("active claims")
        .is_empty());

    let stored = store
        .record(&ExecutionId::new("exec_renew"))
        .expect("stored record")
        .expect("execution");
    assert_eq!(stored.status(), ExecutionRecordStatus::Succeeded);
}

#[test]
fn retry_clears_the_active_claim_and_requeues_execution() {
    let store = InMemoryExecutionWorkStore::new();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(400);
    let ctx = ctx();
    let runner = runner(&store);

    ExecutionJournal::append(&store, execution_record("exec_retry", now), &ctx).expect("append");

    let cycle = runner
        .claim(
            WorkerDispatchCycleRequest::new("worker-3", 1, Duration::from_secs(30)).at(now),
            &ctx,
        )
        .expect("claim cycle");
    let claim = cycle.claims().first().expect("claim").clone();

    let outcome = runner
        .retry(
            ReferenceActiveClaimRef::new("worker-3", claim.lease_id().clone()),
            now + Duration::from_secs(2),
            now + Duration::from_secs(20),
            "temporary failure",
            &ctx,
        )
        .expect("retry");

    assert_eq!(outcome.disposition(), ReferenceClaimDisposition::Retried);
    assert!(runner
        .list_active_claims(&ctx)
        .expect("active claims")
        .is_empty());

    let stored = store
        .record(&ExecutionId::new("exec_retry"))
        .expect("stored record")
        .expect("execution");
    assert_eq!(stored.status(), ExecutionRecordStatus::Pending);
    assert_eq!(stored.available_at(), now + Duration::from_secs(20));
    assert_eq!(stored.last_error(), Some("temporary failure"));
}

#[test]
fn dead_letter_clears_the_active_claim_and_marks_execution() {
    let store = InMemoryExecutionWorkStore::new();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(500);
    let ctx = ctx();
    let runner = runner(&store);

    ExecutionJournal::append(&store, execution_record("exec_dead", now), &ctx).expect("append");

    let cycle = runner
        .claim(
            WorkerDispatchCycleRequest::new("worker-4", 1, Duration::from_secs(30)).at(now),
            &ctx,
        )
        .expect("claim cycle");
    let claim = cycle.claims().first().expect("claim").clone();

    let outcome = runner
        .dead_letter(
            ReferenceActiveClaimRef::new("worker-4", claim.lease_id().clone()),
            now + Duration::from_secs(3),
            "permanent failure",
            &ctx,
        )
        .expect("dead letter");

    assert_eq!(
        outcome.disposition(),
        ReferenceClaimDisposition::DeadLettered
    );
    assert!(runner
        .list_active_claims(&ctx)
        .expect("active claims")
        .is_empty());

    let stored = store
        .record(&ExecutionId::new("exec_dead"))
        .expect("stored record")
        .expect("execution");
    assert_eq!(stored.status(), ExecutionRecordStatus::DeadLettered);
    assert_eq!(stored.last_error(), Some("permanent failure"));
}

#[test]
fn active_claim_lookup_rejects_worker_mismatch() {
    let store = InMemoryExecutionWorkStore::new();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(600);
    let ctx = ctx();
    let runner = runner(&store);

    ExecutionJournal::append(&store, execution_record("exec_mismatch", now), &ctx).expect("append");

    let cycle = runner
        .claim(
            WorkerDispatchCycleRequest::new("worker-5", 1, Duration::from_secs(10)).at(now),
            &ctx,
        )
        .expect("claim cycle");
    let claim = cycle.claims().first().expect("claim").clone();

    let error = runner
        .renew(
            ReferenceActiveClaimRef::new("worker-other", claim.lease_id().clone()),
            now + Duration::from_secs(1),
            Duration::from_secs(10),
            &ctx,
        )
        .expect_err("mismatched worker should fail");
    assert_eq!(error.code(), "reference_runner_claim_owner_mismatch");
}

#[test]
fn list_worker_details_merges_snapshot_state_with_active_claims() {
    let store = InMemoryExecutionWorkStore::new();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(700);
    let ctx = ctx();
    let runner = runner(&store);

    ExecutionJournal::append(&store, execution_record("exec_details", now), &ctx).expect("append");

    runner
        .claim(
            WorkerDispatchCycleRequest::new("worker-details", 1, Duration::from_secs(20)).at(now),
            &ctx,
        )
        .expect("claim");

    let snapshots = runner
        .list_worker_details(crate::WorkerSnapshotListQuery::new().at(now), &ctx)
        .expect("worker details");
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].worker_id(), "worker-details");
    assert_eq!(snapshots[0].active_claim_count(), 1);
    assert_eq!(snapshots[0].claimed_work(), 1);
    assert_eq!(snapshots[0].due_work(), 0);
    assert_eq!(snapshots[0].visibility(), WorkerVisibility::Visible);
}

#[test]
fn outbox_runner_claims_retries_and_completes_messages() {
    let store = InMemoryExecutionWorkStore::new();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(800);
    let ctx = ctx();
    let runner = outbox_runner(&store);

    store
        .append_outbox(
            OutboxMessage::new(
                OutboxMessageId::new("msg_outbox_runner"),
                "thread",
                "thread_1",
                ExecutionId::new("exec_outbox_runner"),
                OutboxEnvelope::new("agent_bus", "thread_1", "{\"ok\":true}"),
            )
            .schedule_at(now),
            &ctx,
        )
        .expect("append outbox");

    let cycle = runner
        .claim(
            WorkerDispatchCycleRequest::new("worker-outbox", 1, Duration::from_secs(20)).at(now),
            &ctx,
        )
        .expect("claim outbox");
    let claim = cycle.claims().first().expect("claim").clone();
    assert_eq!(claim.queue(), DispatchQueue::Outbox);

    let retried = runner
        .retry(
            ReferenceActiveClaimRef::new("worker-outbox", claim.lease_id().clone()),
            now + Duration::from_secs(2),
            now + Duration::from_secs(15),
            "transport down",
            &ctx,
        )
        .expect("retry outbox");
    assert_eq!(retried.disposition(), ReferenceClaimDisposition::Retried);

    let reclaimed = runner
        .claim(
            WorkerDispatchCycleRequest::new("worker-outbox", 1, Duration::from_secs(20))
                .at(now + Duration::from_secs(16)),
            &ctx,
        )
        .expect("reclaim outbox");
    let reclaimed = reclaimed.claims().first().expect("claim").clone();

    let completed = runner
        .complete(
            ReferenceActiveClaimRef::new("worker-outbox", reclaimed.lease_id().clone()),
            now + Duration::from_secs(17),
            &ctx,
        )
        .expect("complete outbox");
    assert_eq!(
        completed.disposition(),
        ReferenceClaimDisposition::Completed
    );

    let detail = crate::OutboxInspectionStore::lookup_outbox_message(
        &store,
        &OutboxMessageId::new("msg_outbox_runner"),
        now + Duration::from_secs(17),
        &ctx,
    )
    .expect("lookup outbox")
    .expect("outbox record");
    assert_eq!(detail.status(), crate::OutboxMessageStatus::Delivered);
    assert_eq!(detail.message().attempts(), 2);
    assert_eq!(detail.last_error(), None);
}

#[test]
fn execution_inspector_combines_runner_and_backlog_state() {
    let store = InMemoryExecutionWorkStore::new();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(900);
    let ctx = ctx();
    let runner = runner(&store);

    ExecutionJournal::append(&store, execution_record("exec_ready", now), &ctx).expect("append");
    ExecutionJournal::append(
        &store,
        execution_record("exec_retry", now).schedule_retry(
            now + Duration::from_secs(1),
            now + Duration::from_secs(40),
            "temporary",
        ),
        &ctx,
    )
    .expect("append retry");

    runner
        .claim(
            WorkerDispatchCycleRequest::new("worker-inspect", 1, Duration::from_secs(15)).at(now),
            &ctx,
        )
        .expect("claim");

    let inspector = ReferenceDispatchInspector::for_executions(&runner, &store);
    let state = inspector
        .inspect(
            ReferenceSchedulerStateQuery::new()
                .at(now + Duration::from_secs(5))
                .with_backlog_limit(10),
            &ctx,
        )
        .expect("inspect");

    assert_eq!(state.summary().queue(), DispatchQueue::Execution);
    assert_eq!(state.summary().workers(), 1);
    assert_eq!(state.summary().active_claims(), 1);
    assert_eq!(state.summary().backlog().total(), 2);
    assert_eq!(state.summary().backlog().leased(), 1);
    assert_eq!(state.summary().backlog().retry_scheduled(), 1);
    assert!(matches!(
        state.summary().backlog(),
        ReferenceQueueInspectionSummary::Execution(_)
    ));
    assert_eq!(state.backlog().len(), 2);
    assert!(state
        .backlog()
        .iter()
        .any(|record| record.is_retry_scheduled_at(now + Duration::from_secs(5))));
}

#[test]
fn inspector_summary_counts_do_not_change_with_detail_limits() {
    let store = InMemoryExecutionWorkStore::new();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(940);
    let ctx = ctx();
    let runner = runner(&store);

    for execution_id in ["exec_limit_1", "exec_limit_2", "exec_limit_3"] {
        ExecutionJournal::append(&store, execution_record(execution_id, now), &ctx)
            .expect("append");
    }

    runner
        .claim(
            WorkerDispatchCycleRequest::new("worker-limit-1", 1, Duration::from_secs(30)).at(now),
            &ctx,
        )
        .expect("claim first");
    runner
        .claim(
            WorkerDispatchCycleRequest::new("worker-limit-2", 1, Duration::from_secs(30)).at(now),
            &ctx,
        )
        .expect("claim second");

    let inspector = ReferenceDispatchInspector::for_executions(&runner, &store);
    let summary = inspector
        .summarize(
            ReferenceSchedulerStateQuery::new()
                .at(now + Duration::from_secs(1))
                .with_worker_limit(1)
                .with_active_claim_limit(1),
            &ctx,
        )
        .expect("summarize");

    assert_eq!(summary.workers(), 2);
    assert_eq!(summary.workers_with_claims(), 2);
    assert_eq!(summary.active_claims(), 2);
    assert_eq!(summary.backlog().total(), 3);
    assert_eq!(summary.backlog().leased(), 2);
    assert_eq!(summary.backlog().due(), 1);
}

#[test]
fn operator_summary_exposes_stable_worker_and_queue_rollups() {
    let store = InMemoryExecutionWorkStore::new();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(960);
    let ctx = ctx();
    let runner = runner(&store);

    ExecutionJournal::append(&store, execution_record("exec_operator_ready", now), &ctx)
        .expect("append ready");
    ExecutionJournal::append(
        &store,
        execution_record("exec_operator_retry", now).schedule_retry(
            now + Duration::from_secs(1),
            now + Duration::from_secs(20),
            "temporary",
        ),
        &ctx,
    )
    .expect("append retry");

    runner
        .claim(
            WorkerDispatchCycleRequest::new("worker-operator", 1, Duration::from_secs(15)).at(now),
            &ctx,
        )
        .expect("claim");

    let inspector = ReferenceDispatchInspector::for_executions(&runner, &store);
    let summary = inspector
        .operator_summary(
            ReferenceSchedulerStateQuery::new()
                .at(now + Duration::from_secs(2))
                .with_worker_limit(1)
                .with_active_claim_limit(1)
                .with_backlog_limit(1),
            &ctx,
        )
        .expect("operator summary");

    assert_eq!(summary.scheduler().workers(), 1);
    assert_eq!(summary.scheduler().active_claims(), 1);
    assert_eq!(summary.queue().queue(), DispatchQueue::Execution);
    assert_eq!(summary.queue().total(), 2);
    assert_eq!(summary.queue().pending(), 1);
    assert_eq!(summary.queue().in_flight(), 1);
    assert_eq!(summary.queue().completed(), 0);
    assert_eq!(summary.queue().failed(), 0);
    assert_eq!(summary.queue().dead_lettered(), 0);
    assert_eq!(summary.queue().leased(), 1);
    assert_eq!(summary.queue().due(), 0);
    assert_eq!(summary.queue().retry_scheduled(), 1);

    let worker = summary.workers().first().expect("worker summary");
    assert_eq!(worker.worker_id(), "worker-operator");
    assert_eq!(worker.visibility(), WorkerVisibility::Visible);
    assert_eq!(worker.active_claims(), 1);
    assert_eq!(worker.claimed_work(), 1);
    assert_eq!(worker.due_work(), 0);
    let execution_queue = worker
        .queues()
        .iter()
        .find(|queue| queue.queue() == DispatchQueue::Execution)
        .expect("execution queue summary");
    assert_eq!(execution_queue.claimed(), 1);
    assert_eq!(execution_queue.due(), 0);
}

#[test]
fn inspector_helpers_expose_queue_worker_and_backlog_snapshots() {
    let store = InMemoryExecutionWorkStore::new();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(970);
    let ctx = ctx();
    let runner = runner(&store);

    ExecutionJournal::append(&store, execution_record("exec_helper_ready", now), &ctx)
        .expect("append ready");
    ExecutionJournal::append(
        &store,
        execution_record("exec_helper_retry", now).schedule_retry(
            now + Duration::from_secs(1),
            now + Duration::from_secs(25),
            "helper temporary",
        ),
        &ctx,
    )
    .expect("append retry");

    runner
        .claim(
            WorkerDispatchCycleRequest::new("worker-helper", 1, Duration::from_secs(15)).at(now),
            &ctx,
        )
        .expect("claim");

    let observed_at = now + Duration::from_secs(3);
    let inspector = ReferenceDispatchInspector::for_executions(&runner, &store);
    let query = ReferenceSchedulerStateQuery::new()
        .at(observed_at)
        .with_worker_limit(1)
        .with_backlog_limit(10)
        .with_active_claim_limit(1);

    let queue = inspector
        .queue_summary(query.clone(), &ctx)
        .expect("queue summary");
    assert_eq!(queue.queue(), DispatchQueue::Execution);
    assert_eq!(queue.total(), 2);
    assert_eq!(queue.in_flight(), 1);
    assert_eq!(queue.retry_scheduled(), 1);

    let workers = inspector
        .worker_summaries(query.clone(), &ctx)
        .expect("worker summaries");
    assert_eq!(workers.len(), 1);
    assert_eq!(workers[0].worker_id(), "worker-helper");
    assert_eq!(workers[0].active_claims(), 1);
    assert_eq!(workers[0].claimed_work(), 1);

    let snapshots = inspector
        .backlog_snapshots(query, &ctx)
        .expect("backlog snapshots");
    assert_eq!(snapshots.len(), 2);
    assert!(snapshots.iter().any(|snapshot| {
        snapshot.queue() == DispatchQueue::Execution
            && snapshot.id() == "exec_helper_ready"
            && snapshot.active_lease()
            && snapshot.leased_by() == Some("worker-helper")
            && snapshot.execution().is_some()
    }));
    assert!(snapshots.iter().any(|snapshot| {
        snapshot.id() == "exec_helper_retry"
            && snapshot.retry_scheduled()
            && snapshot.last_error() == Some("helper temporary")
    }));
}

#[test]
fn inspector_helpers_lookup_worker_and_active_execution_claim_snapshots() {
    let store = InMemoryExecutionWorkStore::new();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(975);
    let ctx = ctx();
    let runner = runner(&store);

    ExecutionJournal::append(&store, execution_record("exec_active", now), &ctx)
        .expect("append active");
    ExecutionJournal::append(&store, execution_record("exec_idle", now), &ctx)
        .expect("append idle");

    runner
        .claim(
            WorkerDispatchCycleRequest::new("worker-active", 1, Duration::from_secs(20)).at(now),
            &ctx,
        )
        .expect("claim active");

    let observed_at = now + Duration::from_secs(2);
    let inspector = ReferenceDispatchInspector::for_executions(&runner, &store);
    let query = ReferenceSchedulerStateQuery::new()
        .at(observed_at)
        .with_active_claim_limit(10)
        .with_execution_status(ExecutionRecordStatus::InFlight)
        .with_target("target_exec_active");

    let worker = inspector
        .worker_summary("worker-active", query.clone(), &ctx)
        .expect("worker summary")
        .expect("worker summary exists");
    assert_eq!(worker.worker_id(), "worker-active");
    assert_eq!(worker.visibility(), WorkerVisibility::Visible);
    assert_eq!(worker.active_claims(), 1);
    assert_eq!(worker.claimed_work(), 1);

    let missing = inspector
        .worker_summary("worker-missing", query.clone(), &ctx)
        .expect("missing worker summary");
    assert!(missing.is_none());

    let claims = inspector
        .active_claim_snapshots(query.clone(), &ctx)
        .expect("active claim snapshots");
    assert_eq!(claims.len(), 1);
    let claim = &claims[0];
    assert_eq!(claim.queue(), DispatchQueue::Execution);
    assert_eq!(claim.worker_id(), "worker-active");
    assert_eq!(claim.id(), "exec_active");
    assert_eq!(claim.leased_by(), Some("worker-active"));
    assert!(claim.active_lease());
    assert!(claim.execution().is_some());
    assert!(claim.outbox().is_none());

    let filtered = inspector
        .active_claim_snapshots(query.with_target("target_other"), &ctx)
        .expect("filtered active claim snapshots");
    assert!(filtered.is_empty());
}

#[test]
fn execution_claim_helpers_lookup_worker_scoped_claims_and_summary() {
    let store = InMemoryExecutionWorkStore::new();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(977);
    let ctx = ctx();
    let runner = runner(&store);

    ExecutionJournal::append(&store, execution_record("exec_claim_primary", now), &ctx)
        .expect("append primary");
    ExecutionJournal::append(
        &store,
        execution_record("exec_claim_stale", now + Duration::from_secs(1)),
        &ctx,
    )
    .expect("append stale");
    ExecutionJournal::append(
        &store,
        execution_record("exec_claim_idle", now + Duration::from_secs(2)),
        &ctx,
    )
    .expect("append idle");

    let primary = runner
        .claim(
            WorkerDispatchCycleRequest::new("worker-claims", 1, Duration::from_secs(20)).at(now),
            &ctx,
        )
        .expect("claim primary")
        .claims()
        .first()
        .expect("primary claim")
        .clone();
    let stale = runner
        .claim(
            WorkerDispatchCycleRequest::new("worker-stale", 1, Duration::from_secs(2))
                .at(now + Duration::from_secs(1)),
            &ctx,
        )
        .expect("claim stale")
        .claims()
        .first()
        .expect("stale claim")
        .clone();

    let observed_at = now + Duration::from_secs(5);
    let inspector = ReferenceDispatchInspector::for_executions(&runner, &store);
    let query = ReferenceSchedulerStateQuery::new()
        .at(observed_at)
        .with_active_claim_limit(10)
        .with_execution_status(ExecutionRecordStatus::InFlight);

    let worker_claims = inspector
        .worker_active_claim_snapshots("worker-claims", query.clone(), &ctx)
        .expect("worker active claims");
    assert_eq!(worker_claims.len(), 1);
    assert_eq!(worker_claims[0].id(), "exec_claim_primary");
    assert_eq!(worker_claims[0].worker_id(), "worker-claims");
    assert!(worker_claims[0].active_lease());

    let stale_worker_claims = inspector
        .worker_active_claim_snapshots("worker-stale", query.clone(), &ctx)
        .expect("stale worker claims");
    assert_eq!(stale_worker_claims.len(), 1);
    assert_eq!(stale_worker_claims[0].id(), "exec_claim_stale");
    assert!(stale_worker_claims[0].stale_lease());

    let missing_worker_claims = inspector
        .worker_active_claim_snapshots("worker-missing", query.clone(), &ctx)
        .expect("missing worker claims");
    assert!(missing_worker_claims.is_empty());

    let primary_snapshot = inspector
        .active_claim_snapshot(
            &ReferenceActiveClaimRef::new("worker-claims", primary.lease_id().clone()),
            observed_at,
            &ctx,
        )
        .expect("lookup active claim")
        .expect("claim snapshot");
    assert_eq!(primary_snapshot.id(), "exec_claim_primary");
    assert_eq!(primary_snapshot.leased_by(), Some("worker-claims"));
    assert!(primary_snapshot.active_lease());

    let missing_snapshot = inspector
        .active_claim_snapshot(
            &ReferenceActiveClaimRef::new("worker-claims", crate::LeaseId::new("lease_missing")),
            observed_at,
            &ctx,
        )
        .expect("lookup missing claim");
    assert!(missing_snapshot.is_none());

    let summary = inspector
        .active_claim_summary(query, &ctx)
        .expect("active claim summary");
    assert_eq!(summary.queue(), DispatchQueue::Execution);
    assert_eq!(summary.observed_at(), observed_at);
    assert_eq!(summary.total(), 2);
    assert_eq!(summary.workers(), 2);
    assert_eq!(summary.active_leases(), 1);
    assert_eq!(summary.stale_leases(), 1);
    assert_eq!(summary.due_work(), 1);
    assert_eq!(summary.retry_scheduled(), 0);
    assert_eq!(summary.oldest_leased_at(), Some(now));
    assert_eq!(
        summary.earliest_lease_expiry(),
        Some(now + Duration::from_secs(3))
    );

    let stale_snapshot = inspector
        .active_claim_snapshot(
            &ReferenceActiveClaimRef::new("worker-stale", stale.lease_id().clone()),
            observed_at,
            &ctx,
        )
        .expect("lookup stale claim")
        .expect("stale claim snapshot");
    assert!(stale_snapshot.stale_lease());
    assert!(stale_snapshot.due());
}

#[test]
fn outbox_inspector_filters_operator_backlog_queries() {
    let store = InMemoryExecutionWorkStore::new();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(980);
    let ctx = ctx();
    let runner = outbox_runner(&store);

    store
        .append_outbox(
            OutboxMessage::new(
                OutboxMessageId::new("msg_delivery_1"),
                "thread",
                "thread_1",
                ExecutionId::new("exec_delivery_1"),
                OutboxEnvelope::new("agent_bus", "thread_1", "{\"ok\":true}"),
            )
            .schedule_at(now),
            &ctx,
        )
        .expect("append first");
    store
        .append_outbox(
            OutboxMessage::new(
                OutboxMessageId::new("msg_delivery_2"),
                "approval",
                "approval_1",
                ExecutionId::new("exec_delivery_2"),
                OutboxEnvelope::new("audit_log", "approval_1", "{\"ok\":true}"),
            )
            .schedule_at(now + Duration::from_secs(30)),
            &ctx,
        )
        .expect("append second");

    let inspector = ReferenceDispatchInspector::for_outbox(&runner, &store);
    let state = inspector
        .inspect(
            ReferenceSchedulerStateQuery::new()
                .at(now + Duration::from_secs(1))
                .with_topic("agent_bus")
                .with_outbox_status(OutboxMessageStatus::Pending),
            &ctx,
        )
        .expect("inspect outbox");

    assert_eq!(state.summary().queue(), DispatchQueue::Outbox);
    assert_eq!(state.summary().backlog().total(), 1);
    assert_eq!(state.backlog().len(), 1);
    assert!(matches!(
        &state.backlog()[0],
        ReferenceQueueInspectionRecord::Outbox(record)
            if record.message().envelope().topic() == "agent_bus"
    ));
}

#[test]
fn outbox_backlog_snapshots_use_stable_queue_snapshot_shape() {
    let store = InMemoryExecutionWorkStore::new();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(990);
    let ctx = ctx();
    let runner = outbox_runner(&store);

    store
        .append_outbox(
            OutboxMessage::new(
                OutboxMessageId::new("msg_snapshot_retry"),
                "approval",
                "approval_1",
                ExecutionId::new("exec_snapshot_retry"),
                OutboxEnvelope::new("audit_log", "approval_1", "{\"ok\":true}"),
            )
            .schedule_at(now),
            &ctx,
        )
        .expect("append retry");

    let retry_claim = runner
        .claim(
            WorkerDispatchCycleRequest::new("worker-outbox-helper", 1, Duration::from_secs(15))
                .at(now),
            &ctx,
        )
        .expect("claim retry")
        .claims()
        .first()
        .expect("retry claim")
        .clone();
    runner
        .retry(
            ReferenceActiveClaimRef::new("worker-outbox-helper", retry_claim.lease_id().clone()),
            now + Duration::from_secs(1),
            now + Duration::from_secs(20),
            "transport",
            &ctx,
        )
        .expect("retry outbox");

    store
        .append_outbox(
            OutboxMessage::new(
                OutboxMessageId::new("msg_snapshot_due"),
                "thread",
                "thread_1",
                ExecutionId::new("exec_snapshot_due"),
                OutboxEnvelope::new("agent_bus", "thread_1", "{\"ok\":true}"),
            )
            .schedule_at(now),
            &ctx,
        )
        .expect("append due");

    runner
        .claim(
            WorkerDispatchCycleRequest::new("worker-outbox-helper", 1, Duration::from_secs(15))
                .at(now + Duration::from_secs(2)),
            &ctx,
        )
        .expect("claim");

    let snapshots = ReferenceDispatchInspector::for_outbox(&runner, &store)
        .backlog_snapshots(
            ReferenceSchedulerStateQuery::new()
                .at(now + Duration::from_secs(3))
                .with_backlog_limit(10),
            &ctx,
        )
        .expect("backlog snapshots");

    assert_eq!(snapshots.len(), 2);
    assert!(snapshots.iter().any(|snapshot| {
        matches!(
            snapshot,
            ReferenceQueueInspectionSnapshot::Outbox(detail)
                if detail.message_id().as_str() == "msg_snapshot_due"
                    && snapshot.queue() == DispatchQueue::Outbox
                    && snapshot.active_lease()
                    && snapshot.leased_by() == Some("worker-outbox-helper")
                    && detail.topic() == "agent_bus"
        )
    }));
    assert!(snapshots.iter().any(|snapshot| {
        matches!(
            snapshot,
            ReferenceQueueInspectionSnapshot::Outbox(detail)
                if detail.message_id().as_str() == "msg_snapshot_retry"
                    && snapshot.retry_scheduled()
                    && snapshot.last_error() == Some("transport")
                    && detail.aggregate() == "approval"
        )
    }));
}

#[test]
fn outbox_inspector_helpers_filter_active_claim_snapshots_and_worker_visibility() {
    let store = InMemoryExecutionWorkStore::new();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(995);
    let ctx = ctx();
    let runner = outbox_runner(&store);

    store
        .append_outbox(
            OutboxMessage::new(
                OutboxMessageId::new("msg_active_helper"),
                "thread",
                "thread_1",
                ExecutionId::new("exec_active_helper"),
                OutboxEnvelope::new("agent_bus", "thread_1", "{\"ok\":true}"),
            )
            .schedule_at(now),
            &ctx,
        )
        .expect("append active outbox");

    runner
        .claim(
            WorkerDispatchCycleRequest::new("worker-outbox-active", 1, Duration::from_secs(20))
                .at(now),
            &ctx,
        )
        .expect("claim outbox");

    let inspector = ReferenceDispatchInspector::for_outbox(&runner, &store);
    let visible_query = ReferenceSchedulerStateQuery::new()
        .at(now + Duration::from_secs(2))
        .with_topic("agent_bus")
        .with_active_claim_limit(10);

    let claims = inspector
        .active_claim_snapshots(visible_query.clone(), &ctx)
        .expect("active outbox claim snapshots");
    assert_eq!(claims.len(), 1);
    let claim = &claims[0];
    assert_eq!(claim.queue(), DispatchQueue::Outbox);
    assert_eq!(claim.id(), "msg_active_helper");
    assert_eq!(claim.worker_id(), "worker-outbox-active");
    assert_eq!(claim.leased_by(), Some("worker-outbox-active"));
    assert!(claim.active_lease());
    assert!(claim.execution().is_none());
    assert!(claim.outbox().is_some());

    let filtered = inspector
        .active_claim_snapshots(visible_query.clone().with_topic("audit_log"), &ctx)
        .expect("filtered outbox claim snapshots");
    assert!(filtered.is_empty());

    let expired = inspector
        .worker_summary(
            "worker-outbox-active",
            ReferenceSchedulerStateQuery::new()
                .at(now + Duration::from_secs(12))
                .with_expired_workers(false),
            &ctx,
        )
        .expect("expired worker query");
    assert!(expired.is_none());
}

#[test]
fn outbox_claim_helpers_lookup_worker_scoped_claims_and_summary() {
    let store = InMemoryExecutionWorkStore::new();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(997);
    let ctx = ctx();
    let runner = outbox_runner(&store);

    store
        .append_outbox(
            OutboxMessage::new(
                OutboxMessageId::new("msg_claim_primary"),
                "thread",
                "thread_1",
                ExecutionId::new("exec_claim_primary"),
                OutboxEnvelope::new("agent_bus", "thread_1", "{\"ok\":true}"),
            )
            .schedule_at(now),
            &ctx,
        )
        .expect("append primary outbox");
    store
        .append_outbox(
            OutboxMessage::new(
                OutboxMessageId::new("msg_claim_stale"),
                "approval",
                "approval_1",
                ExecutionId::new("exec_claim_stale"),
                OutboxEnvelope::new("audit_log", "approval_1", "{\"ok\":true}"),
            )
            .schedule_at(now),
            &ctx,
        )
        .expect("append stale outbox");

    let primary = runner
        .claim(
            WorkerDispatchCycleRequest::new("worker-outbox-claims", 1, Duration::from_secs(20))
                .at(now),
            &ctx,
        )
        .expect("claim primary outbox")
        .claims()
        .first()
        .expect("primary outbox claim")
        .clone();
    let stale = runner
        .claim(
            WorkerDispatchCycleRequest::new("worker-outbox-stale", 1, Duration::from_secs(2))
                .at(now),
            &ctx,
        )
        .expect("claim stale outbox")
        .claims()
        .first()
        .expect("stale outbox claim")
        .clone();

    let observed_at = now + Duration::from_secs(5);
    let inspector = ReferenceDispatchInspector::for_outbox(&runner, &store);
    let query = ReferenceSchedulerStateQuery::new()
        .at(observed_at)
        .with_active_claim_limit(10);

    let worker_claims = inspector
        .worker_active_claim_snapshots("worker-outbox-claims", query.clone(), &ctx)
        .expect("worker outbox claims");
    assert_eq!(worker_claims.len(), 1);
    assert_eq!(worker_claims[0].id(), "msg_claim_primary");
    assert_eq!(worker_claims[0].worker_id(), "worker-outbox-claims");
    assert!(worker_claims[0].active_lease());

    let primary_snapshot = inspector
        .active_claim_snapshot(
            &ReferenceActiveClaimRef::new("worker-outbox-claims", primary.lease_id().clone()),
            observed_at,
            &ctx,
        )
        .expect("lookup active outbox claim")
        .expect("active outbox claim snapshot");
    assert_eq!(primary_snapshot.id(), "msg_claim_primary");
    assert_eq!(primary_snapshot.leased_by(), Some("worker-outbox-claims"));
    assert!(primary_snapshot.outbox().is_some());

    let stale_snapshot = inspector
        .active_claim_snapshot(
            &ReferenceActiveClaimRef::new("worker-outbox-stale", stale.lease_id().clone()),
            observed_at,
            &ctx,
        )
        .expect("lookup stale outbox claim")
        .expect("stale outbox claim snapshot");
    assert_eq!(stale_snapshot.id(), "msg_claim_stale");
    assert!(stale_snapshot.stale_lease());
    assert!(stale_snapshot.due());

    let summary = inspector
        .active_claim_summary(query, &ctx)
        .expect("outbox claim summary");
    assert_eq!(summary.queue(), DispatchQueue::Outbox);
    assert_eq!(summary.total(), 2);
    assert_eq!(summary.workers(), 2);
    assert_eq!(summary.active_leases(), 1);
    assert_eq!(summary.stale_leases(), 1);
    assert_eq!(summary.due_work(), 1);
    assert_eq!(summary.retry_scheduled(), 0);
    assert_eq!(summary.oldest_leased_at(), Some(now));
    assert_eq!(
        summary.earliest_lease_expiry(),
        Some(now + Duration::from_secs(2))
    );
}
