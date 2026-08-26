//! Slice tests: user lifecycle (create/disable/rotate), token lookup, and
//! the authentication seam (shared token vs personal token vs garbage).

use axum::http::HeaderMap;
use axum::http::StatusCode;
use bos_contracts::operator_users::OperatorUser;

use super::store::{self, UserActionContext};
use crate::http::test_support::test_state;
use crate::persistence::{Persistence, PersistencePool};
use crate::store_core::StoreError;

const CLIENT: &str = "test-client";

fn user(user_id: &str, display_name: &str) -> OperatorUser {
    OperatorUser {
        user_id: user_id.to_string(),
        display_name: display_name.to_string(),
        active: true,
        archived_at_ms: None,
        default_calendar_id: None,
        created_at_ms: 1_000,
        updated_at_ms: 1_000,
    }
}

#[test]
fn archive_disabled_user_hides_from_default_list_and_kills_token() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    store::create_user(
        conn,
        CLIENT,
        "operator",
        &user("user_temp", "Temp"),
        "bosu_tok_temp",
        "u1",
    )
    .expect("create");
    store::set_active(conn, ctx("d1"), "user_temp", false).expect("disable");

    store::archive_user(conn, ctx("a1"), "user_temp").expect("archive");

    assert!(store::find_active_by_token(conn, CLIENT, "bosu_tok_temp")
        .expect("lookup")
        .is_none());
    assert!(store::list_users(conn, CLIENT, false)
        .expect("list default")
        .is_empty());
    let archived = store::list_users(conn, CLIENT, true).expect("list archived");
    assert_eq!(archived.len(), 1);
    assert_eq!(archived[0].archived_at_ms, Some(5_000));
    assert!(!archived[0].active);

    let receipts = crate::store_core::receipts_for_entity(
        persistence.connection_ref(),
        CLIENT,
        store::USER_ENTITY_KIND,
        "user_temp",
        10,
    )
    .expect("receipts");
    assert!(receipts
        .iter()
        .any(|receipt| receipt.change_kind == "archive" && receipt.actor_id == "operator"));
    for receipt in &receipts {
        let dump = serde_json::to_string(receipt).expect("receipt json");
        assert!(!dump.contains("bosu_"), "receipt leaked a token: {dump}");
    }
}

#[test]
fn archive_requires_disabled_user() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    store::create_user(
        conn,
        CLIENT,
        "operator",
        &user("user_temp", "Temp"),
        "bosu_tok_temp",
        "u1",
    )
    .expect("create");

    let err = store::archive_user(conn, ctx("a1"), "user_temp").expect_err("active user");
    assert!(
        matches!(err, StoreError::Domain(code) if code == "operator_user_archive_requires_disabled")
    );
}

#[test]
fn archive_is_terminal_for_user_mutations() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    store::create_user(
        conn,
        CLIENT,
        "operator",
        &user("user_temp", "Temp"),
        "bosu_tok_temp",
        "u1",
    )
    .expect("create");
    store::set_active(conn, ctx("d1"), "user_temp", false).expect("disable");
    store::archive_user(conn, ctx("a1"), "user_temp").expect("archive");

    let err = store::rotate_token(conn, ctx("r1"), "user_temp", "bosu_tok_new")
        .expect_err("archived rotation");
    assert!(matches!(err, StoreError::Domain(code) if code == "operator_user_archived"));

    let err = store::set_default_calendar(conn, ctx("c1"), "user_temp", Some("team@calendar"))
        .expect_err("archived calendar edit");
    assert!(matches!(err, StoreError::Domain(code) if code == "operator_user_archived"));

    let err = store::set_active(conn, ctx("e1"), "user_temp", true).expect_err("archived enable");
    assert!(matches!(err, StoreError::Domain(code) if code == "operator_user_archived"));
}

#[test]
fn archive_blocks_users_with_connector_credentials() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    store::create_user(
        conn,
        CLIENT,
        "operator",
        &user("user_temp", "Temp"),
        "bosu_tok_temp",
        "u1",
    )
    .expect("create");
    store::set_active(conn, ctx("d1"), "user_temp", false).expect("disable");

    crate::slices::google_connector::store::store_credential(
        conn,
        CLIENT,
        "user_temp",
        "gmail",
        "refresh-token",
        &["https://www.googleapis.com/auth/gmail.readonly".to_string()],
        2_000,
    )
    .expect("google credential");
    let err = store::archive_user(conn, ctx("a1"), "user_temp").expect_err("google credential");
    assert!(
        matches!(err, StoreError::Domain(code) if code == "operator_user_has_google_credentials")
    );
    crate::slices::google_connector::store::delete_credential(
        conn,
        CLIENT,
        "user_temp",
        "gmail",
        3_000,
    )
    .expect("google disconnect");

    let grant = bos_integrations::qbo_oauth::QboTokenGrant {
        refresh_token: "qbo-refresh".to_string(),
        refresh_token_expires_at_ms: 100_000,
        access_token: "qbo-access".to_string(),
        access_token_expires_at_ms: 50_000,
    };
    crate::slices::accounting::store::store_credential(
        conn,
        CLIENT,
        "realm",
        "sandbox",
        &grant,
        "user_temp",
        4_000,
    )
    .expect("qbo credential");
    let err = store::archive_user(conn, ctx("a2"), "user_temp").expect_err("qbo credential");
    assert!(matches!(err, StoreError::Domain(code) if code == "operator_user_has_qbo_credential"));
    crate::slices::accounting::store::delete_credential(conn, CLIENT, "operator", true, 5_000)
        .expect("qbo disconnect");

    store::archive_user(conn, ctx("a3"), "user_temp").expect("archive after disconnects");
}

fn ctx<'a>(key: &'a str) -> UserActionContext<'a> {
    UserActionContext {
        client_id: CLIENT,
        actor_id: "operator",
        expected_revision: None,
        idempotency_key: key,
        now_ms: 5_000,
    }
}

#[test]
fn create_lookup_disable_rotate_lifecycle() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    store::create_user(
        conn,
        CLIENT,
        "operator",
        &user("user_jordan", "Jordan"),
        "bosu_tok_jordan",
        "u1",
    )
    .expect("create");

    // Duplicate id and duplicate token both refuse.
    let err = store::create_user(
        conn,
        CLIENT,
        "operator",
        &user("user_jordan", "Jordan"),
        "bosu_other",
        "u2",
    )
    .expect_err("dup id");
    assert!(matches!(err, StoreError::Domain(code) if code == "operator_user_exists"));

    let found = store::find_active_by_token(conn, CLIENT, "bosu_tok_jordan")
        .expect("lookup")
        .expect("found");
    assert_eq!(found.user_id, "user_jordan");
    assert!(store::find_active_by_token(conn, CLIENT, "bosu_wrong")
        .expect("lookup")
        .is_none());

    // Disable kills the token immediately; enable restores it.
    store::set_active(conn, ctx("d1"), "user_jordan", false).expect("disable");
    assert!(store::find_active_by_token(conn, CLIENT, "bosu_tok_jordan")
        .expect("lookup")
        .is_none());
    store::set_active(conn, ctx("e1"), "user_jordan", true).expect("enable");

    // Rotation swaps the credential.
    store::rotate_token(conn, ctx("r1"), "user_jordan", "bosu_tok_jordan_2").expect("rotate");
    assert!(store::find_active_by_token(conn, CLIENT, "bosu_tok_jordan")
        .expect("lookup")
        .is_none());
    assert!(
        store::find_active_by_token(conn, CLIENT, "bosu_tok_jordan_2")
            .expect("lookup")
            .is_some()
    );

    // Default calendar: set, read back, clear.
    store::set_default_calendar(
        conn,
        ctx("c1"),
        "user_jordan",
        Some("team@business-8fbed7d5f2.test"),
    )
    .expect("set calendar");
    let found = store::get_user(conn, CLIENT, "user_jordan")
        .expect("get")
        .expect("found");
    assert_eq!(
        found.default_calendar_id.as_deref(),
        Some("team@business-8fbed7d5f2.test")
    );
    store::set_default_calendar(conn, ctx("c2"), "user_jordan", None).expect("clear calendar");
    let found = store::get_user(conn, CLIENT, "user_jordan")
        .expect("get")
        .expect("found");
    assert!(found.default_calendar_id.is_none());

    // Receipts never carry the token.
    let receipts = crate::store_core::receipts_for_entity(
        persistence.connection_ref(),
        CLIENT,
        store::USER_ENTITY_KIND,
        "user_jordan",
        10,
    )
    .expect("receipts");
    assert!(!receipts.is_empty());
    for receipt in &receipts {
        let dump = serde_json::to_string(receipt).expect("receipt json");
        assert!(!dump.contains("bosu_"), "receipt leaked a token: {dump}");
    }
}

fn bearer(token: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("authorization", format!("Bearer {token}").parse().unwrap());
    headers
}

fn session_cookie(session_id: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "cookie",
        format!("bos_operator_session={session_id}")
            .parse()
            .unwrap(),
    );
    headers
}

#[test]
fn authentication_resolves_personal_identity_and_rejects_garbage() {
    // test_state has no env operator token = open dev mode.
    let state = test_state();
    {
        let mut persistence = state.persistence.lock();
        store::create_user(
            persistence.connection(),
            CLIENT,
            "operator",
            &user("user_jordan", "Jordan"),
            "bosu_tok_jordan",
            "u1",
        )
        .expect("create");
    }

    // No token in open mode = the shared operator.
    let identity = state
        .authenticate_operator(&HeaderMap::new())
        .expect("open mode");
    assert_eq!(identity.actor_id, "operator");

    // Personal token resolves to the user.
    let identity = state
        .authenticate_operator(&bearer("bosu_tok_jordan"))
        .expect("personal token");
    assert_eq!(identity.actor_id, "user_jordan");
    assert_eq!(identity.display_name, "Jordan");

    // Presenting a wrong credential is rejected even in open mode.
    assert!(state
        .authenticate_operator(&bearer("bosu_garbage"))
        .is_err());

    // Actor resolution: personal identity wins over the request field;
    // shared identity falls back to it.
    assert_eq!(
        state.resolve_actor(&bearer("bosu_tok_jordan"), Some("spoofed")),
        "user_jordan"
    );
    assert_eq!(
        state.resolve_actor(&HeaderMap::new(), Some("legacy_actor")),
        "legacy_actor"
    );
    assert_eq!(state.resolve_actor(&HeaderMap::new(), None), "operator");
}

#[test]
fn personal_token_auth_returns_503_when_persistence_is_busy() {
    let pool =
        PersistencePool::open_in_memory_with_config(1, std::time::Duration::from_millis(100))
            .expect("pool");
    let schema_version = pool.schema_version();
    let mut state = test_state();
    state.schema_version = schema_version;
    state.persistence = pool;
    {
        let mut persistence = state.persistence.lock();
        store::create_user(
            persistence.connection(),
            CLIENT,
            "operator",
            &user("user_jordan", "Jordan"),
            "bosu_tok_jordan",
            "u1",
        )
        .expect("create");
    }

    let _guard = state.persistence.lock();
    let response = state
        .authenticate_operator(&bearer("bosu_tok_jordan"))
        .expect_err("personal token auth should be bounded");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[test]
fn query_token_authentication_resolves_the_user_for_browser_flows() {
    // The OAuth connect URL is opened in a browser tab, so the token rides
    // as a query param — it must still resolve WHO is connecting.
    let state = test_state();
    {
        let mut persistence = state.persistence.lock();
        store::create_user(
            persistence.connection(),
            CLIENT,
            "operator",
            &user("user_jordan", "Jordan"),
            "bosu_tok_jordan",
            "u1",
        )
        .expect("create");
    }

    let identity = state
        .authenticate_operator_or_query_token(&HeaderMap::new(), Some("bosu_tok_jordan"))
        .expect("personal token via query");
    assert_eq!(identity.actor_id, "user_jordan");

    // A wrong query token never falls back to open mode.
    assert!(state
        .authenticate_operator_or_query_token(&HeaderMap::new(), Some("bosu_wrong"))
        .is_err());

    // No/empty query token falls through to header auth (open mode here).
    let identity = state
        .authenticate_operator_or_query_token(&HeaderMap::new(), Some(""))
        .expect("empty token = unauthenticated open mode");
    assert_eq!(identity.actor_id, "operator");
    let identity = state
        .authenticate_operator_or_query_token(&bearer("bosu_tok_jordan"), None)
        .expect("header auth");
    assert_eq!(identity.actor_id, "user_jordan");
}

#[test]
fn cookie_session_revalidates_the_underlying_token() {
    let state = test_state();
    {
        let mut persistence = state.persistence.lock();
        store::create_user(
            persistence.connection(),
            CLIENT,
            "operator",
            &user("user_jordan", "Jordan"),
            "bosu_tok_jordan",
            "u1",
        )
        .expect("create");
    }

    let session_id = state
        .create_operator_session_for_token("bosu_tok_jordan")
        .expect("session");
    let headers = session_cookie(&session_id);
    let identity = state
        .authenticate_operator(&headers)
        .expect("cookie session");
    assert_eq!(identity.actor_id, "user_jordan");

    {
        let mut persistence = state.persistence.lock();
        store::set_active(persistence.connection(), ctx("d1"), "user_jordan", false)
            .expect("disable");
    }
    assert!(state.authenticate_operator(&headers).is_err());
}
