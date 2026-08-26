use super::retry::DirectLlmRetrySleeper;
use super::*;
use crate::llm_typed_tasks::{
    TypedLlmAuthority, TypedLlmExecutionPolicy, TypedLlmFallbackPolicy, TypedLlmProviderPolicy,
    TypedLlmRawOutputRetention, TypedLlmRedactionPolicy, TypedLlmResponseFormat,
    TypedLlmRetryPolicy, TypedLlmSafetyPolicy, TypedLlmTaskCapabilities, TypedLlmTaskClass,
    TypedLlmTaskInput, TypedLlmTaskSpec,
};
use serde_json::json;
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

#[test]
fn debug_redacts_direct_llm_config_api_key() {
    let config = OpenAiCompatibleDirectLlmConfig {
        provider_id: "openrouter".to_string(),
        api_key: "sk-super-secret-key".to_string(),
        model: "model-x".to_string(),
        endpoint: "https://example/v1".to_string(),
        timeout_ms: 1000,
    };
    let rendered = format!("{config:?}");
    assert!(
        !rendered.contains("sk-super-secret-key"),
        "api_key leaked: {rendered}"
    );
    // Non-secret selector retained.
    assert!(rendered.contains("openrouter"));
}

#[derive(Default)]
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
        match self.requests.lock() {
            Ok(mut requests) => requests.push(request.clone()),
            Err(error) => error.into_inner().push(request.clone()),
        }
        let mut responses = match self.responses.lock() {
            Ok(responses) => responses,
            Err(error) => error.into_inner(),
        };
        responses.pop_front().unwrap_or_else(|| {
            Err(AppError::unexpected(
                "fake_direct_llm_missing_response",
                "fake direct LLM transport has no response",
                CorrelationId::generate(),
            ))
        })
    }
}

#[derive(Clone, Default)]
struct RecordingSleeper {
    delays: Arc<Mutex<Vec<Duration>>>,
}

impl DirectLlmRetrySleeper for RecordingSleeper {
    fn sleep(&self, delay: Duration) {
        match self.delays.lock() {
            Ok(mut delays) => delays.push(delay),
            Err(error) => error.into_inner().push(delay),
        }
    }
}

fn request() -> TypedLlmTaskRequest {
    TypedLlmTaskRequest {
        task_id: "task-1".to_string(),
        correlation_id: "corr-1".to_string(),
        idempotency_key: "idem-1".to_string(),
        tenant_or_project_scope: "tenant-1".to_string(),
        source_entity: None,
        spec: TypedLlmTaskSpec {
            task_class: TypedLlmTaskClass::Classify,
            prompt_template_id: "email_triage_direct.v1".to_string(),
            prompt_template_version: "1".to_string(),
            prompt_template_hash: "hash-1".to_string(),
            schema_ref: "email.triage_result.v1".to_string(),
            response_format: TypedLlmResponseFormat::JsonObject,
            max_input_bytes: 8192,
            max_output_bytes: 8192,
            max_tokens: 512,
            timeout_ms: 500,
            capabilities: TypedLlmTaskCapabilities::pure_transformation(),
            authority: TypedLlmAuthority::no_side_effects(),
        },
        input: TypedLlmTaskInput {
            json: json!({"thread_id":"thread-1"}),
            text_blocks: Vec::new(),
        },
        execution_policy: TypedLlmExecutionPolicy {
            default_route: TypedLlmExecutionRoute::DirectApi,
            fallback_policy: TypedLlmFallbackPolicy::FailClosed,
            retry_policy: TypedLlmRetryPolicy {
                max_attempts: 1,
                backoff_ms: 0,
                max_elapsed_ms: 500,
            },
        },
        provider_policy: TypedLlmProviderPolicy {
            preferred_provider: "openrouter".to_string(),
            preferred_model: "openai/gpt-4.1-mini".to_string(),
            fallback_provider: None,
            fallback_model: None,
        },
        safety_policy: TypedLlmSafetyPolicy {
            redaction_policy: TypedLlmRedactionPolicy::PreAndPost,
            raw_output_retention: TypedLlmRawOutputRetention::LocalOnly,
        },
    }
}

fn openrouter_config() -> OpenAiCompatibleDirectLlmConfig {
    OpenAiCompatibleDirectLlmConfig::openrouter(
        "sk-test-redacted",
        "openai/gpt-4.1-mini",
        "https://example.test/v1/chat/completions",
    )
}

fn success_response() -> DirectLlmTransportResponse {
    DirectLlmTransportResponse {
        status: 200,
        headers: BTreeMap::from([("x-request-id".to_string(), "req-1".to_string())]),
        body: json!({
            "choices":[{"message":{"content":"{\"schema_version\":1}"}, "finish_reason":"stop"}],
            "usage":{"prompt_tokens":4,"completion_tokens":6,"total_tokens":10}
        })
        .to_string(),
    }
}

fn client_with_response(response: DirectLlmTransportResponse) -> OpenAiCompatibleDirectLlmClient {
    let fake = FakeTransport::with_responses(vec![Ok(response)]);
    OpenAiCompatibleDirectLlmClient::with_transport(openrouter_config(), Box::new(fake))
}

struct ClientWithRecording {
    client: OpenAiCompatibleDirectLlmClient,
    requests: Arc<Mutex<Vec<DirectLlmTransportRequest>>>,
    delays: Arc<Mutex<Vec<Duration>>>,
}

fn client_with_sequence(
    responses: Vec<AppResult<DirectLlmTransportResponse>>,
) -> ClientWithRecording {
    let fake = FakeTransport::with_responses(responses);
    let requests = fake.requests.clone();
    let sleeper = RecordingSleeper::default();
    let delays = sleeper.delays.clone();
    let client = OpenAiCompatibleDirectLlmClient::with_transport_and_sleeper(
        openrouter_config(),
        Box::new(fake),
        Box::new(sleeper),
    );
    ClientWithRecording {
        client,
        requests,
        delays,
    }
}

fn retry_request(max_attempts: u8, backoff_ms: u64, max_elapsed_ms: u64) -> TypedLlmTaskRequest {
    let mut request = request();
    request.execution_policy.retry_policy = TypedLlmRetryPolicy {
        max_attempts,
        backoff_ms,
        max_elapsed_ms,
    };
    request
}

fn tool_request() -> TypedLlmTaskRequest {
    let mut request = request();
    request.spec.capabilities.tools = true;
    request.execution_policy.default_route = TypedLlmExecutionRoute::Harness;
    request
}

fn timeout_transport_error() -> AppError {
    AppError::new(
        ErrorCode::Timeout,
        "direct_llm_timeout",
        "direct LLM transport timed out",
        CorrelationId::generate(),
    )
}

fn locked_len<T>(values: &Arc<Mutex<Vec<T>>>) -> usize {
    match values.lock() {
        Ok(values) => values.len(),
        Err(error) => error.into_inner().len(),
    }
}

fn locked_durations(values: &Arc<Mutex<Vec<Duration>>>) -> Vec<Duration> {
    match values.lock() {
        Ok(values) => values.clone(),
        Err(error) => error.into_inner().clone(),
    }
}

#[test]
fn openai_compatible_client_normalizes_json_output() {
    let fake = FakeTransport::with_responses(vec![Ok(success_response())]);
    let captured = fake.requests.clone();
    let client =
        OpenAiCompatibleDirectLlmClient::with_transport(openrouter_config(), Box::new(fake));

    let envelope_result = client.complete_typed_task(&request());
    assert!(envelope_result.is_ok(), "{envelope_result:?}");
    let Ok(envelope) = envelope_result else {
        return;
    };

    assert_eq!(envelope.execution_route, TypedLlmExecutionRoute::DirectApi);
    assert_eq!(envelope.provider_id, "openrouter");
    assert_eq!(envelope.response_json, json!({"schema_version":1}));
    assert_eq!(envelope.provider_request_id.as_deref(), Some("req-1"));
    assert_eq!(
        envelope.usage.and_then(|usage| usage.total_tokens),
        Some(10)
    );
    let requests = match captured.lock() {
        Ok(requests) => requests,
        Err(error) => error.into_inner(),
    };
    assert_eq!(requests.len(), 1);
    assert!(requests[0].body.contains("\"response_format\""));
    assert!(!requests[0].body.contains("sk-test-redacted"));
}

#[test]
fn openai_compatible_client_accepts_whole_json_markdown_fence() {
    let client = client_with_response(DirectLlmTransportResponse {
        status: 200,
        headers: BTreeMap::from([("request-id".to_string(), "req-1".to_string())]),
        body: json!({
            "choices":[{
                "message":{"content":"```json\n{\"schema_version\":1}\n```"},
                "finish_reason":"stop"
            }],
        })
        .to_string(),
    });

    let envelope_result = client.complete_typed_task(&request());
    assert!(envelope_result.is_ok(), "{envelope_result:?}");
    let Ok(envelope) = envelope_result else {
        return;
    };
    assert_eq!(envelope.response_json, json!({"schema_version": 1}));
}

#[test]
fn openai_compatible_client_prefers_request_id_fallback_header() {
    let client = client_with_response(DirectLlmTransportResponse {
        status: 200,
        headers: BTreeMap::from([
            ("x-request-id".to_string(), "   ".to_string()),
            ("request-id".to_string(), "req-fallback".to_string()),
        ]),
        body: json!({
            "choices":[{"message":{"content":"{\"schema_version\":1}"}, "finish_reason":"stop"}],
        })
        .to_string(),
    });

    let envelope_result = client.complete_typed_task(&request());
    assert!(envelope_result.is_ok(), "{envelope_result:?}");
    let Ok(envelope) = envelope_result else {
        return;
    };
    assert_eq!(
        envelope.provider_request_id.as_deref(),
        Some("req-fallback")
    );
}

#[test]
fn openai_compatible_client_ignores_unsafe_request_id_header() {
    let client = client_with_response(DirectLlmTransportResponse {
        status: 200,
        headers: BTreeMap::from([(
            "request-id".to_string(),
            "req-1\nauthorization: sk-leak".to_string(),
        )]),
        body: json!({
            "choices":[{"message":{"content":"{\"schema_version\":1}"}, "finish_reason":"stop"}],
        })
        .to_string(),
    });

    let envelope_result = client.complete_typed_task(&request());
    assert!(envelope_result.is_ok(), "{envelope_result:?}");
    let Ok(envelope) = envelope_result else {
        return;
    };
    assert!(envelope.provider_request_id.is_none());
}

#[test]
fn direct_client_rejects_provider_write_authority() {
    let fake = FakeTransport::default();
    let client =
        OpenAiCompatibleDirectLlmClient::with_transport(openrouter_config(), Box::new(fake));
    let mut request = request();
    request.spec.authority.provider_writes_enabled = true;

    let result = client.complete_typed_task(&request);
    assert!(result.is_err());
    let Err(error) = result else {
        return;
    };

    assert_eq!(error.code(), "direct_llm_provider_writes_forbidden");
}

#[test]
fn direct_client_rejects_non_direct_route_before_transport() {
    let fake = FakeTransport::default();
    let client =
        OpenAiCompatibleDirectLlmClient::with_transport(openrouter_config(), Box::new(fake));
    let mut request = request();
    request.execution_policy.default_route = TypedLlmExecutionRoute::Harness;

    let result = client.complete_typed_task(&request);
    assert!(result.is_err());
    let Err(error) = result else {
        return;
    };

    assert_eq!(error.code(), "direct_llm_task_route_not_direct");
}

#[test]
fn direct_client_rejects_unsupported_response_format_before_transport() {
    let fake = FakeTransport::default();
    let client =
        OpenAiCompatibleDirectLlmClient::with_transport(openrouter_config(), Box::new(fake));
    let mut request = request();
    request.spec.response_format = TypedLlmResponseFormat::JsonSchema;

    let result = client.complete_typed_task(&request);
    assert!(result.is_err());
    let Err(error) = result else {
        return;
    };

    assert_eq!(error.code(), "direct_llm_response_format_unsupported");
}

#[test]
fn openai_tool_turn_serializes_tools_and_tool_results() {
    let fake = FakeTransport::with_responses(vec![Ok(DirectLlmTransportResponse {
        status: 200,
        headers: BTreeMap::from([("request-id".to_string(), "req-tool".to_string())]),
        body: json!({
            "choices":[{
                "message":{"content":"{\"schema_version\":1}"},
                "finish_reason":"stop"
            }]
        })
        .to_string(),
    })]);
    let captured = fake.requests.clone();
    let client =
        OpenAiCompatibleDirectLlmClient::with_transport(openrouter_config(), Box::new(fake));

    let result = client.complete_typed_task_turn(
        &tool_request(),
        &DirectLlmToolTurnRequest {
            tools: vec![DirectLlmToolDefinition {
                name: "crm_account_lookup".to_string(),
                description: "Read-only CRM account lookup.".to_string(),
                parameters_schema: json!({
                    "type": "object",
                    "required": ["query"],
                    "properties": {"query": {"type": "string"}}
                }),
            }],
            prior_tool_turns: vec![DirectLlmToolTurn {
                tool_calls: vec![DirectLlmToolCall {
                    id: "call-1".to_string(),
                    name: "crm_account_lookup".to_string(),
                    arguments: json!({"query": "Acme"}),
                }],
                tool_results: vec![DirectLlmToolResult {
                    call_id: "call-1".to_string(),
                    name: "crm_account_lookup".to_string(),
                    arguments: json!({"query": "Acme"}),
                    result_json: json!({"matches": []}),
                }],
            }],
        },
    );
    assert!(result.is_ok(), "{result:?}");
    let Ok(DirectLlmToolTurnResponse::Final(envelope)) = result else {
        return;
    };

    assert_eq!(envelope.execution_route, TypedLlmExecutionRoute::DirectApi);
    assert_eq!(envelope.provider_request_id.as_deref(), Some("req-tool"));

    let requests = match captured.lock() {
        Ok(requests) => requests,
        Err(error) => error.into_inner(),
    };
    let body: serde_json::Value =
        serde_json::from_str(&requests[0].body).expect("request body is JSON");
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(
        body["tools"][0]["function"]["name"],
        json!("crm_account_lookup")
    );
    assert_eq!(body["tool_choice"], json!("auto"));
    assert_eq!(body["messages"][2]["role"], json!("assistant"));
    assert_eq!(body["messages"][2]["tool_calls"][0]["id"], json!("call-1"));
    assert_eq!(
        body["messages"][2]["tool_calls"][0]["function"]["arguments"],
        json!("{\"query\":\"Acme\"}")
    );
    assert_eq!(body["messages"][3]["role"], json!("tool"));
    assert_eq!(body["messages"][3]["tool_call_id"], json!("call-1"));
    let system = body["messages"][0]["content"].as_str().unwrap_or_default();
    assert!(system.contains("read-only tools"));
    assert!(system.contains("provider writes"));
}

#[test]
fn openai_tool_turn_preserves_grouped_prior_tool_calls() {
    let fake = FakeTransport::with_responses(vec![Ok(DirectLlmTransportResponse {
        status: 200,
        headers: BTreeMap::new(),
        body: json!({
            "choices":[{
                "message":{"content":"{\"schema_version\":1}"},
                "finish_reason":"stop"
            }]
        })
        .to_string(),
    })]);
    let captured = fake.requests.clone();
    let client =
        OpenAiCompatibleDirectLlmClient::with_transport(openrouter_config(), Box::new(fake));

    let result = client.complete_typed_task_turn(
        &tool_request(),
        &DirectLlmToolTurnRequest {
            tools: vec![DirectLlmToolDefinition {
                name: "crm_account_lookup".to_string(),
                description: "Read-only CRM account lookup.".to_string(),
                parameters_schema: json!({"type": "object"}),
            }],
            prior_tool_turns: vec![DirectLlmToolTurn {
                tool_calls: vec![
                    DirectLlmToolCall {
                        id: "call-1".to_string(),
                        name: "crm_account_lookup".to_string(),
                        arguments: json!({"query": "Acme"}),
                    },
                    DirectLlmToolCall {
                        id: "call-2".to_string(),
                        name: "crm_account_lookup".to_string(),
                        arguments: json!({"query": "Beta"}),
                    },
                ],
                tool_results: vec![
                    DirectLlmToolResult {
                        call_id: "call-1".to_string(),
                        name: "crm_account_lookup".to_string(),
                        arguments: json!({"query": "Acme"}),
                        result_json: json!({"matches": []}),
                    },
                    DirectLlmToolResult {
                        call_id: "call-2".to_string(),
                        name: "crm_account_lookup".to_string(),
                        arguments: json!({"query": "Beta"}),
                        result_json: json!({"matches": [{"id": "beta"}]}),
                    },
                ],
            }],
        },
    );
    assert!(result.is_ok(), "{result:?}");

    let requests = match captured.lock() {
        Ok(requests) => requests,
        Err(error) => error.into_inner(),
    };
    let body: serde_json::Value =
        serde_json::from_str(&requests[0].body).expect("request body is JSON");
    assert_eq!(body["messages"][2]["role"], json!("assistant"));
    assert_eq!(
        body["messages"][2]["tool_calls"]
            .as_array()
            .expect("tool_calls")
            .len(),
        2
    );
    assert_eq!(body["messages"][3]["role"], json!("tool"));
    assert_eq!(body["messages"][3]["tool_call_id"], json!("call-1"));
    assert_eq!(body["messages"][4]["role"], json!("tool"));
    assert_eq!(body["messages"][4]["tool_call_id"], json!("call-2"));
}

#[test]
fn openai_tool_turn_returns_tool_calls() {
    let client = client_with_response(DirectLlmTransportResponse {
        status: 200,
        headers: BTreeMap::from([("request-id".to_string(), "req-tool-call".to_string())]),
        body: json!({
            "choices":[{
                "message":{
                    "tool_calls":[{
                        "id": "call-1",
                        "type": "function",
                        "function": {
                            "name": "crm_account_lookup",
                            "arguments": "{\"query\":\"Acme\"}"
                        }
                    }]
                },
                "finish_reason":"tool_calls"
            }]
        })
        .to_string(),
    });

    let result = client.complete_typed_task_turn(
        &tool_request(),
        &DirectLlmToolTurnRequest {
            tools: vec![DirectLlmToolDefinition {
                name: "crm_account_lookup".to_string(),
                description: "Read-only CRM account lookup.".to_string(),
                parameters_schema: json!({"type": "object"}),
            }],
            prior_tool_turns: Vec::new(),
        },
    );
    assert!(result.is_ok(), "{result:?}");
    let Ok(DirectLlmToolTurnResponse::ToolCalls {
        tool_calls,
        provider_request_id,
        ..
    }) = result
    else {
        return;
    };

    assert_eq!(provider_request_id.as_deref(), Some("req-tool-call"));
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0].name, "crm_account_lookup");
    assert_eq!(tool_calls[0].arguments, json!({"query": "Acme"}));
}

#[test]
fn direct_client_rejects_oversized_input_before_transport() {
    let fake = FakeTransport::default();
    let client =
        OpenAiCompatibleDirectLlmClient::with_transport(openrouter_config(), Box::new(fake));
    let mut request = request();
    request.spec.max_input_bytes = 4;

    let result = client.complete_typed_task(&request);
    assert!(result.is_err());
    let Err(error) = result else {
        return;
    };

    assert_eq!(error.code(), "direct_llm_input_too_large");
}

#[test]
fn direct_client_rejects_oversized_output_before_content_parse() {
    let client = client_with_response(DirectLlmTransportResponse {
        status: 200,
        headers: BTreeMap::new(),
        body: json!({
            "choices":[{"message":{"content":"{\"schema_version\":1}"}}],
        })
        .to_string(),
    });
    let mut request = request();
    request.spec.max_output_bytes = 4;

    let result = client.complete_typed_task(&request);
    assert!(result.is_err());
    let Err(error) = result else {
        return;
    };

    assert_eq!(error.code(), "direct_llm_response_too_large");
}

#[test]
fn direct_client_rejects_non_object_json_output() {
    let client = client_with_response(DirectLlmTransportResponse {
        status: 200,
        headers: BTreeMap::new(),
        body: json!({
            "choices":[{"message":{"content":"[1,2,3]"}}],
        })
        .to_string(),
    });

    let result = client.complete_typed_task(&request());
    assert!(result.is_err());
    let Err(error) = result else {
        return;
    };

    assert_eq!(error.code(), "direct_llm_schema_mismatch");
}

#[test]
fn direct_client_rejects_malformed_json_output() {
    let client = client_with_response(DirectLlmTransportResponse {
        status: 200,
        headers: BTreeMap::new(),
        body: json!({
            "choices":[{"message":{"content":"{\"schema_version\":"}}],
        })
        .to_string(),
    });

    let result = client.complete_typed_task(&request());
    assert!(result.is_err());
    let Err(error) = result else {
        return;
    };

    assert_eq!(error.code(), "direct_llm_response_parse_failed");
}

#[test]
fn direct_client_maps_provider_status_classes() {
    let cases = [
        (
            429,
            BTreeMap::from([("retry-after".to_string(), "3".to_string())]),
            "direct_llm_rate_limited",
        ),
        (401, BTreeMap::new(), "direct_llm_provider_auth_failed"),
        (403, BTreeMap::new(), "direct_llm_provider_rejected"),
        (503, BTreeMap::new(), "direct_llm_provider_5xx"),
    ];

    for (status, headers, expected_code) in cases {
        let client = client_with_response(DirectLlmTransportResponse {
            status,
            headers,
            body: "{}".to_string(),
        });

        let result = client.complete_typed_task(&request());
        assert!(result.is_err());
        let Err(error) = result else {
            return;
        };

        assert_eq!(error.code(), expected_code);
        if status == 429 {
            assert!(error.message().contains("retry-after=3"));
        }
    }
}

#[test]
fn direct_client_error_message_includes_provider_request_id_when_present() {
    let client = client_with_response(DirectLlmTransportResponse {
        status: 429,
        headers: BTreeMap::from([
            ("retry-after".to_string(), "3".to_string()),
            ("request-id".to_string(), "req-rate-limit".to_string()),
        ]),
        body: "{}".to_string(),
    });

    let result = client.complete_typed_task(&request());
    assert!(result.is_err());
    let Err(error) = result else {
        return;
    };

    assert_eq!(error.code(), "direct_llm_rate_limited");
    assert!(error
        .message()
        .contains("provider_request_id=req-rate-limit"));
}

#[test]
fn direct_client_retries_rate_limit_using_retry_after_without_real_sleep() {
    let recording = client_with_sequence(vec![
        Ok(DirectLlmTransportResponse {
            status: 429,
            headers: BTreeMap::from([("retry-after".to_string(), "3".to_string())]),
            body: "{}".to_string(),
        }),
        Ok(success_response()),
    ]);

    let envelope_result = recording
        .client
        .complete_typed_task(&retry_request(2, 25, 4_000));
    assert!(envelope_result.is_ok(), "{envelope_result:?}");
    let Ok(envelope) = envelope_result else {
        return;
    };

    assert_eq!(envelope.retry_count, 1);
    assert_eq!(locked_len(&recording.requests), 2);
    assert_eq!(
        locked_durations(&recording.delays),
        vec![Duration::from_millis(3_000)]
    );
}

#[test]
fn direct_client_retries_5xx_with_policy_backoff() {
    let recording = client_with_sequence(vec![
        Ok(DirectLlmTransportResponse {
            status: 503,
            headers: BTreeMap::new(),
            body: "{}".to_string(),
        }),
        Ok(DirectLlmTransportResponse {
            status: 500,
            headers: BTreeMap::new(),
            body: "{}".to_string(),
        }),
        Ok(success_response()),
    ]);

    let envelope_result = recording
        .client
        .complete_typed_task(&retry_request(3, 25, 500));
    assert!(envelope_result.is_ok(), "{envelope_result:?}");
    let Ok(envelope) = envelope_result else {
        return;
    };

    assert_eq!(envelope.retry_count, 2);
    assert_eq!(locked_len(&recording.requests), 3);
    assert_eq!(
        locked_durations(&recording.delays),
        vec![Duration::from_millis(25), Duration::from_millis(25)]
    );
}

#[test]
fn direct_client_retries_transport_backoff_error() {
    let recording =
        client_with_sequence(vec![Err(timeout_transport_error()), Ok(success_response())]);

    let envelope_result = recording
        .client
        .complete_typed_task(&retry_request(2, 40, 500));
    assert!(envelope_result.is_ok(), "{envelope_result:?}");
    let Ok(envelope) = envelope_result else {
        return;
    };

    assert_eq!(envelope.retry_count, 1);
    assert_eq!(locked_len(&recording.requests), 2);
    assert_eq!(
        locked_durations(&recording.delays),
        vec![Duration::from_millis(40)]
    );
}

#[test]
fn direct_client_does_not_retry_non_backoff_transport_error() {
    let recording = client_with_sequence(vec![Err(AppError::unexpected(
        "direct_llm_transport_failed",
        "direct LLM transport failed permanently",
        CorrelationId::generate(),
    ))]);

    let result = recording
        .client
        .complete_typed_task(&retry_request(3, 40, 500));
    assert!(result.is_err());
    let Err(error) = result else {
        return;
    };

    assert_eq!(error.code(), "direct_llm_transport_failed");
    assert_eq!(locked_len(&recording.requests), 1);
    assert!(locked_durations(&recording.delays).is_empty());
}

#[test]
fn direct_client_does_not_retry_non_retryable_provider_failure() {
    let recording = client_with_sequence(vec![Ok(DirectLlmTransportResponse {
        status: 400,
        headers: BTreeMap::new(),
        body: "{}".to_string(),
    })]);

    let result = recording
        .client
        .complete_typed_task(&retry_request(3, 25, 500));
    assert!(result.is_err());
    let Err(error) = result else {
        return;
    };

    assert_eq!(error.code(), "direct_llm_provider_rejected");
    assert_eq!(locked_len(&recording.requests), 1);
    assert!(locked_durations(&recording.delays).is_empty());
}

#[test]
fn direct_client_stops_retrying_at_max_attempts() {
    let recording = client_with_sequence(vec![
        Ok(DirectLlmTransportResponse {
            status: 503,
            headers: BTreeMap::new(),
            body: "{}".to_string(),
        }),
        Ok(DirectLlmTransportResponse {
            status: 503,
            headers: BTreeMap::new(),
            body: "{}".to_string(),
        }),
        Ok(success_response()),
    ]);

    let result = recording
        .client
        .complete_typed_task(&retry_request(2, 25, 500));
    assert!(result.is_err());
    let Err(error) = result else {
        return;
    };

    assert_eq!(error.code(), "direct_llm_provider_5xx");
    assert_eq!(locked_len(&recording.requests), 2);
    assert_eq!(
        locked_durations(&recording.delays),
        vec![Duration::from_millis(25)]
    );
}

#[test]
fn direct_client_skips_retry_when_retry_after_exceeds_elapsed_budget() {
    let recording = client_with_sequence(vec![Ok(DirectLlmTransportResponse {
        status: 429,
        headers: BTreeMap::from([("retry-after".to_string(), "2".to_string())]),
        body: "{}".to_string(),
    })]);

    let result = recording
        .client
        .complete_typed_task(&retry_request(3, 25, 1_000));
    assert!(result.is_err());
    let Err(error) = result else {
        return;
    };

    assert_eq!(error.code(), "direct_llm_rate_limited");
    assert!(error.message().contains("retry-after=2"));
    assert_eq!(locked_len(&recording.requests), 1);
    assert!(locked_durations(&recording.delays).is_empty());
}
