use std::time::SystemTime;

use crate::{AgentSessionId, CausationId, CorrelationId, IdempotencyKey, ThreadId, WorkflowId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorRef {
    pub kind: String,
    pub id: String,
}

impl ActorRef {
    pub fn new(kind: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            id: id.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionContext {
    pub actor: Option<ActorRef>,
    pub correlation_id: CorrelationId,
    pub causation_id: Option<CausationId>,
    pub idempotency_key: Option<IdempotencyKey>,
    pub workflow_id: Option<WorkflowId>,
    pub session_id: Option<AgentSessionId>,
    pub thread_id: Option<ThreadId>,
    pub attempt: u32,
    pub started_at: SystemTime,
    pub finished_at: Option<SystemTime>,
}

impl ExecutionContext {
    pub fn new(correlation_id: CorrelationId) -> Self {
        Self {
            actor: None,
            correlation_id,
            causation_id: None,
            idempotency_key: None,
            workflow_id: None,
            session_id: None,
            thread_id: None,
            attempt: 1,
            started_at: SystemTime::now(),
            finished_at: None,
        }
    }

    pub fn idempotency_key(&self) -> Option<&IdempotencyKey> {
        self.idempotency_key.as_ref()
    }

    pub fn with_actor(mut self, actor: ActorRef) -> Self {
        self.actor = Some(actor);
        self
    }

    pub fn with_causation_id(mut self, causation_id: CausationId) -> Self {
        self.causation_id = Some(causation_id);
        self
    }

    pub fn with_idempotency_key(mut self, idempotency_key: IdempotencyKey) -> Self {
        self.idempotency_key = Some(idempotency_key);
        self
    }

    pub fn with_workflow_id(mut self, workflow_id: WorkflowId) -> Self {
        self.workflow_id = Some(workflow_id);
        self
    }

    pub fn with_session_id(mut self, session_id: AgentSessionId) -> Self {
        self.session_id = Some(session_id);
        self
    }

    pub fn with_thread_id(mut self, thread_id: ThreadId) -> Self {
        self.thread_id = Some(thread_id);
        self
    }

    pub fn with_attempt(mut self, attempt: u32) -> Self {
        self.attempt = attempt.max(1);
        self
    }

    pub fn start_at(mut self, started_at: SystemTime) -> Self {
        self.started_at = started_at;
        self
    }

    pub fn finish(mut self, finished_at: SystemTime) -> Self {
        self.finished_at = Some(finished_at);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelemetryEvent {
    pub name: String,
    pub at: SystemTime,
    pub attributes: Vec<(String, String)>,
}

impl TelemetryEvent {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            at: SystemTime::now(),
            attributes: Vec::new(),
        }
    }

    pub fn with_attribute(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.attributes.push((key.into(), value.into()));
        self
    }
}

pub trait TelemetrySink: Send + Sync {
    fn record(&self, ctx: &ExecutionContext, event: TelemetryEvent);
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NoopTelemetry;

impl TelemetrySink for NoopTelemetry {
    fn record(&self, _ctx: &ExecutionContext, _event: TelemetryEvent) {}
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use crate::{CorrelationId, IdempotencyKey, WorkflowId};

    use super::{ExecutionContext, TelemetryEvent};

    #[test]
    fn execution_context_tracks_core_ids() {
        let ctx = ExecutionContext::new(CorrelationId::new("corr_1"))
            .with_workflow_id(WorkflowId::new("wf_1"))
            .with_idempotency_key(IdempotencyKey::new("idem_1"))
            .with_attempt(2);

        assert_eq!(ctx.correlation_id.as_str(), "corr_1");
        assert_eq!(ctx.workflow_id.expect("workflow").as_str(), "wf_1");
        assert_eq!(ctx.idempotency_key.expect("key").as_str(), "idem_1");
        assert_eq!(ctx.attempt, 2);
        assert!(ctx.finished_at.is_none());
    }

    #[test]
    fn execution_context_can_capture_finish_time() {
        let finished_at = SystemTime::now();
        let ctx = ExecutionContext::new(CorrelationId::new("corr_2")).finish(finished_at);
        assert_eq!(ctx.finished_at, Some(finished_at));
    }

    #[test]
    fn telemetry_event_collects_attributes() {
        let event = TelemetryEvent::new("outbox.publish").with_attribute("topic", "agent_bus");
        assert_eq!(event.attributes.len(), 1);
    }
}
