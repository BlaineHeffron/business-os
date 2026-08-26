//! Slice tests: pure status derivation, rollup queries over receipted seed
//! data, and router-level auth shape (/readyz open, health operator-gated).

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use bos_contracts::admin_settings::AdminSettingUpdateRequest;
use bos_contracts::instance_diagnostics::{InstanceHealth, PumpStatusDto, ReadyzResponse};
use bos_contracts::operator_users::OperatorUser;
use bos_contracts::receipt::ActorKindDto;
use bos_integrations::qbo_oauth::QboTokenGrant;
use http_body_util::BodyExt;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

use super::{service, store};
use crate::http::build_router;
use crate::http::test_support::test_state_configured;
use crate::outbox;
use crate::overlay::{AccountingVisibilityPolicy, OwnerReportsOverlay};
use crate::persistence::{Persistence, PersistencePool};
use crate::store_core::{self, MutationRequest, StoreError};

const CLIENT: &str = "test-client";
const HOUR_MS: u64 = service::HOUR_MS;
const NOW: u64 = 100 * 24 * HOUR_MS;

fn pump(last_outcome: Option<&str>) -> PumpStatusDto {
    PumpStatusDto {
        pump: "accounting_sync".to_string(),
        in_flight: false,
        last_attempt_ms: Some(NOW),
        last_outcome: last_outcome.map(str::to_string),
        next_allowed_at_ms: 0,
    }
}

fn qbo_grant(now_ms: u64) -> QboTokenGrant {
    QboTokenGrant {
        access_token: "at".to_string(),
        access_token_expires_at_ms: now_ms + 3_600_000,
        refresh_token: "rt".to_string(),
        refresh_token_expires_at_ms: now_ms + 8_640_000_000,
    }
}

fn operator_user(user_id: &str, display_name: &str) -> OperatorUser {
    OperatorUser {
        user_id: user_id.to_string(),
        display_name: display_name.to_string(),
        active: true,
        archived_at_ms: None,
        default_calendar_id: None,
        created_at_ms: NOW,
        updated_at_ms: NOW,
    }
}

fn request<'a>(
    entity_id: &'a str,
    idempotency_key: &'a str,
    expected_revision: Option<u64>,
    now_ms: u64,
) -> MutationRequest<'a> {
    MutationRequest {
        client_id: CLIENT,
        entity_kind: "diag_seed",
        entity_id,
        change_kind: "seed",
        actor_id: "tester",
        actor_kind: ActorKindDto::System,
        expected_revision,
        idempotency_key,
        correlation_id: None,
        causation_id: None,
        before_json: None,
        after_json: Some("{}".to_string()),
        now_ms,
    }
}

fn llm_failure(usage_id: &str, recorded_at_ms: u64) -> crate::slices::ai_usage::store::UsageInsert {
    crate::slices::ai_usage::store::UsageInsert {
        row: bos_contracts::ai_usage::AiUsageRow {
            usage_id: usage_id.to_string(),
            purpose: "email_ai_triage".to_string(),
            route: "api".to_string(),
            provider: "anthropic".to_string(),
            model: "claude-sonnet-4-6".to_string(),
            tokens_in: None,
            tokens_out: None,
            total_tokens: None,
            cost_micros: None,
            latency_ms: 100,
            success: false,
            error_code: Some("llm_timeout".to_string()),
            correlation_id: "corr_diag".to_string(),
            recorded_at_ms,
        },
        task_kind: None,
        thinking_level: None,
        cached_tokens: None,
        provider_request_id: None,
        error_message: Some("LLM timed out".to_string()),
    }
}

#[test]
fn derive_status_flags_pump_errors_and_terminal_jobs() {
    assert_eq!(service::derive_status(&[pump(None)], 0), "ok");
    assert_eq!(service::derive_status(&[pump(Some("ok"))], 0), "ok");
    assert_eq!(
        service::derive_status(&[pump(Some("ok (1 narration failed)"))], 0),
        "ok"
    );
    assert_eq!(
        service::derive_status(&[pump(Some("error: provider timeout"))], 0),
        "degraded"
    );
    assert_eq!(service::derive_status(&[pump(Some("ok"))], 1), "degraded");
    assert_eq!(service::derive_status(&[], 0), "ok");
}

#[test]
fn error_rollup_counts_failures_within_window() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();

    // Applied mutation: never counted.
    store_core::mutate(conn, request("ent_ok", "idem_ok", None, NOW), |_| Ok(()))
        .expect("applied seed");
    // Conflict against the applied entity, inside the hour.
    let outcome = store_core::mutate(
        conn,
        request("ent_ok", "idem_conflict", Some(99), NOW),
        |_| Ok(()),
    )
    .expect("conflict is a recorded outcome");
    assert!(matches!(
        outcome,
        store_core::MutationOutcome::RevisionConflict { .. }
    ));
    // Failed mutation inside the hour.
    store_core::mutate(conn, request("ent_f1", "idem_f1", None, NOW), |_| {
        Err(StoreError::Domain("seed_failure".to_string()))
    })
    .expect_err("seed failure");
    // Failed mutation two hours ago: 24h window only.
    store_core::mutate(
        conn,
        request("ent_f2", "idem_f2", None, NOW - 2 * HOUR_MS),
        |_| Err(StoreError::Domain("seed_failure".to_string())),
    )
    .expect_err("old seed failure");
    // LLM failures: one fresh, one outside even the 24h window.
    crate::slices::ai_usage::store::insert_usage(conn, CLIENT, &llm_failure("aiu_fresh", NOW))
        .expect("usage insert");
    crate::slices::ai_usage::store::insert_usage(
        conn,
        CLIENT,
        &llm_failure("aiu_stale", NOW - 30 * 24 * HOUR_MS),
    )
    .expect("usage insert");

    let hour =
        store::error_rollup(persistence.connection_ref(), CLIENT, NOW, HOUR_MS).expect("1h rollup");
    assert_eq!(hour.failed_receipts, 1);
    assert_eq!(hour.conflict_receipts, 1);
    assert_eq!(hour.llm_failures, 1);
    assert_eq!(hour.llm_errors.len(), 1);
    assert_eq!(hour.llm_errors[0].purpose, "email_ai_triage");
    assert_eq!(
        hour.llm_errors[0].error_code.as_deref(),
        Some("llm_timeout")
    );

    let day = store::error_rollup(persistence.connection_ref(), CLIENT, NOW, service::DAY_MS)
        .expect("24h rollup");
    assert_eq!(day.failed_receipts, 2);
    assert_eq!(day.conflict_receipts, 1);
    assert_eq!(day.llm_failures, 1);

    // Another client's window stays empty.
    let other = store::error_rollup(persistence.connection_ref(), "other", NOW, service::DAY_MS)
        .expect("other rollup");
    assert_eq!(other.failed_receipts, 0);
    assert_eq!(other.llm_failures, 0);
}

#[test]
fn outbox_backlog_counts_pending_and_terminal() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    for job_id in ["job_a", "job_b"] {
        let job = outbox::NewOutboxJob {
            job_id: job_id.to_string(),
            provider: "gmail".to_string(),
            capability: "create_draft".to_string(),
            payload_json: "{}".to_string(),
            source_entity_kind: "diag_seed".to_string(),
            source_entity_id: job_id.to_string(),
            correlation_id: None,
            causation_id: None,
            idempotency_key: format!("idem_{job_id}"),
        };
        store_core::mutate(
            conn,
            request(job_id, &format!("enqueue_{job_id}"), None, NOW),
            |tx| outbox::enqueue_within(tx, CLIENT, &job, NOW),
        )
        .expect("enqueue");
    }
    let claimed = outbox::claim_due_jobs(conn, CLIENT, None, 1_000, 10, NOW).expect("claim");
    assert_eq!(claimed.len(), 2);
    outbox::record_attempt(
        conn,
        CLIENT,
        &claimed[0],
        &outbox::AttemptOutcome::Terminal {
            error: "permanent_rejection".to_string(),
            result_json: None,
        },
        NOW,
    )
    .expect("terminal attempt");

    let backlog = store::outbox_backlog(persistence.connection_ref(), CLIENT).expect("backlog");
    assert_eq!(backlog.pending_jobs, 1);
    assert_eq!(backlog.terminal_jobs, 1);
    assert_eq!(
        backlog.last_terminal_error.as_deref(),
        Some("permanent_rejection")
    );
}

async fn body_json<T: serde::de::DeserializeOwned>(response: axum::response::Response) -> T {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("json body")
}

#[tokio::test]
async fn readyz_is_open_and_health_is_operator_gated() {
    let router = build_router(test_state_configured(Some("secret"), &[]));

    let response = router
        .clone()
        .oneshot(
            Request::get("/readyz")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), 200, "/readyz must not require auth");
    let readyz: ReadyzResponse = body_json(response).await;
    assert_eq!(readyz.client_id, CLIENT);
    assert_eq!(readyz.display_name, "BusinessOS");
    assert_eq!(readyz.status, "ok");
    assert!(readyz.schema_version > 0);
    assert!(readyz
        .enabled_slices
        .iter()
        .any(|id| id == "instance_diagnostics"));

    let response = router
        .clone()
        .oneshot(
            Request::get("/api/diagnostics/health")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), 401, "health must require the token");

    let response = router
        .clone()
        .oneshot(
            Request::get("/api/diagnostics/health")
                .header("authorization", "Bearer wrong")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), 401, "a wrong token is rejected");

    let response = router
        .oneshot(
            Request::get("/api/diagnostics/health")
                .header("authorization", "Bearer secret")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), 200);
    let health: InstanceHealth = body_json(response).await;
    assert_eq!(health.client_id, CLIENT);
    assert_eq!(health.status, "ok");
    assert_eq!(health.pumps.len(), 10);
    assert!(health
        .pumps
        .iter()
        .any(|pump| pump.pump == "crm_cache_sync"));
    assert!(health
        .pumps
        .iter()
        .any(|pump| pump.pump == "enrichment_freshness"));
    assert!(health
        .pumps
        .iter()
        .any(|pump| pump.pump == "data_retention"));
    assert!(health.schema_version > 0);
    assert!(health
        .enabled_slices
        .iter()
        .any(|id| id == "instance_diagnostics"));
    assert_eq!(health.enabled_slices, health.visible_slices);
}

#[tokio::test]
async fn health_visible_slices_follow_operator_visibility_policy() {
    let mut state = test_state_configured(
        None,
        &["accounting", "owner_reports", "instance_diagnostics"],
    );
    state.accounting_visibility_policy = AccountingVisibilityPolicy::AuthorizerOnly;
    state.owner_reports_overlay = Arc::new(Some(OwnerReportsOverlay {
        allowed_operator_user_ids: vec!["user_jordan".to_string()],
        ..OwnerReportsOverlay::default()
    }));
    {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        crate::slices::operator_users::store::create_user(
            conn,
            CLIENT,
            "operator",
            &operator_user("user_jordan", "Jordan"),
            "tok_jordan",
            "create_jordan",
        )
        .expect("create jordan");
        crate::slices::operator_users::store::create_user(
            conn,
            CLIENT,
            "operator",
            &operator_user("user_casey", "Casey"),
            "tok_casey",
            "create_casey",
        )
        .expect("create casey");
        crate::slices::accounting::store::store_credential(
            conn,
            CLIENT,
            "realm-1",
            "sandbox",
            &qbo_grant(NOW),
            "user_jordan",
            NOW,
        )
        .expect("store qbo credential");
    }
    let router = build_router(state);

    let casey_response = router
        .clone()
        .oneshot(
            Request::get("/api/diagnostics/health")
                .header("authorization", "Bearer tok_casey")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("casey response");
    assert_eq!(casey_response.status(), StatusCode::OK);
    let casey: InstanceHealth = body_json(casey_response).await;
    assert!(casey.enabled_slices.iter().any(|id| id == "accounting"));
    assert!(casey.enabled_slices.iter().any(|id| id == "owner_reports"));
    assert!(!casey.visible_slices.iter().any(|id| id == "accounting"));
    assert!(!casey.visible_slices.iter().any(|id| id == "owner_reports"));

    let jordan_response = router
        .oneshot(
            Request::get("/api/diagnostics/health")
                .header("authorization", "Bearer tok_jordan")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("jordan response");
    assert_eq!(jordan_response.status(), StatusCode::OK);
    let jordan: InstanceHealth = body_json(jordan_response).await;
    assert!(jordan.visible_slices.iter().any(|id| id == "accounting"));
    assert!(jordan.visible_slices.iter().any(|id| id == "owner_reports"));
}

#[test]
fn readyz_returns_promptly_while_persistence_is_held() {
    let state = test_state_configured(None, &[]);
    assert!(state.schema_version > 0);
    let holder_state = state.clone();
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = holder_state.persistence.lock();
        held_tx.send(()).expect("signal held");
        let _ = release_rx.recv();
    });
    held_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("persistence lock held");

    let ready_state = state.clone();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let ready = std::thread::spawn(move || {
        let response = service::readyz(&ready_state, NOW).expect("readyz");
        ready_tx.send(response).expect("send readyz response");
    });
    let response = ready_rx
        .recv_timeout(Duration::from_millis(100))
        .expect("readyz must not wait on persistence");
    assert_eq!(response.schema_version, state.schema_version);

    release_tx.send(()).expect("release holder");
    holder.join().expect("holder join");
    ready.join().expect("ready join");
}

#[tokio::test]
async fn persistence_or_busy_returns_503_when_pool_is_exhausted() {
    let pool =
        PersistencePool::open_in_memory_with_config(1, Duration::from_millis(100)).expect("pool");
    let schema_version = pool.schema_version();
    let mut state = test_state_configured(None, &[]);
    state.schema_version = schema_version;
    state.persistence = pool;
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let holder_state = state.clone();
    let holder = std::thread::spawn(move || {
        let _guard = holder_state.persistence.lock();
        held_tx.send(()).expect("signal held");
        let _ = release_rx.recv();
    });
    held_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("persistence pool exhausted");

    let call_state = state.clone();
    let response = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::task::spawn_blocking(move || match call_state.persistence_or_busy() {
            Ok(_) => StatusCode::OK.into_response(),
            Err(response) => *response,
        }),
    )
    .await
    .expect("bounded persistence acquisition timed out")
    .expect("blocking task");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: serde_json::Value = body_json(response).await;
    assert_eq!(body["error"], "persistence_busy");
    assert!(
        body.get("code").is_none(),
        "error envelope should use error only"
    );

    release_tx.send(()).expect("release holder");
    holder.join().expect("holder join");
}

#[tokio::test]
async fn readyz_serves_even_when_slice_is_disabled() {
    let router = build_router(test_state_configured(None, &["work_queue"]));

    let response = router
        .clone()
        .oneshot(
            Request::get("/readyz")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), 200);
    let readyz: ReadyzResponse = body_json(response).await;
    assert_eq!(readyz.enabled_slices, vec!["work_queue"]);

    let response = router
        .oneshot(
            Request::get("/api/diagnostics/health")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(
        response.status(),
        404,
        "disabled slice mounts no health route"
    );
}

#[tokio::test]
async fn readyz_reports_effective_runtime_overrides() {
    let state = test_state_configured(None, &["work_queue"]);
    {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        for (var_name, key) in [
            (
                crate::env_registry::BOS_AUTO_PRODUCE_ENABLED.name,
                "readyz-auto-produce",
            ),
            (
                crate::env_registry::BOS_AI_TRIAGE_ENABLED.name,
                "readyz-ai-triage",
            ),
        ] {
            crate::slices::admin_settings::service::upsert_setting(
                conn,
                &state.client_id,
                "operator",
                var_name,
                &AdminSettingUpdateRequest {
                    expected_revision: None,
                    idempotency_key: key.to_string(),
                    actor_id: None,
                    value: "1".to_string(),
                },
                NOW,
            )
            .expect("runtime override");
        }
    }

    let router = build_router(state);
    let response = router
        .oneshot(
            Request::get("/readyz")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), 200);
    let readyz: ReadyzResponse = body_json(response).await;
    assert!(
        readyz.auto_produce_enabled,
        "readyz must report the stored auto-produce override"
    );
    assert!(
        readyz.ai_triage_enabled,
        "readyz must report the stored AI triage override"
    );
}
