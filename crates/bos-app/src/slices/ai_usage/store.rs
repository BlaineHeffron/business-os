//! Usage-row persistence through store_core. Rows are immutable accounting
//! records; the only mutation is the insert.

use bos_contracts::ai_usage::{AiUsageRow, AiUsageTotals};
use bos_contracts::llm_settings::{
    LlmGlobalRouteSettingsUpdate, LlmPurposeRouteOverrideUpdate, LlmRouteSettingsUpdateRequest,
};
use bos_contracts::receipt::ActorKindDto;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::BTreeMap;

use crate::store_core::{self, MutationOutcome, MutationRequest, StoreError};

pub const USAGE_ENTITY_KIND: &str = "ai_usage";
pub const LLM_SETTINGS_ENTITY_KIND: &str = "llm_settings";
pub const LLM_SETTINGS_ENTITY_ID: &str = "llm_route_settings";
pub const CLAUDE_SUBSCRIPTION_AUTH_ENTITY_KIND: &str = "claude_subscription_auth";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageSignal {
    pub usage_id: String,
    pub correlation_id: String,
    pub purpose: String,
    pub success: bool,
    pub error_code: Option<String>,
    pub recorded_at_ms: u64,
}

/// Full insert shape (the wire row plus audit-only columns).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageInsert {
    pub row: AiUsageRow,
    pub task_kind: Option<String>,
    pub thinking_level: Option<String>,
    pub cached_tokens: Option<u64>,
    pub provider_request_id: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredLlmRouteSettings {
    pub global: LlmGlobalRouteSettingsUpdate,
    pub overrides: Vec<LlmPurposeRouteOverrideUpdate>,
    pub revision: Option<u64>,
}

pub fn get_llm_route_settings(
    conn: &Connection,
    client_id: &str,
) -> Result<Option<StoredLlmRouteSettings>, StoreError> {
    let Some(global) = conn
        .query_row(
            "SELECT default_backend, default_model, max_tokens, timeout_ms \
             FROM llm_route_settings WHERE client_id = ?1",
            params![client_id],
            |row| {
                Ok(LlmGlobalRouteSettingsUpdate {
                    backend: row.get(0)?,
                    model: row.get(1)?,
                    max_tokens: row.get::<_, i64>(2)? as u32,
                    timeout_ms: row.get::<_, i64>(3)? as u64,
                })
            },
        )
        .optional()?
    else {
        return Ok(None);
    };
    let overrides = list_llm_route_overrides(conn, client_id)?;
    let revision = current_revision(
        conn,
        client_id,
        LLM_SETTINGS_ENTITY_KIND,
        LLM_SETTINGS_ENTITY_ID,
    )?;
    Ok(Some(StoredLlmRouteSettings {
        global,
        overrides,
        revision,
    }))
}

pub fn list_llm_route_overrides(
    conn: &Connection,
    client_id: &str,
) -> Result<Vec<LlmPurposeRouteOverrideUpdate>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT purpose, backend, model FROM llm_route_overrides \
         WHERE client_id = ?1 ORDER BY purpose ASC",
    )?;
    let rows = stmt.query_map(params![client_id], |row| {
        Ok(LlmPurposeRouteOverrideUpdate {
            purpose: row.get(0)?,
            backend: row.get(1)?,
            model: row.get(2)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn current_revision(
    conn: &Connection,
    client_id: &str,
    entity_kind: &str,
    entity_id: &str,
) -> Result<Option<u64>, StoreError> {
    store_core::current_revision(conn, client_id, entity_kind, entity_id)
}

pub fn replace_llm_route_settings(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    request: &LlmRouteSettingsUpdateRequest,
    is_allowed_purpose: impl Fn(&str) -> bool,
    now_ms: u64,
) -> Result<MutationOutcome, StoreError> {
    let normalized = normalize_settings_request(request, is_allowed_purpose)?;
    let before = get_llm_route_settings(conn, client_id)?
        .and_then(|settings| serde_json::to_string(&settings_payload(&settings)).ok());
    let after = serde_json::to_string(&settings_payload(&normalized))
        .map_err(|err| StoreError::Domain(format!("serialize llm settings: {err}")))?;
    let owned = normalized.clone();
    let owned_client = client_id.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: LLM_SETTINGS_ENTITY_KIND,
            entity_id: LLM_SETTINGS_ENTITY_ID,
            change_kind: "replace",
            actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: request.expected_revision,
            idempotency_key: &request.idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: before,
            after_json: Some(after),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO llm_route_settings \
                 (client_id, default_backend, default_model, max_tokens, timeout_ms, updated_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                 ON CONFLICT(client_id) DO UPDATE SET \
                   default_backend = excluded.default_backend, \
                   default_model = excluded.default_model, \
                   max_tokens = excluded.max_tokens, \
                   timeout_ms = excluded.timeout_ms, \
                   updated_at_ms = excluded.updated_at_ms",
                params![
                    &owned_client,
                    &owned.global.backend,
                    &owned.global.model,
                    owned.global.max_tokens as i64,
                    owned.global.timeout_ms as i64,
                    now_ms as i64,
                ],
            )?;
            tx.execute(
                "DELETE FROM llm_route_overrides WHERE client_id = ?1",
                params![&owned_client],
            )?;
            for route in owned.overrides {
                tx.execute(
                    "INSERT INTO llm_route_overrides \
                     (client_id, purpose, backend, model, updated_at_ms) \
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        &owned_client,
                        &route.purpose,
                        &route.backend,
                        &route.model,
                        now_ms as i64,
                    ],
                )?;
            }
            Ok(())
        },
    )
}

pub fn record_claude_subscription_action(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    flow_id: &str,
    change_kind: &str,
    idempotency_key: &str,
    now_ms: u64,
) -> Result<MutationOutcome, StoreError> {
    let state = match change_kind {
        "authorize_requested" => "authorization_pending",
        "authorization_code_submitted" => "authorization_submitted",
        _ => return Err(StoreError::Domain("llm_auth_action_invalid".to_string())),
    };
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: CLAUDE_SUBSCRIPTION_AUTH_ENTITY_KIND,
            entity_id: flow_id,
            change_kind,
            actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: None,
            idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(serde_json::json!({ "state": state }).to_string()),
            now_ms,
        },
        |_| Ok(()),
    )
}

pub fn claude_subscription_action_was_applied(
    conn: &Connection,
    client_id: &str,
    idempotency_key: &str,
) -> Result<bool, StoreError> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM receipts \
         WHERE client_id = ?1 AND idempotency_key = ?2 AND outcome = 'applied')",
        params![client_id, idempotency_key],
        |row| row.get(0),
    )?)
}

pub fn record_claude_subscription_failure(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    flow_id: &str,
    idempotency_key: &str,
    error_class: &str,
    now_ms: u64,
) -> Result<String, StoreError> {
    store_core::record_failed_receipt(
        conn,
        MutationRequest {
            client_id,
            entity_kind: CLAUDE_SUBSCRIPTION_AUTH_ENTITY_KIND,
            entity_id: flow_id,
            change_kind: "authorization_code_submit_failed",
            actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: None,
            idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(
                serde_json::json!({ "state": "authorization_submit_failed" }).to_string(),
            ),
            now_ms,
        },
        error_class,
    )
}

fn normalize_settings_request(
    request: &LlmRouteSettingsUpdateRequest,
    is_allowed_purpose: impl Fn(&str) -> bool,
) -> Result<StoredLlmRouteSettings, StoreError> {
    if request.idempotency_key.trim().is_empty() {
        return Err(StoreError::Domain("idempotency_key_required".to_string()));
    }
    let global = LlmGlobalRouteSettingsUpdate {
        backend: normalize_backend(&request.global.backend)?,
        model: normalize_optional_model(request.global.model.as_deref()),
        max_tokens: validate_max_tokens(request.global.max_tokens)?,
        timeout_ms: validate_timeout_ms(request.global.timeout_ms)?,
    };
    let mut by_purpose = BTreeMap::new();
    for override_config in &request.overrides {
        let purpose = override_config.purpose.trim();
        if purpose.is_empty() {
            return Err(StoreError::Domain("llm_purpose_required".to_string()));
        }
        if !is_allowed_purpose(purpose) {
            return Err(StoreError::Domain("llm_purpose_unknown".to_string()));
        }
        by_purpose.insert(
            purpose.to_string(),
            LlmPurposeRouteOverrideUpdate {
                purpose: purpose.to_string(),
                backend: normalize_backend(&override_config.backend)?,
                model: normalize_optional_model(override_config.model.as_deref()),
            },
        );
    }
    Ok(StoredLlmRouteSettings {
        global,
        overrides: by_purpose.into_values().collect(),
        revision: request.expected_revision,
    })
}

fn normalize_backend(raw: &str) -> Result<String, StoreError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "api" | "harness" => Ok(raw.trim().to_ascii_lowercase()),
        _ => Err(StoreError::Domain("llm_backend_invalid".to_string())),
    }
}

fn normalize_optional_model(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|model| !model.is_empty())
        .map(ToOwned::to_owned)
}

fn validate_max_tokens(value: u32) -> Result<u32, StoreError> {
    if (256..=65_536).contains(&value) {
        Ok(value)
    } else {
        Err(StoreError::Domain("llm_max_tokens_invalid".to_string()))
    }
}

fn validate_timeout_ms(value: u64) -> Result<u64, StoreError> {
    if (5_000..=600_000).contains(&value) {
        Ok(value)
    } else {
        Err(StoreError::Domain("llm_timeout_ms_invalid".to_string()))
    }
}

fn settings_payload(
    settings: &StoredLlmRouteSettings,
) -> (
    LlmGlobalRouteSettingsUpdate,
    Vec<LlmPurposeRouteOverrideUpdate>,
) {
    (settings.global.clone(), settings.overrides.clone())
}

pub fn insert_usage(
    conn: &mut Connection,
    client_id: &str,
    insert: &UsageInsert,
) -> Result<MutationOutcome, StoreError> {
    let after = serde_json::to_string(&insert.row)
        .map_err(|err| StoreError::Domain(format!("serialize usage row: {err}")))?;
    let owned = insert.clone();
    let owned_client = client_id.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: USAGE_ENTITY_KIND,
            entity_id: &insert.row.usage_id,
            change_kind: "record",
            actor_id: "ai_usage_recorder",
            actor_kind: ActorKindDto::System,
            expected_revision: None,
            idempotency_key: &insert.row.usage_id,
            correlation_id: Some(&insert.row.correlation_id),
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms: insert.row.recorded_at_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO ai_usage_log \
                 (client_id, usage_id, purpose, task_kind, route, provider, model, \
                  thinking_level, tokens_in, tokens_out, total_tokens, cached_tokens, \
                  cost_micros, latency_ms, success, error_code, correlation_id, \
                  provider_request_id, recorded_at_ms, error_message) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, \
                  ?16, ?17, ?18, ?19, ?20) \
                 ON CONFLICT (client_id, usage_id) DO NOTHING",
                params![
                    owned_client,
                    owned.row.usage_id,
                    owned.row.purpose,
                    owned.task_kind,
                    owned.row.route,
                    owned.row.provider,
                    owned.row.model,
                    owned.thinking_level,
                    owned.row.tokens_in.map(|v| v as i64),
                    owned.row.tokens_out.map(|v| v as i64),
                    owned.row.total_tokens.map(|v| v as i64),
                    owned.cached_tokens.map(|v| v as i64),
                    owned.row.cost_micros.map(|v| v as i64),
                    owned.row.latency_ms as i64,
                    owned.row.success,
                    owned.row.error_code,
                    owned.row.correlation_id,
                    owned.provider_request_id,
                    owned.row.recorded_at_ms as i64,
                    owned.error_message,
                ],
            )?;
            Ok(())
        },
    )
}

pub fn list_recent(
    conn: &Connection,
    client_id: &str,
    limit: usize,
) -> Result<Vec<AiUsageRow>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT usage_id, purpose, route, provider, model, tokens_in, tokens_out, \
         total_tokens, cost_micros, latency_ms, success, error_code, correlation_id, \
         recorded_at_ms \
         FROM ai_usage_log WHERE client_id = ?1 \
         ORDER BY recorded_at_ms DESC, usage_id DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![client_id, limit as i64], |row| {
        Ok(AiUsageRow {
            usage_id: row.get(0)?,
            purpose: row.get(1)?,
            route: row.get(2)?,
            provider: row.get(3)?,
            model: row.get(4)?,
            tokens_in: row.get::<_, Option<i64>>(5)?.map(|v| v as u64),
            tokens_out: row.get::<_, Option<i64>>(6)?.map(|v| v as u64),
            total_tokens: row.get::<_, Option<i64>>(7)?.map(|v| v as u64),
            cost_micros: row.get::<_, Option<i64>>(8)?.map(|v| v as u64),
            latency_ms: row.get::<_, i64>(9)? as u64,
            success: row.get(10)?,
            error_code: row.get(11)?,
            correlation_id: row.get(12)?,
            recorded_at_ms: row.get::<_, i64>(13)? as u64,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Aggregate rows recorded at or after `since_ms` (0 = all time).
pub fn totals_since(
    conn: &Connection,
    client_id: &str,
    since_ms: u64,
) -> Result<AiUsageTotals, StoreError> {
    conn.query_row(
        "SELECT COUNT(*), \
         COALESCE(SUM(CASE WHEN success = 0 THEN 1 ELSE 0 END), 0), \
         COALESCE(SUM(tokens_in), 0), COALESCE(SUM(tokens_out), 0), \
         COALESCE(SUM(cost_micros), 0) \
         FROM ai_usage_log WHERE client_id = ?1 AND recorded_at_ms >= ?2",
        params![client_id, since_ms as i64],
        |row| {
            Ok(AiUsageTotals {
                calls: row.get::<_, i64>(0)? as u64,
                failures: row.get::<_, i64>(1)? as u64,
                tokens_in: row.get::<_, i64>(2)? as u64,
                tokens_out: row.get::<_, i64>(3)? as u64,
                cost_micros: row.get::<_, i64>(4)? as u64,
            })
        },
    )
    .map_err(Into::into)
}

pub fn cost_micros_for_purpose_correlation(
    conn: &Connection,
    client_id: &str,
    purpose: &str,
    correlation_id: &str,
) -> Result<u64, StoreError> {
    conn.query_row(
        "SELECT COALESCE(SUM(cost_micros), 0) \
         FROM ai_usage_log \
         WHERE client_id = ?1 AND purpose = ?2 AND correlation_id = ?3",
        params![client_id, purpose, correlation_id],
        |row| Ok(row.get::<_, i64>(0)? as u64),
    )
    .map_err(Into::into)
}

/// Recent LLM calls for the supplied correlation ids. Produce requests
/// stamp the work item id as correlation_id, so the queue can use this as a
/// read-only failure signal without a second produce-status table.
pub fn usage_for_correlations(
    conn: &Connection,
    client_id: &str,
    correlation_ids: &[String],
    since_ms: u64,
) -> Result<Vec<UsageSignal>, StoreError> {
    if correlation_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; correlation_ids.len()].join(", ");
    let sql = format!(
        "SELECT usage_id, correlation_id, purpose, success, error_code, recorded_at_ms \
         FROM ai_usage_log \
         WHERE client_id = ? AND recorded_at_ms >= ? \
           AND correlation_id IN ({placeholders}) \
         ORDER BY recorded_at_ms DESC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let since_ms = since_ms as i64;
    let mut params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(correlation_ids.len() + 2);
    params.push(&client_id);
    params.push(&since_ms);
    for id in correlation_ids {
        params.push(id);
    }
    let rows = stmt.query_map(params.as_slice(), |row| {
        Ok(UsageSignal {
            usage_id: row.get(0)?,
            correlation_id: row.get(1)?,
            purpose: row.get(2)?,
            success: row.get(3)?,
            error_code: row.get(4)?,
            recorded_at_ms: row.get::<_, i64>(5)? as u64,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}
