use bos_contracts::receipt::ActorKindDto;
use rusqlite::{params, Connection, OptionalExtension};

use crate::store_core::{self, MutationOutcome, MutationRequest, StoreError};

pub const RUNTIME_SETTING_ENTITY_KIND: &str = "runtime_setting_override";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSettingOverride {
    pub var_name: String,
    pub value: String,
    pub revision: Option<u64>,
}

pub struct OverrideWrite<'a> {
    pub client_id: &'a str,
    pub actor_id: &'a str,
    pub var_name: &'a str,
    pub value: &'a str,
    pub expected_revision: Option<u64>,
    pub idempotency_key: &'a str,
    pub now_ms: u64,
}

pub struct OverrideDelete<'a> {
    pub client_id: &'a str,
    pub actor_id: &'a str,
    pub var_name: &'a str,
    pub expected_revision: Option<u64>,
    pub idempotency_key: &'a str,
    pub now_ms: u64,
}

pub fn list_overrides(
    conn: &Connection,
    client_id: &str,
) -> Result<Vec<RuntimeSettingOverride>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT var_name, value FROM runtime_setting_overrides \
         WHERE client_id = ?1 ORDER BY var_name",
    )?;
    let rows = stmt.query_map(params![client_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (var_name, value) = row?;
        let revision = current_revision(conn, client_id, &var_name)?;
        out.push(RuntimeSettingOverride {
            var_name,
            value,
            revision,
        });
    }
    Ok(out)
}

pub fn get_override(
    conn: &Connection,
    client_id: &str,
    var_name: &str,
) -> Result<Option<RuntimeSettingOverride>, StoreError> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM runtime_setting_overrides \
             WHERE client_id = ?1 AND var_name = ?2",
            params![client_id, var_name],
            |row| row.get(0),
        )
        .optional()?;
    let Some(value) = value else {
        return Ok(None);
    };
    let revision = current_revision(conn, client_id, var_name)?;
    Ok(Some(RuntimeSettingOverride {
        var_name: var_name.to_string(),
        value,
        revision,
    }))
}

pub fn current_revision(
    conn: &Connection,
    client_id: &str,
    var_name: &str,
) -> Result<Option<u64>, StoreError> {
    store_core::current_revision(conn, client_id, RUNTIME_SETTING_ENTITY_KIND, var_name)
}

pub fn upsert_override(
    conn: &mut Connection,
    request: OverrideWrite<'_>,
) -> Result<MutationOutcome, StoreError> {
    let before_json = get_override(conn, request.client_id, request.var_name)?
        .and_then(|stored| serde_json::to_string(&stored.value).ok());
    let after_json = serde_json::to_string(request.value).ok();
    let owned_client = request.client_id.to_string();
    let owned_var = request.var_name.to_string();
    let owned_value = request.value.to_string();
    let now_ms = request.now_ms;
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: request.client_id,
            entity_kind: RUNTIME_SETTING_ENTITY_KIND,
            entity_id: request.var_name,
            change_kind: "upsert",
            actor_id: request.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: request.expected_revision,
            idempotency_key: request.idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json,
            after_json,
            now_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO runtime_setting_overrides \
                 (client_id, var_name, value, updated_at_ms) VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(client_id, var_name) DO UPDATE SET \
                   value = excluded.value, updated_at_ms = excluded.updated_at_ms",
                params![&owned_client, &owned_var, &owned_value, now_ms as i64],
            )?;
            Ok(())
        },
    )
}

pub fn delete_override(
    conn: &mut Connection,
    request: OverrideDelete<'_>,
) -> Result<MutationOutcome, StoreError> {
    let before_json = get_override(conn, request.client_id, request.var_name)?
        .and_then(|stored| serde_json::to_string(&stored.value).ok());
    let owned_client = request.client_id.to_string();
    let owned_var = request.var_name.to_string();
    let now_ms = request.now_ms;
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: request.client_id,
            entity_kind: RUNTIME_SETTING_ENTITY_KIND,
            entity_id: request.var_name,
            change_kind: "delete",
            actor_id: request.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: request.expected_revision,
            idempotency_key: request.idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json,
            after_json: None,
            now_ms,
        },
        move |tx| {
            tx.execute(
                "DELETE FROM runtime_setting_overrides WHERE client_id = ?1 AND var_name = ?2",
                params![&owned_client, &owned_var],
            )?;
            Ok(())
        },
    )
}
