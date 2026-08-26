//! Lead finding persistence through store_core.

use bos_contracts::lead_discovery::{LeadFinding, LeadFindingStatus, LeadFindingWithRevision};
use bos_contracts::receipt::ActorKindDto;
use bos_contracts::source::EvidenceRecord;
use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::slices::mutation_context::MutationContext;
use crate::store_core::{self, MutationOutcome, MutationRequest, StoreError};

pub const FINDING_ENTITY_KIND: &str = "lead_finding";

const FINDING_COLUMNS: &str = "f.finding_id, f.source_id, f.status, f.title, f.summary, \
     f.contact_hint, f.company_hint, f.matched_terms_json, f.evidence_json, f.work_item_id, \
     f.created_at_ms, f.updated_at_ms, COALESCE(er.revision, 0) AS revision";

fn finding_from_row(row: &Row<'_>) -> rusqlite::Result<LeadFindingWithRevision> {
    let evidence_json: String = row.get("evidence_json")?;
    Ok(LeadFindingWithRevision {
        finding: LeadFinding {
            finding_id: row.get("finding_id")?,
            source_id: row.get("source_id")?,
            status: status_from_str(&row.get::<_, String>("status")?),
            title: row.get("title")?,
            summary: row.get("summary")?,
            contact_hint: row.get("contact_hint")?,
            company_hint: row.get("company_hint")?,
            matched_terms: serde_json::from_str(&row.get::<_, String>("matched_terms_json")?)
                .unwrap_or_default(),
            evidence: serde_json::from_str::<EvidenceRecord>(&evidence_json).map_err(|err| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(err),
                )
            })?,
            work_item_id: row.get("work_item_id")?,
            created_at_ms: row.get::<_, i64>("created_at_ms")? as u64,
            updated_at_ms: row.get::<_, i64>("updated_at_ms")? as u64,
        },
        revision: row.get::<_, i64>("revision")? as u64,
    })
}

pub fn get_finding(
    conn: &Connection,
    client_id: &str,
    finding_id: &str,
) -> Result<Option<LeadFindingWithRevision>, StoreError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {FINDING_COLUMNS} FROM lead_findings f \
         LEFT JOIN entity_revisions er ON er.client_id = f.client_id \
           AND er.entity_kind = ?3 AND er.entity_id = f.finding_id \
         WHERE f.client_id = ?1 AND f.finding_id = ?2"
    ))?;
    Ok(stmt
        .query_row(
            params![client_id, finding_id, FINDING_ENTITY_KIND],
            finding_from_row,
        )
        .optional()?)
}

pub fn list_findings(
    conn: &Connection,
    client_id: &str,
    status: Option<LeadFindingStatus>,
    limit: usize,
) -> Result<Vec<LeadFindingWithRevision>, StoreError> {
    let mut sql = format!(
        "SELECT {FINDING_COLUMNS} FROM lead_findings f \
         LEFT JOIN entity_revisions er ON er.client_id = f.client_id \
           AND er.entity_kind = ?2 AND er.entity_id = f.finding_id \
         WHERE f.client_id = ?1"
    );
    let status_string = status.map(status_str).map(str::to_string);
    if status_string.is_some() {
        sql.push_str(" AND f.status = ?3");
    }
    sql.push_str(" ORDER BY f.updated_at_ms DESC, f.finding_id DESC LIMIT ?");
    let limit_i64 = limit as i64;
    let mut stmt = conn.prepare(&sql)?;
    let rows = if let Some(status) = status_string.as_deref() {
        stmt.query_map(
            params![client_id, FINDING_ENTITY_KIND, status, limit_i64],
            finding_from_row,
        )?
    } else {
        stmt.query_map(
            params![client_id, FINDING_ENTITY_KIND, limit_i64],
            finding_from_row,
        )?
    };
    let mut findings = Vec::new();
    for row in rows {
        findings.push(row?);
    }
    Ok(findings)
}

pub fn count_findings_created_since(
    conn: &Connection,
    client_id: &str,
    since_ms: u64,
) -> Result<usize, StoreError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM lead_findings \
         WHERE client_id = ?1 AND created_at_ms >= ?2",
        params![client_id, since_ms as i64],
        |row| row.get(0),
    )?;
    Ok(count.max(0) as usize)
}

pub fn count_findings_by_status(
    conn: &Connection,
    client_id: &str,
    status: LeadFindingStatus,
) -> Result<usize, StoreError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM lead_findings \
         WHERE client_id = ?1 AND status = ?2",
        params![client_id, status_str(status)],
        |row| row.get(0),
    )?;
    Ok(count.max(0) as usize)
}

pub fn insert_finding(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    actor_kind: ActorKindDto,
    finding: &LeadFinding,
    idempotency_key: &str,
) -> Result<MutationOutcome, StoreError> {
    let after = serde_json::to_string(finding)
        .map_err(|err| StoreError::Domain(format!("serialize finding: {err}")))?;
    let matched_terms_json = serde_json::to_string(&finding.matched_terms)
        .map_err(|err| StoreError::Domain(format!("serialize matched terms: {err}")))?;
    let evidence_json = serde_json::to_string(&finding.evidence)
        .map_err(|err| StoreError::Domain(format!("serialize evidence: {err}")))?;
    let row = finding.clone();
    let owned_client = client_id.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: FINDING_ENTITY_KIND,
            entity_id: &finding.finding_id,
            change_kind: "stage",
            actor_id,
            actor_kind,
            expected_revision: None,
            idempotency_key,
            correlation_id: Some(&finding.source_id),
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms: finding.created_at_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO lead_findings \
                 (client_id, finding_id, source_id, status, title, summary, contact_hint, \
                  company_hint, matched_terms_json, evidence_json, work_item_id, \
                  created_at_ms, updated_at_ms) \
                 VALUES (?1, ?2, ?3, 'staged', ?4, ?5, ?6, ?7, ?8, ?9, NULL, ?10, ?10)",
                params![
                    owned_client,
                    row.finding_id,
                    row.source_id,
                    row.title,
                    row.summary,
                    row.contact_hint,
                    row.company_hint,
                    matched_terms_json,
                    evidence_json,
                    row.created_at_ms as i64,
                ],
            )?;
            Ok(())
        },
    )
}

pub fn accept_finding(
    conn: &mut Connection,
    ctx: MutationContext<'_>,
    finding_id: &str,
    criteria: &bos_contracts::lead_discovery::LeadDiscoveryCriteria,
) -> Result<MutationOutcome, StoreError> {
    let current = get_finding(conn, ctx.client_id, finding_id)?
        .ok_or_else(|| StoreError::Domain("lead_finding_not_found".to_string()))?
        .finding;
    let owned_client = ctx.client_id.to_string();
    let owned_finding_id = finding_id.to_string();
    let finding_for_item = current.clone();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: FINDING_ENTITY_KIND,
            entity_id: finding_id,
            change_kind: "accept",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: Some(finding_id),
            causation_id: None,
            before_json: Some("{\"status\":\"staged\"}".to_string()),
            after_json: Some("{\"status\":\"accepted\"}".to_string()),
            now_ms: ctx.now_ms,
        },
        move |tx| {
            if finding_for_item.status != LeadFindingStatus::Staged {
                return Err(StoreError::Domain("lead_finding_not_staged".to_string()));
            }
            let work_item_id = emit_work_item_for_finding(
                tx,
                &owned_client,
                &finding_for_item,
                criteria,
                ctx.now_ms,
            )?;
            tx.execute(
                "UPDATE lead_findings SET status = 'accepted', work_item_id = ?3, \
                 updated_at_ms = ?4 WHERE client_id = ?1 AND finding_id = ?2",
                params![
                    owned_client,
                    owned_finding_id,
                    work_item_id,
                    ctx.now_ms as i64,
                ],
            )?;
            Ok(())
        },
    )
}

pub fn reject_finding(
    conn: &mut Connection,
    ctx: MutationContext<'_>,
    finding_id: &str,
) -> Result<MutationOutcome, StoreError> {
    let current = get_finding(conn, ctx.client_id, finding_id)?
        .ok_or_else(|| StoreError::Domain("lead_finding_not_found".to_string()))?
        .finding;
    let owned_client = ctx.client_id.to_string();
    let owned_finding_id = finding_id.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: FINDING_ENTITY_KIND,
            entity_id: finding_id,
            change_kind: "reject",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: Some(finding_id),
            causation_id: None,
            before_json: Some("{\"status\":\"staged\"}".to_string()),
            after_json: Some("{\"status\":\"rejected\"}".to_string()),
            now_ms: ctx.now_ms,
        },
        move |tx| {
            if current.status != LeadFindingStatus::Staged {
                return Err(StoreError::Domain("lead_finding_not_staged".to_string()));
            }
            tx.execute(
                "UPDATE lead_findings SET status = 'rejected', updated_at_ms = ?3 \
                 WHERE client_id = ?1 AND finding_id = ?2",
                params![owned_client, owned_finding_id, ctx.now_ms as i64],
            )?;
            Ok(())
        },
    )
}

fn emit_work_item_for_finding(
    tx: &rusqlite::Transaction<'_>,
    client_id: &str,
    finding: &LeadFinding,
    criteria: &bos_contracts::lead_discovery::LeadDiscoveryCriteria,
    now_ms: u64,
) -> Result<String, StoreError> {
    let item_id = format!(
        "wi_{}_{}",
        super::SOURCE_KIND_LEAD_FINDING,
        finding.finding_id
    );
    let packet_kinds_json = serde_json::to_string(&super::service::routing_packet_kinds(criteria))
        .map_err(|err| StoreError::Domain(format!("serialize packet kinds: {err}")))?;
    tx.execute(
        "INSERT INTO work_items \
         (client_id, item_id, source_kind, source_ref, category_id, title, summary, \
          packet_kinds_json, status, ai_suggested, rationale, produce_guidance, \
          created_at_ms, updated_at_ms, source_user_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'open', 0, '', '', ?9, ?9, NULL)",
        params![
            client_id,
            item_id,
            super::SOURCE_KIND_LEAD_FINDING,
            finding.finding_id,
            super::service::CATEGORY_ID,
            finding.title,
            finding.summary,
            packet_kinds_json,
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

fn status_str(status: LeadFindingStatus) -> &'static str {
    match status {
        LeadFindingStatus::Staged => "staged",
        LeadFindingStatus::Accepted => "accepted",
        LeadFindingStatus::Rejected => "rejected",
    }
}

fn status_from_str(raw: &str) -> LeadFindingStatus {
    match raw {
        "accepted" => LeadFindingStatus::Accepted,
        "rejected" => LeadFindingStatus::Rejected,
        _ => LeadFindingStatus::Staged,
    }
}
