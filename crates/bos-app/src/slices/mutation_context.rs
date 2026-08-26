use crate::http::OperatorScope;

pub struct MutationContext<'a> {
    pub client_id: &'a str,
    pub actor_id: &'a str,
    pub expected_revision: Option<u64>,
    pub idempotency_key: &'a str,
    pub now_ms: u64,
}

pub struct ScopedMutationContext<'a> {
    pub client_id: &'a str,
    pub actor_id: &'a str,
    pub scope: &'a OperatorScope,
    pub expected_revision: Option<u64>,
    pub idempotency_key: &'a str,
    pub now_ms: u64,
}
