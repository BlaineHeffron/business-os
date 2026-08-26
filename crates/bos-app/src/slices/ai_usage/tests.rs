//! Slice tests: sink persistence, idempotent inserts, totals windows, and the
//! fail-closed API path recording a failure row. No live LLM calls.

use bos_contracts::ai_usage::AiUsageRow;
use bos_contracts::llm_settings::{
    LlmGlobalRouteSettingsUpdate, LlmPurposeRouteOverrideUpdate, LlmRouteSettingsUpdateRequest,
};
use bos_kernel::{AiCallUsageRecord, AiCallUsageSink};

use super::service::{self, PersistedUsageSink};
use super::store::{self, UsageInsert};
use crate::persistence::{Persistence, PersistencePool};
use crate::store_core;

const CLIENT: &str = "test-client";

fn test_known_purpose(purpose: &str) -> bool {
    matches!(purpose, "email_ai_triage" | "invoice_fill")
}

fn insert(usage_id: &str, success: bool, recorded_at_ms: u64) -> UsageInsert {
    UsageInsert {
        row: AiUsageRow {
            usage_id: usage_id.to_string(),
            purpose: "email_ai_triage".to_string(),
            route: "api".to_string(),
            provider: "anthropic".to_string(),
            model: "claude-sonnet-4-6".to_string(),
            tokens_in: Some(1_000),
            tokens_out: Some(200),
            total_tokens: Some(1_200),
            cost_micros: Some(4_500),
            latency_ms: 1_500,
            success,
            error_code: (!success).then(|| "llm_api_not_configured".to_string()),
            correlation_id: "corr_1".to_string(),
            recorded_at_ms,
        },
        task_kind: Some("classify".to_string()),
        thinking_level: None,
        cached_tokens: None,
        provider_request_id: None,
        error_message: (!success).then(|| "LLM API backend requires BOS_LLM_API_KEY".to_string()),
    }
}

#[test]
fn usage_insert_is_receipted_and_idempotent() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    store::insert_usage(conn, CLIENT, &insert("aiu_1", true, 1_000)).expect("insert");
    // Same usage_id again (e.g. a replayed sink callback) stays single-row.
    store::insert_usage(conn, CLIENT, &insert("aiu_1", true, 1_000)).expect("replay");

    let rows = store::list_recent(conn, CLIENT, 10).expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].usage_id, "aiu_1");
    assert_eq!(rows[0].tokens_in, Some(1_000));

    let receipts = store_core::receipts_for_entity(
        persistence.connection_ref(),
        CLIENT,
        store::USAGE_ENTITY_KIND,
        "aiu_1",
        10,
    )
    .expect("receipts");
    assert!(!receipts.is_empty(), "usage insert must be receipted");
}

#[test]
fn totals_aggregate_and_window() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    store::insert_usage(conn, CLIENT, &insert("aiu_old", true, 1_000)).expect("insert");
    store::insert_usage(conn, CLIENT, &insert("aiu_new", true, 100_000)).expect("insert");
    store::insert_usage(conn, CLIENT, &insert("aiu_fail", false, 100_500)).expect("insert");

    let all = store::totals_since(conn, CLIENT, 0).expect("totals");
    assert_eq!(all.calls, 3);
    assert_eq!(all.failures, 1);
    assert_eq!(all.tokens_in, 3_000);
    assert_eq!(all.tokens_out, 600);
    assert_eq!(all.cost_micros, 13_500);

    let recent = store::totals_since(conn, CLIENT, 50_000).expect("totals");
    assert_eq!(recent.calls, 2);
    assert_eq!(recent.failures, 1);
}

#[test]
fn cost_total_can_be_scoped_to_purpose_and_correlation() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let mut first = insert("aiu_research_1", true, 1_000);
    first.row.purpose = "enrichment_research_action".to_string();
    first.row.correlation_id = "enr_1".to_string();
    first.row.cost_micros = Some(2_000);
    let mut second = insert("aiu_research_2", true, 1_100);
    second.row.purpose = "enrichment_research_action".to_string();
    second.row.correlation_id = "enr_1".to_string();
    second.row.cost_micros = Some(3_000);
    let mut other_run = insert("aiu_research_other_run", true, 1_200);
    other_run.row.purpose = "enrichment_research_action".to_string();
    other_run.row.correlation_id = "enr_2".to_string();
    other_run.row.cost_micros = Some(9_000);
    let mut other_purpose = insert("aiu_other_purpose", true, 1_300);
    other_purpose.row.purpose = "email_ai_triage".to_string();
    other_purpose.row.correlation_id = "enr_1".to_string();
    other_purpose.row.cost_micros = Some(9_000);

    store::insert_usage(conn, CLIENT, &first).expect("first");
    store::insert_usage(conn, CLIENT, &second).expect("second");
    store::insert_usage(conn, CLIENT, &other_run).expect("other run");
    store::insert_usage(conn, CLIENT, &other_purpose).expect("other purpose");

    assert_eq!(
        store::cost_micros_for_purpose_correlation(
            conn,
            CLIENT,
            "enrichment_research_action",
            "enr_1",
        )
        .expect("cost"),
        5_000
    );
}

#[test]
fn persisted_sink_records_harness_attempts_with_real_purpose() {
    let persistence = PersistencePool::open_in_memory().expect("db");
    let sink = PersistedUsageSink::new(
        persistence.clone(),
        CLIENT.to_string(),
        "calendar_event_extract".to_string(),
    );
    sink.record(AiCallUsageRecord {
        usage_id: "ai-usage-harness-typed-1-1".to_string(),
        recorded_at_ms: 5_000,
        call_purpose: "harness_typed_task".to_string(), // generic; overridden
        task_kind: Some("extract".to_string()),
        route: "harness".to_string(),
        provider: "claude".to_string(),
        model: "claude-sonnet-4-6".to_string(),
        thinking_level: None,
        tokens_in: Some(900),
        tokens_out: Some(150),
        total_tokens: Some(1_050),
        cached_tokens: None,
        cost_micros: None,
        latency_ms: 30_000,
        success: true,
        error_code: None,
        error_message: None,
        correlation_id: "wi_email_m1".to_string(),
        tenant_or_project_scope: Some(CLIENT.to_string()),
        provider_request_id: None,
    });

    let guard = persistence.lock();
    let rows = store::list_recent(guard.connection_ref(), CLIENT, 10).expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].purpose, "calendar_event_extract");
    assert_eq!(rows[0].route, "harness");
    assert_eq!(rows[0].tokens_in, Some(900));
}

fn minimal_request() -> bos_integrations::llm_typed_tasks::TypedLlmTaskRequest {
    use bos_integrations::llm_typed_tasks::*;
    TypedLlmTaskRequest {
        task_id: "task-usage-1".to_string(),
        correlation_id: "corr-usage-1".to_string(),
        idempotency_key: "idem-usage-1".to_string(),
        tenant_or_project_scope: CLIENT.to_string(),
        source_entity: None,
        spec: TypedLlmTaskSpec {
            task_class: TypedLlmTaskClass::Classify,
            prompt_template_id: "usage_test.v1".to_string(),
            prompt_template_version: "1".to_string(),
            prompt_template_hash: String::new(),
            schema_ref: "usage.test.v1".to_string(),
            response_format: TypedLlmResponseFormat::JsonObject,
            max_input_bytes: 1024,
            max_output_bytes: 1024,
            max_tokens: 0,
            timeout_ms: 0,
            capabilities: TypedLlmTaskCapabilities::pure_transformation(),
            authority: TypedLlmAuthority::no_side_effects(),
        },
        input: TypedLlmTaskInput {
            json: serde_json::json!({"probe": true}),
            text_blocks: Vec::new(),
        },
        execution_policy: TypedLlmExecutionPolicy {
            default_route: TypedLlmExecutionRoute::DirectApi,
            fallback_policy: TypedLlmFallbackPolicy::FailClosed,
            retry_policy: TypedLlmRetryPolicy {
                max_attempts: 1,
                backoff_ms: 0,
                max_elapsed_ms: 0,
            },
        },
        provider_policy: TypedLlmProviderPolicy {
            preferred_provider: String::new(),
            preferred_model: String::new(),
            fallback_provider: None,
            fallback_model: None,
        },
        safety_policy: TypedLlmSafetyPolicy {
            redaction_policy: TypedLlmRedactionPolicy::PreSubmit,
            raw_output_retention: TypedLlmRawOutputRetention::None,
        },
    }
}

#[test]
fn unconfigured_api_call_records_a_failure_row() {
    let persistence = PersistencePool::open_in_memory().expect("db");
    let request = minimal_request();

    // Explicitly unconfigured API backend: the spine fails closed without
    // any network, and the recording seam must account for the failed call.
    let config = crate::llm::LlmRuntimeConfig {
        api_provider: crate::llm::LlmApiProvider::Anthropic,
        api_key: None,
        api_model: None,
        api_endpoint: None,
        local_api_key: None,
        local_endpoint: "http://127.0.0.1:11434/v1/chat/completions".to_string(),
        local_model: None,
        default_backend: crate::llm::LlmBackend::Api,
        default_model: None,
        harness_enabled: false,
        harness_program: "claude".to_string(),
        harness_model: None,
        harness_thinking_level: None,
        max_tokens: 1024,
        timeout_ms: 1_000,
        route_overrides: Default::default(),
        harness_result_root: std::env::temp_dir(),
    };
    let result = service::execute_recorded_with_config(
        persistence.clone(),
        CLIENT,
        "ai_usage_test_purpose",
        &request,
        &config,
    );
    assert!(result.is_err(), "unconfigured API must fail closed");

    let guard = persistence.lock();
    let rows = store::list_recent(guard.connection_ref(), CLIENT, 10).expect("list");
    assert_eq!(rows.len(), 1, "failed call must still be accounted");
    assert!(!rows[0].success);
    assert_eq!(rows[0].purpose, "ai_usage_test_purpose");
    assert_eq!(rows[0].route, "api");
    assert_eq!(
        rows[0].error_code.as_deref(),
        Some("llm_api_not_configured")
    );
}

#[test]
fn unconfigured_harness_model_records_a_failure_row() {
    let persistence = PersistencePool::open_in_memory().expect("db");
    let request = minimal_request();
    let config = crate::llm::LlmRuntimeConfig {
        api_provider: crate::llm::LlmApiProvider::Anthropic,
        api_key: None,
        api_model: None,
        api_endpoint: None,
        local_api_key: None,
        local_endpoint: "http://127.0.0.1:11434/v1/chat/completions".to_string(),
        local_model: None,
        default_backend: crate::llm::LlmBackend::Harness,
        default_model: None,
        harness_enabled: true,
        harness_program: "claude".to_string(),
        harness_model: None,
        harness_thinking_level: None,
        max_tokens: 1024,
        timeout_ms: 1_000,
        route_overrides: Default::default(),
        harness_result_root: std::env::temp_dir(),
    };

    let result = service::execute_recorded_with_config(
        persistence.clone(),
        CLIENT,
        "invoice_fill",
        &request,
        &config,
    );
    assert!(
        result.is_err(),
        "unconfigured harness model must fail closed"
    );

    let guard = persistence.lock();
    let rows = store::list_recent(guard.connection_ref(), CLIENT, 10).expect("list");
    assert_eq!(rows.len(), 1, "failed harness route must be accounted");
    assert_eq!(rows[0].route, "harness");
    assert_eq!(rows[0].provider, "claude");
    assert_eq!(
        rows[0].error_code.as_deref(),
        Some("llm_harness_model_not_configured")
    );
}

#[test]
fn missing_harness_program_records_a_failure_row() {
    let persistence = PersistencePool::open_in_memory().expect("db");
    let request = minimal_request();
    let config = crate::llm::LlmRuntimeConfig {
        api_provider: crate::llm::LlmApiProvider::Anthropic,
        api_key: None,
        api_model: None,
        api_endpoint: None,
        local_api_key: None,
        local_endpoint: "http://127.0.0.1:11434/v1/chat/completions".to_string(),
        local_model: None,
        default_backend: crate::llm::LlmBackend::Harness,
        default_model: None,
        harness_enabled: true,
        harness_program: "/tmp/bos-definitely-missing-claude-cli".to_string(),
        harness_model: Some("claude-sonnet-4-6".to_string()),
        harness_thinking_level: None,
        max_tokens: 1024,
        timeout_ms: 1_000,
        route_overrides: Default::default(),
        harness_result_root: std::env::temp_dir(),
    };

    let result = service::execute_recorded_with_config(
        persistence.clone(),
        CLIENT,
        "email_ai_triage",
        &request,
        &config,
    );
    assert!(result.is_err(), "missing harness program must fail closed");

    let guard = persistence.lock();
    let rows = store::list_recent(guard.connection_ref(), CLIENT, 10).expect("list");
    assert_eq!(rows.len(), 1, "failed harness route must be accounted");
    assert_eq!(rows[0].purpose, "email_ai_triage");
    assert_eq!(rows[0].route, "harness");
    assert_eq!(rows[0].provider, "claude");
    assert_eq!(
        rows[0].error_code.as_deref(),
        Some("llm_harness_program_not_found")
    );
}

#[test]
fn llm_settings_replace_is_receipted_and_revision_checked() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let request = LlmRouteSettingsUpdateRequest {
        expected_revision: None,
        idempotency_key: "llm-settings-1".to_string(),
        actor_id: None,
        global: LlmGlobalRouteSettingsUpdate {
            backend: "harness".to_string(),
            model: Some("claude-sonnet-4-6".to_string()),
            max_tokens: 2048,
            timeout_ms: 90_000,
        },
        overrides: vec![LlmPurposeRouteOverrideUpdate {
            purpose: "invoice_fill".to_string(),
            backend: "api".to_string(),
            model: Some("gpt-4.1-mini".to_string()),
        }],
    };

    let outcome = store::replace_llm_route_settings(
        conn,
        CLIENT,
        "operator",
        &request,
        test_known_purpose,
        10_000,
    )
    .expect("replace settings");
    let revision = match outcome {
        store_core::MutationOutcome::Applied { revision, .. } => revision,
        other => panic!("unexpected outcome: {other:?}"),
    };
    assert_eq!(revision, 1);

    let stored = store::get_llm_route_settings(conn, CLIENT)
        .expect("load settings")
        .expect("settings row");
    assert_eq!(stored.global.backend, "harness");
    assert_eq!(stored.global.timeout_ms, 90_000);
    assert_eq!(stored.overrides.len(), 1);
    assert_eq!(stored.revision, Some(1));

    let mut stale = request.clone();
    stale.expected_revision = Some(0);
    stale.idempotency_key = "llm-settings-stale".to_string();
    let conflict = store::replace_llm_route_settings(
        conn,
        CLIENT,
        "operator",
        &stale,
        test_known_purpose,
        10_100,
    )
    .expect("conflict outcome");
    assert!(matches!(
        conflict,
        store_core::MutationOutcome::RevisionConflict {
            current_revision: Some(1),
            ..
        }
    ));
}

#[test]
fn persisted_harness_settings_are_coerced_when_harness_is_unavailable() {
    let persistence = PersistencePool::open_in_memory().expect("db");
    let request = LlmRouteSettingsUpdateRequest {
        expected_revision: None,
        idempotency_key: "llm-settings-effective".to_string(),
        actor_id: None,
        global: LlmGlobalRouteSettingsUpdate {
            backend: "harness".to_string(),
            model: Some("claude-sonnet-4-6".to_string()),
            max_tokens: 1024,
            timeout_ms: 30_000,
        },
        overrides: vec![LlmPurposeRouteOverrideUpdate {
            purpose: "email_ai_triage".to_string(),
            backend: "api".to_string(),
            model: Some("gpt-4.1-mini".to_string()),
        }],
    };
    {
        let mut guard = persistence.lock();
        store::replace_llm_route_settings(
            guard.connection(),
            CLIENT,
            "operator",
            &request,
            test_known_purpose,
            12_000,
        )
        .expect("save settings");
    }

    let config = {
        let guard = persistence.lock();
        service::effective_config(guard.connection_ref(), CLIENT).expect("effective config")
    };
    assert_eq!(
        crate::llm::route_config_for_purpose(&config, "invoice_fill"),
        crate::llm::ResolvedLlmRoute {
            backend: crate::llm::LlmBackend::Api,
            model: None,
        }
    );
    assert_eq!(
        crate::llm::route_config_for_purpose(&config, "email_ai_triage"),
        crate::llm::ResolvedLlmRoute {
            backend: crate::llm::LlmBackend::Api,
            model: Some("gpt-4.1-mini".to_string()),
        }
    );

    let response = {
        let guard = persistence.lock();
        service::settings_response(guard.connection_ref(), CLIENT).expect("settings response")
    };
    assert!(!response.harness_available);
    assert_eq!(response.global.backend, "api");
    assert_eq!(response.global.model, None);
    let invoice = response
        .purposes
        .iter()
        .find(|purpose| purpose.purpose == "invoice_fill")
        .expect("invoice purpose");
    assert_eq!(invoice.effective_backend, "api");
    assert_eq!(invoice.effective_model, None);
}

#[test]
fn llm_settings_reject_unknown_override_purposes() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let request = LlmRouteSettingsUpdateRequest {
        expected_revision: None,
        idempotency_key: "llm-settings-unknown-purpose".to_string(),
        actor_id: None,
        global: LlmGlobalRouteSettingsUpdate {
            backend: "api".to_string(),
            model: Some("gpt-4.1-mini".to_string()),
            max_tokens: 2048,
            timeout_ms: 90_000,
        },
        overrides: vec![LlmPurposeRouteOverrideUpdate {
            purpose: "not_registered".to_string(),
            backend: "harness".to_string(),
            model: Some("claude-sonnet-4-6".to_string()),
        }],
    };

    let err = store::replace_llm_route_settings(
        conn,
        CLIENT,
        "operator",
        &request,
        test_known_purpose,
        10_000,
    )
    .expect_err("unknown purpose must be rejected");
    assert_eq!(err.to_string(), "store domain error: llm_purpose_unknown");
    assert!(
        store::get_llm_route_settings(conn, CLIENT)
            .expect("settings lookup")
            .is_none(),
        "rejected settings must not be persisted"
    );
}

#[test]
fn claude_authorization_url_parser_accepts_only_claude_https() {
    let line =
        "If the browser didn't open, visit: https://claude.com/cai/oauth/authorize?state=abc\n";
    assert_eq!(
        service::extract_claude_authorization_url(line).as_deref(),
        Some("https://claude.com/cai/oauth/authorize?state=abc")
    );
    assert!(service::extract_claude_authorization_url(
        "visit: http://claude.com/cai/oauth/authorize"
    )
    .is_none());
    assert!(service::extract_claude_authorization_url(
        "visit: https://example.com/cai/oauth/authorize"
    )
    .is_none());
}

#[test]
fn claude_authorization_code_validation_rejects_blank_and_control_chars() {
    assert_eq!(
        service::validate_claude_authorization_code("  one-time-code  "),
        Ok("one-time-code")
    );
    assert!(service::validate_claude_authorization_code("   ").is_err());
    assert!(service::validate_claude_authorization_code("code\ninjection").is_err());
}

#[test]
fn claude_subscription_actions_are_receipted_without_the_code() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let outcome = store::record_claude_subscription_action(
        conn,
        CLIENT,
        "operator",
        "claude_auth_flow_1",
        "authorization_code_submitted",
        "claude-auth-submit-1",
        20_000,
    )
    .expect("record auth action");
    assert!(matches!(
        outcome,
        store_core::MutationOutcome::Applied { .. }
    ));
    let receipts = store_core::receipts_for_entity(
        conn,
        CLIENT,
        store::CLAUDE_SUBSCRIPTION_AUTH_ENTITY_KIND,
        "claude_auth_flow_1",
        10,
    )
    .expect("receipts");
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].change_kind, "authorization_code_submitted");

    let after_json: Option<String> = conn
        .query_row(
            "SELECT after_json FROM receipts WHERE receipt_id = ?1",
            rusqlite::params![receipts[0].receipt_id],
            |row| row.get(0),
        )
        .expect("receipt payload");
    let after_json = after_json.expect("safe status payload");
    assert!(after_json.contains("authorization_submitted"));
    assert!(!after_json.contains("one-time-code"));
    assert!(
        store::claude_subscription_action_was_applied(conn, CLIENT, "claude-auth-submit-1")
            .expect("idempotency lookup")
    );

    store::record_claude_subscription_failure(
        conn,
        CLIENT,
        "operator",
        "claude_auth_flow_2",
        "claude-auth-submit-2:submit_failed",
        "llm_subscription_auth_flow_not_found",
        20_001,
    )
    .expect("record auth failure");
    let failure_outcome: String = conn
        .query_row(
            "SELECT outcome FROM receipts WHERE idempotency_key = ?1",
            rusqlite::params!["claude-auth-submit-2:submit_failed"],
            |row| row.get(0),
        )
        .expect("failure receipt");
    assert_eq!(failure_outcome, "failed");
    assert!(
        !store::claude_subscription_action_was_applied(conn, CLIENT, "claude-auth-submit-2")
            .expect("failed action must remain retryable")
    );
}

#[cfg(unix)]
#[test]
fn claude_subscription_status_kills_a_stalled_cli() {
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let program = std::env::temp_dir().join(format!(
        "bos-stalled-claude-status-{}-{unique}",
        std::process::id()
    ));
    std::fs::write(&program, "#!/bin/sh\nexec sleep 30\n").expect("fake stalled cli");
    let mut permissions = std::fs::metadata(&program)
        .expect("fake cli metadata")
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&program, permissions).expect("fake cli executable");

    let started = std::time::Instant::now();
    assert!(service::run_claude_auth_status_with_timeout(
        &program.display().to_string(),
        Duration::from_millis(50)
    )
    .is_none());
    assert!(started.elapsed() < Duration::from_secs(2));
    std::fs::remove_file(program).expect("remove fake cli");
}

#[cfg(unix)]
#[test]
fn claude_subscription_flow_drives_the_configured_cli_without_persisting_the_code() {
    use std::collections::BTreeMap;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "bos-claude-subscription-test-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("test dir");
    let program = root.join("fake-claude");
    std::fs::write(
        &program,
        r#"#!/bin/sh
status_file="$(dirname "$0")/connected"
if [ "$1" = "auth" ] && [ "$2" = "status" ]; then
  if [ -f "$status_file" ]; then
    printf '%s\n' '{"loggedIn":true,"authMethod":"claude.ai","subscriptionType":"max"}'
    exit 0
  fi
  printf '%s\n' '{"loggedIn":false,"authMethod":"none"}'
  exit 1
fi
if [ "$1" = "auth" ] && [ "$2" = "login" ]; then
  printf '%s\n' 'If the browser did not open, visit: https://claude.com/cai/oauth/authorize?state=test'
  IFS= read -r code
  if [ "$code" = "one-time-code" ]; then
    : > "$status_file"
    exit 0
  fi
fi
exit 1
"#,
    )
    .expect("fake cli");
    let mut permissions = std::fs::metadata(&program)
        .expect("fake cli metadata")
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&program, permissions).expect("fake cli executable");

    let config = crate::llm::LlmRuntimeConfig {
        api_provider: crate::llm::LlmApiProvider::Anthropic,
        api_key: None,
        api_model: None,
        api_endpoint: None,
        local_api_key: None,
        local_endpoint: "http://127.0.0.1:11434/v1/chat/completions".to_string(),
        local_model: None,
        default_backend: crate::llm::LlmBackend::Harness,
        default_model: Some("claude-sonnet-4-6".to_string()),
        harness_enabled: true,
        harness_program: program.display().to_string(),
        harness_model: Some("claude-sonnet-4-6".to_string()),
        harness_thinking_level: None,
        max_tokens: 4_096,
        timeout_ms: 120_000,
        route_overrides: BTreeMap::new(),
        harness_result_root: root.join("runs"),
    };

    let initial = service::claude_subscription_status(&config);
    assert!(initial.available);
    assert!(!initial.connected);
    let started_at_ms = crate::http::now_ms();
    let flow = service::start_claude_subscription_auth(&config, "operator", started_at_ms)
        .expect("start auth");
    assert!(flow.authorization_url.starts_with("https://claude.com/"));
    service::submit_claude_subscription_code(
        &flow.flow_id,
        "operator",
        "one-time-code",
        started_at_ms.saturating_add(100),
    )
    .expect("submit code");

    let connected = (0..100).any(|_| {
        let status = service::claude_subscription_status(&config);
        if status.connected {
            true
        } else {
            std::thread::sleep(Duration::from_millis(50));
            false
        }
    });
    assert!(connected, "fake Claude CLI should report connected");
    let status = service::claude_subscription_status(&config);
    assert_eq!(status.auth_method.as_deref(), Some("claude.ai"));
    assert_eq!(status.subscription_type.as_deref(), Some("max"));
    assert!(
        !std::fs::read_to_string(root.join("connected"))
            .expect("status marker")
            .contains("one-time-code"),
        "the fake provider state must not contain the submitted code"
    );
    std::fs::remove_dir_all(root).expect("remove test dir");
}
