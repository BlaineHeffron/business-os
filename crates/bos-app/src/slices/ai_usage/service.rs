//! The recording seam for typed LLM executions: every call in the app goes
//! through [`execute_recorded`], which persists one usage row per API call
//! and one per harness attempt (via the kernel's [`AiCallUsageSink`]).
//!
//! Recording is best-effort: a failed usage insert is logged, never allowed
//! to fail the LLM call it accounts for.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use bos_contracts::ai_usage::AiUsageRow;
use bos_contracts::llm_settings::{
    ClaudeSubscriptionAuthStartResponse, ClaudeSubscriptionStatus, LlmGlobalRouteSettings,
    LlmPurposeRouteOverrideUpdate, LlmPurposeRouteSettings, LlmRouteSettingsResponse,
    LlmRouteSettingsUpdateRequest,
};
use bos_integrations::llm_api::{
    DirectLlmClient, DirectLlmToolCall, DirectLlmToolDefinition, DirectLlmToolResult,
    DirectLlmToolTurn, DirectLlmToolTurnRequest, DirectLlmToolTurnResponse,
    OpenAiCompatibleDirectLlmClient, OpenAiCompatibleDirectLlmConfig,
};
use bos_integrations::llm_typed_tasks::{
    sanitize_typed_task_request, scrub_json_in_place, TypedLlmExecutionRoute,
    TypedLlmTaskOutputEnvelope, TypedLlmTaskRequest, TypedLlmUsage,
};
use bos_kernel::{AiCallUsageRecord, AiCallUsageSink, AppError, AppResult, CorrelationId};
use parking_lot::Mutex;
use serde::Deserialize;

use super::store::{self, UsageInsert};
use crate::http::now_ms;
use crate::llm::{self, LlmBackend};
use crate::persistence::PersistencePool;
use crate::store_core::{MutationOutcome, StoreError};

/// Distinguishes API rows recorded in the same millisecond.
static API_USAGE_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static CLAUDE_AUTH_FLOW: OnceLock<Mutex<Option<ClaudeAuthFlow>>> = OnceLock::new();

pub const TOOL_LOOP_UNAVAILABLE_CODE: &str = "llm_tool_loop_unavailable";
pub const TOOL_LOOP_EXHAUSTED_CODE: &str = "llm_tool_loop_exhausted";
const CLAUDE_AUTH_FLOW_TTL_MS: u64 = 15 * 60 * 1_000;
const CLAUDE_AUTH_START_TIMEOUT: Duration = Duration::from_secs(10);
const CLAUDE_AUTH_STATUS_TIMEOUT: Duration = Duration::from_secs(5);
const CLAUDE_AUTH_STATUS_MAX_BYTES: u64 = 16_384;
const CLAUDE_AUTH_URL_MAX_BYTES: usize = 8_192;
const CLAUDE_AUTH_CODE_MAX_BYTES: usize = 8_192;

struct ClaudeAuthFlow {
    flow_id: String,
    actor_id: String,
    authorization_url: String,
    started_at_ms: u64,
    child: Child,
    stdin: Option<ChildStdin>,
    // Keep the read end open until the CLI exits. Claude prints a final
    // success line after consuming the code; dropping this pipe early can
    // turn that harmless print into SIGPIPE before credentials are flushed.
    _stdout: ChildStdout,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeAuthStatusOutput {
    #[serde(default)]
    logged_in: bool,
    auth_method: Option<String>,
    subscription_type: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolLoopLimits {
    pub max_turns: u32,
    pub max_tool_calls: u32,
    pub max_evidence_bytes: usize,
    pub wall_clock_ms: u64,
}

pub struct ToolLoopRecordedRequest<'a> {
    pub persistence: PersistencePool,
    pub client_id: &'a str,
    pub purpose: &'a str,
    pub request: &'a TypedLlmTaskRequest,
    pub tools: &'a [DirectLlmToolDefinition],
    pub limits: ToolLoopLimits,
}

/// Execute one typed LLM task with usage persisted. The harness transport
/// reports per-attempt usage through the sink; the API transport reports on
/// the output envelope (and errors produce a failure row here).
pub fn execute_recorded(
    persistence: PersistencePool,
    client_id: &str,
    purpose: &str,
    request: &TypedLlmTaskRequest,
) -> AppResult<TypedLlmTaskOutputEnvelope> {
    let config = {
        let persistence_guard = persistence.lock();
        effective_config(persistence_guard.connection_ref(), client_id).map_err(|err| {
            AppError::unexpected(
                "llm_config_read_failed",
                format!("read LLM route config: {err}"),
                CorrelationId::generate(),
            )
        })?
    };
    execute_recorded_with_config(persistence, client_id, purpose, request, &config)
}

pub fn execute_tool_loop_recorded(
    persistence: PersistencePool,
    client_id: &str,
    purpose: &str,
    request: &TypedLlmTaskRequest,
    tools: &[DirectLlmToolDefinition],
    limits: ToolLoopLimits,
    execute_tool: impl FnMut(u32, &DirectLlmToolCall) -> AppResult<DirectLlmToolResult>,
) -> AppResult<TypedLlmTaskOutputEnvelope> {
    let config = {
        let persistence_guard = persistence.lock();
        effective_config(persistence_guard.connection_ref(), client_id).map_err(|err| {
            AppError::unexpected(
                "llm_config_read_failed",
                format!("read LLM route config: {err}"),
                CorrelationId::generate(),
            )
        })?
    };
    let route = llm::route_config_for_purpose(&config, purpose);
    if route.backend != LlmBackend::Api {
        return Err(tool_loop_unavailable(
            "tool-loop typed tasks require the direct API backend",
        ));
    }
    let Some(api_key) = config.api_key.as_deref() else {
        return Err(tool_loop_unavailable(
            "tool-loop typed tasks require BOS_LLM_API_KEY",
        ));
    };
    let Some(model) = route
        .model
        .as_deref()
        .filter(|model| !model.trim().is_empty())
    else {
        return Err(tool_loop_unavailable(
            "tool-loop typed tasks require an API model",
        ));
    };
    let provider_id = match config.api_provider {
        llm::LlmApiProvider::OpenAi => "openai",
        llm::LlmApiProvider::OpenRouter => "openrouter",
        llm::LlmApiProvider::Anthropic => {
            return Err(tool_loop_unavailable(
                "configured direct provider does not support tool turns",
            ));
        }
    };
    let endpoint = config
        .api_endpoint
        .clone()
        .unwrap_or_else(|| config.api_provider.default_endpoint().to_string());
    let client = OpenAiCompatibleDirectLlmClient::new(OpenAiCompatibleDirectLlmConfig {
        provider_id: provider_id.to_string(),
        api_key: api_key.to_string(),
        model: model.to_string(),
        endpoint,
        timeout_ms: config.timeout_ms,
    })?;
    let mut routed_request = request.clone();
    routed_request.execution_policy.default_route = TypedLlmExecutionRoute::DirectApi;
    if routed_request.spec.max_tokens == 0 {
        routed_request.spec.max_tokens = config.max_tokens;
    }
    if routed_request.spec.timeout_ms == 0 {
        routed_request.spec.timeout_ms = config.timeout_ms;
    }
    routed_request.provider_policy.preferred_model = model.to_string();
    execute_tool_loop_recorded_with_client(
        ToolLoopRecordedRequest {
            persistence,
            client_id,
            purpose,
            request: &routed_request,
            tools,
            limits,
        },
        &client,
        execute_tool,
    )
}

pub fn execute_tool_loop_recorded_with_client(
    context: ToolLoopRecordedRequest<'_>,
    client: &dyn DirectLlmClient,
    mut execute_tool: impl FnMut(u32, &DirectLlmToolCall) -> AppResult<DirectLlmToolResult>,
) -> AppResult<TypedLlmTaskOutputEnvelope> {
    let ToolLoopRecordedRequest {
        persistence,
        client_id,
        purpose,
        request,
        tools,
        limits,
    } = context;
    let mut request = sanitize_typed_task_request(request);
    request.execution_policy.default_route = TypedLlmExecutionRoute::DirectApi;
    let started = Instant::now();
    let mut prior_tool_turns = Vec::new();
    let mut turn_index = 0_u32;
    let mut tool_call_count = 0_u32;
    let mut evidence_bytes = 0_usize;
    loop {
        if turn_index >= limits.max_turns
            || started.elapsed().as_millis() as u64 > limits.wall_clock_ms
        {
            let error = tool_loop_exhausted();
            persist_tool_loop_failure(
                &persistence,
                client_id,
                purpose,
                &request,
                &error,
                started.elapsed().as_millis() as u64,
            );
            return Err(error);
        }
        let turn_request = DirectLlmToolTurnRequest {
            tools: tools.to_vec(),
            prior_tool_turns: prior_tool_turns.clone(),
        };
        let response = match client.complete_typed_task_turn(&request, &turn_request) {
            Ok(response) => response,
            Err(error) => {
                persist_tool_loop_failure(
                    &persistence,
                    client_id,
                    purpose,
                    &request,
                    &error,
                    started.elapsed().as_millis() as u64,
                );
                return Err(error);
            }
        };
        match response {
            DirectLlmToolTurnResponse::Final(envelope) => {
                if let Err(error) = llm::validate_typed_task_output(
                    &request.spec.schema_ref,
                    &envelope.response_json,
                ) {
                    let insert = failure_insert_from_envelope(purpose, &request, &envelope, &error);
                    persist(&persistence, client_id, &insert);
                    return Err(error);
                }
                let insert = insert_from_envelope(purpose, &envelope);
                persist(&persistence, client_id, &insert);
                return Ok(envelope);
            }
            DirectLlmToolTurnResponse::ToolCalls {
                provider_id,
                model,
                tool_calls,
                usage,
                finish_reason: _,
                latency_ms,
                provider_request_id,
            } => {
                let insert = insert_from_tool_turn(
                    purpose,
                    &request,
                    &provider_id,
                    &model,
                    usage,
                    latency_ms,
                    provider_request_id,
                );
                persist(&persistence, client_id, &insert);
                if tool_calls.is_empty() {
                    let error = tool_loop_exhausted();
                    persist_tool_loop_failure(
                        &persistence,
                        client_id,
                        purpose,
                        &request,
                        &error,
                        started.elapsed().as_millis() as u64,
                    );
                    return Err(error);
                }
                let mut tool_results = Vec::new();
                for call in &tool_calls {
                    tool_call_count = tool_call_count.saturating_add(1);
                    if tool_call_count > limits.max_tool_calls {
                        let error = tool_loop_exhausted();
                        persist_tool_loop_failure(
                            &persistence,
                            client_id,
                            purpose,
                            &request,
                            &error,
                            started.elapsed().as_millis() as u64,
                        );
                        return Err(error);
                    }
                    let mut result = match execute_tool(turn_index, call) {
                        Ok(result) => result,
                        Err(error) => {
                            persist_tool_loop_failure(
                                &persistence,
                                client_id,
                                purpose,
                                &request,
                                &error,
                                started.elapsed().as_millis() as u64,
                            );
                            return Err(error);
                        }
                    };
                    scrub_json_in_place(&mut result.result_json);
                    evidence_bytes = evidence_bytes.saturating_add(
                        serde_json::to_vec(&result.result_json)
                            .map(|bytes| bytes.len())
                            .unwrap_or(usize::MAX),
                    );
                    if evidence_bytes > limits.max_evidence_bytes {
                        let error = tool_loop_exhausted();
                        persist_tool_loop_failure(
                            &persistence,
                            client_id,
                            purpose,
                            &request,
                            &error,
                            started.elapsed().as_millis() as u64,
                        );
                        return Err(error);
                    }
                    tool_results.push(result);
                }
                prior_tool_turns.push(DirectLlmToolTurn {
                    tool_calls,
                    tool_results,
                });
                turn_index = turn_index.saturating_add(1);
            }
        }
    }
}

pub fn settings_response(
    conn: &rusqlite::Connection,
    client_id: &str,
) -> Result<LlmRouteSettingsResponse, crate::store_core::StoreError> {
    let config = effective_config(conn, client_id)?;
    let persisted = store::get_llm_route_settings(conn, client_id)?;
    let persisted_overrides = persisted
        .as_ref()
        .map(|settings| settings.overrides.as_slice())
        .unwrap_or(&[]);
    let global_source = if persisted.is_some() { "stored" } else { "env" };
    Ok(LlmRouteSettingsResponse {
        revision: persisted
            .as_ref()
            .and_then(|settings| settings.revision)
            .or_else(|| {
                store::current_revision(
                    conn,
                    client_id,
                    store::LLM_SETTINGS_ENTITY_KIND,
                    store::LLM_SETTINGS_ENTITY_ID,
                )
                .ok()
                .flatten()
            }),
        api_provider: config.api_provider.as_str().to_string(),
        harness_available: config.harness_enabled,
        global: LlmGlobalRouteSettings {
            backend: config.default_backend.as_str().to_string(),
            model: config.default_model.clone(),
            max_tokens: config.max_tokens,
            timeout_ms: config.timeout_ms,
            source: global_source.to_string(),
        },
        purposes: known_purposes()
            .iter()
            .map(|purpose| purpose_settings(&config, purpose, persisted_overrides))
            .collect(),
    })
}

pub fn replace_llm_route_settings(
    conn: &mut rusqlite::Connection,
    client_id: &str,
    actor_id: &str,
    request: &LlmRouteSettingsUpdateRequest,
    now_ms: u64,
) -> Result<MutationOutcome, StoreError> {
    store::replace_llm_route_settings(conn, client_id, actor_id, request, is_known_purpose, now_ms)
}

pub fn claude_subscription_status(config: &llm::LlmRuntimeConfig) -> ClaudeSubscriptionStatus {
    let authorization_pending = {
        let mut slot = claude_auth_flow().lock();
        cleanup_claude_auth_flow(&mut slot, now_ms());
        slot.is_some()
    };
    if !config.harness_enabled {
        return ClaudeSubscriptionStatus {
            available: false,
            connected: false,
            auth_method: None,
            subscription_type: None,
            authorization_pending,
        };
    }

    let stdout = match run_claude_auth_status(&config.harness_program) {
        Some(stdout) => stdout,
        None => {
            return ClaudeSubscriptionStatus {
                available: false,
                connected: false,
                auth_method: None,
                subscription_type: None,
                authorization_pending,
            };
        }
    };
    let parsed = serde_json::from_slice::<ClaudeAuthStatusOutput>(&stdout).ok();
    let connected = parsed.as_ref().is_some_and(|status| {
        status.logged_in && status.auth_method.as_deref() == Some("claude.ai")
    });
    ClaudeSubscriptionStatus {
        available: true,
        connected,
        auth_method: parsed
            .as_ref()
            .and_then(|status| status.auth_method.clone()),
        subscription_type: parsed.and_then(|status| status.subscription_type),
        authorization_pending,
    }
}

fn run_claude_auth_status(program: &str) -> Option<Vec<u8>> {
    run_claude_auth_status_with_timeout(program, CLAUDE_AUTH_STATUS_TIMEOUT)
}

pub(super) fn run_claude_auth_status_with_timeout(
    program: &str,
    timeout: Duration,
) -> Option<Vec<u8>> {
    let mut child = Command::new(program)
        .args(["auth", "status", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .take(CLAUDE_AUTH_STATUS_MAX_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
            Ok(None) | Err(_) => {
                terminate_auth_child(&mut child);
                let _ = reader.join();
                return None;
            }
        }
    }
    let bytes = reader.join().ok()?.ok()?;
    (bytes.len() <= CLAUDE_AUTH_STATUS_MAX_BYTES as usize).then_some(bytes)
}

pub fn start_claude_subscription_auth(
    config: &llm::LlmRuntimeConfig,
    actor_id: &str,
    now_ms: u64,
) -> Result<ClaudeSubscriptionAuthStartResponse, &'static str> {
    if !config.harness_enabled {
        return Err("llm_harness_unavailable");
    }

    let mut slot = claude_auth_flow().lock();
    cleanup_claude_auth_flow(&mut slot, now_ms);
    if let Some(flow) = slot.as_ref() {
        if flow.actor_id != actor_id {
            return Err("llm_subscription_auth_in_progress");
        }
        return Ok(ClaudeSubscriptionAuthStartResponse {
            flow_id: flow.flow_id.clone(),
            authorization_url: flow.authorization_url.clone(),
        });
    }

    let mut child = Command::new(&config.harness_program)
        .args(["auth", "login", "--claudeai"])
        .env("BROWSER", "/bin/true")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "llm_harness_program_not_found")?;
    let stdin = child
        .stdin
        .take()
        .ok_or("llm_subscription_auth_start_failed")?;
    let stdout = child
        .stdout
        .take()
        .ok_or("llm_subscription_auth_start_failed")?;
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut total_bytes = 0usize;
        let result = loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => break Err("llm_subscription_auth_start_failed"),
                Ok(bytes) => {
                    total_bytes = total_bytes.saturating_add(bytes);
                    if total_bytes > CLAUDE_AUTH_URL_MAX_BYTES * 2 {
                        break Err("llm_subscription_auth_start_failed");
                    }
                    if let Some(url) = extract_claude_authorization_url(&line) {
                        break Ok((url, reader.into_inner()));
                    }
                }
                Err(_) => break Err("llm_subscription_auth_start_failed"),
            }
        };
        let _ = sender.send(result);
    });

    let (authorization_url, stdout) = match receiver.recv_timeout(CLAUDE_AUTH_START_TIMEOUT) {
        Ok(Ok(result)) => result,
        Ok(Err(code)) => {
            terminate_auth_child(&mut child);
            return Err(code);
        }
        Err(_) => {
            terminate_auth_child(&mut child);
            return Err("llm_subscription_auth_start_timeout");
        }
    };
    let flow_id = format!("claude_auth_{}", CorrelationId::generate());
    *slot = Some(ClaudeAuthFlow {
        flow_id: flow_id.clone(),
        actor_id: actor_id.to_string(),
        authorization_url: authorization_url.clone(),
        started_at_ms: now_ms,
        child,
        stdin: Some(stdin),
        _stdout: stdout,
    });
    Ok(ClaudeSubscriptionAuthStartResponse {
        flow_id,
        authorization_url,
    })
}

pub fn submit_claude_subscription_code(
    flow_id: &str,
    actor_id: &str,
    authorization_code: &str,
    now_ms: u64,
) -> Result<(), &'static str> {
    let code = validate_claude_authorization_code(authorization_code)?;
    let mut slot = claude_auth_flow().lock();
    cleanup_claude_auth_flow(&mut slot, now_ms);
    let flow = slot
        .as_mut()
        .ok_or("llm_subscription_auth_flow_not_found")?;
    if flow.flow_id != flow_id.trim() || flow.actor_id != actor_id {
        return Err("llm_subscription_auth_flow_not_found");
    }
    let mut stdin = flow
        .stdin
        .take()
        .ok_or("llm_subscription_auth_code_already_submitted")?;
    if writeln!(stdin, "{code}")
        .and_then(|_| stdin.flush())
        .is_err()
    {
        terminate_auth_child(&mut flow.child);
        *slot = None;
        return Err("llm_subscription_auth_submit_failed");
    }
    Ok(())
}

pub fn cancel_claude_subscription_auth(flow_id: &str) {
    let mut slot = claude_auth_flow().lock();
    if slot
        .as_ref()
        .is_some_and(|flow| flow.flow_id == flow_id.trim())
    {
        if let Some(flow) = slot.as_mut() {
            terminate_auth_child(&mut flow.child);
        }
        *slot = None;
    }
}

fn claude_auth_flow() -> &'static Mutex<Option<ClaudeAuthFlow>> {
    CLAUDE_AUTH_FLOW.get_or_init(|| Mutex::new(None))
}

fn cleanup_claude_auth_flow(slot: &mut Option<ClaudeAuthFlow>, now_ms: u64) {
    let should_clear = slot.as_mut().is_some_and(|flow| {
        let expired = now_ms.saturating_sub(flow.started_at_ms) > CLAUDE_AUTH_FLOW_TTL_MS;
        let exited = flow.child.try_wait().ok().flatten().is_some();
        if expired && !exited {
            terminate_auth_child(&mut flow.child);
        }
        expired || exited
    });
    if should_clear {
        *slot = None;
    }
}

fn terminate_auth_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

pub(super) fn extract_claude_authorization_url(line: &str) -> Option<String> {
    let start = line.find("https://claude.com/")?;
    let candidate = line[start..].split_whitespace().next()?.trim();
    if candidate.len() > CLAUDE_AUTH_URL_MAX_BYTES {
        return None;
    }
    let parsed = url::Url::parse(candidate).ok()?;
    if parsed.scheme() != "https" || parsed.host_str() != Some("claude.com") {
        return None;
    }
    Some(candidate.to_string())
}

pub(super) fn validate_claude_authorization_code(raw: &str) -> Result<&str, &'static str> {
    let code = raw.trim();
    if code.is_empty()
        || code.len() > CLAUDE_AUTH_CODE_MAX_BYTES
        || code.chars().any(char::is_control)
    {
        return Err("llm_subscription_authorization_code_invalid");
    }
    Ok(code)
}

pub fn effective_config(
    conn: &rusqlite::Connection,
    client_id: &str,
) -> Result<llm::LlmRuntimeConfig, crate::store_core::StoreError> {
    let mut config = llm::config_from_env();
    if let Some(settings) = store::get_llm_route_settings(conn, client_id)? {
        apply_settings_to_config(&mut config, &settings);
    }
    Ok(config)
}

fn apply_settings_to_config(
    config: &mut llm::LlmRuntimeConfig,
    settings: &store::StoredLlmRouteSettings,
) {
    if let Some(backend) = llm::parse_backend_choice(&settings.global.backend) {
        let route = available_route(config, backend, settings.global.model.clone());
        config.default_backend = route.backend;
        config.default_model = route.model;
    } else {
        config.default_model = settings.global.model.clone();
    }
    config.max_tokens = settings.global.max_tokens;
    config.timeout_ms = settings.global.timeout_ms;
    // Persisted choices override matching env routes, but do not erase an
    // env-pinned purpose that the settings row does not mention (notably a
    // deployment-pinned loopback local route).
    for override_config in &settings.overrides {
        if !is_known_purpose(&override_config.purpose) {
            tracing::warn!(
                purpose = override_config.purpose,
                "ignoring unknown persisted LLM route override"
            );
            continue;
        }
        if let Some(backend) = llm::parse_backend_choice(&override_config.backend) {
            let route = available_route(config, backend, override_config.model.clone());
            config.route_overrides.insert(
                override_config.purpose.clone(),
                llm::LlmRouteOverride {
                    backend: route.backend,
                    model: route.model,
                },
            );
        }
    }
}

struct AvailableRoute {
    backend: llm::LlmBackend,
    model: Option<String>,
}

fn available_route(
    config: &llm::LlmRuntimeConfig,
    backend: llm::LlmBackend,
    model: Option<String>,
) -> AvailableRoute {
    if backend == llm::LlmBackend::Harness && !config.harness_enabled {
        return AvailableRoute {
            backend: llm::LlmBackend::Api,
            model: None,
        };
    }
    AvailableRoute { backend, model }
}

#[derive(Debug, Clone, Copy)]
struct LlmPurposeDescriptor {
    purpose: &'static str,
    label: &'static str,
    description: &'static str,
}

fn known_purposes() -> &'static [LlmPurposeDescriptor] {
    &[
        LlmPurposeDescriptor {
            purpose: crate::slices::email_triage::service::AI_TRIAGE_PURPOSE,
            label: "Email AI triage",
            description: "Suggests work-queue actions for classified inbound email.",
        },
        LlmPurposeDescriptor {
            purpose: crate::slices::packet_proposals::service::PROPOSAL_PURPOSE,
            label: "Packet proposals",
            description:
                "Runs Smart draft to decide and fill proposal-enabled packet drafts in one call.",
        },
        LlmPurposeDescriptor {
            purpose: crate::slices::calendar_drafts::service::EXTRACT_PURPOSE,
            label: "Calendar event drafts",
            description: "Extracts event details from accepted queue items.",
        },
        LlmPurposeDescriptor {
            purpose: crate::slices::crm_drafts::service::FILL_PURPOSE,
            label: "CRM note drafts",
            description: "Writes the note body and contact fields for CRM activity drafts.",
        },
        LlmPurposeDescriptor {
            purpose: crate::slices::crm_record_drafts::service::FILL_PURPOSE,
            label: "CRM record drafts",
            description: "Extracts company and contact records referenced by operator notes.",
        },
        LlmPurposeDescriptor {
            purpose: crate::slices::crm_record_drafts::service::ENRICH_PURPOSE,
            label: "CRM web enrichment",
            description: "Fills missing CRM record fields from fetched website text.",
        },
        LlmPurposeDescriptor {
            purpose: crate::slices::crm_sales_intent::service::FILL_PURPOSE,
            label: "CRM sales intent",
            description:
                "Extracts lead intent, qualification, rationale, and next step from accepted work.",
        },
        LlmPurposeDescriptor {
            purpose: crate::slices::email_drafts::service::FILL_PURPOSE,
            label: "Email reply drafts",
            description: "Drafts Gmail replies from the source message thread.",
        },
        LlmPurposeDescriptor {
            purpose: crate::slices::follow_up_tasks::service::FILL_PURPOSE,
            label: "Follow-up task drafts",
            description: "Extracts a task title, due date, and context from accepted work.",
        },
        LlmPurposeDescriptor {
            purpose: crate::slices::invoice_drafts::service::FILL_PURPOSE,
            label: "Invoice drafts",
            description: "Extracts billable line items and grounded amounts for invoice drafts.",
        },
        LlmPurposeDescriptor {
            purpose: crate::slices::invoice_drafts::service::CUSTOMER_ENRICH_PURPOSE,
            label: "Invoice customer enrichment",
            description: "Fills missing invoice customer fields from fetched website text.",
        },
        LlmPurposeDescriptor {
            purpose: crate::slices::enrichment::service::RESEARCH_ACTION_PURPOSE,
            label: "Agentic web research actions",
            description:
                "Chooses bounded search, fetch, or finish actions for enrichment research.",
        },
        LlmPurposeDescriptor {
            purpose: crate::slices::ledger_drafts::service::FILL_PURPOSE,
            label: "Ledger entry drafts",
            description: "Extracts received-payment details for accounting provider writes.",
        },
        LlmPurposeDescriptor {
            purpose: crate::slices::content_drafts::service::FILL_PURPOSE,
            label: "Content drafts",
            description: "Drafts grounded content from Drive corpus evidence snippets.",
        },
        LlmPurposeDescriptor {
            purpose: crate::slices::social_publishing::service::DRAFT_PURPOSE,
            label: "Social post drafts",
            description: "Drafts grounded per-channel social copy from published content.",
        },
        LlmPurposeDescriptor {
            purpose: crate::slices::claim_drafts::service::FILL_PURPOSE,
            label: "Claim narratives",
            description: "Writes grounded shipping-damage claim narrative text.",
        },
        LlmPurposeDescriptor {
            purpose: crate::slices::owner_reports::service::NARRATION_PURPOSE,
            label: "Owner report narration",
            description: "Summarizes local reporting metrics into the weekly or MTD digest.",
        },
    ]
}

fn is_known_purpose(purpose: &str) -> bool {
    known_purposes()
        .iter()
        .any(|known| known.purpose == purpose)
}

fn purpose_settings(
    config: &llm::LlmRuntimeConfig,
    descriptor: &LlmPurposeDescriptor,
    persisted_overrides: &[LlmPurposeRouteOverrideUpdate],
) -> LlmPurposeRouteSettings {
    let route = llm::route_config_for_purpose(config, descriptor.purpose);
    let override_config = persisted_overrides
        .iter()
        .find(|override_config| override_config.purpose == descriptor.purpose);
    LlmPurposeRouteSettings {
        purpose: descriptor.purpose.to_string(),
        label: descriptor.label.to_string(),
        description: descriptor.description.to_string(),
        effective_backend: route.backend.as_str().to_string(),
        effective_model: route.model,
        override_backend: override_config.map(|config| config.backend.clone()),
        override_model: override_config.and_then(|config| config.model.clone()),
    }
}

/// [`execute_recorded`] with an explicit runtime config (tests inject an
/// unconfigured one so no environment can route them to a live backend).
pub fn execute_recorded_with_config(
    persistence: PersistencePool,
    client_id: &str,
    purpose: &str,
    request: &TypedLlmTaskRequest,
    config: &llm::LlmRuntimeConfig,
) -> AppResult<TypedLlmTaskOutputEnvelope> {
    let route = llm::route_config_for_purpose(config, purpose);
    let backend = route.backend;
    let sink = PersistedUsageSink::new(
        persistence.clone(),
        client_id.to_string(),
        purpose.to_string(),
    );
    let started = Instant::now();
    let result = llm::execute_typed_task_with_usage_sink(config, purpose, request, Some(&sink));
    if backend != LlmBackend::Harness {
        // The API transport never calls the sink — account from the result.
        let mut insert = match &result {
            Ok(envelope) => insert_from_envelope(purpose, envelope),
            Err(err) => failure_insert(FailureUsage {
                purpose,
                request,
                route: backend.as_str(),
                provider: if backend == LlmBackend::Local {
                    "local_openai_compatible"
                } else {
                    ""
                },
                model: route.model.as_deref().unwrap_or_default(),
                latency_ms: started.elapsed().as_millis() as u64,
                error_code: err.code(),
                error_message: err.message(),
            }),
        };
        insert.row.route = backend.as_str().to_string();
        persist(&persistence, client_id, &insert);
    } else if result.is_err() && sink.records_written() == 0 {
        let err = result.as_ref().expect_err("checked is_err");
        let insert = failure_insert(FailureUsage {
            purpose,
            request,
            route: "harness",
            provider: "claude",
            model: route.model.as_deref().unwrap_or_default(),
            latency_ms: started.elapsed().as_millis() as u64,
            error_code: err.code(),
            error_message: err.message(),
        });
        persist(&persistence, client_id, &insert);
    }
    result
}

fn insert_from_envelope(purpose: &str, envelope: &TypedLlmTaskOutputEnvelope) -> UsageInsert {
    UsageInsert {
        row: AiUsageRow {
            usage_id: next_api_usage_id(),
            purpose: purpose.to_string(),
            route: match envelope.execution_route {
                TypedLlmExecutionRoute::Harness => "harness".to_string(),
                TypedLlmExecutionRoute::DirectApi => "api".to_string(),
            },
            provider: envelope.provider_id.clone(),
            model: envelope.model.clone(),
            tokens_in: envelope.usage.as_ref().and_then(|u| u.prompt_tokens),
            tokens_out: envelope.usage.as_ref().and_then(|u| u.completion_tokens),
            total_tokens: envelope.usage.as_ref().and_then(|u| u.total_tokens),
            cost_micros: envelope.usage.as_ref().and_then(|u| u.cost_micros),
            latency_ms: envelope.latency_ms,
            success: true,
            error_code: None,
            correlation_id: envelope.correlation_id.clone(),
            recorded_at_ms: now_ms(),
        },
        task_kind: None,
        thinking_level: None,
        cached_tokens: envelope.usage.as_ref().and_then(|u| u.cached_tokens),
        provider_request_id: envelope.provider_request_id.clone(),
        error_message: None,
    }
}

fn insert_from_tool_turn(
    purpose: &str,
    request: &TypedLlmTaskRequest,
    provider_id: &str,
    model: &str,
    usage: Option<TypedLlmUsage>,
    latency_ms: u64,
    provider_request_id: Option<String>,
) -> UsageInsert {
    UsageInsert {
        row: AiUsageRow {
            usage_id: next_api_usage_id(),
            purpose: purpose.to_string(),
            route: "api".to_string(),
            provider: provider_id.to_string(),
            model: model.to_string(),
            tokens_in: usage.as_ref().and_then(|u| u.prompt_tokens),
            tokens_out: usage.as_ref().and_then(|u| u.completion_tokens),
            total_tokens: usage.as_ref().and_then(|u| u.total_tokens),
            cost_micros: usage.as_ref().and_then(|u| u.cost_micros),
            latency_ms,
            success: true,
            error_code: None,
            correlation_id: request.correlation_id.clone(),
            recorded_at_ms: now_ms(),
        },
        task_kind: Some(task_kind(request)),
        thinking_level: None,
        cached_tokens: usage.as_ref().and_then(|u| u.cached_tokens),
        provider_request_id,
        error_message: None,
    }
}

fn failure_insert_from_envelope(
    purpose: &str,
    request: &TypedLlmTaskRequest,
    envelope: &TypedLlmTaskOutputEnvelope,
    error: &AppError,
) -> UsageInsert {
    UsageInsert {
        row: AiUsageRow {
            usage_id: next_api_usage_id(),
            purpose: purpose.to_string(),
            route: match envelope.execution_route {
                TypedLlmExecutionRoute::Harness => "harness".to_string(),
                TypedLlmExecutionRoute::DirectApi => "api".to_string(),
            },
            provider: envelope.provider_id.clone(),
            model: envelope.model.clone(),
            tokens_in: envelope.usage.as_ref().and_then(|u| u.prompt_tokens),
            tokens_out: envelope.usage.as_ref().and_then(|u| u.completion_tokens),
            total_tokens: envelope.usage.as_ref().and_then(|u| u.total_tokens),
            cost_micros: envelope.usage.as_ref().and_then(|u| u.cost_micros),
            latency_ms: envelope.latency_ms,
            success: false,
            error_code: Some(error.code().to_string()),
            correlation_id: envelope.correlation_id.clone(),
            recorded_at_ms: now_ms(),
        },
        task_kind: Some(task_kind(request)),
        thinking_level: None,
        cached_tokens: envelope.usage.as_ref().and_then(|u| u.cached_tokens),
        provider_request_id: envelope.provider_request_id.clone(),
        error_message: Some(trim_error_message(error.message())),
    }
}

fn persist_tool_loop_failure(
    persistence: &PersistencePool,
    client_id: &str,
    purpose: &str,
    request: &TypedLlmTaskRequest,
    error: &AppError,
    latency_ms: u64,
) {
    let insert = failure_insert(FailureUsage {
        purpose,
        request,
        route: "api",
        provider: "",
        model: request.provider_policy.preferred_model.as_str(),
        latency_ms,
        error_code: error.code(),
        error_message: error.message(),
    });
    persist(persistence, client_id, &insert);
}

fn tool_loop_unavailable(message: &'static str) -> AppError {
    AppError::conflict(
        TOOL_LOOP_UNAVAILABLE_CODE,
        message,
        CorrelationId::generate(),
    )
}

fn tool_loop_exhausted() -> AppError {
    AppError::conflict(
        TOOL_LOOP_EXHAUSTED_CODE,
        "tool-loop typed task exhausted its bounded turns before final output",
        CorrelationId::generate(),
    )
}

fn task_kind(request: &TypedLlmTaskRequest) -> String {
    format!("{:?}", request.spec.task_class).to_ascii_lowercase()
}

struct FailureUsage<'a> {
    purpose: &'a str,
    request: &'a TypedLlmTaskRequest,
    route: &'a str,
    provider: &'a str,
    model: &'a str,
    latency_ms: u64,
    error_code: &'a str,
    error_message: &'a str,
}

fn failure_insert(failure: FailureUsage<'_>) -> UsageInsert {
    UsageInsert {
        row: AiUsageRow {
            usage_id: next_api_usage_id(),
            purpose: failure.purpose.to_string(),
            route: failure.route.to_string(),
            provider: failure.provider.to_string(),
            model: failure.model.to_string(),
            tokens_in: None,
            tokens_out: None,
            total_tokens: None,
            cost_micros: None,
            latency_ms: failure.latency_ms,
            success: false,
            error_code: Some(failure.error_code.to_string()),
            correlation_id: failure.request.correlation_id.clone(),
            recorded_at_ms: now_ms(),
        },
        task_kind: Some(task_kind(failure.request)),
        thinking_level: None,
        cached_tokens: None,
        provider_request_id: None,
        error_message: Some(trim_error_message(failure.error_message)),
    }
}

fn trim_error_message(message: &str) -> String {
    message.trim().chars().take(1_000).collect()
}

fn next_api_usage_id() -> String {
    let sequence = API_USAGE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("aiu-api-{}-{sequence}", now_ms())
}

fn persist(persistence: &PersistencePool, client_id: &str, insert: &UsageInsert) {
    let mut guard = persistence.lock();
    if let Err(err) = store::insert_usage(guard.connection(), client_id, insert) {
        tracing::warn!(error = %err, usage_id = %insert.row.usage_id, "ai usage row insert failed");
    }
}

/// Sink handed to the harness transport: one row per attempt, stamped with
/// the real purpose (the transport only knows the generic task family).
pub struct PersistedUsageSink {
    persistence: PersistencePool,
    client_id: String,
    purpose: String,
    records_written: AtomicU64,
}

impl PersistedUsageSink {
    pub fn new(persistence: PersistencePool, client_id: String, purpose: String) -> Self {
        Self {
            persistence,
            client_id,
            purpose,
            records_written: AtomicU64::new(0),
        }
    }

    pub fn records_written(&self) -> u64 {
        self.records_written.load(Ordering::Relaxed)
    }
}

impl AiCallUsageSink for PersistedUsageSink {
    fn record(&self, record: AiCallUsageRecord) {
        self.records_written.fetch_add(1, Ordering::Relaxed);
        let insert = UsageInsert {
            row: AiUsageRow {
                usage_id: record.usage_id.clone(),
                purpose: self.purpose.clone(),
                route: record.route.clone(),
                provider: record.provider.clone(),
                model: record.model.clone(),
                tokens_in: record.tokens_in,
                tokens_out: record.tokens_out,
                total_tokens: record.total_tokens,
                cost_micros: record.cost_micros,
                latency_ms: record.latency_ms,
                success: record.success,
                error_code: record.error_code.clone(),
                correlation_id: record.correlation_id.clone(),
                recorded_at_ms: record.recorded_at_ms.max(0) as u64,
            },
            task_kind: record.task_kind.clone(),
            thinking_level: record.thinking_level.clone(),
            cached_tokens: record.cached_tokens,
            provider_request_id: record.provider_request_id.clone(),
            error_message: record.error_message.as_deref().map(trim_error_message),
        };
        persist(&self.persistence, &self.client_id, &insert);
    }
}
