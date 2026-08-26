use super::*;
use crate::llm_api::DirectLlmTransportResponse;
use crate::llm_typed_tasks::{
    TypedLlmAuthority, TypedLlmExecutionPolicy, TypedLlmFallbackPolicy, TypedLlmProviderPolicy,
    TypedLlmRawOutputRetention, TypedLlmRedactionPolicy, TypedLlmResponseFormat,
    TypedLlmRetryPolicy, TypedLlmSafetyPolicy, TypedLlmTaskCapabilities, TypedLlmTaskClass,
    TypedLlmTaskInput, TypedLlmTaskSpec,
};
use bos_kernel::ErrorCode;
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

struct FakeTransport {
    requests: Arc<Mutex<Vec<DirectLlmTransportRequest>>>,
    responses: Arc<Mutex<VecDeque<AppResult<DirectLlmTransportResponse>>>>,
}

impl FakeTransport {
    fn with_responses(responses: Vec<AppResult<DirectLlmTransportResponse>>) -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(VecDeque::from(responses))),
        }
    }
}

impl DirectLlmTransport for FakeTransport {
    fn send(&self, request: &DirectLlmTransportRequest) -> AppResult<DirectLlmTransportResponse> {
        lock_or_recover(&self.requests).push(request.clone());
        match lock_or_recover(&self.responses).pop_front() {
            Some(result) => result,
            None => Err(AppError::unexpected(
                "fake_missing_response",
                "no response",
                CorrelationId::generate(),
            )),
        }
    }
}

#[derive(Clone, Default)]
struct RecordingSleeper {
    delays: Arc<Mutex<Vec<Duration>>>,
}

impl DirectLlmRetrySleeper for RecordingSleeper {
    fn sleep(&self, delay: Duration) {
        lock_or_recover(&self.delays).push(delay);
    }
}

fn config() -> AnthropicDirectLlmConfig {
    AnthropicDirectLlmConfig::anthropic(
        "sk-ant-redacted",
        "claude-sonnet-4-6",
        "https://api.anthropic.com/v1/messages",
    )
}

/// Test schema registry: serves a schema for `contact.extract.v1` only.
fn test_schema_lookup(schema_ref: &str) -> Option<Value> {
    (schema_ref == "contact.extract.v1").then(|| {
        json!({
            "type": "object",
            "required": ["schema_version", "contacts"],
            "properties": {
                "schema_version": {"type": "integer"},
                "contacts": {"type": "array"}
            }
        })
    })
}

fn request_with_schema(schema_ref: &str) -> TypedLlmTaskRequest {
    TypedLlmTaskRequest {
        task_id: "task-1".to_string(),
        correlation_id: "corr-1".to_string(),
        idempotency_key: "idem-1".to_string(),
        tenant_or_project_scope: "tenant-1".to_string(),
        source_entity: None,
        spec: TypedLlmTaskSpec {
            task_class: TypedLlmTaskClass::Extract,
            prompt_template_id: "contact_extract_direct.v1".to_string(),
            prompt_template_version: "1".to_string(),
            prompt_template_hash: "hash-1".to_string(),
            schema_ref: schema_ref.to_string(),
            response_format: TypedLlmResponseFormat::JsonSchema,
            max_input_bytes: 8192,
            max_output_bytes: 8192,
            max_tokens: 1024,
            timeout_ms: 500,
            capabilities: TypedLlmTaskCapabilities::pure_transformation(),
            authority: TypedLlmAuthority::no_side_effects(),
        },
        input: TypedLlmTaskInput {
            json: json!({"notes": "Jane Roe jane@example.com Acme Owner"}),
            text_blocks: Vec::new(),
        },
        execution_policy: TypedLlmExecutionPolicy {
            default_route: TypedLlmExecutionRoute::DirectApi,
            fallback_policy: TypedLlmFallbackPolicy::FailClosed,
            retry_policy: TypedLlmRetryPolicy {
                max_attempts: 2,
                backoff_ms: 0,
                max_elapsed_ms: 500,
            },
        },
        provider_policy: TypedLlmProviderPolicy {
            preferred_provider: "anthropic".to_string(),
            preferred_model: "claude-sonnet-4-6".to_string(),
            fallback_provider: None,
            fallback_model: None,
        },
        safety_policy: TypedLlmSafetyPolicy {
            redaction_policy: TypedLlmRedactionPolicy::PreAndPost,
            raw_output_retention: TypedLlmRawOutputRetention::LocalOnly,
        },
    }
}

fn ok_response(stop_reason: &str, json_text: &str) -> DirectLlmTransportResponse {
    DirectLlmTransportResponse {
        status: 200,
        headers: BTreeMap::from([("request-id".to_string(), "req-ant-1".to_string())]),
        body: json!({
            "content": [{ "type": "text", "text": json_text }],
            "stop_reason": stop_reason
        })
        .to_string(),
    }
}

fn client(response: DirectLlmTransportResponse) -> AnthropicDirectLlmClient {
    AnthropicDirectLlmClient::with_transport(
        config(),
        Box::new(FakeTransport::with_responses(vec![Ok(response)])),
        test_schema_lookup,
    )
}

fn lock_or_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn last_request_body(
    client_requests: &Arc<Mutex<Vec<DirectLlmTransportRequest>>>,
) -> AppResult<Value> {
    let requests = lock_or_recover(client_requests);
    let request = requests.last().ok_or_else(|| {
        AppError::unexpected(
            "anthropic_test_missing_request",
            "expected fake transport request",
            CorrelationId::generate(),
        )
    })?;
    serde_json::from_str(&request.body).map_err(|error| {
        AppError::unexpected(
            "anthropic_test_request_parse_failed",
            format!("failed to parse fake transport request body: {error}"),
            CorrelationId::generate(),
        )
    })
}

fn last_request_headers(
    client_requests: &Arc<Mutex<Vec<DirectLlmTransportRequest>>>,
) -> AppResult<BTreeMap<String, String>> {
    let requests = lock_or_recover(client_requests);
    requests
        .last()
        .map(|request| request.headers.clone())
        .ok_or_else(|| {
            AppError::unexpected(
                "anthropic_test_missing_request",
                "expected fake transport request",
                CorrelationId::generate(),
            )
        })
}

fn require_error<T>(result: AppResult<T>) -> AppResult<AppError> {
    match result {
        Ok(_) => Err(AppError::unexpected(
            "anthropic_test_unexpected_success",
            "expected Anthropic direct LLM client call to fail",
            CorrelationId::generate(),
        )),
        Err(error) => Ok(error),
    }
}

#[test]
fn engages_structured_outputs_when_schema_registered() -> AppResult<()> {
    let fake = FakeTransport::with_responses(vec![Ok(ok_response(
        "end_turn",
        "{\"schema_version\":1,\"contacts\":[]}",
    ))]);
    let requests = fake.requests.clone();
    let client =
        AnthropicDirectLlmClient::with_transport(config(), Box::new(fake), test_schema_lookup);

    let envelope = client.complete_typed_task(&request_with_schema("contact.extract.v1"))?;

    let body = last_request_body(&requests)?;
    assert_eq!(body["output_config"]["format"]["type"], "json_schema");
    assert!(body["output_config"]["format"]["schema"].is_object());
    // System block carries cache_control.
    assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
    assert_eq!(envelope.provider_id, "anthropic");
    assert_eq!(envelope.finish_reason.as_deref(), Some("end_turn"));
    Ok(())
}

#[test]
fn omits_output_config_when_no_schema_registered() -> AppResult<()> {
    let fake = FakeTransport::with_responses(vec![Ok(ok_response("end_turn", "{\"ok\":true}"))]);
    let requests = fake.requests.clone();
    let client =
        AnthropicDirectLlmClient::with_transport(config(), Box::new(fake), test_schema_lookup);

    client.complete_typed_task(&request_with_schema("email.classification.v1"))?;

    let body = last_request_body(&requests)?;
    assert!(
        body.get("output_config").is_none(),
        "no schema => no output_config"
    );
    Ok(())
}

#[test]
fn accepts_whole_json_markdown_fence() -> AppResult<()> {
    let client = client(ok_response(
        "end_turn",
        "```json\n{\"schema_version\":1,\"contacts\":[]}\n```",
    ));

    let envelope = client.complete_typed_task(&request_with_schema("contact.extract.v1"))?;

    assert_eq!(
        envelope.response_json,
        json!({"schema_version": 1, "contacts": []})
    );
    Ok(())
}

#[test]
fn sends_anthropic_headers_without_structured_output_beta() -> AppResult<()> {
    let fake = FakeTransport::with_responses(vec![Ok(ok_response("end_turn", "{\"ok\":true}"))]);
    let requests = fake.requests.clone();
    let client =
        AnthropicDirectLlmClient::with_transport(config(), Box::new(fake), test_schema_lookup);

    client.complete_typed_task(&request_with_schema("email.classification.v1"))?;

    let headers = last_request_headers(&requests)?;
    assert_eq!(
        headers.get("x-api-key").map(String::as_str),
        Some("sk-ant-redacted")
    );
    assert_eq!(
        headers.get("anthropic-version").map(String::as_str),
        Some("2023-06-01")
    );
    assert_eq!(
        headers.get("anthropic-beta").map(String::as_str),
        Some("prompt-caching-2024-07-31")
    );
    // No structured-outputs beta header (output_config.format is GA).
    assert!(!headers.values().any(|v| v.contains("structured-outputs")));
    Ok(())
}

#[test]
fn refusal_stop_reason_maps_to_policy_error_without_parsing() -> AppResult<()> {
    let client = client(ok_response("refusal", "not json at all"));
    let error =
        require_error(client.complete_typed_task(&request_with_schema("contact.extract.v1")))?;
    assert_eq!(error.code(), "direct_llm_anthropic_refusal");
    assert_eq!(error.kind(), ErrorCode::Policy);
    Ok(())
}

#[test]
fn max_tokens_stop_reason_maps_to_truncated_error() -> AppResult<()> {
    let client = client(ok_response("max_tokens", "{\"contacts\":[")); // truncated
    let error =
        require_error(client.complete_typed_task(&request_with_schema("contact.extract.v1")))?;
    assert_eq!(error.code(), "direct_llm_anthropic_output_truncated");
    Ok(())
}

#[test]
fn empty_content_maps_to_response_empty() -> AppResult<()> {
    let response = DirectLlmTransportResponse {
        status: 200,
        headers: BTreeMap::new(),
        body: json!({ "content": [], "stop_reason": "end_turn" }).to_string(),
    };
    let client = client(response);
    let error =
        require_error(client.complete_typed_task(&request_with_schema("contact.extract.v1")))?;
    assert_eq!(error.code(), "direct_llm_response_empty");
    Ok(())
}

#[test]
fn guard_rejects_non_direct_route() -> AppResult<()> {
    let mut request = request_with_schema("contact.extract.v1");
    request.execution_policy.default_route = TypedLlmExecutionRoute::Harness;
    let client = client(ok_response("end_turn", "{}"));
    let error = require_error(client.complete_typed_task(&request))?;
    assert_eq!(error.code(), "direct_llm_task_route_not_direct");
    Ok(())
}

#[test]
fn rate_limit_then_success_retries() -> AppResult<()> {
    let rl = DirectLlmTransportResponse {
        status: 429,
        headers: BTreeMap::from([("retry-after".to_string(), "0".to_string())]),
        body: String::new(),
    };
    let ok = ok_response("end_turn", "{\"schema_version\":1,\"contacts\":[]}");
    let sleeper = RecordingSleeper::default();
    let delays = sleeper.delays.clone();
    let client = AnthropicDirectLlmClient::with_transport_and_sleeper(
        config(),
        Box::new(FakeTransport::with_responses(vec![Ok(rl), Ok(ok)])),
        Box::new(sleeper),
        test_schema_lookup,
    );

    let envelope = client.complete_typed_task(&request_with_schema("contact.extract.v1"))?;
    assert_eq!(envelope.retry_count, 1);
    assert_eq!(lock_or_recover(&delays).len(), 1);
    Ok(())
}

#[test]
fn hard_4xx_maps_provider_error() -> AppResult<()> {
    let response = DirectLlmTransportResponse {
        status: 400,
        headers: BTreeMap::new(),
        body: json!({"error": {"type": "invalid_request_error"}}).to_string(),
    };
    let client = client(response);
    let error =
        require_error(client.complete_typed_task(&request_with_schema("contact.extract.v1")))?;
    assert_eq!(error.code(), "direct_llm_provider_rejected");
    Ok(())
}

#[test]
fn debug_redacts_anthropic_config_api_key() {
    let rendered = format!("{:?}", config());
    assert!(
        !rendered.contains("sk-ant-redacted"),
        "api_key leaked: {rendered}"
    );
    assert!(rendered.contains("anthropic"));
}
