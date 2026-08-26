//! Operator note persistence through store_core.

use bos_contracts::operator_notes::OperatorNote;
use bos_contracts::receipt::ActorKindDto;
use rusqlite::{params, Connection, OptionalExtension};

use crate::store_core::{self, MutationOutcome, MutationRequest, StoreError};

pub const NOTE_ENTITY_KIND: &str = "operator_note";

pub fn insert_note(
    conn: &mut Connection,
    client_id: &str,
    note: &OperatorNote,
    idempotency_key: &str,
) -> Result<MutationOutcome, StoreError> {
    if note.body.trim().is_empty() {
        return Err(StoreError::Domain("operator_note_body_empty".to_string()));
    }
    let after = serde_json::to_string(note)
        .map_err(|err| StoreError::Domain(format!("serialize note: {err}")))?;
    let row = note.clone();
    let owned_client = client_id.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: NOTE_ENTITY_KIND,
            entity_id: &note.note_id,
            change_kind: "create",
            actor_id: &note.created_by,
            actor_kind: ActorKindDto::Operator,
            expected_revision: None,
            idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms: note.created_at_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO operator_notes \
                 (client_id, note_id, body, category_id, created_by, created_at_ms, \
                  updated_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6) \
                 ON CONFLICT (client_id, note_id) DO NOTHING",
                params![
                    owned_client,
                    row.note_id,
                    row.body,
                    row.category_id,
                    row.created_by,
                    row.created_at_ms as i64,
                ],
            )?;
            Ok(())
        },
    )
}

pub fn get_note(
    conn: &Connection,
    client_id: &str,
    note_id: &str,
) -> Result<Option<OperatorNote>, StoreError> {
    let row = conn
        .query_row(
            "SELECT note_id, body, category_id, created_by, created_at_ms \
             FROM operator_notes WHERE client_id = ?1 AND note_id = ?2",
            params![client_id, note_id],
            note_from_row,
        )
        .optional()?;
    Ok(row)
}

pub fn list_recent(
    conn: &Connection,
    client_id: &str,
    limit: usize,
) -> Result<Vec<OperatorNote>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT note_id, body, category_id, created_by, created_at_ms \
         FROM operator_notes WHERE client_id = ?1 \
         ORDER BY created_at_ms DESC, note_id DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![client_id, limit as i64], note_from_row)?;
    let mut notes = Vec::new();
    for row in rows {
        notes.push(row?);
    }
    Ok(notes)
}

fn note_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<OperatorNote> {
    Ok(OperatorNote {
        note_id: row.get(0)?,
        body: row.get(1)?,
        category_id: row.get(2)?,
        created_by: row.get(3)?,
        created_at_ms: row.get::<_, i64>(4)? as u64,
    })
}
