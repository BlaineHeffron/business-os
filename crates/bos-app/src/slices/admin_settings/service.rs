use std::collections::BTreeMap;

use bos_contracts::admin_settings::{
    AdminSettingClearRequest, AdminSettingRow, AdminSettingSource, AdminSettingUpdateRequest,
    AdminSettingValueKind, AdminSettingsResponse,
};

use super::store;
use crate::env_registry::{self, EnvVar, EnvVarGroup};
use crate::store_core::{MutationOutcome, StoreError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditableValueKind {
    Bool,
    Uint,
    String,
    Enum { allowed: &'static [&'static str] },
}

struct RuntimeEditableVar {
    var: &'static EnvVar,
    kind: EditableValueKind,
}

pub struct OverlayRuntimeValue<'a> {
    pub var_name: &'static str,
    pub value: std::borrow::Cow<'a, str>,
}

const AI_CONFIDENCE_VALUES: &[&str] = &["high", "medium", "low"];
const VISIBILITY_POLICY_VALUES: &[&str] = &["shared", "admin_only", "authorizer_only"];

macro_rules! runtime_editable_vars {
    ($(($var:ident, $kind:expr)),+ $(,)?) => {
        const RUNTIME_EDITABLE_VARS: &[RuntimeEditableVar] = &[
            $(
                RuntimeEditableVar {
                    var: &env_registry::$var,
                    kind: $kind,
                },
            )+
        ];
    };
}

runtime_editable_vars!(
    (
        BOS_ACCOUNTING_MAX_REQUESTS_PER_CYCLE,
        EditableValueKind::Uint
    ),
    (BOS_ACCOUNTING_SYNC_ENABLED, EditableValueKind::Bool),
    (BOS_ACCOUNTING_SYNC_INTERVAL_SECS, EditableValueKind::Uint),
    (
        BOS_ACCOUNTING_VISIBILITY_POLICY,
        EditableValueKind::Enum {
            allowed: VISIBILITY_POLICY_VALUES
        }
    ),
    (BOS_AGENT_EVIDENCE_CLEANUP_ENABLED, EditableValueKind::Bool),
    (
        BOS_AGENT_EVIDENCE_CLEANUP_INTERVAL_SECS,
        EditableValueKind::Uint
    ),
    (BOS_AGENT_EVIDENCE_MAX_BYTES, EditableValueKind::Uint),
    (BOS_AGENT_EVIDENCE_RETENTION_DAYS, EditableValueKind::Uint),
    (BOS_AI_TRIAGE_ENABLED, EditableValueKind::Bool),
    (
        BOS_AI_TRIAGE_MAX_LLM_CALLS_PER_CYCLE,
        EditableValueKind::Uint
    ),
    (
        BOS_AI_TRIAGE_MIN_CONFIDENCE,
        EditableValueKind::Enum {
            allowed: AI_CONFIDENCE_VALUES
        }
    ),
    (
        BOS_AI_TRIAGE_PACKET_PROPOSALS_ENABLED,
        EditableValueKind::Bool
    ),
    (BOS_AUTO_PRODUCE_ENABLED, EditableValueKind::Bool),
    (BOS_AUTO_PRODUCE_INTERVAL_SECS, EditableValueKind::Uint),
    (BOS_AUTO_PRODUCE_MAX_PER_CYCLE, EditableValueKind::Uint),
    (BOS_BUFFER_WRITE_ENABLED, EditableValueKind::Bool),
    (
        BOS_CALL_INPUTS_AUDIO_TRANSCRIPTION_ENABLED,
        EditableValueKind::Bool
    ),
    (BOS_CALL_INPUTS_SYNC_ENABLED, EditableValueKind::Bool),
    (BOS_CALL_INPUTS_SYNC_INTERVAL_SECS, EditableValueKind::Uint),
    (BOS_CLAIMS_MAX_REQUESTS_PER_CYCLE, EditableValueKind::Uint),
    (BOS_CLAIMS_SYNC_ENABLED, EditableValueKind::Bool),
    (BOS_CLAIMS_SYNC_INTERVAL_SECS, EditableValueKind::Uint),
    (BOS_CONTENT_PUBLISH_WRITE_ENABLED, EditableValueKind::Bool),
    (BOS_CONTENT_WEB_FACTS_ENABLED, EditableValueKind::Bool),
    (
        BOS_CRM_CONTEXT_NEUTRAL_SENDER_DOMAINS,
        EditableValueKind::String
    ),
    (
        BOS_CRM_DEAL_VISIBILITY_POLICY,
        EditableValueKind::Enum {
            allowed: VISIBILITY_POLICY_VALUES
        }
    ),
    (BOS_CRM_READ_MAX_REQUESTS_PER_CYCLE, EditableValueKind::Uint),
    (BOS_CRM_READ_SYNC_ENABLED, EditableValueKind::Bool),
    (BOS_CRM_READ_SYNC_INTERVAL_SECS, EditableValueKind::Uint),
    (BOS_DATA_RETENTION_BATCH_SIZE, EditableValueKind::Uint),
    (BOS_DATA_RETENTION_EMAIL_BODY_DAYS, EditableValueKind::Uint),
    (BOS_DATA_RETENTION_ENABLED, EditableValueKind::Bool),
    (
        BOS_DATA_RETENTION_INCREMENTAL_VACUUM_PAGES,
        EditableValueKind::Uint
    ),
    (BOS_DATA_RETENTION_INTERVAL_SECS, EditableValueKind::Uint),
    (
        BOS_DATA_RETENTION_MAX_ROWS_PER_CYCLE,
        EditableValueKind::Uint
    ),
    (
        BOS_DATA_RETENTION_RECEIPT_PAYLOAD_DAYS,
        EditableValueKind::Uint
    ),
    (BOS_DRIVE_MAX_REQUESTS_PER_CYCLE, EditableValueKind::Uint),
    (BOS_DRIVE_SYNC_ENABLED, EditableValueKind::Bool),
    (BOS_DRIVE_SYNC_INTERVAL_SECS, EditableValueKind::Uint),
    (BOS_EMAIL_ENRICHMENT_BACKFILL_BATCH, EditableValueKind::Uint),
    (
        BOS_EMAIL_ENRICHMENT_BACKFILL_ENABLED,
        EditableValueKind::Bool
    ),
    (BOS_ENRICHMENT_FRESHNESS_ENABLED, EditableValueKind::Bool),
    (
        BOS_ENRICHMENT_FRESHNESS_INTERVAL_SECS,
        EditableValueKind::Uint
    ),
    (
        BOS_ENRICHMENT_FRESHNESS_MAX_ENRICHMENTS_PER_CYCLE,
        EditableValueKind::Uint
    ),
    (
        BOS_ENRICHMENT_FRESHNESS_STALE_AFTER_SECS,
        EditableValueKind::Uint
    ),
    (BOS_ESPOCRM_WRITE_ENABLED, EditableValueKind::Bool),
    (BOS_GMAIL_INGEST_ENABLED, EditableValueKind::Bool),
    (BOS_GMAIL_INGEST_INTERVAL_SECS, EditableValueKind::Uint),
    (BOS_GMAIL_WRITE_ENABLED, EditableValueKind::Bool),
    (BOS_GOOGLE_CALENDAR_WRITE_ENABLED, EditableValueKind::Bool),
    (BOS_HUBSPOT_WRITE_ENABLED, EditableValueKind::Bool),
    (BOS_INVOICE_NINJA_WRITE_ENABLED, EditableValueKind::Bool),
    (
        BOS_LEAD_DISCOVERY_AUTOSCRAPE_ENABLED,
        EditableValueKind::Bool
    ),
    (
        BOS_LEAD_DISCOVERY_AUTOSCRAPE_INTERVAL_SECS,
        EditableValueKind::Uint
    ),
    (
        BOS_LEAD_DISCOVERY_AUTOSCRAPE_MAX_FINDINGS_PER_CYCLE,
        EditableValueKind::Uint
    ),
    (BOS_OUTBOX_DELIVERY_ENABLED, EditableValueKind::Bool),
    (BOS_OUTBOX_DELIVERY_INTERVAL_SECS, EditableValueKind::Uint),
    (
        BOS_PACKET_PROPOSAL_TOOL_LOOP_ENABLED,
        EditableValueKind::Bool
    ),
    (BOS_QBO_WRITE_ENABLED, EditableValueKind::Bool),
    (BOS_REPORT_DIGEST_DELIVERY_ENABLED, EditableValueKind::Bool),
    (BOS_REPORT_DIGEST_ENABLED, EditableValueKind::Bool),
    (BOS_REPORT_DIGEST_INTERVAL_SECS, EditableValueKind::Uint),
    (BOS_REPORT_DIGEST_TO_ADDR, EditableValueKind::String),
    (
        BOS_SEARCH_CONSOLE_MAX_REQUESTS_PER_CYCLE,
        EditableValueKind::Uint
    ),
    (BOS_SEARCH_CONSOLE_SYNC_ENABLED, EditableValueKind::Bool),
    (
        BOS_SEARCH_CONSOLE_SYNC_INTERVAL_SECS,
        EditableValueKind::Uint
    ),
    (BOS_SHOPIFY_READ_SYNC_ENABLED, EditableValueKind::Bool),
    (BOS_SHOPIFY_READ_SYNC_INTERVAL_SECS, EditableValueKind::Uint),
    (
        BOS_SHOPIFY_READ_SYNC_MAX_ORDERS_PER_CYCLE,
        EditableValueKind::Uint
    ),
    (
        BOS_SHOPIFY_SALES_VISIBILITY_POLICY,
        EditableValueKind::Enum {
            allowed: VISIBILITY_POLICY_VALUES
        }
    ),
    (BOS_SHOPIFY_WRITE_ENABLED, EditableValueKind::Bool),
    (
        BOS_STOCKFORGE_MAX_REQUESTS_PER_CYCLE,
        EditableValueKind::Uint
    ),
    (BOS_STOCKFORGE_SYNC_ENABLED, EditableValueKind::Bool),
    (BOS_STOCKFORGE_SYNC_INTERVAL_SECS, EditableValueKind::Uint),
    (BOS_STRIPE_WRITE_ENABLED, EditableValueKind::Bool),
    (BOS_WEB_ENRICHMENT_ENABLED, EditableValueKind::Bool),
);

pub fn runtime_editable_vars() -> impl Iterator<Item = &'static EnvVar> {
    RUNTIME_EDITABLE_VARS.iter().map(|editable| editable.var)
}

pub fn settings_response(
    conn: &rusqlite::Connection,
    client_id: &str,
) -> Result<AdminSettingsResponse, StoreError> {
    settings_response_with_overlay(conn, client_id, &[])
}

pub fn settings_response_with_overlay(
    conn: &rusqlite::Connection,
    client_id: &str,
    overlay_values: &[OverlayRuntimeValue<'_>],
) -> Result<AdminSettingsResponse, StoreError> {
    let overrides = store::list_overrides(conn, client_id)?
        .into_iter()
        .map(|stored| (stored.var_name.clone(), stored))
        .collect::<BTreeMap<_, _>>();
    let settings = env_registry::ALL
        .iter()
        .map(|var| setting_row(conn, client_id, var, &overrides, overlay_values))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(AdminSettingsResponse { settings })
}

pub fn value(
    conn: &rusqlite::Connection,
    client_id: &str,
    var: &EnvVar,
) -> Result<Option<String>, StoreError> {
    if !is_runtime_editable(var) {
        return Ok(env_registry::string(var));
    }
    Ok(store::get_override(conn, client_id, var.name)?
        .map(|stored| stored.value)
        .or_else(|| env_registry::string(var)))
}

pub fn flag(
    conn: &rusqlite::Connection,
    client_id: &str,
    var: &EnvVar,
) -> Result<bool, StoreError> {
    Ok(value(conn, client_id, var)?
        .map(|raw| {
            matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes"
            )
        })
        .unwrap_or(false))
}

pub fn usize_or(
    conn: &rusqlite::Connection,
    client_id: &str,
    var: &EnvVar,
    default: usize,
) -> Result<usize, StoreError> {
    Ok(value(conn, client_id, var)?
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .unwrap_or(default))
}

pub fn upsert_setting(
    conn: &mut rusqlite::Connection,
    client_id: &str,
    actor_id: &str,
    var_name: &str,
    request: &AdminSettingUpdateRequest,
    now_ms: u64,
) -> Result<MutationOutcome, StoreError> {
    let var = editable_var(var_name)?;
    let value = request.value.trim();
    if value.is_empty() {
        return Err(StoreError::Domain(
            "runtime_setting_value_required".to_string(),
        ));
    }
    validate_value(var, value)?;
    store::upsert_override(
        conn,
        store::OverrideWrite {
            client_id,
            actor_id,
            var_name: var.name,
            value,
            expected_revision: request.expected_revision,
            idempotency_key: &request.idempotency_key,
            now_ms,
        },
    )
}

pub fn clear_setting(
    conn: &mut rusqlite::Connection,
    client_id: &str,
    actor_id: &str,
    var_name: &str,
    request: &AdminSettingClearRequest,
    now_ms: u64,
) -> Result<MutationOutcome, StoreError> {
    let var = editable_var(var_name)?;
    store::delete_override(
        conn,
        store::OverrideDelete {
            client_id,
            actor_id,
            var_name: var.name,
            expected_revision: request.expected_revision,
            idempotency_key: &request.idempotency_key,
            now_ms,
        },
    )
}

fn setting_row(
    conn: &rusqlite::Connection,
    client_id: &str,
    var: &EnvVar,
    overrides: &BTreeMap<String, store::RuntimeSettingOverride>,
    overlay_values: &[OverlayRuntimeValue<'_>],
) -> Result<AdminSettingRow, StoreError> {
    let stored = overrides.get(var.name);
    let overlay_value = overlay_values
        .iter()
        .find(|value| value.var_name == var.name)
        .map(|value| value.value.as_ref());
    let env_value = env_registry::string(var);
    let source = if stored.is_some() {
        AdminSettingSource::StoredOverride
    } else if env_value.is_some() {
        AdminSettingSource::EnvDefault
    } else if overlay_value.is_some() {
        AdminSettingSource::OverlayDefault
    } else {
        AdminSettingSource::Unset
    };
    let effective_value = if is_secret(var) {
        None
    } else if let Some(stored) = stored {
        Some(stored.value.clone())
    } else if let Some(value) = env_value {
        Some(value)
    } else {
        overlay_value.map(|value| value.to_string())
    };
    let default_value = if is_secret(var) {
        None
    } else {
        var.default.map(str::to_string)
    };
    let revision = if let Some(stored) = stored {
        stored.revision
    } else {
        store::current_revision(conn, client_id, var.name)?
    };
    let runtime_editable = runtime_editable(var);
    let editable = runtime_editable.is_some() && !is_secret(var) && !is_infra(var);
    let value_kind = if editable {
        runtime_editable.map(|editable| editable.kind.contract_kind())
    } else {
        None
    };
    let allowed_values = if editable {
        runtime_editable.and_then(|editable| editable.kind.allowed_values())
    } else {
        None
    };
    Ok(AdminSettingRow {
        name: var.name.to_string(),
        description: var.description.to_string(),
        group: var.group.as_str().to_string(),
        secret: is_secret(var),
        editable,
        value_kind,
        allowed_values,
        read_only_reason: read_only_reason(var, editable),
        default_value,
        effective_value,
        source,
        revision,
    })
}

fn editable_var(var_name: &str) -> Result<&'static EnvVar, StoreError> {
    let var = env_registry::ALL
        .iter()
        .copied()
        .find(|var| var.name == var_name)
        .ok_or_else(|| StoreError::Domain("runtime_setting_unknown_var".to_string()))?;
    if !is_runtime_editable(var) || is_secret(var) || is_infra(var) {
        return Err(StoreError::Domain(
            "runtime_setting_not_editable".to_string(),
        ));
    }
    Ok(var)
}

pub fn is_runtime_editable(var: &EnvVar) -> bool {
    runtime_editable(var).is_some()
}

fn runtime_editable(var: &EnvVar) -> Option<&'static RuntimeEditableVar> {
    RUNTIME_EDITABLE_VARS
        .iter()
        .find(|candidate| candidate.var.name == var.name)
}

fn validate_value(var: &EnvVar, value: &str) -> Result<(), StoreError> {
    let Some(editable) = runtime_editable(var) else {
        return Err(StoreError::Domain(
            "runtime_setting_not_editable".to_string(),
        ));
    };
    let valid = match editable.kind {
        EditableValueKind::Bool => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "0" | "false" | "no"
        ),
        EditableValueKind::Uint => value.trim().parse::<u64>().is_ok(),
        EditableValueKind::String => true,
        EditableValueKind::Enum { allowed } => allowed.contains(&value),
    };
    if valid {
        Ok(())
    } else {
        Err(StoreError::Domain(
            "runtime_setting_invalid_value".to_string(),
        ))
    }
}

impl EditableValueKind {
    fn contract_kind(self) -> AdminSettingValueKind {
        match self {
            Self::Bool => AdminSettingValueKind::Bool,
            Self::Uint => AdminSettingValueKind::Uint,
            Self::String => AdminSettingValueKind::String,
            Self::Enum { .. } => AdminSettingValueKind::Enum,
        }
    }

    fn allowed_values(self) -> Option<Vec<String>> {
        match self {
            Self::Enum { allowed } => {
                Some(allowed.iter().map(|value| (*value).to_string()).collect())
            }
            Self::Bool | Self::Uint | Self::String => None,
        }
    }
}

fn is_secret(var: &EnvVar) -> bool {
    var.secret || secret_name_pattern(var.name)
}

fn is_infra(var: &EnvVar) -> bool {
    var.group == EnvVarGroup::InfraServer
}

fn is_security_gate(var: &EnvVar) -> bool {
    matches!(
        var.name,
        "BOS_AGENT_LAUNCH_ENABLED" | "BOS_AGENT_MCP_ENABLED"
    )
}

fn read_only_reason(var: &EnvVar, editable: bool) -> Option<String> {
    if editable {
        None
    } else if is_secret(var) {
        Some("secret".to_string())
    } else if is_security_gate(var) {
        Some("security gate — env-only by policy".to_string())
    } else if is_infra(var) {
        Some("applied at startup — restart to change".to_string())
    } else if is_runtime_editable(var) {
        Some("not editable".to_string())
    } else {
        Some("wireable — pending follow-up".to_string())
    }
}

fn secret_name_pattern(name: &str) -> bool {
    name.split('_')
        .any(|part| matches!(part, "KEY" | "TOKEN" | "SECRET" | "PASSWORD"))
        || name.contains("CLIENT_SECRET")
}
