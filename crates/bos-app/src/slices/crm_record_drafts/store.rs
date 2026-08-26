//! CRM record-create draft persistence through store_core. Approval enqueues
//! the provider create-records outbox job inside the SAME mutation
//! transaction. Unlike most draft slices, one work item may have multiple
//! active CRM record drafts so a single note can stage one draft per missing
//! contact. Draft ids and produce in-flight guards resolve duplicate produce
//! races.

use bos_contracts::crm_record_drafts::{
    CrmRecordDraft, CrmRecordDraftStatus, CrmRecordDraftWithRevision,
};
use bos_contracts::receipt::ActorKindDto;
use rusqlite::{params, Connection, Row};

use crate::outbox::{self, NewOutboxJob};
use crate::slices::draft_store::{self, DraftStore, DraftTableSpec};
use crate::store_core::{self, MutationOutcome, MutationRequest, StoreError};

pub const DRAFT_ENTITY_KIND: &str = "crm_record_draft";
const APPROVE_SQL: &str = "UPDATE crm_record_drafts SET status = 'approved', outbox_job_id = ?3, \
     enrichment_trace_json = NULL, updated_at_ms = ?4 \
     WHERE client_id = ?1 AND draft_id = ?2";
const REJECT_SQL: &str = "UPDATE crm_record_drafts SET status = 'rejected', updated_at_ms = ?3 \
     WHERE client_id = ?1 AND draft_id = ?2";
const DRAFT_TABLE: DraftTableSpec = DraftTableSpec {
    table: CrmRecordDraftStore::TABLE,
    entity_kind: DRAFT_ENTITY_KIND,
    not_found_code: CrmRecordDraftStore::NOT_FOUND,
    not_staged_code: "crm_record_draft_not_staged",
    approve_sql: APPROVE_SQL,
    reject_sql: REJECT_SQL,
};

const DRAFT_COLUMNS: &str = "d.draft_id, d.item_id, d.source_kind, d.source_ref, d.status, \
     d.create_company, d.company_name, d.company_website, d.company_phone, d.company_address, \
     d.create_contact, d.contact_first_name, d.contact_last_name, d.contact_email, \
     d.contact_phone, d.contact_title, d.provider_ids_json, d.provenance_json, d.model, \
     d.confidence, d.outbox_job_id, d.created_at_ms, d.updated_at_ms, d.enrichment_trace_json, \
     d.company_description, COALESCE(er.revision, 0) AS revision";

fn draft_from_row(row: &Row<'_>) -> rusqlite::Result<CrmRecordDraftWithRevision> {
    let enrichment_trace = row
        .get::<_, Option<String>>("enrichment_trace_json")?
        .and_then(|raw| {
            serde_json::from_str::<bos_contracts::crm_record_drafts::CrmEnrichmentTrace>(&raw).ok()
        });
    let research_annotations = enrichment_trace
        .as_ref()
        .map(|trace| trace.research_annotations.clone())
        .unwrap_or_default();
    Ok(CrmRecordDraftWithRevision {
        draft: CrmRecordDraft {
            draft_id: row.get("draft_id")?,
            item_id: row.get("item_id")?,
            source_kind: row.get("source_kind")?,
            source_ref: row.get("source_ref")?,
            status: status_from_str(&row.get::<_, String>("status")?),
            create_company: row.get::<_, i64>("create_company")? != 0,
            company_name: row.get("company_name")?,
            company_website: row.get("company_website")?,
            company_phone: row.get("company_phone")?,
            company_address: row.get("company_address")?,
            company_description: row.get("company_description")?,
            create_contact: row.get::<_, i64>("create_contact")? != 0,
            contact_first_name: row.get("contact_first_name")?,
            contact_last_name: row.get("contact_last_name")?,
            contact_email: row.get("contact_email")?,
            contact_phone: row.get("contact_phone")?,
            contact_title: row.get("contact_title")?,
            provider_ids: serde_json::from_str(&row.get::<_, String>("provider_ids_json")?)
                .unwrap_or_default(),
            provenance: serde_json::from_str(&row.get::<_, String>("provenance_json")?)
                .unwrap_or_default(),
            enrichment_trace,
            research_annotations,
            model: row.get("model")?,
            confidence: row.get("confidence")?,
            outbox_job_id: row.get("outbox_job_id")?,
            created_at_ms: row.get::<_, i64>("created_at_ms")? as u64,
            updated_at_ms: row.get::<_, i64>("updated_at_ms")? as u64,
        },
        revision: row.get::<_, i64>("revision")? as u64,
        outbox_job: None,
    })
}

fn attach_job_summary(
    conn: &Connection,
    client_id: &str,
    mut entry: CrmRecordDraftWithRevision,
) -> Result<CrmRecordDraftWithRevision, StoreError> {
    if let Some(job_id) = entry.draft.outbox_job_id.as_deref() {
        entry.outbox_job = outbox::job_summary(conn, client_id, job_id)?;
    }
    Ok(entry)
}

struct CrmRecordDraftStore;

impl DraftStore for CrmRecordDraftStore {
    type WithRevision = CrmRecordDraftWithRevision;

    const TABLE: &'static str = "crm_record_drafts";
    const COLUMNS: &'static str = DRAFT_COLUMNS;
    const ENTITY_KIND: &'static str = DRAFT_ENTITY_KIND;
    const NOT_FOUND: &'static str = "crm_record_draft_not_found";

    fn map_row(row: &Row<'_>) -> rusqlite::Result<Self::WithRevision> {
        draft_from_row(row)
    }

    fn attach(
        conn: &Connection,
        client_id: &str,
        entry: Self::WithRevision,
    ) -> Result<Self::WithRevision, StoreError> {
        attach_job_summary(conn, client_id, entry)
    }
}

pub fn active_draft_for_item(
    conn: &Connection,
    client_id: &str,
    item_id: &str,
) -> Result<Option<CrmRecordDraftWithRevision>, StoreError> {
    draft_store::active_draft_for_item::<CrmRecordDraftStore>(conn, client_id, item_id)
}

pub fn get_draft(
    conn: &Connection,
    client_id: &str,
    draft_id: &str,
) -> Result<Option<CrmRecordDraftWithRevision>, StoreError> {
    draft_store::get_draft_unscoped::<CrmRecordDraftStore>(conn, client_id, draft_id)
}

pub fn list_drafts(
    conn: &Connection,
    client_id: &str,
    item_id: Option<&str>,
    limit: usize,
) -> Result<Vec<CrmRecordDraftWithRevision>, StoreError> {
    draft_store::list_drafts_unscoped::<CrmRecordDraftStore>(conn, client_id, item_id, limit)
}

pub fn count_drafts_for_item(
    conn: &Connection,
    client_id: &str,
    item_id: &str,
) -> Result<u64, StoreError> {
    draft_store::count_drafts_for_item::<CrmRecordDraftStore>(conn, client_id, item_id)
}

/// Item ids with a STAGED draft (operator decision pending). Feeds the queue's
/// "needs you" decoration via the produce spine.
pub fn staged_item_ids(conn: &Connection, client_id: &str) -> Result<Vec<String>, StoreError> {
    draft_store::staged_item_ids::<CrmRecordDraftStore>(conn, client_id)
}

pub fn insert_draft(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    draft: &CrmRecordDraft,
    idempotency_key: &str,
) -> Result<MutationOutcome, StoreError> {
    let after = serde_json::to_string(draft)
        .map_err(|err| StoreError::Domain(format!("serialize draft: {err}")))?;
    let provider_ids_json = serde_json::to_string(&draft.provider_ids)
        .map_err(|err| StoreError::Domain(format!("serialize provider ids: {err}")))?;
    let provenance_json = serde_json::to_string(&draft.provenance)
        .map_err(|err| StoreError::Domain(format!("serialize provenance: {err}")))?;
    let row = draft.clone();
    let owned_client = client_id.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: DRAFT_ENTITY_KIND,
            entity_id: &draft.draft_id,
            change_kind: "stage",
            actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: None,
            idempotency_key,
            correlation_id: Some(&draft.item_id),
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms: draft.created_at_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO crm_record_drafts \
                 (client_id, draft_id, item_id, source_kind, source_ref, status, \
                  create_company, company_name, company_website, company_phone, company_address, \
                  company_description, \
                  create_contact, contact_first_name, contact_last_name, contact_email, \
                  contact_phone, contact_title, provider_ids_json, provenance_json, model, \
                  confidence, created_at_ms, updated_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 'staged', ?6, ?7, ?8, ?9, ?10, ?22, ?11, ?12, ?13, \
                  ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?21)",
                params![
                    owned_client,
                    row.draft_id,
                    row.item_id,
                    row.source_kind,
                    row.source_ref,
                    row.create_company as i64,
                    row.company_name,
                    row.company_website,
                    row.company_phone,
                    row.company_address,
                    row.create_contact as i64,
                    row.contact_first_name,
                    row.contact_last_name,
                    row.contact_email,
                    row.contact_phone,
                    row.contact_title,
                    provider_ids_json,
                    provenance_json,
                    row.model,
                    row.confidence,
                    row.created_at_ms as i64,
                    row.company_description,
                ],
            )
            .map_err(|err| match err {
                rusqlite::Error::SqliteFailure(code, _)
                    if code.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    StoreError::Domain("crm_record_draft_already_active".to_string())
                }
                other => other.into(),
            })?;
            Ok(())
        },
    )
}

pub use crate::slices::mutation_context::MutationContext as DraftActionContext;

/// Approve a staged draft: status flip + outbox enqueue, one transaction.
pub fn approve_draft(
    conn: &mut Connection,
    ctx: DraftActionContext<'_>,
    draft_id: &str,
    job: &NewOutboxJob,
) -> Result<MutationOutcome, StoreError> {
    draft_store::approve(conn, ctx.into(), &DRAFT_TABLE, draft_id, Some(job))
}

pub fn reject_draft(
    conn: &mut Connection,
    ctx: DraftActionContext<'_>,
    draft_id: &str,
) -> Result<MutationOutcome, StoreError> {
    draft_store::reject(conn, ctx.into(), &DRAFT_TABLE, draft_id)
}

/// Validated edit fields for a STAGED draft (the operator tunes the proposed
/// records before approval). Each proposed record must keep a non-empty name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordEdit {
    pub create_company: bool,
    pub company_name: Option<String>,
    pub company_website: Option<String>,
    pub company_phone: Option<String>,
    pub company_address: Option<String>,
    pub company_description: Option<String>,
    pub create_contact: bool,
    pub contact_first_name: Option<String>,
    pub contact_last_name: Option<String>,
    pub contact_email: Option<String>,
    pub contact_phone: Option<String>,
    pub contact_title: Option<String>,
}

/// Edit a STAGED draft's proposed-record set + fields (full replacement,
/// receipted). Approval rebuilds the provider payload from the stored row, so
/// edits flow into the write.
pub fn update_draft(
    conn: &mut Connection,
    ctx: DraftActionContext<'_>,
    draft_id: &str,
    edit: &RecordEdit,
) -> Result<MutationOutcome, StoreError> {
    let current = require_status(conn, ctx.client_id, draft_id)?;
    if current != "staged" {
        return Err(StoreError::Domain(format!(
            "crm_record_draft_not_staged:{current}"
        )));
    }
    let after = serde_json::json!({
        "create_company": edit.create_company,
        "company_name": edit.company_name,
        "create_contact": edit.create_contact,
        "contact_first_name": edit.contact_first_name,
        "contact_last_name": edit.contact_last_name,
    });
    let owned_client = ctx.client_id.to_string();
    let owned_draft = draft_id.to_string();
    let e = edit.clone();
    let now_ms = ctx.now_ms;
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: DRAFT_ENTITY_KIND,
            entity_id: draft_id,
            change_kind: "edit",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(after.to_string()),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE crm_record_drafts SET create_company = ?3, company_name = ?4, \
                 company_website = ?5, company_phone = ?6, company_address = ?7, \
                 create_contact = ?8, contact_first_name = ?9, contact_last_name = ?10, \
                 contact_email = ?11, contact_phone = ?12, contact_title = ?13, \
                 company_description = ?15, updated_at_ms = ?14 \
                 WHERE client_id = ?1 AND draft_id = ?2",
                params![
                    owned_client,
                    owned_draft,
                    e.create_company as i64,
                    e.company_name,
                    e.company_website,
                    e.company_phone,
                    e.company_address,
                    e.create_contact as i64,
                    e.contact_first_name,
                    e.contact_last_name,
                    e.contact_email,
                    e.contact_phone,
                    e.contact_title,
                    now_ms as i64,
                    e.company_description,
                ],
            )?;
            Ok(())
        },
    )
}

/// One enrichment-sourced value plus the provenance quote backing it (a
/// `page:<url>` marker for deterministic fields, a literal page quote for the
/// LLM gap-filler).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrichedValue {
    pub value: String,
    pub provenance_quote: String,
}

/// The website-enrichment values to graft onto a staged draft. Each field is
/// applied ONLY when the draft's column is still NULL — the note-fill and any
/// operator edit always win (enrichment is a gap-filler, never an overwrite).
#[derive(Debug, Clone, Default)]
pub struct WebEnrichmentApply {
    pub company_name: Option<EnrichedValue>,
    pub company_website: Option<EnrichedValue>,
    pub company_phone: Option<EnrichedValue>,
    pub company_address: Option<EnrichedValue>,
    pub company_description: Option<EnrichedValue>,
    pub contact_email: Option<EnrichedValue>,
    pub contact_phone: Option<EnrichedValue>,
    pub contact_title: Option<EnrichedValue>,
}

impl WebEnrichmentApply {
    pub fn is_empty(&self) -> bool {
        self.company_name.is_none()
            && self.company_website.is_none()
            && self.company_phone.is_none()
            && self.company_address.is_none()
            && self.company_description.is_none()
            && self.contact_email.is_none()
            && self.contact_phone.is_none()
            && self.contact_title.is_none()
    }
}

/// Graft website-enrichment values onto a STAGED draft, filling only NULL
/// columns (COALESCE keeps the note-fill / operator edits) and appending a
/// provenance entry for each column actually filled. Also stores the
/// operator-facing enrichment `trace` (what the crawl read + fed the model),
/// which persists even when no field was filled. Receipted like every mutation.
pub fn apply_web_enrichment(
    conn: &mut Connection,
    ctx: DraftActionContext<'_>,
    draft_id: &str,
    apply: &WebEnrichmentApply,
    trace: Option<&bos_contracts::crm_record_drafts::CrmEnrichmentTrace>,
) -> Result<MutationOutcome, StoreError> {
    let current = require_status(conn, ctx.client_id, draft_id)?;
    if current != "staged" {
        return Err(StoreError::Domain(format!(
            "crm_record_draft_not_staged:{current}"
        )));
    }
    let owned_client = ctx.client_id.to_string();
    let owned_draft = draft_id.to_string();
    let apply = apply.clone();
    let trace = trace.cloned();
    let now_ms = ctx.now_ms;
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: DRAFT_ENTITY_KIND,
            entity_id: draft_id,
            change_kind: "enrich",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::System,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some("{\"web_enrichment\":true}".to_string()),
            now_ms,
        },
        move |tx| {
            // Read current values + provenance so we only fill genuine gaps and
            // never duplicate a provenance entry for a field already grounded.
            struct Current {
                website: Option<String>,
                name: Option<String>,
                phone: Option<String>,
                address: Option<String>,
                description: Option<String>,
                c_email: Option<String>,
                c_phone: Option<String>,
                c_title: Option<String>,
                provenance_json: String,
            }
            let Current {
                website,
                name,
                phone,
                address,
                description,
                c_email,
                c_phone,
                c_title,
                provenance_json,
            } = tx.query_row(
                "SELECT company_website, company_name, company_phone, company_address, company_description, \
                 contact_email, contact_phone, contact_title, provenance_json \
                 FROM crm_record_drafts WHERE client_id = ?1 AND draft_id = ?2",
                params![owned_client, owned_draft],
                |row| {
                    Ok(Current {
                        website: row.get(0)?,
                        name: row.get(1)?,
                        phone: row.get(2)?,
                        address: row.get(3)?,
                        description: row.get(4)?,
                        c_email: row.get(5)?,
                        c_phone: row.get(6)?,
                        c_title: row.get(7)?,
                        provenance_json: row.get(8)?,
                    })
                },
            )?;

            let mut provenance: Vec<bos_contracts::calendar_drafts::DraftFieldProvenance> =
                serde_json::from_str(&provenance_json).unwrap_or_default();
            let mut applied_fields = std::collections::BTreeSet::<String>::new();

            let name = if let Some(incoming) = &apply.company_name {
                if crate::produce::draft_field_policy::may_replace_weak_company_name(
                    name.as_deref(),
                    &incoming.value,
                    &provenance,
                ) {
                    applied_fields.insert("company_name".to_string());
                    provenance.retain(|p| p.field != "company_name");
                    provenance.push(bos_contracts::calendar_drafts::DraftFieldProvenance {
                        field: "company_name".to_string(),
                        quote: incoming.provenance_quote.clone(),
                    });
                    Some(incoming.value.clone())
                } else {
                    name
                }
            } else {
                name
            };

            // (column current value, incoming enrichment, provenance field id)
            let mut fill = |current: &Option<String>,
                            incoming: &Option<EnrichedValue>,
                            field: &str|
             -> Option<String> {
                match (current, incoming) {
                    (None, Some(value)) => {
                        applied_fields.insert(field.to_string());
                        if !provenance.iter().any(|p| p.field == field) {
                            provenance.push(bos_contracts::calendar_drafts::DraftFieldProvenance {
                                field: field.to_string(),
                                quote: value.provenance_quote.clone(),
                            });
                        }
                        Some(value.value.clone())
                    }
                    _ => current.clone(),
                }
            };

            let website = fill(&website, &apply.company_website, "company_website");
            let phone = fill(&phone, &apply.company_phone, "company_phone");
            let address = fill(&address, &apply.company_address, "company_address");
            let description = fill(
                &description,
                &apply.company_description,
                "company_description",
            );
            let c_email = fill(&c_email, &apply.contact_email, "contact_email");
            let c_phone = fill(&c_phone, &apply.contact_phone, "contact_phone");
            let c_title = fill(&c_title, &apply.contact_title, "contact_title");

            let provenance_json =
                serde_json::to_string(&provenance).unwrap_or_else(|_| provenance_json.clone());
            let trace_json = trace
                .as_ref()
                .map(|trace| filtered_enrichment_trace_json(trace, &applied_fields))
                .transpose()?;

            tx.execute(
                "UPDATE crm_record_drafts SET company_website = ?3, company_name = ?4, \
                 company_phone = ?5, company_address = ?6, contact_email = ?7, contact_phone = ?8, \
                 contact_title = ?9, provenance_json = ?10, enrichment_trace_json = ?11, \
                 company_description = ?13, updated_at_ms = ?12 \
                 WHERE client_id = ?1 AND draft_id = ?2",
                params![
                    owned_client,
                    owned_draft,
                    website,
                    name,
                    phone,
                    address,
                    c_email,
                    c_phone,
                    c_title,
                    provenance_json,
                    trace_json,
                    now_ms as i64,
                    description,
                ],
            )?;
            Ok(())
        },
    )
}

fn filtered_enrichment_trace_json(
    trace: &bos_contracts::crm_record_drafts::CrmEnrichmentTrace,
    applied_fields: &std::collections::BTreeSet<String>,
) -> Result<String, StoreError> {
    let mut trace = trace.clone();
    trace
        .items
        .retain(|item| applied_fields.contains(&item.field));
    trace
        .research_annotations
        .retain(|annotation| applied_fields.contains(&annotation.field_id));
    serde_json::to_string(&trace)
        .map_err(|err| StoreError::Domain(format!("crm_record_enrichment_trace_invalid:{err}")))
}

fn require_status(
    conn: &Connection,
    client_id: &str,
    draft_id: &str,
) -> Result<String, StoreError> {
    draft_store::require_status_unscoped::<CrmRecordDraftStore>(conn, client_id, draft_id)
}

fn status_from_str(raw: &str) -> CrmRecordDraftStatus {
    match raw {
        "approved" => CrmRecordDraftStatus::Approved,
        "rejected" => CrmRecordDraftStatus::Rejected,
        _ => CrmRecordDraftStatus::Staged,
    }
}
