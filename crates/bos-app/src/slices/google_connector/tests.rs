use super::service::{
    generate_state, redirect_uri_from_base_or_headers, requested_scopes_for_enabled_slices,
    ANALYTICS_READONLY_SCOPE, CALENDAR_EVENTS_SCOPE, CALENDAR_LIST_READONLY_SCOPE,
    DRIVE_READONLY_SCOPE, GMAIL_COMPOSE_SCOPE, GMAIL_READONLY_SCOPE, SEARCH_CONSOLE_READONLY_SCOPE,
};
use super::store;
use crate::persistence::Persistence;
use crate::store_core;

// NOTE: gmail_status / resolve_google_oauth read the env registry, so their
// env-dependent branches are not asserted here (parallel tests + process env
// don't mix — predecessor lesson). The store and CSRF pieces are env-free.

#[test]
fn credential_store_round_trip_and_disconnect() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();

    assert!(
        store::get_credential(conn, "test-client", "user_jordan", "gmail")
            .expect("get")
            .is_none()
    );

    store::store_credential(
        conn,
        "test-client",
        "user_jordan",
        "gmail",
        "rt-secret-token",
        &[GMAIL_READONLY_SCOPE.to_string()],
        1_000,
    )
    .expect("store");

    let stored = store::get_credential(
        persistence.connection_ref(),
        "test-client",
        "user_jordan",
        "gmail",
    )
    .expect("get")
    .expect("present");
    assert_eq!(stored.user_id, "user_jordan");
    assert_eq!(stored.refresh_token, "rt-secret-token");
    assert_eq!(stored.scopes.len(), 1);

    // Another user's connection is a SEPARATE credential, not an overwrite.
    store::store_credential(
        persistence.connection(),
        "test-client",
        "user_casey",
        "gmail",
        "rt-casey-token",
        &[],
        2_000,
    )
    .expect("second user");
    assert!(store::get_credential(
        persistence.connection_ref(),
        "test-client",
        "user_jordan",
        "gmail"
    )
    .expect("get")
    .is_some_and(|c| c.refresh_token == "rt-secret-token"));
    let all = store::list_credentials(persistence.connection_ref(), "test-client", "gmail")
        .expect("list");
    assert_eq!(
        all.iter().map(|c| c.user_id.as_str()).collect::<Vec<_>>(),
        vec!["user_jordan", "user_casey"],
        "oldest connection first"
    );

    // Reconnect overwrites the SAME user's credential.
    store::store_credential(
        persistence.connection(),
        "test-client",
        "user_jordan",
        "gmail",
        "rt-new-token",
        &[],
        3_000,
    )
    .expect("overwrite");
    let stored = store::get_credential(
        persistence.connection_ref(),
        "test-client",
        "user_jordan",
        "gmail",
    )
    .expect("get")
    .expect("present");
    assert_eq!(stored.refresh_token, "rt-new-token");

    store::delete_credential(
        persistence.connection(),
        "test-client",
        "user_jordan",
        "gmail",
        4_000,
    )
    .expect("disconnect");
    assert!(store::get_credential(
        persistence.connection_ref(),
        "test-client",
        "user_jordan",
        "gmail"
    )
    .expect("get")
    .is_none());
    assert!(
        store::get_credential(
            persistence.connection_ref(),
            "test-client",
            "user_casey",
            "gmail"
        )
        .expect("get")
        .is_some(),
        "disconnect removes only the acting user's credential"
    );
}

#[test]
fn receipts_never_contain_the_refresh_token() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    store::store_credential(
        persistence.connection(),
        "test-client",
        "user_jordan",
        "gmail",
        "rt-super-secret-value",
        &["scope-a".to_string()],
        1_000,
    )
    .expect("store");

    let receipts = store_core::receipts_for_entity(
        persistence.connection_ref(),
        "test-client",
        store::ENTITY_KIND,
        "gmail:user_jordan",
        10,
    )
    .expect("receipts");
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].change_kind, "connect");

    // The whole receipts table must be free of the secret, not just this row.
    let leaked: i64 = persistence
        .connection_ref()
        .query_row(
            "SELECT COUNT(*) FROM receipts WHERE before_json LIKE '%rt-super-secret-value%' \
             OR after_json LIKE '%rt-super-secret-value%'",
            [],
            |row| row.get(0),
        )
        .expect("scan");
    assert_eq!(leaked, 0, "refresh token leaked into the receipt spine");
}

#[test]
fn revoked_credential_cleanup_is_receipted_as_system() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    store::store_credential(
        persistence.connection(),
        "test-client",
        "user_jordan",
        "gmail",
        "rt-super-secret-value",
        &["scope-a".to_string()],
        1_000,
    )
    .expect("store");

    store::mark_credential_revoked(
        persistence.connection(),
        "test-client",
        "user_jordan",
        "gmail",
        "google_oauth_invalid_grant",
        2_000,
    )
    .expect("mark revoked");

    assert!(
        store::get_credential(
            persistence.connection_ref(),
            "test-client",
            "user_jordan",
            "gmail"
        )
        .expect("get")
        .is_none(),
        "revoked credential should no longer satisfy connector status"
    );

    let receipts = store_core::receipts_for_entity(
        persistence.connection_ref(),
        "test-client",
        store::ENTITY_KIND,
        "gmail:user_jordan",
        10,
    )
    .expect("receipts");
    assert!(
        receipts
            .iter()
            .any(|receipt| receipt.change_kind == "oauth_revoked"
                && receipt.actor_id == "gmail_ingest_pump"
                && receipt.actor_kind == bos_contracts::receipt::ActorKindDto::System),
        "cleanup should be auditable as a system mutation"
    );

    let leaked: i64 = persistence
        .connection_ref()
        .query_row(
            "SELECT COUNT(*) FROM receipts WHERE before_json LIKE '%rt-super-secret-value%' \
             OR after_json LIKE '%rt-super-secret-value%'",
            [],
            |row| row.get(0),
        )
        .expect("scan");
    assert_eq!(leaked, 0, "refresh token leaked into cleanup receipt");
}

#[test]
fn csrf_state_tokens_are_unique_single_use_and_carry_the_user() {
    let a = generate_state();
    let b = generate_state();
    assert_ne!(a, b);
    assert!(a.starts_with("st_") && a.len() > 20);

    let state = crate::http::test_support::test_state();
    state
        .register_oauth_state("google", &a, "user_jordan")
        .expect("register");
    assert_eq!(
        state
            .consume_oauth_state("google", &a)
            .expect("consume")
            .as_deref(),
        Some("user_jordan"),
        "registered state must validate and return the bound user"
    );
    assert!(
        state
            .consume_oauth_state("google", &a)
            .expect("replay")
            .is_none(),
        "state must be single-use"
    );
    assert!(
        state
            .consume_oauth_state("google", &b)
            .expect("unknown")
            .is_none(),
        "unregistered state must fail"
    );
}

#[test]
fn csrf_state_survives_app_state_recreation_and_is_connector_bound() {
    let state_dir = std::env::temp_dir().join(format!(
        "bos-oauth-state-restart-{}-{}",
        std::process::id(),
        generate_state()
    ));
    let persistence = crate::persistence::PersistencePool::open_at(&state_dir).expect("disk db");
    let mut state = crate::http::test_support::test_state();
    state.persistence = persistence;
    let token = generate_state();
    state
        .register_oauth_state("google", &token, "user_adam")
        .expect("register");
    drop(state);

    let persistence = crate::persistence::PersistencePool::open_at(&state_dir).expect("reopen db");
    let mut restarted = crate::http::test_support::test_state();
    restarted.persistence = persistence;
    assert!(restarted
        .consume_oauth_state("qbo", &token)
        .expect("wrong connector")
        .is_none());
    assert_eq!(
        restarted
            .consume_oauth_state("google", &token)
            .expect("consume after recreation")
            .as_deref(),
        Some("user_adam")
    );
    drop(restarted);
    std::fs::remove_dir_all(&state_dir).expect("remove test db");
}

#[test]
fn csrf_state_raw_token_is_never_persisted_or_receipted() {
    let state = crate::http::test_support::test_state();
    let token = "st_raw-secret-state-token";
    state
        .register_oauth_state("google", token, "user_adam")
        .expect("register");
    let persistence = state.persistence.lock();
    let leaked: i64 = persistence
        .connection_ref()
        .query_row(
            "SELECT \
               (SELECT COUNT(*) FROM connector_oauth_states WHERE state_hash = ?1) + \
               (SELECT COUNT(*) FROM receipts \
                WHERE entity_id LIKE '%' || ?1 || '%' \
                   OR idempotency_key LIKE '%' || ?1 || '%' \
                   OR before_json LIKE '%' || ?1 || '%' \
                   OR after_json LIKE '%' || ?1 || '%')",
            [token],
            |row| row.get(0),
        )
        .expect("leak scan");
    assert_eq!(leaked, 0);
}

#[test]
fn expired_csrf_state_is_deleted_and_rejected() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    crate::slices::oauth_state::register_oauth_state(
        persistence.connection(),
        "test-client",
        "google",
        "st_expiring",
        "user_adam",
        1_000,
        100,
    )
    .expect("register");

    assert!(crate::slices::oauth_state::consume_oauth_state(
        persistence.connection(),
        "test-client",
        "google",
        "st_expiring",
        "attempt_1",
        1_100,
    )
    .expect("expire")
    .is_none());
    let remaining: i64 = persistence
        .connection_ref()
        .query_row("SELECT COUNT(*) FROM connector_oauth_states", [], |row| {
            row.get(0)
        })
        .expect("count");
    assert_eq!(remaining, 0);
}

#[test]
fn concurrent_csrf_callbacks_accept_exactly_once() {
    let state_dir = std::env::temp_dir().join(format!(
        "bos-oauth-state-concurrent-{}-{}",
        std::process::id(),
        generate_state()
    ));
    let persistence = crate::persistence::PersistencePool::open_at(&state_dir).expect("disk db");
    let mut state = crate::http::test_support::test_state();
    state.persistence = persistence;
    let token = generate_state();
    state
        .register_oauth_state("google", &token, "user_adam")
        .expect("register");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let mut handles = Vec::new();
    for _ in 0..2 {
        let state = state.clone();
        let token = token.clone();
        let barrier = barrier.clone();
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            state
                .consume_oauth_state("google", &token)
                .expect("consume")
        }));
    }
    barrier.wait();
    let accepted = handles
        .into_iter()
        .map(|handle| handle.join().expect("callback thread"))
        .filter(Option::is_some)
        .count();
    assert_eq!(accepted, 1);
    drop(state);
    std::fs::remove_dir_all(&state_dir).expect("remove test db");
}

#[test]
fn redirect_uri_uses_public_base_when_configured() {
    let uri = redirect_uri_from_base_or_headers(
        Some("https://ops.business-914f630770.example.test/"),
        Some("http"),
        Some("business-a7010c80eb.example.test:4400"),
        Some("127.0.0.1:4400"),
    );
    assert_eq!(
        uri,
        "https://ops.business-914f630770.example.test/api/connectors/google/callback"
    );
}

#[test]
fn redirect_uri_falls_back_to_forwarded_public_host() {
    let uri = redirect_uri_from_base_or_headers(
        None,
        Some("https"),
        Some("ops.business-914f630770.example.test"),
        Some("business-a7010c80eb.example.test:4400"),
    );
    assert_eq!(
        uri,
        "https://ops.business-914f630770.example.test/api/connectors/google/callback"
    );
}

#[test]
fn redirect_uri_defaults_localhost_to_http() {
    let uri = redirect_uri_from_base_or_headers(None, None, None, Some("127.0.0.1:4400"));
    assert_eq!(uri, "http://127.0.0.1:4400/api/connectors/google/callback");
}

#[test]
fn requested_scopes_follow_enabled_slices() {
    let demo_enabled = |slice_id: &str| {
        matches!(
            slice_id,
            "email_drafts" | "calendar_drafts" | "google_connector"
        )
    };
    let scopes = requested_scopes_for_enabled_slices(demo_enabled);
    assert_eq!(
        scopes,
        vec![
            GMAIL_READONLY_SCOPE,
            GMAIL_COMPOSE_SCOPE,
            CALENDAR_EVENTS_SCOPE,
            CALENDAR_LIST_READONLY_SCOPE,
        ]
    );
    assert!(
        !scopes.contains(&DRIVE_READONLY_SCOPE),
        "Demo does not enable drive_corpus, so connect must not request Drive"
    );
}

#[test]
fn requested_scopes_include_drive_only_for_drive_consumers() {
    let scopes = requested_scopes_for_enabled_slices(|slice_id| slice_id == "drive_corpus");
    assert_eq!(scopes, vec![GMAIL_READONLY_SCOPE, DRIVE_READONLY_SCOPE]);

    let scopes = requested_scopes_for_enabled_slices(|slice_id| slice_id == "content_drafts");
    assert_eq!(scopes, vec![GMAIL_READONLY_SCOPE, DRIVE_READONLY_SCOPE]);

    let scopes = requested_scopes_for_enabled_slices(|slice_id| slice_id == "call_inputs");
    assert_eq!(scopes, vec![GMAIL_READONLY_SCOPE, DRIVE_READONLY_SCOPE]);
}

#[test]
fn requested_scopes_include_search_console_for_search_console_only() {
    let scopes = requested_scopes_for_enabled_slices(|slice_id| slice_id == "search_console");
    assert_eq!(
        scopes,
        vec![
            GMAIL_READONLY_SCOPE,
            SEARCH_CONSOLE_READONLY_SCOPE,
            ANALYTICS_READONLY_SCOPE,
        ]
    );
}

#[test]
fn requested_scopes_include_search_console_and_analytics_for_owner_reports() {
    let scopes = requested_scopes_for_enabled_slices(|slice_id| slice_id == "owner_reports");
    assert_eq!(
        scopes,
        vec![
            GMAIL_READONLY_SCOPE,
            GMAIL_COMPOSE_SCOPE,
            SEARCH_CONSOLE_READONLY_SCOPE,
            ANALYTICS_READONLY_SCOPE,
        ]
    );
}
