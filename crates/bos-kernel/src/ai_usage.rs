#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiCallUsageRecord {
    pub usage_id: String,
    pub recorded_at_ms: i64,
    pub call_purpose: String,
    pub task_kind: Option<String>,
    pub route: String,
    pub provider: String,
    pub model: String,
    pub thinking_level: Option<String>,
    pub tokens_in: Option<u64>,
    pub tokens_out: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub cost_micros: Option<u64>,
    pub latency_ms: u64,
    pub success: bool,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub correlation_id: String,
    pub tenant_or_project_scope: Option<String>,
    pub provider_request_id: Option<String>,
}

pub trait AiCallUsageSink: Send + Sync {
    fn record(&self, record: AiCallUsageRecord);
}

#[derive(Debug, Default)]
pub struct NoopAiCallUsageSink;

impl AiCallUsageSink for NoopAiCallUsageSink {
    fn record(&self, _record: AiCallUsageRecord) {}
}

pub fn trace_ai_call_usage(record: &AiCallUsageRecord) {
    tracing::info!(
        usage_id = %record.usage_id,
        recorded_at_ms = record.recorded_at_ms,
        call_purpose = %record.call_purpose,
        task_kind = record.task_kind.as_deref(),
        route = %record.route,
        provider = %record.provider,
        model = %record.model,
        thinking_level = record.thinking_level.as_deref(),
        tokens_in = record.tokens_in,
        tokens_out = record.tokens_out,
        total_tokens = record.total_tokens,
        cached_tokens = record.cached_tokens,
        cost_micros = record.cost_micros,
        latency_ms = record.latency_ms,
        success = record.success,
        error_code = record.error_code.as_deref(),
        error_message = record.error_message.as_deref(),
        correlation_id = %record.correlation_id,
        tenant_or_project_scope = record.tenant_or_project_scope.as_deref(),
        provider_request_id = record.provider_request_id.as_deref(),
        "ai call usage recorded"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_render_contains_metadata_without_prompt_body_or_secret_fields() {
        let record = AiCallUsageRecord {
            usage_id: "usage-1".to_string(),
            recorded_at_ms: 1,
            call_purpose: "humanization_rewrite".to_string(),
            task_kind: Some("rewrite".to_string()),
            route: "direct".to_string(),
            provider: "anthropic".to_string(),
            model: "claude-test".to_string(),
            thinking_level: Some("medium".to_string()),
            tokens_in: Some(10),
            tokens_out: Some(5),
            total_tokens: Some(15),
            cached_tokens: None,
            cost_micros: None,
            latency_ms: 12,
            success: true,
            error_code: None,
            error_message: None,
            correlation_id: "corr-1".to_string(),
            tenant_or_project_scope: Some("tenant-1".to_string()),
            provider_request_id: Some("req-1".to_string()),
        };

        let rendered = format!("{record:?}");

        assert!(rendered.contains("humanization_rewrite"));
        for forbidden in ["prompt", "response_body", "api_key", "secret", "sk-test"] {
            assert!(!rendered.contains(forbidden), "leaked field: {forbidden}");
        }
    }
}
