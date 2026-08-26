use bos_contracts::lead_discovery::{
    LeadDiscoveryCriteria, LeadDiscoverySourceConfig, LeadDiscoverySourceKind,
    LeadFindingStageRequest, LeadFindingStatus,
};
use bos_contracts::receipt::ActorKindDto;
use bos_contracts::source::{EvidenceAccessMode, SourceKind};
use bos_contracts::work_queue::WorkItemStatus;
use bos_integrations::web_page_read::{
    HostResolver, WebCrawlConfig, WebFetchError, WebHttp, WebHttpResponse, WebPageReader,
};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

use super::{service, store, worker};
use crate::overlay::LeadDiscoveryOverlay;
use crate::persistence::PersistencePool;
use crate::slices::mutation_context::MutationContext;
use crate::store_core::MutationOutcome;

fn overlay_with_source() -> LeadDiscoveryOverlay {
    LeadDiscoveryOverlay {
        sources: vec![LeadDiscoverySourceConfig {
            source_id: "forum_boat_restoration".to_string(),
            display_name: "Boat Restoration Forum".to_string(),
            kind: LeadDiscoverySourceKind::Forum,
            url: Some("https://example.test/forum".to_string()),
            approved: true,
            enabled: true,
            auto_poll: false,
            feed_url: None,
            approval_note: Some("Approved by operator".to_string()),
            tags: vec!["home-services".to_string()],
        }],
        criteria: LeadDiscoveryCriteria {
            lead_markets: vec!["boat restoration".to_string()],
            intent_terms: vec!["recommend".to_string()],
            prohibited_sources: vec!["broad scraping".to_string()],
            routing_packet_kinds: vec!["follow_up_task".to_string()],
        },
    }
}

fn stage_request() -> LeadFindingStageRequest {
    LeadFindingStageRequest {
        source_id: "forum_boat_restoration".to_string(),
        title: "Owner needs furniture repair advice".to_string(),
        summary: "A homeowner asked for furniture repair recommendations for a restoration."
            .to_string(),
        contact_hint: Some("Dana".to_string()),
        company_hint: None,
        matched_terms: vec!["recommend".to_string()],
        item_url: Some("https://example.test/forum/thread-1".to_string()),
        evidence_quote: "Looking for a recommendation on furniture repair.".to_string(),
        captured_at_ms: Some(1_700_000),
        idempotency_key: "stage-1".to_string(),
        actor_id: None,
    }
}

#[test]
fn status_is_pending_without_enabled_sources() {
    let status = service::status(&LeadDiscoveryOverlay::default());
    assert!(!status.configured);
    assert_eq!(status.enabled_sources, 0);
    assert!(status.sources.is_empty());
}

#[test]
fn source_must_be_configured_and_enabled() {
    let overlay = overlay_with_source();
    assert!(service::resolve_enabled_source(&overlay, "forum_boat_restoration").is_ok());
    let err = service::resolve_enabled_source(&overlay, "not_approved")
        .expect_err("unknown source refused");
    assert!(err.to_string().contains("lead_source_not_configured"));

    let mut disabled = overlay.clone();
    disabled.sources[0].enabled = false;
    let err = service::resolve_enabled_source(&disabled, "forum_boat_restoration")
        .expect_err("disabled source refused");
    assert!(err.to_string().contains("lead_source_not_enabled"));
}

#[test]
fn finding_can_be_staged_and_accepted_into_queue() {
    let pool = PersistencePool::open_in_memory().expect("db");
    let mut conn = pool.get().expect("conn");
    let overlay = overlay_with_source();
    let source = service::resolve_enabled_source(&overlay, "forum_boat_restoration").unwrap();
    let finding = service::finding_from_stage(&stage_request(), source, 2_000_000).unwrap();
    store::insert_finding(
        conn.connection(),
        "client",
        "operator",
        ActorKindDto::Operator,
        &finding,
        "stage-1",
    )
    .expect("finding staged");

    let staged = store::get_finding(conn.connection_ref(), "client", &finding.finding_id)
        .unwrap()
        .expect("finding");
    assert_eq!(staged.finding.status, LeadFindingStatus::Staged);
    assert_eq!(
        staged.finding.evidence.evidence_quote,
        "Looking for a recommendation on furniture repair."
    );
    assert_eq!(staged.finding.evidence.source.kind, SourceKind::Forum);
    assert_eq!(
        staged.finding.evidence.policy.access_mode,
        EvidenceAccessMode::ApprovedSourceImport
    );
    assert!(!staged.finding.evidence.policy.automated_outreach_allowed);

    store::accept_finding(
        conn.connection(),
        MutationContext {
            client_id: "client",
            actor_id: "operator",
            expected_revision: Some(staged.revision),
            idempotency_key: "accept-1",
            now_ms: 2_000_100,
        },
        &finding.finding_id,
        &overlay.criteria,
    )
    .expect("finding accepted");

    let accepted = store::get_finding(conn.connection_ref(), "client", &finding.finding_id)
        .unwrap()
        .expect("finding");
    assert_eq!(accepted.finding.status, LeadFindingStatus::Accepted);
    let item_id = accepted.finding.work_item_id.expect("queue item id");
    let item = crate::slices::work_queue::store::get_item_unscoped(
        conn.connection_ref(),
        "client",
        &item_id,
    )
    .unwrap()
    .expect("queue item");
    assert_eq!(item.item.status, WorkItemStatus::Open);
    assert_eq!(item.item.source_kind, super::SOURCE_KIND_LEAD_FINDING);
    assert_eq!(item.item.packet_kinds, vec!["follow_up_task"]);
}

#[test]
fn count_findings_created_since_returns_weekly_source_data() {
    let pool = PersistencePool::open_in_memory().expect("db");
    let mut conn = pool.get().expect("conn");
    let overlay = overlay_with_source();
    let source = service::resolve_enabled_source(&overlay, "forum_boat_restoration").unwrap();
    let mut fresh_request = stage_request();
    fresh_request.idempotency_key = "fresh".to_string();
    let fresh = service::finding_from_stage(&fresh_request, source, 20_000).unwrap();
    store::insert_finding(
        conn.connection(),
        "client",
        "operator",
        ActorKindDto::Operator,
        &fresh,
        "stage-fresh",
    )
    .expect("fresh finding");
    let mut old_request = stage_request();
    old_request.idempotency_key = "old".to_string();
    let old = service::finding_from_stage(&old_request, source, 1_000).unwrap();
    store::insert_finding(
        conn.connection(),
        "client",
        "operator",
        ActorKindDto::Operator,
        &old,
        "stage-old",
    )
    .expect("old finding");

    let count = store::count_findings_created_since(conn.connection_ref(), "client", 10_000)
        .expect("count");
    let staged_count =
        store::count_findings_by_status(conn.connection_ref(), "client", LeadFindingStatus::Staged)
            .expect("staged count");

    assert_eq!(count, 1);
    assert_eq!(staged_count, 2);
}

#[test]
fn accept_replays_idempotently_after_status_changes() {
    let pool = PersistencePool::open_in_memory().expect("db");
    let mut conn = pool.get().expect("conn");
    let overlay = overlay_with_source();
    let source = service::resolve_enabled_source(&overlay, "forum_boat_restoration").unwrap();
    let finding = service::finding_from_stage(&stage_request(), source, 2_000_000).unwrap();
    store::insert_finding(
        conn.connection(),
        "client",
        "operator",
        ActorKindDto::Operator,
        &finding,
        "stage-1",
    )
    .expect("finding staged");
    let staged = store::get_finding(conn.connection_ref(), "client", &finding.finding_id)
        .unwrap()
        .expect("finding");

    let first = store::accept_finding(
        conn.connection(),
        MutationContext {
            client_id: "client",
            actor_id: "operator",
            expected_revision: Some(staged.revision),
            idempotency_key: "accept-1",
            now_ms: 2_000_100,
        },
        &finding.finding_id,
        &overlay.criteria,
    )
    .expect("finding accepted");
    assert!(matches!(first, MutationOutcome::Applied { .. }));

    let replay = store::accept_finding(
        conn.connection(),
        MutationContext {
            client_id: "client",
            actor_id: "operator",
            expected_revision: Some(staged.revision),
            idempotency_key: "accept-1",
            now_ms: 2_000_200,
        },
        &finding.finding_id,
        &overlay.criteria,
    )
    .expect("accept replayed");
    assert!(matches!(replay, MutationOutcome::ReplayedIdempotent { .. }));
}

#[test]
fn parser_handles_rss_cdata_entities_and_atom_links() {
    let rss = r#"
        <rss><channel><item>
          <title>Need &amp; want retail service</title>
          <guid>post-1</guid>
          <link>https://example.test/p/1</link>
          <description><![CDATA[Looking for a <b>recommendation</b> for furniture restoration.]]></description>
        </item></channel></rss>
    "#;
    let posts = worker::parse_feed(&LeadDiscoverySourceKind::Forum, rss).expect("rss");
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].guid, "post-1");
    assert_eq!(posts[0].title, "Need & want retail service");
    assert_eq!(posts[0].url.as_deref(), Some("https://example.test/p/1"));
    assert_eq!(
        posts[0].body,
        "Looking for a recommendation for furniture restoration."
    );

    let atom = r#"
        <feed xmlns="http://www.w3.org/2005/Atom"><entry>
          <title>Topside help</title>
          <id>tag:example.test,2026:2</id>
          <link href="https://example.test/p/2" />
          <summary>Can anyone recommend a boat restoration coating?</summary>
        </entry></feed>
    "#;
    let posts = worker::parse_feed(&LeadDiscoverySourceKind::GoogleAlert, atom).expect("atom");
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].guid, "tag:example.test,2026:2");
    assert_eq!(posts[0].url.as_deref(), Some("https://example.test/p/2"));
}

#[test]
fn parser_handles_reddit_json() {
    let posts = worker::parse_feed(
        &LeadDiscoverySourceKind::Reddit,
        r#"{
          "data": {
            "children": [{
              "data": {
                "name": "t3_abc",
                "title": "Boat restoration quote",
                "selftext": "Looking for a recommendation on retail service",
                "permalink": "/r/boats/comments/abc/boat_restoration_quote/"
              }
            }]
          }
        }"#,
    )
    .expect("reddit json");
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].guid, "t3_abc");
    assert_eq!(
        posts[0].url.as_deref(),
        Some("https://www.reddit.com/r/boats/comments/abc/boat_restoration_quote/")
    );
}

#[test]
fn keyword_match_is_case_insensitive_and_deduped() {
    let criteria = LeadDiscoveryCriteria {
        lead_markets: vec!["Boat Restoration".to_string(), "retail service".to_string()],
        intent_terms: vec!["recommend".to_string(), "recommend".to_string()],
        prohibited_sources: Vec::new(),
        routing_packet_kinds: Vec::new(),
    };
    let matches = service::matched_terms_for_text(
        &criteria,
        "Can anyone RECOMMEND retail service for a boat restoration?",
    );
    assert_eq!(
        matches,
        vec!["Boat Restoration", "retail service", "recommend"]
    );
}

#[test]
fn autoscrape_match_requires_market_and_intent_when_both_are_configured() {
    let criteria = LeadDiscoveryCriteria {
        lead_markets: vec!["boat restoration".to_string()],
        intent_terms: vec!["recommend".to_string()],
        prohibited_sources: Vec::new(),
        routing_packet_kinds: Vec::new(),
    };

    assert!(
        service::autoscrape_match_terms(&criteria, "Can anyone recommend a good coating?")
            .is_empty()
    );
    assert!(
        service::autoscrape_match_terms(&criteria, "Boat restoration project diary").is_empty()
    );
    assert_eq!(
        service::autoscrape_match_terms(
            &criteria,
            "Can anyone recommend a boat restoration coating?"
        ),
        vec!["boat restoration", "recommend"]
    );
}

#[test]
fn autoscrape_ids_include_hash_suffix_for_long_guid_collisions() {
    let overlay = overlay_with_source();
    let source = &overlay.sources[0];
    let common_prefix = "https://example.test/posts/".to_string() + &"a".repeat(120);
    let (first, first_key) = service::finding_from_autoscrape(
        source,
        &overlay.criteria,
        service::AutoscrapeFindingInput {
            post_guid: &(common_prefix.clone() + "-first"),
            title: "Boat restoration quote",
            summary: "Looking for a recommendation on retail service.",
            item_url: None,
            evidence_quote: "Looking for a recommendation on retail service.",
            captured_at_ms: None,
        },
        50_000,
    )
    .expect("first finding");
    let (second, second_key) = service::finding_from_autoscrape(
        source,
        &overlay.criteria,
        service::AutoscrapeFindingInput {
            post_guid: &(common_prefix + "-second"),
            title: "Boat restoration quote",
            summary: "Looking for a recommendation on retail service.",
            item_url: None,
            evidence_quote: "Looking for a recommendation on retail service.",
            captured_at_ms: None,
        },
        50_000,
    )
    .expect("second finding");

    assert_ne!(first_key, second_key);
    assert_ne!(first.finding_id, second.finding_id);
}

#[test]
fn autoscrape_stages_system_findings_and_respects_budget() {
    let mut state = crate::http::test_support::test_state();
    state.client_id = "client".into();
    let mut overlay = overlay_with_source();
    overlay.sources[0].auto_poll = true;
    overlay.sources[0].feed_url = Some("https://feeds.example.test/rss".to_string());
    state.lead_discovery_overlay = Arc::new(overlay.clone());

    let reader = fake_reader(
        "https://feeds.example.test/rss",
        r#"
        <rss><channel>
          <item>
            <title>First boat restoration lead</title>
            <guid>post-1</guid>
            <link>https://example.test/post-1</link>
            <description>Looking for a recommend on furniture restoration.</description>
          </item>
          <item>
            <title>Second boat restoration lead</title>
            <guid>post-2</guid>
            <link>https://example.test/post-2</link>
            <description>Need retail service recommendation for restoration.</description>
          </item>
        </channel></rss>
        "#,
    );

    let summary = worker::run_sync_cycle(&state, &reader, 1, 50_000).expect("cycle");
    assert_eq!(summary.requests_used, 1);
    assert_eq!(summary.matched, 1);
    assert_eq!(summary.staged, 1);
    let findings = store::list_findings(
        state.persistence.lock().connection_ref(),
        "client",
        None,
        10,
    )
    .expect("findings");
    assert_eq!(findings.len(), 1);
    let finding = &findings[0].finding;
    assert_eq!(finding.status, LeadFindingStatus::Staged);
    assert!(finding.work_item_id.is_none());
    assert_eq!(
        finding.evidence.policy.access_mode,
        EvidenceAccessMode::ApprovedSourceImport
    );
    assert!(!finding.evidence.policy.automated_outreach_allowed);

    let receipts: Vec<(String, String)> = state
        .persistence
        .lock()
        .connection_ref()
        .prepare("SELECT actor_id, actor_kind FROM receipts WHERE entity_kind = 'lead_finding'")
        .expect("stmt")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("rows")
        .collect::<Result<_, _>>()
        .expect("receipts");
    assert_eq!(receipts, vec![("system".to_string(), "system".to_string())]);
}

#[test]
fn autoscrape_replay_does_not_reopen_rejected_findings() {
    let mut state = crate::http::test_support::test_state();
    state.client_id = "client".into();
    let mut overlay = overlay_with_source();
    overlay.sources[0].auto_poll = true;
    overlay.sources[0].feed_url = Some("https://feeds.example.test/rss".to_string());
    state.lead_discovery_overlay = Arc::new(overlay.clone());

    let reader = fake_reader(
        "https://feeds.example.test/rss",
        r#"<rss><channel><item>
          <title>Boat restoration lead</title>
          <guid>same-post</guid>
          <description>Looking for a recommend on furniture restoration.</description>
        </item></channel></rss>"#,
    );

    worker::run_sync_cycle(&state, &reader, 10, 60_000).expect("first cycle");
    let staged = store::list_findings(
        state.persistence.lock().connection_ref(),
        "client",
        Some(LeadFindingStatus::Staged),
        10,
    )
    .expect("staged")
    .pop()
    .expect("finding");
    store::reject_finding(
        state.persistence.lock().connection(),
        MutationContext {
            client_id: "client",
            actor_id: "operator",
            expected_revision: Some(staged.revision),
            idempotency_key: "reject-autoscrape",
            now_ms: 60_100,
        },
        &staged.finding.finding_id,
    )
    .expect("reject");

    worker::run_sync_cycle(&state, &reader, 10, 70_000).expect("replay cycle");
    let findings = store::list_findings(
        state.persistence.lock().connection_ref(),
        "client",
        None,
        10,
    )
    .expect("findings");
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].finding.status, LeadFindingStatus::Rejected);
}

#[test]
fn overlay_rejects_facebook_group_auto_poll() {
    let raw = r#"
schema_version = 1

[identity]
client_id = "test"
display_name = "Test"

[slices]
enabled = ["lead_discovery"]

[[lead_discovery.sources]]
source_id = "fb"
display_name = "Facebook"
kind = "facebook_group"
url = "https://facebook.com/groups/example"
approved = true
enabled = true
auto_poll = true
"#;
    let err = crate::overlay::parse(raw).expect_err("facebook auto poll rejected");
    assert!(err.to_string().contains("facebook_group cannot auto_poll"));
}

fn fake_reader(url: &str, body: &str) -> WebPageReader<FakeHttp, FakeResolver> {
    let mut responses = HashMap::new();
    responses.insert(
        url.to_string(),
        WebHttpResponse {
            status: 200,
            content_type: Some("application/rss+xml".to_string()),
            location: None,
            body: body.to_string(),
        },
    );
    WebPageReader::new(
        Arc::new(FakeHttp { responses }),
        Arc::new(FakeResolver),
        WebCrawlConfig::default(),
    )
}

struct FakeHttp {
    responses: HashMap<String, WebHttpResponse>,
}

impl WebHttp for FakeHttp {
    fn get(&self, url: &str) -> Result<WebHttpResponse, WebFetchError> {
        self.responses
            .get(url)
            .map(|response| WebHttpResponse {
                status: response.status,
                content_type: response.content_type.clone(),
                location: response.location.clone(),
                body: response.body.clone(),
            })
            .ok_or_else(|| WebFetchError::Transport {
                message: format!("unexpected url {url}"),
            })
    }
}

struct FakeResolver;

impl HostResolver for FakeResolver {
    fn resolve(&self, _host: &str) -> Result<Vec<IpAddr>, WebFetchError> {
        Ok(vec![IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34))])
    }
}

#[test]
fn reject_replays_idempotently_after_status_changes() {
    let pool = PersistencePool::open_in_memory().expect("db");
    let mut conn = pool.get().expect("conn");
    let overlay = overlay_with_source();
    let source = service::resolve_enabled_source(&overlay, "forum_boat_restoration").unwrap();
    let mut request = stage_request();
    request.idempotency_key = "stage-reject".to_string();
    let finding = service::finding_from_stage(&request, source, 2_000_000).unwrap();
    store::insert_finding(
        conn.connection(),
        "client",
        "operator",
        ActorKindDto::Operator,
        &finding,
        "stage-reject",
    )
    .expect("finding staged");
    let staged = store::get_finding(conn.connection_ref(), "client", &finding.finding_id)
        .unwrap()
        .expect("finding");

    let first = store::reject_finding(
        conn.connection(),
        MutationContext {
            client_id: "client",
            actor_id: "operator",
            expected_revision: Some(staged.revision),
            idempotency_key: "reject-1",
            now_ms: 2_000_100,
        },
        &finding.finding_id,
    )
    .expect("finding rejected");
    assert!(matches!(first, MutationOutcome::Applied { .. }));

    let replay = store::reject_finding(
        conn.connection(),
        MutationContext {
            client_id: "client",
            actor_id: "operator",
            expected_revision: Some(staged.revision),
            idempotency_key: "reject-1",
            now_ms: 2_000_200,
        },
        &finding.finding_id,
    )
    .expect("reject replayed");
    assert!(matches!(replay, MutationOutcome::ReplayedIdempotent { .. }));
}
