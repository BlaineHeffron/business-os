//! OpenAI-compatible request body construction for typed tasks and tool turns.
//! Ported verbatim from agent-monitor-rust `direct_llm_payload.rs`.

use crate::llm_api::{DirectLlmToolCall, DirectLlmToolDefinition, DirectLlmToolResult};
use crate::llm_typed_tasks::TypedLlmTaskRequest;
use bos_kernel::{AppError, AppResult, CorrelationId};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

const DEFAULT_TEMPERATURE: f32 = 0.0;

#[derive(Debug, Serialize)]
pub(crate) struct OpenAiTypedTaskRequestBody {
    pub(crate) model: String,
    pub(crate) max_tokens: u32,
    pub(crate) temperature: f32,
    pub(crate) response_format: OpenAiResponseFormat,
    pub(crate) messages: Vec<OpenAiDirectMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) tools: Vec<OpenAiToolDefinition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tool_choice: Option<&'static str>,
}

#[derive(Debug, Serialize)]
pub(crate) struct OpenAiResponseFormat {
    #[serde(rename = "type")]
    pub(crate) kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) json_schema: Option<OpenAiJsonSchemaSpec>,
}

#[derive(Debug, Serialize)]
pub(crate) struct OpenAiJsonSchemaSpec {
    pub(crate) name: String,
    pub(crate) schema: Value,
}

#[derive(Debug, Serialize)]
pub(crate) struct OpenAiDirectMessage {
    pub(crate) role: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) tool_calls: Vec<OpenAiOutboundToolCall>,
}

#[derive(Debug, Serialize)]
pub(crate) struct OpenAiToolDefinition {
    #[serde(rename = "type")]
    pub(crate) kind: &'static str,
    pub(crate) function: OpenAiToolFunction,
}

#[derive(Debug, Serialize)]
pub(crate) struct OpenAiToolFunction {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) parameters: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub(crate) struct OpenAiOutboundToolCall {
    pub(crate) id: String,
    #[serde(rename = "type")]
    pub(crate) kind: &'static str,
    pub(crate) function: OpenAiOutboundToolFunction,
}

#[derive(Debug, Serialize)]
pub(crate) struct OpenAiOutboundToolFunction {
    pub(crate) name: String,
    pub(crate) arguments: String,
}

pub(crate) fn build_openai_compatible_request_body(
    request: &TypedLlmTaskRequest,
    model: &str,
    input_json: &str,
    schema: Option<&Value>,
    schema_response_format: bool,
) -> AppResult<String> {
    encode_openai_body(OpenAiTypedTaskRequestBody {
        model: model.to_string(),
        max_tokens: request.spec.max_tokens,
        temperature: DEFAULT_TEMPERATURE,
        response_format: openai_response_format(request, schema, schema_response_format),
        messages: vec![
            OpenAiDirectMessage {
                role: "system",
                content: Some(typed_task_system_prompt(request, false, schema)),
                tool_call_id: None,
                tool_calls: Vec::new(),
            },
            OpenAiDirectMessage {
                role: "user",
                content: Some(input_json.to_string()),
                tool_call_id: None,
                tool_calls: Vec::new(),
            },
        ],
        tools: Vec::new(),
        tool_choice: None,
    })
}

pub(crate) fn build_openai_compatible_tool_turn_request_body(
    request: &TypedLlmTaskRequest,
    model: &str,
    input_json: &str,
    tools: &[DirectLlmToolDefinition],
    prior_tool_turns: &[crate::llm_api::DirectLlmToolTurn],
    schema: Option<&Value>,
    schema_response_format: bool,
) -> AppResult<String> {
    let mut messages = vec![
        OpenAiDirectMessage {
            role: "system",
            content: Some(typed_task_system_prompt(request, !tools.is_empty(), schema)),
            tool_call_id: None,
            tool_calls: Vec::new(),
        },
        OpenAiDirectMessage {
            role: "user",
            content: Some(input_json.to_string()),
            tool_call_id: None,
            tool_calls: Vec::new(),
        },
    ];
    for turn in prior_tool_turns {
        messages.push(OpenAiDirectMessage {
            role: "assistant",
            content: None,
            tool_call_id: None,
            tool_calls: turn
                .tool_calls
                .iter()
                .map(openai_outbound_tool_call)
                .collect::<AppResult<Vec<_>>>()?,
        });
        for result in &turn.tool_results {
            messages.push(tool_result_message(result)?);
        }
    }
    let tools = tools
        .iter()
        .map(|tool| OpenAiToolDefinition {
            kind: "function",
            function: OpenAiToolFunction {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: tool.parameters_schema.clone(),
            },
        })
        .collect::<Vec<_>>();
    encode_openai_body(OpenAiTypedTaskRequestBody {
        model: model.to_string(),
        max_tokens: request.spec.max_tokens,
        temperature: DEFAULT_TEMPERATURE,
        response_format: openai_response_format(request, schema, schema_response_format),
        messages,
        tool_choice: (!tools.is_empty()).then_some("auto"),
        tools,
    })
}

fn encode_openai_body(body: OpenAiTypedTaskRequestBody) -> AppResult<String> {
    serde_json::to_string(&body).map_err(|error| {
        AppError::unexpected(
            "direct_llm_request_encode_failed",
            format!("failed to encode direct LLM request: {error}"),
            CorrelationId::generate(),
        )
    })
}

fn openai_response_format(
    request: &TypedLlmTaskRequest,
    schema: Option<&Value>,
    schema_response_format: bool,
) -> OpenAiResponseFormat {
    match (schema_response_format, schema) {
        (true, Some(schema)) => OpenAiResponseFormat {
            kind: "json_schema",
            json_schema: Some(OpenAiJsonSchemaSpec {
                name: json_schema_name(&request.spec.schema_ref),
                schema: schema.clone(),
            }),
        },
        _ => OpenAiResponseFormat {
            kind: "json_object",
            json_schema: None,
        },
    }
}

pub(crate) fn json_schema_name(schema_ref: &str) -> String {
    let mut name = String::with_capacity(schema_ref.len());
    for ch in schema_ref.chars() {
        if ch.is_ascii_alphanumeric() {
            name.push(ch);
        } else {
            name.push('_');
        }
    }
    if name.is_empty() {
        "typed_output".to_string()
    } else {
        name.truncate(64);
        name
    }
}

fn openai_outbound_tool_call(call: &DirectLlmToolCall) -> AppResult<OpenAiOutboundToolCall> {
    Ok(OpenAiOutboundToolCall {
        id: call.id.clone(),
        kind: "function",
        function: OpenAiOutboundToolFunction {
            name: call.name.clone(),
            arguments: serde_json::to_string(&call.arguments).map_err(|error| {
                AppError::unexpected(
                    "direct_llm_tool_arguments_encode_failed",
                    format!("failed to encode direct LLM prior tool arguments: {error}"),
                    CorrelationId::generate(),
                )
            })?,
        },
    })
}

fn tool_result_message(result: &DirectLlmToolResult) -> AppResult<OpenAiDirectMessage> {
    Ok(OpenAiDirectMessage {
        role: "tool",
        content: Some(serde_json::to_string(&result.result_json).map_err(|error| {
            AppError::unexpected(
                "direct_llm_tool_result_encode_failed",
                format!("failed to encode direct LLM tool result: {error}"),
                CorrelationId::generate(),
            )
        })?),
        tool_call_id: Some(result.call_id.clone()),
        tool_calls: Vec::new(),
    })
}

pub(crate) fn openai_compatible_headers(api_key: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            "authorization".to_string(),
            format!("Bearer {}", api_key.trim()),
        ),
        ("content-type".to_string(), "application/json".to_string()),
    ])
}

pub(crate) fn typed_task_system_prompt(
    request: &TypedLlmTaskRequest,
    tools_enabled: bool,
    schema: Option<&Value>,
) -> String {
    let tool_policy = if tools_enabled {
        "You may call only the supplied read-only tools when needed. Tool calls must not mutate state, perform provider writes, browse outside the supplied tools, access the filesystem, or change workflow route."
    } else {
        "Side effects, provider writes, browsing, tools, filesystem access, and route changes are forbidden."
    };
    let schema_block = match schema {
        Some(schema) => match serde_json::to_string_pretty(schema) {
            Ok(rendered) => format!(
                "Output JSON only for schema_ref={}.\nJSON schema:\n{rendered}",
                request.spec.schema_ref
            ),
            Err(_) => format!(
                "Output JSON only for schema_ref={}.",
                request.spec.schema_ref
            ),
        },
        None => format!(
            "Output JSON only for schema_ref={}.",
            request.spec.schema_ref
        ),
    };
    format!(
        "You perform one bounded typed transformation.\n\
         {schema_block}\n\
         Prompt template id={} version={} hash={}.\n\
         {tool_policy}",
        request.spec.prompt_template_id,
        request.spec.prompt_template_version,
        request.spec.prompt_template_hash
    )
}
