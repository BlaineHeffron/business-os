//! Reusable enrichment waterfall runner. The engine owns enrichment-run
//! diagnostics; subject slices own provider reads and final draft grafts.

use bos_contracts::calendar_drafts::DraftFieldProvenance;
use bos_contracts::enrichment::{
    EnrichmentConfidence, EnrichmentEligibility, EnrichmentFieldProposal, EnrichmentFieldSpec,
    EnrichmentPlan, EnrichmentRunStatus, EnrichmentTier, EnrichmentTierEvent,
};
use bos_contracts::work_queue::WorkItem;
use bos_integrations::llm_typed_tasks::TypedLlmTaskRequest;
use bos_integrations::web_page_read::{EnrichedPageText, FetchedPage, WebEnrichment};
use bos_integrations::web_search_enrichment::SearchEvidence;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::ops::ControlFlow;

use crate::env_registry;
use crate::http::AppState;

pub(crate) const VALUE_KIND_NAME: &str = "name";
pub(crate) const VALUE_KIND_DOMAIN: &str = "domain";
pub(crate) const VALUE_KIND_EMAIL: &str = "email";
pub(crate) const VALUE_KIND_PHONE: &str = "phone";
pub(crate) const VALUE_KIND_ADDRESS: &str = "address";
pub(crate) const VALUE_KIND_DESCRIPTION: &str = "description";
pub const RESEARCH_ACTION_SCHEMA_REF: &str = "bos.enrichment.research_action.v1";
pub(crate) const RESEARCH_ACTION_PURPOSE: &str = "enrichment_research_action";

pub(crate) const SENSITIVITY_COMPANY_SAFE: &str = "company_safe";
pub(crate) const SENSITIVITY_PERSON_SENSITIVE: &str = "person_sensitive";
pub(crate) const FRESHNESS_ACTOR: &str = "enrichment_freshness";

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ShapeEnrichmentContract {
    pub subject: String,
    pub target_shape: Value,
    pub current_values: Value,
    pub eligible_fields: Vec<String>,
    pub context: Value,
    pub guidance: String,
}

pub(crate) fn shape_enrichment_input(contract: ShapeEnrichmentContract) -> Value {
    json!({
        "instructions": format!(
            "You are reading curated evidence to enrich a typed draft. The subject names the draft being enriched. The full target_shape shows every output field; current_values shows what is already known; eligible_fields is the ONLY set you may fill. Return one JSON object: {{\"confidence\":\"high|medium|low\",\"fields\":{{\"field_id\":{{\"value\":\"...\",\"quote\":\"literal evidence span\"}}}}}}. You may include any target_shape field in fields only when it is listed in eligible_fields. Omit fields you cannot ground. GROUNDING: every returned field needs a quote that is a LITERAL span from the evidence, and the quote must support the returned value. Do NOT invent or guess. {}",
            contract.guidance
        ),
        "subject": contract.subject,
        "target_shape": contract.target_shape,
        "current_values": contract.current_values,
        "eligible_fields": contract.eligible_fields,
        "context": contract.context,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ValueKindRegistration {
    pub value_kind: &'static str,
    pub sensitivity: &'static str,
    pub default_confidence: EnrichmentConfidence,
    pub research_comparator: ResearchValueComparator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResearchValueComparator {
    CanonicalContains,
    Domain,
    Email,
    Phone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProviderRegistration {
    pub provider_id: &'static str,
    pub tier: EnrichmentTier,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SubjectRegistration {
    pub subject_id: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FreshnessCandidate {
    pub slice_id: &'static str,
    pub subject_id: &'static str,
    pub draft_id: String,
    pub item_id: String,
    pub run_id: String,
}

pub(crate) type FreshnessCandidateCollector = fn(
    &AppState,
    &FreshnessAdapterRegistration,
    u64,
    u64,
    usize,
) -> Result<Vec<FreshnessCandidate>, String>;

pub(crate) type FreshnessCandidateRunner =
    fn(&AppState, &FreshnessCandidate, &str) -> EnrichmentOutcome;

#[derive(Debug, Clone, Copy)]
pub(crate) struct FreshnessAdapterRegistration {
    pub slice_id: &'static str,
    pub subject_id: &'static str,
    pub critical_fields: &'static [&'static str],
    pub collect_candidates: FreshnessCandidateCollector,
    pub run_candidate: FreshnessCandidateRunner,
}

pub(crate) fn registered_value_kinds() -> &'static [ValueKindRegistration] {
    &[
        ValueKindRegistration {
            value_kind: VALUE_KIND_NAME,
            sensitivity: SENSITIVITY_COMPANY_SAFE,
            default_confidence: EnrichmentConfidence::Medium,
            research_comparator: ResearchValueComparator::CanonicalContains,
        },
        ValueKindRegistration {
            value_kind: VALUE_KIND_DOMAIN,
            sensitivity: SENSITIVITY_COMPANY_SAFE,
            default_confidence: EnrichmentConfidence::Medium,
            research_comparator: ResearchValueComparator::Domain,
        },
        ValueKindRegistration {
            value_kind: VALUE_KIND_EMAIL,
            sensitivity: SENSITIVITY_PERSON_SENSITIVE,
            default_confidence: EnrichmentConfidence::Medium,
            research_comparator: ResearchValueComparator::Email,
        },
        ValueKindRegistration {
            value_kind: VALUE_KIND_PHONE,
            sensitivity: SENSITIVITY_PERSON_SENSITIVE,
            default_confidence: EnrichmentConfidence::Medium,
            research_comparator: ResearchValueComparator::Phone,
        },
        ValueKindRegistration {
            value_kind: VALUE_KIND_ADDRESS,
            sensitivity: SENSITIVITY_COMPANY_SAFE,
            default_confidence: EnrichmentConfidence::Medium,
            research_comparator: ResearchValueComparator::CanonicalContains,
        },
        ValueKindRegistration {
            value_kind: VALUE_KIND_DESCRIPTION,
            sensitivity: SENSITIVITY_COMPANY_SAFE,
            default_confidence: EnrichmentConfidence::Medium,
            research_comparator: ResearchValueComparator::CanonicalContains,
        },
    ]
}

pub(crate) fn registered_providers() -> &'static [ProviderRegistration] {
    &[
        ProviderRegistration {
            provider_id: "guarded_crawl",
            tier: EnrichmentTier::WebSearch,
        },
        ProviderRegistration {
            provider_id: "agentic_web_research",
            tier: EnrichmentTier::Research,
        },
        ProviderRegistration {
            provider_id: "web_search",
            tier: EnrichmentTier::WebSearch,
        },
    ]
}

pub(crate) fn registered_subjects() -> &'static [SubjectRegistration] {
    &[
        SubjectRegistration {
            subject_id: "crm_record_company",
        },
        SubjectRegistration {
            subject_id: "crm_record_contact",
        },
        SubjectRegistration {
            subject_id: "content_company_facts",
        },
        SubjectRegistration {
            subject_id: "invoice_customer",
        },
    ]
}

pub(crate) fn registered_freshness_adapters() -> &'static [FreshnessAdapterRegistration] {
    &[
        FreshnessAdapterRegistration {
            slice_id: "crm_record_drafts",
            subject_id: "crm_record_company",
            critical_fields: &[
                "company_name",
                "company_website",
                "company_phone",
                "company_address",
            ],
            collect_candidates: crate::slices::crm_record_drafts::service::freshness_candidates,
            run_candidate: crate::slices::crm_record_drafts::service::run_freshness_enrichment,
        },
        FreshnessAdapterRegistration {
            slice_id: "crm_record_drafts",
            subject_id: "crm_record_contact",
            critical_fields: &[
                "company_name",
                "company_website",
                "company_phone",
                "company_address",
                "contact_email",
                "contact_phone",
            ],
            collect_candidates: crate::slices::crm_record_drafts::service::freshness_candidates,
            run_candidate: crate::slices::crm_record_drafts::service::run_freshness_enrichment,
        },
        FreshnessAdapterRegistration {
            slice_id: "invoice_drafts",
            subject_id: "invoice_customer",
            critical_fields: &["customer_name", "customer_email"],
            collect_candidates: crate::slices::invoice_drafts::service::freshness_candidates,
            run_candidate: crate::slices::invoice_drafts::service::run_freshness_enrichment,
        },
    ]
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct EnrichmentRunContext<'a> {
    pub slice_id: &'a str,
    pub actor_id: &'a str,
    pub item: &'a WorkItem,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EnrichmentOutcome {
    pub run_id: String,
    pub status: EnrichmentRunStatus,
    pub reason: String,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct EnrichmentRunHandle<'a> {
    run_id: &'a str,
    ctx: EnrichmentRunContext<'a>,
    plan: &'a EnrichmentPlan,
}

impl<'a> EnrichmentRunHandle<'a> {
    pub(crate) fn run_id(&self) -> &str {
        self.run_id
    }

    pub(crate) fn append(
        &self,
        state: &AppState,
        event_seq: &str,
        diagnostics: &[EnrichmentTierEvent],
        proposals: &[EnrichmentFieldProposal],
        cost_micros: u64,
    ) {
        trace_value_kind_acceptance(self.plan, diagnostics, proposals);
        // Record the gate's `would_reject` verdicts as durable diagnostics so they
        // are reviewable in /api/enrichment/runs, not just in tracing logs. This is
        // still NON-enforcing: proposals and grafts are untouched.
        let would_reject = value_kind_would_reject_events(self.plan, diagnostics, proposals);
        if would_reject.is_empty() {
            append_enrichment_diagnostics(
                state,
                self.ctx,
                self.run_id,
                event_seq,
                diagnostics,
                proposals,
                cost_micros,
            );
        } else {
            let mut combined = diagnostics.to_vec();
            combined.extend(would_reject);
            append_enrichment_diagnostics(
                state,
                self.ctx,
                self.run_id,
                event_seq,
                &combined,
                proposals,
                cost_micros,
            );
        }
    }

    pub(crate) fn transition(
        &self,
        state: &AppState,
        status: EnrichmentRunStatus,
        reason: &str,
    ) -> EnrichmentOutcome {
        transition_enrichment_run(state, self.ctx, self.run_id, status, reason);
        EnrichmentOutcome {
            run_id: self.run_id.to_string(),
            status,
            reason: reason.to_string(),
        }
    }
}

pub(crate) trait EnrichmentSubject {
    fn draft_id(&self) -> &str;
    fn item_id(&self) -> &str;
    fn plan(&self) -> EnrichmentPlan;
    fn tier1_events(&self) -> (Vec<EnrichmentTierEvent>, Vec<EnrichmentFieldProposal>);
    fn literal_domain(&self) -> Option<String>;
    fn run_web_search_tier(
        &self,
        state: &AppState,
        ctx: EnrichmentRunContext<'_>,
        run: EnrichmentRunHandle<'_>,
        domain: &str,
    ) -> EnrichmentOutcome;
}

pub(crate) struct WebEnrichmentFinalizeInputs<Apply> {
    pub apply: Apply,
    pub llm_apply: Apply,
    pub deterministic: Apply,
    pub pages: Vec<FetchedPage>,
    pub page_texts: Vec<EnrichedPageText>,
    pub search_evidence: Option<SearchEvidence>,
    pub llm_ran: bool,
    pub domain: String,
}

pub(crate) trait EnrichableDraft {
    type Apply: Clone + Default;

    fn deterministic_apply(&self, enrich: &WebEnrichment) -> Self::Apply;
    fn apply_is_empty(&self, apply: &Self::Apply) -> bool;
    fn missing_fields(&self, apply: &Self::Apply) -> Vec<String>;
    fn build_request(
        &self,
        client_id: &str,
        item: &WorkItem,
        missing_fields: &[String],
        page_texts: &[EnrichedPageText],
    ) -> TypedLlmTaskRequest;
    fn parse_response(
        &self,
        response: &serde_json::Value,
        page_text: &str,
        missing_fields: &[String],
    ) -> Self::Apply;
    fn parse_response_with_diagnostics(
        &self,
        response: &serde_json::Value,
        page_text: &str,
        missing_fields: &[String],
        tier: EnrichmentTier,
        reason: &str,
    ) -> (
        Self::Apply,
        Vec<EnrichmentTierEvent>,
        Vec<EnrichmentFieldProposal>,
    ) {
        let apply = self.parse_response(response, page_text, missing_fields);
        let (events, proposals) = self.apply_diagnostics(&apply, tier, reason);
        (apply, events, proposals)
    }
    fn merge_apply(&self, apply: &mut Self::Apply, patch: Self::Apply);
    fn apply_diagnostics(
        &self,
        apply: &Self::Apply,
        tier: EnrichmentTier,
        reason: &str,
    ) -> (Vec<EnrichmentTierEvent>, Vec<EnrichmentFieldProposal>);
    fn search_trigger_reason(&self, apply: &Self::Apply) -> Option<&'static str>;
    fn search_queries(&self, domain: &str) -> Vec<String>;
    fn search_fields(&self, apply: &Self::Apply) -> Vec<String>;
    fn purpose(&self) -> &'static str;
    fn slice_id(&self) -> &'static str;
    fn max_text_chars(&self) -> usize;
    fn gap_fill_log_message(&self) -> &'static str;
    fn search_gap_fill_log_message(&self) -> &'static str;
    fn finalize_web_enrichment(
        &self,
        state: &AppState,
        ctx: EnrichmentRunContext<'_>,
        run: EnrichmentRunHandle<'_>,
        inputs: WebEnrichmentFinalizeInputs<Self::Apply>,
    ) -> EnrichmentOutcome;
}

pub(crate) fn run_web_search_tier<E>(
    subject: &E,
    state: &AppState,
    ctx: EnrichmentRunContext<'_>,
    run: EnrichmentRunHandle<'_>,
    domain: &str,
) -> EnrichmentOutcome
where
    E: EnrichableDraft,
{
    let pages = match crate::slices::enrichment::web_tier::crawl_outcome(
        state,
        run,
        domain,
        crate::slices::enrichment::web_tier::live_guarded_crawl(domain),
    ) {
        ControlFlow::Continue(pages) => pages,
        ControlFlow::Break(outcome) => return outcome,
    };

    let enrich =
        bos_integrations::web_page_read::extract_enrichment(&pages, subject.max_text_chars());
    let deterministic = subject.deterministic_apply(&enrich);
    let (events, proposals) =
        subject.apply_diagnostics(&deterministic, EnrichmentTier::WebSearch, "deterministic");
    run.append(state, "tier3-deterministic", &events, &proposals, 0);
    let mut apply = deterministic.clone();
    let missing = subject.missing_fields(&apply);
    let mut llm_apply = E::Apply::default();
    let mut llm_ran = false;
    if !missing.is_empty() && !enrich.page_texts.is_empty() {
        let request =
            subject.build_request(&state.client_id, ctx.item, &missing, &enrich.page_texts);
        match crate::slices::ai_usage::service::execute_recorded(
            state.persistence.clone(),
            &state.client_id,
            subject.purpose(),
            &request,
        ) {
            Ok(envelope) => {
                let page_text = enrich
                    .page_texts
                    .iter()
                    .map(|p| p.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                llm_ran = true;
                let (parsed_apply, events, proposals) = subject.parse_response_with_diagnostics(
                    &envelope.response_json,
                    &page_text,
                    &missing,
                    EnrichmentTier::WebSearch,
                    "ai",
                );
                llm_apply = parsed_apply;
                subject.merge_apply(&mut apply, llm_apply.clone());
                run.append(state, "tier3-ai", &events, &proposals, 0);
            }
            Err(err) => {
                tracing::info!(
                    item_id = %ctx.item.item_id,
                    error = ?err,
                    "{}",
                    subject.gap_fill_log_message()
                );
                let events = vec![skip_event(
                    EnrichmentTier::WebSearch,
                    "failure",
                    &format!("web_gap_fill_failed:{err:?}"),
                )];
                run.append(state, "tier3-ai-failed", &events, &[], 0);
            }
        }
    }
    let mut search_evidence: Option<SearchEvidence> = None;
    if let Some(reason) = subject.search_trigger_reason(&apply) {
        let queries = subject.search_queries(domain);
        let evidence = crate::slices::enrichment::web_tier::collect_search_evidence(
            subject.slice_id(),
            reason,
            &queries,
            env_registry::web_search_enrichment_config(),
        );
        let search_events = crate::slices::enrichment::web_tier::search_evidence_events(&evidence);
        run.append(state, "tier3-search-collect", &search_events, &[], 0);
        let evidence_text = evidence.text_for_llm(24 * 1024);
        if !evidence_text.is_empty() {
            let search_fields = subject.search_fields(&apply);
            let search_pages = vec![EnrichedPageText {
                url: "search:evidence".to_string(),
                text: evidence_text.clone(),
            }];
            let request =
                subject.build_request(&state.client_id, ctx.item, &search_fields, &search_pages);
            match crate::slices::ai_usage::service::execute_recorded(
                state.persistence.clone(),
                &state.client_id,
                subject.purpose(),
                &request,
            ) {
                Ok(envelope) => {
                    llm_ran = true;
                    let (search_apply, events, proposals) = subject
                        .parse_response_with_diagnostics(
                            &envelope.response_json,
                            &evidence_text,
                            &search_fields,
                            EnrichmentTier::WebSearch,
                            "search_ai",
                        );
                    subject.merge_apply(&mut apply, search_apply.clone());
                    subject.merge_apply(&mut llm_apply, search_apply.clone());
                    run.append(state, "tier3-search-ai", &events, &proposals, 0);
                }
                Err(err) => {
                    tracing::info!(
                        item_id = %ctx.item.item_id,
                        error = ?err,
                        "{}",
                        subject.search_gap_fill_log_message()
                    );
                    let events = vec![skip_event(
                        EnrichmentTier::WebSearch,
                        "failure",
                        &format!("web_search_gap_fill_failed:{err:?}"),
                    )];
                    run.append(state, "tier3-search-ai-failed", &events, &[], 0);
                }
            }
        }
        search_evidence = Some(evidence);
    }

    subject.finalize_web_enrichment(
        state,
        ctx,
        run,
        WebEnrichmentFinalizeInputs {
            apply,
            llm_apply,
            deterministic,
            pages,
            page_texts: enrich.page_texts,
            search_evidence,
            llm_ran,
            domain: domain.to_string(),
        },
    )
}

pub(crate) fn planned_run_id<S>(ctx: EnrichmentRunContext<'_>, subject: &S) -> String
where
    S: EnrichmentSubject,
{
    let plan = subject.plan();
    planned_run_id_for_plan(
        ctx.slice_id,
        subject.draft_id(),
        subject.item_id(),
        &plan,
        None,
    )
}

pub(crate) fn planned_run_id_with_epoch<S>(
    ctx: EnrichmentRunContext<'_>,
    subject: &S,
    trigger_epoch: &str,
) -> String
where
    S: EnrichmentSubject,
{
    let plan = subject.plan();
    planned_run_id_for_plan(
        ctx.slice_id,
        subject.draft_id(),
        subject.item_id(),
        &plan,
        Some(trigger_epoch),
    )
}

pub(crate) fn planned_run_id_with_runtime_fingerprint<S>(
    ctx: EnrichmentRunContext<'_>,
    subject: &S,
    mode: bos_contracts::enrichment::EnrichmentMode,
    unresolved_field_ids: &[String],
) -> String
where
    S: EnrichmentSubject,
{
    let plan = subject.plan();
    let mut unresolved_field_ids = unresolved_field_ids.to_vec();
    unresolved_field_ids.sort();
    unresolved_field_ids.dedup();
    let runtime_fingerprint = format!(
        "mode={mode:?};unresolved={}",
        unresolved_field_ids.join(",")
    );
    planned_run_id_for_plan(
        ctx.slice_id,
        subject.draft_id(),
        subject.item_id(),
        &plan,
        Some(&runtime_fingerprint),
    )
}

pub(crate) fn freshness_epoch(stale_after_ms: u64, now_ms: u64) -> String {
    let bucket = now_ms / stale_after_ms.max(1);
    format!("freshness:{stale_after_ms}:{bucket}")
}

pub(crate) fn run<S>(
    state: &AppState,
    ctx: EnrichmentRunContext<'_>,
    subject: &S,
) -> EnrichmentOutcome
where
    S: EnrichmentSubject,
{
    run_inner(state, ctx, subject, None)
}

pub(crate) fn run_with_trigger_epoch<S>(
    state: &AppState,
    ctx: EnrichmentRunContext<'_>,
    subject: &S,
    trigger_epoch: &str,
) -> EnrichmentOutcome
where
    S: EnrichmentSubject,
{
    run_inner(state, ctx, subject, Some(trigger_epoch))
}

fn run_inner<S>(
    state: &AppState,
    ctx: EnrichmentRunContext<'_>,
    subject: &S,
    trigger_epoch: Option<&str>,
) -> EnrichmentOutcome
where
    S: EnrichmentSubject,
{
    let plan = subject.plan();
    validate_plan_against_registries(&plan);
    let run_id = planned_run_id_for_plan(
        ctx.slice_id,
        subject.draft_id(),
        subject.item_id(),
        &plan,
        trigger_epoch,
    );
    start_enrichment_run(state, ctx, &run_id, subject, &plan);
    let handle = EnrichmentRunHandle {
        run_id: &run_id,
        ctx,
        plan: &plan,
    };
    let (tier1_diagnostics, tier1_proposals) = subject.tier1_events();
    handle.append(state, "tier1", &tier1_diagnostics, &tier1_proposals, 0);

    if !plan.enabled_tiers.contains(&EnrichmentTier::WebSearch) {
        let events = vec![skip_event(
            EnrichmentTier::WebSearch,
            "tier_skipped",
            "web_search_tier_disabled_by_plan",
        )];
        handle.append(state, "tier3-disabled-by-plan", &events, &[], 0);
        return handle.transition(
            state,
            EnrichmentRunStatus::Skipped,
            "web_search_tier_disabled_by_plan",
        );
    }

    let web_enrichment_enabled = {
        let persistence = state.persistence.lock();
        crate::slices::admin_settings::service::flag(
            persistence.connection_ref(),
            &state.client_id,
            &env_registry::BOS_WEB_ENRICHMENT_ENABLED,
        )
        .unwrap_or(false)
    };
    if !web_enrichment_enabled {
        let events = vec![skip_event(
            EnrichmentTier::WebSearch,
            "tier_skipped",
            "web_enrichment_disabled",
        )];
        handle.append(state, "tier3-disabled", &events, &[], 0);
        return handle.transition(
            state,
            EnrichmentRunStatus::Skipped,
            "web_enrichment_disabled",
        );
    }

    let Some(domain) = subject.literal_domain() else {
        let events = vec![skip_event(
            EnrichmentTier::WebSearch,
            "tier_skipped",
            "no_literal_domain",
        )];
        handle.append(state, "tier3-no-domain", &events, &[], 0);
        return handle.transition(state, EnrichmentRunStatus::Skipped, "no_literal_domain");
    };

    subject.run_web_search_tier(state, ctx, handle, &domain)
}

pub(crate) fn field_event(
    tier: EnrichmentTier,
    field_id: &str,
    value: &str,
    status: &str,
    reason: &str,
    quote: Option<&str>,
) -> EnrichmentTierEvent {
    EnrichmentTierEvent {
        event_type: "field_proposal".to_string(),
        tier,
        field_id: Some(field_id.to_string()),
        status: Some(status.to_string()),
        reason: Some(reason.to_string()),
        source_id: None,
        url: None,
        final_url: None,
        query: None,
        rank: None,
        title: None,
        snippet: None,
        proposed_value: Some(value.to_string()),
        confidence: Some(match tier {
            EnrichmentTier::Local => EnrichmentConfidence::High,
            _ => EnrichmentConfidence::Medium,
        }),
        quote: quote.map(str::to_string),
        latency_ms: None,
        bytes: None,
        cost_micros: None,
        ..Default::default()
    }
}

pub(crate) fn field_spec(
    field_id: &str,
    value_kind: &str,
    eligibility: EnrichmentEligibility,
    min_confidence: EnrichmentConfidence,
) -> EnrichmentFieldSpec {
    EnrichmentFieldSpec {
        field_id: field_id.to_string(),
        value_kind: value_kind.to_string(),
        eligibility,
        min_confidence,
        provenance_required: true,
        operator_override: true,
    }
}

pub(crate) fn source_evidence_event(source_id: &str, reason: &str) -> EnrichmentTierEvent {
    EnrichmentTierEvent {
        event_type: "source_evidence".to_string(),
        tier: EnrichmentTier::Local,
        field_id: None,
        status: Some("available".to_string()),
        reason: Some(reason.to_string()),
        source_id: Some(source_id.to_string()),
        url: None,
        final_url: None,
        query: None,
        rank: None,
        title: None,
        snippet: None,
        proposed_value: None,
        confidence: Some(EnrichmentConfidence::High),
        quote: None,
        latency_ms: None,
        bytes: None,
        cost_micros: None,
        ..Default::default()
    }
}

pub(crate) fn existing_prefill_events<'a, I>(
    source_id: &str,
    fields: I,
) -> (Vec<EnrichmentTierEvent>, Vec<EnrichmentFieldProposal>)
where
    I: IntoIterator<Item = (&'a str, Option<&'a str>)>,
{
    let mut events = Vec::new();
    let mut proposals = Vec::new();
    for (field_id, value) in fields {
        let Some(value) = value.map(str::trim).filter(|v| !v.is_empty()) else {
            continue;
        };
        events.push(field_event(
            EnrichmentTier::Local,
            field_id,
            value,
            "accepted",
            "existing_draft_prefill",
            None,
        ));
        proposals.push(EnrichmentFieldProposal {
            field_id: field_id.to_string(),
            proposed_value: value.to_string(),
            source_tier: EnrichmentTier::Local,
            confidence: EnrichmentConfidence::High,
            provenance_refs: vec![source_id.to_string()],
            accepted: true,
            reason: "existing_draft_prefill".to_string(),
        });
    }
    (events, proposals)
}

pub(crate) fn literal_span_in_text(text: &str, quote: &str) -> bool {
    let quote = quote.trim();
    if quote.is_empty() {
        return false;
    }
    let text_lower = text.to_lowercase();
    let quote_lower = quote.to_lowercase();
    text_lower.contains(&quote_lower)
        || normalize_grounding_whitespace(&text_lower)
            .contains(&normalize_grounding_whitespace(&quote_lower))
}

pub(crate) fn quote_contains_value(quote: &str, value: &str) -> bool {
    let value = value.trim();
    if value.is_empty() {
        return false;
    }
    let quote_lower = quote.to_lowercase();
    let value_lower = value.to_lowercase();
    if quote_lower.contains(&value_lower) {
        return true;
    }
    let quote_tokens = normalized_grounding_tokens(quote);
    let value_tokens = normalized_grounding_tokens(value);
    if value_tokens.is_empty() {
        return false;
    }
    let matched = value_tokens
        .iter()
        .filter(|token| quote_tokens.iter().any(|quote_token| quote_token == *token))
        .count();
    matched == value_tokens.len() || (value_tokens.len() >= 4 && matched + 1 >= value_tokens.len())
}

fn normalized_grounding_tokens(value: &str) -> Vec<String> {
    value
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter_map(|token| {
            let token = token.trim().to_ascii_lowercase();
            (token.len() >= 2).then_some(token)
        })
        .collect()
}

fn normalize_grounding_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn quote_grounds(
    provenance: &[DraftFieldProvenance],
    fields: &[&str],
    value: &str,
) -> bool {
    provenance.iter().any(|entry| {
        fields.contains(&entry.field.as_str()) && quote_contains_value(&entry.quote, value)
    })
}

pub(crate) fn valid_email_shape(value: &str) -> bool {
    value.contains('@') && !value.contains(char::is_whitespace)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ValueKindAcceptanceVerdict {
    pub field_id: String,
    pub value_kind: Option<String>,
    pub proposed_value: String,
    pub confidence: EnrichmentConfidence,
    pub status: ValueKindAcceptanceStatus,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ValueKindAcceptanceStatus {
    Passed,
    WouldReject,
}

pub(crate) fn value_kind_acceptance_verdicts(
    plan: &EnrichmentPlan,
    diagnostics: &[EnrichmentTierEvent],
    proposals: &[EnrichmentFieldProposal],
) -> Vec<ValueKindAcceptanceVerdict> {
    proposals
        .iter()
        .map(|proposal| value_kind_acceptance_verdict(plan, diagnostics, proposal))
        .collect()
}

fn trace_value_kind_acceptance(
    plan: &EnrichmentPlan,
    diagnostics: &[EnrichmentTierEvent],
    proposals: &[EnrichmentFieldProposal],
) {
    for verdict in value_kind_acceptance_verdicts(plan, diagnostics, proposals) {
        match verdict.status {
            ValueKindAcceptanceStatus::Passed => {
                tracing::trace!(
                    field_id = %verdict.field_id,
                    value_kind = verdict.value_kind.as_deref().unwrap_or("(unknown)"),
                    confidence = ?verdict.confidence,
                    reason = %verdict.reason,
                    "enrichment value-kind acceptance gate passed"
                );
            }
            ValueKindAcceptanceStatus::WouldReject => {
                tracing::warn!(
                    field_id = %verdict.field_id,
                    value_kind = verdict.value_kind.as_deref().unwrap_or("(unknown)"),
                    confidence = ?verdict.confidence,
                    reason = %verdict.reason,
                    "enrichment value-kind acceptance gate would reject current proposal"
                );
            }
        }
    }
}

/// Build durable diagnostic events for the gate's `would_reject` verdicts so they
/// surface in enrichment_runs (and /api/enrichment/runs) for review. NON-enforcing:
/// proposals/grafts are never changed; these events only annotate what the gate
/// WOULD reject if it were flipped to enforce. Passing verdicts are not recorded.
fn value_kind_would_reject_events(
    plan: &EnrichmentPlan,
    diagnostics: &[EnrichmentTierEvent],
    proposals: &[EnrichmentFieldProposal],
) -> Vec<EnrichmentTierEvent> {
    proposals
        .iter()
        .filter_map(|proposal| {
            let verdict = value_kind_acceptance_verdict(plan, diagnostics, proposal);
            if verdict.status != ValueKindAcceptanceStatus::WouldReject {
                return None;
            }
            Some(EnrichmentTierEvent {
                event_type: "value_kind_would_reject".to_string(),
                tier: proposal.source_tier,
                field_id: Some(verdict.field_id),
                status: Some("would_reject".to_string()),
                reason: Some(verdict.reason),
                source_id: None,
                url: None,
                final_url: None,
                query: None,
                rank: None,
                title: None,
                snippet: None,
                proposed_value: Some(verdict.proposed_value),
                confidence: Some(verdict.confidence),
                quote: None,
                latency_ms: None,
                bytes: None,
                cost_micros: None,
                ..Default::default()
            })
        })
        .collect()
}

fn value_kind_acceptance_verdict(
    plan: &EnrichmentPlan,
    diagnostics: &[EnrichmentTierEvent],
    proposal: &EnrichmentFieldProposal,
) -> ValueKindAcceptanceVerdict {
    let Some(field) = plan
        .fields
        .iter()
        .find(|field| field.field_id == proposal.field_id)
    else {
        return acceptance_verdict(
            proposal,
            None,
            ValueKindAcceptanceStatus::WouldReject,
            "field_not_in_plan",
        );
    };
    if !registered_value_kinds()
        .iter()
        .any(|registered| registered.value_kind == field.value_kind)
    {
        return acceptance_verdict(
            proposal,
            Some(&field.value_kind),
            ValueKindAcceptanceStatus::WouldReject,
            "unregistered_value_kind",
        );
    }
    if confidence_rank(proposal.confidence) < confidence_rank(field.min_confidence) {
        return acceptance_verdict(
            proposal,
            Some(&field.value_kind),
            ValueKindAcceptanceStatus::WouldReject,
            "below_min_confidence",
        );
    }
    if proposal.proposed_value.trim().is_empty() {
        return acceptance_verdict(
            proposal,
            Some(&field.value_kind),
            ValueKindAcceptanceStatus::WouldReject,
            "empty_value",
        );
    }
    if field.value_kind == VALUE_KIND_EMAIL && !valid_email_shape(&proposal.proposed_value) {
        return acceptance_verdict(
            proposal,
            Some(&field.value_kind),
            ValueKindAcceptanceStatus::WouldReject,
            "invalid_email_shape",
        );
    }
    if field.provenance_required {
        match quote_for_proposal(diagnostics, proposal) {
            // Only literal-ground real text quotes. A source citation such as
            // `page:<url>` is already grounded by the extraction source, not by
            // containing the value text; running quote_contains_value against it
            // would false-reject correctly-extracted values (for example tel: phones).
            Some(quote)
                if field.value_kind != VALUE_KIND_DESCRIPTION
                    && !quote_is_source_reference(quote, &proposal.provenance_refs) =>
            {
                if !quote_contains_value(quote, &proposal.proposed_value) {
                    return acceptance_verdict(
                        proposal,
                        Some(&field.value_kind),
                        ValueKindAcceptanceStatus::WouldReject,
                        "quote_does_not_ground_value",
                    );
                }
            }
            Some(_) => {}
            None => {
                return acceptance_verdict(
                    proposal,
                    Some(&field.value_kind),
                    ValueKindAcceptanceStatus::Passed,
                    "quote_not_observed_for_annotation",
                );
            }
        }
    }
    acceptance_verdict(
        proposal,
        Some(&field.value_kind),
        ValueKindAcceptanceStatus::Passed,
        "accepted_by_current_registry",
    )
}

fn acceptance_verdict(
    proposal: &EnrichmentFieldProposal,
    value_kind: Option<&str>,
    status: ValueKindAcceptanceStatus,
    reason: &str,
) -> ValueKindAcceptanceVerdict {
    ValueKindAcceptanceVerdict {
        field_id: proposal.field_id.clone(),
        value_kind: value_kind.map(str::to_string),
        proposed_value: proposal.proposed_value.clone(),
        confidence: proposal.confidence,
        status,
        reason: reason.to_string(),
    }
}

fn quote_is_source_reference(quote: &str, provenance_refs: &[String]) -> bool {
    let quote = quote.trim();
    provenance_refs
        .iter()
        .map(|reference| reference.trim())
        .any(|reference| reference == quote && source_reference_like(reference))
}

fn source_reference_like(reference: &str) -> bool {
    [
        "page:",
        "search:",
        "web:target:",
        "email:",
        "operator_note:",
    ]
    .iter()
    .any(|prefix| reference.starts_with(prefix))
}

fn quote_for_proposal<'a>(
    diagnostics: &'a [EnrichmentTierEvent],
    proposal: &EnrichmentFieldProposal,
) -> Option<&'a str> {
    diagnostics.iter().find_map(|event| {
        (event.event_type == "field_proposal"
            && event.field_id.as_deref() == Some(proposal.field_id.as_str())
            && event.proposed_value.as_deref() == Some(proposal.proposed_value.as_str()))
        .then_some(event.quote.as_deref())
        .flatten()
    })
}

fn confidence_rank(confidence: EnrichmentConfidence) -> u8 {
    match confidence {
        EnrichmentConfidence::Low => 0,
        EnrichmentConfidence::Medium => 1,
        EnrichmentConfidence::High => 2,
    }
}

pub(crate) fn skip_event(
    tier: EnrichmentTier,
    event_type: &str,
    reason: &str,
) -> EnrichmentTierEvent {
    EnrichmentTierEvent {
        event_type: event_type.to_string(),
        tier,
        field_id: None,
        status: Some("skipped".to_string()),
        reason: Some(reason.to_string()),
        source_id: None,
        url: None,
        final_url: None,
        query: None,
        rank: None,
        title: None,
        snippet: None,
        proposed_value: None,
        confidence: None,
        quote: None,
        latency_ms: None,
        bytes: None,
        cost_micros: None,
        ..Default::default()
    }
}

fn validate_plan_against_registries(plan: &EnrichmentPlan) {
    if !registered_subjects()
        .iter()
        .any(|registered| registered.subject_id == plan.subject)
    {
        tracing::warn!(subject = %plan.subject, "enrichment subject is not registered");
    }
    for field in &plan.fields {
        let Some(kind) = registered_value_kinds()
            .iter()
            .find(|registered| registered.value_kind == field.value_kind)
        else {
            tracing::warn!(
                subject = %plan.subject,
                field_id = %field.field_id,
                value_kind = %field.value_kind,
                "enrichment value kind is not registered"
            );
            continue;
        };
        tracing::trace!(
            subject = %plan.subject,
            field_id = %field.field_id,
            value_kind = kind.value_kind,
            sensitivity = kind.sensitivity,
            default_confidence = ?kind.default_confidence,
            min_confidence = ?field.min_confidence,
            "enrichment value kind registered"
        );
    }
    if plan.enabled_tiers.contains(&EnrichmentTier::WebSearch)
        && registered_providers()
            .iter()
            .filter(|registered| registered.tier == EnrichmentTier::WebSearch)
            .inspect(|registered| {
                tracing::trace!(
                    subject = %plan.subject,
                    provider_id = registered.provider_id,
                    tier = ?registered.tier,
                    "enrichment provider registered"
                );
            })
            .count()
            == 0
    {
        tracing::warn!(subject = %plan.subject, "web_search tier enabled without a registered provider");
    }
}

fn planned_run_id_for_plan(
    slice_id: &str,
    draft_id: &str,
    item_id: &str,
    plan: &EnrichmentPlan,
    trigger_epoch: Option<&str>,
) -> String {
    let plan_json = serde_json::to_string(plan).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(slice_id.as_bytes());
    hasher.update(b":");
    hasher.update(draft_id.as_bytes());
    hasher.update(b":");
    hasher.update(item_id.as_bytes());
    hasher.update(b":");
    hasher.update(plan_json.as_bytes());
    if let Some(trigger_epoch) = trigger_epoch {
        hasher.update(b":trigger_epoch:");
        hasher.update(trigger_epoch.as_bytes());
    }
    let digest = hasher.finalize();
    format!("enr_{}", hex_prefix(&digest, 16))
}

fn hex_prefix(bytes: &[u8], count: usize) -> String {
    bytes
        .iter()
        .take(count)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn start_enrichment_run<S: EnrichmentSubject>(
    state: &AppState,
    ctx: EnrichmentRunContext<'_>,
    run_id: &str,
    subject: &S,
    plan: &EnrichmentPlan,
) {
    let mut persistence = state.persistence.lock();
    if let Err(err) = super::store::start_run(
        persistence.connection(),
        &state.client_id,
        ctx.actor_id,
        super::store::StartRun {
            run_id,
            slice_id: ctx.slice_id,
            draft_id: subject.draft_id(),
            item_id: subject.item_id(),
            plan,
            created_by: ctx.actor_id,
            now_ms: crate::http::now_ms(),
        },
    ) {
        tracing::warn!(draft_id = %subject.draft_id(), error = %err, "enrichment run start failed");
    }
}

fn append_enrichment_diagnostics(
    state: &AppState,
    ctx: EnrichmentRunContext<'_>,
    run_id: &str,
    event_seq: &str,
    diagnostics: &[EnrichmentTierEvent],
    proposals: &[EnrichmentFieldProposal],
    cost_micros: u64,
) {
    if diagnostics.is_empty() && proposals.is_empty() && cost_micros == 0 {
        return;
    }
    let mut persistence = state.persistence.lock();
    if let Err(err) = super::store::append_run_diagnostics(
        persistence.connection(),
        &state.client_id,
        ctx.actor_id,
        super::store::AppendRunDiagnostics {
            run_id,
            event_seq,
            diagnostics,
            proposals,
            cost_micros,
            now_ms: crate::http::now_ms(),
        },
    ) {
        tracing::warn!(run_id = %run_id, event_seq, error = %err, "enrichment diagnostics append failed");
    }
}

fn transition_enrichment_run(
    state: &AppState,
    ctx: EnrichmentRunContext<'_>,
    run_id: &str,
    status: EnrichmentRunStatus,
    reason: &str,
) {
    let mut persistence = state.persistence.lock();
    if let Err(err) = super::store::transition_run_status(
        persistence.connection(),
        &state.client_id,
        ctx.actor_id,
        super::store::TransitionRunStatus {
            run_id,
            status,
            now_ms: crate::http::now_ms(),
            reason,
        },
    ) {
        tracing::warn!(run_id = %run_id, error = %err, "enrichment run status transition failed");
    }
}
