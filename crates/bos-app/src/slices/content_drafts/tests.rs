//! Slice tests: evidence selection (budget, per-doc cap, dedupe, trim),
//! grounded-fill parsing, the deterministic citation gate (supported /
//! missing-citation / unsupported; gate blocks approval), and the stage →
//! approve/reject/update lifecycle. LLM interactions are tested at the
//! parse/build seams only — no live LLM, no network.

use bos_contracts::content_drafts::{ContentClaimStatus, ContentEvidenceSnippet};
use bos_contracts::email_triage::InboundMessageRecord;
use bos_contracts::work_queue::{WorkItem, WorkItemStatus};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::service::{
    self, EVIDENCE_MAX_PER_DOC, EVIDENCE_MAX_SNIPPETS, WEB_EVIDENCE_MAX_SNIPPETS,
};
use super::store::{self, DraftActionContext};
use crate::http::test_support::test_state;

const CLIENT: &str = "test-client";

#[test]
fn grounded_draft_request_appends_background_after_evidence() {
    use bos_integrations::llm_typed_tasks::TypedLlmTextBlock;
    let item = work_item("wi_bg");
    let source = message("How should we prep concrete floors?");

    let plain = service::build_grounded_draft_request(CLIENT, &item, &source, &[], None, 1);
    assert!(!plain
        .input
        .text_blocks
        .iter()
        .any(|b| b.block_id == "background"));

    let block = TypedLlmTextBlock {
        block_id: "background".to_string(),
        text: "Company: Example Company".to_string(),
    };
    let grounded =
        service::build_grounded_draft_request(CLIENT, &item, &source, &[], Some(block), 1);
    // Background lands AFTER brief + evidence blocks.
    let last = grounded.input.text_blocks.last().expect("a block");
    assert_eq!(last.block_id, "background");
    assert_eq!(last.text, "Company: Example Company");
}

fn work_item(item_id: &str) -> WorkItem {
    WorkItem {
        item_id: item_id.to_string(),
        source_kind: "operator_note".to_string(),
        source_ref: "note-1".to_string(),
        category_id: "marketing".to_string(),
        title: "Epoxy floor coating guide".to_string(),
        summary: String::new(),
        packet_kinds: vec!["content_draft".to_string()],
        status: WorkItemStatus::Accepted,
        accept_actor: Some(bos_contracts::work_queue::WorkItemAcceptActor::Operator),
        ai_suggested: false,
        rationale: String::new(),
        produce_guidance: String::new(),
        source_user_id: None,
        assignee_user_id: None,
        visible_to_user_ids: Vec::new(),
        created_at_ms: 1,
        updated_at_ms: 1,
    }
}

fn message(body: &str) -> InboundMessageRecord {
    InboundMessageRecord {
        source_key: "note-1".to_string(),
        message_id: "note-1".to_string(),
        thread_id: None,
        internal_date_ms: Some(1_000),
        from_addr: None,
        to_addr: None,
        subject: Some("brief".to_string()),
        body_excerpt: body.to_string(),
        body_full: String::new(),
        headers: Vec::new(),
        labels: Vec::new(),
        resolved_category: "marketing".to_string(),
        matched_rule_id: None,
        ingested_at_ms: 1_000,
        ai_triage_status: None,
        ai_triage_rationale: None,
        attachments: Vec::new(),
        source_user_id: None,
    }
}

fn snippet(snippet_id: &str, file_id: &str, text: &str) -> ContentEvidenceSnippet {
    ContentEvidenceSnippet {
        snippet_id: snippet_id.to_string(),
        file_id: file_id.to_string(),
        doc_title: "Epoxy Guide".to_string(),
        heading_path: vec!["Prep".to_string()],
        text: text.to_string(),
        web_view_link: None,
    }
}

fn context_with_evidence(evidence: Vec<ContentEvidenceSnippet>) -> serde_json::Value {
    serde_json::json!({ "evidence": serde_json::to_value(evidence).expect("evidence json") })
}

fn valid_fill() -> serde_json::Value {
    json!({
        "title": "How to Prep a Floor for Epoxy Coating",
        "body_markdown": "## Prep\n\nDegrease and etch the slab before coating.",
        "target_query": "epoxy floor prep",
        "meta_description": "A step-by-step guide to preparing concrete for epoxy.",
        "claims": [
            {"text": "Degrease and etch the slab before coating", "snippet_ids": ["doc-1:0"]}
        ],
        "confidence": "high"
    })
}

// ---------------------------------------------------------------------------
// Evidence selection over a seeded corpus
// ---------------------------------------------------------------------------

fn seed_corpus_doc(state: &crate::http::AppState, file_id: &str, title: &str, text: &str) {
    use bos_integrations::google_drive_read::{DriveFileMeta, GOOGLE_DOC_MIME};
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let meta = DriveFileMeta {
        file_id: file_id.to_string(),
        name: title.to_string(),
        mime_type: GOOGLE_DOC_MIME.to_string(),
        modified_time: "2026-06-01T00:00:00Z".to_string(),
        version: Some("1".to_string()),
        parent_folder_ids: vec!["folder-a".to_string()],
        web_view_link: None,
        trashed: false,
    };
    crate::slices::drive_corpus::store::mark_stale_from_meta(conn, CLIENT, &meta, 1_000)
        .expect("stale");
    let chunks = crate::slices::drive_corpus::service::chunk_document(text);
    crate::slices::drive_corpus::store::index_document(
        conn,
        CLIENT,
        file_id,
        title,
        &format!("hash-{file_id}"),
        &chunks,
        2_000,
    )
    .expect("index");
}

#[test]
fn evidence_selection_respects_budget_per_doc_cap_and_trim() {
    let state = test_state();
    // Many sections in one doc → many chunks for the same file.
    let mut busy_doc = String::from("# Epoxy Floor Guide\n");
    for section in 0..8 {
        busy_doc.push_str(&format!(
            "\n## Step {section}\n\nEpoxy coating prep step {section}: degrease, etch, rinse the floor.\n"
        ));
    }
    seed_corpus_doc(&state, "doc-busy", "Epoxy Floor Guide", &busy_doc);
    for index in 0..6 {
        seed_corpus_doc(
            &state,
            &format!("doc-{index}"),
            &format!("Floor Notes {index}"),
            &format!("# Notes {index}\n\nEpoxy floor coating observation number {index}.\n"),
        );
    }

    let persistence = state.persistence.lock();
    let conn = persistence.connection_ref();
    let evidence =
        service::select_evidence(conn, CLIENT, "epoxy floor coating prep").expect("evidence");

    assert!(evidence.len() <= EVIDENCE_MAX_SNIPPETS);
    assert!(evidence.len() >= 5, "got {}", evidence.len());
    let busy_count = evidence
        .iter()
        .filter(|snippet| snippet.file_id == "doc-busy")
        .count();
    assert!(
        busy_count <= EVIDENCE_MAX_PER_DOC,
        "per-doc cap violated: {busy_count}"
    );
    for snippet in &evidence {
        assert!(snippet.text.chars().count() <= 903); // budget + ellipsis
        assert!(!snippet.snippet_id.is_empty());
    }
}

#[test]
fn evidence_selection_fails_loud_when_corpus_is_empty_or_brief_unsearchable() {
    let state = test_state();
    let persistence = state.persistence.lock();
    let conn = persistence.connection_ref();
    let err = service::select_evidence(conn, CLIENT, "epoxy coating")
        .expect_err("empty corpus must refuse");
    assert!(matches!(
        err,
        crate::store_core::StoreError::Domain(code) if code == "content_no_evidence"
    ));
    let err =
        service::select_evidence(conn, CLIENT, "a an").expect_err("unsearchable brief refuses");
    assert!(matches!(
        err,
        crate::store_core::StoreError::Domain(code) if code == "content_brief_unsearchable"
    ));
}

// ---------------------------------------------------------------------------
// Web fact evidence seams
// ---------------------------------------------------------------------------

#[test]
fn web_fact_snippets_are_stable_bounded_and_citable() {
    let pages = vec![
        bos_integrations::web_page_read::FetchedPage {
            url: "https://example.com/about".to_string(),
            html: r#"
                <html><body>
                  <h1>Example Co</h1>
                  <p>Example Co installs epoxy floor coatings for commercial kitchens and warehouses.</p>
                  <p>The team prepares concrete by degreasing, etching, and rinsing slabs before coating.</p>
                  <p>Decorative sentence with very little relevant information.</p>
                </body></html>
            "#
            .to_string(),
        },
        bos_integrations::web_page_read::FetchedPage {
            url: "https://example.com/services".to_string(),
            html: r#"
                <html><body>
                  <p>Example Co offers epoxy coating maintenance plans for high traffic facilities.</p>
                  <p>Warehouse floors can be inspected before coating to identify cracks and moisture.</p>
                </body></html>
            "#
            .to_string(),
        },
    ];
    let snippets = service::extract_web_fact_snippets(
        "cnt_wi_web_1",
        "example.com",
        &pages,
        "write about epoxy floor coating prep for Example Co",
    );
    let again = service::extract_web_fact_snippets(
        "cnt_wi_web_1",
        "example.com",
        &pages,
        "write about epoxy floor coating prep for Example Co",
    );

    assert_eq!(snippets, again);
    assert!(!snippets.is_empty());
    assert!(snippets.len() <= WEB_EVIDENCE_MAX_SNIPPETS);
    assert!(snippets[0].snippet_id.starts_with("web:cnt_wi_web_1:"));
    assert_eq!(snippets[0].file_id, "web:example.com");
    assert_eq!(
        snippets[0].web_view_link.as_deref(),
        Some("https://example.com/about")
    );
    assert!(
        snippets[0].text.to_ascii_lowercase().contains("epoxy"),
        "highest-ranked web fact should be relevant: {:?}",
        snippets[0]
    );

    let claims = vec![(
        "Example Co installs epoxy floor coatings".to_string(),
        vec![snippets[0].snippet_id.clone()],
    )];
    let (_checked, gate) = service::evaluate_citation_gate(&claims, &snippets);
    assert!(
        gate.passed,
        "web snippets must feed the existing citation gate"
    );
}

#[test]
fn web_evidence_merges_after_local_with_a_small_cap() {
    let mut local = Vec::new();
    for index in 0..8 {
        local.push(snippet(
            &format!("doc-1:{index}"),
            "doc-1",
            &format!("Local epoxy evidence {index}"),
        ));
    }
    let web = (0..5)
        .map(|index| ContentEvidenceSnippet {
            snippet_id: format!("web:cnt:{}", index),
            file_id: "web:example.com".to_string(),
            doc_title: "Web facts: example.com".to_string(),
            heading_path: vec!["Web facts".to_string()],
            text: format!("Web epoxy evidence {index}"),
            web_view_link: Some(format!("https://example.com/{index}")),
        })
        .collect();

    let merged = service::merge_evidence_with_web(local.clone(), web);

    assert_eq!(merged.len(), EVIDENCE_MAX_SNIPPETS);
    assert_eq!(merged[0].snippet_id, "doc-1:0");
    assert_eq!(merged[7].snippet_id, "doc-1:7");
    assert_eq!(merged[8].snippet_id, "web:cnt:0");
    assert_eq!(merged[9].snippet_id, "web:cnt:1");
    assert_eq!(
        merged
            .iter()
            .filter(|snippet| snippet.file_id.starts_with("web:"))
            .count(),
        2
    );

    let full_local = (0..12)
        .map(|index| snippet(&format!("doc-full:{index}"), "doc-full", "Local"))
        .collect();
    let merged = service::merge_evidence_with_web(full_local, Vec::new());
    assert_eq!(merged.len(), EVIDENCE_MAX_SNIPPETS);
    assert!(merged
        .iter()
        .all(|snippet| !snippet.file_id.starts_with("web:")));
}

#[test]
fn content_web_facts_round_trip_by_run_and_target() {
    let state = test_state();
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let snippets = vec![
        ContentEvidenceSnippet {
            snippet_id: "web:cnt_wi_1:aaa".to_string(),
            file_id: "web:example.com".to_string(),
            doc_title: "Web facts: example.com".to_string(),
            heading_path: vec![
                "Web facts".to_string(),
                "https://example.com/about".to_string(),
            ],
            text: "Example Co installs epoxy floor coatings.".to_string(),
            web_view_link: Some("https://example.com/about".to_string()),
        },
        ContentEvidenceSnippet {
            snippet_id: "web:cnt_wi_1:bbb".to_string(),
            file_id: "web:example.com".to_string(),
            doc_title: "Web facts: example.com".to_string(),
            heading_path: vec![
                "Web facts".to_string(),
                "https://example.com/services".to_string(),
            ],
            text: "Example Co prepares concrete before coating.".to_string(),
            web_view_link: Some("https://example.com/services".to_string()),
        },
    ];
    let record = store::ContentWebFactsRecord {
        target_id: "cnt_wi_1_1".to_string(),
        item_id: "wi_1".to_string(),
        source_kind: "operator_note".to_string(),
        source_ref: "note-1".to_string(),
        run_id: "enr_content_1".to_string(),
        snippets: snippets.clone(),
    };
    store::persist_web_facts(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "content_company_facts",
            expected_revision: None,
            idempotency_key: "contentwebfacts:cnt_wi_1_1:enr_content_1",
            now_ms: 4_000,
        },
        &record,
    )
    .expect("persist");

    assert_eq!(
        store::web_facts_by_run(conn, CLIENT, "enr_content_1").expect("by run"),
        snippets
    );
    assert_eq!(
        store::web_facts_for_target(conn, CLIENT, "cnt_wi_1_1").expect("by target"),
        snippets
    );

    let receipt_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM receipts WHERE client_id = ?1 \
             AND entity_kind = ?2 AND entity_id = ?3 AND change_kind = 'enrich_evidence'",
            rusqlite::params![CLIENT, store::WEB_FACTS_ENTITY_KIND, "cnt_wi_1_1"],
            |row| row.get(0),
        )
        .expect("receipt count");
    assert_eq!(receipt_count, 1);
}

#[test]
fn content_web_facts_default_off_is_corpus_only() {
    let state = test_state();
    let item = work_item("wi_default_off");
    let source = message("Write about https://example.com epoxy coating prep.");
    let context = context_with_evidence(vec![snippet(
        "doc-1:0",
        "doc-1",
        "Local corpus evidence stays alone.",
    )]);
    let enriched = service::enrich_context_with_web_facts(
        &state,
        &item,
        &source,
        context.clone(),
        1,
        false,
        &|_| panic!("disabled content web facts must not crawl"),
    );

    assert_eq!(enriched, context);
}

#[test]
fn web_fact_snippets_match_legacy_page_text_bytes_for_migration() {
    let pages = vec![
        bos_integrations::web_page_read::FetchedPage {
            url: "https://example.com/about".to_string(),
            html: r#"
                <html><head><style>.hidden{}</style><script>bad()</script></head><body>
                  <h1>Example Co</h1>
                  <p>Example Co installs epoxy floor coatings for commercial kitchens &amp; warehouses.</p>
                  <p>The team prepares concrete&nbsp;by degreasing, etching, and rinsing slabs.</p>
                </body></html>
            "#
            .to_string(),
        },
        bos_integrations::web_page_read::FetchedPage {
            url: "https://example.com/contact".to_string(),
            html: r#"
                <html><body>
                  <address>1 Market St<br>San Francisco, CA</address>
                  <p>Call (415) 555-0199 for coating maintenance.</p>
                </body></html>
            "#
            .to_string(),
        },
    ];
    let production = service::extract_web_fact_snippets(
        "cnt_wi_web_migration",
        "example.com",
        &pages,
        "write about epoxy floor coating prep for Example Co",
    );
    let legacy_texts: Vec<(usize, &str, String)> = pages
        .iter()
        .enumerate()
        .map(|(page_rank, page)| {
            (
                page_rank,
                page.url.as_str(),
                bos_integrations::web_page_read::strip_to_text(&page.html, 12_000),
            )
        })
        .collect();
    let legacy = service::extract_web_fact_snippets_from_legacy_page_texts(
        "cnt_wi_web_migration",
        "example.com",
        &legacy_texts,
        "write about epoxy floor coating prep for Example Co",
    );
    assert_eq!(production, legacy);
}

#[test]
fn content_web_facts_enabled_merges_web_evidence_and_stage_gate_accepts_it() {
    use crate::produce::ProduceFlavor;

    let state = test_state();
    let item = work_item("wi_web_live");
    let source = message("Write about https://example.com epoxy coating prep.");
    let local = vec![snippet(
        "doc-1:0",
        "doc-1",
        "Local corpus evidence about epoxy floors.",
    )];
    let context = context_with_evidence(local);
    let pages = vec![bos_integrations::web_page_read::FetchedPage {
        url: "https://example.com/about".to_string(),
        html: r#"
            <html><body>
              <p>Example Co installs epoxy floor coatings for commercial kitchens and warehouses.</p>
              <p>The team prepares concrete by degreasing and etching slabs before coating.</p>
            </body></html>
        "#
        .to_string(),
    }];
    let enriched =
        service::enrich_context_with_web_facts(&state, &item, &source, context, 1, true, &|_| {
            Ok(pages.clone())
        });
    let evidence: Vec<ContentEvidenceSnippet> = enriched
        .get("evidence")
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .expect("evidence");
    let web = evidence
        .iter()
        .find(|snippet| snippet.file_id == "web:example.com")
        .expect("web evidence merged");

    let response = json!({
        "title": "Example Co Epoxy Prep",
        "body_markdown": "Example Co installs epoxy floor coatings for commercial kitchens and warehouses.",
        "target_query": "example co epoxy coating",
        "meta_description": "Example Co epoxy coating prep guidance.",
        "claims": [
            {
                "text": "Example Co installs epoxy floor coatings for commercial kitchens and warehouses",
                "snippet_ids": [web.snippet_id]
            }
        ],
        "confidence": "high"
    });
    let mut persistence = state.persistence.lock();
    service::Produce
        .stage(crate::produce::StageContext {
            conn: persistence.connection(),
            client_id: CLIENT,
            actor_id: "operator",
            item: &item,
            message: &source,
            response: &response,
            context: &enriched,
            model: "model",
            attempt: 1,
            idempotency_key: "stage:web:evidence",
            now_ms: 5_000,
        })
        .expect("stage");
    let draft = store::get_draft(persistence.connection_ref(), CLIENT, "cnt_wi_web_live_1")
        .expect("get")
        .expect("draft")
        .draft;
    assert!(draft.citation_gate.passed);
    assert!(draft.evidence.iter().any(|snippet| {
        snippet.file_id == "web:example.com" && snippet.web_view_link.is_some()
    }));
    let run_count: i64 = persistence
        .connection_ref()
        .query_row(
            "SELECT COUNT(*) FROM enrichment_runs \
             WHERE client_id = ?1 AND item_id = ?2 AND draft_id = ?3 \
               AND subject = 'content_company_facts'",
            rusqlite::params![CLIENT, item.item_id, "cnt_wi_web_live_1"],
            |row| row.get(0),
        )
        .expect("run count");
    assert_eq!(run_count, 1);
}

#[test]
fn content_web_facts_no_literal_domain_or_crawl_failure_stays_corpus_only() {
    let state = test_state();
    let item = work_item("wi_no_domain");
    let source = message("Write about epoxy coating prep without a URL.");
    let context = context_with_evidence(vec![snippet(
        "doc-1:0",
        "doc-1",
        "Local corpus evidence stays available.",
    )]);
    let enriched = service::enrich_context_with_web_facts(
        &state,
        &item,
        &source,
        context.clone(),
        1,
        true,
        &|_| panic!("no literal domain must not crawl"),
    );
    assert_eq!(enriched, context);

    let failing_source = message("Write about https://example.com epoxy coating prep.");
    let enriched = service::enrich_context_with_web_facts(
        &state,
        &work_item("wi_crawl_fail"),
        &failing_source,
        context.clone(),
        1,
        true,
        &|_| {
            Err(bos_integrations::web_page_read::WebFetchError::Transport {
                message: "forced failure".to_string(),
            })
        },
    );
    assert_eq!(enriched, context);
}

#[test]
fn content_web_facts_retry_reuses_persisted_run_without_recrawling() {
    let state = test_state();
    let item = work_item("wi_retry");
    let source = message("Write about https://example.com epoxy coating prep.");
    let context =
        context_with_evidence(vec![snippet("doc-1:0", "doc-1", "Local corpus evidence.")]);
    let calls = AtomicUsize::new(0);
    let pages = vec![bos_integrations::web_page_read::FetchedPage {
        url: "https://example.com/about".to_string(),
        html: r#"
            <html><body>
              <p>Example Co installs epoxy floor coatings for commercial kitchens and warehouses.</p>
              <p>The team prepares concrete by degreasing and etching slabs before coating.</p>
            </body></html>
        "#
        .to_string(),
    }];
    let crawl = |_: &str| {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(pages.clone())
    };

    let first = service::enrich_context_with_web_facts(
        &state,
        &item,
        &source,
        context.clone(),
        1,
        true,
        &crawl,
    );
    let second =
        service::enrich_context_with_web_facts(&state, &item, &source, context, 1, true, &crawl);

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(first, second);
}

// ---------------------------------------------------------------------------
// Fill parsing + citation gate
// ---------------------------------------------------------------------------

#[test]
fn fill_parses_and_requires_title_body_claims_confidence() {
    let fill = service::parse_grounded_draft_response(&valid_fill()).expect("valid");
    assert_eq!(fill.title, "How to Prep a Floor for Epoxy Coating");
    assert_eq!(fill.claims.len(), 1);
    assert_eq!(fill.target_query.as_deref(), Some("epoxy floor prep"));

    let mut missing_claims = valid_fill();
    missing_claims["claims"] = json!([]);
    assert!(service::parse_grounded_draft_response(&missing_claims).is_err());

    let mut no_title = valid_fill();
    no_title["title"] = json!("");
    assert!(service::parse_grounded_draft_response(&no_title).is_err());

    let mut bad_confidence = valid_fill();
    bad_confidence["confidence"] = json!("certain");
    assert!(service::parse_grounded_draft_response(&bad_confidence).is_err());
}

#[test]
fn citation_gate_classifies_supported_missing_and_unsupported() {
    let evidence = vec![
        snippet(
            "doc-1:0",
            "doc-1",
            "Degrease and etch the slab before coating for adhesion.",
        ),
        snippet(
            "doc-1:1",
            "doc-1",
            "Allow 72 hours of cure before heavy traffic.",
        ),
    ];
    let claims = vec![
        // Supported: terms present in the cited snippet.
        (
            "Degrease and etch the slab before coating".to_string(),
            vec!["doc-1:0".to_string()],
        ),
        // Missing: no citation at all.
        ("Epoxy lasts twenty years".to_string(), Vec::new()),
        // Unsupported: cites a snippet that says something else.
        (
            "Epoxy floors cost five dollars per square foot".to_string(),
            vec!["doc-1:1".to_string()],
        ),
        // Unknown snippet id → treated as uncited.
        (
            "Unknown citation claim".to_string(),
            vec!["doc-9:9".to_string()],
        ),
    ];

    let (checked, gate) = service::evaluate_citation_gate(&claims, &evidence);

    assert_eq!(checked[0].status, ContentClaimStatus::Supported);
    assert_eq!(checked[1].status, ContentClaimStatus::MissingCitation);
    assert_eq!(checked[2].status, ContentClaimStatus::Unsupported);
    assert_eq!(checked[3].status, ContentClaimStatus::MissingCitation);
    assert!(!gate.passed);
    assert_eq!(gate.missing_citation_claim_ids, vec!["claim-2", "claim-4"]);
    assert_eq!(gate.unsupported_claim_ids, vec!["claim-3"]);

    // All supported → gate passes.
    let (checked, gate) = service::evaluate_citation_gate(&claims[..1], &evidence);
    assert!(gate.passed);
    assert_eq!(checked.len(), 1);
}

// ---------------------------------------------------------------------------
// Stage → approve/reject/update lifecycle (gate enforced)
// ---------------------------------------------------------------------------

fn staged_draft(
    state: &crate::http::AppState,
    item_id: &str,
    fill_value: &serde_json::Value,
    evidence: Vec<ContentEvidenceSnippet>,
) -> String {
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let fill = service::parse_grounded_draft_response(fill_value).expect("fill");
    let (claims, gate) = service::evaluate_citation_gate(&fill.claims, &evidence);
    let draft = service::draft_from_fill(
        &work_item(item_id),
        &fill,
        evidence,
        claims,
        gate,
        1,
        "m",
        5_000,
    );
    let draft_id = draft.draft_id.clone();
    store::insert_draft(
        conn,
        CLIENT,
        "operator",
        &draft,
        &format!("stage:{draft_id}"),
    )
    .expect("insert");
    draft_id
}

#[test]
fn stage_approve_lifecycle_enforces_the_citation_gate() {
    let state = test_state();
    let evidence = vec![snippet(
        "doc-1:0",
        "doc-1",
        "Degrease and etch the slab before coating for adhesion.",
    )];

    // Gate-passing draft approves cleanly.
    let draft_id = staged_draft(&state, "wi_ok", &valid_fill(), evidence.clone());
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let staged = store::get_draft(conn, CLIENT, &draft_id)
        .expect("get")
        .expect("exists");
    assert!(staged.draft.citation_gate.passed);
    store::approve_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "avery",
            expected_revision: Some(staged.revision),
            idempotency_key: "appr-1",
            now_ms: 6_000,
        },
        &draft_id,
    )
    .expect("approve");
    let approved = store::get_draft(conn, CLIENT, &draft_id)
        .expect("get")
        .expect("exists");
    assert_eq!(
        approved.draft.status,
        bos_contracts::content_drafts::ContentDraftStatus::Approved
    );
    drop(persistence);

    // Gate-failing draft refuses approval but can be rejected.
    let mut uncited = valid_fill();
    uncited["claims"] = json!([{"text": "Epoxy lasts twenty years", "snippet_ids": []}]);
    let draft_id = staged_draft(&state, "wi_bad", &uncited, evidence);
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let err = store::approve_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "avery",
            expected_revision: None,
            idempotency_key: "appr-2",
            now_ms: 7_000,
        },
        &draft_id,
    )
    .expect_err("gate must block approval");
    assert!(matches!(
        err,
        crate::store_core::StoreError::Domain(code) if code == "content_citation_gate_failed"
    ));
    store::reject_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "avery",
            expected_revision: None,
            idempotency_key: "rej-1",
            now_ms: 8_000,
        },
        &draft_id,
    )
    .expect("reject still allowed");
}

#[test]
fn one_active_draft_per_item_and_staged_ids_surface() {
    let state = test_state();
    let evidence = vec![snippet("doc-1:0", "doc-1", "Degrease and etch the slab.")];
    let _first = staged_draft(&state, "wi_1", &valid_fill(), evidence.clone());

    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    assert_eq!(
        store::staged_item_ids(conn, CLIENT).expect("ids"),
        vec!["wi_1"]
    );
    assert_eq!(
        store::count_drafts_for_item(conn, CLIENT, "wi_1").expect("count"),
        1
    );

    // A second active draft for the same item violates the unique index.
    let fill = service::parse_grounded_draft_response(&valid_fill()).expect("fill");
    let (claims, gate) = service::evaluate_citation_gate(&fill.claims, &evidence);
    let duplicate = service::draft_from_fill(
        &work_item("wi_1"),
        &fill,
        evidence,
        claims,
        gate,
        2,
        "m",
        9_000,
    );
    let err = store::insert_draft(conn, CLIENT, "operator", &duplicate, "stage:dup")
        .expect_err("second active draft refused");
    assert!(matches!(
        err,
        crate::store_core::StoreError::Domain(code) if code == "content_draft_already_active"
    ));
}

#[test]
fn approved_publish_request_is_validated_and_atomically_enqueued() {
    let state = test_state();
    let evidence = vec![snippet("doc-1:0", "doc-1", "Degrease and etch the slab.")];
    let draft_id = staged_draft(&state, "wi_publish", &valid_fill(), evidence);
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let staged = store::get_draft(conn, CLIENT, &draft_id)
        .expect("get")
        .expect("exists");
    assert!(service::build_publish_job(
        CLIENT,
        &staged.draft,
        "epoxy-floor-prep",
        "2026-07-30",
        "publish-1",
    )
    .is_err());

    store::approve_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "avery",
            expected_revision: Some(staged.revision),
            idempotency_key: "approve-publish",
            now_ms: 6_000,
        },
        &draft_id,
    )
    .expect("approve");
    let approved = store::get_draft(conn, CLIENT, &draft_id)
        .expect("get approved")
        .expect("exists");
    let job = service::build_publish_job(
        CLIENT,
        &approved.draft,
        "epoxy-floor-prep",
        "2026-07-30",
        "publish-1",
    )
    .expect("valid job");
    let publish_outcome = store::publish_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "avery",
            expected_revision: Some(approved.revision),
            idempotency_key: "publish-1",
            now_ms: 7_000,
        },
        &draft_id,
        &job,
    )
    .expect("publish request");
    assert!(matches!(
        publish_outcome,
        crate::store_core::MutationOutcome::Applied { .. }
    ));
    let replay = store::publish_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "avery",
            expected_revision: Some(approved.revision),
            idempotency_key: "publish-1",
            now_ms: 7_001,
        },
        &draft_id,
        &job,
    )
    .expect("publish replay");
    assert!(matches!(
        replay,
        crate::store_core::MutationOutcome::ReplayedIdempotent { .. }
    ));
    let job_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM outbox_jobs WHERE client_id = ?1 AND job_id = ?2",
            rusqlite::params![CLIENT, job.job_id],
            |row| row.get(0),
        )
        .expect("count publish jobs");
    assert_eq!(job_count, 1);
    let requested = store::get_draft(conn, CLIENT, &draft_id)
        .expect("get requested")
        .expect("exists");
    let summary = requested.outbox_job.expect("attached outbox summary");
    assert_eq!(summary.job_id, job.job_id);
    assert_eq!(summary.status, crate::outbox::STATUS_PENDING);

    let claimed = crate::outbox::claim_due_job_by_id(conn, CLIENT, &job.job_id, 60_000, 7_001)
        .expect("claim first publish")
        .expect("pending publish job");
    crate::outbox::record_attempt(
        conn,
        CLIENT,
        &claimed,
        &crate::outbox::AttemptOutcome::Terminal {
            error: "test_terminal_failure".to_string(),
            result_json: None,
        },
        7_001,
    )
    .expect("make first publish retryable");
    let retry_job = service::build_publish_job(
        CLIENT,
        &approved.draft,
        "epoxy-floor-prep",
        "2026-07-30",
        "publish-2",
    )
    .expect("valid retry job");
    store::publish_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "avery",
            expected_revision: Some(requested.revision),
            idempotency_key: "publish-2",
            now_ms: 7_002,
        },
        &draft_id,
        &retry_job,
    )
    .expect("new publish after terminal failure");
    let delayed_replay = store::publish_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "avery",
            expected_revision: Some(approved.revision),
            idempotency_key: "publish-1",
            now_ms: 7_003,
        },
        &draft_id,
        &job,
    )
    .expect("delayed replay of first publish");
    assert!(matches!(
        delayed_replay,
        crate::store_core::MutationOutcome::ReplayedIdempotent { .. }
    ));

    assert!(service::build_publish_job(
        CLIENT,
        &approved.draft,
        "Bad Slug",
        "2026-07-30",
        "bad-slug",
    )
    .is_err());
    assert!(service::build_publish_job(
        CLIENT,
        &approved.draft,
        "valid-slug",
        "2026-02-30",
        "bad-date",
    )
    .is_err());
}

#[test]
fn update_edits_text_fields_only_while_staged() {
    let state = test_state();
    let evidence = vec![snippet("doc-1:0", "doc-1", "Degrease and etch the slab.")];
    let draft_id = staged_draft(&state, "wi_edit", &valid_fill(), evidence);
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();

    store::update_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "avery",
            expected_revision: None,
            idempotency_key: "edit-1",
            now_ms: 6_000,
        },
        &draft_id,
        "Better Title",
        "## Updated\n\nNew body.",
        Some("epoxy prep"),
        None,
    )
    .expect("update");
    let updated = store::get_draft(conn, CLIENT, &draft_id)
        .expect("get")
        .expect("exists");
    assert_eq!(updated.draft.title, "Better Title");
    assert_eq!(updated.draft.body_markdown, "## Updated\n\nNew body.");
    assert_eq!(updated.draft.meta_description, None);
    // Claims/evidence/gate untouched by the edit.
    assert_eq!(updated.draft.claims.len(), 1);
    assert!(updated.draft.citation_gate.passed);

    let err = store::update_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "avery",
            expected_revision: None,
            idempotency_key: "edit-2",
            now_ms: 7_000,
        },
        &draft_id,
        "",
        "body",
        None,
        None,
    )
    .expect_err("empty title refused");
    assert!(matches!(
        err,
        crate::store_core::StoreError::Domain(code) if code == "content_draft_title_required"
    ));
}

#[test]
fn prepare_context_flows_evidence_into_request_and_stage() {
    use crate::produce::ProduceFlavor;
    let state = test_state();
    seed_corpus_doc(
        &state,
        "doc-1",
        "Epoxy Guide",
        "# Epoxy Guide\n\n## Prep\n\nDegrease and etch the slab before coating.\n",
    );
    let item = work_item("wi_ctx");
    let source = message("How should we prep concrete floors for epoxy coating?");
    let flavor = service::Produce;

    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let context = flavor
        .prepare_context(
            conn,
            CLIENT,
            &item,
            &source,
            &crate::http::OperatorScope::All,
            "operator",
        )
        .expect("context");
    let evidence: Vec<ContentEvidenceSnippet> =
        serde_json::from_value(context["evidence"].clone()).expect("round-trip");
    assert!(!evidence.is_empty());

    // The request renders the same snippet ids the gate will check.
    let request = flavor.build_request(CLIENT, &item, &source, &context, 1);
    let rendered = &request.input.text_blocks[1].text;
    for snippet in &evidence {
        assert!(rendered.contains(&format!("[{}]", snippet.snippet_id)));
    }

    // Stage with a response citing a real snippet id → staged with gate state.
    let mut fill = valid_fill();
    fill["claims"] = json!([{
        "text": "Degrease and etch the slab before coating",
        "snippet_ids": [evidence[0].snippet_id]
    }]);
    flavor
        .stage(crate::produce::StageContext {
            conn: &mut *conn,
            client_id: CLIENT,
            actor_id: "operator",
            item: &item,
            message: &source,
            response: &fill,
            context: &context,
            model: "model-x",
            attempt: 1,
            idempotency_key: "stage:ctx",
            now_ms: 10_000,
        })
        .expect("stage");
    let staged = store::active_draft_for_item(conn, CLIENT, "wi_ctx")
        .expect("get")
        .expect("exists");
    assert!(staged.draft.citation_gate.passed);
    assert_eq!(staged.draft.evidence.len(), evidence.len());
    assert_eq!(staged.draft.model, "model-x");
}
