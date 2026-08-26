//! Slice tests: release notes are created idempotently, newest-first, and
//! dismissed per authenticated operator.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use bos_contracts::release_notes::{
    ReleaseNote, ReleaseNoteCreateRequest, ReleaseNoteDismissRequest, ReleaseNotesResponse,
};
use http_body_util::BodyExt;
use tower::ServiceExt;

use super::{service, store};
use crate::http::{
    build_router,
    test_support::{test_state_configured, EnvGuard},
};
use crate::persistence::Persistence;

const CLIENT: &str = "test-client";
const WEBHOOK_SECRET_ENV: &str = "BOS_RELEASE_NOTES_WEBHOOK_SECRET";

fn note(id: &str, created_at_ms: u64) -> ReleaseNote {
    ReleaseNote {
        release_note_id: id.to_string(),
        title: "What's new".to_string(),
        summary: "The queue is easier to scan.".to_string(),
        body: Some("- Follow-ups are easier to review.".to_string()),
        build_sha: Some(id.to_string()),
        created_at_ms,
    }
}

#[test]
fn create_request_uses_release_note_id_or_idempotency_key() {
    let request = ReleaseNoteCreateRequest {
        release_note_id: Some("build_a".to_string()),
        idempotency_key: "idem_a".to_string(),
        title: "  ".to_string(),
        summary: "  Operators can review work faster.  ".to_string(),
        body: Some("  Detail  ".to_string()),
        build_sha: Some("  abc123  ".to_string()),
    };
    let note = service::note_from_request(&request, 1_000).expect("note");
    assert_eq!(note.release_note_id, "build_a");
    assert_eq!(note.title, "What's new");
    assert_eq!(note.summary, "Operators can review work faster.");
    assert_eq!(note.body.as_deref(), Some("Detail"));
    assert_eq!(note.build_sha.as_deref(), Some("abc123"));

    let fallback = service::note_from_request(
        &ReleaseNoteCreateRequest {
            release_note_id: None,
            idempotency_key: "build_b".to_string(),
            title: "What's new".to_string(),
            summary: "Inventory alerts are clearer.".to_string(),
            body: None,
            build_sha: None,
        },
        2_000,
    )
    .expect("fallback");
    assert_eq!(fallback.release_note_id, "build_b");
}

#[test]
fn latest_visible_excludes_only_this_users_dismissals() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    store::insert_note(conn, CLIENT, "fleet", &note("build_a", 1_000), "build_a")
        .expect("insert old");
    store::insert_note(conn, CLIENT, "fleet", &note("build_b", 2_000), "build_b")
        .expect("insert new");

    assert_eq!(
        store::latest_visible(conn, CLIENT, "user_jordan")
            .expect("latest")
            .expect("note")
            .release_note_id,
        "build_b"
    );
    store::dismiss_note(
        conn,
        CLIENT,
        "user_jordan",
        "user_jordan",
        "build_b",
        "dismiss_build_b",
        3_000,
    )
    .expect("dismiss");
    assert_eq!(
        store::latest_visible(conn, CLIENT, "user_jordan")
            .expect("latest")
            .expect("note")
            .release_note_id,
        "build_a"
    );
    assert_eq!(
        store::latest_visible(conn, CLIENT, "user_casey")
            .expect("latest")
            .expect("note")
            .release_note_id,
        "build_b",
        "dismissal is per user"
    );
}

#[tokio::test]
async fn webhook_requires_webhook_token_and_latest_dismisses() {
    let _env = EnvGuard::set(WEBHOOK_SECRET_ENV, "webhook-secret");
    let router = build_router(test_state_configured(Some("secret"), &["release_notes"]));

    let request = ReleaseNoteCreateRequest {
        release_note_id: Some("build_a".to_string()),
        idempotency_key: "build_a".to_string(),
        title: "What's new".to_string(),
        summary: "The home view loads faster.".to_string(),
        body: Some("- The daily work list is easier to scan.".to_string()),
        build_sha: Some("build_a".to_string()),
    };
    let unauthorized = router
        .clone()
        .oneshot(
            Request::post("/api/webhooks/release-notes")
                .header("authorization", "Bearer secret")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&request).expect("json")))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let created = router
        .clone()
        .oneshot(
            Request::post("/api/webhooks/release-notes")
                .header("authorization", "Bearer webhook-secret")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&request).expect("json")))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(created.status(), StatusCode::ACCEPTED);

    let replayed = router
        .clone()
        .oneshot(
            Request::post("/api/webhooks/release-notes")
                .header("authorization", "Bearer webhook-secret")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&request).expect("json")))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(replayed.status(), StatusCode::OK);

    let latest = router
        .clone()
        .oneshot(
            Request::get("/api/release-notes/latest")
                .header("authorization", "Bearer secret")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(latest.status(), StatusCode::OK);
    let body: ReleaseNotesResponse =
        serde_json::from_slice(&latest.into_body().collect().await.expect("body").to_bytes())
            .expect("latest body");
    assert_eq!(body.notes.len(), 1);
    assert_eq!(body.notes[0].release_note_id, "build_a");

    let dismiss = ReleaseNoteDismissRequest {
        idempotency_key: "dismiss_a".to_string(),
        actor_id: None,
    };
    let dismissed = router
        .clone()
        .oneshot(
            Request::post("/api/release-notes/build_a/dismiss")
                .header("authorization", "Bearer secret")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&dismiss).expect("json")))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(dismissed.status(), StatusCode::OK);

    let latest = router
        .oneshot(
            Request::get("/api/release-notes/latest")
                .header("authorization", "Bearer secret")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let body: ReleaseNotesResponse =
        serde_json::from_slice(&latest.into_body().collect().await.expect("body").to_bytes())
            .expect("latest body");
    assert!(body.notes.is_empty());
}

#[tokio::test]
async fn webhook_is_not_mounted_when_secret_is_unset() {
    let _env = EnvGuard::unset(WEBHOOK_SECRET_ENV);
    let router = build_router(test_state_configured(Some("secret"), &["release_notes"]));
    let request = ReleaseNoteCreateRequest {
        release_note_id: Some("build_a".to_string()),
        idempotency_key: "build_a".to_string(),
        title: "What's new".to_string(),
        summary: "The home view loads faster.".to_string(),
        body: None,
        build_sha: Some("build_a".to_string()),
    };

    let response = router
        .oneshot(
            Request::post("/api/webhooks/release-notes")
                .header("authorization", "Bearer webhook-secret")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&request).expect("json")))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[test]
fn create_with_same_release_id_and_new_idempotency_key_updates_note() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    store::insert_note(conn, CLIENT, "fleet", &note("build_a", 1_000), "build_a").expect("insert");

    let mut updated = note("build_a", 2_000);
    updated.summary = "The work queue has a faster loading path.".to_string();
    updated.body = None;
    store::insert_note(conn, CLIENT, "fleet", &updated, "build_a_second_delivery").expect("upsert");

    let latest = store::latest_visible(conn, CLIENT, "user_jordan")
        .expect("latest")
        .expect("note");
    assert_eq!(latest.release_note_id, "build_a");
    assert_eq!(latest.summary, "The work queue has a faster loading path.");
    assert_eq!(latest.body, None);
    assert_eq!(latest.created_at_ms, 2_000);
}
