//! Produce + approval + delivery logic for CRM record-create drafts (the
//! `crm_record_create` packet kind).
//!
//! A note that references a company and/or people who are not yet in the CRM
//! becomes one or more drafts proposing the MISSING records. The produce stage
//! extracts the records with a bounded typed fill — names are GROUNDED (a record
//! whose name has no literal provenance quote is dropped, never invented) —
//! then runs a bounded LIVE CRM search against the configured provider to decide
//! which records already exist. Only the missing ones are proposed for creation;
//! a matched company is still carried so the approval's ensure-chain can link
//! the new contact to it.
//!
//! Approval enqueues a create-records outbox job whose executor runs the
//! deterministic ensure-chain behind the configured provider's write gate:
//! EspoCRM account → contact, or HubSpot company → contact + default
//! association.

use bos_contracts::calendar_drafts::DraftFieldProvenance;
use bos_contracts::crm_record_drafts::{
    CrmRecordDraft, CrmRecordDraftStatus, CrmRecordProviderIds, CrmResearchFieldAnnotation,
};
use bos_contracts::email_triage::InboundMessageRecord;
use bos_contracts::enrichment::{
    EnrichmentConfidence, EnrichmentEligibility, EnrichmentFieldProposal, EnrichmentMode,
    EnrichmentPlan, EnrichmentRunStatus, EnrichmentSeedEvidence, EnrichmentTier,
    EnrichmentTierEvent,
};
use bos_contracts::work_queue::WorkItem;
use bos_integrations::espocrm::{
    espocrm_records_execution_client, espocrm_records_search_client, EspoCrmApprovalMetadata,
    EspoCrmCompanyInput, EspoCrmContactInput, EspoCrmRecordsCreateOutboxPayload,
    EspoCrmWriteConfig, EspoCrmWriteError,
};
use bos_integrations::llm_typed_tasks::{
    TypedLlmAuthority, TypedLlmExecutionPolicy, TypedLlmExecutionRoute, TypedLlmFallbackPolicy,
    TypedLlmProviderPolicy, TypedLlmRawOutputRetention, TypedLlmRedactionPolicy,
    TypedLlmResponseFormat, TypedLlmRetryPolicy, TypedLlmSafetyPolicy, TypedLlmSourceEntity,
    TypedLlmTaskCapabilities, TypedLlmTaskClass, TypedLlmTaskInput, TypedLlmTaskRequest,
    TypedLlmTaskSpec, TypedLlmTextBlock,
};
use serde_json::json;

use super::store::RecordEdit;
use crate::env_registry;
use crate::outbox::{
    provider_error_detail, retry_backoff_ms, AttemptOutcome, ClaimedJob, NewOutboxJob,
};
use crate::slices::async_kickoff::{
    KickoffCapacity, KickoffDecision, KickoffSpec, RecordedKickoff,
};
use crate::slices::crm_drafts::service::{PROVIDER_ESPOCRM, PROVIDER_HUBSPOT};
use crate::slices::enrichment::service as enrichment_engine;
use crate::store_core::{MutationOutcome, StoreError};
use bos_integrations::hubspot::{
    hubspot_records_execution_client, HubSpotApprovalMetadata, HubSpotCompanyInput,
    HubSpotContactInput, HubSpotRecordsCreateOutboxPayload, HubSpotWriteConfig, HubSpotWriteError,
};
use bos_integrations::web_page_read::{ReqwestWebHttpClient, SystemHostResolver};
use bos_integrations::web_search_enrichment::{ReqwestWebSearchApi, WebSearchCollector};
use std::sync::Arc;

pub const PACKET_KIND: &str = "crm_record_create";
pub const FILL_SCHEMA_REF: &str = "bos.crm_record_drafts.record_fill.v1";
pub const FILL_PURPOSE: &str = "crm_record_fill";
pub const CAPABILITY_CREATE_RECORDS: &str = "create_records";

/// Website-enrichment gap-filler: a SECOND bounded transform over the stripped
/// page text, run only for record fields still missing after the deterministic
/// extraction pass. Registered in the output-schema registry.
pub const ENRICH_SCHEMA_REF: &str = "bos.crm_record_drafts.web_enrichment.v1";
pub const ENRICH_PURPOSE: &str = "crm_web_enrichment";
/// System actor stamped on the enrichment graft (a read-derived prefill).
pub const WEB_ENRICHMENT_ACTOR: &str = "crm_web_enrichment";
pub const SEARCH_ENRICH_REASON_WEAK_COMPANY_NAME: &str = "weak_domain_company_name";
/// Per-page stripped-text budget handed to the gap-filler.
const ENRICH_MAX_TEXT_CHARS: usize = 8_000;

pub fn build_record_fill_request(
    client_id: &str,
    item: &WorkItem,
    message: &InboundMessageRecord,
    context: &serde_json::Value,
    attempt: u64,
) -> TypedLlmTaskRequest {
    let task_id = format!("crm_record_fill_{}_{attempt}", item.item_id);
    let mut request = TypedLlmTaskRequest {
        task_id: task_id.clone(),
        correlation_id: item.item_id.clone(),
        idempotency_key: task_id,
        tenant_or_project_scope: client_id.to_string(),
        source_entity: Some(TypedLlmSourceEntity {
            entity_kind: "email_inbound_message".to_string(),
            entity_id: message.message_id.clone(),
        }),
        spec: TypedLlmTaskSpec {
            task_class: TypedLlmTaskClass::Extract,
            prompt_template_id: "crm_record_fill".to_string(),
            prompt_template_version: "1".to_string(),
            prompt_template_hash: String::new(),
            schema_ref: FILL_SCHEMA_REF.to_string(),
            response_format: TypedLlmResponseFormat::JsonObject,
            max_input_bytes: 64 * 1024,
            max_output_bytes: 4 * 1024,
            max_tokens: 0, // filled from runtime config
            timeout_ms: 0, // filled from runtime config
            capabilities: TypedLlmTaskCapabilities::pure_transformation(),
            authority: TypedLlmAuthority::no_side_effects(),
        },
        input: TypedLlmTaskInput {
            json: json!({
                "instructions": "Extract the CRM records this note references so they can be created if missing. Respond with a single JSON object with EXACTLY these fields: company_name (the organization's name when the note names one, else null), company_website (the company's homepage/base domain when a URL/domain is stated; strip deep paths like /about or /contact), contacts (array of people, each {first_name,last_name,email,phone,title,quote}; empty array when no named person is stated), confidence (\"high\" | \"medium\" | \"low\"), provenance (array of {field, quote} where quote is the LITERAL text span the value came from). GROUNDING: every NAME you return MUST be written in a literal quote — company_name needs a {field:\"company_name\"} entry, and each contact needs quote containing that person's name. NEVER invent a name that is not written in the note; omit it instead. For backward compatibility you may also include the legacy single-contact fields contact_first_name/contact_last_name/contact_email/contact_phone/contact_title, but contacts[] is authoritative when present.",
                "current_category": item.category_id,
                "source_kind": item.source_kind,
            }),
            text_blocks: vec![TypedLlmTextBlock {
                block_id: "source".to_string(),
                text: format!(
                    "From: {}\nSubject: {}\n{}\n\n{}",
                    message.from_addr.as_deref().unwrap_or("(unknown)"),
                    message.subject.as_deref().unwrap_or("(no subject)"),
                    crate::slices::datetime_input::email_prompt_datetime_context(message),
                    crate::slices::email_triage::service::body_for_ai(message)
                ),
            }],
        },
        execution_policy: TypedLlmExecutionPolicy {
            default_route: TypedLlmExecutionRoute::Harness, // realigned by the router
            fallback_policy: TypedLlmFallbackPolicy::NoFallback,
            retry_policy: TypedLlmRetryPolicy {
                max_attempts: 2,
                backoff_ms: 1_000,
                max_elapsed_ms: 240_000,
            },
        },
        provider_policy: TypedLlmProviderPolicy {
            preferred_provider: String::new(),
            preferred_model: String::new(),
            fallback_provider: None,
            fallback_model: None,
        },
        safety_policy: TypedLlmSafetyPolicy {
            redaction_policy: TypedLlmRedactionPolicy::PreSubmit,
            raw_output_retention: TypedLlmRawOutputRetention::None,
        },
    };
    if let Some(text) = context
        .get("grounding_text")
        .and_then(serde_json::Value::as_str)
    {
        if !text.trim().is_empty() {
            request.input.text_blocks.push(TypedLlmTextBlock {
                block_id: "grounding".to_string(),
                text: text.to_string(),
            });
        }
    }
    request
}

const PROVENANCE_FIELDS: &[&str] = &[
    "company_name",
    "company_website",
    "contact_name",
    "contact_first_name",
    "contact_last_name",
    "contact_email",
    "contact_phone",
    "contact_title",
];

/// One grounded person candidate from the fill.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecordContactFill {
    pub contact_first_name: Option<String>,
    pub contact_last_name: Option<String>,
    pub contact_email: Option<String>,
    pub contact_phone: Option<String>,
    pub contact_title: Option<String>,
    pub provenance: Vec<DraftFieldProvenance>,
}

impl RecordContactFill {
    /// Espo's computed full name for the contact ("First Last").
    pub fn contact_full_name(&self) -> String {
        let mut parts = Vec::new();
        if let Some(first) = self.contact_first_name.as_deref() {
            parts.push(first);
        }
        if let Some(last) = self.contact_last_name.as_deref() {
            parts.push(last);
        }
        parts.join(" ")
    }

    fn has_contact(&self) -> bool {
        !self.contact_full_name().is_empty()
    }
}

/// A validated record fill. Names are already grounded — an ungrounded company
/// or contact name has been dropped, so a proposed record never carries an
/// invented name. The legacy single-contact fields mirror the first contact for
/// older call sites and tests; new code should use `contacts`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecordFill {
    pub company_name: Option<String>,
    pub company_website: Option<String>,
    pub contacts: Vec<RecordContactFill>,
    pub contact_first_name: Option<String>,
    pub contact_last_name: Option<String>,
    pub contact_email: Option<String>,
    pub contact_phone: Option<String>,
    pub contact_title: Option<String>,
    pub confidence: String,
    pub provenance: Vec<DraftFieldProvenance>,
}

impl RecordFill {
    /// Espo's computed full name for the contact ("First Last").
    pub fn contact_full_name(&self) -> String {
        if let Some(contact) = self.contacts.first() {
            return contact.contact_full_name();
        }
        let mut parts = Vec::new();
        if let Some(first) = self.contact_first_name.as_deref() {
            parts.push(first);
        }
        if let Some(last) = self.contact_last_name.as_deref() {
            parts.push(last);
        }
        parts.join(" ")
    }
}

pub fn parse_record_fill_response(response: &serde_json::Value) -> Result<RecordFill, String> {
    let confidence = string_field(response, "confidence")
        .filter(|raw| matches!(raw.as_str(), "high" | "medium" | "low"))
        .ok_or("confidence missing or invalid")?;
    let provenance: Vec<DraftFieldProvenance> = response
        .get("provenance")
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    let field = entry.get("field")?.as_str()?.trim().to_string();
                    if !PROVENANCE_FIELDS.contains(&field.as_str()) {
                        return None;
                    }
                    let quote: String = entry
                        .get("quote")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("")
                        .trim()
                        .chars()
                        .take(300)
                        .collect();
                    Some(DraftFieldProvenance { field, quote })
                })
                .collect()
        })
        .unwrap_or_default();

    // Company: kept only when its name is literally grounded.
    let company_name = string_field(response, "company_name")
        .filter(|name| enrichment_engine::quote_grounds(&provenance, &["company_name"], name))
        .map(|name| name.chars().take(200).collect::<String>());
    let company_website = company_name
        .as_ref()
        .and(string_field(response, "company_website"))
        .and_then(|w| normalize_company_website(&w))
        .map(|w| w.chars().take(300).collect::<String>());

    let contacts = parse_record_contacts(response, &provenance);
    let first_contact = contacts.first().cloned().unwrap_or_default();

    Ok(RecordFill {
        company_name,
        company_website,
        contacts,
        contact_first_name: first_contact.contact_first_name,
        contact_last_name: first_contact.contact_last_name,
        contact_email: first_contact.contact_email,
        contact_phone: first_contact.contact_phone,
        contact_title: first_contact.contact_title,
        confidence,
        provenance,
    })
}

fn parse_record_contacts(
    response: &serde_json::Value,
    legacy_provenance: &[DraftFieldProvenance],
) -> Vec<RecordContactFill> {
    let mut contacts = response
        .get("contacts")
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(parse_contact_object)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if contacts.is_empty() {
        if let Some(contact) = parse_legacy_contact(response, legacy_provenance) {
            contacts.push(contact);
        }
    }
    dedupe_contacts(contacts)
}

fn parse_contact_object(entry: &serde_json::Value) -> Option<RecordContactFill> {
    let first =
        string_field(entry, "first_name").or_else(|| string_field(entry, "contact_first_name"));
    let last =
        string_field(entry, "last_name").or_else(|| string_field(entry, "contact_last_name"));
    let full_name = [first.as_deref(), last.as_deref()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    let quote = string_field(entry, "quote")?;
    if full_name.is_empty() || !enrichment_engine::quote_contains_value(&quote, &full_name) {
        return None;
    }

    let email = string_field(entry, "email")
        .or_else(|| string_field(entry, "contact_email"))
        .filter(|raw| enrichment_engine::valid_email_shape(raw));
    let phone = string_field(entry, "phone")
        .or_else(|| string_field(entry, "contact_phone"))
        .map(|p| p.chars().take(50).collect::<String>());
    let title = string_field(entry, "title")
        .or_else(|| string_field(entry, "contact_title"))
        .map(|t| t.chars().take(150).collect::<String>());
    let mut provenance = vec![DraftFieldProvenance {
        field: "contact_name".to_string(),
        quote: quote.chars().take(300).collect(),
    }];
    if email
        .as_deref()
        .is_some_and(|value| enrichment_engine::quote_contains_value(&quote, value))
    {
        provenance.push(DraftFieldProvenance {
            field: "contact_email".to_string(),
            quote: quote.chars().take(300).collect(),
        });
    }
    if phone
        .as_deref()
        .is_some_and(|value| enrichment_engine::quote_contains_value(&quote, value))
    {
        provenance.push(DraftFieldProvenance {
            field: "contact_phone".to_string(),
            quote: quote.chars().take(300).collect(),
        });
    }
    if title
        .as_deref()
        .is_some_and(|value| enrichment_engine::quote_contains_value(&quote, value))
    {
        provenance.push(DraftFieldProvenance {
            field: "contact_title".to_string(),
            quote: quote.chars().take(300).collect(),
        });
    }
    Some(RecordContactFill {
        contact_first_name: first.map(|n| n.chars().take(100).collect::<String>()),
        contact_last_name: last.map(|n| n.chars().take(100).collect::<String>()),
        contact_email: email,
        contact_phone: phone,
        contact_title: title,
        provenance,
    })
}

fn parse_legacy_contact(
    response: &serde_json::Value,
    provenance: &[DraftFieldProvenance],
) -> Option<RecordContactFill> {
    let contact_first_name = string_field(response, "contact_first_name");
    let contact_last_name = string_field(response, "contact_last_name");
    let full_name = [contact_first_name.as_deref(), contact_last_name.as_deref()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    let contact_grounded = !full_name.is_empty()
        && enrichment_engine::quote_grounds(
            provenance,
            &["contact_name", "contact_first_name", "contact_last_name"],
            &full_name,
        );
    contact_grounded.then(|| RecordContactFill {
        contact_first_name: contact_first_name.map(|n| n.chars().take(100).collect::<String>()),
        contact_last_name: contact_last_name.map(|n| n.chars().take(100).collect::<String>()),
        contact_email: string_field(response, "contact_email")
            .filter(|raw| enrichment_engine::valid_email_shape(raw)),
        contact_phone: string_field(response, "contact_phone")
            .map(|p| p.chars().take(50).collect::<String>()),
        contact_title: string_field(response, "contact_title")
            .map(|t| t.chars().take(150).collect::<String>()),
        provenance: provenance
            .iter()
            .filter(|entry| entry.field.starts_with("contact_"))
            .cloned()
            .collect(),
    })
}

fn dedupe_contacts(contacts: Vec<RecordContactFill>) -> Vec<RecordContactFill> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for contact in contacts {
        let key = contact
            .contact_email
            .as_ref()
            .map(|email| email.to_ascii_lowercase())
            .unwrap_or_else(|| contact.contact_full_name().to_ascii_lowercase());
        if key.trim().is_empty() || !seen.insert(key) {
            continue;
        }
        out.push(contact);
    }
    out
}

fn normalize_company_website(raw: &str) -> Option<String> {
    let mut url = bos_integrations::web_page_read::normalize_seed_url(raw).ok()?;
    url.set_path("/");
    url.set_query(None);
    url.set_fragment(None);
    Some(url.to_string())
}

/// Which referenced records already exist in the CRM (Some = matched id).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecordMatches {
    pub account_id: Option<String>,
    pub contact_id: Option<String>,
}

/// Bounded LIVE CRM search (≤2 requests): company by name, contact by
/// email/name, against the CONFIGURED provider (EspoCRM or HubSpot). Read-only
/// and gate-independent — the operator sees accurate matched/missing proposals
/// before any write gate opens. Any error or an unconfigured instance records a
/// MISS (the safe default: propose creating the record, the operator reviews).
/// Shared with the CRM-aware crm_activity path (increment C).
pub fn search_existing_records(
    company_name: Option<&str>,
    contact_email: Option<&str>,
    contact_full_name: Option<&str>,
) -> RecordMatches {
    match crate::slices::crm_drafts::service::configured_crm_provider() {
        Ok(crate::slices::crm_drafts::service::PROVIDER_HUBSPOT) => {
            search_existing_records_hubspot(company_name, contact_email, contact_full_name)
        }
        // EspoCRM (and the safe default for an unknown/misconfigured provider —
        // a miss just proposes creation, which the operator reviews).
        _ => search_existing_records_espocrm(company_name, contact_email, contact_full_name),
    }
}

fn search_existing_records_espocrm(
    company_name: Option<&str>,
    contact_email: Option<&str>,
    contact_full_name: Option<&str>,
) -> RecordMatches {
    let config = EspoCrmWriteConfig {
        base_url: env_registry::string(&env_registry::BOS_ESPOCRM_BASE_URL),
        api_key: env_registry::string(&env_registry::BOS_ESPOCRM_API_KEY),
        write_enabled: false,
    };
    let Some(client) = espocrm_records_search_client(&config) else {
        return RecordMatches::default();
    };
    let account_id = company_name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .and_then(|name| match client.find_account(name) {
            Ok(found) => found,
            Err(err) => {
                tracing::warn!(error = ?err, "espocrm account search failed - treating as miss");
                None
            }
        });
    let contact_id = (contact_email.is_some() || contact_full_name.is_some())
        .then(|| client.find_contact(contact_email, contact_full_name))
        .and_then(|result| match result {
            Ok(found) => found,
            Err(err) => {
                tracing::warn!(error = ?err, "espocrm contact search failed - treating as miss");
                None
            }
        });
    RecordMatches {
        account_id,
        contact_id,
    }
}

fn search_existing_records_hubspot(
    company_name: Option<&str>,
    contact_email: Option<&str>,
    contact_full_name: Option<&str>,
) -> RecordMatches {
    let config = bos_integrations::hubspot::HubSpotWriteConfig {
        access_token: env_registry::string(&env_registry::BOS_HUBSPOT_ACCESS_TOKEN),
        write_enabled: false,
    };
    let Some(client) = bos_integrations::hubspot::hubspot_records_search_client(&config) else {
        return RecordMatches::default();
    };
    let account_id = company_name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .and_then(|name| match client.find_company(name) {
            Ok(found) => found,
            Err(err) => {
                tracing::warn!(error = ?err, "hubspot company search failed - treating as miss");
                None
            }
        });
    let contact_id = (contact_email.is_some() || contact_full_name.is_some())
        .then(|| client.find_contact(contact_email, contact_full_name))
        .and_then(|result| match result {
            Ok(found) => found,
            Err(err) => {
                tracing::warn!(error = ?err, "hubspot contact search failed - treating as miss");
                None
            }
        });
    RecordMatches {
        account_id,
        contact_id,
    }
}

/// Assemble the draft proposing the MISSING records. `matches` decides which
/// records are missing; a matched company is still carried (create_company
/// false) so the ensure-chain can link the contact to it. Returns None when
/// nothing is missing — both records already exist (or nothing was grounded),
/// so there is nothing to propose.
pub fn draft_from_fill(
    item: &WorkItem,
    fill: &RecordFill,
    matches: &RecordMatches,
    attempt: u64,
    model: &str,
    now_ms: u64,
) -> Option<CrmRecordDraft> {
    let has_company = fill.company_name.is_some();
    let contact = first_contact_for_fill(fill);
    let has_contact = contact.has_contact();
    let create_company = has_company && matches.account_id.is_none();
    let create_contact = has_contact && matches.contact_id.is_none();
    if !create_company && !create_contact {
        return None;
    }
    let provenance = draft_provenance(fill, &contact);
    Some(CrmRecordDraft {
        draft_id: format!("crd_{}_{attempt}", item.item_id),
        item_id: item.item_id.clone(),
        source_kind: item.source_kind.clone(),
        source_ref: item.source_ref.clone(),
        status: CrmRecordDraftStatus::Staged,
        create_company,
        company_name: fill.company_name.clone(),
        company_website: fill.company_website.clone(),
        company_phone: None,
        company_address: None,
        company_description: None,
        create_contact,
        contact_first_name: contact.contact_first_name,
        contact_last_name: contact.contact_last_name,
        contact_email: contact.contact_email,
        contact_phone: contact.contact_phone,
        contact_title: contact.contact_title,
        provider_ids: CrmRecordProviderIds::default(),
        provenance,
        enrichment_trace: None,
        research_annotations: Vec::new(),
        model: model.to_string(),
        confidence: fill.confidence.clone(),
        outbox_job_id: None,
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
    })
}

fn first_contact_for_fill(fill: &RecordFill) -> RecordContactFill {
    fill.contacts
        .first()
        .cloned()
        .unwrap_or_else(|| RecordContactFill {
            contact_first_name: fill.contact_first_name.clone(),
            contact_last_name: fill.contact_last_name.clone(),
            contact_email: fill.contact_email.clone(),
            contact_phone: fill.contact_phone.clone(),
            contact_title: fill.contact_title.clone(),
            provenance: fill
                .provenance
                .iter()
                .filter(|entry| entry.field.starts_with("contact_"))
                .cloned()
                .collect(),
        })
}

pub fn drafts_from_fill(
    item: &WorkItem,
    fill: &RecordFill,
    account_id: Option<String>,
    contact_matches: &[(RecordContactFill, Option<String>)],
    attempt: u64,
    model: &str,
    now_ms: u64,
) -> Vec<CrmRecordDraft> {
    let mut drafts = Vec::new();
    if contact_matches.is_empty() {
        if let Some(draft) = draft_from_fill(
            item,
            fill,
            &RecordMatches {
                account_id,
                contact_id: None,
            },
            attempt,
            model,
            now_ms,
        ) {
            drafts.push(draft);
        }
        return drafts;
    }
    let has_missing_contact = contact_matches
        .iter()
        .any(|(contact, contact_id)| contact.has_contact() && contact_id.is_none());
    let mut company_only_staged = false;
    for (idx, (contact, contact_id)) in contact_matches.iter().enumerate() {
        let mut scoped = fill.clone();
        scoped.contacts = vec![contact.clone()];
        scoped.contact_first_name = contact.contact_first_name.clone();
        scoped.contact_last_name = contact.contact_last_name.clone();
        scoped.contact_email = contact.contact_email.clone();
        scoped.contact_phone = contact.contact_phone.clone();
        scoped.contact_title = contact.contact_title.clone();
        let matches = RecordMatches {
            account_id: account_id.clone(),
            contact_id: contact_id.clone(),
        };
        if let Some(mut draft) = draft_from_fill(item, &scoped, &matches, attempt, model, now_ms) {
            if draft.create_company && !draft.create_contact {
                if has_missing_contact {
                    continue;
                }
                if company_only_staged {
                    continue;
                }
                company_only_staged = true;
            }
            draft.draft_id = format!("crd_{}_{}_{}", item.item_id, attempt, idx + 1);
            drafts.push(draft);
        }
    }
    drafts
}

fn draft_provenance(fill: &RecordFill, contact: &RecordContactFill) -> Vec<DraftFieldProvenance> {
    let mut provenance: Vec<DraftFieldProvenance> = fill
        .provenance
        .iter()
        .filter(|entry| entry.field.starts_with("company_"))
        .cloned()
        .collect();
    provenance.extend(contact.provenance.clone());
    provenance
}

// ---------------------------------------------------------------------------
// Website enrichment (Increment E): deterministic-first, LLM gap-filler last.
// ---------------------------------------------------------------------------

use super::store::{EnrichedValue, WebEnrichmentApply};
use bos_integrations::web_page_read::{EnrichmentField, WebEnrichment};

/// Espo's "First Last" for the draft's proposed contact (empty when none).
fn draft_contact_name(draft: &CrmRecordDraft) -> String {
    [
        draft.contact_first_name.as_deref(),
        draft.contact_last_name.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ")
}

fn enriched(field: &EnrichmentField) -> EnrichedValue {
    EnrichedValue {
        value: field.value.clone(),
        provenance_quote: field.provenance.clone(),
    }
}

/// Map the deterministic extraction onto the draft's still-missing columns.
/// Company fields apply only when a company is being created; the deterministic
/// pass never sources contact-person fields (a site rarely states them
/// structurally — those are the gap-filler's job, grounded to the named person).
pub fn deterministic_apply(enrich: &WebEnrichment, draft: &CrmRecordDraft) -> WebEnrichmentApply {
    let mut apply = WebEnrichmentApply::default();
    if draft.create_company {
        if let Some(name) = enrich.company_name.as_ref().filter(|name| {
            crate::produce::draft_field_policy::may_replace_weak_company_name(
                draft.company_name.as_deref(),
                &name.value,
                &draft.provenance,
            )
        }) {
            apply.company_name = Some(enriched(name));
        }
        if draft.company_website.is_none() {
            apply.company_website = enrich.company_website.as_ref().map(enriched);
        }
        if draft.company_phone.is_none() {
            apply.company_phone = enrich.company_phone.as_ref().map(enriched);
        }
        if draft.company_address.is_none() {
            apply.company_address = enrich.company_address.as_ref().map(enriched);
        }
        if draft.company_description.is_none() {
            apply.company_description = enrich.company_description.as_ref().map(enriched);
        }
    }
    apply
}

/// Which enrichment-eligible fields are STILL missing after the deterministic
/// pass — the set the LLM gap-filler is asked to ground. A field is missing
/// when its column is null on the draft, it applies to a record being created,
/// and the deterministic pass did not already fill it.
pub fn missing_enrich_fields(draft: &CrmRecordDraft, apply: &WebEnrichmentApply) -> Vec<String> {
    let mut missing = Vec::new();
    if draft.create_company
        && apply.company_name.is_none()
        && draft.company_name.as_deref().is_some_and(|current| {
            crate::produce::draft_field_policy::is_domain_like_display_name(current)
                && crate::produce::draft_field_policy::still_ai_prefill(
                    &draft.provenance,
                    "company_name",
                    current,
                )
        })
    {
        missing.push("company_name".to_string());
    }
    let mut want = |applies: bool, current: &Option<String>, filled: bool, field: &str| {
        if applies && current.is_none() && !filled {
            missing.push(field.to_string());
        }
    };
    want(
        draft.create_company,
        &draft.company_website,
        apply.company_website.is_some(),
        "company_website",
    );
    want(
        draft.create_company,
        &draft.company_phone,
        apply.company_phone.is_some(),
        "company_phone",
    );
    want(
        draft.create_company,
        &draft.company_address,
        apply.company_address.is_some(),
        "company_address",
    );
    want(
        draft.create_company,
        &draft.company_description,
        apply.company_description.is_some(),
        "company_description",
    );
    want(
        draft.create_contact,
        &draft.contact_email,
        apply.contact_email.is_some(),
        "contact_email",
    );
    want(
        draft.create_contact,
        &draft.contact_phone,
        apply.contact_phone.is_some(),
        "contact_phone",
    );
    want(
        draft.create_contact,
        &draft.contact_title,
        apply.contact_title.is_some(),
        "contact_title",
    );
    missing
}

/// The bounded gap-filler request: extract ONLY the still-missing record fields
/// from the fetched page text, each grounded by a literal page quote. The named
/// company/contact ride in so the model can find the right person's title/email.
pub fn build_enrichment_request(
    client_id: &str,
    item: &WorkItem,
    draft: &CrmRecordDraft,
    missing_fields: &[String],
    page_texts: &[bos_integrations::web_page_read::EnrichedPageText],
) -> TypedLlmTaskRequest {
    let task_id = format!("crm_web_enrich_{}", draft.draft_id);
    let company = draft.company_name.as_deref().unwrap_or("(unknown)");
    let contact = draft_contact_name(draft);
    let text_blocks = page_texts
        .iter()
        .enumerate()
        .map(|(idx, page)| TypedLlmTextBlock {
            block_id: format!("page_{idx}"),
            text: format!("URL: {}\n{}", page.url, page.text),
        })
        .collect();
    TypedLlmTaskRequest {
        task_id: task_id.clone(),
        correlation_id: item.item_id.clone(),
        idempotency_key: task_id,
        tenant_or_project_scope: client_id.to_string(),
        source_entity: Some(TypedLlmSourceEntity {
            entity_kind: "crm_record_draft".to_string(),
            entity_id: draft.draft_id.clone(),
        }),
        spec: TypedLlmTaskSpec {
            task_class: TypedLlmTaskClass::Extract,
            prompt_template_id: "crm_web_enrichment".to_string(),
            prompt_template_version: "1".to_string(),
            prompt_template_hash: String::new(),
            schema_ref: ENRICH_SCHEMA_REF.to_string(),
            response_format: TypedLlmResponseFormat::JsonObject,
            max_input_bytes: 48 * 1024,
            max_output_bytes: 2 * 1024,
            max_tokens: 0,
            timeout_ms: 0,
            capabilities: TypedLlmTaskCapabilities::pure_transformation(),
            authority: TypedLlmAuthority::no_side_effects(),
        },
        input: TypedLlmTaskInput {
            json: enrichment_engine::shape_enrichment_input(enrichment_engine::ShapeEnrichmentContract {
                subject: "crm_record_draft".to_string(),
                target_shape: crm_enrichment_target_shape(),
                current_values: crm_enrichment_current_values(draft),
                eligible_fields: missing_fields.to_vec(),
                context: json!({
                    "company_name": company,
                    "contact_name": contact,
                }),
                guidance: "CRM-specific rules: company_website should be the homepage/base domain, not a deep /about or /contact URL. company_description must be a concise factual phrase supported by its literal quote. For contact_* fields, the value must belong to the named contact, not a different person.".to_string(),
            }),
            text_blocks,
        },
        execution_policy: TypedLlmExecutionPolicy {
            default_route: TypedLlmExecutionRoute::Harness,
            fallback_policy: TypedLlmFallbackPolicy::NoFallback,
            retry_policy: TypedLlmRetryPolicy {
                max_attempts: 2,
                backoff_ms: 1_000,
                max_elapsed_ms: 180_000,
            },
        },
        provider_policy: TypedLlmProviderPolicy {
            preferred_provider: String::new(),
            preferred_model: String::new(),
            fallback_provider: None,
            fallback_model: None,
        },
        safety_policy: TypedLlmSafetyPolicy {
            redaction_policy: TypedLlmRedactionPolicy::PreSubmit,
            raw_output_retention: TypedLlmRawOutputRetention::None,
        },
    }
}

fn crm_enrichment_target_shape() -> serde_json::Value {
    json!({
        "company_name": "string|null",
        "company_website": "string|null",
        "company_phone": "string|null",
        "company_address": "string|null",
        "company_description": "string|null",
        "contact_first_name": "string|null",
        "contact_last_name": "string|null",
        "contact_email": "string|null",
        "contact_phone": "string|null",
        "contact_title": "string|null",
    })
}

fn crm_enrichment_current_values(draft: &CrmRecordDraft) -> serde_json::Value {
    json!({
        "company_name": draft.company_name,
        "company_website": draft.company_website,
        "company_phone": draft.company_phone,
        "company_address": draft.company_address,
        "company_description": draft.company_description,
        "contact_first_name": draft.contact_first_name,
        "contact_last_name": draft.contact_last_name,
        "contact_email": draft.contact_email,
        "contact_phone": draft.contact_phone,
        "contact_title": draft.contact_title,
    })
}

pub(crate) fn enrichment_domain_seed(
    draft: &CrmRecordDraft,
    note_text: &str,
    domain_override: Option<&str>,
) -> Option<String> {
    domain_override
        .and_then(bos_integrations::web_page_read::find_domain)
        .or_else(|| bos_integrations::web_page_read::find_domain(note_text))
        .or_else(|| {
            draft
                .company_website
                .as_deref()
                .and_then(bos_integrations::web_page_read::find_domain)
        })
}

fn manual_enrichment_epoch(idempotency_key: &str) -> String {
    format!("manual:{idempotency_key}")
}

fn standard_enrichment_run_id(
    slice_id: &'static str,
    actor_id: &str,
    item: &WorkItem,
    subject: &CrmRecordEnrichmentSubject,
    idempotency_key: Option<&str>,
) -> String {
    let ctx = enrichment_engine::EnrichmentRunContext {
        slice_id,
        actor_id,
        item,
    };
    match idempotency_key {
        Some(key) => enrichment_engine::planned_run_id_with_epoch(
            ctx,
            subject,
            &manual_enrichment_epoch(key),
        ),
        None => enrichment_engine::planned_run_id(ctx, subject),
    }
}

/// Parse the gap-filler output, KEEPING only fields whose provenance quote
/// literally appears in the fetched page text (case-insensitive). An ungrounded
/// field is dropped — never the whole fill (enrichment is best-effort). Only
/// fields in `missing_fields` are honored. The stored provenance quote is the
/// literal page span.
pub fn parse_enrichment_response(
    response: &serde_json::Value,
    page_text: &str,
    missing_fields: &[String],
) -> WebEnrichmentApply {
    parse_enrichment_response_with_diagnostics(response, page_text, missing_fields).apply
}

#[derive(Debug, Clone)]
pub(crate) struct WebEnrichmentParseResult {
    pub apply: WebEnrichmentApply,
    pub diagnostics: Vec<EnrichmentTierEvent>,
    pub proposals: Vec<EnrichmentFieldProposal>,
}

pub(crate) fn parse_enrichment_response_with_diagnostics(
    response: &serde_json::Value,
    page_text: &str,
    missing_fields: &[String],
) -> WebEnrichmentParseResult {
    let quotes: std::collections::HashMap<String, String> = response
        .get("provenance")
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    let field = entry.get("field")?.as_str()?.trim().to_string();
                    let quote = entry.get("quote")?.as_str()?.trim().to_string();
                    (!quote.is_empty()).then_some((field, quote))
                })
                .collect()
        })
        .unwrap_or_default();

    let mut diagnostics = Vec::new();
    let mut proposals = Vec::new();

    let mut company_website = parse_grounded_enrichment_field(
        response,
        &quotes,
        page_text,
        missing_fields,
        "company_website",
        false,
        &mut diagnostics,
        &mut proposals,
    );
    if let Some(value) = &mut company_website {
        if let Some(normalized) = normalize_company_website(&value.value) {
            value.value = normalized;
        }
    }

    let apply = WebEnrichmentApply {
        company_name: parse_grounded_enrichment_field(
            response,
            &quotes,
            page_text,
            missing_fields,
            "company_name",
            false,
            &mut diagnostics,
            &mut proposals,
        ),
        company_website,
        company_phone: parse_grounded_enrichment_field(
            response,
            &quotes,
            page_text,
            missing_fields,
            "company_phone",
            false,
            &mut diagnostics,
            &mut proposals,
        ),
        company_address: parse_grounded_enrichment_field(
            response,
            &quotes,
            page_text,
            missing_fields,
            "company_address",
            false,
            &mut diagnostics,
            &mut proposals,
        ),
        company_description: parse_grounded_enrichment_field(
            response,
            &quotes,
            page_text,
            missing_fields,
            "company_description",
            false,
            &mut diagnostics,
            &mut proposals,
        ),
        contact_email: parse_grounded_enrichment_field(
            response,
            &quotes,
            page_text,
            missing_fields,
            "contact_email",
            true,
            &mut diagnostics,
            &mut proposals,
        ),
        contact_phone: parse_grounded_enrichment_field(
            response,
            &quotes,
            page_text,
            missing_fields,
            "contact_phone",
            false,
            &mut diagnostics,
            &mut proposals,
        ),
        contact_title: parse_grounded_enrichment_field(
            response,
            &quotes,
            page_text,
            missing_fields,
            "contact_title",
            false,
            &mut diagnostics,
            &mut proposals,
        ),
    };
    WebEnrichmentParseResult {
        apply,
        diagnostics,
        proposals,
    }
}

#[allow(clippy::too_many_arguments)]
fn parse_grounded_enrichment_field(
    response: &serde_json::Value,
    quotes: &std::collections::HashMap<String, String>,
    page_text: &str,
    missing_fields: &[String],
    field: &str,
    require_email_shape: bool,
    diagnostics: &mut Vec<EnrichmentTierEvent>,
    proposals: &mut Vec<EnrichmentFieldProposal>,
) -> Option<EnrichedValue> {
    let (value, quote) = enrichment_field_candidate(response, quotes, field)?;
    let quote_ref = quote.as_deref();
    if !missing_fields.iter().any(|f| f == field) {
        record_parse_proposal(
            diagnostics,
            proposals,
            field,
            &value,
            quote_ref,
            false,
            "field_not_eligible",
        );
        return None;
    }
    let Some(quote) = quote else {
        record_parse_proposal(
            diagnostics,
            proposals,
            field,
            &value,
            None,
            false,
            "quote_missing",
        );
        return None;
    };
    if !enrichment_engine::literal_span_in_text(page_text, &quote) {
        record_parse_proposal(
            diagnostics,
            proposals,
            field,
            &value,
            Some(&quote),
            false,
            "quote_not_in_evidence",
        );
        return None;
    }
    if !enrichment_engine::quote_contains_value(&quote, &value) {
        record_parse_proposal(
            diagnostics,
            proposals,
            field,
            &value,
            Some(&quote),
            false,
            "value_not_supported_by_quote",
        );
        return None;
    }
    if require_email_shape && !enrichment_engine::valid_email_shape(&value) {
        record_parse_proposal(
            diagnostics,
            proposals,
            field,
            &value,
            Some(&quote),
            false,
            "invalid_email_shape",
        );
        return None;
    }
    record_parse_proposal(
        diagnostics,
        proposals,
        field,
        &value,
        Some(&quote),
        true,
        "grounded_quote",
    );
    Some(EnrichedValue {
        value: value.chars().take(300).collect(),
        provenance_quote: quote.chars().take(300).collect(),
    })
}

fn enrichment_field_candidate(
    response: &serde_json::Value,
    quotes: &std::collections::HashMap<String, String>,
    field: &str,
) -> Option<(String, Option<String>)> {
    if let Some(entry) = response.get("fields").and_then(|fields| fields.get(field)) {
        let value = entry.get("value")?.as_str()?.trim().to_string();
        if value.is_empty() {
            return None;
        }
        let quote = entry
            .get("quote")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|quote| !quote.is_empty())
            .map(str::to_string);
        return Some((value, quote));
    }
    let value = string_field(response, field)?;
    let quote = quotes.get(field).cloned();
    Some((value, quote))
}

fn record_parse_proposal(
    diagnostics: &mut Vec<EnrichmentTierEvent>,
    proposals: &mut Vec<EnrichmentFieldProposal>,
    field: &str,
    value: &str,
    quote: Option<&str>,
    accepted: bool,
    reason: &str,
) {
    diagnostics.push(enrichment_engine::field_event(
        EnrichmentTier::WebSearch,
        field,
        value,
        if accepted { "accepted" } else { "rejected" },
        reason,
        quote,
    ));
    proposals.push(EnrichmentFieldProposal {
        field_id: field.to_string(),
        proposed_value: value.to_string(),
        source_tier: EnrichmentTier::WebSearch,
        confidence: EnrichmentConfidence::Medium,
        provenance_refs: quote.map(|q| vec![q.to_string()]).unwrap_or_default(),
        accepted,
        reason: reason.to_string(),
    });
}

/// Merge the (grounded) gap-filler result into the deterministic apply — only
/// for fields the deterministic pass left empty.
pub fn merge_llm_apply(apply: &mut WebEnrichmentApply, llm: WebEnrichmentApply) {
    if apply.company_name.is_none() {
        apply.company_name = llm.company_name;
    }
    if apply.company_website.is_none() {
        apply.company_website = llm.company_website;
    }
    if apply.company_phone.is_none() {
        apply.company_phone = llm.company_phone;
    }
    if apply.company_address.is_none() {
        apply.company_address = llm.company_address;
    }
    if apply.company_description.is_none() {
        apply.company_description = llm.company_description;
    }
    if apply.contact_email.is_none() {
        apply.contact_email = llm.contact_email;
    }
    if apply.contact_phone.is_none() {
        apply.contact_phone = llm.contact_phone;
    }
    if apply.contact_title.is_none() {
        apply.contact_title = llm.contact_title;
    }
}

pub fn crm_search_enrichment_queries(domain: &str, draft: &CrmRecordDraft) -> Vec<String> {
    let current = draft.company_name.as_deref().unwrap_or(domain);
    vec![
        format!("{current} contact address phone email {domain}"),
        format!("{current} official company name {domain}"),
    ]
}

fn crm_search_enrichment_reason(
    draft: &CrmRecordDraft,
    apply: &WebEnrichmentApply,
) -> Option<&'static str> {
    if !missing_enrich_fields(draft, apply).is_empty() {
        return Some("missing_crm_fields");
    }
    let current = draft.company_name.as_deref()?;
    (draft.create_company
        && apply.company_name.is_none()
        && crate::produce::draft_field_policy::is_domain_like_display_name(current)
        && crate::produce::draft_field_policy::still_ai_prefill(
            &draft.provenance,
            "company_name",
            current,
        ))
    .then_some(SEARCH_ENRICH_REASON_WEAK_COMPANY_NAME)
}

fn crm_enrichment_plan(
    draft: &CrmRecordDraft,
    note_text: &str,
    domain_override: Option<&str>,
) -> EnrichmentPlan {
    let mut fields = vec![
        enrichment_engine::field_spec(
            "company_name",
            "name",
            EnrichmentEligibility::WeakPrefill,
            EnrichmentConfidence::Medium,
        ),
        enrichment_engine::field_spec(
            "company_website",
            "domain",
            EnrichmentEligibility::MissingOnly,
            EnrichmentConfidence::Medium,
        ),
        enrichment_engine::field_spec(
            "company_phone",
            "phone",
            EnrichmentEligibility::MissingOnly,
            EnrichmentConfidence::Medium,
        ),
        enrichment_engine::field_spec(
            "company_address",
            "address",
            EnrichmentEligibility::MissingOnly,
            EnrichmentConfidence::Medium,
        ),
        enrichment_engine::field_spec(
            "company_description",
            "description",
            EnrichmentEligibility::MissingOnly,
            EnrichmentConfidence::Medium,
        ),
    ];
    if draft.create_contact {
        fields.extend([
            enrichment_engine::field_spec(
                "contact_email",
                "email",
                EnrichmentEligibility::MissingOnly,
                EnrichmentConfidence::Medium,
            ),
            enrichment_engine::field_spec(
                "contact_phone",
                "phone",
                EnrichmentEligibility::MissingOnly,
                EnrichmentConfidence::Medium,
            ),
            enrichment_engine::field_spec(
                "contact_title",
                "description",
                EnrichmentEligibility::MissingOnly,
                EnrichmentConfidence::Medium,
            ),
        ]);
    }
    let mut seed_evidence = vec![EnrichmentSeedEvidence {
        source_id: format!("{}:{}", draft.source_kind, draft.source_ref),
        label: "Source".to_string(),
        quote: Some(note_text.chars().take(500).collect()),
    }];
    if let Some(domain) = enrichment_domain_seed(draft, note_text, domain_override) {
        let source_domain = bos_integrations::web_page_read::find_domain(note_text);
        let (source_id, label) = if domain_override.is_some() {
            ("operator_domain_seed", "Operator domain seed")
        } else if source_domain.as_deref() == Some(domain.as_str()) {
            ("source_domain_seed", "Source domain seed")
        } else {
            ("draft_website_domain_seed", "Draft website domain seed")
        };
        seed_evidence.push(EnrichmentSeedEvidence {
            source_id: source_id.to_string(),
            label: label.to_string(),
            quote: Some(domain),
        });
    }
    EnrichmentPlan {
        subject: if draft.create_contact {
            "crm_record_contact".to_string()
        } else {
            "crm_record_company".to_string()
        },
        fields,
        seed_evidence,
        enabled_tiers: vec![EnrichmentTier::Local, EnrichmentTier::WebSearch],
        stop_policy: vec![
            "all_fields_accepted".to_string(),
            "no_literal_domain_for_tier3".to_string(),
            "tier_budget_exhausted".to_string(),
            "draft_left_staged_state".to_string(),
        ],
    }
}

#[cfg(test)]
pub(crate) fn crm_enrichment_plan_for_test(
    draft: &CrmRecordDraft,
    note_text: &str,
    domain_override: Option<&str>,
) -> EnrichmentPlan {
    crm_enrichment_plan(draft, note_text, domain_override)
}

fn tier1_events(
    draft: &CrmRecordDraft,
) -> (Vec<EnrichmentTierEvent>, Vec<EnrichmentFieldProposal>) {
    let source_id = format!("{}:{}", draft.source_kind, draft.source_ref);
    let mut events = vec![enrichment_engine::source_evidence_event(
        &source_id,
        "operator_source_loaded",
    )];
    let (prefill_events, proposals) = enrichment_engine::existing_prefill_events(
        &source_id,
        [
            ("company_name", draft.company_name.as_deref()),
            ("company_website", draft.company_website.as_deref()),
            ("company_phone", draft.company_phone.as_deref()),
            ("company_address", draft.company_address.as_deref()),
            ("company_description", draft.company_description.as_deref()),
            ("contact_email", draft.contact_email.as_deref()),
            ("contact_phone", draft.contact_phone.as_deref()),
            ("contact_title", draft.contact_title.as_deref()),
        ],
    );
    events.extend(prefill_events);
    if !draft.create_company {
        events.push(enrichment_engine::skip_event(
            EnrichmentTier::Local,
            "provider_record_match",
            "company_already_exists",
        ));
    }
    if !draft.create_contact {
        events.push(enrichment_engine::skip_event(
            EnrichmentTier::Local,
            "provider_record_match",
            "contact_already_exists",
        ));
    }
    (events, proposals)
}

struct CrmRecordEnrichmentSubject {
    draft: CrmRecordDraft,
    note_text: String,
    domain_override: Option<String>,
}

impl CrmRecordEnrichmentSubject {
    fn new(draft: CrmRecordDraft, note_text: String, domain_override: Option<String>) -> Self {
        Self {
            draft,
            note_text,
            domain_override,
        }
    }
}

impl enrichment_engine::EnrichableDraft for CrmRecordEnrichmentSubject {
    type Apply = WebEnrichmentApply;

    fn deterministic_apply(
        &self,
        enrich: &bos_integrations::web_page_read::WebEnrichment,
    ) -> Self::Apply {
        deterministic_apply(enrich, &self.draft)
    }

    fn apply_is_empty(&self, apply: &Self::Apply) -> bool {
        apply.is_empty()
    }

    fn missing_fields(&self, apply: &Self::Apply) -> Vec<String> {
        missing_enrich_fields(&self.draft, apply)
    }

    fn build_request(
        &self,
        client_id: &str,
        item: &WorkItem,
        missing_fields: &[String],
        page_texts: &[bos_integrations::web_page_read::EnrichedPageText],
    ) -> TypedLlmTaskRequest {
        build_enrichment_request(client_id, item, &self.draft, missing_fields, page_texts)
    }

    fn parse_response(
        &self,
        response: &serde_json::Value,
        page_text: &str,
        missing_fields: &[String],
    ) -> Self::Apply {
        parse_enrichment_response(response, page_text, missing_fields)
    }

    fn parse_response_with_diagnostics(
        &self,
        response: &serde_json::Value,
        page_text: &str,
        missing_fields: &[String],
        _tier: EnrichmentTier,
        _reason: &str,
    ) -> (
        Self::Apply,
        Vec<EnrichmentTierEvent>,
        Vec<EnrichmentFieldProposal>,
    ) {
        let result =
            parse_enrichment_response_with_diagnostics(response, page_text, missing_fields);
        (result.apply, result.diagnostics, result.proposals)
    }

    fn merge_apply(&self, apply: &mut Self::Apply, patch: Self::Apply) {
        merge_llm_apply(apply, patch);
    }

    fn apply_diagnostics(
        &self,
        apply: &Self::Apply,
        tier: EnrichmentTier,
        reason: &str,
    ) -> (Vec<EnrichmentTierEvent>, Vec<EnrichmentFieldProposal>) {
        web_apply_diagnostics(apply, tier, reason)
    }

    fn search_trigger_reason(&self, apply: &Self::Apply) -> Option<&'static str> {
        crm_search_enrichment_reason(&self.draft, apply)
    }

    fn search_queries(&self, domain: &str) -> Vec<String> {
        crm_search_enrichment_queries(domain, &self.draft)
    }

    fn search_fields(&self, apply: &Self::Apply) -> Vec<String> {
        missing_enrich_fields(&self.draft, apply)
    }

    fn purpose(&self) -> &'static str {
        ENRICH_PURPOSE
    }

    fn slice_id(&self) -> &'static str {
        "crm_record_drafts"
    }

    fn max_text_chars(&self) -> usize {
        ENRICH_MAX_TEXT_CHARS
    }

    fn gap_fill_log_message(&self) -> &'static str {
        "web enrichment gap-fill failed"
    }

    fn search_gap_fill_log_message(&self) -> &'static str {
        "web-search enrichment gap-fill failed"
    }

    fn finalize_web_enrichment(
        &self,
        state: &crate::http::AppState,
        ctx: enrichment_engine::EnrichmentRunContext<'_>,
        run: enrichment_engine::EnrichmentRunHandle<'_>,
        inputs: enrichment_engine::WebEnrichmentFinalizeInputs<Self::Apply>,
    ) -> enrichment_engine::EnrichmentOutcome {
        let enrichment_engine::WebEnrichmentFinalizeInputs {
            apply,
            llm_apply,
            deterministic,
            pages,
            page_texts,
            search_evidence,
            llm_ran,
            domain,
        } = inputs;
        // Build the operator-facing trace (persisted even when nothing applied,
        // so the panel can show "we crawled X, found nothing groundable").
        let trace = build_enrichment_trace(
            &domain,
            &self.draft,
            &pages,
            &deterministic,
            &llm_apply,
            &page_texts,
            search_evidence.as_ref(),
            llm_ran,
            crate::http::now_ms(),
        );

        // Graft + store the trace under the lock; the store fills only NULL
        // columns (note-fill / operator edits win) and appends provenance for
        // what it filled.
        let mut persistence = state.persistence.lock();
        let idempotency_key = format!("crmenrich:{}:{}", self.draft.draft_id, run.run_id());
        let apply_ctx = super::store::DraftActionContext {
            client_id: &state.client_id,
            actor_id: ctx.actor_id,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            now_ms: crate::http::now_ms(),
        };
        if let Err(err) = super::store::apply_web_enrichment(
            persistence.connection(),
            apply_ctx,
            &self.draft.draft_id,
            &apply,
            Some(&trace),
        ) {
            tracing::warn!(item_id = %ctx.item.item_id, error = %err, "web enrichment apply failed");
            drop(persistence);
            let events = vec![enrichment_engine::skip_event(
                EnrichmentTier::WebSearch,
                "failure",
                &format!("apply_failed:{err}"),
            )];
            run.append(state, "tier3-apply-failed", &events, &[], 0);
            return run.transition(state, EnrichmentRunStatus::Failed, "apply_failed");
        }
        drop(persistence);
        let no_accepted_fields = self.apply_is_empty(&apply);
        run.transition(
            state,
            if no_accepted_fields {
                EnrichmentRunStatus::Partial
            } else {
                EnrichmentRunStatus::Completed
            },
            if no_accepted_fields {
                "no_accepted_fields"
            } else {
                "accepted_fields_applied"
            },
        )
    }
}

impl enrichment_engine::EnrichmentSubject for CrmRecordEnrichmentSubject {
    fn draft_id(&self) -> &str {
        &self.draft.draft_id
    }

    fn item_id(&self) -> &str {
        &self.draft.item_id
    }

    fn plan(&self) -> EnrichmentPlan {
        crm_enrichment_plan(
            &self.draft,
            &self.note_text,
            self.domain_override.as_deref(),
        )
    }

    fn tier1_events(&self) -> (Vec<EnrichmentTierEvent>, Vec<EnrichmentFieldProposal>) {
        tier1_events(&self.draft)
    }

    fn literal_domain(&self) -> Option<String> {
        enrichment_domain_seed(
            &self.draft,
            &self.note_text,
            self.domain_override.as_deref(),
        )
    }

    fn run_web_search_tier(
        &self,
        state: &crate::http::AppState,
        ctx: enrichment_engine::EnrichmentRunContext<'_>,
        run: enrichment_engine::EnrichmentRunHandle<'_>,
        domain: &str,
    ) -> enrichment_engine::EnrichmentOutcome {
        enrichment_engine::run_web_search_tier(self, state, ctx, run, domain)
    }
}

fn cached_crm_contact_id(context: &serde_json::Value, email: Option<&str>) -> Option<String> {
    let needle = email?.trim().to_ascii_lowercase();
    let lookup: crate::slices::grounding::CrmContactLookup =
        serde_json::from_value(context.get("crm_contact_lookup")?.clone()).ok()?;
    let mut matches = lookup
        .contacts
        .into_iter()
        .filter(|contact| {
            contact
                .email
                .as_deref()
                .is_some_and(|email| email.trim().eq_ignore_ascii_case(&needle))
        })
        .map(|contact| contact.provider_contact_id);
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

#[cfg(test)]
pub(crate) fn cached_crm_contact_id_for_test(
    context: &serde_json::Value,
    email: Option<&str>,
) -> Option<String> {
    cached_crm_contact_id(context, email)
}

#[allow(clippy::too_many_arguments)]
fn append_grounding_evidence(
    conn: &mut rusqlite::Connection,
    client_id: &str,
    item: &WorkItem,
    attempt: u64,
    _message: &InboundMessageRecord,
    scope: &crate::http::OperatorScope,
    actor_id: &str,
    tool_name: &str,
    tool_args_json: &str,
    result_ref: &str,
    result_excerpt: &str,
    actor_kind: bos_contracts::receipt::ActorKindDto,
    now_ms: u64,
) {
    let _ = crate::slices::grounding::append_grounding_evidence(
        conn,
        client_id,
        crate::slices::grounding::NewGroundingEvidence {
            work_item_id: &item.item_id,
            draft_id: None,
            packet_kind: PACKET_KIND,
            attempt,
            source_kind: &item.source_kind,
            source_ref: &item.source_ref,
            tool_name,
            tool_args_json,
            result_ref,
            result_excerpt,
            scope,
            actor_id,
            actor_kind,
            now_ms,
        },
    );
}

#[derive(Debug)]
pub(crate) enum OnDemandEnrichmentError {
    DraftNotFound,
    DraftNotStaged,
    NothingToEnrich,
    SourceMissing,
    DomainSeedInvalid,
    ResearchModeDisabled,
    ResearchDomainMissing,
    ResearchConcurrencyLimit,
    Store(StoreError),
}

impl From<StoreError> for OnDemandEnrichmentError {
    fn from(err: StoreError) -> Self {
        Self::Store(err)
    }
}

pub(crate) struct OnDemandEnrichmentKickoff {
    pub run_id: String,
    pub already_running: bool,
}

pub(crate) fn normalize_enrichment_domain_seed(
    domain_seed: Option<&str>,
) -> Result<Option<String>, OnDemandEnrichmentError> {
    crate::slices::enrichment::web_tier::normalize_domain_seed(domain_seed)
        .map_err(|_| OnDemandEnrichmentError::DomainSeedInvalid)
}

pub(crate) fn kick_on_demand_enrichment(
    state: crate::http::AppState,
    draft_id: String,
    actor_id: String,
    idempotency_key: String,
    domain_override: Option<String>,
    mode: Option<EnrichmentMode>,
) -> Result<OnDemandEnrichmentKickoff, OnDemandEnrichmentError> {
    if mode == Some(EnrichmentMode::Research) {
        return kick_on_demand_research_enrichment(
            state,
            draft_id,
            actor_id,
            idempotency_key,
            domain_override,
        );
    }
    let (draft, item, note_text, planned_run_id) = {
        let persistence = state.persistence.lock();
        let conn = persistence.connection_ref();
        let draft = load_staged_enrichment_draft(conn, &state.client_id, &draft_id)?;
        let item = crate::slices::work_queue::store::get_item_unscoped(
            conn,
            &state.client_id,
            &draft.item_id,
        )?
        .ok_or(OnDemandEnrichmentError::SourceMissing)?
        .item;
        let note_text = enrichment_note_text(conn, &state.client_id, &item)?
            .ok_or(OnDemandEnrichmentError::SourceMissing)?;
        let subject = CrmRecordEnrichmentSubject::new(
            draft.clone(),
            note_text.clone(),
            domain_override.clone(),
        );
        let planned_run_id = standard_enrichment_run_id(
            "crm_record_drafts",
            &actor_id,
            &item,
            &subject,
            Some(&idempotency_key),
        );
        (draft, item, note_text, planned_run_id)
    };

    match crate::slices::async_kickoff::begin(
        KickoffSpec {
            slice_id: "crm_record_drafts",
            draft_id: &draft_id,
            planned_run_id: &planned_run_id,
            capacity: KickoffCapacity::Unbounded,
        },
        || {
            record_enrichment_kickoff(
                &state,
                &actor_id,
                &idempotency_key,
                &planned_run_id,
                &draft_id,
                &item.item_id,
            )
        },
    )? {
        KickoffDecision::AlreadyRunning { run_id } => Ok(OnDemandEnrichmentKickoff {
            run_id,
            already_running: true,
        }),
        KickoffDecision::CapacityExceeded => {
            unreachable!("standard enrichment does not request capacity")
        }
        KickoffDecision::Replayed { run_id } => Ok(OnDemandEnrichmentKickoff {
            run_id,
            already_running: false,
        }),
        KickoffDecision::Spawn { run_id, guard } => {
            std::thread::Builder::new()
                .name(format!("enrich-crm-record-{draft_id}"))
                .spawn(move || {
                    let _guard = guard;
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        run_record_enrichment(
                            &state,
                            &item,
                            draft,
                            note_text,
                            &actor_id,
                            domain_override,
                            Some(&manual_enrichment_epoch(&idempotency_key)),
                        );
                    }));
                    if result.is_err() {
                        tracing::error!(draft_id = %draft_id, "crm on-demand enrichment panicked");
                    }
                })
                .expect("spawn crm enrichment thread");
            Ok(OnDemandEnrichmentKickoff {
                run_id,
                already_running: false,
            })
        }
    }
}

fn kick_on_demand_research_enrichment(
    state: crate::http::AppState,
    draft_id: String,
    actor_id: String,
    idempotency_key: String,
    domain_override: Option<String>,
) -> Result<OnDemandEnrichmentKickoff, OnDemandEnrichmentError> {
    let research_config = env_registry::agentic_web_research_config();
    if !research_config.enabled {
        return Err(OnDemandEnrichmentError::ResearchModeDisabled);
    }

    let (draft, item, note_text, seed_domain, missing_fields, planned_run_id) = {
        let persistence = state.persistence.lock();
        let conn = persistence.connection_ref();
        let draft = load_staged_enrichment_draft(conn, &state.client_id, &draft_id)?;
        let item = crate::slices::work_queue::store::get_item_unscoped(
            conn,
            &state.client_id,
            &draft.item_id,
        )?
        .ok_or(OnDemandEnrichmentError::SourceMissing)?
        .item;
        let note_text = enrichment_note_text(conn, &state.client_id, &item)?
            .ok_or(OnDemandEnrichmentError::SourceMissing)?;
        let seed_domain = enrichment_domain_seed(&draft, &note_text, domain_override.as_deref())
            .ok_or(OnDemandEnrichmentError::ResearchDomainMissing)?;
        let missing_fields = missing_enrich_fields(&draft, &WebEnrichmentApply::default());
        if missing_fields.is_empty() {
            return Err(OnDemandEnrichmentError::NothingToEnrich);
        }
        let subject = CrmRecordEnrichmentSubject::new(
            draft.clone(),
            note_text.clone(),
            domain_override.clone(),
        );
        let ctx = enrichment_engine::EnrichmentRunContext {
            slice_id: "crm_record_drafts",
            actor_id: &actor_id,
            item: &item,
        };
        let planned_run_id = enrichment_engine::planned_run_id_with_runtime_fingerprint(
            ctx,
            &subject,
            EnrichmentMode::Research,
            &missing_fields,
        );
        (
            draft,
            item,
            note_text,
            seed_domain,
            missing_fields,
            planned_run_id,
        )
    };

    match crate::slices::async_kickoff::begin(
        KickoffSpec {
            slice_id: "crm_record_drafts",
            draft_id: &draft_id,
            planned_run_id: &planned_run_id,
            capacity: KickoffCapacity::Limited {
                group: "agentic_web_research",
                max_concurrent: research_config.max_concurrent_runs,
            },
        },
        || {
            record_enrichment_kickoff(
                &state,
                &actor_id,
                &idempotency_key,
                &planned_run_id,
                &draft_id,
                &item.item_id,
            )
        },
    )? {
        KickoffDecision::AlreadyRunning { run_id } => Ok(OnDemandEnrichmentKickoff {
            run_id,
            already_running: true,
        }),
        KickoffDecision::CapacityExceeded => Err(OnDemandEnrichmentError::ResearchConcurrencyLimit),
        KickoffDecision::Replayed { run_id } => Ok(OnDemandEnrichmentKickoff {
            run_id,
            already_running: false,
        }),
        KickoffDecision::Spawn { run_id, guard } => {
            let thread_run_id = run_id.clone();
            std::thread::Builder::new()
                .name(format!("research-crm-record-{draft_id}"))
                .spawn(move || {
                    let _guard = guard;
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        run_record_research_enrichment(
                            &state,
                            &item,
                            draft,
                            note_text,
                            &actor_id,
                            domain_override,
                            seed_domain,
                            missing_fields,
                            research_config,
                            &thread_run_id,
                        );
                    }));
                    if result.is_err() {
                        tracing::error!(draft_id = %draft_id, "crm research enrichment panicked");
                    }
                })
                .expect("spawn crm research enrichment thread");
            Ok(OnDemandEnrichmentKickoff {
                run_id,
                already_running: false,
            })
        }
    }
}

fn record_enrichment_kickoff(
    state: &crate::http::AppState,
    actor_id: &str,
    idempotency_key: &str,
    planned_run_id: &str,
    draft_id: &str,
    item_id: &str,
) -> Result<RecordedKickoff, OnDemandEnrichmentError> {
    let mut persistence = state.persistence.lock();
    let kickoff = crate::slices::enrichment::store::record_on_demand_kickoff(
        persistence.connection(),
        &state.client_id,
        actor_id,
        crate::slices::enrichment::store::OnDemandKickoff {
            run_id: planned_run_id,
            slice_id: "crm_record_drafts",
            draft_id,
            item_id,
            idempotency_key,
            now_ms: crate::http::now_ms(),
        },
    )?;
    Ok(RecordedKickoff {
        run_id: kickoff.run_id,
        replayed: matches!(kickoff.mutation, MutationOutcome::ReplayedIdempotent { .. }),
    })
}

fn load_staged_enrichment_draft(
    conn: &rusqlite::Connection,
    client_id: &str,
    draft_id: &str,
) -> Result<CrmRecordDraft, OnDemandEnrichmentError> {
    let draft = super::store::get_draft(conn, client_id, draft_id)?
        .ok_or(OnDemandEnrichmentError::DraftNotFound)?
        .draft;
    if draft.status != CrmRecordDraftStatus::Staged {
        return Err(OnDemandEnrichmentError::DraftNotStaged);
    }
    if !draft.create_company && !draft.create_contact {
        return Err(OnDemandEnrichmentError::NothingToEnrich);
    }
    Ok(draft)
}

fn enrichment_note_text(
    conn: &rusqlite::Connection,
    client_id: &str,
    item: &WorkItem,
) -> Result<Option<String>, OnDemandEnrichmentError> {
    match crate::produce::resolve_source(conn, client_id, item) {
        Ok(message) => Ok(message.map(|message| {
            format!(
                "{} {}",
                message.subject.as_deref().unwrap_or(""),
                crate::slices::email_triage::service::body_for_ai(&message)
            )
        })),
        Err(crate::produce::SourceError::Store(err)) => Err(OnDemandEnrichmentError::Store(err)),
        Err(crate::produce::SourceError::Unsupported) => Ok(None),
    }
}

fn run_record_enrichment(
    state: &crate::http::AppState,
    item: &WorkItem,
    draft: CrmRecordDraft,
    note_text: String,
    actor_id: &str,
    domain_override: Option<String>,
    trigger_epoch: Option<&str>,
) -> enrichment_engine::EnrichmentOutcome {
    let subject = CrmRecordEnrichmentSubject::new(draft, note_text, domain_override);
    let ctx = enrichment_engine::EnrichmentRunContext {
        slice_id: "crm_record_drafts",
        actor_id,
        item,
    };
    match trigger_epoch {
        Some(trigger_epoch) => {
            enrichment_engine::run_with_trigger_epoch(state, ctx, &subject, trigger_epoch)
        }
        None => enrichment_engine::run(state, ctx, &subject),
    }
}

#[allow(clippy::too_many_arguments)]
fn run_record_research_enrichment(
    state: &crate::http::AppState,
    item: &WorkItem,
    draft: CrmRecordDraft,
    note_text: String,
    actor_id: &str,
    domain_override: Option<String>,
    seed_domain: String,
    unresolved_field_ids: Vec<String>,
    research_config: env_registry::AgenticWebResearchConfig,
    run_id: &str,
) -> enrichment_engine::EnrichmentOutcome {
    let subject = CrmRecordEnrichmentSubject::new(draft.clone(), note_text, domain_override);
    let plan = enrichment_engine::EnrichmentSubject::plan(&subject);
    start_research_run(state, item, actor_id, &draft, &plan, run_id);
    let (tier1_diagnostics, tier1_proposals) =
        enrichment_engine::EnrichmentSubject::tier1_events(&subject);
    append_research_diagnostics(
        state,
        actor_id,
        run_id,
        "tier1",
        &tier1_diagnostics,
        &tier1_proposals,
    );

    let unresolved = unresolved_research_fields(&plan, &unresolved_field_ids);
    let runner_input = research_run_input(&plan, seed_domain.clone(), &unresolved);
    let mut search_config = env_registry::web_search_enrichment_config();
    search_config.enabled = true;
    search_config.max_queries = research_config.max_searches;
    search_config.max_results_per_query = research_config.max_results;
    search_config.max_fetched_pages = 0;
    search_config.timeout_ms = research_config.timeout_ms;
    search_config.cost_budget_micros = research_config.cost_budget_micros;
    let http = Arc::new(ReqwestWebHttpClient::default());
    let resolver = Arc::new(SystemHostResolver);
    let search_collector = WebSearchCollector::new(
        Arc::new(ReqwestWebSearchApi),
        http.clone(),
        resolver.clone(),
        search_config,
    );
    let page_reader =
        crate::slices::enrichment::research::runner_page_reader(http, resolver, &research_config);
    let decider = crate::slices::enrichment::research::RealResearchDecider {
        persistence: state.persistence.clone(),
        research_config: research_config.clone(),
        client_id: state.client_id.to_string(),
        run_id: run_id.to_string(),
    };
    let outcome = crate::slices::enrichment::research::ResearchRunner::new(
        search_collector,
        page_reader,
        decider,
        research_config,
    )
    .run(runner_input);
    append_research_diagnostics(
        state,
        actor_id,
        run_id,
        "tier4-research",
        &outcome.diagnostics,
        &[],
    );

    let candidates = crate::slices::enrichment::research_finalize::extract_candidates(
        &outcome.evidence,
        &unresolved,
    );
    let accepted = crate::slices::enrichment::research_finalize::finalize_research_candidates(
        candidates,
        &outcome.evidence,
        seed_domain.clone(),
        enrichment_engine::registered_value_kinds(),
        &unresolved,
    );
    let apply = research_apply_from_accepted(&accepted);
    let annotations = research_annotations_from_accepted(&accepted, &outcome.evidence, &unresolved);
    let trace = build_research_enrichment_trace(
        &seed_domain,
        &outcome,
        &accepted,
        annotations,
        crate::http::now_ms(),
    );

    let mut persistence = state.persistence.lock();
    let idempotency_key = format!("enrichment:{run_id}:research_apply");
    let apply_ctx = super::store::DraftActionContext {
        client_id: &state.client_id,
        actor_id,
        expected_revision: None,
        idempotency_key: &idempotency_key,
        now_ms: crate::http::now_ms(),
    };
    let apply_result = super::store::apply_web_enrichment(
        persistence.connection(),
        apply_ctx,
        &draft.draft_id,
        &apply,
        Some(&trace),
    );
    drop(persistence);

    let (status, reason) = match apply_result {
        Ok(_) if !apply.is_empty() => (
            EnrichmentRunStatus::Completed,
            "research_accepted_fields_applied",
        ),
        Ok(_) => (EnrichmentRunStatus::Partial, "research_no_accepted_fields"),
        Err(err) => {
            tracing::warn!(item_id = %item.item_id, error = %err, "research enrichment apply failed");
            (EnrichmentRunStatus::Failed, "research_apply_failed")
        }
    };
    transition_research_run(state, actor_id, run_id, status, reason);
    enrichment_engine::EnrichmentOutcome {
        run_id: run_id.to_string(),
        status,
        reason: reason.to_string(),
    }
}

fn start_research_run(
    state: &crate::http::AppState,
    item: &WorkItem,
    actor_id: &str,
    draft: &CrmRecordDraft,
    plan: &EnrichmentPlan,
    run_id: &str,
) {
    let mut persistence = state.persistence.lock();
    if let Err(err) = crate::slices::enrichment::store::start_run(
        persistence.connection(),
        &state.client_id,
        actor_id,
        crate::slices::enrichment::store::StartRun {
            run_id,
            slice_id: "crm_record_drafts",
            draft_id: &draft.draft_id,
            item_id: &item.item_id,
            plan,
            created_by: actor_id,
            now_ms: crate::http::now_ms(),
        },
    ) {
        tracing::warn!(draft_id = %draft.draft_id, error = %err, "research enrichment run start failed");
    }
}

fn append_research_diagnostics(
    state: &crate::http::AppState,
    actor_id: &str,
    run_id: &str,
    event_seq: &str,
    diagnostics: &[EnrichmentTierEvent],
    proposals: &[EnrichmentFieldProposal],
) {
    if diagnostics.is_empty() && proposals.is_empty() {
        return;
    }
    let mut persistence = state.persistence.lock();
    if let Err(err) = crate::slices::enrichment::store::append_run_diagnostics(
        persistence.connection(),
        &state.client_id,
        actor_id,
        crate::slices::enrichment::store::AppendRunDiagnostics {
            run_id,
            event_seq,
            diagnostics,
            proposals,
            cost_micros: 0,
            now_ms: crate::http::now_ms(),
        },
    ) {
        tracing::warn!(run_id = %run_id, event_seq, error = %err, "research diagnostics append failed");
    }
}

fn transition_research_run(
    state: &crate::http::AppState,
    actor_id: &str,
    run_id: &str,
    status: EnrichmentRunStatus,
    reason: &str,
) {
    let mut persistence = state.persistence.lock();
    if let Err(err) = crate::slices::enrichment::store::transition_run_status(
        persistence.connection(),
        &state.client_id,
        actor_id,
        crate::slices::enrichment::store::TransitionRunStatus {
            run_id,
            status,
            now_ms: crate::http::now_ms(),
            reason,
        },
    ) {
        tracing::warn!(run_id = %run_id, error = %err, "research run transition failed");
    }
}

fn unresolved_research_fields(
    plan: &EnrichmentPlan,
    unresolved_field_ids: &[String],
) -> crate::slices::enrichment::research_finalize::UnresolvedFieldSet {
    let ids: std::collections::BTreeSet<&str> =
        unresolved_field_ids.iter().map(String::as_str).collect();
    plan.fields
        .iter()
        .filter(|field| ids.contains(field.field_id.as_str()))
        .map(|field| (field.field_id.clone(), field.clone()))
        .collect()
}

pub(crate) fn research_run_input(
    plan: &EnrichmentPlan,
    seed_domain: String,
    unresolved: &crate::slices::enrichment::research_finalize::UnresolvedFieldSet,
) -> crate::slices::enrichment::research::ResearchRunInput {
    crate::slices::enrichment::research::ResearchRunInput {
        subject: plan.subject.clone(),
        seed_domain,
        unresolved_field_ids: unresolved.keys().cloned().collect(),
    }
}

pub(crate) fn research_apply_from_accepted(
    accepted: &[crate::slices::enrichment::research_finalize::AcceptedField],
) -> WebEnrichmentApply {
    let mut apply = WebEnrichmentApply::default();
    for field in accepted {
        let value = EnrichedValue {
            value: field.value.clone(),
            provenance_quote: field.quote.clone(),
        };
        match field.field_id.as_str() {
            "company_name" if apply.company_name.is_none() => apply.company_name = Some(value),
            "company_website" if apply.company_website.is_none() => {
                apply.company_website = Some(value)
            }
            "company_phone" if apply.company_phone.is_none() => apply.company_phone = Some(value),
            "company_address" if apply.company_address.is_none() => {
                apply.company_address = Some(value)
            }
            "company_description" if apply.company_description.is_none() => {
                apply.company_description = Some(value)
            }
            "contact_email" if apply.contact_email.is_none() => apply.contact_email = Some(value),
            "contact_phone" if apply.contact_phone.is_none() => apply.contact_phone = Some(value),
            "contact_title" if apply.contact_title.is_none() => apply.contact_title = Some(value),
            _ => {}
        }
    }
    apply
}

pub(crate) fn research_annotations_from_accepted(
    accepted: &[crate::slices::enrichment::research_finalize::AcceptedField],
    evidence: &bos_integrations::evidence::EvidenceStore,
    unresolved: &crate::slices::enrichment::research_finalize::UnresolvedFieldSet,
) -> Vec<CrmResearchFieldAnnotation> {
    accepted
        .iter()
        .filter_map(|field| {
            let page = evidence.get(&field.evidence_id)?;
            let spec = unresolved.get(&field.field_id)?;
            let kind = enrichment_engine::registered_value_kinds()
                .iter()
                .find(|kind| kind.value_kind == spec.value_kind)?;
            Some(CrmResearchFieldAnnotation {
                field_id: field.field_id.clone(),
                confidence: field.confidence,
                source_domain: page.registrable_domain.clone(),
                quote: field.quote.clone(),
                person_sensitive: kind.sensitivity
                    == enrichment_engine::SENSITIVITY_PERSON_SENSITIVE,
            })
        })
        .collect()
}

fn build_research_enrichment_trace(
    domain: &str,
    outcome: &crate::slices::enrichment::research::ResearchRunOutcome,
    accepted: &[crate::slices::enrichment::research_finalize::AcceptedField],
    annotations: Vec<CrmResearchFieldAnnotation>,
    now_ms: u64,
) -> bos_contracts::crm_record_drafts::CrmEnrichmentTrace {
    let items = accepted
        .iter()
        .map(
            |field| bos_contracts::crm_record_drafts::CrmEnrichmentTraceItem {
                field: field.field_id.clone(),
                previous_value: None,
                value: field.value.clone(),
                source: field.quote.clone(),
                via: "research".to_string(),
            },
        )
        .collect();
    let search_queries: Vec<String> = outcome
        .diagnostics
        .iter()
        .filter(|event| event.event_type == "search_query")
        .filter_map(|event| event.query.clone())
        .collect();
    let search_results: Vec<bos_contracts::crm_record_drafts::CrmSearchTraceResult> = outcome
        .diagnostics
        .iter()
        .filter(|event| event.event_type == "search_result")
        .map(
            |event| bos_contracts::crm_record_drafts::CrmSearchTraceResult {
                query: event.query.clone().unwrap_or_default(),
                title: event.title.clone().unwrap_or_default(),
                url: event.url.clone().unwrap_or_default(),
                snippet: event.snippet.clone().unwrap_or_default(),
            },
        )
        .collect();
    let failures: Vec<String> = outcome
        .diagnostics
        .iter()
        .filter(|event| event.status.as_deref() == Some("failed"))
        .filter_map(|event| event.reason.clone())
        .collect();
    bos_contracts::crm_record_drafts::CrmEnrichmentTrace {
        captured_at_ms: now_ms,
        domain: domain.to_string(),
        pages: outcome
            .evidence
            .pages()
            .map(|page| page.final_url.as_str().to_string())
            .collect(),
        items,
        llm_ran: true,
        llm_input_chars: outcome
            .evidence
            .pages()
            .map(|page| page.normalized_text.chars().count() as u32)
            .sum(),
        llm_input_preview: outcome
            .evidence
            .pages()
            .map(|page| page.normalized_text.as_ref())
            .collect::<Vec<_>>()
            .join("\n")
            .chars()
            .take(1_500)
            .collect(),
        search_ran: !search_queries.is_empty(),
        search_reason: Some(outcome.reason.clone()),
        search_queries,
        search_results,
        failures,
        research_annotations: annotations,
    }
}

pub(crate) fn freshness_candidates(
    state: &crate::http::AppState,
    adapter: &enrichment_engine::FreshnessAdapterRegistration,
    stale_after_ms: u64,
    now_ms: u64,
    limit: usize,
) -> Result<Vec<enrichment_engine::FreshnessCandidate>, String> {
    let mut out = Vec::new();
    let epoch = enrichment_engine::freshness_epoch(stale_after_ms, now_ms);
    let persistence = state.persistence.lock();
    let conn = persistence.connection_ref();
    for entry in super::store::list_drafts(conn, &state.client_id, None, limit.max(1) * 4)
        .map_err(|err| err.to_string())?
    {
        if out.len() >= limit {
            break;
        }
        let draft = entry.draft;
        if draft.status != CrmRecordDraftStatus::Staged {
            continue;
        }
        let actionable_fields = missing_enrich_fields(&draft, &WebEnrichmentApply::default())
            .into_iter()
            .filter(|field| adapter.critical_fields.contains(&field.as_str()))
            .collect::<Vec<_>>();
        if actionable_fields.is_empty() {
            continue;
        }
        let Some(item) = crate::slices::work_queue::store::get_item_unscoped(
            conn,
            &state.client_id,
            &draft.item_id,
        )
        .map_err(|err| err.to_string())?
        .map(|entry| entry.item) else {
            continue;
        };
        let Some(note_text) = enrichment_note_text(conn, &state.client_id, &item)
            .map_err(|err| format!("{err:?}"))?
        else {
            continue;
        };
        let subject = CrmRecordEnrichmentSubject::new(draft.clone(), note_text, None);
        if enrichment_engine::EnrichmentSubject::plan(&subject).subject != adapter.subject_id {
            continue;
        }
        let actionable_field_refs = actionable_fields
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let last_accepted = crate::slices::enrichment::store::last_accepted_proposal_at_ms(
            conn,
            &state.client_id,
            adapter.slice_id,
            &draft.draft_id,
            adapter.subject_id,
            &actionable_field_refs,
        )
        .map_err(|err| err.to_string())?;
        if last_accepted.is_some_and(|at| at > now_ms.saturating_sub(stale_after_ms)) {
            continue;
        }
        let ctx = enrichment_engine::EnrichmentRunContext {
            slice_id: adapter.slice_id,
            actor_id: enrichment_engine::FRESHNESS_ACTOR,
            item: &item,
        };
        let run_id = enrichment_engine::planned_run_id_with_epoch(ctx, &subject, &epoch);
        if crate::slices::enrichment::store::run_exists(conn, &state.client_id, &run_id)
            .map_err(|err| err.to_string())?
        {
            continue;
        }
        out.push(enrichment_engine::FreshnessCandidate {
            slice_id: adapter.slice_id,
            subject_id: adapter.subject_id,
            draft_id: draft.draft_id.clone(),
            item_id: draft.item_id.clone(),
            run_id,
        });
    }
    Ok(out)
}

pub(crate) fn run_freshness_enrichment(
    state: &crate::http::AppState,
    candidate: &enrichment_engine::FreshnessCandidate,
    trigger_epoch: &str,
) -> enrichment_engine::EnrichmentOutcome {
    let loaded = {
        let persistence = state.persistence.lock();
        let conn = persistence.connection_ref();
        let draft = match load_staged_enrichment_draft(conn, &state.client_id, &candidate.draft_id)
        {
            Ok(draft) => draft,
            Err(err) => {
                tracing::info!(draft_id = %candidate.draft_id, error = ?err, "crm freshness candidate skipped");
                return enrichment_engine::EnrichmentOutcome {
                    run_id: candidate.run_id.clone(),
                    status: EnrichmentRunStatus::Skipped,
                    reason: "candidate_no_longer_staged".to_string(),
                };
            }
        };
        let Some(item) = crate::slices::work_queue::store::get_item_unscoped(
            conn,
            &state.client_id,
            &draft.item_id,
        )
        .ok()
        .flatten()
        .map(|entry| entry.item) else {
            return enrichment_engine::EnrichmentOutcome {
                run_id: candidate.run_id.clone(),
                status: EnrichmentRunStatus::Skipped,
                reason: "source_missing".to_string(),
            };
        };
        let note_text = match enrichment_note_text(conn, &state.client_id, &item) {
            Ok(Some(note_text)) => note_text,
            _ => {
                return enrichment_engine::EnrichmentOutcome {
                    run_id: candidate.run_id.clone(),
                    status: EnrichmentRunStatus::Skipped,
                    reason: "source_missing".to_string(),
                };
            }
        };
        (draft, item, note_text)
    };
    let (draft, item, note_text) = loaded;
    let subject = CrmRecordEnrichmentSubject::new(draft, note_text, None);
    enrichment_engine::run_with_trigger_epoch(
        state,
        enrichment_engine::EnrichmentRunContext {
            slice_id: candidate.slice_id,
            actor_id: enrichment_engine::FRESHNESS_ACTOR,
            item: &item,
        },
        &subject,
        trigger_epoch,
    )
}

/// The record-create kind's plug into the shared produce flow.
pub struct Produce;

impl crate::produce::ProduceFlavor for Produce {
    type Response = bos_contracts::crm_record_drafts::CrmRecordDraftProduceResponse;

    fn packet_kind(&self) -> &'static str {
        PACKET_KIND
    }

    fn purpose(&self) -> &'static str {
        FILL_PURPOSE
    }

    fn slice(&self) -> &'static str {
        "crm_record_drafts"
    }

    fn already_active_code(&self) -> &'static str {
        "crm_record_draft_already_active"
    }

    fn active_draft(
        &self,
        conn: &rusqlite::Connection,
        client_id: &str,
        item_id: &str,
    ) -> Result<Option<Self::Response>, crate::store_core::StoreError> {
        Ok(
            super::store::active_draft_for_item(conn, client_id, item_id)?.map(|draft| {
                bos_contracts::crm_record_drafts::CrmRecordDraftProduceResponse { draft }
            }),
        )
    }

    fn draft_attempts(
        &self,
        conn: &rusqlite::Connection,
        client_id: &str,
        item_id: &str,
    ) -> Result<u64, crate::store_core::StoreError> {
        super::store::count_drafts_for_item(conn, client_id, item_id)
    }

    fn prepare_context(
        &self,
        _conn: &rusqlite::Connection,
        _client_id: &str,
        _item: &WorkItem,
        _message: &InboundMessageRecord,
        _scope: &crate::http::OperatorScope,
        _actor_id: &str,
    ) -> Result<serde_json::Value, crate::store_core::StoreError> {
        Ok(serde_json::json!({}))
    }

    fn enrich_context_unlocked(&self, ctx: crate::produce::EnrichContext<'_>) -> serde_json::Value {
        let crate::produce::EnrichContext {
            state,
            item,
            message,
            scope,
            actor_id,
            actor_kind,
            mut context,
            attempt,
            now_ms,
        } = ctx;
        let Some(sender) = message.from_addr.as_deref() else {
            return context;
        };
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        let Ok(crm) = crate::slices::grounding::crm_contact_lookup(
            conn,
            &state.client_id,
            scope,
            Some(sender),
            None,
        ) else {
            return context;
        };
        let Some(text) = crate::slices::grounding::render_crm_contact(&crm) else {
            return context;
        };
        append_grounding_evidence(
            conn,
            &state.client_id,
            item,
            attempt,
            message,
            scope,
            actor_id,
            crate::slices::grounding::TOOL_CRM_CONTACT_LOOKUP,
            &serde_json::json!({ "email": sender }).to_string(),
            &format!("crm_contact:{sender}"),
            &text,
            actor_kind,
            now_ms,
        );
        if let Some(object) = context.as_object_mut() {
            object.insert(
                "crm_contact_lookup".to_string(),
                serde_json::to_value(crm).unwrap_or(serde_json::Value::Null),
            );
            object.insert(
                "grounding_text".to_string(),
                serde_json::Value::String(format!(
                    "Cached read-only CRM grounding. Use only contact and deal facts from this block; do not invent customer identity, pipeline stage, amounts, or dates.\n\n{}",
                    text
                )),
            );
        }
        context
    }

    fn build_request(
        &self,
        client_id: &str,
        item: &WorkItem,
        message: &InboundMessageRecord,
        context: &serde_json::Value,
        attempt: u64,
    ) -> TypedLlmTaskRequest {
        build_record_fill_request(client_id, item, message, context, attempt)
    }

    fn stage(
        &self,
        ctx: crate::produce::StageContext<'_>,
    ) -> Result<(), crate::store_core::StoreError> {
        let crate::produce::StageContext {
            conn,
            client_id,
            actor_id,
            item,
            message: _message,
            response,
            context,
            model,
            attempt,
            idempotency_key,
            now_ms,
        } = ctx;
        use crate::store_core::StoreError;
        let fill = match parse_record_fill_response(response) {
            Ok(fill) => fill,
            Err(parse_err) => {
                tracing::warn!(item_id = %item.item_id, error = %parse_err, "record fill unparseable");
                return Err(StoreError::Domain(
                    "crm_record_fill_invalid_response".to_string(),
                ));
            }
        };
        // Bounded LIVE search decides which referenced records already exist.
        let account_id =
            search_existing_records(fill.company_name.as_deref(), None, None).account_id;
        let contact_matches = fill
            .contacts
            .iter()
            .map(|contact| {
                let full_name = contact.contact_full_name();
                let matches = search_existing_records(
                    None,
                    contact.contact_email.as_deref(),
                    (!full_name.is_empty()).then_some(full_name.as_str()),
                );
                let cached_contact_id =
                    cached_crm_contact_id(context, contact.contact_email.as_deref());
                (contact.clone(), matches.contact_id.or(cached_contact_id))
            })
            .collect::<Vec<_>>();
        let drafts = drafts_from_fill(
            item,
            &fill,
            account_id,
            &contact_matches,
            attempt,
            model,
            now_ms,
        );
        if drafts.is_empty() {
            tracing::info!(item_id = %item.item_id, "crm records already exist - nothing to propose");
            return Ok(());
        }
        for (idx, draft) in drafts.iter().enumerate() {
            let child_key = format!("{idempotency_key}:crm_record:{idx}");
            super::store::insert_draft(conn, client_id, actor_id, draft, &child_key)?;
        }
        Ok(())
    }

    /// Website enrichment (Increment E): after a draft proposing a CREATE is
    /// staged, if the note LITERALLY named a domain, crawl that site (read-only,
    /// guarded), extract company/contact facts deterministically, gap-fill the
    /// rest with one bounded LLM transform, and prefill the draft's still-empty
    /// fields. Runs UNLOCKED (network + a second LLM call must not hold the
    /// persistence lock). Best-effort throughout — every failure only logs; the
    /// note-fill and any operator edit always win (store fills NULL columns
    /// only). Kill-switch: BOS_WEB_ENRICHMENT_ENABLED (default on).
    fn after_stage(&self, state: &crate::http::AppState, item: &WorkItem, _actor_id: &str) {
        // Load the just-staged drafts + the note text under the lock, then run
        // the network/LLM waterfall through the shared engine unlocked.
        let (drafts, note_text) = {
            let persistence = state.persistence.lock();
            let conn = persistence.connection_ref();
            let drafts =
                match super::store::list_drafts(conn, &state.client_id, Some(&item.item_id), 100) {
                    Ok(entries) => entries
                        .into_iter()
                        .map(|entry| entry.draft)
                        .filter(|draft| {
                            draft.status == CrmRecordDraftStatus::Staged
                                && (draft.create_company || draft.create_contact)
                        })
                        .collect::<Vec<_>>(),
                    Err(_) => return,
                };
            if drafts.is_empty() {
                return;
            }
            let note_text = match enrichment_note_text(conn, &state.client_id, item) {
                Ok(Some(note_text)) => note_text,
                _ => return,
            };
            (drafts, note_text)
        };

        for draft in drafts {
            run_record_enrichment(
                state,
                item,
                draft,
                note_text.clone(),
                WEB_ENRICHMENT_ACTOR,
                None,
                None,
            );
        }
    }
}

fn web_apply_diagnostics(
    apply: &WebEnrichmentApply,
    tier: EnrichmentTier,
    reason: &str,
) -> (Vec<EnrichmentTierEvent>, Vec<EnrichmentFieldProposal>) {
    let values = [
        ("company_name", &apply.company_name),
        ("company_website", &apply.company_website),
        ("company_phone", &apply.company_phone),
        ("company_address", &apply.company_address),
        ("company_description", &apply.company_description),
        ("contact_email", &apply.contact_email),
        ("contact_phone", &apply.contact_phone),
        ("contact_title", &apply.contact_title),
    ]
    .into_iter()
    .filter_map(|(field_id, value)| {
        value
            .as_ref()
            .map(|value| crate::slices::enrichment::web_tier::AcceptedValue {
                field_id,
                value: &value.value,
                quote: &value.provenance_quote,
                provenance_refs: vec![value.provenance_quote.clone()],
            })
    });
    crate::slices::enrichment::web_tier::accepted_value_diagnostics(values, tier, reason)
}

/// Assemble the operator-facing enrichment trace from the deterministic + AI
/// applies and the crawled pages. Bounded: a head preview of the model input,
/// not whole pages.
#[allow(clippy::too_many_arguments)]
fn build_enrichment_trace(
    domain: &str,
    draft: &CrmRecordDraft,
    pages: &[bos_integrations::web_page_read::FetchedPage],
    deterministic: &WebEnrichmentApply,
    ai: &WebEnrichmentApply,
    page_texts: &[bos_integrations::web_page_read::EnrichedPageText],
    search: Option<&bos_integrations::web_search_enrichment::SearchEvidence>,
    llm_ran: bool,
    now_ms: u64,
) -> bos_contracts::crm_record_drafts::CrmEnrichmentTrace {
    use bos_contracts::crm_record_drafts::CrmEnrichmentTraceItem;
    let mut items: Vec<CrmEnrichmentTraceItem> = Vec::new();
    let collect =
        |items: &mut Vec<CrmEnrichmentTraceItem>, apply: &WebEnrichmentApply, via: &str| {
            let mut push = |slot: &Option<EnrichedValue>, field: &str, previous: Option<&str>| {
                if let Some(value) = slot {
                    items.push(CrmEnrichmentTraceItem {
                        field: field.to_string(),
                        previous_value: previous.map(str::to_string),
                        value: value.value.clone(),
                        source: value.provenance_quote.clone(),
                        via: via.to_string(),
                    });
                }
            };
            push(
                &apply.company_name,
                "company_name",
                draft.company_name.as_deref(),
            );
            push(&apply.company_website, "company_website", None);
            push(&apply.company_phone, "company_phone", None);
            push(&apply.company_address, "company_address", None);
            push(&apply.company_description, "company_description", None);
            push(&apply.contact_email, "contact_email", None);
            push(&apply.contact_phone, "contact_phone", None);
            push(&apply.contact_title, "contact_title", None);
        };
    collect(&mut items, deterministic, "deterministic");
    collect(&mut items, ai, "ai");

    let page_concat = page_texts
        .iter()
        .map(|p| p.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    bos_contracts::crm_record_drafts::CrmEnrichmentTrace {
        captured_at_ms: now_ms,
        domain: domain.to_string(),
        pages: pages.iter().map(|p| p.url.clone()).collect(),
        items,
        llm_ran,
        llm_input_chars: page_concat.chars().count() as u32,
        llm_input_preview: page_concat.chars().take(1_500).collect(),
        search_ran: search.is_some_and(|s| s.search_was_attempted()),
        search_reason: search.map(|s| s.reason.clone()),
        search_queries: search.map(|s| s.queries.clone()).unwrap_or_default(),
        search_results: search
            .map(|s| {
                s.results
                    .iter()
                    .map(
                        |result| bos_contracts::crm_record_drafts::CrmSearchTraceResult {
                            query: result.query.clone(),
                            title: result.title.clone(),
                            url: result.url.clone(),
                            snippet: result.snippet.clone(),
                        },
                    )
                    .collect()
            })
            .unwrap_or_default(),
        failures: search.map(|s| s.failures.clone()).unwrap_or_default(),
        research_annotations: Vec::new(),
    }
}

/// Provider-agnostic validation of a draft before it can be approved: at least
/// one record proposed, and a non-empty name per proposed record. `Err` carries
/// the wire code (→ 422).
fn validate_proposed_records(draft: &CrmRecordDraft) -> Result<(), String> {
    if !draft.create_company && !draft.create_contact {
        return Err("crm_record_nothing_proposed".to_string());
    }
    if draft.create_company
        && draft
            .company_name
            .as_deref()
            .map(|n| n.trim().is_empty())
            .unwrap_or(true)
    {
        return Err("crm_record_company_name_required".to_string());
    }
    let has_contact_name = [
        draft.contact_first_name.as_deref(),
        draft.contact_last_name.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|p| !p.trim().is_empty());
    if draft.create_contact && !has_contact_name {
        return Err("crm_record_contact_name_required".to_string());
    }
    Ok(())
}

fn validate_provider_proposed_records(
    draft: &CrmRecordDraft,
    provider: &str,
) -> Result<(), String> {
    validate_proposed_records(draft)?;
    if provider == PROVIDER_ESPOCRM
        && draft.create_contact
        && draft
            .contact_last_name
            .as_deref()
            .map(|name| name.trim().is_empty())
            .unwrap_or(true)
    {
        return Err("crm_record_contact_last_name_required".to_string());
    }
    Ok(())
}

fn records_job(
    draft: &CrmRecordDraft,
    provider: &str,
    payload_json: String,
    idempotency_key: String,
) -> NewOutboxJob {
    NewOutboxJob {
        job_id: format!("obj_{}", draft.draft_id),
        provider: provider.to_string(),
        capability: CAPABILITY_CREATE_RECORDS.to_string(),
        payload_json,
        source_entity_kind: super::store::DRAFT_ENTITY_KIND.to_string(),
        source_entity_id: draft.draft_id.clone(),
        correlation_id: Some(draft.item_id.clone()),
        causation_id: None,
        idempotency_key,
    }
}

/// Build the create-records outbox job for an approved draft, for the
/// CONFIGURED CRM provider (EspoCRM or HubSpot — both run an ensure-chain that
/// creates the missing company/contact and links them). The store approve gate
/// runs this first; it returns the wire error code when no record is proposed or
/// a proposed record lacks a name.
pub fn build_approval_job(
    draft: &CrmRecordDraft,
    actor_id: &str,
    now_ms: u64,
    provider: &str,
) -> Result<NewOutboxJob, String> {
    validate_provider_proposed_records(draft, provider)?;
    let idempotency_key = format!("crmrecords:{}", draft.draft_id);
    let approval_id = format!("appr_{}", draft.draft_id);
    let approved_at = crate::produce::epoch_ms_to_rfc3339_utc(now_ms);
    let has_contact = draft.contact_first_name.is_some() || draft.contact_last_name.is_some();

    match provider {
        PROVIDER_HUBSPOT => {
            let company = draft
                .company_name
                .as_deref()
                .map(|name| HubSpotCompanyInput {
                    name: name.to_string(),
                    website: draft.company_website.clone(),
                    phone: draft.company_phone.clone(),
                    address: draft.company_address.clone(),
                    description: draft.company_description.clone(),
                });
            let contact = has_contact.then(|| HubSpotContactInput {
                first_name: draft.contact_first_name.clone(),
                last_name: draft.contact_last_name.clone(),
                email: draft.contact_email.clone(),
                phone: draft.contact_phone.clone(),
                title: draft.contact_title.clone(),
            });
            let payload = HubSpotRecordsCreateOutboxPayload {
                idempotency_key: idempotency_key.clone(),
                approval: HubSpotApprovalMetadata {
                    approval_id,
                    approved_by: actor_id.to_string(),
                    approved_at,
                },
                draft_ref: draft.draft_id.clone(),
                company,
                create_company: draft.create_company,
                contact,
                create_contact: draft.create_contact,
            };
            let payload_json = serde_json::to_string(&payload)
                .map_err(|err| format!("serialize outbox payload: {err}"))?;
            Ok(records_job(
                draft,
                PROVIDER_HUBSPOT,
                payload_json,
                idempotency_key,
            ))
        }
        PROVIDER_ESPOCRM => {
            let company = draft
                .company_name
                .as_deref()
                .map(|name| EspoCrmCompanyInput {
                    name: name.to_string(),
                    website: draft.company_website.clone(),
                    phone: draft.company_phone.clone(),
                    address: draft.company_address.clone(),
                    description: draft.company_description.clone(),
                });
            let contact = has_contact.then(|| EspoCrmContactInput {
                first_name: draft.contact_first_name.clone(),
                last_name: draft.contact_last_name.clone(),
                email: draft.contact_email.clone(),
                phone: draft.contact_phone.clone(),
                title: draft.contact_title.clone(),
            });
            let payload = EspoCrmRecordsCreateOutboxPayload {
                idempotency_key: idempotency_key.clone(),
                approval: EspoCrmApprovalMetadata {
                    approval_id,
                    approved_by: actor_id.to_string(),
                    approved_at,
                },
                draft_ref: draft.draft_id.clone(),
                company,
                create_company: draft.create_company,
                contact,
                create_contact: draft.create_contact,
            };
            let payload_json = serde_json::to_string(&payload)
                .map_err(|err| format!("serialize outbox payload: {err}"))?;
            Ok(records_job(
                draft,
                PROVIDER_ESPOCRM,
                payload_json,
                idempotency_key,
            ))
        }
        other => Err(format!("crm_provider_unsupported:{other}")),
    }
}

/// Validate + sanitize an operator edit of the proposed-record set. Trims,
/// nulls empties, and requires a non-empty name per proposed record and at
/// least one proposed record. `Err` carries the wire code (→ 422).
pub fn sanitize_record_edit(
    request: &bos_contracts::crm_record_drafts::CrmRecordDraftUpdateRequest,
) -> Result<RecordEdit, &'static str> {
    let clean = |v: &Option<String>, max: usize| -> Option<String> {
        v.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.chars().take(max).collect())
    };
    let company_name = clean(&request.company_name, 200);
    let contact_first_name = clean(&request.contact_first_name, 100);
    let contact_last_name = clean(&request.contact_last_name, 100);
    let contact_email = clean(&request.contact_email, 200);
    if let Some(email) = contact_email.as_deref() {
        if !email.contains('@') || email.contains(char::is_whitespace) {
            return Err("crm_record_contact_email_invalid");
        }
    }
    if !request.create_company && !request.create_contact {
        return Err("crm_record_nothing_proposed");
    }
    if request.create_company && company_name.is_none() {
        return Err("crm_record_company_name_required");
    }
    if request.create_contact && contact_first_name.is_none() && contact_last_name.is_none() {
        return Err("crm_record_contact_name_required");
    }
    Ok(RecordEdit {
        create_company: request.create_company,
        company_name,
        company_website: clean(&request.company_website, 300),
        company_phone: clean(&request.company_phone, 50),
        company_address: clean(&request.company_address, 300),
        company_description: clean(&request.company_description, 1000),
        create_contact: request.create_contact,
        contact_first_name,
        contact_last_name,
        contact_email,
        contact_phone: clean(&request.contact_phone, 50),
        contact_title: clean(&request.contact_title, 150),
    })
}

/// EspoCRM create-records delivery executor for the spine outbox pump (the
/// records arm of the espocrm provider dispatch). Env read here; the client is
/// env-free. Rides the same BOS_ESPOCRM_WRITE_ENABLED gate as every Espo write.
pub fn deliver_espocrm_records(
    state: &crate::http::AppState,
    job: &ClaimedJob,
    now_ms: u64,
) -> AttemptOutcome {
    let write_enabled = {
        let persistence = state.persistence.lock();
        crate::slices::admin_settings::service::flag(
            persistence.connection_ref(),
            &state.client_id,
            &env_registry::BOS_ESPOCRM_WRITE_ENABLED,
        )
        .unwrap_or_else(|err| {
            tracing::warn!(error = %err, "espocrm records write gate read failed");
            false
        })
    };
    let config = EspoCrmWriteConfig {
        base_url: env_registry::string(&env_registry::BOS_ESPOCRM_BASE_URL),
        api_key: env_registry::string(&env_registry::BOS_ESPOCRM_API_KEY),
        write_enabled,
    };
    execute_espocrm_records_job(job, &config, now_ms)
}

pub fn execute_espocrm_records_job(
    job: &ClaimedJob,
    config: &EspoCrmWriteConfig,
    now_ms: u64,
) -> AttemptOutcome {
    if job.provider != PROVIDER_ESPOCRM || job.capability != CAPABILITY_CREATE_RECORDS {
        return AttemptOutcome::Terminal {
            error: format!("outbox_unsupported_job:{}:{}", job.provider, job.capability),
            result_json: None,
        };
    }
    let payload = match serde_json::from_str::<EspoCrmRecordsCreateOutboxPayload>(&job.payload_json)
    {
        Ok(payload) => payload,
        Err(err) => {
            return AttemptOutcome::Terminal {
                error: format!("espocrm_records_payload_invalid:{err}"),
                result_json: None,
            }
        }
    };
    let client = espocrm_records_execution_client(config);
    match client.create_records(&payload) {
        Ok(response) => AttemptOutcome::Delivered {
            result_json: serde_json::json!({
                "dry_run": response.status.dry_run,
                "provider_status": response.status.reason,
                "account_id": response.account_id,
                "contact_id": response.contact_id,
            })
            .to_string(),
        },
        Err(EspoCrmWriteError::Retryable { code, .. }) => AttemptOutcome::Retry {
            error: code,
            retry_at_ms: now_ms + retry_backoff_ms(job.attempts),
        },
        Err(EspoCrmWriteError::Permanent { code, message }) => AttemptOutcome::Terminal {
            error: provider_error_detail(&code, &message),
            result_json: Some(serde_json::json!({ "message": message }).to_string()),
        },
    }
}

/// HubSpot create-records delivery executor (the records arm of the hubspot
/// provider dispatch). Rides the BOS_HUBSPOT_WRITE_ENABLED gate.
pub fn deliver_hubspot_records(
    state: &crate::http::AppState,
    job: &ClaimedJob,
    now_ms: u64,
) -> AttemptOutcome {
    let write_enabled = {
        let persistence = state.persistence.lock();
        crate::slices::admin_settings::service::flag(
            persistence.connection_ref(),
            &state.client_id,
            &env_registry::BOS_HUBSPOT_WRITE_ENABLED,
        )
        .unwrap_or_else(|err| {
            tracing::warn!(error = %err, "hubspot records write gate read failed");
            false
        })
    };
    let config = HubSpotWriteConfig {
        access_token: env_registry::string(&env_registry::BOS_HUBSPOT_ACCESS_TOKEN),
        write_enabled,
    };
    execute_hubspot_records_job(job, &config, now_ms)
}

pub fn execute_hubspot_records_job(
    job: &ClaimedJob,
    config: &HubSpotWriteConfig,
    now_ms: u64,
) -> AttemptOutcome {
    if job.provider != PROVIDER_HUBSPOT || job.capability != CAPABILITY_CREATE_RECORDS {
        return AttemptOutcome::Terminal {
            error: format!("outbox_unsupported_job:{}:{}", job.provider, job.capability),
            result_json: None,
        };
    }
    let payload = match serde_json::from_str::<HubSpotRecordsCreateOutboxPayload>(&job.payload_json)
    {
        Ok(payload) => payload,
        Err(err) => {
            return AttemptOutcome::Terminal {
                error: format!("hubspot_records_payload_invalid:{err}"),
                result_json: None,
            }
        }
    };
    let client = hubspot_records_execution_client(config);
    match client.create_records(&payload) {
        Ok(response) => AttemptOutcome::Delivered {
            result_json: serde_json::json!({
                "dry_run": response.status.dry_run,
                "provider_status": response.status.reason,
                "account_id": response.company_id,
                "contact_id": response.contact_id,
            })
            .to_string(),
        },
        Err(HubSpotWriteError::Retryable { code, .. }) => AttemptOutcome::Retry {
            error: code,
            retry_at_ms: now_ms + retry_backoff_ms(job.attempts),
        },
        Err(HubSpotWriteError::Permanent { code, message }) => AttemptOutcome::Terminal {
            error: provider_error_detail(&code, &message),
            result_json: Some(serde_json::json!({ "message": message }).to_string()),
        },
    }
}

fn string_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|raw| !raw.is_empty() && *raw != "null")
        .map(str::to_string)
}
