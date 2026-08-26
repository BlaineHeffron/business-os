use axum::http::HeaderMap;
use bos_contracts::follow_up_tasks::{TaskStatus, TasksResponse};
use bos_contracts::operator_notes::{OperatorNote, OperatorNoteCreateResponse};
use bos_contracts::work_queue::{WorkItemStatus, WorkQueueResponse};
use serde_json::{json, Value};

use crate::http::{now_ms, AppState, AuthContext, OperatorScope};
use crate::store_core::StoreError;

const SERVER_NAME: &str = "businessos";
const SERVER_VERSION: &str = "0.1.0";
const LEGACY_PROTOCOL_VERSION: &str = "2025-11-25";
pub const STATELESS_PROTOCOL_VERSION: &str = "2026-07-28";
const PROTOCOL_VERSION_META_KEY: &str = "io.modelcontextprotocol/protocolVersion";
const CLIENT_CAPABILITIES_META_KEY: &str = "io.modelcontextprotocol/clientCapabilities";
const SERVER_INFO_META_KEY: &str = "io.modelcontextprotocol/serverInfo";
const LIST_CACHE_TTL_MS: u64 = 5 * 60 * 1000;
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &[
    "2024-11-05",
    "2025-03-26",
    "2025-06-18",
    LEGACY_PROTOCOL_VERSION,
    STATELESS_PROTOCOL_VERSION,
];
const DEFAULT_QUEUE_LIMIT: usize = 50;
const DEFAULT_SEARCH_LIMIT: usize = 10;
const DEFAULT_TASK_LIMIT: usize = 100;
const MCP_ACTOR_PREFIX: &str = "mcp:";

pub enum McpHttpResponse {
    Json(Value),
    Accepted,
}

#[derive(Debug)]
struct ToolError {
    code: &'static str,
    message: String,
}

impl ToolError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

pub fn enabled() -> bool {
    crate::env_registry::flag(&crate::env_registry::BOS_AGENT_MCP_ENABLED)
}

pub fn manifest(state: &AppState) -> Value {
    json!({
        "name": SERVER_NAME,
        "transport": "streamable-http",
        "path": "/api/agent-mcp",
        "enabled": enabled(),
        "stateless": true,
        "protocolVersions": [STATELESS_PROTOCOL_VERSION, LEGACY_PROTOCOL_VERSION],
        "authorization": "operator_bearer_token",
        "injection": "explicit_bos_context_only",
        "approval_model": "Tools may read local BOS context, create notes/work-queue artifacts, and stage drafts. They cannot approve drafts, send email, publish content, or write to providers.",
        "tools": tools_for_state(state),
    })
}

pub fn monitor_mcp_server_config() -> Option<Value> {
    if !enabled() {
        return None;
    }
    let base = crate::env_registry::string(&crate::env_registry::BOS_PUBLIC_BASE_URL)?
        .trim()
        .trim_end_matches('/')
        .to_string();
    if base.is_empty() {
        return None;
    }
    Some(json!({
        "type": "http",
        "url": format!("{base}/api/agent-mcp"),
        "authorization": "operator_bearer_token_required",
        "alwaysLoad": false,
    }))
}

pub fn handle_request(state: AppState, auth: AuthContext, message: Value) -> McpHttpResponse {
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    let method = message.get("method").and_then(Value::as_str).unwrap_or("");
    let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
    let stateless = method == "server/discover" || request_claims_stateless(&message);
    let cacheable = matches!(
        method,
        "tools/list"
            | "prompts/list"
            | "resources/list"
            | "resources/templates/list"
            | "resources/read"
    );
    let response = match method {
        "server/discover" => jsonrpc_result(
            id,
            json!({
                "supportedVersions": [STATELESS_PROTOCOL_VERSION],
                "capabilities": {
                    "tools": {},
                    "resources": {},
                    "prompts": {},
                },
                "instructions": "BusinessOS tools are operator-authenticated. They may read BOS context, create notes and queue artifacts, and stage drafts; they cannot approve drafts, send email, publish, or write providers.",
            }),
            true,
            true,
        ),
        "initialize" if !stateless => jsonrpc_result(
            id,
            json!({
                "protocolVersion": params.get("protocolVersion").and_then(Value::as_str).unwrap_or(LEGACY_PROTOCOL_VERSION),
                "capabilities": {
                    "tools": {},
                    "resources": {},
                    "prompts": {},
                },
                "serverInfo": {
                    "name": SERVER_NAME,
                    "version": SERVER_VERSION,
                },
            }),
            false,
            false,
        ),
        "notifications/initialized" if !stateless => return McpHttpResponse::Accepted,
        "tools/list" => jsonrpc_result(
            id,
            json!({ "tools": tools_for_state(&state) }),
            stateless,
            cacheable,
        ),
        "resources/list" => jsonrpc_result(id, json!({ "resources": [] }), stateless, cacheable),
        "resources/templates/list" => {
            jsonrpc_result(id, json!({ "resourceTemplates": [] }), stateless, cacheable)
        }
        "prompts/list" => jsonrpc_result(id, json!({ "prompts": [] }), stateless, cacheable),
        "tools/call" => match call_tool(state, auth, params) {
            Ok(result) => jsonrpc_result(id, result, stateless, false),
            Err(err) => jsonrpc_error(id, -32000, &err.message, Some(json!({ "code": err.code }))),
        },
        "" => jsonrpc_error(id, -32600, "missing method", None),
        _ => jsonrpc_error(id, -32601, "method not found", None),
    };
    McpHttpResponse::Json(response)
}

fn request_claims_stateless(message: &Value) -> bool {
    message
        .get("params")
        .and_then(|value| value.get("_meta"))
        .and_then(|value| value.get(PROTOCOL_VERSION_META_KEY))
        .and_then(Value::as_str)
        == Some(STATELESS_PROTOCOL_VERSION)
}

pub fn validate_http_request(headers: &HeaderMap, message: &Value) -> Option<Value> {
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    let header_version = headers
        .get("mcp-protocol-version")
        .and_then(|value| value.to_str().ok());
    if header_version.is_some_and(|version| !SUPPORTED_PROTOCOL_VERSIONS.contains(&version)) {
        return Some(jsonrpc_error(
            id,
            -32022,
            "unsupported MCP protocol version",
            Some(json!({ "supportedVersions": SUPPORTED_PROTOCOL_VERSIONS })),
        ));
    }
    let claims_stateless = message.get("method").and_then(Value::as_str) == Some("server/discover")
        || header_version == Some(STATELESS_PROTOCOL_VERSION)
        || request_claims_stateless(message);
    if !claims_stateless {
        return None;
    }
    if header_version != Some(STATELESS_PROTOCOL_VERSION) {
        return Some(jsonrpc_error(
            id,
            -32020,
            "MCP-Protocol-Version header must match the stateless protocol version",
            None,
        ));
    }
    let meta = message.pointer("/params/_meta");
    if meta
        .and_then(|value| value.get(PROTOCOL_VERSION_META_KEY))
        .and_then(Value::as_str)
        != Some(STATELESS_PROTOCOL_VERSION)
    {
        return Some(jsonrpc_error(
            id,
            -32602,
            "request _meta must carry the stateless protocol version",
            None,
        ));
    }
    if !meta
        .and_then(|value| value.get(CLIENT_CAPABILITIES_META_KEY))
        .is_some_and(Value::is_object)
    {
        return Some(jsonrpc_error(
            id,
            -32602,
            "request _meta must carry client capabilities",
            None,
        ));
    }
    let method = message.get("method").and_then(Value::as_str).unwrap_or("");
    let header_method = headers
        .get("mcp-method")
        .and_then(|value| value.to_str().ok());
    if header_method != Some(method) {
        return Some(jsonrpc_error(
            id,
            -32020,
            "Mcp-Method header must match the JSON-RPC method",
            None,
        ));
    }
    if matches!(method, "tools/call" | "resources/read" | "prompts/get") {
        let expected_name = message
            .pointer("/params/name")
            .or_else(|| message.pointer("/params/uri"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let header_name = headers
            .get("mcp-name")
            .and_then(|value| value.to_str().ok());
        if header_name != Some(expected_name) {
            return Some(jsonrpc_error(
                id,
                -32020,
                "Mcp-Name header must match params.name or params.uri",
                None,
            ));
        }
    }
    None
}

fn tools_for_state(state: &AppState) -> Vec<Value> {
    let mut tools = vec![
        tool(
            "bos_work_queue_list",
            "List BusinessOS work queue items visible to the authenticated operator.",
            json!({
                "type": "object",
                "properties": {
                    "status": { "type": "string", "enum": ["open", "accepted", "dismissed", "all"], "description": "Defaults to open." },
                    "limit": { "type": "integer", "description": "Maximum items to return. Defaults to 50, max 200." }
                },
                "additionalProperties": false
            }),
        ),
        tool(
            "bos_work_item_source",
            "Read the source email or note behind one visible work item.",
            json!({
                "type": "object",
                "properties": {
                    "item_id": { "type": "string" }
                },
                "required": ["item_id"],
                "additionalProperties": false
            }),
        ),
        tool(
            "bos_follow_ups_list",
            "List local tasks and outbound email follow-up workflows visible to the authenticated operator.",
            json!({
                "type": "object",
                "properties": {
                    "task_status": { "type": "string", "enum": ["open", "done", "all"], "description": "Defaults to open." },
                    "email_follow_up_status": { "type": "string", "enum": ["open", "resolved", "all"], "description": "Defaults to open." },
                    "today": { "type": "string", "description": "Optional local date YYYY-MM-DD for task escalation decoration." }
                },
                "additionalProperties": false
            }),
        ),
        tool(
            "bos_operator_note_create",
            "Create an operator note and accepted queue artifact. Optional action kinds stage drafts only; no approval or provider write occurs.",
            json!({
                "type": "object",
                "properties": {
                    "body": { "type": "string" },
                    "actions": { "type": "array", "items": { "type": "string" }, "description": "Optional packet kinds to stage from this note, e.g. follow_up_task or crm_activity. Empty means note/queue artifact only." },
                    "idempotency_key": { "type": "string" }
                },
                "required": ["body", "idempotency_key"],
                "additionalProperties": false
            }),
        ),
        tool(
            "bos_agent_result_ingest",
            "Record an agent result as a BusinessOS operator note/work-queue artifact for human review.",
            json!({
                "type": "object",
                "properties": {
                    "summary": { "type": "string" },
                    "details": { "type": "string" },
                    "source_item_id": { "type": "string" },
                    "agent_session_id": { "type": "string" },
                    "actions": { "type": "array", "items": { "type": "string" }, "description": "Optional packet kinds to stage from the result." },
                    "idempotency_key": { "type": "string" }
                },
                "required": ["summary", "idempotency_key"],
                "additionalProperties": false
            }),
        ),
        tool(
            "bos_stage_draft",
            "Kick draft production for an accepted work item and packet kind. This stages a draft only; approval and outbox gates remain separate.",
            json!({
                "type": "object",
                "properties": {
                    "item_id": { "type": "string" },
                    "packet_kind": { "type": "string", "description": "One enabled packet kind already suggested on the item." },
                    "idempotency_key": { "type": "string" }
                },
                "required": ["item_id", "packet_kind", "idempotency_key"],
                "additionalProperties": false
            }),
        ),
        tool(
            "bos_crm_context_search",
            "Read-only CRM existence check for a company/contact when CRM search support is configured.",
            json!({
                "type": "object",
                "properties": {
                    "company_name": { "type": "string" },
                    "contact_email": { "type": "string" },
                    "contact_full_name": { "type": "string" }
                },
                "additionalProperties": false
            }),
        ),
    ];
    if state.slice_enabled(crate::slices::drive_corpus::SLICE.id) {
        tools.push(tool(
            "bos_drive_corpus_search",
            "Search the local Google Drive corpus index. Reads cached BOS data only.",
            json!({
                "type": "object",
                "properties": {
                    "q": { "type": "string" },
                    "limit": { "type": "integer", "description": "Defaults to 10, max 100." }
                },
                "required": ["q"],
                "additionalProperties": false
            }),
        ));
    }
    if state.slice_enabled(crate::slices::social_publishing::SLICE.id) {
        tools.push(tool(
            "bos_social_published_content_ingest",
            "Register an already-published canonical article and metadata. BusinessOS drafts social copy separately; this tool cannot submit copy, approve, publish, select channels, or access Buffer credentials.",
            json!({
                "type": "object",
                "properties": {
                    "source_content_draft_id": { "type": "string", "description": "Optional BusinessOS content draft whose delivered canonical URL is being promoted." },
                    "source_kind": { "type": "string", "description": "Stable CMS/source namespace, for example wordpress." },
                    "external_id": { "type": "string", "description": "Stable identifier in the publishing source." },
                    "canonical_url": { "type": "string", "description": "Canonical https URL of the already-published blog post." },
                    "title": { "type": "string" },
                    "excerpt": { "type": "string", "description": "Optional published excerpt/summary used as bounded drafting context." },
                    "published_at": { "type": "string", "description": "Optional RFC3339 publication timestamp." },
                    "idempotency_key": { "type": "string" }
                },
                "required": ["source_kind", "external_id", "canonical_url", "title", "idempotency_key"],
                "additionalProperties": false
            }),
        ));
    }
    tools
}

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema,
    })
}

fn call_tool(state: AppState, auth: AuthContext, params: Value) -> Result<Value, ToolError> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::new("mcp_tool_name_required", "tool name is required"))?;
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    match name {
        "bos_work_queue_list" => work_queue_list(&state, &auth.scope, &args),
        "bos_work_item_source" => work_item_source(&state, &auth.scope, &args),
        "bos_follow_ups_list" => follow_ups_list(&state, &auth.scope, &args),
        "bos_operator_note_create" => operator_note_create(state, auth, &args),
        "bos_agent_result_ingest" => agent_result_ingest(state, auth, &args),
        "bos_stage_draft" => stage_draft(state, auth, &args),
        "bos_crm_context_search" => crm_context_search(&state, &args),
        "bos_drive_corpus_search" => drive_corpus_search(&state, &args),
        "bos_social_published_content_ingest" => {
            social_published_content_ingest(state, auth, &args)
        }
        _ => Err(ToolError::new(
            "mcp_tool_unsupported",
            format!("unsupported BusinessOS MCP tool {name}"),
        )),
    }
}

fn social_published_content_ingest(
    state: AppState,
    auth: AuthContext,
    args: &Value,
) -> Result<Value, ToolError> {
    require_slice(&state, crate::slices::social_publishing::SLICE.id)?;
    let request: bos_contracts::social_publishing::SocialPublishedContentIngressRequest =
        serde_json::from_value(args.clone()).map_err(|_| {
            ToolError::new(
                "mcp_argument_invalid",
                "published-content arguments do not match the ingress contract",
            )
        })?;
    let actor_id = mcp_actor(&auth);
    let source = {
        let mut persistence = state.persistence.lock();
        crate::slices::social_publishing::service::ingest_source_request(
            persistence.connection(),
            &state.client_id,
            &actor_id,
            bos_contracts::receipt::ActorKindDto::Agent,
            &request,
            now_ms(),
        )
        .map_err(store_tool_error)?
    };
    let generation_key = format!("social-ingress-generate:{}", request.idempotency_key);
    let source = match crate::slices::social_publishing::service::kickoff_generation(
        state,
        &source.source_id,
        source.revision,
        &generation_key,
        "social_draft_generator",
        bos_contracts::receipt::ActorKindDto::System,
    )
    .map_err(store_tool_error)?
    {
        crate::slices::social_publishing::service::GenerationKickoffOutcome::Accepted(source) => {
            source
        }
        crate::slices::social_publishing::service::GenerationKickoffOutcome::Conflict(_) => {
            return Err(ToolError::new(
                "expected_revision_conflict",
                "published content changed before drafting began",
            ));
        }
    };
    let proposal_state = source.generation_status;
    Ok(tool_result(
        "Published content registered. BusinessOS social drafting started.",
        json!({
            "source": source,
            "proposal_state": proposal_state,
            "approval_required": true,
            "provider_write": "not_performed",
        }),
    ))
}

fn work_queue_list(
    state: &AppState,
    scope: &OperatorScope,
    args: &Value,
) -> Result<Value, ToolError> {
    require_slice(state, crate::slices::work_queue::SLICE.id)?;
    let status = match optional_string(args, "status").as_deref() {
        None | Some("") | Some("open") => Some(WorkItemStatus::Open),
        Some("accepted") => Some(WorkItemStatus::Accepted),
        Some("dismissed") => Some(WorkItemStatus::Dismissed),
        Some("all") => None,
        Some(_) => {
            return Err(ToolError::new(
                "work_queue_status_invalid",
                "invalid status",
            ))
        }
    };
    let limit = optional_usize(args, "limit")
        .unwrap_or(DEFAULT_QUEUE_LIMIT)
        .clamp(1, 200);
    let in_flight = crate::produce::produce_in_flight_snapshot(state);
    let persistence = state.persistence.lock();
    let items = crate::slices::work_queue::service::feed(
        persistence.connection_ref(),
        &state.client_id,
        status,
        limit,
        scope,
        crate::slices::work_queue::service::FeedOptions {
            now_ms: now_ms(),
            auto_produce_running: crate::slices::admin_settings::service::flag(
                persistence.connection_ref(),
                &state.client_id,
                &crate::env_registry::BOS_AUTO_PRODUCE_ENABLED,
            )
            .map_err(store_tool_error)?,
            debug_enabled: crate::env_registry::flag(&crate::env_registry::BOS_DEBUG_ENABLED),
            in_flight: &in_flight,
        },
    )
    .map_err(store_tool_error)?;
    let payload = WorkQueueResponse { items };
    Ok(tool_result("Work queue fetched.", json!(payload)))
}

fn work_item_source(
    state: &AppState,
    scope: &OperatorScope,
    args: &Value,
) -> Result<Value, ToolError> {
    require_slice(state, crate::slices::work_queue::SLICE.id)?;
    let item_id = required_string(args, "item_id")?;
    let persistence = state.persistence.lock();
    let source = crate::slices::work_queue::service::item_source(
        persistence.connection_ref(),
        &state.client_id,
        &item_id,
        scope,
    )
    .map_err(|err| match err {
        crate::slices::work_queue::service::ItemSourceError::ItemNotFound => {
            ToolError::new("work_item_not_found", "work item not found")
        }
        crate::slices::work_queue::service::ItemSourceError::SourceMissing => {
            ToolError::new("work_item_source_missing", "work item source missing")
        }
        crate::slices::work_queue::service::ItemSourceError::SourceUnsupported => {
            ToolError::new("produce_source_unsupported", "work item source unsupported")
        }
        crate::slices::work_queue::service::ItemSourceError::Store(err) => store_tool_error(err),
    })?;
    Ok(tool_result("Work item source fetched.", json!(source)))
}

fn follow_ups_list(
    state: &AppState,
    scope: &OperatorScope,
    args: &Value,
) -> Result<Value, ToolError> {
    require_slice(state, crate::slices::follow_up_tasks::SLICE.id)?;
    let task_status = match optional_string(args, "task_status").as_deref() {
        None | Some("") | Some("open") => Some(TaskStatus::Open),
        Some("done") => Some(TaskStatus::Done),
        Some("all") => None,
        Some(_) => return Err(ToolError::new("task_status_invalid", "invalid task status")),
    };
    let today = optional_string(args, "today");
    if let Some(today) = today.as_deref() {
        if !today.is_empty() && !crate::slices::follow_up_tasks::service::is_iso_date(today) {
            return Err(ToolError::new(
                "task_today_invalid",
                "today must be YYYY-MM-DD",
            ));
        }
    }
    let email_status = match optional_string(args, "email_follow_up_status").as_deref() {
        None | Some("") | Some("open") => {
            crate::slices::email_drafts::store::FollowUpListStatus::Open
        }
        Some("resolved") => crate::slices::email_drafts::store::FollowUpListStatus::Resolved,
        Some("all") => crate::slices::email_drafts::store::FollowUpListStatus::All,
        Some(_) => {
            return Err(ToolError::new(
                "email_follow_up_status_invalid",
                "invalid email follow-up status",
            ))
        }
    };
    let persistence = state.persistence.lock();
    let mut tasks = crate::slices::follow_up_tasks::store::list_tasks(
        persistence.connection_ref(),
        &state.client_id,
        task_status,
        DEFAULT_TASK_LIMIT,
        scope,
    )
    .map_err(store_tool_error)?;
    if state.slice_enabled(crate::slices::email_drafts::SLICE.id) {
        crate::slices::email_drafts::store::decorate_tasks_with_follow_ups(
            persistence.connection_ref(),
            &state.client_id,
            scope,
            &mut tasks,
        )
        .map_err(store_tool_error)?;
    }
    if let Some(today) = today.as_deref().filter(|value| !value.is_empty()) {
        crate::slices::follow_up_tasks::service::decorate_task_escalations(&mut tasks, today);
    }
    let follow_ups = if state.slice_enabled(crate::slices::email_drafts::SLICE.id) {
        crate::slices::email_drafts::store::list_follow_ups(
            persistence.connection_ref(),
            &state.client_id,
            email_status,
            scope,
        )
        .map_err(store_tool_error)?
    } else {
        Vec::new()
    };
    Ok(tool_result(
        "Follow-ups fetched.",
        json!({
            "tasks": TasksResponse { tasks }.tasks,
            "email_follow_ups": follow_ups,
        }),
    ))
}

fn operator_note_create(
    state: AppState,
    auth: AuthContext,
    args: &Value,
) -> Result<Value, ToolError> {
    let body = required_string(args, "body")?;
    let idempotency_key = required_string(args, "idempotency_key")?;
    let actions = optional_string_array(args, "actions")?;
    let actor_id = mcp_actor(&auth);
    create_note_with_actions(
        state,
        &auth.scope,
        &actor_id,
        &body,
        &actions,
        &idempotency_key,
    )
}

fn agent_result_ingest(
    state: AppState,
    auth: AuthContext,
    args: &Value,
) -> Result<Value, ToolError> {
    let summary = required_string(args, "summary")?;
    let idempotency_key = required_string(args, "idempotency_key")?;
    let details = optional_string(args, "details").unwrap_or_default();
    let source_item_id = optional_string(args, "source_item_id").unwrap_or_default();
    let agent_session_id = optional_string(args, "agent_session_id").unwrap_or_default();
    let actions = optional_string_array(args, "actions")?;
    let mut body = String::from("Agent result");
    if !agent_session_id.is_empty() {
        body.push_str(&format!("\nAgent session: {agent_session_id}"));
    }
    if !source_item_id.is_empty() {
        body.push_str(&format!("\nSource work item: {source_item_id}"));
    }
    body.push_str(&format!("\nSummary: {summary}"));
    if !details.trim().is_empty() {
        body.push_str("\n\n");
        body.push_str(details.trim());
    }
    let actor_id = mcp_actor(&auth);
    create_note_with_actions(
        state,
        &auth.scope,
        &actor_id,
        &body,
        &actions,
        &idempotency_key,
    )
}

fn create_note_with_actions(
    state: AppState,
    scope: &OperatorScope,
    actor_id: &str,
    body: &str,
    actions: &[String],
    idempotency_key: &str,
) -> Result<Value, ToolError> {
    require_slice(&state, crate::slices::operator_notes::SLICE.id)?;
    require_slice(&state, crate::slices::work_queue::SLICE.id)?;
    if idempotency_key.trim().is_empty() {
        return Err(ToolError::new(
            "idempotency_key_required",
            "idempotency_key is required",
        ));
    }
    if body.trim().is_empty() {
        return Err(ToolError::new(
            "operator_note_body_empty",
            "operator note body is empty",
        ));
    }
    let actions = resolve_actions_for_enabled(&state, actions)?;
    let now = now_ms();
    let trimmed_idempotency_key = idempotency_key.trim();
    let note_id = format!("note_mcp_{trimmed_idempotency_key}");
    let store_idempotency_key = format!("mcp_note:{trimmed_idempotency_key}");
    let note = OperatorNote {
        note_id,
        body: body.trim().to_string(),
        category_id: crate::slices::operator_notes::service::DEFAULT_CATEGORY.to_string(),
        created_by: actor_id.to_string(),
        created_at_ms: now,
    };
    let work_item_emitted = {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        crate::slices::operator_notes::store::insert_note(
            conn,
            &state.client_id,
            &note,
            &store_idempotency_key,
        )
        .map_err(store_tool_error)?;
        crate::slices::operator_notes::service::emit_item_for_note(
            conn,
            &state.client_id,
            &note,
            &actions,
            now,
        )
        .map_err(store_tool_error)?
    };
    let item_id = format!(
        "wi_{}_{}",
        crate::slices::work_queue::SOURCE_KIND_OPERATOR_NOTE,
        note.note_id
    );
    if work_item_emitted && !actions.is_empty() {
        for kind in actions.clone() {
            crate::produce::kick_produce_for_kind(
                state.clone(),
                item_id.clone(),
                kind.clone(),
                format!("mcp_note_action:{item_id}:{kind}:{idempotency_key}"),
                note.created_by.clone(),
                bos_contracts::receipt::ActorKindDto::Agent,
            );
        }
    }
    let response = OperatorNoteCreateResponse {
        note,
        work_item_id: item_id.clone(),
        work_item_emitted,
    };
    let visible = match scope {
        OperatorScope::All => true,
        OperatorScope::User(user_id) => mcp_source_user(&response.note.created_by) == Some(user_id),
    };
    Ok(tool_result(
        "Operator note recorded.",
        json!({
            "note": response.note,
            "work_item_emitted": response.work_item_emitted,
            "work_item_id": item_id,
            "staged_actions": actions,
            "visible_to_authenticated_scope": visible,
        }),
    ))
}

fn stage_draft(state: AppState, auth: AuthContext, args: &Value) -> Result<Value, ToolError> {
    require_slice(&state, crate::slices::work_queue::SLICE.id)?;
    let item_id = required_string(args, "item_id")?;
    let kind = required_string(args, "packet_kind")?;
    let idempotency_key = required_string(args, "idempotency_key")?;
    if idempotency_key.trim().is_empty() {
        return Err(ToolError::new(
            "idempotency_key_required",
            "idempotency_key is required",
        ));
    }
    let owning_slice = crate::slices::work_queue::packet_kind_slice(&kind)
        .ok_or_else(|| ToolError::new("produce_kind_unsupported", "unknown packet kind"))?;
    require_slice(&state, owning_slice)?;
    {
        let persistence = state.persistence.lock();
        let item = crate::slices::work_queue::store::get_item_scoped(
            persistence.connection_ref(),
            &state.client_id,
            &item_id,
            &auth.scope,
        )
        .map_err(store_tool_error)?
        .ok_or_else(|| ToolError::new("work_item_not_found", "work item not found"))?
        .item;
        crate::produce::validate_item_for_kind(&item, &kind)
            .map_err(|code| ToolError::new(code, code))?;
    }
    crate::produce::kick_produce_for_kind(
        state,
        item_id.clone(),
        kind.clone(),
        format!("mcp_stage:{}", idempotency_key.trim()),
        mcp_actor(&auth),
        bos_contracts::receipt::ActorKindDto::Agent,
    );
    Ok(tool_result(
        "Draft production accepted.",
        json!({
            "producing": true,
            "item_id": item_id,
            "packet_kind": kind,
            "actor_id": mcp_actor(&auth),
            "approval_required": true,
            "provider_write": "not_performed",
        }),
    ))
}

fn mcp_actor(auth: &AuthContext) -> String {
    let actor = auth.actor_id.trim();
    let actor = if actor.is_empty() {
        crate::http::SHARED_OPERATOR_ACTOR
    } else {
        actor
    };
    if actor.starts_with(MCP_ACTOR_PREFIX) {
        actor.to_string()
    } else {
        format!("{MCP_ACTOR_PREFIX}{actor}")
    }
}

fn mcp_source_user(actor_id: &str) -> Option<&str> {
    let actor = actor_id.strip_prefix(MCP_ACTOR_PREFIX).unwrap_or(actor_id);
    (actor != crate::http::SHARED_OPERATOR_ACTOR).then_some(actor)
}

fn crm_context_search(state: &AppState, args: &Value) -> Result<Value, ToolError> {
    if !state.slice_enabled(crate::slices::crm_drafts::SLICE.id)
        && !state.slice_enabled(crate::slices::crm_record_drafts::SLICE.id)
    {
        return Err(ToolError::new(
            "slice_disabled",
            "CRM slices are disabled for this client",
        ));
    }
    let company_name = optional_string(args, "company_name");
    let contact_email = optional_string(args, "contact_email");
    let contact_full_name = optional_string(args, "contact_full_name");
    if company_name.as_deref().unwrap_or("").is_empty()
        && contact_email.as_deref().unwrap_or("").is_empty()
        && contact_full_name.as_deref().unwrap_or("").is_empty()
    {
        return Err(ToolError::new(
            "crm_search_query_empty",
            "provide company_name, contact_email, or contact_full_name",
        ));
    }
    let provider =
        crate::slices::crm_drafts::service::configured_crm_provider().unwrap_or("unknown");
    let matches = crate::slices::crm_record_drafts::service::search_existing_records(
        company_name.as_deref(),
        contact_email.as_deref(),
        contact_full_name.as_deref(),
    );
    Ok(tool_result(
        "CRM context search completed.",
        json!({
            "provider": provider,
            "account_id": matches.account_id,
            "contact_id": matches.contact_id,
            "read_only": true,
        }),
    ))
}

fn drive_corpus_search(state: &AppState, args: &Value) -> Result<Value, ToolError> {
    require_slice(state, crate::slices::drive_corpus::SLICE.id)?;
    let q = required_string(args, "q")?;
    let Some(match_expr) = crate::slices::drive_corpus::service::fts_match_expression(&q) else {
        return Err(ToolError::new(
            "drive_search_query_empty",
            "drive corpus search query is empty",
        ));
    };
    let limit = optional_usize(args, "limit")
        .unwrap_or(DEFAULT_SEARCH_LIMIT)
        .clamp(1, 100);
    let persistence = state.persistence.lock();
    let hits = crate::slices::drive_corpus::store::search_chunks(
        persistence.connection_ref(),
        &state.client_id,
        &match_expr,
        limit,
    )
    .map_err(store_tool_error)?;
    Ok(tool_result(
        "Drive corpus search completed.",
        json!({ "hits": hits }),
    ))
}

fn resolve_actions_for_enabled(
    state: &AppState,
    requested: &[String],
) -> Result<Vec<String>, ToolError> {
    let mut actions = Vec::new();
    for raw in requested {
        let action = raw.trim();
        if action.is_empty() || actions.iter().any(|existing| existing == action) {
            continue;
        }
        let Some(slice_id) = crate::slices::work_queue::packet_kind_slice(action) else {
            return Err(ToolError::new(
                "operator_note_action_invalid",
                format!("unknown action kind {action}"),
            ));
        };
        if !state.slice_enabled(slice_id) {
            return Err(ToolError::new(
                "operator_note_action_invalid",
                format!("action kind {action} is disabled for this client"),
            ));
        }
        actions.push(action.to_string());
    }
    Ok(actions)
}

fn require_slice(state: &AppState, slice_id: &str) -> Result<(), ToolError> {
    if state.slice_enabled(slice_id) {
        Ok(())
    } else {
        Err(ToolError::new(
            "slice_disabled",
            format!("slice {slice_id} is disabled for this client"),
        ))
    }
}

fn tool_result(text: &str, structured_content: Value) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": structured_content,
    })
}

fn jsonrpc_result(id: Value, mut result: Value, stateless: bool, cacheable: bool) -> Value {
    if stateless {
        let result_object = result.as_object_mut().expect("MCP results are objects");
        result_object.insert("resultType".to_string(), json!("complete"));
        if cacheable {
            result_object.insert("ttlMs".to_string(), json!(LIST_CACHE_TTL_MS));
            result_object.insert("cacheScope".to_string(), json!("private"));
        }
        let meta = result_object.entry("_meta").or_insert_with(|| json!({}));
        meta.as_object_mut()
            .expect("MCP result metadata is an object")
            .insert(
                SERVER_INFO_META_KEY.to_string(),
                json!({ "name": SERVER_NAME, "version": SERVER_VERSION }),
            );
    }
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn jsonrpc_error(id: Value, code: i64, message: &str, data: Option<Value>) -> Value {
    let mut error = json!({ "code": code, "message": message });
    if let Some(data) = data {
        error["data"] = data;
    }
    json!({ "jsonrpc": "2.0", "id": id, "error": error })
}

fn required_string(args: &Value, key: &'static str) -> Result<String, ToolError> {
    optional_string(args, key)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ToolError::new("mcp_argument_required", format!("{key} is required")))
}

fn optional_string(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn optional_usize(args: &Value, key: &str) -> Option<usize> {
    args.get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
}

fn optional_string_array(args: &Value, key: &str) -> Result<Vec<String>, ToolError> {
    let Some(value) = args.get(key) else {
        return Ok(Vec::new());
    };
    let Some(items) = value.as_array() else {
        return Err(ToolError::new(
            "mcp_argument_invalid",
            format!("{key} must be an array"),
        ));
    };
    let mut out = Vec::new();
    for item in items {
        let Some(raw) = item.as_str() else {
            return Err(ToolError::new(
                "mcp_argument_invalid",
                format!("{key} entries must be strings"),
            ));
        };
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            out.push(trimmed.to_string());
        }
    }
    Ok(out)
}

fn store_tool_error(err: StoreError) -> ToolError {
    match err {
        StoreError::Domain(code) => ToolError::new("domain_error", code),
        StoreError::Sqlite(message) => {
            tracing::error!(error = %message, "agent MCP store failure");
            ToolError::new("storage_failure", "storage failure")
        }
    }
}

#[cfg(test)]
pub(crate) fn test_call_tool(
    state: AppState,
    auth: AuthContext,
    name: &str,
    args: Value,
) -> Result<Value, String> {
    call_tool(
        state,
        auth,
        json!({
            "name": name,
            "arguments": args,
        }),
    )
    .map_err(|err| err.code.to_string())
}

#[cfg(test)]
pub(crate) fn count_notes(
    conn: &rusqlite::Connection,
    client_id: &str,
) -> Result<i64, rusqlite::Error> {
    conn.query_row(
        "SELECT COUNT(*) FROM operator_notes WHERE client_id = ?1",
        rusqlite::params![client_id],
        |row| row.get(0),
    )
}
