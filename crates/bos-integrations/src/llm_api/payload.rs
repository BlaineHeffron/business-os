//! OpenAI-compatible request body construction for typed tasks and tool turns.
//! Ported verbatim from agent-monitor-rust `direct_llm_payload.rs`.

use crate::llm_api::{DirectLlmToolCall, DirectLlmToolDefinition, DirectLlmToolResult};
use crate::llm_typed_tasks::TypedLlmTaskRequest;
use bos_kernel::{AppError, AppResult, CorrelationId};
use serde::Serialize;
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
    input_json: String,
) -> AppResult<String> {
    serde_json::to_string(&OpenAiTypedTaskRequestBody {
        model: model.to_string(),
        max_tokens: request.spec.max_tokens,
        temperature: DEFAULT_TEMPERATURE,
        response_format: OpenAiResponseFormat {
            kind: "json_object",
        },
        messages: vec![
            OpenAiDirectMessage {
                role: "system",
                content: Some(typed_task_system_prompt(request, false)),
                tool_call_id: None,
                tool_calls: Vec::new(),
            },
            OpenAiDirectMessage {
                role: "user",
                content: Some(input_json),
                tool_call_id: None,
                tool_calls: Vec::new(),
            },
        ],
        tools: Vec::new(),
        tool_choice: None,
    })
    .map_err(|error| {
        AppError::unexpected(
            "direct_llm_request_encode_failed",
            format!("failed to encode direct LLM request: {error}"),
            CorrelationId::generate(),
        )
    })
}

pub(crate) fn build_openai_compatible_tool_turn_request_body(
    request: &TypedLlmTaskRequest,
    model: &str,
    input_json: String,
    tools: &[DirectLlmToolDefinition],
    prior_tool_turns: &[crate::llm_api::DirectLlmToolTurn],
) -> AppResult<String> {
    let mut messages = vec![
        OpenAiDirectMessage {
            role: "system",
            content: Some(typed_task_system_prompt(request, !tools.is_empty())),
            tool_call_id: None,
            tool_calls: Vec::new(),
        },
        OpenAiDirectMessage {
            role: "user",
            content: Some(input_json),
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
    serde_json::to_string(&OpenAiTypedTaskRequestBody {
        model: model.to_string(),
        max_tokens: request.spec.max_tokens,
        temperature: DEFAULT_TEMPERATURE,
        response_format: OpenAiResponseFormat {
            kind: "json_object",
        },
        messages,
        tool_choice: (!tools.is_empty()).then_some("auto"),
        tools,
    })
    .map_err(|error| {
        AppError::unexpected(
            "direct_llm_request_encode_failed",
            format!("failed to encode direct LLM tool request: {error}"),
            CorrelationId::generate(),
        )
    })
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

fn typed_task_system_prompt(request: &TypedLlmTaskRequest, tools_enabled: bool) -> String {
    let tool_policy = if tools_enabled {
        "You may call only the supplied read-only tools when needed. Tool calls must not mutate state, perform provider writes, browse outside the supplied tools, access the filesystem, or change workflow route."
    } else {
        "Side effects, provider writes, browsing, tools, filesystem access, and route changes are forbidden."
    };
    format!(
        "You perform one bounded typed transformation.\n\
         Output JSON only for schema_ref={}.\n\
         Prompt template id={} version={} hash={}.\n\
         {tool_policy}",
        request.spec.schema_ref,
        request.spec.prompt_template_id,
        request.spec.prompt_template_version,
        request.spec.prompt_template_hash
    )
}
