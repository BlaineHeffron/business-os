//! Scoped API-token persistence through store_core. Receipts NEVER carry the
//! secret or its hash — create/rotate receipts record only that a credential
//! was issued.

use bos_contracts::operator_api_tokens::OperatorApiToken;
use bos_contracts::receipt::ActorKindDto;
use rusqlite::{params, Connection, OptionalExtension, Row};
use sha2::{Digest, Sha256};

use crate::store_core::{self, MutationOutcome, MutationRequest, StoreError};

pub const TOKEN_ENTITY_KIND: &str = "operator_api_token";

pub fn token_hash(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"bos.operator_api_token.v1");
    hasher.update([0]);
    hasher.update(token.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn token_from_row(row: &Row<'_>) -> rusqlite::Result<OperatorApiToken> {
    let capabilities_json: String = row.get(2)?;
    let capabilities = serde_json::from_str(&capabilities_json).unwrap_or_default();
    Ok(OperatorApiToken {
        token_id: row.get(0)?,
        label: row.get(1)?,
        capabilities,
        active: row.get(3)?,
        revoked_at_ms: row
            .get::<_, Option<i64>>(7)?
            .and_then(|value| u64::try_from(value).ok()),
        created_by: row.get(4)?,
        created_at_ms: row.get::<_, i64>(5)? as u64,
        updated_at_ms: row.get::<_, i64>(6)? as u64,
    })
}

const SELECT_COLUMNS: &str = "token_id, label, capabilities_json, active, created_by, \
     created_at_ms, updated_at_ms, revoked_at_ms";

pub fn list_tokens(
    conn: &Connection,
    client_id: &str,
) -> Result<Vec<OperatorApiToken>, StoreError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {SELECT_COLUMNS} FROM operator_api_tokens \
         WHERE client_id = ?1 ORDER BY created_at_ms ASC"
    ))?;
    let rows = stmt.query_map(params![client_id], token_from_row)?;
    let mut tokens = Vec::new();
    for row in rows {
        tokens.push(row?);
    }
    Ok(tokens)
}

pub fn get_token(
    conn: &Connection,
    client_id: &str,
    token_id: &str,
) -> Result<Option<OperatorApiToken>, StoreError> {
    let row = conn
        .query_row(
            &format!(
                "SELECT {SELECT_COLUMNS} FROM operator_api_tokens \
                 WHERE client_id = ?1 AND token_id = ?2"
            ),
            params![client_id, token_id],
            token_from_row,
        )
        .optional()?;
    Ok(row)
}

/// The ACTIVE, non-revoked token a presented hashed bearer belongs to.
pub fn find_active_by_token_hash(
    conn: &Connection,
    client_id: &str,
    token_hash: &str,
) -> Result<Option<OperatorApiToken>, StoreError> {
    let row = conn
        .query_row(
            &format!(
                "SELECT {SELECT_COLUMNS} FROM operator_api_tokens \
                 WHERE client_id = ?1 AND token_hash = ?2 \
                   AND active = 1 AND revoked_at_ms IS NULL"
            ),
            params![client_id, token_hash],
            token_from_row,
        )
        .optional()?;
    Ok(row)
}

pub fn any_active_token(conn: &Connection, client_id: &str) -> Result<bool, StoreError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM operator_api_tokens \
         WHERE client_id = ?1 AND active = 1 AND revoked_at_ms IS NULL",
        params![client_id],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

pub struct TokenActionContext<'a> {
    pub client_id: &'a str,
    pub actor_id: &'a str,
    pub expected_revision: Option<u64>,
    pub idempotency_key: &'a str,
    pub now_ms: u64,
}

fn metadata_json(token: &OperatorApiToken) -> String {
    serde_json::json!({
        "token_id": token.token_id,
        "label": token.label,
        "capabilities": token.capabilities,
        "active": token.active,
        "revoked_at_ms": token.revoked_at_ms,
    })
    .to_string()
}

pub fn create_token(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    token: &OperatorApiToken,
    token_hash: &str,
    idempotency_key: &str,
) -> Result<MutationOutcome, StoreError> {
    let after = metadata_json(token);
    let owned_client = client_id.to_string();
    let owned_token = token.clone();
    let owned_hash = token_hash.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: TOKEN_ENTITY_KIND,
            entity_id: &token.token_id,
            change_kind: "create",
            actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: None,
            idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms: token.created_at_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO operator_api_tokens \
                 (client_id, token_id, label, token_hash, capabilities_json, active, \
                  created_by, created_at_ms, updated_at_ms, revoked_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?7, ?7, NULL)",
                params![
                    owned_client,
                    owned_token.token_id,
                    owned_token.label,
                    owned_hash,
                    serde_json::to_string(&owned_token.capabilities)
                        .unwrap_or_else(|_| "[]".into()),
                    owned_token.created_by,
                    owned_token.created_at_ms as i64,
                ],
            )
            .map_err(|err| match err {
                rusqlite::Error::SqliteFailure(code, _)
                    if code.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    StoreError::Domain("operator_api_token_exists".to_string())
                }
                other => other.into(),
            })?;
            Ok(())
        },
    )
}

fn require_live(token: &OperatorApiToken) -> Result<(), StoreError> {
    if token.revoked_at_ms.is_some() {
        return Err(StoreError::Domain("operator_api_token_revoked".to_string()));
    }
    Ok(())
}

pub fn set_active(
    conn: &mut Connection,
    ctx: TokenActionContext<'_>,
    token_id: &str,
    active: bool,
) -> Result<MutationOutcome, StoreError> {
    let current = get_token(conn, ctx.client_id, token_id)?
        .ok_or_else(|| StoreError::Domain("operator_api_token_not_found".to_string()))?;
    require_live(&current)?;
    let owned_client = ctx.client_id.to_string();
    let owned_id = token_id.to_string();
    let now_ms = ctx.now_ms;
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: TOKEN_ENTITY_KIND,
            entity_id: token_id,
            change_kind: if active { "enable" } else { "disable" },
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: Some(format!("{{\"active\":{}}}", current.active)),
            after_json: Some(format!("{{\"active\":{active}}}")),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE operator_api_tokens SET active = ?3, updated_at_ms = ?4 \
                 WHERE client_id = ?1 AND token_id = ?2",
                params![owned_client, owned_id, active, now_ms as i64],
            )?;
            Ok(())
        },
    )
}

pub fn revoke_token(
    conn: &mut Connection,
    ctx: TokenActionContext<'_>,
    token_id: &str,
) -> Result<MutationOutcome, StoreError> {
    let current = get_token(conn, ctx.client_id, token_id)?
        .ok_or_else(|| StoreError::Domain("operator_api_token_not_found".to_string()))?;
    require_live(&current)?;
    let owned_client = ctx.client_id.to_string();
    let owned_id = token_id.to_string();
    let now_ms = ctx.now_ms;
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: TOKEN_ENTITY_KIND,
            entity_id: token_id,
            change_kind: "revoke",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: Some(metadata_json(&current)),
            after_json: Some(format!("{{\"active\":false,\"revoked_at_ms\":{now_ms}}}")),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE operator_api_tokens \
                 SET active = 0, revoked_at_ms = ?3, updated_at_ms = ?3 \
                 WHERE client_id = ?1 AND token_id = ?2",
                params![owned_client, owned_id, now_ms as i64],
            )?;
            Ok(())
        },
    )
}

/// Replace the bearer hash (the old secret stops authenticating). The receipt
/// records THAT the credential rotated, never the credential.
pub fn rotate_token(
    conn: &mut Connection,
    ctx: TokenActionContext<'_>,
    token_id: &str,
    new_token_hash: &str,
) -> Result<MutationOutcome, StoreError> {
    let current = get_token(conn, ctx.client_id, token_id)?
        .ok_or_else(|| StoreError::Domain("operator_api_token_not_found".to_string()))?;
    require_live(&current)?;
    let owned_client = ctx.client_id.to_string();
    let owned_id = token_id.to_string();
    let owned_hash = new_token_hash.to_string();
    let now_ms = ctx.now_ms;
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: TOKEN_ENTITY_KIND,
            entity_id: token_id,
            change_kind: "rotate_token",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: Some("{\"token_rotated\":false}".to_string()),
            after_json: Some("{\"token_rotated\":true}".to_string()),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE operator_api_tokens SET token_hash = ?3, updated_at_ms = ?4 \
                 WHERE client_id = ?1 AND token_id = ?2",
                params![owned_client, owned_id, owned_hash, now_ms as i64],
            )
            .map_err(|err| match err {
                rusqlite::Error::SqliteFailure(code, _)
                    if code.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    StoreError::Domain("operator_api_token_exists".to_string())
                }
                other => other.into(),
            })?;
            Ok(())
        },
    )
}
