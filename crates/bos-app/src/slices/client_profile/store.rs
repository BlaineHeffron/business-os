//! Client profile persistence through store_core (receipted upsert + read).

use bos_contracts::client_profile::ClientProfile;
use bos_contracts::receipt::ActorKindDto;
use rusqlite::{params, Connection, OptionalExtension};

use crate::store_core::{self, MutationOutcome, MutationRequest, StoreError};

pub const ENTITY_KIND: &str = "client_profile";

/// Upsert the client's profile (receipted). One row per client.
pub fn upsert_profile(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    profile: &ClientProfile,
    idempotency_key: &str,
    now_ms: u64,
) -> Result<MutationOutcome, StoreError> {
    let after = serde_json::to_string(profile)
        .map_err(|err| StoreError::Domain(format!("serialize client profile: {err}")))?;
    let row = profile.clone();
    let owned_client = client_id.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: ENTITY_KIND,
            entity_id: client_id,
            change_kind: "upsert",
            actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: None,
            idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO client_profile \
                 (client_id, company_name, bio, industry, website, persona, updated_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                 ON CONFLICT(client_id) DO UPDATE SET \
                   company_name = excluded.company_name, \
                   bio = excluded.bio, \
                   industry = excluded.industry, \
                   website = excluded.website, \
                   persona = excluded.persona, \
                   updated_at_ms = excluded.updated_at_ms",
                params![
                    owned_client,
                    row.company_name,
                    row.bio,
                    row.industry,
                    row.website,
                    row.persona,
                    now_ms as i64
                ],
            )?;
            Ok(())
        },
    )
}

/// The client's profile, or None when none has been seeded.
pub fn load_profile(
    conn: &Connection,
    client_id: &str,
) -> Result<Option<ClientProfile>, StoreError> {
    let row = conn
        .query_row(
            "SELECT client_id, company_name, bio, industry, website, persona \
             FROM client_profile WHERE client_id = ?1",
            params![client_id],
            |row| {
                Ok(ClientProfile {
                    client_id: row.get(0)?,
                    company_name: row.get(1)?,
                    bio: row.get(2)?,
                    industry: row.get(3)?,
                    website: row.get(4)?,
                    persona: row.get(5)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}
