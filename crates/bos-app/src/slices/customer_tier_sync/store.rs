//! Customer tier sync persistence through store_core. Staging stores the
//! reviewed plan; approval flips status and enqueues the Shopify outbox job in
//! the same receipted transaction.

use bos_contracts::customer_tier_sync::{
    CustomerTierSyncPlan, CustomerTierSyncRun, CustomerTierSyncStatus,
};
use bos_contracts::receipt::ActorKindDto;
use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::outbox::{self, NewOutboxJob};
use crate::slices::mutation_context::MutationContext;
use crate::store_core::{self, MutationOutcome, MutationRequest, StoreError};

pub const RUN_ENTITY_KIND: &str = "customer_tier_sync_run";

fn status_str(status: &CustomerTierSyncStatus) -> &'static str {
    match status {
        CustomerTierSyncStatus::Staged => "staged",
        CustomerTierSyncStatus::Approved => "approved",
        CustomerTierSyncStatus::Rejected => "rejected",
    }
}

fn status_from_str(raw: &str) -> CustomerTierSyncStatus {
    match raw {
        "approved" => CustomerTierSyncStatus::Approved,
        "rejected" => CustomerTierSyncStatus::Rejected,
        _ => CustomerTierSyncStatus::Staged,
    }
}

fn run_from_row(row: &Row<'_>) -> rusqlite::Result<CustomerTierSyncRun> {
    let plan_json: String = row.get("plan_json")?;
    Ok(CustomerTierSyncRun {
        run_id: row.get("run_id")?,
        status: status_from_str(&row.get::<_, String>("status")?),
        revision: row.get::<_, i64>("revision")? as u64,
        plan: serde_json::from_str(&plan_json).unwrap_or(CustomerTierSyncPlan {
            source_provider: "qbo".to_string(),
            target_provider: "shopify".to_string(),
            mapping_version: "invalid".to_string(),
            actions: vec![],
            skipped: vec![],
        }),
        outbox_job_id: row.get("outbox_job_id")?,
        outbox_job: None,
        created_at_ms: row.get::<_, i64>("created_at_ms")? as u64,
        updated_at_ms: row.get::<_, i64>("updated_at_ms")? as u64,
    })
}

fn attach_outbox(
    conn: &Connection,
    client_id: &str,
    mut run: CustomerTierSyncRun,
) -> Result<CustomerTierSyncRun, StoreError> {
    if let Some(job_id) = run.outbox_job_id.as_deref() {
        run.outbox_job = outbox::job_summary(conn, client_id, job_id)?;
    }
    Ok(run)
}

pub fn list_runs(
    conn: &Connection,
    client_id: &str,
    limit: usize,
) -> Result<Vec<CustomerTierSyncRun>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT r.run_id, r.status, r.plan_json, r.outbox_job_id, r.created_at_ms, \
         r.updated_at_ms, COALESCE(er.revision, 0) AS revision \
         FROM customer_tier_sync_runs r \
         LEFT JOIN entity_revisions er \
           ON er.client_id = r.client_id AND er.entity_kind = ?2 AND er.entity_id = r.run_id \
         WHERE r.client_id = ?1 \
         ORDER BY r.created_at_ms DESC, r.run_id DESC LIMIT ?3",
    )?;
    let rows = stmt.query_map(
        params![client_id, RUN_ENTITY_KIND, limit as i64],
        run_from_row,
    )?;
    let mut runs = Vec::new();
    for row in rows {
        runs.push(attach_outbox(conn, client_id, row?)?);
    }
    Ok(runs)
}

pub fn get_run(
    conn: &Connection,
    client_id: &str,
    run_id: &str,
) -> Result<Option<CustomerTierSyncRun>, StoreError> {
    let run = conn
        .query_row(
            "SELECT r.run_id, r.status, r.plan_json, r.outbox_job_id, r.created_at_ms, \
             r.updated_at_ms, COALESCE(er.revision, 0) AS revision \
             FROM customer_tier_sync_runs r \
             LEFT JOIN entity_revisions er \
               ON er.client_id = r.client_id AND er.entity_kind = ?3 AND er.entity_id = r.run_id \
             WHERE r.client_id = ?1 AND r.run_id = ?2",
            params![client_id, run_id, RUN_ENTITY_KIND],
            run_from_row,
        )
        .optional()?;
    run.map(|run| attach_outbox(conn, client_id, run))
        .transpose()
}

pub fn stage_run(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    run_id: &str,
    plan: &CustomerTierSyncPlan,
    idempotency_key: &str,
    now_ms: u64,
) -> Result<MutationOutcome, StoreError> {
    let plan_json = serde_json::to_string(plan)
        .map_err(|err| StoreError::Domain(format!("serialize tier sync plan: {err}")))?;
    let after = serde_json::json!({
        "status": "staged",
        "action_count": plan.actions.len(),
        "skipped_count": plan.skipped.len(),
        "mapping_version": plan.mapping_version,
    })
    .to_string();
    let owned_client = client_id.to_string();
    let owned_run = run_id.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: RUN_ENTITY_KIND,
            entity_id: run_id,
            change_kind: "stage",
            actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: None,
            idempotency_key,
            correlation_id: Some(run_id),
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO customer_tier_sync_runs \
                 (client_id, run_id, status, plan_json, created_at_ms, updated_at_ms) \
                 VALUES (?1, ?2, 'staged', ?3, ?4, ?4)",
                params![owned_client, owned_run, plan_json, now_ms as i64],
            )?;
            Ok(())
        },
    )
}

pub fn approve_run(
    conn: &mut Connection,
    ctx: MutationContext<'_>,
    run_id: &str,
    job: &NewOutboxJob,
) -> Result<MutationOutcome, StoreError> {
    let current = get_run(conn, ctx.client_id, run_id)?
        .ok_or_else(|| StoreError::Domain("customer_tier_sync_run_not_found".to_string()))?;
    if current.status != CustomerTierSyncStatus::Staged {
        return Err(StoreError::Domain(
            "customer_tier_sync_run_not_staged".to_string(),
        ));
    }
    let after = serde_json::json!({
        "status": "approved",
        "outbox_job_id": job.job_id,
        "action_count": current.plan.actions.len(),
    })
    .to_string();
    let owned_client = ctx.client_id.to_string();
    let owned_run = run_id.to_string();
    let owned_job = job.clone();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: RUN_ENTITY_KIND,
            entity_id: run_id,
            change_kind: "approve",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: Some(run_id),
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms: ctx.now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE customer_tier_sync_runs \
                 SET status = 'approved', outbox_job_id = ?3, updated_at_ms = ?4 \
                 WHERE client_id = ?1 AND run_id = ?2",
                params![owned_client, owned_run, owned_job.job_id, ctx.now_ms as i64],
            )?;
            outbox::enqueue_within(tx, &owned_client, &owned_job, ctx.now_ms)?;
            Ok(())
        },
    )
}

pub fn reject_run(
    conn: &mut Connection,
    ctx: MutationContext<'_>,
    run_id: &str,
) -> Result<MutationOutcome, StoreError> {
    let current = get_run(conn, ctx.client_id, run_id)?
        .ok_or_else(|| StoreError::Domain("customer_tier_sync_run_not_found".to_string()))?;
    if current.status != CustomerTierSyncStatus::Staged {
        return Err(StoreError::Domain(
            "customer_tier_sync_run_not_staged".to_string(),
        ));
    }
    let owned_client = ctx.client_id.to_string();
    let owned_run = run_id.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: RUN_ENTITY_KIND,
            entity_id: run_id,
            change_kind: "reject",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: Some(run_id),
            causation_id: None,
            before_json: None,
            after_json: Some(
                serde_json::json!({ "status": status_str(&CustomerTierSyncStatus::Rejected) })
                    .to_string(),
            ),
            now_ms: ctx.now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE customer_tier_sync_runs \
                 SET status = 'rejected', updated_at_ms = ?3 \
                 WHERE client_id = ?1 AND run_id = ?2",
                params![owned_client, owned_run, ctx.now_ms as i64],
            )?;
            Ok(())
        },
    )
}
