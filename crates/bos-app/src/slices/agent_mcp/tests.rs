use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::http::{
    build_router,
    test_support::{test_state_configured, EnvGuard},
    AuthContext, OperatorIdentity,
};

fn shared_auth() -> AuthContext {
    let identity = OperatorIdentity {
        actor_id: crate::http::SHARED_OPERATOR_ACTOR.to_string(),
        display_name: "Operator".to_string(),
    };
    AuthContext {
        scope: identity.scope(),
        actor_id: identity.actor_id.clone(),
        identity,
    }
}

fn user_auth(user_id: &str) -> AuthContext {
    let identity = OperatorIdentity {
        actor_id: user_id.to_string(),
        display_name: user_id.to_string(),
    };
    AuthContext {
        scope: identity.scope(),
        actor_id: identity.actor_id.clone(),
        identity,
    }
}

async fn response_json(response: axum::response::Response) -> Value {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("json")
}

#[tokio::test]
async fn manifest_is_discoverable_when_mcp_is_disabled() {
    let _env = EnvGuard::unset("BOS_AGENT_MCP_ENABLED");
    let router = build_router(test_state_configured(None, &["agent_mcp"]));
    let response = router
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/agent-mcp")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["name"], "businessos");
    assert_eq!(body["enabled"], false);
    assert_eq!(body["injection"], "explicit_bos_context_only");
}

#[tokio::test]
async fn post_is_404_until_enabled() {
    let _env = EnvGuard::unset("BOS_AGENT_MCP_ENABLED");
    let router = build_router(test_state_configured(None, &["agent_mcp"]));
    let response = router
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/agent-mcp")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }).to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn tools_list_uses_mcp_protocol_when_enabled() {
    let _env = EnvGuard::set("BOS_AGENT_MCP_ENABLED", "1");
    let router = build_router(test_state_configured(
        Some("test-token"),
        &["agent_mcp", "work_queue"],
    ));
    let response = router
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/agent-mcp")
                .header("content-type", "application/json")
                .header("authorization", "Bearer test-token")
                .body(Body::from(
                    json!({ "jsonrpc": "2.0", "id": "tools", "method": "tools/list" }).to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let tools = body["result"]["tools"].as_array().expect("tools");
    assert!(tools
        .iter()
        .any(|tool| tool["name"] == "bos_work_queue_list"));
    assert!(tools
        .iter()
        .any(|tool| tool["name"] == "bos_agent_result_ingest"));
}

#[tokio::test]
async fn stateless_discovery_and_tool_list_use_modern_protocol_envelope() {
    let _env = EnvGuard::set("BOS_AGENT_MCP_ENABLED", "1");
    let router = build_router(test_state_configured(
        Some("test-token"),
        &["agent_mcp", "work_queue"],
    ));
    let meta = json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {},
        "io.modelcontextprotocol/clientInfo": { "name": "bos-test", "version": "1.0.0" }
    });

    let discover = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/agent-mcp")
                .header("content-type", "application/json")
                .header("authorization", "Bearer test-token")
                .header("mcp-protocol-version", "2026-07-28")
                .header("mcp-method", "server/discover")
                .body(Body::from(
                    json!({
                        "jsonrpc": "2.0",
                        "id": "discover",
                        "method": "server/discover",
                        "params": { "_meta": meta.clone() }
                    })
                    .to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(discover.status(), StatusCode::OK);
    assert_eq!(
        discover.headers().get("mcp-session-id"),
        None,
        "stateless responses must not mint transport sessions"
    );
    let body = response_json(discover).await;
    assert_eq!(body["result"]["supportedVersions"], json!(["2026-07-28"]));
    assert_eq!(body["result"]["resultType"], "complete");
    assert_eq!(
        body["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "businessos"
    );

    let tools = router
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/agent-mcp")
                .header("content-type", "application/json")
                .header("authorization", "Bearer test-token")
                .header("mcp-protocol-version", "2026-07-28")
                .header("mcp-method", "tools/list")
                .body(Body::from(
                    json!({
                        "jsonrpc": "2.0",
                        "id": "tools",
                        "method": "tools/list",
                        "params": { "_meta": meta }
                    })
                    .to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(tools.status(), StatusCode::OK);
    let body = response_json(tools).await;
    assert_eq!(body["result"]["ttlMs"], 300_000);
    assert_eq!(body["result"]["cacheScope"], "private");
}

#[tokio::test]
async fn stateless_requests_reject_header_body_mismatch() {
    let _env = EnvGuard::set("BOS_AGENT_MCP_ENABLED", "1");
    let router = build_router(test_state_configured(Some("test-token"), &["agent_mcp"]));
    let response = router
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/agent-mcp")
                .header("content-type", "application/json")
                .header("authorization", "Bearer test-token")
                .header("mcp-protocol-version", "2026-07-28")
                .header("mcp-method", "tools/list")
                .body(Body::from(
                    json!({
                        "jsonrpc": "2.0",
                        "id": "discover",
                        "method": "server/discover",
                        "params": {
                            "_meta": {
                                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                                "io.modelcontextprotocol/clientCapabilities": {}
                            }
                        }
                    })
                    .to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response_json(response).await;
    assert_eq!(body["error"]["code"], -32020);
}

#[tokio::test]
async fn post_refuses_open_dev_mode_when_enabled_without_credentials() {
    let _env = EnvGuard::set("BOS_AGENT_MCP_ENABLED", "1");
    let router = build_router(test_state_configured(None, &["agent_mcp", "work_queue"]));
    let response = router
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/agent-mcp")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "jsonrpc": "2.0", "id": "tools", "method": "tools/list" }).to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[test]
fn operator_note_create_ignores_actor_id_and_stamps_mcp_provenance() {
    let state = test_state_configured(None, &[]);
    let result = super::service::test_call_tool(
        state.clone(),
        shared_auth(),
        "bos_operator_note_create",
        json!({
            "body": "Review suggested CRM cleanup.",
            "actor_id": "user_jordan",
            "idempotency_key": "note-spoof-1"
        }),
    )
    .expect("note create");
    assert_eq!(
        result["structuredContent"]["note"]["note_id"],
        json!("note_mcp_note-spoof-1")
    );
    assert_eq!(
        result["structuredContent"]["note"]["created_by"],
        json!("mcp:operator")
    );

    let persistence = state.persistence.lock();
    let receipts = crate::store_core::receipts_for_entity(
        persistence.connection_ref(),
        &state.client_id,
        crate::slices::operator_notes::store::NOTE_ENTITY_KIND,
        "note_mcp_note-spoof-1",
        10,
    )
    .expect("receipts");
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].actor_id, "mcp:operator");
    assert_eq!(receipts[0].idempotency_key, "mcp_note:note-spoof-1");
}

#[test]
fn agent_result_ingest_creates_one_note_and_queue_artifact_idempotently() {
    let state = test_state_configured(None, &[]);
    let first = super::service::test_call_tool(
        state.clone(),
        shared_auth(),
        "bos_agent_result_ingest",
        json!({
            "summary": "Customer needs a follow-up call.",
            "details": "Found the requested callback in the source email.",
            "source_item_id": "wi_email_m1",
            "agent_session_id": "codex_test",
            "idempotency_key": "agent-result-1"
        }),
    )
    .expect("first ingest");
    assert_eq!(first["structuredContent"]["work_item_emitted"], json!(true));

    let replay = super::service::test_call_tool(
        state.clone(),
        shared_auth(),
        "bos_agent_result_ingest",
        json!({
            "summary": "Customer needs a follow-up call.",
            "idempotency_key": "agent-result-1"
        }),
    )
    .expect("replay ingest");
    assert_eq!(
        replay["structuredContent"]["work_item_emitted"],
        json!(false)
    );

    let persistence = state.persistence.lock();
    let note_count =
        super::service::count_notes(persistence.connection_ref(), &state.client_id).expect("count");
    assert_eq!(note_count, 1);
    let item = crate::slices::work_queue::store::get_item_for_source(
        persistence.connection_ref(),
        &state.client_id,
        crate::slices::work_queue::SOURCE_KIND_OPERATOR_NOTE,
        "note_mcp_agent-result-1",
    )
    .expect("item")
    .expect("item exists");
    assert_eq!(item.item.source_ref, "note_mcp_agent-result-1");
    assert!(item.item.packet_kinds.is_empty());
    let note = crate::slices::operator_notes::store::get_note(
        persistence.connection_ref(),
        &state.client_id,
        "note_mcp_agent-result-1",
    )
    .expect("note")
    .expect("note exists");
    assert_eq!(note.created_by, "mcp:operator");
}

#[test]
fn email_thread_tool_enforces_operator_scope_before_gmail_access() {
    let _env = EnvGuard::unset("BOS_GMAIL_OAUTH_REFRESH_TOKEN");
    let state = test_state_configured(None, &["email_triage"]);
    let message = bos_contracts::email_triage::InboundMessageRecord {
        source_key: "source-jordan".to_string(),
        message_id: "message-jordan".to_string(),
        thread_id: Some("thread-jordan".to_string()),
        internal_date_ms: Some(1_000),
        from_addr: Some("customer@example.test".to_string()),
        to_addr: Some("jordan@example.test".to_string()),
        subject: Some("Question".to_string()),
        body_excerpt: "Please help.".to_string(),
        body_full: "Please help.".to_string(),
        headers: Vec::new(),
        labels: vec!["INBOX".to_string()],
        resolved_category: "inbound_email".to_string(),
        matched_rule_id: None,
        ingested_at_ms: 1_000,
        ai_triage_status: None,
        ai_triage_rationale: None,
        attachments: Vec::new(),
        source_user_id: Some("user_jordan".to_string()),
    };
    {
        let mut persistence = state.persistence.lock();
        crate::slices::email_triage::store::record_inbound_message(
            persistence.connection(),
            &state.client_id,
            &message,
        )
        .expect("store inbound message");
    }

    let hidden = super::service::test_call_tool(
        state.clone(),
        user_auth("user_casey"),
        "bos_email_thread_read",
        json!({ "source_ref": "source-jordan" }),
    )
    .expect_err("another user's source must stay hidden");
    assert_eq!(hidden, "email_source_not_found");

    let visible = super::service::test_call_tool(
        state,
        user_auth("user_jordan"),
        "bos_email_thread_read",
        json!({ "source_ref": "source-jordan" }),
    )
    .expect_err("missing credential must fail before Gmail access");
    assert_eq!(visible, "gmail_credential_missing");
}

#[test]
fn social_tool_is_published_content_ingress_only_and_stamps_agent_provenance() {
    let _env = EnvGuard::set(
        "BOS_BUFFER_CHANNELS_JSON",
        r#"[{"channel_id":"buf_linkedin","name":"Company LinkedIn","platform":"linkedin"}]"#,
    );
    let state = test_state_configured(None, &["social_publishing"]);
    let manifest = super::service::manifest(&state);
    let tool_names = manifest["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    assert!(tool_names.contains(&"bos_social_published_content_ingest"));
    assert!(!tool_names
        .iter()
        .any(|name| name.contains("proposal_stage")));
    assert!(!tool_names.iter().any(|name| name.contains("approve")));
    let narrowed = super::service::test_call_tool(
        state.clone(),
        shared_auth(),
        "bos_social_published_content_ingest",
        json!({
            "source_kind": "wordpress",
            "external_id": "post-42",
            "canonical_url": "https://example.com/blog/post",
            "title": "Published article",
            "targets": [{"text": "agent-authored copy is forbidden"}],
            "idempotency_key": "agent-social-forbidden"
        }),
    )
    .expect_err("copy fields must be outside the ingress contract");
    assert_eq!(narrowed, "mcp_argument_invalid");
    crate::slices::social_publishing::service::set_test_social_draft_responses(vec![json!({
        "targets": [{
            "target_ref": "target_1",
            "text": "Published article",
            "utm_source": "linkedin",
            "utm_medium": "social",
            "utm_campaign": "blog",
            "utm_content": null,
            "source_quotes": ["Published article"]
        }],
        "confidence": "high"
    })]);
    let result = super::service::test_call_tool(
        state.clone(),
        shared_auth(),
        "bos_social_published_content_ingest",
        json!({
            "source_kind": "wordpress",
            "external_id": "post-42",
            "canonical_url": "https://example.com/blog/post",
            "title": "Published article",
            "idempotency_key": "agent-social-1"
        }),
    )
    .expect("ingest published content");
    assert_eq!(result["structuredContent"]["approval_required"], true);
    assert_eq!(
        result["structuredContent"]["provider_write"],
        "not_performed"
    );
    let source_id = result["structuredContent"]["source"]["source_id"]
        .as_str()
        .expect("source id");
    let persistence = state.persistence.lock();
    let receipts = crate::store_core::receipts_for_entity(
        persistence.connection_ref(),
        &state.client_id,
        crate::slices::social_publishing::store::SOURCE_ENTITY_KIND,
        source_id,
        10,
    )
    .expect("receipts");
    let ingress = receipts
        .iter()
        .find(|receipt| receipt.change_kind == "ingest")
        .expect("ingress receipt");
    assert_eq!(ingress.actor_id, "mcp:operator");
    assert_eq!(
        ingress.actor_kind,
        bos_contracts::receipt::ActorKindDto::Agent
    );
    let job_count: i64 = persistence
        .connection_ref()
        .query_row("SELECT COUNT(*) FROM outbox_jobs", [], |row| row.get(0))
        .expect("jobs");
    assert_eq!(
        job_count, 0,
        "published-content ingress must not enqueue provider writes"
    );
}
