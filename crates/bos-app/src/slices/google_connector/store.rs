//! Credential persistence, keyed (client, operator user, google service).
//! Mutations flow through store_core (receipted), but the receipt payload
//! NEVER contains the refresh token — only scope metadata.

use bos_contracts::receipt::ActorKindDto;
use rusqlite::{params, Connection, OptionalExtension};

use crate::store_core::{self, MutationOutcome, MutationRequest, StoreError};

pub const ENTITY_KIND: &str = "google_oauth_credential";
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredCredential {
    pub user_id: String,
    pub refresh_token: String,
    pub scopes: Vec<String>,
    pub connected_at_ms: u64,
}

pub fn get_credential(
    conn: &Connection,
    client_id: &str,
    user_id: &str,
    service: &str,
) -> Result<Option<StoredCredential>, StoreError> {
    let row = conn
        .query_row(
            "SELECT user_id, refresh_token, scopes_json, connected_at_ms \
             FROM google_oauth_credentials \
             WHERE client_id = ?1 AND user_id = ?2 AND service = ?3",
            params![client_id, user_id, service],
            credential_from_row,
        )
        .optional()?;
    Ok(row)
}

/// Every connected credential for a service, oldest connection first (a
/// stable order so single-credential fallback is deterministic).
pub fn list_credentials(
    conn: &Connection,
    client_id: &str,
    service: &str,
) -> Result<Vec<StoredCredential>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT user_id, refresh_token, scopes_json, connected_at_ms \
         FROM google_oauth_credentials \
         WHERE client_id = ?1 AND service = ?2 \
         ORDER BY connected_at_ms ASC, user_id ASC",
    )?;
    let rows = stmt.query_map(params![client_id, service], credential_from_row)?;
    let mut credentials = Vec::new();
    for row in rows {
        credentials.push(row?);
    }
    Ok(credentials)
}

fn credential_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredCredential> {
    Ok(StoredCredential {
        user_id: row.get(0)?,
        refresh_token: row.get(1)?,
        scopes: serde_json::from_str(&row.get::<_, String>(2)?).unwrap_or_default(),
        connected_at_ms: row.get::<_, i64>(3)? as u64,
    })
}

pub fn store_credential(
    conn: &mut Connection,
    client_id: &str,
    user_id: &str,
    service: &str,
    refresh_token: &str,
    scopes: &[String],
    now_ms: u64,
) -> Result<MutationOutcome, StoreError> {
    let scopes_json = serde_json::to_string(scopes)
        .map_err(|err| StoreError::Domain(format!("serialize scopes: {err}")))?;
    // Receipt payload: scopes only. The token must never enter the audit trail.
    let redacted_after = serde_json::json!({
        "refresh_token": "[redacted]",
        "user_id": user_id,
        "scopes": scopes,
    })
    .to_string();
    let idempotency_key = format!("connect:{service}:{user_id}:{now_ms}");
    let entity_id = format!("{service}:{user_id}");
    let owned_service = service.to_string();
    let owned_user = user_id.to_string();
    let owned_token = refresh_token.to_string();
    let owned_client = client_id.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: ENTITY_KIND,
            entity_id: &entity_id,
            change_kind: "connect",
            actor_id: user_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(redacted_after),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO google_oauth_credentials \
                 (client_id, user_id, service, refresh_token, scopes_json, connected_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                 ON CONFLICT (client_id, user_id, service) DO UPDATE SET \
                   refresh_token = excluded.refresh_token, \
                   scopes_json = excluded.scopes_json, \
                   connected_at_ms = excluded.connected_at_ms",
                params![
                    owned_client,
                    owned_user,
                    owned_service,
                    owned_token,
                    scopes_json,
                    now_ms as i64
                ],
            )?;
            Ok(())
        },
    )
}

pub fn delete_credential(
    conn: &mut Connection,
    client_id: &str,
    user_id: &str,
    service: &str,
    now_ms: u64,
) -> Result<MutationOutcome, StoreError> {
    let idempotency_key = format!("disconnect:{service}:{user_id}:{now_ms}");
    let entity_id = format!("{service}:{user_id}");
    let owned_service = service.to_string();
    let owned_user = user_id.to_string();
    let owned_client = client_id.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: ENTITY_KIND,
            entity_id: &entity_id,
            change_kind: "disconnect",
            actor_id: user_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: None,
            now_ms,
        },
        move |tx| {
            tx.execute(
                "DELETE FROM google_oauth_credentials \
                 WHERE client_id = ?1 AND user_id = ?2 AND service = ?3",
                params![owned_client, owned_user, owned_service],
            )?;
            Ok(())
        },
    )
}

pub fn mark_credential_revoked(
    conn: &mut Connection,
    client_id: &str,
    user_id: &str,
    service: &str,
    reason: &str,
    now_ms: u64,
) -> Result<MutationOutcome, StoreError> {
    let idempotency_key = format!("oauth_revoked:{service}:{user_id}:{now_ms}");
    let entity_id = format!("{service}:{user_id}");
    let owned_service = service.to_string();
    let owned_user = user_id.to_string();
    let owned_client = client_id.to_string();
    let after_json = serde_json::json!({
        "user_id": user_id,
        "service": service,
        "reason": reason,
    })
    .to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: ENTITY_KIND,
            entity_id: &entity_id,
            change_kind: "oauth_revoked",
            actor_id: "gmail_ingest_pump",
            actor_kind: ActorKindDto::System,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(after_json),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "DELETE FROM google_oauth_credentials \
                 WHERE client_id = ?1 AND user_id = ?2 AND service = ?3",
                params![owned_client, owned_user, owned_service],
            )?;
            Ok(())
        },
    )
}
