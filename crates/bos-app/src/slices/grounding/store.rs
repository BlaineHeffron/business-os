use bos_contracts::receipt::ActorKindDto;
use rusqlite::{params, Connection, Row};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::http::OperatorScope;
use crate::store_core::{self, MutationOutcome, MutationRequest, StoreError};

use super::util::capped_chars;
use super::MAX_EXCERPT_CHARS;

pub const GROUNDING_EVIDENCE_ENTITY_KIND: &str = "grounding_evidence";

const MAX_TOOL_ARGS_CHARS: usize = 2_000;
const MAX_EVIDENCE_ROWS_PER_ATTEMPT: usize = 12;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroundingEvidenceRow {
    pub evidence_id: String,
    pub work_item_id: String,
    pub draft_id: Option<String>,
    pub packet_kind: String,
    pub attempt: u64,
    pub grounding_mode: String,
    pub source_kind: String,
    pub source_ref: String,
    pub tool_name: String,
    pub tool_args_json: String,
    pub result_ref: String,
    pub result_excerpt: String,
    pub scope_label: String,
    pub scope_user_id: Option<String>,
    pub actor_id: String,
    pub actor_kind: ActorKindDto,
    pub correlation_id: Option<String>,
    pub causation_id: Option<String>,
    pub created_at_ms: u64,
}

pub struct NewGroundingEvidence<'a> {
    pub work_item_id: &'a str,
    pub draft_id: Option<&'a str>,
    pub packet_kind: &'a str,
    pub attempt: u64,
    pub source_kind: &'a str,
    pub source_ref: &'a str,
    pub tool_name: &'a str,
    pub tool_args_json: &'a str,
    pub result_ref: &'a str,
    pub result_excerpt: &'a str,
    pub scope: &'a OperatorScope,
    pub actor_id: &'a str,
    pub actor_kind: ActorKindDto,
    pub now_ms: u64,
}

pub fn append_grounding_evidence(
    conn: &mut Connection,
    client_id: &str,
    evidence: NewGroundingEvidence<'_>,
) -> Result<Option<MutationOutcome>, StoreError> {
    if !work_item_exists(conn, client_id, evidence.work_item_id)? {
        tracing::debug!(
            work_item_id = evidence.work_item_id,
            packet_kind = evidence.packet_kind,
            tool_name = evidence.tool_name,
            "skipping grounding evidence append for non-persisted work item"
        );
        return Ok(None);
    }
    let row_count = grounding_evidence_count(
        conn,
        client_id,
        evidence.work_item_id,
        evidence.packet_kind,
        evidence.attempt,
    )?;
    if row_count >= MAX_EVIDENCE_ROWS_PER_ATTEMPT {
        return Err(StoreError::Domain(
            "grounding_evidence_row_limit".to_string(),
        ));
    }
    let (scope_label, scope_user_id) = scope_parts(evidence.scope);
    let bounded_args = capped_chars(evidence.tool_args_json, MAX_TOOL_ARGS_CHARS);
    let bounded_excerpt = capped_chars(evidence.result_excerpt, MAX_EXCERPT_CHARS);
    let args_hash = stable_hash(&bounded_args);
    let evidence_id = format!(
        "ge_{}_{}_{}_{}_{}",
        sanitize_id(evidence.work_item_id),
        sanitize_id(evidence.packet_kind),
        evidence.attempt,
        sanitize_id(evidence.tool_name),
        &args_hash[..12]
    );
    let idempotency_key = format!(
        "grounding:{}:{}:{}:{}:{}",
        evidence.work_item_id,
        evidence.packet_kind,
        evidence.attempt,
        evidence.tool_name,
        args_hash
    );
    let row = GroundingEvidenceRow {
        evidence_id: evidence_id.clone(),
        work_item_id: evidence.work_item_id.to_string(),
        draft_id: evidence.draft_id.map(str::to_string),
        packet_kind: evidence.packet_kind.to_string(),
        attempt: evidence.attempt,
        grounding_mode: "deterministic".to_string(),
        source_kind: evidence.source_kind.to_string(),
        source_ref: evidence.source_ref.to_string(),
        tool_name: evidence.tool_name.to_string(),
        tool_args_json: bounded_args,
        result_ref: evidence.result_ref.to_string(),
        result_excerpt: bounded_excerpt,
        scope_label: scope_label.to_string(),
        scope_user_id,
        actor_id: evidence.actor_id.to_string(),
        actor_kind: evidence.actor_kind,
        correlation_id: Some(evidence.work_item_id.to_string()),
        causation_id: Some(evidence.source_ref.to_string()),
        created_at_ms: evidence.now_ms,
    };
    let after = serde_json::to_string(&row)
        .map_err(|err| StoreError::Domain(format!("serialize grounding evidence: {err}")))?;
    let owned_client = client_id.to_string();
    let owned_row = row.clone();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: GROUNDING_EVIDENCE_ENTITY_KIND,
            entity_id: &evidence_id,
            change_kind: "append",
            actor_id: evidence.actor_id,
            actor_kind: evidence.actor_kind,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            correlation_id: Some(evidence.work_item_id),
            causation_id: Some(evidence.source_ref),
            before_json: None,
            after_json: Some(after),
            now_ms: evidence.now_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO grounding_evidence \
                 (client_id, evidence_id, work_item_id, draft_id, packet_kind, attempt, \
                  grounding_mode, source_kind, source_ref, tool_name, tool_args_json, \
                  result_ref, result_excerpt, scope_label, scope_user_id, actor_id, actor_kind, \
                  correlation_id, causation_id, created_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, \
                         ?16, ?17, ?18, ?19, ?20)",
                params![
                    owned_client,
                    owned_row.evidence_id,
                    owned_row.work_item_id,
                    owned_row.draft_id,
                    owned_row.packet_kind,
                    owned_row.attempt as i64,
                    owned_row.grounding_mode,
                    owned_row.source_kind,
                    owned_row.source_ref,
                    owned_row.tool_name,
                    owned_row.tool_args_json,
                    owned_row.result_ref,
                    owned_row.result_excerpt,
                    owned_row.scope_label,
                    owned_row.scope_user_id,
                    owned_row.actor_id,
                    actor_kind_str(owned_row.actor_kind),
                    owned_row.correlation_id,
                    owned_row.causation_id,
                    owned_row.created_at_ms as i64,
                ],
            )?;
            Ok(())
        },
    )
    .map(Some)
}

pub fn grounding_evidence_for_item(
    conn: &Connection,
    client_id: &str,
    work_item_id: &str,
) -> Result<Vec<GroundingEvidenceRow>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT evidence_id, work_item_id, draft_id, packet_kind, attempt, grounding_mode, \
         source_kind, source_ref, tool_name, tool_args_json, result_ref, result_excerpt, \
         scope_label, scope_user_id, actor_id, actor_kind, correlation_id, causation_id, \
         created_at_ms \
         FROM grounding_evidence WHERE client_id = ?1 AND work_item_id = ?2 \
         ORDER BY packet_kind ASC, attempt ASC, created_at_ms ASC, evidence_id ASC",
    )?;
    let rows = stmt.query_map(params![client_id, work_item_id], grounding_row_from_row)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn work_item_exists(
    conn: &Connection,
    client_id: &str,
    work_item_id: &str,
) -> Result<bool, StoreError> {
    let found: i64 = conn.query_row(
        "SELECT COUNT(*) FROM work_items WHERE client_id = ?1 AND item_id = ?2",
        params![client_id, work_item_id],
        |row| row.get(0),
    )?;
    Ok(found > 0)
}

fn grounding_evidence_count(
    conn: &Connection,
    client_id: &str,
    work_item_id: &str,
    packet_kind: &str,
    attempt: u64,
) -> Result<usize, StoreError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM grounding_evidence \
         WHERE client_id = ?1 AND work_item_id = ?2 AND packet_kind = ?3 AND attempt = ?4",
        params![client_id, work_item_id, packet_kind, attempt as i64],
        |row| row.get(0),
    )?;
    Ok(count as usize)
}

fn grounding_row_from_row(row: &Row<'_>) -> rusqlite::Result<GroundingEvidenceRow> {
    Ok(GroundingEvidenceRow {
        evidence_id: row.get("evidence_id")?,
        work_item_id: row.get("work_item_id")?,
        draft_id: row.get("draft_id")?,
        packet_kind: row.get("packet_kind")?,
        attempt: row.get::<_, i64>("attempt")? as u64,
        grounding_mode: row.get("grounding_mode")?,
        source_kind: row.get("source_kind")?,
        source_ref: row.get("source_ref")?,
        tool_name: row.get("tool_name")?,
        tool_args_json: row.get("tool_args_json")?,
        result_ref: row.get("result_ref")?,
        result_excerpt: row.get("result_excerpt")?,
        scope_label: row.get("scope_label")?,
        scope_user_id: row.get("scope_user_id")?,
        actor_id: row.get("actor_id")?,
        actor_kind: actor_kind_from_str(&row.get::<_, String>("actor_kind")?),
        correlation_id: row.get("correlation_id")?,
        causation_id: row.get("causation_id")?,
        created_at_ms: row.get::<_, i64>("created_at_ms")? as u64,
    })
}

fn stable_hash(raw: &str) -> String {
    let digest = Sha256::digest(raw.as_bytes());
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn sanitize_id(raw: &str) -> String {
    raw.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .take(80)
        .collect()
}

fn scope_parts(scope: &OperatorScope) -> (&'static str, Option<String>) {
    match scope {
        OperatorScope::All => ("all", None),
        OperatorScope::User(user_id) => ("user", Some(user_id.clone())),
    }
}

fn actor_kind_str(kind: ActorKindDto) -> &'static str {
    match kind {
        ActorKindDto::Operator => "operator",
        ActorKindDto::System => "system",
        ActorKindDto::Agent => "agent",
    }
}

fn actor_kind_from_str(raw: &str) -> ActorKindDto {
    match raw {
        "operator" => ActorKindDto::Operator,
        "agent" => ActorKindDto::Agent,
        _ => ActorKindDto::System,
    }
}

#[cfg(test)]
mod tests {
    use super::super::{GROUNDING_ACTOR, TOOL_RESOLVE_PARTY};
    use super::*;

    const CLIENT: &str = "test-client";

    #[test]
    fn grounding_evidence_cascades_with_work_item_delete() {
        let mut persistence = crate::persistence::Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        conn.execute(
            "INSERT INTO work_items \
             (client_id, item_id, source_kind, source_ref, category_id, title, summary, \
              packet_kinds_json, status, created_at_ms, updated_at_ms) \
             VALUES (?1, 'wi1', 'email', 'src1', 'billing', 'Title', '', '[\"invoice_draft\"]', \
                     'accepted', 1, 1)",
            params![CLIENT],
        )
        .expect("work item");
        append_grounding_evidence(
            conn,
            CLIENT,
            NewGroundingEvidence {
                work_item_id: "wi1",
                draft_id: None,
                packet_kind: "invoice_draft",
                attempt: 1,
                source_kind: "email",
                source_ref: "src1",
                tool_name: TOOL_RESOLVE_PARTY,
                tool_args_json: "{}",
                result_ref: "party:none",
                result_excerpt: "none",
                scope: &OperatorScope::All,
                actor_id: GROUNDING_ACTOR,
                actor_kind: ActorKindDto::System,
                now_ms: 2,
            },
        )
        .expect("evidence");
        conn.execute(
            "DELETE FROM work_items WHERE client_id = ?1 AND item_id = 'wi1'",
            params![CLIENT],
        )
        .expect("delete work item");
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM grounding_evidence WHERE client_id = ?1",
                params![CLIENT],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(count, 0);
    }
}
