//! Store lifecycle, secret hygiene, and fail-closed HTTP capability gates.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use bos_contracts::operator_api_tokens::OperatorApiToken;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::store::{self, TokenActionContext};
use crate::http::build_router;
use crate::http::test_support::{test_state_configured, EnvGuard};
use crate::persistence::Persistence;
use crate::store_core::StoreError;

const CLIENT: &str = "test-client";
const FULL: &str = "full-operator-token";
const BRIDGE_SECRET: &str = "bost_bridge_secret_for_tests";
const AGENT_SECRET: &str = "bost_agent_secret_for_tests";

fn meta(
    token_id: &str,
    label: &str,
    capabilities: &[&str],
    created_at_ms: u64,
) -> OperatorApiToken {
    OperatorApiToken {
        token_id: token_id.to_string(),
        label: label.to_string(),
        capabilities: capabilities.iter().map(|cap| cap.to_string()).collect(),
        active: true,
        revoked_at_ms: None,
        created_by: "operator".to_string(),
        created_at_ms,
        updated_at_ms: created_at_ms,
    }
}

fn ctx(key: &'static str) -> TokenActionContext<'static> {
    TokenActionContext {
        client_id: CLIENT,
        actor_id: "operator",
        expected_revision: None,
        idempotency_key: key,
        now_ms: 5_000,
    }
}

fn seed_token(
    conn: &mut rusqlite::Connection,
    token: &OperatorApiToken,
    secret: &str,
    idempotency_key: &str,
) {
    store::create_token(
        conn,
        CLIENT,
        "operator",
        token,
        &store::token_hash(secret),
        idempotency_key,
    )
    .expect("create token");
}

#[test]
fn hashed_lookup_and_receipts_never_carry_the_secret() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let token = meta(
        "apitok_bridge",
        "Slack bridge",
        &[
            "social_publishing:read",
            "social_publishing:update",
            "social_publishing:approve",
            "agent_mcp:ingest",
        ],
        1_000,
    );
    seed_token(conn, &token, BRIDGE_SECRET, "c1");

    let found = store::find_active_by_token_hash(conn, CLIENT, &store::token_hash(BRIDGE_SECRET))
        .expect("lookup")
        .expect("found");
    assert_eq!(found.token_id, "apitok_bridge");
    assert_eq!(found.label, "Slack bridge");
    assert!(
        store::find_active_by_token_hash(conn, CLIENT, &store::token_hash("wrong"))
            .expect("miss")
            .is_none()
    );

    store::set_active(conn, ctx("d1"), "apitok_bridge", false).expect("disable");
    assert!(
        store::find_active_by_token_hash(conn, CLIENT, &store::token_hash(BRIDGE_SECRET))
            .expect("disabled")
            .is_none()
    );

    let receipts = crate::store_core::receipts_for_entity(
        persistence.connection_ref(),
        CLIENT,
        store::TOKEN_ENTITY_KIND,
        "apitok_bridge",
        10,
    )
    .expect("receipts");
    assert!(!receipts.is_empty());
    for receipt in &receipts {
        let dump = serde_json::to_string(receipt).expect("json");
        assert!(
            !dump.contains(BRIDGE_SECRET) && !dump.contains("bost_"),
            "receipt leaked a token: {dump}"
        );
        assert!(
            !dump.contains(&store::token_hash(BRIDGE_SECRET)),
            "receipt leaked a token hash: {dump}"
        );
    }
}

#[test]
fn revoke_is_terminal_and_kills_the_hash() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    seed_token(
        conn,
        &meta("apitok_agent", "Agent", &["agent_mcp:ingest"], 1_000),
        AGENT_SECRET,
        "c1",
    );
    store::revoke_token(conn, ctx("r1"), "apitok_agent").expect("revoke");
    assert!(
        store::find_active_by_token_hash(conn, CLIENT, &store::token_hash(AGENT_SECRET))
            .expect("lookup")
            .is_none()
    );
    let err = store::rotate_token(conn, ctx("rot"), "apitok_agent", "new-hash")
        .expect_err("revoked rotate");
    assert!(matches!(err, StoreError::Domain(code) if code == "operator_api_token_revoked"));
}

fn router_with_tokens() -> (axum::Router, crate::http::AppState) {
    let state = test_state_configured(
        Some(FULL),
        &[
            "operator_api_tokens",
            "operator_users",
            "operator_notes",
            "social_publishing",
            "agent_mcp",
        ],
    );
    {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        seed_token(
            conn,
            &meta(
                "apitok_bridge",
                "Slack bridge",
                &[
                    "social_publishing:read",
                    "social_publishing:update",
                    "social_publishing:approve",
                    "agent_mcp:ingest",
                ],
                1_000,
            ),
            BRIDGE_SECRET,
            "c-bridge",
        );
        seed_token(
            conn,
            &meta("apitok_agent", "Ingest agent", &["agent_mcp:ingest"], 1_000),
            AGENT_SECRET,
            "c-agent",
        );
    }
    (build_router(state.clone()), state)
}

async fn json_request(
    router: axum::Router,
    method: Method,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> axum::response::Response {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let body = if let Some(body) = body {
        builder = builder.header("content-type", "application/json");
        Body::from(body.to_string())
    } else {
        Body::empty()
    };
    router
        .oneshot(builder.body(body).expect("request"))
        .await
        .expect("response")
}

async fn response_json(response: axum::response::Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

#[tokio::test]
async fn unscoped_token_still_reaches_operator_admin_and_notes() {
    let (router, _) = router_with_tokens();
    let (status, _) = response_json(
        json_request(
            router.clone(),
            Method::GET,
            "/api/operator-notes",
            Some(FULL),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = response_json(
        json_request(
            router,
            Method::GET,
            "/api/operator-tokens",
            Some(FULL),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["tokens"].as_array().expect("tokens").len(), 2);
}

#[tokio::test]
async fn scoped_tokens_are_forbidden_outside_their_capabilities() {
    let (router, _) = router_with_tokens();
    let denials = [
        (BRIDGE_SECRET, Method::GET, "/api/operator-notes", None),
        (BRIDGE_SECRET, Method::GET, "/api/users", None),
        (BRIDGE_SECRET, Method::GET, "/api/operator-tokens", None),
        (
            BRIDGE_SECRET,
            Method::POST,
            "/api/social-publishing/proposals",
            Some(json!({
                "canonical_url": "https://example.com/post",
                "targets": [],
                "idempotency_key": "stage-denied"
            })),
        ),
        (
            AGENT_SECRET,
            Method::GET,
            "/api/social-publishing/proposals",
            None,
        ),
        (AGENT_SECRET, Method::GET, "/api/operator-notes", None),
    ];
    for (token, method, path, body) in denials {
        let (status, body_json) =
            response_json(json_request(router.clone(), method, path, Some(token), body).await)
                .await;
        assert_eq!(
            (path, status, body_json["error"].as_str()),
            (
                path,
                StatusCode::FORBIDDEN,
                Some("operator_capability_denied")
            )
        );
    }
}

#[tokio::test]
async fn bridge_token_can_list_social_proposals_and_whoami_exposes_capabilities() {
    let (router, _) = router_with_tokens();
    let (status, _) = response_json(
        json_request(
            router.clone(),
            Method::GET,
            "/api/social-publishing/proposals",
            Some(BRIDGE_SECRET),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = response_json(
        json_request(router, Method::GET, "/api/me", Some(BRIDGE_SECRET), None).await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["actor_id"], "operator");
    assert_eq!(body["display_name"], "Slack bridge");
    let caps = body["capabilities"]
        .as_array()
        .expect("capabilities")
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    assert!(caps.contains(&"social_publishing:read"));
    assert!(caps.contains(&"agent_mcp:ingest"));
}

#[tokio::test]
async fn scoped_token_cannot_open_a_browser_session_or_mint_another_token() {
    let (router, _) = router_with_tokens();
    let (status, body) = response_json(
        json_request(
            router.clone(),
            Method::POST,
            "/api/session",
            None,
            Some(json!({ "token": BRIDGE_SECRET })),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "operator_capability_denied");

    let (status, body) = response_json(
        json_request(
            router,
            Method::POST,
            "/api/operator-tokens",
            Some(BRIDGE_SECRET),
            Some(json!({
                "label": "forged",
                "capabilities": ["agent_mcp:ingest"],
                "idempotency_key": "forged"
            })),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "operator_capability_denied");
}

#[tokio::test]
async fn mint_rejects_empty_and_unknown_capabilities() {
    let (router, _) = router_with_tokens();
    let (status, body) = response_json(
        json_request(
            router.clone(),
            Method::POST,
            "/api/operator-tokens",
            Some(FULL),
            Some(json!({
                "label": "empty",
                "capabilities": [],
                "idempotency_key": "empty-caps"
            })),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "operator_capabilities_required");

    let (status, body) = response_json(
        json_request(
            router,
            Method::POST,
            "/api/operator-tokens",
            Some(FULL),
            Some(json!({
                "label": "wildcard",
                "capabilities": ["*"],
                "idempotency_key": "star"
            })),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "operator_capability_unknown");
}

#[tokio::test]
async fn mint_returns_secret_once_and_unscoped_whoami_omits_capabilities() {
    let (router, _) = router_with_tokens();
    let (status, body) = response_json(
        json_request(
            router.clone(),
            Method::POST,
            "/api/operator-tokens",
            Some(FULL),
            Some(json!({
                "label": "Agent ingest",
                "capabilities": ["agent_mcp:ingest"],
                "idempotency_key": "mint-agent"
            })),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let secret = body["token"].as_str().expect("secret").to_string();
    assert!(secret.starts_with("bost_"));
    assert_eq!(
        body["api_token"]["capabilities"],
        json!(["agent_mcp:ingest"])
    );

    let (status, me) =
        response_json(json_request(router, Method::GET, "/api/me", Some(FULL), None).await).await;
    assert_eq!(status, StatusCode::OK);
    assert!(me.get("capabilities").is_none());
    assert_eq!(me["actor_id"], "operator");
}

#[tokio::test]
async fn ingest_only_mcp_hides_other_tools_and_denies_them() {
    let _env = EnvGuard::set("BOS_AGENT_MCP_ENABLED", "1");
    let (router, _) = router_with_tokens();
    let (status, body) = response_json(
        json_request(
            router.clone(),
            Method::POST,
            "/api/agent-mcp",
            Some(AGENT_SECRET),
            Some(json!({
                "jsonrpc": "2.0",
                "id": "tools",
                "method": "tools/list"
            })),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let names = body["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["bos_social_published_content_ingest"]);

    let (status, body) = response_json(
        json_request(
            router.clone(),
            Method::POST,
            "/api/agent-mcp",
            Some(AGENT_SECRET),
            Some(json!({
                "jsonrpc": "2.0",
                "id": "call",
                "method": "tools/call",
                "params": {
                    "name": "bos_work_queue_list",
                    "arguments": {}
                }
            })),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["error"]["data"]["code"], "operator_capability_denied");

    let (status, _) = response_json(
        json_request(
            router,
            Method::POST,
            "/api/agent-mcp",
            Some(BRIDGE_SECRET),
            Some(json!({
                "jsonrpc": "2.0",
                "id": "tools",
                "method": "tools/list"
            })),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn social_only_token_without_ingest_cannot_post_mcp() {
    let _env = EnvGuard::set("BOS_AGENT_MCP_ENABLED", "1");
    let state = test_state_configured(Some(FULL), &["operator_api_tokens", "agent_mcp"]);
    {
        let mut persistence = state.persistence.lock();
        seed_token(
            persistence.connection(),
            &meta(
                "apitok_social",
                "Social read",
                &["social_publishing:read"],
                1_000,
            ),
            "bost_social_only",
            "c-social",
        );
    }
    let router = build_router(state);
    let (status, body) = response_json(
        json_request(
            router,
            Method::POST,
            "/api/agent-mcp",
            Some("bost_social_only"),
            Some(json!({
                "jsonrpc": "2.0",
                "id": "tools",
                "method": "tools/list"
            })),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "operator_capability_denied");
}
