use bos_contracts::admin_settings::{
    AdminSettingSource, AdminSettingUpdateRequest, AdminSettingValueKind,
};

use super::service;
use crate::env_registry;
use crate::http::test_support::EnvGuard;
use crate::persistence::Persistence;
use crate::store_core::{MutationOutcome, StoreError};

const CLIENT: &str = "test-client";
const EXPECTED_RUNTIME_EDITABLE_NAMES: &[&str] = &[
    "BOS_ACCOUNTING_MAX_REQUESTS_PER_CYCLE",
    "BOS_ACCOUNTING_SYNC_ENABLED",
    "BOS_ACCOUNTING_SYNC_INTERVAL_SECS",
    "BOS_ACCOUNTING_VISIBILITY_POLICY",
    "BOS_AGENT_EVIDENCE_CLEANUP_ENABLED",
    "BOS_AGENT_EVIDENCE_CLEANUP_INTERVAL_SECS",
    "BOS_AGENT_EVIDENCE_MAX_BYTES",
    "BOS_AGENT_EVIDENCE_RETENTION_DAYS",
    "BOS_AI_TRIAGE_ENABLED",
    "BOS_AI_TRIAGE_MAX_LLM_CALLS_PER_CYCLE",
    "BOS_AI_TRIAGE_MIN_CONFIDENCE",
    "BOS_AI_TRIAGE_PACKET_PROPOSALS_ENABLED",
    "BOS_AUTO_PRODUCE_ENABLED",
    "BOS_AUTO_PRODUCE_INTERVAL_SECS",
    "BOS_AUTO_PRODUCE_MAX_PER_CYCLE",
    "BOS_BUFFER_WRITE_ENABLED",
    "BOS_CALL_INPUTS_AUDIO_TRANSCRIPTION_ENABLED",
    "BOS_CALL_INPUTS_SYNC_ENABLED",
    "BOS_CALL_INPUTS_SYNC_INTERVAL_SECS",
    "BOS_CLAIMS_MAX_REQUESTS_PER_CYCLE",
    "BOS_CLAIMS_SYNC_ENABLED",
    "BOS_CLAIMS_SYNC_INTERVAL_SECS",
    "BOS_CONTENT_PUBLISH_WRITE_ENABLED",
    "BOS_CONTENT_WEB_FACTS_ENABLED",
    "BOS_CRM_CONTEXT_NEUTRAL_SENDER_DOMAINS",
    "BOS_CRM_DEAL_VISIBILITY_POLICY",
    "BOS_CRM_READ_MAX_REQUESTS_PER_CYCLE",
    "BOS_CRM_READ_SYNC_ENABLED",
    "BOS_CRM_READ_SYNC_INTERVAL_SECS",
    "BOS_DATA_RETENTION_BATCH_SIZE",
    "BOS_DATA_RETENTION_EMAIL_BODY_DAYS",
    "BOS_DATA_RETENTION_ENABLED",
    "BOS_DATA_RETENTION_INCREMENTAL_VACUUM_PAGES",
    "BOS_DATA_RETENTION_INTERVAL_SECS",
    "BOS_DATA_RETENTION_MAX_ROWS_PER_CYCLE",
    "BOS_DATA_RETENTION_RECEIPT_PAYLOAD_DAYS",
    "BOS_DRIVE_MAX_REQUESTS_PER_CYCLE",
    "BOS_DRIVE_SYNC_ENABLED",
    "BOS_DRIVE_SYNC_INTERVAL_SECS",
    "BOS_EMAIL_ENRICHMENT_BACKFILL_BATCH",
    "BOS_EMAIL_ENRICHMENT_BACKFILL_ENABLED",
    "BOS_ENRICHMENT_FRESHNESS_ENABLED",
    "BOS_ENRICHMENT_FRESHNESS_INTERVAL_SECS",
    "BOS_ENRICHMENT_FRESHNESS_MAX_ENRICHMENTS_PER_CYCLE",
    "BOS_ENRICHMENT_FRESHNESS_STALE_AFTER_SECS",
    "BOS_ESPOCRM_WRITE_ENABLED",
    "BOS_GMAIL_INGEST_ENABLED",
    "BOS_GMAIL_INGEST_INTERVAL_SECS",
    "BOS_GMAIL_WRITE_ENABLED",
    "BOS_GOOGLE_CALENDAR_WRITE_ENABLED",
    "BOS_HUBSPOT_WRITE_ENABLED",
    "BOS_INVOICE_NINJA_WRITE_ENABLED",
    "BOS_LEAD_DISCOVERY_AUTOSCRAPE_ENABLED",
    "BOS_LEAD_DISCOVERY_AUTOSCRAPE_INTERVAL_SECS",
    "BOS_LEAD_DISCOVERY_AUTOSCRAPE_MAX_FINDINGS_PER_CYCLE",
    "BOS_OUTBOX_DELIVERY_ENABLED",
    "BOS_OUTBOX_DELIVERY_INTERVAL_SECS",
    "BOS_PACKET_PROPOSAL_TOOL_LOOP_ENABLED",
    "BOS_QBO_WRITE_ENABLED",
    "BOS_REPORT_DIGEST_DELIVERY_ENABLED",
    "BOS_REPORT_DIGEST_ENABLED",
    "BOS_REPORT_DIGEST_INTERVAL_SECS",
    "BOS_REPORT_DIGEST_TO_ADDR",
    "BOS_SEARCH_CONSOLE_MAX_REQUESTS_PER_CYCLE",
    "BOS_SEARCH_CONSOLE_SYNC_ENABLED",
    "BOS_SEARCH_CONSOLE_SYNC_INTERVAL_SECS",
    "BOS_SHOPIFY_READ_SYNC_ENABLED",
    "BOS_SHOPIFY_READ_SYNC_INTERVAL_SECS",
    "BOS_SHOPIFY_READ_SYNC_MAX_ORDERS_PER_CYCLE",
    "BOS_SHOPIFY_SALES_VISIBILITY_POLICY",
    "BOS_SHOPIFY_WRITE_ENABLED",
    "BOS_STOCKFORGE_MAX_REQUESTS_PER_CYCLE",
    "BOS_STOCKFORGE_SYNC_ENABLED",
    "BOS_STOCKFORGE_SYNC_INTERVAL_SECS",
    "BOS_STRIPE_WRITE_ENABLED",
    "BOS_WEB_ENRICHMENT_ENABLED",
];

#[test]
fn runtime_editable_allowlist_is_safe() {
    let actual_names = service::runtime_editable_vars()
        .map(|var| var.name)
        .collect::<Vec<_>>();
    assert_eq!(actual_names, EXPECTED_RUNTIME_EDITABLE_NAMES);

    for var in service::runtime_editable_vars() {
        assert!(
            env_registry::ALL
                .iter()
                .any(|candidate| candidate.name == var.name),
            "{} must be registered",
            var.name
        );
        assert!(!var.secret, "{} must not be secret", var.name);
        assert_ne!(
            var.group,
            env_registry::EnvVarGroup::InfraServer,
            "{} must not be infra",
            var.name
        );
    }

    let persistence = Persistence::open_in_memory().expect("persistence");
    let response = service::settings_response(persistence.connection_ref(), CLIENT)
        .expect("settings response");
    for name in EXPECTED_RUNTIME_EDITABLE_NAMES {
        let row = response
            .settings
            .iter()
            .find(|row| row.name == *name)
            .expect("editable setting row");
        assert!(row.editable, "{name} must be editable");
        assert!(row.value_kind.is_some(), "{name} must expose a value kind");
        if row.value_kind == Some(AdminSettingValueKind::Enum) {
            assert!(
                row.allowed_values
                    .as_ref()
                    .is_some_and(|values| !values.is_empty()),
                "{name} enum must expose allowed values"
            );
        } else {
            assert!(
                row.allowed_values.is_none(),
                "{name} non-enum must not expose allowed values"
            );
        }
    }
    assert!(
        response
            .settings
            .iter()
            .all(|row| row.read_only_reason.as_deref()
                != Some("read-only; not wired for runtime override")),
        "generic read-only reason must not appear"
    );
}

#[test]
fn settings_response_redacts_secrets_and_marks_only_wired_vars_editable() {
    let persistence = Persistence::open_in_memory().expect("persistence");
    let conn = persistence.connection_ref();

    let response = service::settings_response(conn, CLIENT).expect("settings response");
    let api_key = response
        .settings
        .iter()
        .find(|row| row.name == env_registry::BOS_LLM_API_KEY.name)
        .expect("llm api key row");
    assert!(api_key.secret);
    assert!(!api_key.editable);
    assert!(api_key.effective_value.is_none());

    let auto_produce = response
        .settings
        .iter()
        .find(|row| row.name == env_registry::BOS_AUTO_PRODUCE_ENABLED.name)
        .expect("auto produce row");
    assert!(auto_produce.editable);

    let claim_to_addr = response
        .settings
        .iter()
        .find(|row| row.name == env_registry::BOS_CLAIM_DRAFT_TO_ADDR.name)
        .expect("claim draft to addr row");
    assert!(!claim_to_addr.editable);

    for gate in [
        env_registry::BOS_AGENT_LAUNCH_ENABLED.name,
        env_registry::BOS_AGENT_MCP_ENABLED.name,
    ] {
        let row = response
            .settings
            .iter()
            .find(|row| row.name == gate)
            .expect("security gate row");
        assert!(!row.editable, "{gate} must remain read-only");
        assert_eq!(
            row.read_only_reason.as_deref(),
            Some("security gate — env-only by policy")
        );
    }
}

#[test]
fn settings_response_can_show_overlay_backed_effective_values() {
    let persistence = Persistence::open_in_memory().expect("persistence");
    let conn = persistence.connection_ref();

    let response = service::settings_response_with_overlay(
        conn,
        CLIENT,
        &[service::OverlayRuntimeValue {
            var_name: env_registry::BOS_ACCOUNTING_VISIBILITY_POLICY.name,
            value: "shared".into(),
        }],
    )
    .expect("settings response");

    let accounting_visibility = response
        .settings
        .iter()
        .find(|row| row.name == env_registry::BOS_ACCOUNTING_VISIBILITY_POLICY.name)
        .expect("accounting visibility row");
    assert!(accounting_visibility.editable);
    assert_eq!(
        accounting_visibility.source,
        AdminSettingSource::OverlayDefault
    );
    assert_eq!(
        accounting_visibility.effective_value.as_deref(),
        Some("shared")
    );
    assert_eq!(accounting_visibility.default_value.as_deref(), None);
    assert_eq!(
        accounting_visibility.value_kind,
        Some(AdminSettingValueKind::Enum)
    );
    assert_eq!(
        accounting_visibility.allowed_values.as_deref(),
        Some(
            &[
                "shared".to_string(),
                "admin_only".to_string(),
                "authorizer_only".to_string()
            ][..]
        )
    );
}

#[test]
fn settings_response_shows_overlay_tier_mapping_when_env_is_unset() {
    let _env = EnvGuard::unset("BOS_SHOPIFY_TIER_MAPPING_JSON");
    let persistence = Persistence::open_in_memory().expect("persistence");
    let conn = persistence.connection_ref();

    let response = service::settings_response_with_overlay(
        conn,
        CLIENT,
        &[service::OverlayRuntimeValue {
            var_name: env_registry::BOS_SHOPIFY_TIER_MAPPING_JSON.name,
            value: r#"{"wholesale":{"tag":"Wholesale"}}"#.into(),
        }],
    )
    .expect("settings response");

    let mapping = response
        .settings
        .iter()
        .find(|row| row.name == env_registry::BOS_SHOPIFY_TIER_MAPPING_JSON.name)
        .expect("tier mapping row");
    assert!(!mapping.editable);
    assert_eq!(mapping.source, AdminSettingSource::OverlayDefault);
    assert_eq!(
        mapping.effective_value.as_deref(),
        Some(r#"{"wholesale":{"tag":"Wholesale"}}"#)
    );
}

#[test]
fn settings_response_env_value_wins_over_overlay_display() {
    let _env = EnvGuard::set(
        "BOS_SHOPIFY_TIER_MAPPING_JSON",
        r#"{"Retail":{"tag":"Retail"}}"#,
    );
    let persistence = Persistence::open_in_memory().expect("persistence");
    let conn = persistence.connection_ref();

    let response = service::settings_response_with_overlay(
        conn,
        CLIENT,
        &[service::OverlayRuntimeValue {
            var_name: env_registry::BOS_SHOPIFY_TIER_MAPPING_JSON.name,
            value: r#"{"wholesale":{"tag":"Wholesale"}}"#.into(),
        }],
    )
    .expect("settings response");

    let mapping = response
        .settings
        .iter()
        .find(|row| row.name == env_registry::BOS_SHOPIFY_TIER_MAPPING_JSON.name)
        .expect("tier mapping row");
    assert_eq!(mapping.source, AdminSettingSource::EnvDefault);
    assert_eq!(
        mapping.effective_value.as_deref(),
        Some(r#"{"Retail":{"tag":"Retail"}}"#)
    );
}

#[test]
fn override_replace_is_receipted_revisioned_and_used_by_resolver() {
    let mut persistence = Persistence::open_in_memory().expect("persistence");
    let conn = persistence.connection();
    let request = AdminSettingUpdateRequest {
        expected_revision: None,
        idempotency_key: "runtime-setting-1".to_string(),
        actor_id: None,
        value: "1".to_string(),
    };

    let outcome = service::upsert_setting(
        conn,
        CLIENT,
        "operator",
        env_registry::BOS_AI_TRIAGE_ENABLED.name,
        &request,
        10_000,
    )
    .expect("upsert setting");
    assert!(matches!(
        outcome,
        MutationOutcome::Applied { revision: 1, .. }
    ));
    assert!(
        service::flag(conn, CLIENT, &env_registry::BOS_AI_TRIAGE_ENABLED).expect("resolver flag")
    );

    let stale = AdminSettingUpdateRequest {
        expected_revision: Some(0),
        idempotency_key: "runtime-setting-stale".to_string(),
        actor_id: None,
        value: "0".to_string(),
    };
    let conflict = service::upsert_setting(
        conn,
        CLIENT,
        "operator",
        env_registry::BOS_AI_TRIAGE_ENABLED.name,
        &stale,
        10_100,
    )
    .expect("stale update returns mutation outcome");
    assert!(matches!(
        conflict,
        MutationOutcome::RevisionConflict {
            current_revision: Some(1),
            ..
        }
    ));
}

#[test]
fn non_wired_vars_are_rejected_for_override() {
    let mut persistence = Persistence::open_in_memory().expect("persistence");
    let request = AdminSettingUpdateRequest {
        expected_revision: None,
        idempotency_key: "runtime-setting-reject".to_string(),
        actor_id: None,
        value: "medium".to_string(),
    };

    let err = service::upsert_setting(
        persistence.connection(),
        CLIENT,
        "operator",
        env_registry::BOS_CLAIM_DRAFT_TO_ADDR.name,
        &request,
        10_000,
    )
    .expect_err("non-wired override rejected");
    assert!(matches!(
        err,
        StoreError::Domain(code) if code == "runtime_setting_not_editable"
    ));
}

#[test]
fn invalid_editable_values_are_rejected_before_store() {
    let mut persistence = Persistence::open_in_memory().expect("persistence");
    let uint_request = AdminSettingUpdateRequest {
        expected_revision: None,
        idempotency_key: "runtime-setting-invalid-uint".to_string(),
        actor_id: None,
        value: "abc".to_string(),
    };

    let err = service::upsert_setting(
        persistence.connection(),
        CLIENT,
        "operator",
        env_registry::BOS_AUTO_PRODUCE_MAX_PER_CYCLE.name,
        &uint_request,
        10_000,
    )
    .expect_err("invalid uint override rejected");
    assert!(matches!(
        err,
        StoreError::Domain(code) if code == "runtime_setting_invalid_value"
    ));

    let enum_request = AdminSettingUpdateRequest {
        expected_revision: None,
        idempotency_key: "runtime-setting-invalid-enum".to_string(),
        actor_id: None,
        value: "HIGH".to_string(),
    };
    let err = service::upsert_setting(
        persistence.connection(),
        CLIENT,
        "operator",
        env_registry::BOS_AI_TRIAGE_MIN_CONFIDENCE.name,
        &enum_request,
        10_100,
    )
    .expect_err("invalid enum override rejected");
    assert!(matches!(
        err,
        StoreError::Domain(code) if code == "runtime_setting_invalid_value"
    ));
}

#[test]
fn valid_editable_bool_uint_and_enum_values_are_applied() {
    let mut persistence = Persistence::open_in_memory().expect("persistence");
    let conn = persistence.connection();
    let uint_request = AdminSettingUpdateRequest {
        expected_revision: None,
        idempotency_key: "runtime-setting-valid-uint".to_string(),
        actor_id: None,
        value: "42".to_string(),
    };
    let bool_request = AdminSettingUpdateRequest {
        expected_revision: None,
        idempotency_key: "runtime-setting-valid-bool".to_string(),
        actor_id: None,
        value: "YeS".to_string(),
    };
    let enum_request = AdminSettingUpdateRequest {
        expected_revision: None,
        idempotency_key: "runtime-setting-valid-enum".to_string(),
        actor_id: None,
        value: "shared".to_string(),
    };
    let confidence_request = AdminSettingUpdateRequest {
        expected_revision: None,
        idempotency_key: "runtime-setting-valid-confidence".to_string(),
        actor_id: None,
        value: "high".to_string(),
    };

    let uint_outcome = service::upsert_setting(
        conn,
        CLIENT,
        "operator",
        env_registry::BOS_AUTO_PRODUCE_MAX_PER_CYCLE.name,
        &uint_request,
        10_000,
    )
    .expect("valid uint override");
    let bool_outcome = service::upsert_setting(
        conn,
        CLIENT,
        "operator",
        env_registry::BOS_AUTO_PRODUCE_ENABLED.name,
        &bool_request,
        10_100,
    )
    .expect("valid bool override");
    let enum_outcome = service::upsert_setting(
        conn,
        CLIENT,
        "operator",
        env_registry::BOS_ACCOUNTING_VISIBILITY_POLICY.name,
        &enum_request,
        10_200,
    )
    .expect("valid enum override");
    let confidence_outcome = service::upsert_setting(
        conn,
        CLIENT,
        "operator",
        env_registry::BOS_AI_TRIAGE_MIN_CONFIDENCE.name,
        &confidence_request,
        10_300,
    )
    .expect("valid confidence enum override");

    assert!(matches!(
        uint_outcome,
        MutationOutcome::Applied { revision: 1, .. }
    ));
    assert!(matches!(
        bool_outcome,
        MutationOutcome::Applied { revision: 1, .. }
    ));
    assert!(matches!(
        enum_outcome,
        MutationOutcome::Applied { revision: 1, .. }
    ));
    assert!(matches!(
        confidence_outcome,
        MutationOutcome::Applied { revision: 1, .. }
    ));
}
