use std::time::{Duration, SystemTime};

use crate::{AppError, AppResult, ErrorCode, ExecutionContext};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerVisibility {
    Visible,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerHeartbeat {
    worker_id: String,
    scope: String,
    recorded_at: SystemTime,
    visible_until: SystemTime,
    active_leases: usize,
}

impl WorkerHeartbeat {
    pub fn new(
        worker_id: impl Into<String>,
        scope: impl Into<String>,
        recorded_at: SystemTime,
        visible_until: SystemTime,
    ) -> Self {
        Self {
            worker_id: worker_id.into(),
            scope: scope.into(),
            recorded_at,
            visible_until,
            active_leases: 0,
        }
    }

    pub fn from_ttl(
        worker_id: impl Into<String>,
        scope: impl Into<String>,
        recorded_at: SystemTime,
        visibility_ttl: Duration,
    ) -> Self {
        Self::new(
            worker_id,
            scope,
            recorded_at,
            recorded_at
                .checked_add(visibility_ttl)
                .unwrap_or(recorded_at),
        )
    }

    pub fn worker_id(&self) -> &str {
        &self.worker_id
    }

    pub fn scope(&self) -> &str {
        &self.scope
    }

    pub fn recorded_at(&self) -> SystemTime {
        self.recorded_at
    }

    pub fn visible_until(&self) -> SystemTime {
        self.visible_until
    }

    pub fn active_leases(&self) -> usize {
        self.active_leases
    }

    pub fn with_active_leases(mut self, active_leases: usize) -> Self {
        self.active_leases = active_leases;
        self
    }

    pub fn visibility_at(&self, now: SystemTime) -> WorkerVisibility {
        if self.visible_until >= now {
            WorkerVisibility::Visible
        } else {
            WorkerVisibility::Expired
        }
    }

    pub fn validate(&self, ctx: &ExecutionContext) -> AppResult<()> {
        require_non_empty(
            &self.worker_id,
            "worker heartbeat worker id",
            "worker_heartbeat_worker_id_required",
            ctx,
        )?;
        require_non_empty(
            &self.scope,
            "worker heartbeat scope",
            "worker_heartbeat_scope_required",
            ctx,
        )?;

        if self.visible_until < self.recorded_at {
            return Err(AppError::from_context(
                ErrorCode::InvalidInput,
                "worker_heartbeat_visibility_window_invalid",
                "worker heartbeat visibility must not end before it was recorded",
                ctx,
            ));
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerVisibilityRef {
    worker_id: String,
    scope: String,
    visible_at: SystemTime,
    limit: usize,
}

impl WorkerVisibilityRef {
    pub fn new(worker_id: impl Into<String>, scope: impl Into<String>) -> Self {
        Self {
            worker_id: worker_id.into(),
            scope: scope.into(),
            visible_at: SystemTime::now(),
            limit: 1,
        }
    }

    pub fn worker_id(&self) -> &str {
        &self.worker_id
    }

    pub fn scope(&self) -> &str {
        &self.scope
    }

    pub fn visible_at(&self) -> SystemTime {
        self.visible_at
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    pub fn at(mut self, visible_at: SystemTime) -> Self {
        self.visible_at = visible_at;
        self
    }

    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit.max(1);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleWorkersQuery {
    scope: String,
    visible_at: SystemTime,
    limit: usize,
}

impl VisibleWorkersQuery {
    pub fn new(scope: impl Into<String>) -> Self {
        Self {
            scope: scope.into(),
            visible_at: SystemTime::now(),
            limit: usize::MAX,
        }
    }

    pub fn scope(&self) -> &str {
        &self.scope
    }

    pub fn visible_at(&self) -> SystemTime {
        self.visible_at
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    pub fn at(mut self, visible_at: SystemTime) -> Self {
        self.visible_at = visible_at;
        self
    }

    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit.max(1);
        self
    }
}

pub trait WorkerHeartbeatStore: Send + Sync {
    fn record_heartbeat(&self, heartbeat: WorkerHeartbeat, ctx: &ExecutionContext)
        -> AppResult<()>;

    fn lookup_worker(
        &self,
        worker: WorkerVisibilityRef,
        ctx: &ExecutionContext,
    ) -> AppResult<Option<WorkerHeartbeat>>;

    fn list_visible_workers(
        &self,
        query: VisibleWorkersQuery,
        ctx: &ExecutionContext,
    ) -> AppResult<Vec<WorkerHeartbeat>>;
}

fn require_non_empty(
    value: &str,
    field_name: &'static str,
    code: &'static str,
    ctx: &ExecutionContext,
) -> AppResult<()> {
    if value.trim().is_empty() {
        return Err(AppError::from_context(
            ErrorCode::InvalidInput,
            code,
            format!("{field_name} must not be empty"),
            ctx,
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use crate::CorrelationId;

    use super::{VisibleWorkersQuery, WorkerHeartbeat, WorkerVisibility, WorkerVisibilityRef};

    #[test]
    fn worker_heartbeat_uses_ttl_for_visibility() {
        let recorded_at = SystemTime::UNIX_EPOCH + Duration::from_secs(30);
        let heartbeat = WorkerHeartbeat::from_ttl(
            "worker-1",
            "scheduler",
            recorded_at,
            Duration::from_secs(15),
        )
        .with_active_leases(2);

        assert_eq!(heartbeat.worker_id(), "worker-1");
        assert_eq!(heartbeat.scope(), "scheduler");
        assert_eq!(heartbeat.active_leases(), 2);
        assert_eq!(
            heartbeat.visible_until(),
            recorded_at + Duration::from_secs(15)
        );
        assert_eq!(
            heartbeat.visibility_at(recorded_at + Duration::from_secs(10)),
            WorkerVisibility::Visible
        );
        assert_eq!(
            heartbeat.visibility_at(recorded_at + Duration::from_secs(16)),
            WorkerVisibility::Expired
        );
    }

    #[test]
    fn worker_heartbeat_rejects_invalid_visibility_window() {
        let ctx = crate::ExecutionContext::new(CorrelationId::new("corr_worker_1"));
        let heartbeat = WorkerHeartbeat::new(
            "worker-2",
            "poller",
            SystemTime::UNIX_EPOCH + Duration::from_secs(20),
            SystemTime::UNIX_EPOCH + Duration::from_secs(19),
        );

        let error = heartbeat.validate(&ctx).expect_err("heartbeat should fail");
        assert_eq!(error.code(), "worker_heartbeat_visibility_window_invalid");
    }

    #[test]
    fn visible_worker_queries_clamp_limits() {
        let visible_at = SystemTime::UNIX_EPOCH + Duration::from_secs(60);
        let query = VisibleWorkersQuery::new("scheduler")
            .at(visible_at)
            .with_limit(0);
        let worker = WorkerVisibilityRef::new("worker-3", "scheduler")
            .at(visible_at)
            .with_limit(0);

        assert_eq!(query.limit(), 1);
        assert_eq!(query.visible_at(), visible_at);
        assert_eq!(worker.limit(), 1);
        assert_eq!(worker.visible_at(), visible_at);
        assert_eq!(worker.worker_id(), "worker-3");
    }
}
