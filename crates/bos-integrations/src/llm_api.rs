//! Direct-API backend for bounded typed LLM transforms: OpenAI-compatible
//! (OpenAI/OpenRouter) and Anthropic clients over one blocking transport.
//! Ported from agent-monitor-rust `dm-integrations::direct_llm*` with
//! `dm_kernel` → `bos_kernel` and the typed-task contract from
//! [`crate::llm_typed_tasks`]. No env reads: configuration arrives as structs
//! built by bos-app (`llm.rs`).

use crate::llm_typed_tasks::{
    TypedLlmExecutionRoute, TypedLlmResponseFormat, TypedLlmTaskOutputEnvelope, TypedLlmTaskRequest,
};
use bos_kernel::{AppError, AppResult, CorrelationId, ErrorCode};
use payload::{
    build_openai_compatible_request_body, build_openai_compatible_tool_turn_request_body,
    openai_compatible_headers,
};
use reqwest::blocking::Client;
use reqwest::Method;
use response::{
    enforce_max_input_bytes, extract_typed_json_object, extract_typed_tool_turn, hash_response,
    map_provider_status_error, preferred_provider_request_id, DirectLlmStatusEnvelope,
    OpenAiToolTurnExtraction,
};
use retry::{
    app_error_retry_delay, retry_allowed, status_retry_delay, validate_retry_policy,
    DirectLlmRetrySleeper, ThreadDirectLlmRetrySleeper,
};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

pub mod anthropic;
pub(crate) mod payload;
pub(crate) mod response;
pub(crate) mod retry;

pub trait DirectLlmClient: Send + Sync {
    fn complete_typed_task(
        &self,
        request: &TypedLlmTaskRequest,
    ) -> AppResult<TypedLlmTaskOutputEnvelope>;

    fn complete_typed_task_turn(
        &self,
        request: &TypedLlmTaskRequest,
        turn: &DirectLlmToolTurnRequest,
    ) -> AppResult<DirectLlmToolTurnResponse> {
        if turn.tools.is_empty() && turn.prior_tool_turns.is_empty() {
            return self
                .complete_typed_task(request)
                .map(DirectLlmToolTurnResponse::Final);
        }
        Err(AppError::conflict(
            "direct_llm_tools_unsupported",
            "direct LLM client does not support typed task tool turns",
            CorrelationId::generate(),
        ))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DirectLlmToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters_schema: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DirectLlmToolResult {
    pub call_id: String,
    pub name: String,
    pub arguments: serde_json::Value,
    pub result_json: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DirectLlmToolTurn {
    pub tool_calls: Vec<DirectLlmToolCall>,
    pub tool_results: Vec<DirectLlmToolResult>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DirectLlmToolTurnRequest {
    pub tools: Vec<DirectLlmToolDefinition>,
    pub prior_tool_turns: Vec<DirectLlmToolTurn>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DirectLlmToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DirectLlmToolTurnResponse {
    Final(TypedLlmTaskOutputEnvelope),
    ToolCalls {
        provider_id: String,
        model: String,
        tool_calls: Vec<DirectLlmToolCall>,
        usage: Option<crate::llm_typed_tasks::TypedLlmUsage>,
        finish_reason: Option<String>,
        latency_ms: u64,
        provider_request_id: Option<String>,
    },
}

#[derive(Clone, PartialEq, Eq)]
pub struct OpenAiCompatibleDirectLlmConfig {
    pub provider_id: String,
    pub api_key: String,
    pub model: String,
    pub endpoint: String,
    pub timeout_ms: u64,
}

impl std::fmt::Debug for OpenAiCompatibleDirectLlmConfig {
    // Hand-written so a stray `{:?}` cannot dump the api_key.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenAiCompatibleDirectLlmConfig")
            .field("provider_id", &self.provider_id)
            .field("api_key", &"[redacted]")
            .field("model", &self.model)
            .field("endpoint", &self.endpoint)
            .field("timeout_ms", &self.timeout_ms)
            .finish()
    }
}

impl OpenAiCompatibleDirectLlmConfig {
    pub fn openrouter(
        api_key: impl Into<String>,
        model: impl Into<String>,
        endpoint: impl Into<String>,
    ) -> Self {
        Self {
            provider_id: "openrouter".to_string(),
            api_key: api_key.into(),
            model: model.into(),
            endpoint: endpoint.into(),
            timeout_ms: 20_000,
        }
    }

    pub fn openai(
        api_key: impl Into<String>,
        model: impl Into<String>,
        endpoint: impl Into<String>,
    ) -> Self {
        Self {
            provider_id: "openai".to_string(),
            api_key: api_key.into(),
            model: model.into(),
            endpoint: endpoint.into(),
            timeout_ms: 20_000,
        }
    }
}

pub struct OpenAiCompatibleDirectLlmClient {
    config: OpenAiCompatibleDirectLlmConfig,
    transport: Box<dyn DirectLlmTransport>,
    retry_sleeper: Box<dyn DirectLlmRetrySleeper>,
}

impl OpenAiCompatibleDirectLlmClient {
    pub fn new(config: OpenAiCompatibleDirectLlmConfig) -> AppResult<Self> {
        let transport = ReqwestDirectLlmTransport::new(config.timeout_ms)?;
        Ok(Self {
            config,
            transport: Box::new(transport),
            retry_sleeper: Box::new(ThreadDirectLlmRetrySleeper),
        })
    }

    #[cfg(test)]
    fn with_transport(
        config: OpenAiCompatibleDirectLlmConfig,
        transport: Box<dyn DirectLlmTransport>,
    ) -> Self {
        Self {
            config,
            transport,
            retry_sleeper: Box::new(ThreadDirectLlmRetrySleeper),
        }
    }

    #[cfg(test)]
    fn with_transport_and_sleeper(
        config: OpenAiCompatibleDirectLlmConfig,
        transport: Box<dyn DirectLlmTransport>,
        retry_sleeper: Box<dyn DirectLlmRetrySleeper>,
    ) -> Self {
        Self {
            config,
            transport,
            retry_sleeper,
        }
    }
}

impl DirectLlmClient for OpenAiCompatibleDirectLlmClient {
    fn complete_typed_task(
        &self,
        request: &TypedLlmTaskRequest,
    ) -> AppResult<TypedLlmTaskOutputEnvelope> {
        reject_unsafe_direct_request(request)?;
        validate_retry_policy(&request.execution_policy.retry_policy)?;
        let started = Instant::now();
        let input_json = serde_json::to_string(&request.input).map_err(|error| {
            AppError::unexpected(
                "direct_llm_input_encode_failed",
                format!("failed to encode typed task input: {error}"),
                CorrelationId::generate(),
            )
        })?;
        enforce_max_input_bytes(&input_json, request.spec.max_input_bytes)?;
        let body = build_openai_compatible_request_body(request, &self.config.model, input_json)?;
        let mut attempts: u8 = 0;

        loop {
            attempts = attempts.saturating_add(1);
            let response = match self.transport.send(&DirectLlmTransportRequest {
                method: "POST",
                url: self.config.endpoint.clone(),
                headers: openai_compatible_headers(&self.config.api_key),
                body: body.clone(),
            }) {
                Ok(response) => response,
                Err(error) => {
                    if let Some(delay) =
                        app_error_retry_delay(&error, &request.execution_policy.retry_policy)
                            .filter(|delay| {
                                retry_allowed(
                                    &request.execution_policy.retry_policy,
                                    attempts,
                                    started,
                                    *delay,
                                )
                            })
                    {
                        self.retry_sleeper.sleep(delay);
                        continue;
                    }
                    return Err(error);
                }
            };

            if !(200..300).contains(&response.status) {
                if let Some(delay) = status_retry_delay(
                    response.status,
                    &response.headers,
                    &request.execution_policy.retry_policy,
                )
                .filter(|delay| {
                    retry_allowed(
                        &request.execution_policy.retry_policy,
                        attempts,
                        started,
                        *delay,
                    )
                }) {
                    self.retry_sleeper.sleep(delay);
                    continue;
                }
                return Err(map_provider_status_error(&DirectLlmStatusEnvelope {
                    status: response.status,
                    headers: response.headers.clone(),
                }));
            }
            let (parsed, response_json) =
                extract_typed_json_object(&response.body, request.spec.max_output_bytes)?;
            let raw_response_hash = parsed
                .choices
                .iter()
                .filter_map(|choice| choice.message.as_ref())
                .filter_map(|message| message.content.as_deref())
                .map(str::trim)
                .find(|content| !content.is_empty())
                .map(|content| hash_response(content.as_bytes()))
                .ok_or_else(|| {
                    AppError::unexpected(
                        "direct_llm_response_empty",
                        "direct LLM response did not contain JSON content",
                        CorrelationId::generate(),
                    )
                })?;

            return Ok(TypedLlmTaskOutputEnvelope {
                task_id: request.task_id.clone(),
                execution_route: TypedLlmExecutionRoute::DirectApi,
                provider_id: self.config.provider_id.clone(),
                model: self.config.model.clone(),
                schema_ref: request.spec.schema_ref.clone(),
                raw_response_hash,
                response_json,
                usage: parsed.usage.map(Into::into),
                finish_reason: parsed
                    .choices
                    .first()
                    .and_then(|choice| choice.finish_reason.clone()),
                latency_ms: u128_to_u64_saturating(started.elapsed().as_millis()),
                retry_count: attempts.saturating_sub(1),
                provider_request_id: preferred_provider_request_id(&response.headers),
                correlation_id: request.correlation_id.clone(),
            });
        }
    }

    fn complete_typed_task_turn(
        &self,
        request: &TypedLlmTaskRequest,
        turn: &DirectLlmToolTurnRequest,
    ) -> AppResult<DirectLlmToolTurnResponse> {
        reject_unsafe_tool_turn_request(request)?;
        validate_retry_policy(&request.execution_policy.retry_policy)?;
        let started = Instant::now();
        let input_json = serde_json::to_string(&request.input).map_err(|error| {
            AppError::unexpected(
                "direct_llm_input_encode_failed",
                format!("failed to encode typed task input: {error}"),
                CorrelationId::generate(),
            )
        })?;
        enforce_max_input_bytes(&input_json, request.spec.max_input_bytes)?;
        let body = build_openai_compatible_tool_turn_request_body(
            request,
            &self.config.model,
            input_json,
            &turn.tools,
            &turn.prior_tool_turns,
        )?;
        let mut attempts: u8 = 0;

        loop {
            attempts = attempts.saturating_add(1);
            let response = match self.transport.send(&DirectLlmTransportRequest {
                method: "POST",
                url: self.config.endpoint.clone(),
                headers: openai_compatible_headers(&self.config.api_key),
                body: body.clone(),
            }) {
                Ok(response) => response,
                Err(error) => {
                    if let Some(delay) =
                        app_error_retry_delay(&error, &request.execution_policy.retry_policy)
                            .filter(|delay| {
                                retry_allowed(
                                    &request.execution_policy.retry_policy,
                                    attempts,
                                    started,
                                    *delay,
                                )
                            })
                    {
                        self.retry_sleeper.sleep(delay);
                        continue;
                    }
                    return Err(error);
                }
            };

            if !(200..300).contains(&response.status) {
                if let Some(delay) = status_retry_delay(
                    response.status,
                    &response.headers,
                    &request.execution_policy.retry_policy,
                )
                .filter(|delay| {
                    retry_allowed(
                        &request.execution_policy.retry_policy,
                        attempts,
                        started,
                        *delay,
                    )
                }) {
                    self.retry_sleeper.sleep(delay);
                    continue;
                }
                return Err(map_provider_status_error(&DirectLlmStatusEnvelope {
                    status: response.status,
                    headers: response.headers.clone(),
                }));
            }

            let latency_ms = u128_to_u64_saturating(started.elapsed().as_millis());
            match extract_typed_tool_turn(&response.body, request.spec.max_output_bytes)? {
                OpenAiToolTurnExtraction::Final {
                    parsed,
                    response_json,
                    raw_response_hash,
                } => {
                    return Ok(DirectLlmToolTurnResponse::Final(
                        TypedLlmTaskOutputEnvelope {
                            task_id: request.task_id.clone(),
                            execution_route: TypedLlmExecutionRoute::DirectApi,
                            provider_id: self.config.provider_id.clone(),
                            model: self.config.model.clone(),
                            schema_ref: request.spec.schema_ref.clone(),
                            raw_response_hash,
                            response_json,
                            usage: parsed.usage.map(Into::into),
                            finish_reason: parsed
                                .choices
                                .first()
                                .and_then(|choice| choice.finish_reason.clone()),
                            latency_ms,
                            retry_count: attempts.saturating_sub(1),
                            provider_request_id: preferred_provider_request_id(&response.headers),
                            correlation_id: request.correlation_id.clone(),
                        },
                    ));
                }
                OpenAiToolTurnExtraction::ToolCalls { parsed, tool_calls } => {
                    return Ok(DirectLlmToolTurnResponse::ToolCalls {
                        provider_id: self.config.provider_id.clone(),
                        model: self.config.model.clone(),
                        tool_calls,
                        usage: parsed.usage.map(Into::into),
                        finish_reason: parsed
                            .choices
                            .first()
                            .and_then(|choice| choice.finish_reason.clone()),
                        latency_ms,
                        provider_request_id: preferred_provider_request_id(&response.headers),
                    });
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct MockDirectLlmClient {
    envelope: TypedLlmTaskOutputEnvelope,
}

impl MockDirectLlmClient {
    pub fn new(envelope: TypedLlmTaskOutputEnvelope) -> Self {
        Self { envelope }
    }
}

impl DirectLlmClient for MockDirectLlmClient {
    fn complete_typed_task(
        &self,
        _request: &TypedLlmTaskRequest,
    ) -> AppResult<TypedLlmTaskOutputEnvelope> {
        Ok(self.envelope.clone())
    }
}

#[derive(Debug)]
pub struct MockScriptedDirectLlmClient {
    turns: std::sync::Mutex<Vec<DirectLlmToolTurnResponse>>,
}

impl MockScriptedDirectLlmClient {
    pub fn new(turns: Vec<DirectLlmToolTurnResponse>) -> Self {
        Self {
            turns: std::sync::Mutex::new(turns),
        }
    }

    pub fn remaining_turns(&self) -> AppResult<usize> {
        self.turns
            .lock()
            .map(|turns| turns.len())
            .map_err(|_| mock_script_lock_poisoned())
    }
}

impl DirectLlmClient for MockScriptedDirectLlmClient {
    fn complete_typed_task(
        &self,
        _request: &TypedLlmTaskRequest,
    ) -> AppResult<TypedLlmTaskOutputEnvelope> {
        Err(AppError::conflict(
            "mock_direct_llm_script_requires_tool_turn",
            "scripted direct LLM mock requires complete_typed_task_turn",
            CorrelationId::generate(),
        ))
    }

    fn complete_typed_task_turn(
        &self,
        _request: &TypedLlmTaskRequest,
        _turn: &DirectLlmToolTurnRequest,
    ) -> AppResult<DirectLlmToolTurnResponse> {
        let mut turns = self.turns.lock().map_err(|_| mock_script_lock_poisoned())?;
        if turns.is_empty() {
            return Err(AppError::conflict(
                "mock_direct_llm_script_exhausted",
                "scripted direct LLM mock exhausted",
                CorrelationId::generate(),
            ));
        }
        Ok(turns.remove(0))
    }
}

fn mock_script_lock_poisoned() -> AppError {
    AppError::unexpected(
        "mock_direct_llm_script_lock_poisoned",
        "scripted direct LLM mock lock poisoned",
        CorrelationId::generate(),
    )
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct DirectLlmTransportRequest {
    pub(crate) method: &'static str,
    pub(crate) url: String,
    pub(crate) headers: BTreeMap<String, String>,
    pub(crate) body: String,
}

impl std::fmt::Debug for DirectLlmTransportRequest {
    // Hand-written so auth headers (Authorization / x-api-key) are redacted in
    // any `{:?}` rendering of the outbound request.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DirectLlmTransportRequest")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("headers", &RedactedDirectLlmHeaders(&self.headers))
            .field("body", &self.body)
            .finish()
    }
}

/// Renders an HTTP header map with secret-bearing header values redacted.
struct RedactedDirectLlmHeaders<'a>(&'a BTreeMap<String, String>);

impl std::fmt::Debug for RedactedDirectLlmHeaders<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut map = formatter.debug_map();
        for (key, value) in self.0 {
            let sensitive = matches!(
                key.to_ascii_lowercase().as_str(),
                "authorization"
                    | "proxy-authorization"
                    | "x-api-key"
                    | "api-key"
                    | "cookie"
                    | "set-cookie"
            );
            if sensitive {
                map.entry(key, &"[redacted]");
            } else {
                map.entry(key, value);
            }
        }
        map.finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DirectLlmTransportResponse {
    pub(crate) status: u16,
    pub(crate) headers: BTreeMap<String, String>,
    pub(crate) body: String,
}

pub(crate) trait DirectLlmTransport: Send + Sync {
    fn send(&self, request: &DirectLlmTransportRequest) -> AppResult<DirectLlmTransportResponse>;
}

#[derive(Debug, Clone)]
pub(crate) struct ReqwestDirectLlmTransport {
    client: Client,
}

impl ReqwestDirectLlmTransport {
    pub(crate) fn new(timeout_ms: u64) -> AppResult<Self> {
        let client = Client::builder()
            .timeout(Duration::from_millis(timeout_ms))
            .build()
            .map_err(|error| {
                AppError::unexpected(
                    "direct_llm_client_build_failed",
                    format!("failed to build direct LLM client: {error}"),
                    CorrelationId::generate(),
                )
            })?;
        Ok(Self { client })
    }
}

impl DirectLlmTransport for ReqwestDirectLlmTransport {
    fn send(&self, request: &DirectLlmTransportRequest) -> AppResult<DirectLlmTransportResponse> {
        let method = Method::from_bytes(request.method.as_bytes()).map_err(|error| {
            AppError::unexpected(
                "direct_llm_transport_method_invalid",
                format!("direct LLM transport method is invalid: {error}"),
                CorrelationId::generate(),
            )
        })?;
        let mut builder = self.client.request(method, &request.url);
        for (key, value) in &request.headers {
            builder = builder.header(key.as_str(), value.as_str());
        }
        let response = builder.body(request.body.clone()).send().map_err(|error| {
            let (kind, code) = if error.is_timeout() {
                (ErrorCode::Timeout, "direct_llm_timeout")
            } else {
                (ErrorCode::ExternalDependency, "direct_llm_transport_failed")
            };
            AppError::new(
                kind,
                code,
                format!("direct LLM transport failed: {error}"),
                CorrelationId::generate(),
            )
        })?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(key, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|value| (key.as_str().to_ascii_lowercase(), value.to_string()))
            })
            .collect::<BTreeMap<_, _>>();
        let body = response.text().map_err(|error| {
            AppError::new(
                ErrorCode::ExternalDependency,
                "direct_llm_transport_body_failed",
                format!("direct LLM response body read failed: {error}"),
                CorrelationId::generate(),
            )
        })?;
        Ok(DirectLlmTransportResponse {
            status,
            headers,
            body,
        })
    }
}

fn reject_unsafe_direct_request(request: &TypedLlmTaskRequest) -> AppResult<()> {
    if request.execution_policy.default_route != TypedLlmExecutionRoute::DirectApi {
        return Err(AppError::unexpected(
            "direct_llm_task_route_not_direct",
            "typed LLM task is not configured for direct_api route",
            CorrelationId::generate(),
        ));
    }
    if request.spec.capabilities.requires_harness() {
        return Err(AppError::unexpected(
            "direct_llm_task_requires_harness",
            "typed LLM task declares capabilities that require harness execution",
            CorrelationId::generate(),
        ));
    }
    if request.spec.authority.provider_writes_enabled {
        return Err(AppError::unexpected(
            "direct_llm_provider_writes_forbidden",
            "direct typed LLM task cannot enable provider writes",
            CorrelationId::generate(),
        ));
    }
    if request.spec.response_format != TypedLlmResponseFormat::JsonObject {
        return Err(AppError::invalid_input(
            "direct_llm_response_format_unsupported",
            "OpenAI-compatible direct LLM client currently supports json_object response format only",
            CorrelationId::generate(),
        ));
    }
    Ok(())
}

fn reject_unsafe_tool_turn_request(request: &TypedLlmTaskRequest) -> AppResult<()> {
    if request.spec.authority.provider_writes_enabled {
        return Err(AppError::new(
            ErrorCode::Policy,
            "direct_llm_provider_writes_forbidden",
            "typed LLM tool turn cannot enable provider writes",
            CorrelationId::generate(),
        ));
    }
    if !request.spec.authority.side_effects_forbidden {
        return Err(AppError::new(
            ErrorCode::Policy,
            "direct_llm_side_effects_forbidden_required",
            "typed LLM tool turn requires side_effects_forbidden authority",
            CorrelationId::generate(),
        ));
    }
    Ok(())
}

fn u128_to_u64_saturating(value: u128) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests;
