//! Call input persistence through store_core.

use bos_contracts::call_inputs::{
    CallInputItem, CallInputStatus, CallInputWithRevision, CallInputsDriveSettingsUpdateRequest,
};
use bos_contracts::receipt::ActorKindDto;
use bos_contracts::work_queue::WorkItemStatus;
use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::slices::mutation_context::MutationContext;
use crate::store_core::{self, MutationOutcome, MutationRequest, StoreError};

pub const CALL_INPUT_ENTITY_KIND: &str = "call_input";
pub const DRIVE_SETTINGS_ENTITY_KIND: &str = "call_input_drive_settings";
pub const DRIVE_SETTINGS_ENTITY_ID: &str = "call_input_drive_settings";

const CALL_INPUT_COLUMNS: &str =
    "c.call_input_id, c.source_id, c.source_ref, c.input_kind, c.status, \
     c.title, c.summary, c.caller_name, c.caller_phone, c.caller_email, c.transcript_text, \
     c.recording_ref_json, c.transcription_meta_json, c.occurred_at_ms, c.captured_at_ms, c.work_item_id, \
     c.created_at_ms, c.updated_at_ms, COALESCE(er.revision, 0) AS revision";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredDriveSettings {
    pub credential_user_id: Option<String>,
    pub drive_folder_id: Option<String>,
    pub drive_folder_name: Option<String>,
    pub ingestion_enabled: bool,
    pub interval_secs: Option<u64>,
    pub revision: Option<u64>,
}

fn input_from_row(row: &Row<'_>) -> rusqlite::Result<CallInputWithRevision> {
    let recording_ref_json: String = row.get("recording_ref_json")?;
    let transcription_meta_json: Option<String> = row.get("transcription_meta_json")?;
    Ok(CallInputWithRevision {
        input: CallInputItem {
            call_input_id: row.get("call_input_id")?,
            source_id: row.get("source_id")?,
            source_ref: row.get("source_ref")?,
            input_kind: input_kind_from_str(&row.get::<_, String>("input_kind")?),
            status: status_from_str(&row.get::<_, String>("status")?),
            title: row.get("title")?,
            summary: row.get("summary")?,
            caller_name: row.get("caller_name")?,
            caller_phone: row.get("caller_phone")?,
            caller_email: row.get("caller_email")?,
            transcript_text: row.get("transcript_text")?,
            recording_ref: serde_json::from_str(&recording_ref_json).map_err(|err| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(err),
                )
            })?,
            transcription_meta: transcription_meta_json
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
                .map_err(|err| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(err),
                    )
                })?,
            occurred_at_ms: row
                .get::<_, Option<i64>>("occurred_at_ms")?
                .map(|ms| ms as u64),
            captured_at_ms: row
                .get::<_, Option<i64>>("captured_at_ms")?
                .map(|ms| ms as u64),
            work_item_id: row.get("work_item_id")?,
            created_at_ms: row.get::<_, i64>("created_at_ms")? as u64,
            updated_at_ms: row.get::<_, i64>("updated_at_ms")? as u64,
        },
        revision: row.get::<_, i64>("revision")? as u64,
    })
}

pub fn get_input(
    conn: &Connection,
    client_id: &str,
    call_input_id: &str,
) -> Result<Option<CallInputWithRevision>, StoreError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {CALL_INPUT_COLUMNS} FROM call_inputs c \
         LEFT JOIN entity_revisions er ON er.client_id = c.client_id \
           AND er.entity_kind = ?3 AND er.entity_id = c.call_input_id \
         WHERE c.client_id = ?1 AND c.call_input_id = ?2"
    ))?;
    Ok(stmt
        .query_row(
            params![client_id, call_input_id, CALL_INPUT_ENTITY_KIND],
            input_from_row,
        )
        .optional()?)
}

pub fn list_inputs(
    conn: &Connection,
    client_id: &str,
    status: Option<CallInputStatus>,
    limit: usize,
) -> Result<Vec<CallInputWithRevision>, StoreError> {
    let mut sql = format!(
        "SELECT {CALL_INPUT_COLUMNS} FROM call_inputs c \
         LEFT JOIN entity_revisions er ON er.client_id = c.client_id \
           AND er.entity_kind = ?2 AND er.entity_id = c.call_input_id \
         WHERE c.client_id = ?1"
    );
    let status_string = status.map(status_str).map(str::to_string);
    if status_string.is_some() {
        sql.push_str(" AND c.status = ?3");
    }
    sql.push_str(" ORDER BY c.updated_at_ms DESC, c.call_input_id DESC LIMIT ?");
    let mut stmt = conn.prepare(&sql)?;
    let limit_i64 = limit as i64;
    let rows = if let Some(status) = status_string.as_deref() {
        stmt.query_map(
            params![client_id, CALL_INPUT_ENTITY_KIND, status, limit_i64],
            input_from_row,
        )?
    } else {
        stmt.query_map(
            params![client_id, CALL_INPUT_ENTITY_KIND, limit_i64],
            input_from_row,
        )?
    };
    let mut inputs = Vec::new();
    for row in rows {
        inputs.push(row?);
    }
    Ok(inputs)
}

pub fn get_drive_settings(
    conn: &Connection,
    client_id: &str,
) -> Result<Option<StoredDriveSettings>, StoreError> {
    struct DriveSettingsRow {
        drive_folder_id: Option<String>,
        credential_user_id: Option<String>,
        drive_folder_name: Option<String>,
        ingestion_enabled: i64,
        interval_secs: Option<i64>,
    }

    let row: Option<DriveSettingsRow> = conn
        .query_row(
            "SELECT credential_user_id, drive_folder_id, drive_folder_name, ingestion_enabled, interval_secs \
             FROM call_input_drive_settings \
             WHERE client_id = ?1",
            params![client_id],
            |row| {
                Ok(DriveSettingsRow {
                    credential_user_id: row.get(0)?,
                    drive_folder_id: row.get(1)?,
                    drive_folder_name: row.get(2)?,
                    ingestion_enabled: row.get(3)?,
                    interval_secs: row.get(4)?,
                })
            },
        )
        .optional()?;
    let Some(row) = row else {
        return Ok(None);
    };
    let revision = store_core::current_revision(
        conn,
        client_id,
        DRIVE_SETTINGS_ENTITY_KIND,
        DRIVE_SETTINGS_ENTITY_ID,
    )?;
    Ok(Some(StoredDriveSettings {
        credential_user_id: row.credential_user_id,
        drive_folder_id: row.drive_folder_id,
        drive_folder_name: row.drive_folder_name,
        ingestion_enabled: row.ingestion_enabled != 0,
        interval_secs: row.interval_secs.map(|value| value.max(0) as u64),
        revision,
    }))
}

pub fn replace_drive_settings(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    credential_user_id: Option<&str>,
    request: &CallInputsDriveSettingsUpdateRequest,
    now_ms: u64,
) -> Result<MutationOutcome, StoreError> {
    let folder_id = normalized_optional(&request.drive_folder_id, 256);
    let folder_name = normalized_optional(&request.drive_folder_name, 500);
    if let Some(interval) = request.interval_secs {
        if !(60..=86_400).contains(&interval) {
            return Err(StoreError::Domain(
                "call_input_drive_interval_invalid".to_string(),
            ));
        }
    }
    if folder_id.is_none() && folder_name.is_some() {
        return Err(StoreError::Domain(
            "call_input_drive_folder_id_required".to_string(),
        ));
    }
    let write_credential_user_id = folder_id
        .as_ref()
        .and_then(|_| credential_user_id.map(str::to_string));
    let write_ingestion_enabled = folder_id.is_some();
    let before_json = get_drive_settings(conn, client_id)?.and_then(|settings| {
        serde_json::to_string(&serde_json::json!({
            "drive_folder_id": settings.drive_folder_id,
            "credential_user_id": settings.credential_user_id,
            "drive_folder_name": settings.drive_folder_name,
            "ingestion_enabled": settings.ingestion_enabled,
            "interval_secs": settings.interval_secs,
        }))
        .ok()
    });
    let after_json = serde_json::to_string(&serde_json::json!({
        "drive_folder_id": folder_id,
        "credential_user_id": write_credential_user_id,
        "drive_folder_name": folder_name,
        "ingestion_enabled": write_ingestion_enabled,
        "interval_secs": request.interval_secs,
    }))
    .map_err(|err| StoreError::Domain(format!("serialize call input drive settings: {err}")))?;
    let write_folder_id = folder_id.clone();
    let write_folder_name = folder_name.clone();
    let write_interval_secs = request.interval_secs;
    let owned_client = client_id.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: DRIVE_SETTINGS_ENTITY_KIND,
            entity_id: DRIVE_SETTINGS_ENTITY_ID,
            change_kind: "replace",
            actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: request.expected_revision,
            idempotency_key: &request.idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json,
            after_json: Some(after_json),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO call_input_drive_settings \
                 (client_id, credential_user_id, drive_folder_id, drive_folder_name, ingestion_enabled, interval_secs, \
                  updated_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                 ON CONFLICT(client_id) DO UPDATE SET \
                   credential_user_id = excluded.credential_user_id, \
                   drive_folder_id = excluded.drive_folder_id, \
                   drive_folder_name = excluded.drive_folder_name, \
                   ingestion_enabled = excluded.ingestion_enabled, \
                   interval_secs = excluded.interval_secs, \
                   updated_at_ms = excluded.updated_at_ms",
                params![
                    owned_client,
                    write_credential_user_id,
                    write_folder_id,
                    write_folder_name,
                    if write_ingestion_enabled { 1 } else { 0 },
                    write_interval_secs.map(|value| value as i64),
                    now_ms as i64,
                ],
            )?;
            Ok(())
        },
    )
}

pub fn insert_input(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    actor_kind: ActorKindDto,
    input: &CallInputItem,
    idempotency_key: &str,
) -> Result<MutationOutcome, StoreError> {
    let after = serde_json::to_string(input)
        .map_err(|err| StoreError::Domain(format!("serialize call input: {err}")))?;
    let recording_ref_json = serde_json::to_string(&input.recording_ref)
        .map_err(|err| StoreError::Domain(format!("serialize recording ref: {err}")))?;
    let transcription_meta_json = input
        .transcription_meta
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|err| StoreError::Domain(format!("serialize transcription meta: {err}")))?;
    let row = input.clone();
    let owned_client = client_id.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: CALL_INPUT_ENTITY_KIND,
            entity_id: &input.call_input_id,
            change_kind: "stage",
            actor_id,
            actor_kind,
            expected_revision: None,
            idempotency_key,
            correlation_id: Some(&input.source_id),
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms: input.created_at_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO call_inputs \
                 (client_id, call_input_id, source_id, source_ref, input_kind, status, title, summary, \
                  caller_name, caller_phone, caller_email, transcript_text, recording_ref_json, transcription_meta_json, \
                  occurred_at_ms, captured_at_ms, work_item_id, \
                  created_at_ms, updated_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 'staged', ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, \
                  ?14, ?15, NULL, ?16, ?16)",
                params![
                    owned_client,
                    row.call_input_id,
                    row.source_id,
                    row.source_ref,
                    input_kind_str(row.input_kind),
                    row.title,
                    row.summary,
                    row.caller_name,
                    row.caller_phone,
                    row.caller_email,
                    row.transcript_text,
                    recording_ref_json,
                    transcription_meta_json,
                    row.occurred_at_ms.map(|ms| ms as i64),
                    row.captured_at_ms.map(|ms| ms as i64),
                    row.created_at_ms as i64,
                ],
            )?;
            Ok(())
        },
    )
}

pub fn record_transcription_failure(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    source_ref: &str,
    idempotency_key: &str,
    error_code: &str,
    now_ms: u64,
) -> Result<(), StoreError> {
    let entity_id = format!("transcription:{source_ref}");
    let after = serde_json::json!({
        "source_ref": source_ref,
        "error_code": error_code,
    })
    .to_string();
    match store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: CALL_INPUT_ENTITY_KIND,
            entity_id: &entity_id,
            change_kind: "transcription_failed",
            actor_id,
            actor_kind: ActorKindDto::System,
            expected_revision: None,
            idempotency_key,
            correlation_id: Some(source_ref),
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms,
        },
        |_tx| Err(StoreError::Domain(error_code.to_string())),
    ) {
        Err(StoreError::Domain(code)) if code == error_code => Ok(()),
        Err(err) => Err(err),
        Ok(_) => Ok(()),
    }
}

pub fn accept_input(
    conn: &mut Connection,
    ctx: MutationContext<'_>,
    call_input_id: &str,
    packet_kinds: &[String],
) -> Result<MutationOutcome, StoreError> {
    let current = get_input(conn, ctx.client_id, call_input_id)?
        .ok_or_else(|| StoreError::Domain("call_input_not_found".to_string()))?
        .input;
    let owned_client = ctx.client_id.to_string();
    let owned_input_id = call_input_id.to_string();
    let input_for_item = current.clone();
    if packet_kinds.is_empty() {
        return Err(StoreError::Domain(
            "call_input_packet_kinds_required".to_string(),
        ));
    }
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: CALL_INPUT_ENTITY_KIND,
            entity_id: call_input_id,
            change_kind: "accept",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: Some(call_input_id),
            causation_id: None,
            before_json: Some("{\"status\":\"staged\"}".to_string()),
            after_json: Some("{\"status\":\"accepted\"}".to_string()),
            now_ms: ctx.now_ms,
        },
        move |tx| {
            if input_for_item.status != CallInputStatus::Staged {
                return Err(StoreError::Domain("call_input_not_staged".to_string()));
            }
            let work_item_id = emit_work_item_for_input(
                tx,
                &owned_client,
                &input_for_item,
                packet_kinds,
                ctx.now_ms,
            )?;
            tx.execute(
                "UPDATE call_inputs SET status = 'accepted', work_item_id = ?3, \
                 updated_at_ms = ?4 WHERE client_id = ?1 AND call_input_id = ?2",
                params![
                    owned_client,
                    owned_input_id,
                    work_item_id,
                    ctx.now_ms as i64
                ],
            )?;
            Ok(())
        },
    )
}

pub fn reject_input(
    conn: &mut Connection,
    ctx: MutationContext<'_>,
    call_input_id: &str,
) -> Result<MutationOutcome, StoreError> {
    let current = get_input(conn, ctx.client_id, call_input_id)?
        .ok_or_else(|| StoreError::Domain("call_input_not_found".to_string()))?
        .input;
    let owned_client = ctx.client_id.to_string();
    let owned_input_id = call_input_id.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: CALL_INPUT_ENTITY_KIND,
            entity_id: call_input_id,
            change_kind: "reject",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: Some(call_input_id),
            causation_id: None,
            before_json: Some("{\"status\":\"staged\"}".to_string()),
            after_json: Some("{\"status\":\"rejected\"}".to_string()),
            now_ms: ctx.now_ms,
        },
        move |tx| {
            if current.status != CallInputStatus::Staged {
                return Err(StoreError::Domain("call_input_not_staged".to_string()));
            }
            tx.execute(
                "UPDATE call_inputs SET status = 'rejected', updated_at_ms = ?3 \
                 WHERE client_id = ?1 AND call_input_id = ?2",
                params![owned_client, owned_input_id, ctx.now_ms as i64],
            )?;
            Ok(())
        },
    )
}

fn emit_work_item_for_input(
    tx: &rusqlite::Transaction<'_>,
    client_id: &str,
    input: &CallInputItem,
    packet_kinds: &[String],
    now_ms: u64,
) -> Result<String, StoreError> {
    let item_id = format!(
        "wi_{}_{}",
        super::SOURCE_KIND_CALL_INPUT,
        input.call_input_id
    );
    let packet_kinds_json = serde_json::to_string(packet_kinds)
        .map_err(|err| StoreError::Domain(format!("serialize packet kinds: {err}")))?;
    tx.execute(
        "INSERT INTO work_items \
         (client_id, item_id, source_kind, source_ref, category_id, title, summary, \
          packet_kinds_json, status, ai_suggested, rationale, produce_guidance, \
          created_at_ms, updated_at_ms, source_user_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, '', '', ?10, ?10, NULL)",
        params![
            client_id,
            item_id,
            super::SOURCE_KIND_CALL_INPUT,
            input.call_input_id,
            super::service::CATEGORY_ID,
            input.title,
            input.summary,
            packet_kinds_json,
            work_item_status_str(WorkItemStatus::Open),
            now_ms as i64,
        ],
    )?;
    crate::store_core::initialize_revision_within(
        tx,
        client_id,
        crate::slices::work_queue::store::ITEM_ENTITY_KIND,
        &item_id,
        1,
        now_ms,
    )?;
    Ok(item_id)
}

fn input_kind_str(kind: bos_contracts::call_inputs::CallInputKind) -> &'static str {
    match kind {
        bos_contracts::call_inputs::CallInputKind::CallLog => "call_log",
        bos_contracts::call_inputs::CallInputKind::Transcript => "transcript",
        bos_contracts::call_inputs::CallInputKind::Recording => "recording",
    }
}

fn normalized_optional(raw: &Option<String>, limit: usize) -> Option<String> {
    raw.as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(limit).collect())
}

fn input_kind_from_str(raw: &str) -> bos_contracts::call_inputs::CallInputKind {
    match raw {
        "transcript" => bos_contracts::call_inputs::CallInputKind::Transcript,
        "recording" => bos_contracts::call_inputs::CallInputKind::Recording,
        _ => bos_contracts::call_inputs::CallInputKind::CallLog,
    }
}

fn status_str(status: CallInputStatus) -> &'static str {
    match status {
        CallInputStatus::Staged => "staged",
        CallInputStatus::Accepted => "accepted",
        CallInputStatus::Rejected => "rejected",
    }
}

fn status_from_str(raw: &str) -> CallInputStatus {
    match raw {
        "accepted" => CallInputStatus::Accepted,
        "rejected" => CallInputStatus::Rejected,
        _ => CallInputStatus::Staged,
    }
}

fn work_item_status_str(status: WorkItemStatus) -> &'static str {
    match status {
        WorkItemStatus::Open => "open",
        WorkItemStatus::Accepted => "accepted",
        WorkItemStatus::Dismissed => "dismissed",
    }
}
