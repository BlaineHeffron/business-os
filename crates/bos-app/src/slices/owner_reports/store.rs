//! Owner-report persistence: deterministic report ids (owr_<kind>_<start>)
//! make generation an upsert — replaying a period refreshes the row, never
//! duplicates it. Emailing the digest flips outbox_job_id and enqueues the
//! gated Gmail-draft job in ONE receipted transaction (claim_drafts'
//! approval shape, minus the follow-up task).

use bos_contracts::owner_reports::{
    OwnerDigestMetrics, OwnerReport, OwnerReportPeriodKind, OwnerReportStatus,
    OwnerReportWithRevision,
};
use bos_contracts::receipt::ActorKindDto;
use rusqlite::{params, Connection, OptionalExtension, Row};
use sha2::{Digest, Sha256};

use crate::outbox::{self, NewOutboxJob};
use crate::store_core::{self, MutationOutcome, MutationRequest, StoreError};

pub const REPORT_ENTITY_KIND: &str = "owner_report";
pub const PUMP_ACTOR: &str = "owner_digest_pump";

const REPORT_COLUMNS: &str = "r.report_id, r.period_kind, r.period_start, r.period_end, \
     r.as_of_date, r.status, r.metrics_json, r.headline, r.narrative, r.callouts_json, \
     r.confidence, r.model, r.narration_error, r.outbox_job_id, r.generated_at_ms, \
     r.created_at_ms, r.updated_at_ms, COALESCE(er.revision, 0)";

pub fn period_kind_str(kind: OwnerReportPeriodKind) -> &'static str {
    match kind {
        OwnerReportPeriodKind::Weekly => "weekly",
        OwnerReportPeriodKind::Mtd => "mtd",
    }
}

fn period_kind_from_str(raw: &str) -> OwnerReportPeriodKind {
    match raw {
        "mtd" => OwnerReportPeriodKind::Mtd,
        _ => OwnerReportPeriodKind::Weekly,
    }
}

fn status_str(status: OwnerReportStatus) -> &'static str {
    match status {
        OwnerReportStatus::Complete => "complete",
        OwnerReportStatus::NarrationFailed => "narration_failed",
    }
}

fn status_from_str(raw: &str) -> OwnerReportStatus {
    match raw {
        "narration_failed" => OwnerReportStatus::NarrationFailed,
        _ => OwnerReportStatus::Complete,
    }
}

fn report_from_row(row: &Row<'_>) -> rusqlite::Result<OwnerReportWithRevision> {
    let metrics: OwnerDigestMetrics =
        serde_json::from_str(&row.get::<_, String>(6)?).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, Box::new(err))
        })?;
    Ok(OwnerReportWithRevision {
        report: OwnerReport {
            report_id: row.get(0)?,
            period_kind: period_kind_from_str(&row.get::<_, String>(1)?),
            period_start: row.get(2)?,
            period_end: row.get(3)?,
            as_of_date: row.get(4)?,
            status: status_from_str(&row.get::<_, String>(5)?),
            metrics,
            headline: row.get(7)?,
            narrative: row.get(8)?,
            callouts: serde_json::from_str(&row.get::<_, String>(9)?).unwrap_or_default(),
            confidence: row.get(10)?,
            model: row.get(11)?,
            narration_error: row.get(12)?,
            outbox_job_id: row.get(13)?,
            generated_at_ms: row.get::<_, i64>(14)? as u64,
            created_at_ms: row.get::<_, i64>(15)? as u64,
            updated_at_ms: row.get::<_, i64>(16)? as u64,
        },
        revision: row.get::<_, i64>(17)? as u64,
        outbox_job: None,
    })
}

fn attach_job_summary(
    conn: &Connection,
    client_id: &str,
    mut entry: OwnerReportWithRevision,
) -> Result<OwnerReportWithRevision, StoreError> {
    if let Some(job_id) = entry.report.outbox_job_id.as_deref() {
        entry.outbox_job = outbox::job_summary(conn, client_id, job_id)?;
    }
    Ok(entry)
}

pub fn get_report(
    conn: &Connection,
    client_id: &str,
    report_id: &str,
) -> Result<Option<OwnerReportWithRevision>, StoreError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {REPORT_COLUMNS} FROM owner_reports r \
         LEFT JOIN entity_revisions er \
           ON er.client_id = r.client_id AND er.entity_kind = ?2 AND er.entity_id = r.report_id \
         WHERE r.client_id = ?1 AND r.report_id = ?3"
    ))?;
    let row = stmt
        .query_row(
            params![client_id, REPORT_ENTITY_KIND, report_id],
            report_from_row,
        )
        .optional()?;
    row.map(|entry| attach_job_summary(conn, client_id, entry))
        .transpose()
}

pub fn list_reports(
    conn: &Connection,
    client_id: &str,
    period_kind: Option<&str>,
    limit: usize,
) -> Result<Vec<OwnerReportWithRevision>, StoreError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {REPORT_COLUMNS} FROM owner_reports r \
         LEFT JOIN entity_revisions er \
           ON er.client_id = r.client_id AND er.entity_kind = ?2 AND er.entity_id = r.report_id \
         WHERE r.client_id = ?1 AND (?3 IS NULL OR r.period_kind = ?3) \
         ORDER BY r.period_start DESC, r.period_kind ASC LIMIT ?4"
    ))?;
    let rows = stmt.query_map(
        params![client_id, REPORT_ENTITY_KIND, period_kind, limit as i64],
        report_from_row,
    )?;
    let mut reports = Vec::new();
    for row in rows {
        reports.push(attach_job_summary(conn, client_id, row?)?);
    }
    Ok(reports)
}

/// The as-of date a period's row was last assembled for (the pump's
/// staleness check) — None when never generated.
pub fn report_as_of(
    conn: &Connection,
    client_id: &str,
    report_id: &str,
) -> Result<Option<String>, StoreError> {
    let row = conn
        .query_row(
            "SELECT as_of_date FROM owner_reports WHERE client_id = ?1 AND report_id = ?2",
            params![client_id, report_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(row)
}

/// Insert-or-refresh one report row. Deterministic ids make this idempotent
/// per (period, content); a regenerate resets the email association (the
/// fresh digest has not been emailed — the prior outbox job stays in the
/// audit trail).
pub fn upsert_report(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    report: &OwnerReport,
) -> Result<MutationOutcome, StoreError> {
    let metrics_json = serde_json::to_string(&report.metrics)
        .map_err(|err| StoreError::Domain(format!("serialize metrics: {err}")))?;
    let callouts_json = serde_json::to_string(&report.callouts)
        .map_err(|err| StoreError::Domain(format!("serialize callouts: {err}")))?;
    let content_hash = {
        let mut hasher = Sha256::new();
        for part in [
            metrics_json.as_str(),
            report.headline.as_deref().unwrap_or(""),
            report.narrative.as_deref().unwrap_or(""),
            callouts_json.as_str(),
            report.as_of_date.as_str(),
        ] {
            hasher.update(part.as_bytes());
            hasher.update([0u8]);
        }
        let digest = hasher.finalize();
        digest[..8]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    let idempotency_key = format!("owner_report:{}:{content_hash}", report.report_id);
    let after = serde_json::json!({
        "period_kind": period_kind_str(report.period_kind),
        "period_start": report.period_start,
        "as_of_date": report.as_of_date,
        "status": status_str(report.status),
        "headline": report.headline,
    })
    .to_string();
    let owned_client = client_id.to_string();
    let owned = report.clone();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: REPORT_ENTITY_KIND,
            entity_id: &report.report_id,
            change_kind: "generate",
            actor_id,
            actor_kind: ActorKindDto::System,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms: report.generated_at_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO owner_reports \
                 (client_id, report_id, period_kind, period_start, period_end, as_of_date, \
                  status, metrics_json, headline, narrative, callouts_json, confidence, model, \
                  narration_error, outbox_job_id, generated_at_ms, created_at_ms, updated_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, NULL, \
                         ?15, ?15, ?15) \
                 ON CONFLICT (client_id, report_id) DO UPDATE SET \
                   period_end = excluded.period_end, \
                   as_of_date = excluded.as_of_date, \
                   status = excluded.status, \
                   metrics_json = excluded.metrics_json, \
                   headline = excluded.headline, \
                   narrative = excluded.narrative, \
                   callouts_json = excluded.callouts_json, \
                   confidence = excluded.confidence, \
                   model = excluded.model, \
                   narration_error = excluded.narration_error, \
                   outbox_job_id = NULL, \
                   generated_at_ms = excluded.generated_at_ms, \
                   updated_at_ms = excluded.updated_at_ms",
                params![
                    owned_client,
                    owned.report_id,
                    period_kind_str(owned.period_kind),
                    owned.period_start,
                    owned.period_end,
                    owned.as_of_date,
                    status_str(owned.status),
                    metrics_json,
                    owned.headline,
                    owned.narrative,
                    callouts_json,
                    owned.confidence,
                    owned.model,
                    owned.narration_error,
                    owned.generated_at_ms as i64,
                ],
            )?;
            Ok(())
        },
    )
}

pub struct EmailActionContext<'a> {
    pub client_id: &'a str,
    pub actor_id: &'a str,
    pub actor_kind: ActorKindDto,
    pub expected_revision: Option<u64>,
    pub idempotency_key: &'a str,
    pub now_ms: u64,
}

pub fn email_job_count_since(
    conn: &Connection,
    client_id: &str,
    report_id: &str,
    since_ms: u64,
) -> Result<u64, StoreError> {
    let count = conn.query_row(
        "SELECT COUNT(*) FROM outbox_jobs \
         WHERE client_id = ?1 AND source_entity_kind = ?2 AND source_entity_id = ?3 \
           AND provider = ?4 AND capability = ?5 AND created_at_ms >= ?6",
        params![
            client_id,
            REPORT_ENTITY_KIND,
            report_id,
            crate::slices::email_drafts::service::PROVIDER_GMAIL,
            crate::slices::email_drafts::service::CAPABILITY_CREATE_DRAFT,
            since_ms as i64
        ],
        |row| row.get::<_, i64>(0),
    )? as u64;
    Ok(count)
}

/// Stage the digest email: record the outbox job on the row and enqueue the
/// gated Gmail-draft job, ONE transaction. A report whose current generation
/// already staged an email refuses (regenerate first for a fresh send).
pub fn stage_email(
    conn: &mut Connection,
    ctx: EmailActionContext<'_>,
    report_id: &str,
    job: &NewOutboxJob,
) -> Result<MutationOutcome, StoreError> {
    let owned_client = ctx.client_id.to_string();
    let owned_report = report_id.to_string();
    let owned_job = job.clone();
    let now_ms = ctx.now_ms;
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: REPORT_ENTITY_KIND,
            entity_id: report_id,
            change_kind: "email",
            actor_id: ctx.actor_id,
            actor_kind: ctx.actor_kind,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: Some(&job.job_id),
            causation_id: None,
            before_json: Some("{\"outbox_job_id\":null}".to_string()),
            after_json: Some(format!("{{\"outbox_job_id\":\"{}\"}}", job.job_id)),
            now_ms,
        },
        move |tx| {
            let updated = tx.execute(
                "UPDATE owner_reports SET outbox_job_id = ?3, updated_at_ms = ?4 \
                 WHERE client_id = ?1 AND report_id = ?2 AND outbox_job_id IS NULL",
                params![owned_client, owned_report, owned_job.job_id, now_ms as i64],
            )?;
            if updated != 1 {
                let exists: Option<String> = tx
                    .query_row(
                        "SELECT outbox_job_id FROM owner_reports \
                         WHERE client_id = ?1 AND report_id = ?2",
                        params![owned_client, owned_report],
                        |row| row.get(0),
                    )
                    .optional()?;
                return Err(StoreError::Domain(if exists.is_some() {
                    "owner_report_email_already_staged".to_string()
                } else {
                    "owner_report_not_found".to_string()
                }));
            }
            outbox::enqueue_within(tx, &owned_client, &owned_job, now_ms)?;
            Ok(())
        },
    )
}
