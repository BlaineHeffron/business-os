//! Shared durable connector OAuth CSRF-state persistence.

use bos_contracts::receipt::ActorKindDto;
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use crate::store_core::{self, MutationOutcome, MutationRequest, StoreError};

pub const ENTITY_KIND: &str = "connector_oauth_state";

#[derive(Debug)]
struct PendingOAuthState {
    user_id: String,
    issued_at_ms: u64,
    expires_at_ms: u64,
}

fn oauth_state_hash(state: &str) -> String {
    format!("{:x}", Sha256::digest(state.as_bytes()))
}

pub fn register_oauth_state(
    conn: &mut Connection,
    client_id: &str,
    connector: &str,
    state: &str,
    user_id: &str,
    issued_at_ms: u64,
    ttl_ms: u64,
) -> Result<MutationOutcome, StoreError> {
    let expires_at_ms = issued_at_ms
        .checked_add(ttl_ms)
        .filter(|expires| *expires > issued_at_ms)
        .ok_or_else(|| StoreError::Domain("invalid OAuth state expiry".to_string()))?;
    let state_hash = oauth_state_hash(state);
    let entity_id = format!("{connector}:{state_hash}");
    let idempotency_key = format!("oauth_state_issue:{entity_id}");
    let after_json = serde_json::json!({
        "connector": connector,
        "user_id": user_id,
        "issued_at_ms": issued_at_ms,
        "expires_at_ms": expires_at_ms,
    })
    .to_string();
    let owned_client = client_id.to_string();
    let owned_connector = connector.to_string();
    let owned_hash = state_hash;
    let owned_user = user_id.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: ENTITY_KIND,
            entity_id: &entity_id,
            change_kind: "issue",
            actor_id: user_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(after_json),
            now_ms: issued_at_ms,
        },
        move |tx| {
            tx.execute(
                "DELETE FROM connector_oauth_states WHERE client_id = ?1 AND expires_at_ms <= ?2",
                params![owned_client.as_str(), issued_at_ms as i64],
            )?;
            tx.execute(
                "INSERT INTO connector_oauth_states \
                 (client_id, connector, state_hash, user_id, issued_at_ms, expires_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    owned_client,
                    owned_connector,
                    owned_hash,
                    owned_user,
                    issued_at_ms as i64,
                    expires_at_ms as i64
                ],
            )?;
            Ok(())
        },
    )
}

pub fn consume_oauth_state(
    conn: &mut Connection,
    client_id: &str,
    connector: &str,
    state: &str,
    attempt_id: &str,
    now_ms: u64,
) -> Result<Option<String>, StoreError> {
    let state_hash = oauth_state_hash(state);
    let pending = conn
        .query_row(
            "SELECT user_id, issued_at_ms, expires_at_ms FROM connector_oauth_states \
             WHERE client_id = ?1 AND connector = ?2 AND state_hash = ?3",
            params![client_id, connector, state_hash],
            |row| {
                Ok(PendingOAuthState {
                    user_id: row.get(0)?,
                    issued_at_ms: row.get::<_, i64>(1)? as u64,
                    expires_at_ms: row.get::<_, i64>(2)? as u64,
                })
            },
        )
        .optional()?;
    let Some(pending) = pending else {
        return Ok(None);
    };

    let expired = now_ms >= pending.expires_at_ms;
    let entity_id = format!("{connector}:{state_hash}");
    let change_kind = if expired { "expire" } else { "consume" };
    let idempotency_key = format!("oauth_state_{change_kind}:{entity_id}:{attempt_id}");
    let owned_client = client_id.to_string();
    let owned_connector = connector.to_string();
    let owned_hash = state_hash;
    let expected_issued_at_ms = pending.issued_at_ms;
    let result = store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: ENTITY_KIND,
            entity_id: &entity_id,
            change_kind,
            actor_id: &pending.user_id,
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
            let deleted = tx.execute(
                "DELETE FROM connector_oauth_states \
                 WHERE client_id = ?1 AND connector = ?2 AND state_hash = ?3 AND issued_at_ms = ?4",
                params![
                    owned_client,
                    owned_connector,
                    owned_hash,
                    expected_issued_at_ms as i64
                ],
            )?;
            if deleted != 1 {
                return Err(StoreError::Domain("oauth_state_invalid".to_string()));
            }
            Ok(())
        },
    );
    match result {
        Ok(_) if expired => Ok(None),
        Ok(_) => Ok(Some(pending.user_id)),
        Err(StoreError::Domain(code)) if code == "oauth_state_invalid" => Ok(None),
        Err(err) => Err(err),
    }
}
