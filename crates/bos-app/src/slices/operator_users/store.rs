//! Operator user persistence through store_core. Receipts NEVER carry the
//! token — create/rotate receipts record only that a credential was issued.

use bos_contracts::operator_users::OperatorUser;
use bos_contracts::receipt::ActorKindDto;
use rusqlite::{params, Connection, OptionalExtension, Row};
use sha2::{Digest, Sha256};

use crate::store_core::{self, MutationOutcome, MutationRequest, StoreError};

pub const USER_ENTITY_KIND: &str = "operator_user";

fn user_from_row(row: &Row<'_>) -> rusqlite::Result<OperatorUser> {
    Ok(OperatorUser {
        user_id: row.get(0)?,
        display_name: row.get(1)?,
        active: row.get(2)?,
        archived_at_ms: row
            .get::<_, Option<i64>>(5)?
            .and_then(|value| u64::try_from(value).ok()),
        default_calendar_id: row.get(6)?,
        created_at_ms: row.get::<_, i64>(3)? as u64,
        updated_at_ms: row.get::<_, i64>(4)? as u64,
    })
}

pub fn list_users(
    conn: &Connection,
    client_id: &str,
    include_archived: bool,
) -> Result<Vec<OperatorUser>, StoreError> {
    let archived_filter = if include_archived {
        ""
    } else {
        " AND archived_at_ms IS NULL"
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT user_id, display_name, active, created_at_ms, updated_at_ms, \
             archived_at_ms, default_calendar_id \
             FROM operator_users WHERE client_id = ?1{archived_filter} \
             ORDER BY created_at_ms ASC"
    ))?;
    let rows = stmt.query_map(params![client_id], user_from_row)?;
    let mut users = Vec::new();
    for row in rows {
        users.push(row?);
    }
    Ok(users)
}

pub fn get_user(
    conn: &Connection,
    client_id: &str,
    user_id: &str,
) -> Result<Option<OperatorUser>, StoreError> {
    let row = conn
        .query_row(
            "SELECT user_id, display_name, active, created_at_ms, updated_at_ms, \
             archived_at_ms, default_calendar_id \
             FROM operator_users WHERE client_id = ?1 AND user_id = ?2",
            params![client_id, user_id],
            user_from_row,
        )
        .optional()?;
    Ok(row)
}

/// The ACTIVE user a presented personal token belongs to, if any. Disabled
/// users' tokens stop authenticating immediately.
pub fn find_active_by_token(
    conn: &Connection,
    client_id: &str,
    token: &str,
) -> Result<Option<OperatorUser>, StoreError> {
    let row = conn
        .query_row(
            "SELECT user_id, display_name, active, created_at_ms, updated_at_ms, \
             archived_at_ms, default_calendar_id \
             FROM operator_users \
             WHERE client_id = ?1 AND token = ?2 AND active = 1 AND archived_at_ms IS NULL",
            params![client_id, token],
            user_from_row,
        )
        .optional()?;
    Ok(row)
}

pub fn any_active_token(conn: &Connection, client_id: &str) -> Result<bool, StoreError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM operator_users \
         WHERE client_id = ?1 AND active = 1 AND archived_at_ms IS NULL",
        params![client_id],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Return the ACTIVE personal token matching a browser-session proof. Browser
/// cookies carry only a token fingerprint + proof; the bearer token stays in
/// the server-side operator_users table and is revalidated on every request.
pub fn find_active_token_by_session_proof(
    conn: &Connection,
    client_id: &str,
    token_fingerprint: &str,
    proof: &str,
    proof_material: &str,
) -> Result<Option<String>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT token FROM operator_users \
         WHERE client_id = ?1 AND active = 1 AND archived_at_ms IS NULL",
    )?;
    let rows = stmt.query_map(params![client_id], |row| row.get::<_, String>(0))?;
    for row in rows {
        let token = row?;
        if session_token_fingerprint(&token) == token_fingerprint
            && session_token_proof(&token, token_fingerprint, proof_material) == proof
        {
            return Ok(Some(token));
        }
    }
    Ok(None)
}

pub fn session_token_fingerprint(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"bos.operator_session.token.v1");
    hasher.update([0]);
    hasher.update(token.as_bytes());
    format!("{:x}", hasher.finalize())
}

pub fn session_token_proof(token: &str, token_fingerprint: &str, proof_material: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"bos.operator_session.proof.v1");
    hasher.update([0]);
    hasher.update(proof_material.as_bytes());
    hasher.update([0]);
    hasher.update(token_fingerprint.as_bytes());
    hasher.update([0]);
    hasher.update(token.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Create a user with a freshly generated personal token. The token is NOT
/// written to the receipt; the unique token index turns collisions (or a
/// duplicate user_id) into domain errors.
pub fn create_user(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    user: &OperatorUser,
    token: &str,
    idempotency_key: &str,
) -> Result<MutationOutcome, StoreError> {
    let after = serde_json::json!({
        "user_id": user.user_id,
        "display_name": user.display_name,
        "active": user.active,
        "archived_at_ms": user.archived_at_ms,
    });
    let owned_client = client_id.to_string();
    let owned_user = user.clone();
    let owned_token = token.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: USER_ENTITY_KIND,
            entity_id: &user.user_id,
            change_kind: "create",
            actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: None,
            idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(after.to_string()),
            now_ms: user.created_at_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO operator_users \
                 (client_id, user_id, display_name, token, active, created_at_ms, updated_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, 1, ?5, ?5)",
                params![
                    owned_client,
                    owned_user.user_id,
                    owned_user.display_name,
                    owned_token,
                    owned_user.created_at_ms as i64,
                ],
            )
            .map_err(|err| match err {
                rusqlite::Error::SqliteFailure(code, _)
                    if code.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    StoreError::Domain("operator_user_exists".to_string())
                }
                other => other.into(),
            })?;
            Ok(())
        },
    )
}

fn has_google_credentials(
    conn: &Connection,
    client_id: &str,
    user_id: &str,
) -> Result<bool, StoreError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM google_oauth_credentials WHERE client_id = ?1 AND user_id = ?2",
        params![client_id, user_id],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

fn has_qbo_credential_connected_by(
    conn: &Connection,
    client_id: &str,
    user_id: &str,
) -> Result<bool, StoreError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM qbo_credentials \
         WHERE client_id = ?1 AND connected_by_user_id = ?2",
        params![client_id, user_id],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

pub struct UserActionContext<'a> {
    pub client_id: &'a str,
    pub actor_id: &'a str,
    pub expected_revision: Option<u64>,
    pub idempotency_key: &'a str,
    pub now_ms: u64,
}

/// Soft-delete a disabled user from normal operator surfaces. The row remains
/// for historical receipts/source_user_id references, and the token can no
/// longer authenticate because active is forced false and token lookup excludes
/// archived rows.
pub fn archive_user(
    conn: &mut Connection,
    ctx: UserActionContext<'_>,
    user_id: &str,
) -> Result<MutationOutcome, StoreError> {
    let current = get_user(conn, ctx.client_id, user_id)?
        .ok_or_else(|| StoreError::Domain("operator_user_not_found".to_string()))?;
    if current.active {
        return Err(StoreError::Domain(
            "operator_user_archive_requires_disabled".to_string(),
        ));
    }
    if current.archived_at_ms.is_some() {
        return Err(StoreError::Domain(
            "operator_user_already_archived".to_string(),
        ));
    }
    if has_google_credentials(conn, ctx.client_id, user_id)? {
        return Err(StoreError::Domain(
            "operator_user_has_google_credentials".to_string(),
        ));
    }
    if has_qbo_credential_connected_by(conn, ctx.client_id, user_id)? {
        return Err(StoreError::Domain(
            "operator_user_has_qbo_credential".to_string(),
        ));
    }
    let before = serde_json::json!({
        "active": current.active,
        "archived_at_ms": current.archived_at_ms,
    });
    let after = serde_json::json!({
        "active": false,
        "archived_at_ms": ctx.now_ms,
    });
    let owned_client = ctx.client_id.to_string();
    let owned_user = user_id.to_string();
    let now_ms = ctx.now_ms;
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: USER_ENTITY_KIND,
            entity_id: user_id,
            change_kind: "archive",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: Some(before.to_string()),
            after_json: Some(after.to_string()),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE operator_users \
                 SET active = 0, archived_at_ms = ?3, updated_at_ms = ?3 \
                 WHERE client_id = ?1 AND user_id = ?2",
                params![owned_client, owned_user, now_ms as i64],
            )?;
            Ok(())
        },
    )
}

/// Enable or disable a user. Disabling invalidates the token immediately
/// (the token lookup filters on active).
pub fn set_active(
    conn: &mut Connection,
    ctx: UserActionContext<'_>,
    user_id: &str,
    active: bool,
) -> Result<MutationOutcome, StoreError> {
    let current = get_user(conn, ctx.client_id, user_id)?
        .ok_or_else(|| StoreError::Domain("operator_user_not_found".to_string()))?;
    if current.archived_at_ms.is_some() {
        return Err(StoreError::Domain("operator_user_archived".to_string()));
    }
    let owned_client = ctx.client_id.to_string();
    let owned_user = user_id.to_string();
    let now_ms = ctx.now_ms;
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: USER_ENTITY_KIND,
            entity_id: user_id,
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
                "UPDATE operator_users SET active = ?3, updated_at_ms = ?4 \
                 WHERE client_id = ?1 AND user_id = ?2",
                params![owned_client, owned_user, active, now_ms as i64],
            )?;
            Ok(())
        },
    )
}

/// Replace the user's token (the old one stops authenticating). The receipt
/// records THAT the credential rotated, never the credential.
pub fn rotate_token(
    conn: &mut Connection,
    ctx: UserActionContext<'_>,
    user_id: &str,
    new_token: &str,
) -> Result<MutationOutcome, StoreError> {
    let current = get_user(conn, ctx.client_id, user_id)?
        .ok_or_else(|| StoreError::Domain("operator_user_not_found".to_string()))?;
    if current.archived_at_ms.is_some() {
        return Err(StoreError::Domain("operator_user_archived".to_string()));
    }
    let owned_client = ctx.client_id.to_string();
    let owned_user = user_id.to_string();
    let owned_token = new_token.to_string();
    let now_ms = ctx.now_ms;
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: USER_ENTITY_KIND,
            entity_id: user_id,
            change_kind: "rotate_token",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some("{\"token_rotated\":true}".to_string()),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE operator_users SET token = ?3, updated_at_ms = ?4 \
                 WHERE client_id = ?1 AND user_id = ?2",
                params![owned_client, owned_user, owned_token, now_ms as i64],
            )?;
            Ok(())
        },
    )
}

/// Set (or clear, with None) the calendar the user's approved event drafts
/// default to.
pub fn set_default_calendar(
    conn: &mut Connection,
    ctx: UserActionContext<'_>,
    user_id: &str,
    calendar_id: Option<&str>,
) -> Result<MutationOutcome, StoreError> {
    let current = get_user(conn, ctx.client_id, user_id)?
        .ok_or_else(|| StoreError::Domain("operator_user_not_found".to_string()))?;
    if current.archived_at_ms.is_some() {
        return Err(StoreError::Domain("operator_user_archived".to_string()));
    }
    let before = serde_json::json!({ "default_calendar_id": current.default_calendar_id });
    let after = serde_json::json!({ "default_calendar_id": calendar_id });
    let owned_client = ctx.client_id.to_string();
    let owned_user = user_id.to_string();
    let owned_calendar = calendar_id.map(str::to_string);
    let now_ms = ctx.now_ms;
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: USER_ENTITY_KIND,
            entity_id: user_id,
            change_kind: "set_default_calendar",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: Some(before.to_string()),
            after_json: Some(after.to_string()),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE operator_users SET default_calendar_id = ?3, updated_at_ms = ?4 \
                 WHERE client_id = ?1 AND user_id = ?2",
                params![owned_client, owned_user, owned_calendar, now_ms as i64],
            )?;
            Ok(())
        },
    )
}
