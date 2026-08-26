//! Pure Tier-4 research candidate finalization.
//!
//! This is the enforcing path for agentic research only. Existing Local,
//! Provider, and WebSearch `value_kind_would_reject` diagnostics remain
//! non-enforcing and are intentionally not routed through this module.

#![allow(dead_code)]

use std::collections::BTreeMap;

use bos_contracts::enrichment::{EnrichmentConfidence, EnrichmentFieldSpec};
use bos_integrations::evidence::{EvidencePage, EvidenceStore};
use bos_integrations::web_page_read::{
    canonical_contains, find_domain, first_us_phone, format_us_phone, registrable_domain,
};

use super::service::{
    valid_email_shape, ResearchValueComparator, ValueKindRegistration, VALUE_KIND_DOMAIN,
};

pub(crate) type FieldId = String;
pub(crate) type FieldValue = String;
pub(crate) type RegistrableDomain = String;
pub(crate) type UnresolvedFieldSet = BTreeMap<FieldId, EnrichmentFieldSpec>;
pub(crate) type ValueKindRegistry = [ValueKindRegistration];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResearchCandidate {
    pub field_id: FieldId,
    pub value: FieldValue,
    pub evidence_id: String,
    pub quote: String,
    pub quote_start_hint: Option<u32>,
    pub quote_end_hint: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AcceptedField {
    pub field_id: FieldId,
    pub value: FieldValue,
    pub confidence: EnrichmentConfidence,
    pub evidence_id: String,
    pub quote: String,
    pub display_byte_start: u32,
    pub display_byte_end: u32,
}

pub(crate) fn finalize_research_candidates(
    candidates: Vec<ResearchCandidate>,
    evidence: &EvidenceStore,
    seed_domain: RegistrableDomain,
    registry: &ValueKindRegistry,
    unresolved: &UnresolvedFieldSet,
) -> Vec<AcceptedField> {
    candidates
        .into_iter()
        .filter_map(|candidate| {
            let field = unresolved.get(&candidate.field_id)?;
            let kind = registry
                .iter()
                .find(|registered| registered.value_kind == field.value_kind)?;
            let page = evidence.get(&candidate.evidence_id)?;
            let quote_span = resolve_quote_span(&page.normalized_text, &candidate.quote)?;
            let quote_text = &page.normalized_text[quote_span.0..quote_span.1];
            if !value_derives_from_quote(kind.research_comparator, &candidate.value, quote_text) {
                return None;
            }
            let confidence = research_confidence(page, &seed_domain);
            if confidence_rank(confidence) < confidence_rank(field.min_confidence) {
                return None;
            }
            Some(AcceptedField {
                field_id: candidate.field_id,
                value: candidate.value,
                confidence,
                evidence_id: page.evidence_id.clone(),
                quote: quote_text.to_string(),
                display_byte_start: quote_span.0 as u32,
                display_byte_end: quote_span.1 as u32,
            })
        })
        .collect()
}

pub(crate) fn extract_candidates(
    evidence: &EvidenceStore,
    unresolved: &UnresolvedFieldSet,
) -> Vec<ResearchCandidate> {
    let mut out = Vec::new();
    for page in evidence.pages() {
        for field in unresolved.values() {
            let Some((value, quote, start, end)) = extract_candidate_for_field(page, field) else {
                continue;
            };
            out.push(ResearchCandidate {
                field_id: field.field_id.clone(),
                value,
                evidence_id: page.evidence_id.clone(),
                quote,
                quote_start_hint: Some(start as u32),
                quote_end_hint: Some(end as u32),
            });
        }
    }
    out
}

fn extract_candidate_for_field(
    page: &EvidencePage,
    field: &EnrichmentFieldSpec,
) -> Option<(String, String, usize, usize)> {
    match field.value_kind.as_str() {
        VALUE_KIND_DOMAIN => {
            let value = page.registrable_domain.clone();
            let (quote, start, end) = line_containing(&page.normalized_text, &value)?;
            Some((value, quote, start, end))
        }
        "email" => extract_email(&page.normalized_text),
        "phone" => extract_phone(&page.normalized_text),
        _ => None,
    }
}

fn resolve_quote_span(text: &str, quote: &str) -> Option<(usize, usize)> {
    let quote = quote.trim();
    if quote.is_empty() {
        return None;
    }
    if let Some(start) = text.find(quote) {
        return Some((start, start + quote.len()));
    }
    lines_with_offsets(text)
        .into_iter()
        .find(|(line, _, _)| canonical_contains(quote, line))
        .map(|(_, start, end)| (start, end))
}

fn value_derives_from_quote(comparator: ResearchValueComparator, value: &str, quote: &str) -> bool {
    let value = value.trim();
    if value.is_empty() || quote.trim().is_empty() {
        return false;
    }
    match comparator {
        ResearchValueComparator::CanonicalContains => canonical_contains(value, quote),
        ResearchValueComparator::Email => {
            valid_email_shape(value) && canonical_contains(value, quote)
        }
        ResearchValueComparator::Phone => {
            let Some(value_phone) = format_us_phone(value) else {
                return false;
            };
            first_us_phone(quote).as_deref() == Some(value_phone.as_str())
        }
        ResearchValueComparator::Domain => {
            let Some(value_domain) = find_domain(value).map(|domain| registrable_domain(&domain))
            else {
                return false;
            };
            find_domain(quote)
                .map(|domain| registrable_domain(&domain) == value_domain)
                .unwrap_or(false)
        }
    }
}

fn research_confidence(page: &EvidencePage, seed_domain: &str) -> EnrichmentConfidence {
    if page.registrable_domain == seed_domain {
        EnrichmentConfidence::High
    } else {
        EnrichmentConfidence::Medium
    }
}

fn confidence_rank(confidence: EnrichmentConfidence) -> u8 {
    match confidence {
        EnrichmentConfidence::Low => 0,
        EnrichmentConfidence::Medium => 1,
        EnrichmentConfidence::High => 2,
    }
}

fn lines_with_offsets(text: &str) -> Vec<(&str, usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0;
    for part in text.split_inclusive('\n') {
        let end = start + part.len();
        let trimmed_end = part.trim_end_matches('\n').len();
        out.push((
            &text[start..start + trimmed_end],
            start,
            start + trimmed_end,
        ));
        start = end;
    }
    if start < text.len() {
        out.push((&text[start..], start, text.len()));
    }
    if out.is_empty() && !text.is_empty() {
        out.push((text, 0, text.len()));
    }
    out
}

fn line_containing(text: &str, needle: &str) -> Option<(String, usize, usize)> {
    lines_with_offsets(text)
        .into_iter()
        .find(|(line, _, _)| canonical_contains(needle, line))
        .map(|(line, start, end)| (line.to_string(), start, end))
}

fn extract_email(text: &str) -> Option<(String, String, usize, usize)> {
    for token in text.split_whitespace() {
        let value = token.trim_matches(|ch: char| {
            !ch.is_ascii_alphanumeric()
                && ch != '@'
                && ch != '.'
                && ch != '_'
                && ch != '-'
                && ch != '+'
        });
        if valid_email_shape(value) {
            let (quote, start, end) = line_containing(text, value)?;
            return Some((value.to_string(), quote, start, end));
        }
    }
    None
}

fn extract_phone(text: &str) -> Option<(String, String, usize, usize)> {
    let value = first_us_phone(text)?;
    let (quote, start, end) = line_containing(text, &value).or_else(|| {
        lines_with_offsets(text)
            .into_iter()
            .find(|(line, _, _)| first_us_phone(line).as_deref() == Some(value.as_str()))
            .map(|(line, start, end)| (line.to_string(), start, end))
    })?;
    Some((value, quote, start, end))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bos_contracts::enrichment::{EnrichmentEligibility, EnrichmentFieldSpec};
    use bos_integrations::evidence::EvidenceStore;

    fn field(
        field_id: &str,
        value_kind: &str,
        min_confidence: EnrichmentConfidence,
    ) -> EnrichmentFieldSpec {
        EnrichmentFieldSpec {
            field_id: field_id.to_string(),
            value_kind: value_kind.to_string(),
            eligibility: EnrichmentEligibility::MissingOnly,
            min_confidence,
            provenance_required: true,
            operator_override: true,
        }
    }

    fn unresolved(fields: Vec<EnrichmentFieldSpec>) -> UnresolvedFieldSet {
        fields
            .into_iter()
            .map(|field| (field.field_id.clone(), field))
            .collect()
    }

    fn store_with_page(url: &str, html: &str) -> (EvidenceStore, String) {
        let mut store = EvidenceStore::new();
        let id = store
            .insert_html_page_urls(url, url, 1, 200, html, 16_000)
            .unwrap();
        (store, id)
    }

    fn candidate(field_id: &str, value: &str, evidence_id: &str, quote: &str) -> ResearchCandidate {
        ResearchCandidate {
            field_id: field_id.to_string(),
            value: value.to_string(),
            evidence_id: evidence_id.to_string(),
            quote: quote.to_string(),
            quote_start_hint: Some(999),
            quote_end_hint: Some(1001),
        }
    }

    fn finalize(
        candidates: Vec<ResearchCandidate>,
        store: &EvidenceStore,
        seed_domain: &str,
        unresolved: &UnresolvedFieldSet,
    ) -> Vec<AcceptedField> {
        finalize_research_candidates(
            candidates,
            store,
            seed_domain.to_string(),
            super::super::service::registered_value_kinds(),
            unresolved,
        )
    }

    #[test]
    fn deterministic_for_same_inputs() {
        let (store, id) = store_with_page(
            "https://business-7f3b46080f.test/contact",
            "<html><body><p>Email hello@business-7f3b46080f.test</p></body></html>",
        );
        let unresolved = unresolved(vec![field("email", "email", EnrichmentConfidence::Medium)]);
        let candidates = vec![candidate(
            "email",
            "hello@business-7f3b46080f.test",
            &id,
            "Email hello@business-7f3b46080f.test",
        )];

        assert_eq!(
            finalize(
                candidates.clone(),
                &store,
                "business-7f3b46080f.test",
                &unresolved
            ),
            finalize(candidates, &store, "business-7f3b46080f.test", &unresolved)
        );
    }

    #[test]
    fn fabricated_values_are_rejected_across_canonical_variants() {
        let mut store = EvidenceStore::new();
        let id = store
            .insert_html_page_urls(
                "https://business-7f3b46080f.test/contact",
                "https://business-7f3b46080f.test/contact",
                1,
                200,
                "<html><body><p>Legal&nbsp;name ACME １２ &amp; Holdings</p><p>Email hello@business-7f3b46080f.test</p></body></html>",
                16_000,
            )
            .unwrap();
        let cross_page_id = store
            .insert_html_page_urls(
                "https://business-7f3b46080f.test/other",
                "https://business-7f3b46080f.test/other",
                1,
                200,
                "<html><body><p>Other page ACME 12 &amp; Holdings</p></body></html>",
                16_000,
            )
            .unwrap();
        let unresolved = unresolved(vec![
            field("name", "name", EnrichmentConfidence::Medium),
            field("email", "email", EnrichmentConfidence::Medium),
        ]);
        let candidates = vec![
            candidate("name", "Other Co", &id, "Legal name ACME 12 & Holdings"),
            candidate(
                "email",
                "billing@business-7f3b46080f.test",
                &id,
                "Email hello@business-7f3b46080f.test",
            ),
            candidate(
                "name",
                "ACME 12 Holdings",
                "ev_missing",
                "Legal name ACME 12 Holdings",
            ),
            candidate(
                "name",
                "ACME 12 & Holdings",
                &cross_page_id,
                "Legal name ACME 12 & Holdings",
            ),
        ];

        assert!(finalize(candidates, &store, "business-7f3b46080f.test", &unresolved).is_empty());
    }

    #[test]
    fn canonical_quote_grounding_accepts_display_slice_and_ignores_bad_hints() {
        let (store, id) = store_with_page(
            "https://business-7f3b46080f.test/contact",
            "<html><body><p>Legal&nbsp;name ACME １２ &amp; Holdings</p></body></html>",
        );
        let unresolved = unresolved(vec![field("name", "name", EnrichmentConfidence::High)]);
        let accepted = finalize(
            vec![candidate(
                "name",
                "ACME 12 & Holdings",
                &id,
                "Legal name ACME 12 & Holdings",
            )],
            &store,
            "business-7f3b46080f.test",
            &unresolved,
        );

        assert_eq!(accepted.len(), 1);
        assert_ne!(accepted[0].display_byte_start, 999);
        assert_eq!(accepted[0].quote, "Legal name ACME １２ & Holdings");
    }

    #[test]
    fn value_must_derive_from_cited_quote_not_whole_page() {
        let (store, id) = store_with_page(
            "https://business-7f3b46080f.test/contact",
            "<html><body><p>Email hello@business-7f3b46080f.test</p><p>Phone 212-555-1212</p></body></html>",
        );
        let unresolved = unresolved(vec![field("phone", "phone", EnrichmentConfidence::Medium)]);

        assert!(finalize(
            vec![candidate(
                "phone",
                "(212) 555-1212",
                &id,
                "Email hello@business-7f3b46080f.test"
            )],
            &store,
            "business-7f3b46080f.test",
            &unresolved,
        )
        .is_empty());
    }

    #[test]
    fn person_sensitive_non_seed_is_medium_and_fails_high_gate_seed_can_be_high() {
        let (non_seed, non_seed_id) = store_with_page(
            "https://directory.example/listing",
            "<html><body><p>Call 212-555-1212</p></body></html>",
        );
        let medium_phone = unresolved(vec![field("phone", "phone", EnrichmentConfidence::Medium)]);
        let high_phone = unresolved(vec![field("phone", "phone", EnrichmentConfidence::High)]);
        let medium = finalize(
            vec![candidate(
                "phone",
                "(212) 555-1212",
                &non_seed_id,
                "Call 212-555-1212",
            )],
            &non_seed,
            "business-7f3b46080f.test",
            &medium_phone,
        );
        assert_eq!(medium[0].confidence, EnrichmentConfidence::Medium);
        assert!(finalize(
            vec![candidate(
                "phone",
                "(212) 555-1212",
                &non_seed_id,
                "Call 212-555-1212"
            )],
            &non_seed,
            "business-7f3b46080f.test",
            &high_phone,
        )
        .is_empty());

        let (seed, seed_id) = store_with_page(
            "https://business-7f3b46080f.test/contact",
            "<html><body><p>Call 212-555-1212</p></body></html>",
        );
        let accepted = finalize(
            vec![candidate(
                "phone",
                "(212) 555-1212",
                &seed_id,
                "Call 212-555-1212",
            )],
            &seed,
            "business-7f3b46080f.test",
            &high_phone,
        );
        assert_eq!(accepted[0].confidence, EnrichmentConfidence::High);
    }

    #[test]
    fn below_floor_candidate_is_dropped_not_annotated() {
        let (store, id) = store_with_page(
            "https://directory.example/listing",
            "<html><body><p>Email hello@business-7f3b46080f.test</p></body></html>",
        );
        let unresolved = unresolved(vec![field("email", "email", EnrichmentConfidence::High)]);

        assert!(finalize(
            vec![candidate(
                "email",
                "hello@business-7f3b46080f.test",
                &id,
                "Email hello@business-7f3b46080f.test"
            )],
            &store,
            "business-7f3b46080f.test",
            &unresolved,
        )
        .is_empty());
    }

    #[test]
    fn unknown_evidence_outside_unresolved_and_typed_mismatch_reject() {
        let (store, id) = store_with_page(
            "https://business-7f3b46080f.test/contact",
            "<html><body><p>Zip 10001</p><p>Email hello@business-7f3b46080f.test</p></body></html>",
        );
        let unresolved = unresolved(vec![field("phone", "phone", EnrichmentConfidence::Medium)]);
        let candidates = vec![
            candidate("phone", "10001", &id, "Zip 10001"),
            candidate(
                "email",
                "hello@business-7f3b46080f.test",
                &id,
                "Email hello@business-7f3b46080f.test",
            ),
            candidate(
                "phone",
                "(212) 555-1212",
                "ev_missing",
                "Phone 212-555-1212",
            ),
        ];

        assert!(finalize(candidates, &store, "business-7f3b46080f.test", &unresolved).is_empty());
    }

    #[test]
    fn domain_comparator_uses_registrable_domain() {
        let (store, id) = store_with_page(
            "https://business-7f3b46080f.test/about",
            "<html><body><p>Website https://www.business-7f3b46080f.test/contact</p></body></html>",
        );
        let unresolved = unresolved(vec![field("domain", "domain", EnrichmentConfidence::High)]);
        let accepted = finalize(
            vec![candidate(
                "domain",
                "business-7f3b46080f.test",
                &id,
                "Website https://www.business-7f3b46080f.test/contact",
            )],
            &store,
            "business-7f3b46080f.test",
            &unresolved,
        );

        assert_eq!(accepted[0].value, "business-7f3b46080f.test");
        assert_eq!(accepted[0].confidence, EnrichmentConfidence::High);
    }

    #[test]
    fn extract_candidates_is_deterministic_and_pure() {
        let (store, _id) = store_with_page(
            "https://business-7f3b46080f.test/contact",
            "<html><body><p>Email hello@business-7f3b46080f.test</p><p>Call 212-555-1212</p></body></html>",
        );
        let unresolved = unresolved(vec![
            field("email", "email", EnrichmentConfidence::Medium),
            field("phone", "phone", EnrichmentConfidence::Medium),
        ]);

        let first = extract_candidates(&store, &unresolved);
        let second = extract_candidates(&store, &unresolved);
        assert_eq!(first, second);
        assert_eq!(store.len(), 1);
    }
}
