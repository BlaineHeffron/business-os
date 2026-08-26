//! Anthropic Messages-API client for bounded typed transforms. Ported from
//! agent-monitor-rust `direct_llm_anthropic.rs` + the Anthropic wire types
//! from its `llm_client::anthropic` (inlined here — BusinessOS has no other
//! Anthropic caller).
//!
//! SIMPLIFICATION vs agent_monitor: agent_monitor resolved Structured Outputs schemas from its
//! `dm_business` schema registry. BusinessOS has no schema registry yet, so the
//! lookup is an injectable seam ([`SchemaLookup`], default: no schema, which
//! omits `output_config` and relies on the JSON-only system prompt). Wire a
//! real registry through [`AnthropicDirectLlmClient::new_with_schema_lookup`]
//! when one exists.

use crate::llm_api::response::{
    enforce_max_input_bytes, enforce_max_output_bytes, hash_response, map_provider_status_error,
    parse_json_object_text, preferred_provider_request_id, DirectLlmStatusEnvelope,
};
use crate::llm_api::retry::{
    app_error_retry_delay, retry_allowed, status_retry_delay, validate_retry_policy,
    DirectLlmRetrySleeper, ThreadDirectLlmRetrySleeper,
};
use crate::llm_api::{
    DirectLlmClient, DirectLlmTransport, DirectLlmTransportRequest, ReqwestDirectLlmTransport,
};
use crate::llm_typed_tasks::{
    TypedLlmExecutionRoute, TypedLlmTaskOutputEnvelope, TypedLlmTaskRequest,
};
use bos_kernel::{AppError, AppResult, CorrelationId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Instant;

const DEFAULT_TEMPERATURE: f32 = 0.0;
pub const PROMPT_CACHING_BETA_HEADER: &str = "prompt-caching-2024-07-31";

/// Resolves a JSON schema for a `schema_ref`, enabling Anthropic Structured
/// Outputs when present. `None` = no schema registered for that ref.
pub type SchemaLookup = fn(&str) -> Option<Value>;

fn no_schema_lookup(_schema_ref: &str) -> Option<Value> {
    None
}

#[derive(Clone, PartialEq, Eq)]
pub struct AnthropicDirectLlmConfig {
    pub provider_id: String,
    pub api_key: String,
    pub model: String,
    pub endpoint: String,
    pub timeout_ms: u64,
}

impl std::fmt::Debug for AnthropicDirectLlmConfig {
    // Hand-written so a stray `{:?}` cannot dump the api_key.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AnthropicDirectLlmConfig")
            .field("provider_id", &self.provider_id)
            .field("api_key", &"[redacted]")
            .field("model", &self.model)
            .field("endpoint", &self.endpoint)
            .field("timeout_ms", &self.timeout_ms)
            .finish()
    }
}

impl AnthropicDirectLlmConfig {
    pub fn anthropic(
        api_key: impl Into<String>,
        model: impl Into<String>,
        endpoint: impl Into<String>,
    ) -> Self {
        Self {
            provider_id: "anthropic".to_string(),
            api_key: api_key.into(),
            model: model.into(),
            endpoint: endpoint.into(),
            timeout_ms: 20_000,
        }
    }
}

pub struct AnthropicDirectLlmClient {
    config: AnthropicDirectLlmConfig,
    transport: Box<dyn DirectLlmTransport>,
    retry_sleeper: Box<dyn DirectLlmRetrySleeper>,
    schema_lookup: SchemaLookup,
}

impl AnthropicDirectLlmClient {
    pub fn new(config: AnthropicDirectLlmConfig) -> AppResult<Self> {
        Self::new_with_schema_lookup(config, no_schema_lookup)
    }

    pub fn new_with_schema_lookup(
        config: AnthropicDirectLlmConfig,
        schema_lookup: SchemaLookup,
    ) -> AppResult<Self> {
        let transport = ReqwestDirectLlmTransport::new(config.timeout_ms)?;
        Ok(Self {
            config,
            transport: Box::new(transport),
            retry_sleeper: Box::new(ThreadDirectLlmRetrySleeper),
            schema_lookup,
        })
    }

    #[cfg(test)]
    fn with_transport(
        config: AnthropicDirectLlmConfig,
        transport: Box<dyn DirectLlmTransport>,
        schema_lookup: SchemaLookup,
    ) -> Self {
        Self {
            config,
            transport,
            retry_sleeper: Box::new(ThreadDirectLlmRetrySleeper),
            schema_lookup,
        }
    }

    #[cfg(test)]
    fn with_transport_and_sleeper(
        config: AnthropicDirectLlmConfig,
        transport: Box<dyn DirectLlmTransport>,
        retry_sleeper: Box<dyn DirectLlmRetrySleeper>,
        schema_lookup: SchemaLookup,
    ) -> Self {
        Self {
            config,
            transport,
            retry_sleeper,
            schema_lookup,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicTextBlock {
    #[serde(rename = "type")]
    pub kind: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<AnthropicCacheControl>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicCacheControl {
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicMessage {
    pub role: String,
    pub content: Vec<AnthropicTextBlock>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AnthropicMessagesResponse {
    #[serde(default)]
    pub content: Vec<AnthropicContentBlock>,
    #[serde(default)]
    pub stop_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AnthropicContentBlock {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub text: Option<String>,
}

pub fn anthropic_headers(api_key: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("x-api-key".to_string(), api_key.to_string()),
        ("anthropic-version".to_string(), "2023-06-01".to_string()),
        (
            "anthropic-beta".to_string(),
            PROMPT_CACHING_BETA_HEADER.to_string(),
        ),
        ("content-type".to_string(), "application/json".to_string()),
    ])
}

#[derive(Debug, Serialize)]
struct AnthropicStructuredRequest {
    model: String,
    max_tokens: u16,
    temperature: f32,
    system: Vec<AnthropicTextBlock>,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_config: Option<AnthropicOutputConfig>,
}

#[derive(Debug, Serialize)]
struct AnthropicOutputConfig {
    format: AnthropicOutputFormat,
}

#[derive(Debug, Serialize)]
struct AnthropicOutputFormat {
    #[serde(rename = "type")]
    kind: &'static str,
    schema: Value,
}

impl DirectLlmClient for AnthropicDirectLlmClient {
    fn complete_typed_task(
        &self,
        request: &TypedLlmTaskRequest,
    ) -> AppResult<TypedLlmTaskOutputEnvelope> {
        reject_unsafe_anthropic_request(request)?;
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

        let output_config =
            (self.schema_lookup)(&request.spec.schema_ref).map(|schema| AnthropicOutputConfig {
                format: AnthropicOutputFormat {
                    kind: "json_schema",
                    schema,
                },
            });

        let body = serde_json::to_string(&AnthropicStructuredRequest {
            model: self.config.model.clone(),
            // spec.max_tokens is u32; the Anthropic field is u16. Current Claude output
            // ceilings are < u16::MAX, so saturating is a no-op in practice. Clamp (not
            // error) so an over-large config degrades to the max rather than failing the task.
            max_tokens: u16::try_from(request.spec.max_tokens).unwrap_or(u16::MAX),
            temperature: DEFAULT_TEMPERATURE,
            system: vec![AnthropicTextBlock {
                kind: "text".to_string(),
                text: typed_task_system_prompt(request),
                cache_control: Some(AnthropicCacheControl {
                    kind: "ephemeral".to_string(),
                }),
            }],
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: vec![AnthropicTextBlock {
                    kind: "text".to_string(),
                    text: input_json,
                    cache_control: None,
                }],
            }],
            output_config,
        })
        .map_err(|error| {
            AppError::unexpected(
                "direct_llm_request_encode_failed",
                format!("failed to encode direct LLM request: {error}"),
                CorrelationId::generate(),
            )
        })?;

        let mut attempts: u8 = 0;
        loop {
            attempts = attempts.saturating_add(1);
            let response = match self.transport.send(&DirectLlmTransportRequest {
                method: "POST",
                url: self.config.endpoint.clone(),
                headers: anthropic_headers(&self.config.api_key),
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

            let parsed: AnthropicMessagesResponse =
                serde_json::from_str(&response.body).map_err(|error| {
                    AppError::unexpected(
                        "direct_llm_response_parse_failed",
                        format!("failed to parse Anthropic response: {error}"),
                        CorrelationId::generate(),
                    )
                })?;

            // 200 with non-conforming output: check stop_reason BEFORE parsing JSON.
            if let Some(stop_reason) = parsed.stop_reason.as_deref() {
                match stop_reason {
                    "refusal" => {
                        return Err(AppError::policy(
                            "direct_llm_anthropic_refusal",
                            "Anthropic declined to produce structured output (stop_reason=refusal)",
                            CorrelationId::generate(),
                        ));
                    }
                    "max_tokens" => {
                        return Err(AppError::invalid_input(
                            "direct_llm_anthropic_output_truncated",
                            "Anthropic output was truncated before completing valid JSON (stop_reason=max_tokens)",
                            CorrelationId::generate(),
                        ));
                    }
                    _ => {}
                }
            }

            let response_text = first_text_block(&parsed.content).ok_or_else(|| {
                AppError::unexpected(
                    "direct_llm_response_empty",
                    "Anthropic response did not contain JSON content",
                    CorrelationId::generate(),
                )
            })?;
            enforce_max_output_bytes(response_text, request.spec.max_output_bytes)?;
            let response_json =
                parse_json_object_text(response_text, "Anthropic response content")?;

            return Ok(TypedLlmTaskOutputEnvelope {
                task_id: request.task_id.clone(),
                execution_route: TypedLlmExecutionRoute::DirectApi,
                provider_id: self.config.provider_id.clone(),
                model: self.config.model.clone(),
                schema_ref: request.spec.schema_ref.clone(),
                raw_response_hash: hash_response(response_text.as_bytes()),
                response_json,
                usage: None,
                finish_reason: parsed.stop_reason.clone(),
                latency_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                retry_count: attempts.saturating_sub(1),
                provider_request_id: preferred_provider_request_id(&response.headers),
                correlation_id: request.correlation_id.clone(),
            });
        }
    }
}

fn first_text_block(blocks: &[AnthropicContentBlock]) -> Option<&str> {
    blocks
        .iter()
        .filter(|block| block.kind == "text")
        .filter_map(|block| block.text.as_deref())
        .map(str::trim)
        .find(|text| !text.is_empty())
}

fn typed_task_system_prompt(request: &TypedLlmTaskRequest) -> String {
    format!(
        "You perform one bounded typed transformation.\n\
         Output JSON only for schema_ref={}.\n\
         Prompt template id={} version={} hash={}.\n\
         Side effects, provider writes, browsing, tools, filesystem access, and route changes are forbidden.",
        request.spec.schema_ref,
        request.spec.prompt_template_id,
        request.spec.prompt_template_version,
        request.spec.prompt_template_hash
    )
}

fn reject_unsafe_anthropic_request(request: &TypedLlmTaskRequest) -> AppResult<()> {
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
    // Unlike the OpenAI-compatible client, BOTH JsonObject and JsonSchema are accepted;
    // Structured Outputs engagement is decided by schema-lookup presence, not response_format.
    Ok(())
}

#[cfg(test)]
mod tests;
