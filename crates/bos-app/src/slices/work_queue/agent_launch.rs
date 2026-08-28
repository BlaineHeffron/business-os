//! Launch a Agent Monitor agent session seeded with a work item's context.
//!
//! Operator power tool gated by `BOS_AGENT_LAUNCH_ENABLED`. This mirrors the
//! Debug spawn-agent path (`slices/debug/routes.rs`) but sources its context
//! from a work item + its source message instead of a diagnostic row, and
//! appends the operator's optional free-text notes. The monitor endpoint
//! (`BOS_DEBUG_AGENT_MONITOR_URL`/`_TOKEN`) is shared with the Debug surface.

use bos_contracts::email_triage::InboundMessageRecord;
use bos_contracts::work_queue::{LaunchAgentResponse, WorkItem, WorkItemSourceResponse};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::outbox::{AttemptOutcome, ClaimedJob, NewOutboxJob};

/// Fallback directory for spawned agents when neither the request nor category
/// policy specifies one. Matches the Debug spawn-agent default.
pub const DEFAULT_AGENT_WORK_DIR: &str = "/home/example/projects/BusinessOS";
pub const PROVIDER_AGENT_MONITOR: &str = "agent_monitor";
pub const CAPABILITY_LAUNCH_AGENT: &str = "launch_agent";

#[derive(Debug)]
pub enum MonitorError {
    ClientBuild(reqwest::Error),
    Request(reqwest::Error),
    Rejected,
    InvalidResponse,
}

impl MonitorError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::ClientBuild(_) => "work_queue_agent_client_build_failed",
            Self::Request(_) => "work_queue_agent_monitor_request_failed",
            Self::Rejected => "work_queue_agent_monitor_rejected",
            Self::InvalidResponse => "work_queue_agent_monitor_response_invalid",
        }
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct AgentLaunchOutboxPayload {
    monitor_url: String,
    display_name: String,
    initial_prompt: String,
    work_dir: String,
}

pub struct AgentLaunchOutboxJobInput<'a> {
    pub source_id: &'a str,
    pub idempotency_key: &'a str,
    pub monitor_url: &'a str,
    pub display_name: &'a str,
    pub initial_prompt: &'a str,
    pub work_dir: &'a str,
    pub source_entity_kind: &'a str,
    pub source_entity_id: &'a str,
    pub correlation_id: Option<&'a str>,
}

pub fn job_id(item_id: &str, idempotency_key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(item_id.as_bytes());
    hasher.update([0u8]);
    hasher.update(idempotency_key.as_bytes());
    let digest = hasher.finalize();
    let mut hash = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        hash.push_str(&format!("{byte:02x}"));
    }
    format!("agent_launch:{item_id}:{hash}")
}

pub fn build_outbox_job(
    item_id: &str,
    idempotency_key: &str,
    monitor_url: &str,
    display_name: &str,
    initial_prompt: &str,
    work_dir: &str,
) -> Result<NewOutboxJob, serde_json::Error> {
    build_outbox_job_for_source(AgentLaunchOutboxJobInput {
        source_id: item_id,
        idempotency_key,
        monitor_url,
        display_name,
        initial_prompt,
        work_dir,
        source_entity_kind: super::store::AGENT_LAUNCH_ENTITY_KIND,
        source_entity_id: item_id,
        correlation_id: Some(item_id),
    })
}

pub fn build_outbox_job_for_source(
    input: AgentLaunchOutboxJobInput<'_>,
) -> Result<NewOutboxJob, serde_json::Error> {
    let payload = AgentLaunchOutboxPayload {
        monitor_url: input.monitor_url.to_string(),
        display_name: input.display_name.to_string(),
        initial_prompt: input.initial_prompt.to_string(),
        work_dir: effective_work_dir(input.work_dir).to_string(),
    };
    Ok(NewOutboxJob {
        job_id: job_id(input.source_id, input.idempotency_key),
        provider: PROVIDER_AGENT_MONITOR.to_string(),
        capability: CAPABILITY_LAUNCH_AGENT.to_string(),
        payload_json: serde_json::to_string(&payload)?,
        source_entity_kind: input.source_entity_kind.to_string(),
        source_entity_id: input.source_entity_id.to_string(),
        correlation_id: input.correlation_id.map(str::to_string),
        causation_id: None,
        idempotency_key: input.idempotency_key.to_string(),
    })
}

pub fn deliver(job: &ClaimedJob, _now_ms: u64) -> AttemptOutcome {
    if job.provider != PROVIDER_AGENT_MONITOR || job.capability != CAPABILITY_LAUNCH_AGENT {
        return AttemptOutcome::Terminal {
            error: format!("outbox_unsupported_job:{}:{}", job.provider, job.capability),
            result_json: None,
        };
    }
    let payload = match serde_json::from_str::<AgentLaunchOutboxPayload>(&job.payload_json) {
        Ok(payload) => payload,
        Err(err) => {
            return AttemptOutcome::Terminal {
                error: format!("work_queue_agent_payload_invalid:{err}"),
                result_json: None,
            }
        }
    };
    let token = crate::env_registry::string(&crate::env_registry::BOS_DEBUG_AGENT_MONITOR_TOKEN);
    match post_monitor_agent_session(
        &payload.monitor_url,
        token.as_deref(),
        &job.idempotency_key,
        &payload.display_name,
        &payload.initial_prompt,
        &payload.work_dir,
    ) {
        Ok(response) => AttemptOutcome::Delivered {
            result_json: serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string()),
        },
        // Session creation is not proven idempotent on the monitor side. A
        // transport error after the monitor accepted the POST could otherwise
        // be retried by the outbox pump and create duplicate sessions.
        Err(MonitorError::Request(_)) => AttemptOutcome::Terminal {
            error: "work_queue_agent_monitor_request_failed".to_string(),
            result_json: None,
        },
        Err(err) => AttemptOutcome::Terminal {
            error: err.code().to_string(),
            result_json: None,
        },
    }
}

/// Assemble the initial prompt: a stable header of work-item facts, the source
/// message when one resolves, then the operator's optional notes.
pub fn build_prompt(
    client_id: &str,
    item: &WorkItem,
    source: Option<&WorkItemSourceResponse>,
    operator_context: &str,
    work_dir: &str,
) -> String {
    let work_dir = effective_work_dir(work_dir);
    let mut prompt = format!(
        "Agent session: BusinessOS work item\n\
         Workdir: {work_dir}\n\
         Client: {client_id}\n\
         Work item: {item_id}\n\
         Category: {category}\n\
         Title: {title}\n\
         Summary: {summary}\n\
         Suggested outputs: {kinds}\n\
         Source kind: {source_kind}\n",
        work_dir = work_dir,
        item_id = item.item_id,
        category = item.category_id,
        title = item.title,
        summary = item.summary,
        kinds = if item.packet_kinds.is_empty() {
            "-".to_string()
        } else {
            item.packet_kinds.join(", ")
        },
        source_kind = item.source_kind,
    );

    if let Some(source) = source {
        let message: &InboundMessageRecord = &source.message;
        let body = if message.body_full.trim().is_empty() {
            message.body_excerpt.as_str()
        } else {
            message.body_full.as_str()
        };
        prompt.push_str(&format!(
            "From: {from}\n\
             Subject: {subject}\n\
             --- Source message ---\n{body}\n",
            from = message.from_addr.as_deref().unwrap_or("-"),
            subject = message.subject.as_deref().unwrap_or("-"),
            body = body.trim(),
        ));
        if !message.attachments.is_empty() {
            prompt.push_str("\n--- Source attachments ---\n");
            for attachment in &message.attachments {
                prompt.push_str(&format!(
                    "- {filename} ({mime}, {size}; attachment_id={attachment_id})\n",
                    filename = attachment.filename,
                    mime = attachment.mime_type.as_deref().unwrap_or("unknown mime"),
                    size = attachment
                        .size_bytes
                        .map(|bytes| format!("{bytes} bytes"))
                        .unwrap_or_else(|| "unknown size".to_string()),
                    attachment_id = attachment.attachment_id,
                ));
            }
            prompt.push_str(&format!(
                "To inspect an attachment, stage it through BusinessOS: POST \
                 /api/email-triage/inbox/{message_id}/attachments/<attachment_id>/evidence \
                 with this agent session id, then read the returned local evidence path.\n",
                message_id = message.source_key,
            ));
        }
        if item.source_kind == "email" {
            prompt.push_str(
                "\n--- Required email thread check ---\n\
                 Before you create an action, task, or email draft, inspect the current Gmail thread. \
                 Use bos_email_thread_read with the source_ref when the BusinessOS MCP server is available. \
                 Otherwise, inspect the full thread with an available read-only Gmail tool. \
                 If a sent message follows the source email, determine whether it already resolves the request. \
                 Usually, do not create another reply draft after a response was sent. \
                 Do not assume the inbox message is still unanswered when you cannot verify the thread.\n",
            );
        }
    }

    let notes = operator_context.trim();
    prompt.push_str(&format!(
        "\n--- Operator context ---\n{}\n",
        if notes.is_empty() {
            "(none provided)"
        } else {
            notes
        }
    ));
    if let Some(config) = crate::slices::agent_mcp::service::monitor_mcp_server_config() {
        let url = config.get("url").and_then(Value::as_str).unwrap_or("");
        prompt.push_str(&format!(
            "\n--- Optional BusinessOS MCP ---\n\
             This session was launched with BusinessOS context. If the launcher seeded \
             the `businessos` MCP server, use it for BOS reads, notes, queue artifacts, \
             and draft staging only. Do not approve drafts, send email, publish content, \
             or write providers through MCP. Server URL: {url}. Authentication must be \
             supplied by the operator/Fleet config; do not ask for or print tokens.\n",
        ));
    }

    prompt.push_str(
        "\nAct on this work item. Use the repo instructions, preserve receipts/outbox \
         invariants, and report what you changed and verified.",
    );
    prompt
}

/// POST a new agent session to the Agent Monitor and parse the session/thread
/// ids out of its response. Blocking — call from `spawn_blocking`.
pub fn post_monitor_agent_session(
    monitor_url: &str,
    token: Option<&str>,
    idempotency_key: &str,
    display_name: &str,
    initial_prompt: &str,
    work_dir: &str,
) -> Result<LaunchAgentResponse, MonitorError> {
    let body =
        monitor_session_request_body(idempotency_key, display_name, initial_prompt, work_dir);
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(MonitorError::ClientBuild)?;
    let mut request = client
        .post(format!("{monitor_url}/api/agents/sessions"))
        .json(&body);
    if let Some(token) = token.map(str::trim).filter(|value| !value.is_empty()) {
        request = request.bearer_auth(token);
    }
    let response = request.send().map_err(MonitorError::Request)?;
    let status = response.status();
    let value = response
        .json::<Value>()
        .unwrap_or_else(|_| serde_json::json!({ "invalid_json": true }));
    if !status.is_success() {
        tracing::warn!(status = %status, body = %value, "work item agent monitor rejected launch");
        return Err(MonitorError::Rejected);
    }
    let session_id = value
        .pointer("/result/session/id")
        .or_else(|| value.pointer("/session/id"))
        .and_then(Value::as_str)
        .or_else(|| value.pointer("/result/sessionId").and_then(Value::as_str))
        .ok_or(MonitorError::InvalidResponse)?
        .to_string();
    let thread_id = value
        .pointer("/result/threadId")
        .or_else(|| value.pointer("/result/thread/id"))
        .or_else(|| value.pointer("/thread/id"))
        .or_else(|| value.pointer("/threadId"))
        .or_else(|| value.pointer("/session/threadId"))
        .and_then(Value::as_str)
        .map(str::to_string);
    Ok(LaunchAgentResponse {
        session_id,
        thread_id,
        monitor_url: Some(monitor_url.to_string()),
        staged_evidence_paths: Vec::new(),
    })
}

fn monitor_session_request_body(
    idempotency_key: &str,
    display_name: &str,
    initial_prompt: &str,
    work_dir: &str,
) -> Value {
    let mut body = serde_json::json!({
        "provider": "codex",
        "executor": "tmux",
        "workDir": effective_work_dir(work_dir),
        "displayName": display_name,
        "initialPrompt": initial_prompt,
        "idempotencyKey": idempotency_key,
    });
    if let Some(config) = crate::slices::agent_mcp::service::monitor_mcp_server_config() {
        body["mcpServers"] = serde_json::json!({
            "businessos": config,
        });
        body["metadata"] = serde_json::json!({
            "businessOsMcp": {
                "serverName": "businessos",
                "requiresExplicitSelection": true,
                "authorization": "operator_bearer_token_required",
            }
        });
    }
    body
}

pub fn effective_work_dir(raw: &str) -> &str {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        DEFAULT_AGENT_WORK_DIR
    } else {
        trimmed
    }
}

pub fn resolve_work_dir<'a>(
    request_work_dir: Option<&'a str>,
    category_work_dir: &'a str,
) -> &'a str {
    request_work_dir
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(category_work_dir)
}

pub fn combine_context(category_default: &str, request_context: &str) -> String {
    let category_default = category_default.trim();
    let request_context = request_context.trim();
    match (category_default.is_empty(), request_context.is_empty()) {
        (true, true) => String::new(),
        (false, true) => category_default.to_string(),
        (true, false) => request_context.to_string(),
        (false, false) => format!(
            "--- Category default context ---\n{category_default}\n\n--- Launch override context ---\n{request_context}"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bos_contracts::email_triage::{EmailAttachmentRecord, InboundMessageRecord};
    use bos_contracts::work_queue::WorkItemStatus;

    fn item() -> WorkItem {
        WorkItem {
            item_id: "wi_email_m1".into(),
            source_kind: "email".into(),
            source_ref: "m1".into(),
            category_id: "wythe_hotel".into(),
            title: "Invoice question".into(),
            summary: "They ask about line item 3".into(),
            packet_kinds: vec!["email_draft_reply".into()],
            status: WorkItemStatus::Open,
            accept_actor: None,
            ai_suggested: false,
            rationale: String::new(),
            produce_guidance: String::new(),
            source_user_id: None,
            assignee_user_id: None,
            visible_to_user_ids: Vec::new(),
            created_at_ms: 1,
            updated_at_ms: 1,
        }
    }

    #[test]
    fn prompt_includes_item_facts_and_operator_notes() {
        let prompt = build_prompt(
            "Example Company",
            &item(),
            None,
            "look at ~/projects/wythe",
            "/tmp/wythe",
        );
        assert!(prompt.contains("Work item: wi_email_m1"));
        assert!(prompt.contains("Workdir: /tmp/wythe"));
        assert!(prompt.contains("Category: wythe_hotel"));
        assert!(prompt.contains("Suggested outputs: email_draft_reply"));
        assert!(prompt.contains("look at ~/projects/wythe"));
    }

    #[test]
    fn prompt_marks_absent_operator_context() {
        let prompt = build_prompt("Example Company", &item(), None, "   ", "");
        assert!(prompt.contains("(none provided)"));
    }

    #[test]
    fn prompt_includes_source_attachment_metadata_and_stage_endpoint() {
        let source = WorkItemSourceResponse {
            source_kind: "email".to_string(),
            source_body: "See attached.".to_string(),
            source_body_format: bos_contracts::work_queue::WorkItemSourceBodyFormat::PlainText,
            message: InboundMessageRecord {
                source_key: "m1".to_string(),
                message_id: "m1".to_string(),
                thread_id: None,
                internal_date_ms: None,
                from_addr: Some("buyer@example.com".to_string()),
                to_addr: None,
                subject: Some("Quote files".to_string()),
                body_excerpt: "See attached.".to_string(),
                body_full: "See attached.".to_string(),
                headers: Vec::new(),
                labels: Vec::new(),
                resolved_category: "quote".to_string(),
                matched_rule_id: None,
                ingested_at_ms: 1,
                ai_triage_status: None,
                ai_triage_rationale: None,
                attachments: vec![EmailAttachmentRecord {
                    attachment_id: "att-1".to_string(),
                    part_id: Some("1".to_string()),
                    filename: "spec.pdf".to_string(),
                    mime_type: Some("application/pdf".to_string()),
                    size_bytes: Some(1234),
                    inline: false,
                    content_id: None,
                }],
                source_user_id: None,
            },
        };
        let prompt = build_prompt("Example Company", &item(), Some(&source), "", "");
        assert!(prompt.contains("--- Source attachments ---"));
        assert!(prompt.contains("spec.pdf (application/pdf, 1234 bytes; attachment_id=att-1)"));
        assert!(prompt.contains("/api/email-triage/inbox/m1/attachments/<attachment_id>/evidence"));
        assert!(prompt.contains("--- Required email thread check ---"));
        assert!(prompt.contains("bos_email_thread_read"));
        assert!(prompt.contains("Usually, do not create another reply draft"));
    }

    #[test]
    fn monitor_request_body_carries_idempotency_key() {
        let body = monitor_session_request_body("launch-1", "Work item", "prompt", "/tmp/bos");
        assert_eq!(
            body.pointer("/idempotencyKey")
                .and_then(serde_json::Value::as_str),
            Some("launch-1")
        );
        assert_eq!(
            body.pointer("/workDir").and_then(serde_json::Value::as_str),
            Some("/tmp/bos")
        );
    }

    #[test]
    fn combines_category_default_and_request_context() {
        let combined = combine_context("Use the IMS repo", "Focus claims first");
        assert!(combined.contains("Category default context"));
        assert!(combined.contains("Use the IMS repo"));
        assert!(combined.contains("Launch override context"));
        assert!(combined.contains("Focus claims first"));
        assert_eq!(combine_context("  Use IMS  ", "  "), "Use IMS");
    }

    #[test]
    fn resolves_work_dir_request_then_category_then_platform_default() {
        assert_eq!(
            effective_work_dir(resolve_work_dir(Some("/tmp/request"), "/tmp/category")),
            "/tmp/request"
        );
        assert_eq!(
            effective_work_dir(resolve_work_dir(Some("  "), "/tmp/category")),
            "/tmp/category"
        );
        assert_eq!(
            effective_work_dir(resolve_work_dir(None, "  ")),
            DEFAULT_AGENT_WORK_DIR
        );
    }
}
