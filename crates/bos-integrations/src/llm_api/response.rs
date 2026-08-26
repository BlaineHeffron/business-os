//! OpenAI-compatible response parsing + shared output bounds/hash helpers.
//! Ported verbatim from agent-monitor-rust `direct_llm_response.rs`.

use crate::llm_api::DirectLlmToolCall;
use crate::llm_typed_tasks::TypedLlmUsage;
use bos_kernel::{AppError, AppResult, CorrelationId, ErrorCode};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Deserialize)]
pub(crate) struct OpenAiTypedTaskResponse {
    #[serde(default)]
    pub(crate) choices: Vec<OpenAiTypedChoice>,
    pub(crate) usage: Option<OpenAiTypedUsage>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OpenAiTypedChoice {
    pub(crate) message: Option<OpenAiTypedMessage>,
    pub(crate) finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OpenAiTypedMessage {
    pub(crate) content: Option<String>,
    #[serde(default)]
    pub(crate) tool_calls: Vec<OpenAiRawToolCall>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OpenAiRawToolCall {
    pub(crate) id: String,
    pub(crate) function: OpenAiRawToolFunction,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OpenAiRawToolFunction {
    pub(crate) name: String,
    pub(crate) arguments: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OpenAiTypedUsage {
    pub(crate) prompt_tokens: Option<u64>,
    pub(crate) completion_tokens: Option<u64>,
    pub(crate) total_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DirectLlmStatusEnvelope {
    pub(crate) status: u16,
    pub(crate) headers: BTreeMap<String, String>,
}

#[derive(Debug)]
pub(crate) enum OpenAiToolTurnExtraction {
    Final {
        parsed: OpenAiTypedTaskResponse,
        response_json: Value,
        raw_response_hash: String,
    },
    ToolCalls {
        parsed: OpenAiTypedTaskResponse,
        tool_calls: Vec<DirectLlmToolCall>,
    },
}

impl From<OpenAiTypedUsage> for TypedLlmUsage {
    fn from(value: OpenAiTypedUsage) -> Self {
        Self {
            prompt_tokens: value.prompt_tokens,
            completion_tokens: value.completion_tokens,
            total_tokens: value.total_tokens,
            cached_tokens: None,
            cost_micros: None,
        }
    }
}

pub(crate) fn extract_typed_json_object(
    body: &str,
    max_output_bytes: u64,
) -> AppResult<(OpenAiTypedTaskResponse, Value)> {
    let parsed: OpenAiTypedTaskResponse = serde_json::from_str(body).map_err(|error| {
        AppError::unexpected(
            "direct_llm_response_parse_failed",
            format!("failed to parse direct LLM response: {error}"),
            CorrelationId::generate(),
        )
    })?;
    let response_text = parsed
        .choices
        .iter()
        .filter_map(|choice| choice.message.as_ref())
        .filter_map(|message| message.content.as_deref())
        .map(str::trim)
        .find(|content| !content.is_empty())
        .ok_or_else(|| {
            AppError::unexpected(
                "direct_llm_response_empty",
                "direct LLM response did not contain JSON content",
                CorrelationId::generate(),
            )
        })?;
    enforce_max_output_bytes(response_text, max_output_bytes)?;
    let response_json = parse_json_object_text(response_text, "direct LLM response content")?;
    Ok((parsed, response_json))
}

pub(crate) fn parse_json_object_text(response_text: &str, context: &str) -> AppResult<Value> {
    let response_json =
        serde_json::from_str::<Value>(json_text_for_parse(response_text)).map_err(|error| {
            AppError::unexpected(
                "direct_llm_response_parse_failed",
                format!("{context} was not JSON: {error}"),
                CorrelationId::generate(),
            )
        })?;
    if !response_json.is_object() {
        return Err(AppError::invalid_input(
            "direct_llm_schema_mismatch",
            format!("{context} was valid JSON but not a JSON object"),
            CorrelationId::generate(),
        ));
    }
    Ok(response_json)
}

fn json_text_for_parse(response_text: &str) -> &str {
    let trimmed = response_text.trim();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    let Some(fence_end) = rest.find('\n') else {
        return trimmed;
    };
    let language = rest[..fence_end].trim();
    if !language.is_empty() && !language.eq_ignore_ascii_case("json") {
        return trimmed;
    }
    let body = &rest[fence_end + 1..];
    let Some(body) = body.strip_suffix("```") else {
        return trimmed;
    };
    body.trim()
}

pub(crate) fn extract_typed_tool_turn(
    body: &str,
    max_output_bytes: u64,
) -> AppResult<OpenAiToolTurnExtraction> {
    let parsed: OpenAiTypedTaskResponse = serde_json::from_str(body).map_err(|error| {
        AppError::unexpected(
            "direct_llm_response_parse_failed",
            format!("failed to parse direct LLM tool response: {error}"),
            CorrelationId::generate(),
        )
    })?;

    let tool_calls = parsed
        .choices
        .iter()
        .filter_map(|choice| choice.message.as_ref())
        .flat_map(|message| message.tool_calls.iter())
        .map(parse_tool_call)
        .collect::<AppResult<Vec<_>>>()?;
    if !tool_calls.is_empty() {
        return Ok(OpenAiToolTurnExtraction::ToolCalls { parsed, tool_calls });
    }

    let response_text = parsed
        .choices
        .iter()
        .filter_map(|choice| choice.message.as_ref())
        .filter_map(|message| message.content.as_deref())
        .map(str::trim)
        .find(|content| !content.is_empty())
        .ok_or_else(|| {
            AppError::unexpected(
                "direct_llm_response_empty",
                "direct LLM tool response contained neither final JSON content nor tool calls",
                CorrelationId::generate(),
            )
        })?;
    enforce_max_output_bytes(response_text, max_output_bytes)?;
    let response_json = parse_json_object_text(response_text, "direct LLM final response content")?;
    let raw_response_hash = hash_response(response_text.as_bytes());
    Ok(OpenAiToolTurnExtraction::Final {
        parsed,
        response_json,
        raw_response_hash,
    })
}

fn parse_tool_call(raw: &OpenAiRawToolCall) -> AppResult<DirectLlmToolCall> {
    let arguments = if raw.function.arguments.trim().is_empty() {
        Value::Object(Default::default())
    } else {
        serde_json::from_str::<Value>(&raw.function.arguments).map_err(|error| {
            AppError::invalid_input(
                "direct_llm_tool_call_arguments_parse_failed",
                format!("direct LLM tool call arguments were not JSON: {error}"),
                CorrelationId::generate(),
            )
        })?
    };
    if !arguments.is_object() {
        return Err(AppError::invalid_input(
            "direct_llm_tool_call_arguments_not_object",
            "direct LLM tool call arguments must be a JSON object",
            CorrelationId::generate(),
        ));
    }
    Ok(DirectLlmToolCall {
        id: raw.id.clone(),
        name: raw.function.name.clone(),
        arguments,
    })
}

pub(crate) fn map_provider_status_error(response: &DirectLlmStatusEnvelope) -> AppError {
    let retry_after = normalized_retry_after(&response.headers)
        .map(|value| format!(" retry-after={value}"))
        .unwrap_or_default();
    let provider_request_id = preferred_provider_request_id(&response.headers)
        .map(|value| format!(" provider_request_id={value}"))
        .unwrap_or_default();
    let status = response.status;
    let message =
        format!("direct LLM provider returned status {status}{retry_after}{provider_request_id}");
    match status {
        429 => AppError::transient(
            "direct_llm_rate_limited",
            message,
            CorrelationId::generate(),
        ),
        401 => AppError::new(
            ErrorCode::Unauthorized,
            "direct_llm_provider_auth_failed",
            message,
            CorrelationId::generate(),
        ),
        500..=599 => AppError::transient(
            "direct_llm_provider_5xx",
            message,
            CorrelationId::generate(),
        ),
        400..=499 => AppError::policy(
            "direct_llm_provider_rejected",
            message,
            CorrelationId::generate(),
        ),
        _ => AppError::new(
            ErrorCode::ExternalDependency,
            "direct_llm_provider_rejected",
            message,
            CorrelationId::generate(),
        ),
    }
}

pub(crate) fn preferred_provider_request_id(headers: &BTreeMap<String, String>) -> Option<String> {
    const HEADER_CANDIDATES: [&str; 4] = [
        "x-request-id",
        "request-id",
        "x-openai-request-id",
        "openai-request-id",
    ];

    HEADER_CANDIDATES.iter().find_map(|header_name| {
        headers
            .get(*header_name)
            .and_then(|value| normalized_request_id(value))
    })
}

fn normalized_request_id(value: &str) -> Option<String> {
    const MAX_REQUEST_ID_CHARS: usize = 128;

    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.chars().any(|ch| !is_request_id_char(ch)) {
        return None;
    }

    Some(trimmed.chars().take(MAX_REQUEST_ID_CHARS).collect())
}

fn is_request_id_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':' | '/')
}

fn normalized_retry_after(headers: &BTreeMap<String, String>) -> Option<String> {
    let value = headers.get("retry-after")?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    trimmed
        .parse::<u64>()
        .ok()
        .map(|seconds| seconds.to_string())
}

pub(crate) fn enforce_max_input_bytes(input_json: &str, max_input_bytes: u64) -> AppResult<()> {
    let actual = input_json.len() as u64;
    if actual > max_input_bytes {
        return Err(AppError::invalid_input(
            "direct_llm_input_too_large",
            format!("direct LLM input was {actual} bytes; max is {max_input_bytes}"),
            CorrelationId::generate(),
        ));
    }
    Ok(())
}

pub(crate) fn hash_response(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

pub(crate) fn enforce_max_output_bytes(
    response_text: &str,
    max_output_bytes: u64,
) -> AppResult<()> {
    let actual = response_text.len() as u64;
    if actual > max_output_bytes {
        return Err(AppError::invalid_input(
            "direct_llm_response_too_large",
            format!("direct LLM response content was {actual} bytes; max is {max_output_bytes}"),
            CorrelationId::generate(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        extract_typed_tool_turn, map_provider_status_error, preferred_provider_request_id,
        DirectLlmStatusEnvelope, OpenAiToolTurnExtraction,
    };
    use bos_kernel::{AppError, AppResult, CorrelationId};
    use serde_json::json;
    use std::collections::BTreeMap;

    #[test]
    fn preferred_provider_request_id_falls_back_to_safe_header() {
        let headers = BTreeMap::from([
            ("x-request-id".to_string(), "   ".to_string()),
            ("request-id".to_string(), "req-fallback-1".to_string()),
        ]);

        let request_id = preferred_provider_request_id(&headers);
        assert_eq!(request_id.as_deref(), Some("req-fallback-1"));
    }

    #[test]
    fn preferred_provider_request_id_rejects_unsafe_value() {
        let headers = BTreeMap::from([(
            "request-id".to_string(),
            "req-1\nAuthorization: Bearer secret".to_string(),
        )]);

        let request_id = preferred_provider_request_id(&headers);
        assert!(request_id.is_none());
    }

    #[test]
    fn provider_rate_limit_message_includes_safe_retry_after_and_request_id() {
        let response = DirectLlmStatusEnvelope {
            status: 429,
            headers: BTreeMap::from([
                ("retry-after".to_string(), " 15 ".to_string()),
                ("request-id".to_string(), "req-rate-limit".to_string()),
            ]),
        };

        let error = map_provider_status_error(&response);
        assert_eq!(error.code(), "direct_llm_rate_limited");
        assert!(error.message().contains("status 429"));
        assert!(error.message().contains("retry-after=15"));
        assert!(error
            .message()
            .contains("provider_request_id=req-rate-limit"));
    }

    #[test]
    fn provider_rate_limit_message_omits_invalid_retry_after() {
        let response = DirectLlmStatusEnvelope {
            status: 429,
            headers: BTreeMap::from([(
                "retry-after".to_string(),
                "Mon, 18 May 2026 12:00:00 GMT".to_string(),
            )]),
        };

        let error = map_provider_status_error(&response);
        assert_eq!(error.code(), "direct_llm_rate_limited");
        assert!(!error.message().contains("retry-after="));
    }

    #[test]
    fn extract_typed_tool_turn_parses_tool_calls() -> AppResult<()> {
        let body = json!({
            "choices": [{
                "message": {
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "lookup",
                            "arguments": "{\"query\":\"Acme\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3}
        })
        .to_string();

        let parsed = extract_typed_tool_turn(&body, 1024)?;

        match parsed {
            OpenAiToolTurnExtraction::ToolCalls { tool_calls, .. } => {
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].id, "call_1");
                assert_eq!(tool_calls[0].name, "lookup");
                assert_eq!(tool_calls[0].arguments, json!({"query": "Acme"}));
            }
            OpenAiToolTurnExtraction::Final { .. } => {
                return Err(AppError::unexpected(
                    "test_expected_tool_calls",
                    "expected tool calls",
                    CorrelationId::generate(),
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn extract_typed_tool_turn_parses_final_json() -> AppResult<()> {
        let body = json!({
            "choices": [{
                "message": {
                    "content": "{\"schema_version\":\"v1\",\"ok\":true}"
                },
                "finish_reason": "stop"
            }]
        })
        .to_string();

        let parsed = extract_typed_tool_turn(&body, 1024)?;

        match parsed {
            OpenAiToolTurnExtraction::Final { response_json, .. } => {
                assert_eq!(response_json, json!({"schema_version": "v1", "ok": true}));
            }
            OpenAiToolTurnExtraction::ToolCalls { .. } => {
                return Err(AppError::unexpected(
                    "test_expected_final_json",
                    "expected final JSON",
                    CorrelationId::generate(),
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn extract_typed_tool_turn_rejects_empty_response() {
        let body = json!({"choices": [{"message": {}}]}).to_string();

        let error = extract_typed_tool_turn(&body, 1024).expect_err("empty turn should fail");

        assert_eq!(error.code(), "direct_llm_response_empty");
    }
}
