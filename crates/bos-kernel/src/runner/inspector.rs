use std::time::SystemTime;

use crate::{
    AppResult, ClaimedDispatchWork, ExecutionContext, ExecutionId, ExecutionInspectionQuery,
    ExecutionInspectionStore, OutboxInspectionQuery, OutboxInspectionStore, OutboxMessageId,
    WorkerSnapshotListQuery, WorkerSnapshotQuery, WorkerVisibility,
};

use super::{
    ReferenceActiveClaimRef, ReferenceActiveClaimSnapshot, ReferenceActiveClaimSummary,
    ReferenceDispatchRunner, ReferenceOperatorSummary, ReferenceQueueInspectionRecord,
    ReferenceQueueInspectionSnapshot, ReferenceQueueInspectionSummary, ReferenceQueueSummary,
    ReferenceSchedulerState, ReferenceSchedulerStateQuery, ReferenceSchedulerStateSummary,
    ReferenceWorkClaim, ReferenceWorkerSnapshot, ReferenceWorkerSummary,
};

pub struct ReferenceDispatchInspector<'a> {
    runner: &'a ReferenceDispatchRunner<'a>,
    store: ReferenceInspectionStore<'a>,
}

enum ReferenceInspectionStore<'a> {
    Execution(&'a dyn ExecutionInspectionStore),
    Outbox(&'a dyn OutboxInspectionStore),
}

impl<'a> ReferenceDispatchInspector<'a> {
    pub fn for_executions(
        runner: &'a ReferenceDispatchRunner<'a>,
        store: &'a dyn ExecutionInspectionStore,
    ) -> Self {
        Self {
            runner,
            store: ReferenceInspectionStore::Execution(store),
        }
    }

    pub fn for_outbox(
        runner: &'a ReferenceDispatchRunner<'a>,
        store: &'a dyn OutboxInspectionStore,
    ) -> Self {
        Self {
            runner,
            store: ReferenceInspectionStore::Outbox(store),
        }
    }

    pub fn summarize(
        &self,
        query: ReferenceSchedulerStateQuery,
        ctx: &ExecutionContext,
    ) -> AppResult<ReferenceSchedulerStateSummary> {
        let workers = self.list_workers_for_summary(&query, ctx)?;
        let active_claims = self.list_claims_for_summary(ctx)?;
        let backlog = self.summarize_backlog(&query, ctx)?;
        Ok(ReferenceSchedulerStateSummary::new(
            self.runner.config().queue(),
            self.runner.config().scope(),
            query.observed_at(),
            &workers,
            &active_claims,
            backlog,
        ))
    }

    pub fn inspect(
        &self,
        query: ReferenceSchedulerStateQuery,
        ctx: &ExecutionContext,
    ) -> AppResult<ReferenceSchedulerState> {
        let summary_workers = self.list_workers_for_summary(&query, ctx)?;
        let summary_claims = self.list_claims_for_summary(ctx)?;
        let workers = self.list_workers(&query, ctx)?;
        let active_claims = self.list_claims(&query, ctx)?;
        let backlog_summary = self.summarize_backlog(&query, ctx)?;
        let backlog = self.list_backlog(&query, ctx)?;
        let summary = ReferenceSchedulerStateSummary::new(
            self.runner.config().queue(),
            self.runner.config().scope(),
            query.observed_at(),
            &summary_workers,
            &summary_claims,
            backlog_summary,
        );

        Ok(ReferenceSchedulerState::new(
            summary,
            workers,
            active_claims,
            backlog,
        ))
    }

    pub fn operator_summary(
        &self,
        query: ReferenceSchedulerStateQuery,
        ctx: &ExecutionContext,
    ) -> AppResult<ReferenceOperatorSummary> {
        let state = self.inspect(query, ctx)?;
        Ok(state.operator_summary())
    }

    pub fn queue_summary(
        &self,
        query: ReferenceSchedulerStateQuery,
        ctx: &ExecutionContext,
    ) -> AppResult<ReferenceQueueSummary> {
        let observed_at = query.observed_at();
        let backlog = self.summarize_backlog(&query, ctx)?;
        Ok(backlog.operator_summary(self.runner.config().queue(), observed_at))
    }

    pub fn worker_snapshot(
        &self,
        worker_id: impl Into<String>,
        query: ReferenceSchedulerStateQuery,
        ctx: &ExecutionContext,
    ) -> AppResult<Option<ReferenceWorkerSnapshot>> {
        let snapshot = self.runner.snapshot_worker(
            WorkerSnapshotQuery::new(worker_id).at(query.observed_at()),
            ctx,
        )?;

        if !query.include_expired_workers()
            && snapshot
                .as_ref()
                .is_some_and(|worker| worker.visibility() == WorkerVisibility::Expired)
        {
            return Ok(None);
        }

        Ok(snapshot)
    }

    pub fn worker_summary(
        &self,
        worker_id: impl Into<String>,
        query: ReferenceSchedulerStateQuery,
        ctx: &ExecutionContext,
    ) -> AppResult<Option<ReferenceWorkerSummary>> {
        Ok(self
            .worker_snapshot(worker_id, query, ctx)?
            .map(|worker| worker.operator_summary()))
    }

    pub fn worker_summaries(
        &self,
        query: ReferenceSchedulerStateQuery,
        ctx: &ExecutionContext,
    ) -> AppResult<Vec<ReferenceWorkerSummary>> {
        Ok(self
            .list_workers(&query, ctx)?
            .into_iter()
            .map(|worker| worker.operator_summary())
            .collect())
    }

    pub fn active_claim_snapshots(
        &self,
        query: ReferenceSchedulerStateQuery,
        ctx: &ExecutionContext,
    ) -> AppResult<Vec<ReferenceActiveClaimSnapshot>> {
        let mut snapshots = self.list_active_claim_snapshots(&query, ctx)?;
        snapshots.truncate(query.active_claim_limit());
        Ok(snapshots)
    }

    pub fn active_claim_summary(
        &self,
        query: ReferenceSchedulerStateQuery,
        ctx: &ExecutionContext,
    ) -> AppResult<ReferenceActiveClaimSummary> {
        let claims = self.list_active_claim_snapshots(&query, ctx)?;
        Ok(ReferenceActiveClaimSummary::new(
            self.runner.config().queue(),
            query.observed_at(),
            &claims,
        ))
    }

    pub fn worker_active_claim_snapshots(
        &self,
        worker_id: impl Into<String>,
        query: ReferenceSchedulerStateQuery,
        ctx: &ExecutionContext,
    ) -> AppResult<Vec<ReferenceActiveClaimSnapshot>> {
        let worker_id = worker_id.into();
        let mut claims = self.list_active_claim_snapshots(&query, ctx)?;
        claims.retain(|claim| claim.worker_id() == worker_id);
        claims.truncate(query.active_claim_limit());
        Ok(claims)
    }

    pub fn active_claim_snapshot(
        &self,
        claim: &ReferenceActiveClaimRef,
        observed_at: SystemTime,
        ctx: &ExecutionContext,
    ) -> AppResult<Option<ReferenceActiveClaimSnapshot>> {
        let registered = match self.runner.lookup_claim(claim, ctx) {
            Ok(registered) => registered,
            Err(error) if error.code() == "reference_runner_claim_not_found" => return Ok(None),
            Err(error) => return Err(error),
        };

        match (&self.store, &registered.claimed) {
            (
                ReferenceInspectionStore::Execution(store),
                ClaimedDispatchWork::Execution(claimed),
            ) => Ok(store
                .lookup_execution_inspection(claimed.record().id(), observed_at, ctx)?
                .map(|record| {
                    ReferenceActiveClaimSnapshot::new(
                        ReferenceWorkClaim::from_claimed(&registered.claimed),
                        ReferenceQueueInspectionSnapshot::Execution(
                            record.snapshot_at(observed_at),
                        ),
                    )
                })),
            (ReferenceInspectionStore::Outbox(store), ClaimedDispatchWork::Outbox(claimed)) => {
                Ok(store
                    .lookup_outbox_message(claimed.message().id(), observed_at, ctx)?
                    .map(|record| {
                        ReferenceActiveClaimSnapshot::new(
                            ReferenceWorkClaim::from_claimed(&registered.claimed),
                            ReferenceQueueInspectionSnapshot::Outbox(
                                record.snapshot_at(observed_at),
                            ),
                        )
                    }))
            }
            (ReferenceInspectionStore::Execution(_), ClaimedDispatchWork::Outbox(_))
            | (ReferenceInspectionStore::Outbox(_), ClaimedDispatchWork::Execution(_)) => Ok(None),
        }
    }

    pub fn backlog_snapshots(
        &self,
        query: ReferenceSchedulerStateQuery,
        ctx: &ExecutionContext,
    ) -> AppResult<Vec<ReferenceQueueInspectionSnapshot>> {
        let observed_at = query.observed_at();
        Ok(self
            .list_backlog(&query, ctx)?
            .into_iter()
            .map(|record| record.snapshot_at(observed_at))
            .collect())
    }

    fn list_workers(
        &self,
        query: &ReferenceSchedulerStateQuery,
        ctx: &ExecutionContext,
    ) -> AppResult<Vec<ReferenceWorkerSnapshot>> {
        self.runner.list_worker_details(
            WorkerSnapshotListQuery::new()
                .at(query.observed_at())
                .with_limit(query.worker_limit())
                .with_expired_workers(query.include_expired_workers()),
            ctx,
        )
    }

    fn list_workers_for_summary(
        &self,
        query: &ReferenceSchedulerStateQuery,
        ctx: &ExecutionContext,
    ) -> AppResult<Vec<ReferenceWorkerSnapshot>> {
        self.runner.list_worker_details(
            WorkerSnapshotListQuery::new()
                .at(query.observed_at())
                .with_limit(usize::MAX)
                .with_expired_workers(query.include_expired_workers()),
            ctx,
        )
    }

    fn list_claims(
        &self,
        query: &ReferenceSchedulerStateQuery,
        ctx: &ExecutionContext,
    ) -> AppResult<Vec<ReferenceWorkClaim>> {
        let mut claims = self.runner.list_active_claims(ctx)?;
        claims.truncate(query.active_claim_limit());
        Ok(claims)
    }

    fn list_claims_for_summary(
        &self,
        ctx: &ExecutionContext,
    ) -> AppResult<Vec<ReferenceWorkClaim>> {
        self.runner.list_active_claims(ctx)
    }

    fn list_active_claim_snapshots(
        &self,
        query: &ReferenceSchedulerStateQuery,
        ctx: &ExecutionContext,
    ) -> AppResult<Vec<ReferenceActiveClaimSnapshot>> {
        let observed_at = query.observed_at();
        let claims = self.list_claims_for_summary(ctx)?;

        match self.store {
            ReferenceInspectionStore::Execution(store) => {
                let inspection_query = execution_query(query, usize::MAX);
                let mut snapshots = Vec::new();

                for claim in claims {
                    let Some(record) = store.lookup_execution_inspection(
                        &ExecutionId::new(claim.work_id()),
                        observed_at,
                        ctx,
                    )?
                    else {
                        continue;
                    };

                    if inspection_query.matches_inspection(&record) {
                        snapshots.push(ReferenceActiveClaimSnapshot::new(
                            claim,
                            ReferenceQueueInspectionSnapshot::Execution(
                                record.snapshot_at(observed_at),
                            ),
                        ));
                    }
                }

                Ok(snapshots)
            }
            ReferenceInspectionStore::Outbox(store) => {
                let inspection_query = outbox_query(query, usize::MAX);
                let mut snapshots = Vec::new();

                for claim in claims {
                    let Some(record) = store.lookup_outbox_message(
                        &OutboxMessageId::new(claim.work_id()),
                        observed_at,
                        ctx,
                    )?
                    else {
                        continue;
                    };

                    if inspection_query.matches_inspection(&record) {
                        snapshots.push(ReferenceActiveClaimSnapshot::new(
                            claim,
                            ReferenceQueueInspectionSnapshot::Outbox(
                                record.snapshot_at(observed_at),
                            ),
                        ));
                    }
                }

                Ok(snapshots)
            }
        }
    }

    fn summarize_backlog(
        &self,
        query: &ReferenceSchedulerStateQuery,
        ctx: &ExecutionContext,
    ) -> AppResult<ReferenceQueueInspectionSummary> {
        match self.store {
            ReferenceInspectionStore::Execution(store) => {
                Ok(ReferenceQueueInspectionSummary::Execution(
                    store.summarize_executions(execution_query(query, usize::MAX), ctx)?,
                ))
            }
            ReferenceInspectionStore::Outbox(store) => Ok(ReferenceQueueInspectionSummary::Outbox(
                store.summarize_outbox(outbox_query(query, usize::MAX), ctx)?,
            )),
        }
    }

    fn list_backlog(
        &self,
        query: &ReferenceSchedulerStateQuery,
        ctx: &ExecutionContext,
    ) -> AppResult<Vec<ReferenceQueueInspectionRecord>> {
        match self.store {
            ReferenceInspectionStore::Execution(store) => Ok(store
                .list_execution_inspection(execution_query(query, query.backlog_limit()), ctx)?
                .into_iter()
                .map(ReferenceQueueInspectionRecord::Execution)
                .collect()),
            ReferenceInspectionStore::Outbox(store) => Ok(store
                .list_outbox_messages(outbox_query(query, query.backlog_limit()), ctx)?
                .into_iter()
                .map(ReferenceQueueInspectionRecord::Outbox)
                .collect()),
        }
    }
}

fn execution_query(query: &ReferenceSchedulerStateQuery, limit: usize) -> ExecutionInspectionQuery {
    query.execution_query(limit)
}

fn outbox_query(query: &ReferenceSchedulerStateQuery, limit: usize) -> OutboxInspectionQuery {
    query.outbox_query(limit)
}
