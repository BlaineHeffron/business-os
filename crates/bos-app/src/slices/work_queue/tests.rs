use axum::body::Body;
use axum::http::Request;
use bos_contracts::ai_usage::AiUsageRow;
use bos_contracts::email_triage::InboundMessageRecord;
use bos_contracts::operator_users::OperatorUser;
use bos_contracts::packet_proposals::{
    PacketProposalDecisionMode, PacketProposalExecutionMode, PacketProposalKindOutcome,
    PacketProposalKindOutcomeStatus, PacketProposalReasonCode, PacketProposalRunStatus,
};
use bos_contracts::receipt::ActorKindDto;
use bos_contracts::work_queue::{
    PacketKindsResponse, WorkItem, WorkItemAssignActionKind, WorkItemSourceBodyFormat,
    WorkItemStatus, WorkQueuePolicy, WorkQueueResponse,
};
use http_body_util::BodyExt;
use tower::ServiceExt;

use super::service::emit_for_inbound_message;
use super::store::{self, ItemAction, ItemActionContext};
use crate::http::{build_router, test_support::test_state_configured};
use crate::persistence::Persistence;
use crate::store_core::MutationOutcome;

static ALL_SCOPE: crate::http::OperatorScope = crate::http::OperatorScope::All;

fn usage_insert(
    usage_id: &str,
    item_id: &str,
    purpose: &str,
    success: bool,
    error_code: Option<&str>,
    recorded_at_ms: u64,
) -> crate::slices::ai_usage::store::UsageInsert {
    crate::slices::ai_usage::store::UsageInsert {
        row: AiUsageRow {
            usage_id: usage_id.to_string(),
            purpose: purpose.to_string(),
            route: "api".to_string(),
            provider: "openai".to_string(),
            model: "test-model".to_string(),
            tokens_in: None,
            tokens_out: None,
            total_tokens: None,
            cost_micros: None,
            latency_ms: 30_000,
            success,
            error_code: error_code.map(str::to_string),
            correlation_id: item_id.to_string(),
            recorded_at_ms,
        },
        task_kind: None,
        thinking_level: None,
        cached_tokens: None,
        provider_request_id: None,
        error_message: None,
    }
}

struct PacketProposalRunFixture<'a> {
    run_id: &'a str,
    item_id: &'a str,
    status: PacketProposalRunStatus,
    candidates: &'a [&'a str],
    outcomes: &'a [PacketProposalKindOutcome],
    error_code: Option<&'a str>,
    actor_id: &'a str,
    now_ms: u64,
}

fn insert_packet_proposal_run(
    conn: &mut rusqlite::Connection,
    fixture: PacketProposalRunFixture<'_>,
) {
    let candidate_packet_kinds = fixture
        .candidates
        .iter()
        .map(|candidate| candidate.to_string())
        .collect::<Vec<_>>();
    crate::slices::packet_proposals::store::insert_run(
        conn,
        "test-client",
        crate::slices::packet_proposals::store::NewRun {
            run_id: fixture.run_id,
            source_kind: "email",
            source_ref: fixture.item_id.trim_start_matches("wi_email_"),
            item_id: Some(fixture.item_id),
            resolved_decision_mode: PacketProposalDecisionMode::AiDecides,
            execution_mode: PacketProposalExecutionMode::BoundedTyped,
            candidate_packet_kinds: &candidate_packet_kinds,
            idempotency_key: &format!("{}:start", fixture.run_id),
            actor_id: fixture.actor_id,
            actor_kind: ActorKindDto::Operator,
            now_ms: fixture.now_ms,
        },
    )
    .expect("insert packet proposal run");
    if fixture.status != PacketProposalRunStatus::Running {
        crate::slices::packet_proposals::store::update_run(
            conn,
            "test-client",
            crate::slices::packet_proposals::store::RunUpdate {
                run_id: fixture.run_id,
                item_id: Some(fixture.item_id),
                status: fixture.status,
                outcomes: fixture.outcomes,
                model: Some("test-model"),
                confidence: Some("medium"),
                error_code: fixture.error_code,
                idempotency_key: &format!("{}:finish", fixture.run_id),
                actor_id: fixture.actor_id,
                actor_kind: ActorKindDto::Operator,
                now_ms: fixture.now_ms,
            },
        )
        .expect("finish packet proposal run");
    }
}

fn message(id: &str, category: &str) -> InboundMessageRecord {
    InboundMessageRecord {
        source_key: id.to_string(),
        message_id: id.to_string(),
        thread_id: None,
        internal_date_ms: Some(1_000),
        from_addr: Some("customer@example.com".to_string()),
        to_addr: None,
        subject: Some("Bill is ready".to_string()),
        body_excerpt: "Your bill is now available for viewing.".to_string(),
        body_full: String::new(),
        headers: Vec::new(),
        labels: Vec::new(),
        resolved_category: category.to_string(),
        matched_rule_id: None,
        ingested_at_ms: 1_000,
        ai_triage_status: None,
        ai_triage_rationale: None,
        attachments: Vec::new(),
        source_user_id: None,
    }
}

fn billing_policy(create: bool) -> WorkQueuePolicy {
    WorkQueuePolicy {
        category_id: "billing".to_string(),
        create_work_item: create,
        packet_kinds: vec!["ledger_entry".to_string()],
        ai_suggestible_packet_kinds: Vec::new(),
        ai_suggestible_gmail_scope: Default::default(),
        ai_suggestible_gmail_categories: Vec::new(),
        auto_produce: false,
    }
}

fn personal_operator() -> OperatorUser {
    operator_user("user_jordan", "Jordan")
}

fn operator_user(user_id: &str, display_name: &str) -> OperatorUser {
    OperatorUser {
        user_id: user_id.to_string(),
        display_name: display_name.to_string(),
        active: true,
        archived_at_ms: None,
        default_calendar_id: None,
        created_at_ms: 1_000,
        updated_at_ms: 1_000,
    }
}

fn insert_operator_user(conn: &mut rusqlite::Connection, user_id: &str, display_name: &str) {
    let user = operator_user(user_id, display_name);
    crate::slices::operator_users::store::create_user(
        conn,
        "test-client",
        "op_test",
        &user,
        &format!("tok_{user_id}"),
        &format!("create_user_{user_id}"),
    )
    .expect("operator user");
}

fn disable_operator_user(conn: &mut rusqlite::Connection, user_id: &str) {
    crate::slices::operator_users::store::set_active(
        conn,
        crate::slices::operator_users::store::UserActionContext {
            client_id: "test-client",
            actor_id: "op_test",
            expected_revision: None,
            idempotency_key: &format!("disable_user_{user_id}"),
            now_ms: 2_000,
        },
        user_id,
        false,
    )
    .expect("disable user");
}

fn user_action_ctx<'a>(
    key: &'a str,
    scope: &'a crate::http::OperatorScope,
) -> ItemActionContext<'a> {
    let actor_id = match scope {
        crate::http::OperatorScope::All => "operator",
        crate::http::OperatorScope::User(user_id) => user_id.as_str(),
    };
    ItemActionContext {
        client_id: "test-client",
        actor_id,
        scope,
        expected_revision: None,
        idempotency_key: key,
        now_ms: 5_000,
    }
}

fn shared_visible_work_item(
    id: &str,
    source_user_id: Option<&str>,
    visible_to_user_ids: &[&str],
) -> WorkItem {
    WorkItem {
        item_id: id.to_string(),
        source_kind: "email".to_string(),
        source_ref: id.trim_start_matches("wi_email_").to_string(),
        category_id: "billing".to_string(),
        title: format!("Item {id}"),
        summary: "Test item".to_string(),
        packet_kinds: vec!["crm_activity".to_string()],
        status: WorkItemStatus::Open,
        accept_actor: None,
        ai_suggested: false,
        rationale: String::new(),
        produce_guidance: String::new(),
        source_user_id: source_user_id.map(str::to_string),
        assignee_user_id: None,
        visible_to_user_ids: visible_to_user_ids
            .iter()
            .map(|user_id| user_id.to_string())
            .collect(),
        created_at_ms: 1_000,
        updated_at_ms: 1_000,
    }
}

fn action_ctx<'a>(key: &'a str, expected_revision: Option<u64>) -> ItemActionContext<'a> {
    action_ctx_for_scope(key, expected_revision, &ALL_SCOPE)
}

fn action_ctx_for_scope<'a>(
    key: &'a str,
    expected_revision: Option<u64>,
    scope: &'a crate::http::OperatorScope,
) -> ItemActionContext<'a> {
    ItemActionContext {
        client_id: "test-client",
        actor_id: "op_test",
        scope,
        expected_revision,
        idempotency_key: key,
        now_ms: 5_000,
    }
}

fn work_item(id: &str, source_user_id: Option<&str>) -> WorkItem {
    WorkItem {
        item_id: id.to_string(),
        source_kind: "email".to_string(),
        source_ref: id.trim_start_matches("wi_email_").to_string(),
        category_id: "billing".to_string(),
        title: format!("Item {id}"),
        summary: "Test item".to_string(),
        packet_kinds: vec!["crm_activity".to_string()],
        status: WorkItemStatus::Open,
        accept_actor: None,
        ai_suggested: false,
        rationale: String::new(),
        produce_guidance: String::new(),
        source_user_id: source_user_id.map(str::to_string),
        assignee_user_id: None,
        visible_to_user_ids: source_user_id
            .map(|user_id| vec![user_id.to_string()])
            .unwrap_or_default(),
        created_at_ms: 1_000,
        updated_at_ms: 1_000,
    }
}

fn insert_work_item(
    conn: &mut rusqlite::Connection,
    id: &str,
    source_user_id: Option<&str>,
) -> WorkItem {
    let item = work_item(id, source_user_id);
    store::insert_item(conn, "test-client", &item).expect("insert item");
    item
}

fn assert_scope_forbidden(err: crate::store_core::StoreError) {
    assert!(matches!(
        err,
        crate::store_core::StoreError::Domain(code) if code == "scope_forbidden"
    ));
}

fn queue_feed(
    conn: &rusqlite::Connection,
    now_ms: u64,
    auto_produce_running: bool,
    debug_enabled: bool,
    in_flight: &std::collections::HashSet<(String, String)>,
) -> Vec<bos_contracts::work_queue::WorkItemWithRevision> {
    queue_feed_with_attention_filter(
        conn,
        now_ms,
        auto_produce_running,
        debug_enabled,
        false,
        in_flight,
    )
}

fn queue_feed_with_attention_filter(
    conn: &rusqlite::Connection,
    now_ms: u64,
    auto_produce_running: bool,
    debug_enabled: bool,
    attention_only: bool,
    in_flight: &std::collections::HashSet<(String, String)>,
) -> Vec<bos_contracts::work_queue::WorkItemWithRevision> {
    let load = if attention_only {
        super::service::attention_feed
    } else {
        super::service::feed
    };
    load(
        conn,
        "test-client",
        None,
        10,
        &crate::http::OperatorScope::All,
        super::service::FeedOptions {
            now_ms,
            auto_produce_running,
            debug_enabled,
            in_flight,
        },
    )
    .expect("feed")
}

fn queue_feed_for_scope(
    conn: &rusqlite::Connection,
    scope: &crate::http::OperatorScope,
) -> Vec<bos_contracts::work_queue::WorkItemWithRevision> {
    super::service::feed(
        conn,
        "test-client",
        None,
        10,
        scope,
        super::service::FeedOptions {
            now_ms: 10_000,
            auto_produce_running: false,
            debug_enabled: false,
            in_flight: &std::collections::HashSet::new(),
        },
    )
    .expect("feed")
}

#[test]
fn no_policy_or_disabled_policy_emits_nothing() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();

    assert!(
        !emit_for_inbound_message(conn, "test-client", &message("m1", "billing"), 2_000)
            .expect("no policy")
    );

    let mut disabled_policy = billing_policy(false);
    disabled_policy.packet_kinds.clear();
    store::upsert_policy(
        conn,
        "test-client",
        "op_test",
        &disabled_policy,
        "p1",
        2_100,
    )
    .expect("policy");
    assert!(
        !emit_for_inbound_message(conn, "test-client", &message("m1", "billing"), 2_200)
            .expect("disabled policy")
    );
    assert!(store::list_items(
        persistence.connection_ref(),
        "test-client",
        None,
        10,
        &crate::http::OperatorScope::All
    )
    .expect("list")
    .is_empty());
}

#[test]
fn enabled_policy_emits_once_with_suggested_packets() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    store::upsert_policy(
        conn,
        "test-client",
        "op_test",
        &billing_policy(true),
        "p1",
        1_500,
    )
    .expect("policy");

    assert!(
        emit_for_inbound_message(conn, "test-client", &message("m1", "billing"), 2_000)
            .expect("emit")
    );
    // Re-emit (the pump re-runs over existing candidates every poll) is quiet.
    assert!(
        !emit_for_inbound_message(conn, "test-client", &message("m1", "billing"), 3_000)
            .expect("re-emit")
    );

    let items = store::list_items(
        persistence.connection_ref(),
        "test-client",
        None,
        10,
        &crate::http::OperatorScope::All,
    )
    .expect("list");
    assert_eq!(items.len(), 1);
    let item = &items[0].item;
    assert_eq!(item.item_id, "wi_email_m1");
    assert_eq!(item.category_id, "billing");
    assert_eq!(item.status, WorkItemStatus::Open);
    assert_eq!(item.packet_kinds, vec!["ledger_entry".to_string()]);
    assert!(item.title.contains("Bill is ready"));
    assert!(item.summary.contains("customer@example.com"));

    // Exactly one emit receipt despite the second call.
    let receipts = crate::store_core::receipts_for_entity(
        persistence.connection_ref(),
        "test-client",
        store::ITEM_ENTITY_KIND,
        "wi_email_m1",
        10,
    )
    .expect("receipts");
    assert_eq!(receipts.len(), 1);
}

#[test]
fn list_items_filters_by_operator_scope() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    insert_work_item(conn, "wi_email_null", None);
    insert_work_item(conn, "wi_email_u1", Some("u1"));
    insert_work_item(conn, "wi_email_u2", Some("u2"));

    let all = store::list_items(
        persistence.connection_ref(),
        "test-client",
        None,
        10,
        &crate::http::OperatorScope::All,
    )
    .expect("list all");
    let all_ids: std::collections::HashSet<_> = all
        .iter()
        .map(|entry| entry.item.item_id.as_str())
        .collect();
    assert_eq!(all_ids.len(), 3);
    assert!(all_ids.contains("wi_email_null"));
    assert!(all_ids.contains("wi_email_u1"));
    assert!(all_ids.contains("wi_email_u2"));

    let u1 = store::list_items(
        persistence.connection_ref(),
        "test-client",
        None,
        10,
        &crate::http::OperatorScope::User("u1".to_string()),
    )
    .expect("list u1");
    assert_eq!(u1.len(), 1);
    assert_eq!(u1[0].item.item_id, "wi_email_u1");
}

#[test]
fn get_item_scoped_hides_other_users_and_legacy_null() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    insert_work_item(conn, "wi_email_null", None);
    insert_work_item(conn, "wi_email_u1", Some("u1"));
    insert_work_item(conn, "wi_email_u2", Some("u2"));
    let scope = crate::http::OperatorScope::User("u1".to_string());

    assert!(store::get_item_scoped(
        persistence.connection_ref(),
        "test-client",
        "wi_email_u1",
        &scope,
    )
    .expect("get own")
    .is_some());
    assert!(store::get_item_scoped(
        persistence.connection_ref(),
        "test-client",
        "wi_email_u2",
        &scope,
    )
    .expect("get other")
    .is_none());
    assert!(store::get_item_scoped(
        persistence.connection_ref(),
        "test-client",
        "wi_email_null",
        &scope,
    )
    .expect("get legacy")
    .is_none());
}

#[test]
fn shared_inbox_overlay_emits_one_item_visible_to_configured_users() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    store::upsert_policy(
        conn,
        "test-client",
        "op_test",
        &billing_policy(true),
        "p1",
        1_500,
    )
    .expect("policy");
    let mut shared = std::collections::BTreeMap::new();
    shared.insert(
        "ask".to_string(),
        crate::overlay::SharedInboxOverlay {
            match_to: vec!["ask@example.test".to_string()],
            visible_to_user_ids: vec!["user_jordan".to_string(), "user_casey".to_string()],
        },
    );
    let overlay = crate::overlay::WorkQueueOverlay {
        shared_inboxes: shared,
    };
    let mut inbound = message("m_shared", "billing");
    inbound.source_user_id = Some("source_mailbox".to_string());
    inbound.to_addr =
        Some("Casey <casey@example.test>, example Info <ASK@example.test>".to_string());

    assert!(super::service::emit_for_inbound_message_with_overlay(
        conn,
        "test-client",
        &inbound,
        &overlay,
        2_000,
    )
    .expect("emit"));
    assert!(!super::service::emit_for_inbound_message_with_overlay(
        conn,
        "test-client",
        &inbound,
        &overlay,
        3_000,
    )
    .expect("replay"));

    let jordan = store::list_items(
        persistence.connection_ref(),
        "test-client",
        None,
        10,
        &crate::http::OperatorScope::User("user_jordan".to_string()),
    )
    .expect("jordan list");
    assert_eq!(jordan.len(), 1);
    assert_eq!(jordan[0].item.item_id, "wi_email_m_shared");
    assert_eq!(
        jordan[0].item.visible_to_user_ids,
        vec!["user_casey".to_string(), "user_jordan".to_string()]
    );
    let casey = store::list_items(
        persistence.connection_ref(),
        "test-client",
        None,
        10,
        &crate::http::OperatorScope::User("user_casey".to_string()),
    )
    .expect("casey list");
    assert_eq!(casey.len(), 1);
    let source = store::list_items(
        persistence.connection_ref(),
        "test-client",
        None,
        10,
        &crate::http::OperatorScope::User("source_mailbox".to_string()),
    )
    .expect("source owner still sees via source_user_id");
    assert_eq!(source.len(), 1);
    let third = store::list_items(
        persistence.connection_ref(),
        "test-client",
        None,
        10,
        &crate::http::OperatorScope::User("user_third".to_string()),
    )
    .expect("third list");
    assert!(third.is_empty());
}

#[test]
fn visibility_rows_grant_scoped_queue_mutations_without_changing_source_owner() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let item = shared_visible_work_item("wi_email_shared", Some("source_mailbox"), &["u1", "u2"]);
    store::insert_item(conn, "test-client", &item).expect("insert");
    let u1_scope = crate::http::OperatorScope::User("u1".to_string());
    let third_scope = crate::http::OperatorScope::User("third".to_string());

    store::apply_item_action(
        persistence.connection(),
        action_ctx_for_scope("visible-action", None, &u1_scope),
        "wi_email_shared",
        ItemAction::Accept,
    )
    .expect("visible action");
    store::update_packet_kinds(
        persistence.connection(),
        action_ctx_for_scope("visible-kinds", None, &u1_scope),
        "wi_email_shared",
        &["calendar_event_draft".to_string()],
    )
    .expect("visible kinds");
    store::update_produce_guidance(
        persistence.connection(),
        action_ctx_for_scope("visible-guidance", None, &u1_scope),
        "wi_email_shared",
        "Handle before Friday.",
    )
    .expect("visible guidance");

    let stored = store::get_item_unscoped(
        persistence.connection_ref(),
        "test-client",
        "wi_email_shared",
    )
    .expect("get")
    .expect("item");
    assert_eq!(
        stored.item.source_user_id.as_deref(),
        Some("source_mailbox")
    );

    let err = store::apply_item_action(
        persistence.connection(),
        action_ctx_for_scope("hidden-action", None, &third_scope),
        "wi_email_shared",
        ItemAction::Dismiss,
    )
    .expect_err("hidden action");
    assert_scope_forbidden(err);
}

#[test]
fn assignment_allows_visible_reassignment_and_rejects_hidden_or_disabled_targets() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    insert_operator_user(conn, "u1", "User One");
    insert_operator_user(conn, "u2", "User Two");
    insert_operator_user(conn, "disabled", "Disabled");
    disable_operator_user(conn, "disabled");
    let item = shared_visible_work_item("wi_email_shared", Some("source_mailbox"), &["u1", "u2"]);
    store::insert_item(conn, "test-client", &item).expect("insert");
    let u1_scope = crate::http::OperatorScope::User("u1".to_string());
    let u2_scope = crate::http::OperatorScope::User("u2".to_string());
    let third_scope = crate::http::OperatorScope::User("third".to_string());

    store::update_assignment(
        persistence.connection(),
        user_action_ctx("assign-u1", &u1_scope),
        "wi_email_shared",
        WorkItemAssignActionKind::AssignToMe,
        None,
    )
    .expect("assign u1");
    let assigned = store::get_item_scoped(
        persistence.connection_ref(),
        "test-client",
        "wi_email_shared",
        &u2_scope,
    )
    .expect("get")
    .expect("visible");
    assert_eq!(assigned.item.assignee_user_id.as_deref(), Some("u1"));

    store::update_assignment(
        persistence.connection(),
        user_action_ctx("assign-u2", &u2_scope),
        "wi_email_shared",
        WorkItemAssignActionKind::AssignToMe,
        None,
    )
    .expect("reassign u2");
    let reassigned = store::get_item_unscoped(
        persistence.connection_ref(),
        "test-client",
        "wi_email_shared",
    )
    .expect("get")
    .expect("item");
    assert_eq!(reassigned.item.assignee_user_id.as_deref(), Some("u2"));

    let err = store::update_assignment(
        persistence.connection(),
        user_action_ctx("hidden-assign", &third_scope),
        "wi_email_shared",
        WorkItemAssignActionKind::AssignToMe,
        None,
    )
    .expect_err("hidden assign");
    assert_scope_forbidden(err);

    let err = store::update_assignment(
        persistence.connection(),
        user_action_ctx("bad-target", &u1_scope),
        "wi_email_shared",
        WorkItemAssignActionKind::AssignToUser,
        Some("disabled"),
    )
    .expect_err("disabled target");
    assert!(matches!(
        err,
        crate::store_core::StoreError::Domain(code)
            if code == "work_queue_assignee_not_active"
    ));

    let err = store::update_assignment(
        persistence.connection(),
        user_action_ctx("unassign-not-owner", &u1_scope),
        "wi_email_shared",
        WorkItemAssignActionKind::Unassign,
        None,
    )
    .expect_err("only current assignee unassigns");
    assert!(matches!(
        err,
        crate::store_core::StoreError::Domain(code)
            if code == "work_queue_unassign_forbidden"
    ));
    store::update_assignment(
        persistence.connection(),
        user_action_ctx("unassign-u2", &u2_scope),
        "wi_email_shared",
        WorkItemAssignActionKind::Unassign,
        None,
    )
    .expect("assignee unassigns");
    let unassigned = store::get_item_unscoped(
        persistence.connection_ref(),
        "test-client",
        "wi_email_shared",
    )
    .expect("get")
    .expect("item");
    assert_eq!(unassigned.item.assignee_user_id, None);
}

#[test]
fn all_scope_operator_can_assign_only_visible_active_users_but_not_self() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    insert_operator_user(conn, "manager", "Manager");
    insert_operator_user(conn, "u1", "User One");
    let item = shared_visible_work_item("wi_email_shared", Some("source_mailbox"), &["u1"]);
    store::insert_item(conn, "test-client", &item).expect("insert");

    let err = store::update_assignment(
        persistence.connection(),
        user_action_ctx("assign-operator", &crate::http::OperatorScope::All),
        "wi_email_shared",
        WorkItemAssignActionKind::AssignToMe,
        None,
    )
    .expect_err("all scope has no named self");
    assert!(matches!(
        err,
        crate::store_core::StoreError::Domain(code)
            if code == "work_queue_assignment_named_user_required"
    ));

    let err = store::update_assignment(
        persistence.connection(),
        user_action_ctx("assign-manager", &crate::http::OperatorScope::All),
        "wi_email_shared",
        WorkItemAssignActionKind::AssignToUser,
        Some("manager"),
    )
    .expect_err("hidden target rejected");
    assert!(matches!(
        err,
        crate::store_core::StoreError::Domain(code)
            if code == "work_queue_assignee_not_visible"
    ));

    store::update_assignment(
        persistence.connection(),
        user_action_ctx("assign-u1", &crate::http::OperatorScope::All),
        "wi_email_shared",
        WorkItemAssignActionKind::AssignToUser,
        Some("u1"),
    )
    .expect("all-scope assign visible user");
    let assigned = store::get_item_unscoped(
        persistence.connection_ref(),
        "test-client",
        "wi_email_shared",
    )
    .expect("get")
    .expect("item");
    assert_eq!(assigned.item.assignee_user_id.as_deref(), Some("u1"));
}

#[test]
fn item_mutations_reject_cross_scope_items() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    insert_work_item(conn, "wi_email_null", None);
    insert_work_item(conn, "wi_email_u1", Some("u1"));
    insert_work_item(conn, "wi_email_u2", Some("u2"));
    let u1_scope = crate::http::OperatorScope::User("u1".to_string());

    store::apply_item_action(
        persistence.connection(),
        action_ctx_for_scope("own-action", None, &u1_scope),
        "wi_email_u1",
        ItemAction::Accept,
    )
    .expect("own action");
    store::update_packet_kinds(
        persistence.connection(),
        action_ctx_for_scope("own-kinds", None, &u1_scope),
        "wi_email_u1",
        &["calendar_event_draft".to_string()],
    )
    .expect("own kind edit");
    store::update_produce_guidance(
        persistence.connection(),
        action_ctx_for_scope("own-guidance", None, &u1_scope),
        "wi_email_u1",
        "Prefer weekdays.",
    )
    .expect("own guidance edit");

    for item_id in ["wi_email_u2", "wi_email_null"] {
        let err = store::apply_item_action(
            persistence.connection(),
            action_ctx_for_scope("hidden-action", None, &u1_scope),
            item_id,
            ItemAction::Accept,
        )
        .expect_err("hidden action rejected");
        assert_scope_forbidden(err);

        let err = store::update_packet_kinds(
            persistence.connection(),
            action_ctx_for_scope("hidden-kinds", None, &u1_scope),
            item_id,
            &["crm_activity".to_string()],
        )
        .expect_err("hidden kind edit rejected");
        assert_scope_forbidden(err);

        let err = store::update_produce_guidance(
            persistence.connection(),
            action_ctx_for_scope("hidden-guidance", None, &u1_scope),
            item_id,
            "Hidden edit.",
        )
        .expect_err("hidden guidance edit rejected");
        assert_scope_forbidden(err);
    }
}

#[test]
fn enabled_policy_with_no_packet_kinds_emits_nothing() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    store::upsert_policy(
        conn,
        "test-client",
        "op_test",
        &WorkQueuePolicy {
            category_id: "billing".to_string(),
            create_work_item: true,
            packet_kinds: Vec::new(),
            ai_suggestible_packet_kinds: vec!["follow_up_task".to_string()],
            ai_suggestible_gmail_scope: Default::default(),
            ai_suggestible_gmail_categories: Vec::new(),
            auto_produce: true,
        },
        "p1",
        1_500,
    )
    .expect("policy");

    assert!(
        !emit_for_inbound_message(conn, "test-client", &message("m1", "billing"), 2_000)
            .expect("empty deterministic packet set")
    );
    assert!(store::list_items(
        persistence.connection_ref(),
        "test-client",
        None,
        10,
        &crate::http::OperatorScope::All
    )
    .expect("list")
    .is_empty());
}

#[test]
fn item_lifecycle_accept_dismiss_reopen_with_revisions() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    store::upsert_policy(
        conn,
        "test-client",
        "op_test",
        &billing_policy(true),
        "p1",
        1_500,
    )
    .expect("policy");
    emit_for_inbound_message(conn, "test-client", &message("m1", "billing"), 2_000).expect("emit");

    let outcome = store::apply_item_action(
        conn,
        action_ctx("a1", Some(1)),
        "wi_email_m1",
        ItemAction::Accept,
    )
    .expect("accept");
    assert!(matches!(
        outcome,
        MutationOutcome::Applied { revision: 2, .. }
    ));

    // Stale revision conflicts.
    let conflict = store::apply_item_action(
        conn,
        action_ctx("a2", Some(1)),
        "wi_email_m1",
        ItemAction::Dismiss,
    )
    .expect("conflict path");
    assert!(matches!(conflict, MutationOutcome::RevisionConflict { .. }));

    store::apply_item_action(
        conn,
        action_ctx("a3", Some(2)),
        "wi_email_m1",
        ItemAction::Reopen,
    )
    .expect("reopen");

    let open = store::list_items(
        persistence.connection_ref(),
        "test-client",
        Some(WorkItemStatus::Open),
        10,
        &crate::http::OperatorScope::All,
    )
    .expect("list open");
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].revision, 3);

    let err = store::apply_item_action(
        persistence.connection(),
        action_ctx("a4", None),
        "missing",
        ItemAction::Accept,
    )
    .expect_err("missing item");
    assert!(err.to_string().contains("work_item_not_found"));
}

#[test]
fn agent_launch_request_is_receipted_and_idempotent() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let job = super::agent_launch::build_outbox_job(
        "wi_email_m1",
        "launch-1",
        "http://monitor.local",
        "BusinessOS work item wi_email_m1",
        "prompt",
        "/home/example/projects/BusinessOS",
    )
    .expect("job");
    let payload: serde_json::Value = serde_json::from_str(&job.payload_json).expect("payload");
    assert_eq!(
        payload
            .pointer("/work_dir")
            .and_then(serde_json::Value::as_str),
        Some("/home/example/projects/BusinessOS")
    );

    let first = store::record_agent_launch_request(
        conn,
        store::AgentLaunchRequestContext {
            client_id: "test-client",
            item_id: "wi_email_m1",
            actor_id: "op_test",
            operator_context: "check the customer history",
            job: &job,
            idempotency_key: "launch-1",
            now_ms: 5_000,
        },
    )
    .expect("first launch receipt");
    assert!(matches!(
        first,
        MutationOutcome::Applied { revision: 1, .. }
    ));

    let replay = store::record_agent_launch_request(
        conn,
        store::AgentLaunchRequestContext {
            client_id: "test-client",
            item_id: "wi_email_m1",
            actor_id: "op_test",
            operator_context: "check the customer history",
            job: &job,
            idempotency_key: "launch-1",
            now_ms: 5_100,
        },
    )
    .expect("replay launch receipt");
    assert!(matches!(
        replay,
        MutationOutcome::ReplayedIdempotent {
            revision: Some(1),
            ..
        }
    ));
    let job_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM outbox_jobs", [], |row| row.get(0))
        .expect("outbox count");
    assert_eq!(job_count, 1);
}

#[test]
fn pump_backfills_items_when_policy_enabled_later() {
    use crate::slices::email_triage::store as triage_store;
    use crate::slices::email_triage::worker::ingest_messages;
    use bos_integrations::gmail_inbox_read::GmailFullMessage;

    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();

    // Seed categories + a rule pinning billing, then ingest WITHOUT a policy.
    triage_store::list_categories(conn, "test-client", 500).expect("seed categories");
    triage_store::upsert_category(
        conn,
        "test-client",
        "op_test",
        &bos_contracts::email_triage::CategoryRecord {
            category_id: "billing".to_string(),
            display_name: "Billing".to_string(),
            description: "Bills".to_string(),
            color: "#10b981".to_string(),
            sort: 40,
            is_system: false,
            default_agent_dir: String::new(),
            default_agent_context: String::new(),
        },
        "c1",
        600,
    )
    .expect("billing category");
    triage_store::upsert(
        conn,
        crate::slices::email_triage::store::RuleMutationContext {
            client_id: "test-client",
            actor_id: "op_test",
            expected_revision: None,
            idempotency_key: "r1",
            correlation_id: None,
            now_ms: 700,
        },
        &bos_contracts::email_triage::EmailTriageRule {
            rule_id: "bills".to_string(),
            conditions: vec![bos_contracts::email_triage::EmailTriageCondition {
                field: bos_contracts::email_triage::EmailTriageField::Subject,
                op: bos_contracts::email_triage::EmailTriageOperator::Contains,
                value: "bill".to_string(),
                header_name: None,
            }],
            conditions_v2: Vec::new(),
            match_mode: bos_contracts::email_triage::EmailTriageMatchMode::All,
            priority: 1,
            enabled: true,
            pinned_category: "billing".to_string(),
        },
    )
    .expect("rule");

    let gmail = vec![GmailFullMessage {
        message_id: "m1".to_string(),
        thread_id: None,
        label_ids: vec![],
        internal_date_epoch_ms: Some(1_000),
        subject: Some("Your bill is ready".to_string()),
        from: Some("billing@business-7c4184030e.test".to_string()),
        to: None,
        headers: vec![],
        plain_text_body: "pay us".to_string(),
        html_body: None,
        attachments: Vec::new(),
    }];
    ingest_messages(conn, "test-client", Some("user_jordan"), &gmail, 1_000).expect("ingest");
    assert!(
        store::list_items(
            persistence.connection_ref(),
            "test-client",
            None,
            10,
            &crate::http::OperatorScope::All
        )
        .expect("list")
        .is_empty(),
        "no policy yet -> no items"
    );

    // Enable the policy AFTER ingestion; the next poll over the same batch
    // backfills the item.
    let conn = persistence.connection();
    store::upsert_policy(
        conn,
        "test-client",
        "op_test",
        &billing_policy(true),
        "p1",
        2_000,
    )
    .expect("policy");
    let summary =
        ingest_messages(conn, "test-client", Some("user_jordan"), &gmail, 3_000).expect("re-poll");
    assert_eq!(summary.ingested, 0);
    let items = store::list_items(
        persistence.connection_ref(),
        "test-client",
        None,
        10,
        &crate::http::OperatorScope::All,
    )
    .expect("list");
    assert_eq!(items.len(), 1, "backfill emitted the item");
    assert_eq!(items[0].item.category_id, "billing");
    assert_eq!(
        items[0].item.source_user_id.as_deref(),
        Some("user_jordan"),
        "the item carries the connected account it came from"
    );
}

#[test]
fn policy_rejects_packet_kinds_not_in_catalog() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let err = store::upsert_policy(
        conn,
        "test-client",
        "op_test",
        &WorkQueuePolicy {
            category_id: "billing".to_string(),
            create_work_item: true,
            packet_kinds: vec!["made_up_kind".to_string()],
            ai_suggestible_packet_kinds: Vec::new(),
            ai_suggestible_gmail_scope: Default::default(),
            ai_suggestible_gmail_categories: Vec::new(),
            auto_produce: false,
        },
        "p1",
        1_000,
    )
    .expect_err("unknown kind");
    assert!(err.to_string().contains("work_queue_packet_kind_unknown"));

    // Catalog kinds pass.
    store::upsert_policy(
        conn,
        "test-client",
        "op_test",
        &WorkQueuePolicy {
            category_id: "billing".to_string(),
            create_work_item: true,
            packet_kinds: vec![
                "follow_up_task".to_string(),
                "email_draft_reply".to_string(),
            ],
            ai_suggestible_packet_kinds: Vec::new(),
            ai_suggestible_gmail_scope: Default::default(),
            ai_suggestible_gmail_categories: Vec::new(),
            auto_produce: false,
        },
        "p2",
        2_000,
    )
    .expect("catalog kinds");
    assert_eq!(crate::slices::work_queue::packet_kind_catalog().len(), 10);
}

#[tokio::test]
async fn packet_kind_catalog_route_respects_enabled_slices() {
    let router = build_router(test_state_configured(
        None,
        &[
            crate::slices::work_queue::SLICE.id,
            crate::slices::email_drafts::SLICE.id,
        ],
    ));

    let response = router
        .oneshot(
            Request::get("/api/work-queue/packet-kinds")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), 200);

    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let body: PacketKindsResponse = serde_json::from_slice(&bytes).expect("packet kinds body");
    let kinds: Vec<String> = body.kinds.into_iter().map(|kind| kind.kind_id).collect();
    assert_eq!(kinds, vec!["email_draft_reply".to_string()]);
}

#[tokio::test]
async fn queue_route_supports_attention_filter() {
    let state = test_state_configured(None, &[crate::slices::work_queue::SLICE.id]);
    {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        store::upsert_policy(
            conn,
            "test-client",
            "op_test",
            &billing_policy(true),
            "p1",
            1_500,
        )
        .expect("policy");
        emit_for_inbound_message(conn, "test-client", &message("open", "billing"), 2_000)
            .expect("emit open");
        emit_for_inbound_message(conn, "test-client", &message("staged", "billing"), 2_050)
            .expect("emit staged");
        emit_for_inbound_message(conn, "test-client", &message("quiet", "billing"), 2_100)
            .expect("emit quiet");
        store::apply_item_action(
            conn,
            action_ctx("accept_staged_route", Some(1)),
            "wi_email_staged",
            ItemAction::Accept,
        )
        .expect("accept staged");
        store::apply_item_action(
            conn,
            action_ctx("accept_quiet_route", Some(1)),
            "wi_email_quiet",
            ItemAction::Accept,
        )
        .expect("accept quiet");
        crate::slices::follow_up_tasks::store::insert_draft(
            conn,
            "test-client",
            "op_test",
            &bos_contracts::follow_up_tasks::FollowUpDraft {
                draft_id: "fud_wi_email_staged_route_1".to_string(),
                item_id: "wi_email_staged".to_string(),
                source_kind: "email".to_string(),
                source_ref: "staged".to_string(),
                source_user_id: None,
                status: bos_contracts::follow_up_tasks::FollowUpDraftStatus::Staged,
                title: "Follow up".to_string(),
                due_date: None,
                context: "Needs review.".to_string(),
                provenance: Vec::new(),
                model: "test-model".to_string(),
                confidence: "high".to_string(),
                task_id: None,
                created_at_ms: 3_000,
                updated_at_ms: 3_000,
            },
            "draft_staged_route",
        )
        .expect("stage draft");
    }
    let router = build_router(state);

    async fn queue_ids(router: axum::Router, uri: &str) -> Vec<String> {
        let response = router
            .oneshot(Request::get(uri).body(Body::empty()).expect("request"))
            .await
            .expect("response");
        assert_eq!(response.status(), 200);
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        let body: WorkQueueResponse = serde_json::from_slice(&bytes).expect("queue body");
        body.items
            .into_iter()
            .map(|entry| entry.item.item_id)
            .collect()
    }

    let all = queue_ids(router.clone(), "/api/work-queue").await;
    assert_eq!(all.len(), 3);
    let false_filter = queue_ids(router.clone(), "/api/work-queue?needs_attention=false").await;
    assert_eq!(false_filter, all);

    let attention = queue_ids(router.clone(), "/api/work-queue?needs_attention=true").await;
    assert_eq!(
        attention,
        vec!["wi_email_staged".to_string(), "wi_email_open".to_string()]
    );

    let accepted_attention = queue_ids(
        router,
        "/api/work-queue?status=accepted&needs_attention=true",
    )
    .await;
    assert_eq!(accepted_attention, vec!["wi_email_staged".to_string()]);
}

#[tokio::test]
async fn queue_attention_filter_applies_before_route_limit() {
    let state = test_state_configured(None, &[crate::slices::work_queue::SLICE.id]);
    {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        store::upsert_policy(
            conn,
            "test-client",
            "op_test",
            &billing_policy(true),
            "p1_attention_limit",
            1_500,
        )
        .expect("policy");
        emit_for_inbound_message(
            conn,
            "test-client",
            &message("old_attention", "billing"),
            1_000,
        )
        .expect("emit old attention");
        for idx in 0..201 {
            let source = format!("quiet_{idx:03}");
            emit_for_inbound_message(
                conn,
                "test-client",
                &message(&source, "billing"),
                2_000 + idx,
            )
            .expect("emit quiet");
            store::apply_item_action(
                conn,
                action_ctx(&format!("accept_quiet_{idx:03}"), Some(1)),
                &format!("wi_email_{source}"),
                ItemAction::Accept,
            )
            .expect("accept quiet");
        }
    }
    let router = build_router(state);

    let response = router
        .oneshot(
            Request::get("/api/work-queue?needs_attention=true")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), 200);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let body: WorkQueueResponse = serde_json::from_slice(&bytes).expect("queue body");
    let ids: Vec<String> = body
        .items
        .into_iter()
        .map(|entry| entry.item.item_id)
        .collect();
    assert_eq!(ids, vec!["wi_email_old_attention".to_string()]);
}

#[tokio::test]
async fn queue_source_attention_filter_applies_before_route_limit() {
    use bos_contracts::email_identity::{AttentionLevel, AttentionSignal, ParsedInbound};

    let state = test_state_configured(None, &[crate::slices::work_queue::SLICE.id]);
    {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        store::upsert_policy(
            conn,
            "test-client",
            "op_test",
            &billing_policy(true),
            "p1_source_attention_limit",
            1_500,
        )
        .expect("policy");
        emit_for_inbound_message(
            conn,
            "test-client",
            &message("old_source_attention", "billing"),
            1_000,
        )
        .expect("emit old source attention");
        crate::slices::email_triage::store::upsert_inbound_enrichment(
            conn,
            crate::slices::email_triage::store::InboundEnrichmentWrite {
                client_id: "test-client",
                source_key: "old_source_attention",
                parser_id: "test_parser",
                parser_version: "1",
                parsed: &ParsedInbound {
                    attention_signals: vec![AttentionSignal {
                        level: AttentionLevel::Higher,
                        reason_code: "callback_needed".to_string(),
                        label: Some("Needs callback".to_string()),
                        detail: None,
                        provenance: "test".to_string(),
                    }],
                    ..ParsedInbound::default()
                },
                now_ms: 1_100,
            },
        )
        .expect("source attention enrichment");
        for idx in 0..201 {
            let source = format!("quiet_source_{idx:03}");
            emit_for_inbound_message(
                conn,
                "test-client",
                &message(&source, "billing"),
                2_000 + idx,
            )
            .expect("emit quiet");
        }
    }
    let router = build_router(state);

    let response = router
        .oneshot(
            Request::get("/api/work-queue?attention_level=higher")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), 200);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let body: WorkQueueResponse = serde_json::from_slice(&bytes).expect("queue body");
    let ids: Vec<String> = body
        .items
        .into_iter()
        .map(|entry| entry.item.item_id)
        .collect();
    assert_eq!(ids, vec!["wi_email_old_source_attention".to_string()]);
}

#[tokio::test]
async fn item_action_with_personal_token_does_not_deadlock() {
    let state = test_state_configured(None, &[]);
    {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        crate::slices::operator_users::store::create_user(
            conn,
            "test-client",
            "operator",
            &personal_operator(),
            "bosu_tok_jordan",
            "user_1",
        )
        .expect("operator user");
        store::upsert_policy(
            conn,
            "test-client",
            "op_test",
            &billing_policy(true),
            "p1",
            1_500,
        )
        .expect("policy");
        let mut scoped_message = message("m1", "billing");
        scoped_message.source_user_id = Some("user_jordan".to_string());
        emit_for_inbound_message(conn, "test-client", &scoped_message, 2_000).expect("emit");
    }
    let router = build_router(state.clone());

    let response = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        router.oneshot(
            Request::post("/api/work-queue/wi_email_m1/action")
                .header("authorization", "Bearer bosu_tok_jordan")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "actor_id": "spoofed",
                        "action": "accept",
                        "expected_revision": 1,
                        "idempotency_key": "accept_1"
                    })
                    .to_string(),
                ))
                .expect("request"),
        ),
    )
    .await
    .expect("route timed out")
    .expect("response");
    assert_eq!(response.status(), 200);

    let persistence = state.persistence.lock();
    let receipts = crate::store_core::receipts_for_entity(
        persistence.connection_ref(),
        "test-client",
        store::ITEM_ENTITY_KIND,
        "wi_email_m1",
        10,
    )
    .expect("receipts");
    assert_eq!(receipts[0].actor_id, "user_jordan");
}

#[test]
fn policy_persists_ai_suggestible_packet_kinds_separately() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    store::upsert_policy(
        conn,
        "test-client",
        "op_test",
        &WorkQueuePolicy {
            category_id: "billing".to_string(),
            create_work_item: true,
            packet_kinds: vec!["crm_activity".to_string()],
            ai_suggestible_packet_kinds: vec![
                "follow_up_task".to_string(),
                "email_draft_reply".to_string(),
            ],
            ai_suggestible_gmail_scope: Default::default(),
            ai_suggestible_gmail_categories: Vec::new(),
            auto_produce: false,
        },
        "p1",
        1_000,
    )
    .expect("policy");

    let policy = store::policy_for_category(persistence.connection_ref(), "test-client", "billing")
        .expect("load")
        .expect("policy");
    assert_eq!(policy.packet_kinds, vec!["crm_activity".to_string()]);
    assert_eq!(
        policy.ai_suggestible_packet_kinds,
        vec![
            "follow_up_task".to_string(),
            "email_draft_reply".to_string()
        ]
    );
    assert!(policy.ai_suggestible_gmail_categories.is_empty());

    let err = store::upsert_policy(
        persistence.connection(),
        "test-client",
        "op_test",
        &WorkQueuePolicy {
            category_id: "billing".to_string(),
            create_work_item: true,
            packet_kinds: Vec::new(),
            ai_suggestible_packet_kinds: vec!["made_up_kind".to_string()],
            ai_suggestible_gmail_scope: Default::default(),
            ai_suggestible_gmail_categories: Vec::new(),
            auto_produce: false,
        },
        "p2",
        2_000,
    )
    .expect_err("unknown optional kind");
    assert!(err.to_string().contains("work_queue_packet_kind_unknown"));

    store::upsert_policy(
        persistence.connection(),
        "test-client",
        "op_test",
        &WorkQueuePolicy {
            category_id: "billing".to_string(),
            create_work_item: true,
            packet_kinds: Vec::new(),
            ai_suggestible_packet_kinds: vec![
                bos_contracts::work_queue::AI_SUGGEST_ALL_SENTINEL.to_string()
            ],
            ai_suggestible_gmail_scope: Default::default(),
            ai_suggestible_gmail_categories: Vec::new(),
            auto_produce: false,
        },
        "p3",
        3_000,
    )
    .expect("sentinel-only policy");

    let err = store::upsert_policy(
        persistence.connection(),
        "test-client",
        "op_test",
        &WorkQueuePolicy {
            category_id: "billing".to_string(),
            create_work_item: true,
            packet_kinds: Vec::new(),
            ai_suggestible_packet_kinds: vec![
                bos_contracts::work_queue::AI_SUGGEST_ALL_SENTINEL.to_string(),
                "follow_up_task".to_string(),
            ],
            ai_suggestible_gmail_scope: Default::default(),
            ai_suggestible_gmail_categories: Vec::new(),
            auto_produce: false,
        },
        "p4",
        4_000,
    )
    .expect_err("sentinel must not be mixed with explicit ids");
    assert!(err
        .to_string()
        .contains("work_queue_ai_suggest_all_exclusive"));
}

#[test]
fn policy_persists_fallback_ai_gmail_scope_and_rejects_other_categories() {
    use bos_contracts::email_triage::{EmailTriageGmailCategory, FALLBACK_CATEGORY_ID};
    use bos_contracts::work_queue::WorkQueueAiGmailScope;

    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    store::upsert_policy(
        conn,
        "test-client",
        "op_test",
        &WorkQueuePolicy {
            category_id: FALLBACK_CATEGORY_ID.to_string(),
            create_work_item: true,
            packet_kinds: Vec::new(),
            ai_suggestible_packet_kinds: vec!["follow_up_task".to_string()],
            ai_suggestible_gmail_scope: Default::default(),
            ai_suggestible_gmail_categories: vec![
                EmailTriageGmailCategory::Primary,
                EmailTriageGmailCategory::Updates,
            ],
            auto_produce: false,
        },
        "fallback_ai_scope",
        1_000,
    )
    .expect("policy");
    let policy = store::policy_for_category(
        persistence.connection_ref(),
        "test-client",
        FALLBACK_CATEGORY_ID,
    )
    .expect("load")
    .expect("policy");
    assert_eq!(
        policy.ai_suggestible_gmail_categories,
        vec![
            EmailTriageGmailCategory::Primary,
            EmailTriageGmailCategory::Updates
        ]
    );
    assert_eq!(
        policy.ai_suggestible_gmail_scope,
        WorkQueueAiGmailScope::Default
    );

    store::upsert_policy(
        persistence.connection(),
        "test-client",
        "op_test",
        &WorkQueuePolicy {
            category_id: FALLBACK_CATEGORY_ID.to_string(),
            create_work_item: true,
            packet_kinds: Vec::new(),
            ai_suggestible_packet_kinds: vec!["follow_up_task".to_string()],
            ai_suggestible_gmail_scope: WorkQueueAiGmailScope::All,
            ai_suggestible_gmail_categories: vec![EmailTriageGmailCategory::Primary],
            auto_produce: false,
        },
        "fallback_ai_scope_all",
        1_300,
    )
    .expect("all fallback scope");
    let policy = store::policy_for_category(
        persistence.connection_ref(),
        "test-client",
        FALLBACK_CATEGORY_ID,
    )
    .expect("load all")
    .expect("policy");
    assert_eq!(
        policy.ai_suggestible_gmail_scope,
        WorkQueueAiGmailScope::All
    );
    assert!(policy.ai_suggestible_gmail_categories.is_empty());

    store::upsert_policy(
        persistence.connection(),
        "test-client",
        "op_test",
        &WorkQueuePolicy {
            category_id: FALLBACK_CATEGORY_ID.to_string(),
            create_work_item: true,
            packet_kinds: Vec::new(),
            ai_suggestible_packet_kinds: vec!["follow_up_task".to_string()],
            ai_suggestible_gmail_scope: WorkQueueAiGmailScope::Selected,
            ai_suggestible_gmail_categories: vec![EmailTriageGmailCategory::Social],
            auto_produce: false,
        },
        "fallback_ai_scope_selected",
        1_350,
    )
    .expect("selected fallback scope");
    let policy = store::policy_for_category(
        persistence.connection_ref(),
        "test-client",
        FALLBACK_CATEGORY_ID,
    )
    .expect("load selected")
    .expect("policy");
    assert_eq!(
        policy.ai_suggestible_gmail_scope,
        WorkQueueAiGmailScope::Selected
    );
    assert_eq!(
        policy.ai_suggestible_gmail_categories,
        vec![EmailTriageGmailCategory::Social]
    );

    let err = store::upsert_policy(
        persistence.connection(),
        "test-client",
        "op_test",
        &WorkQueuePolicy {
            category_id: FALLBACK_CATEGORY_ID.to_string(),
            create_work_item: true,
            packet_kinds: Vec::new(),
            ai_suggestible_packet_kinds: vec!["follow_up_task".to_string()],
            ai_suggestible_gmail_scope: WorkQueueAiGmailScope::Selected,
            ai_suggestible_gmail_categories: Vec::new(),
            auto_produce: false,
        },
        "fallback_ai_scope_selected_empty",
        1_400,
    )
    .expect_err("selected scope needs categories");
    assert!(err
        .to_string()
        .contains("work_queue_ai_gmail_scope_selected_empty"));

    store::upsert_policy(
        persistence.connection(),
        "test-client",
        "op_test",
        &WorkQueuePolicy {
            category_id: FALLBACK_CATEGORY_ID.to_string(),
            create_work_item: true,
            packet_kinds: Vec::new(),
            ai_suggestible_packet_kinds: vec!["follow_up_task".to_string()],
            ai_suggestible_gmail_scope: Default::default(),
            ai_suggestible_gmail_categories: Vec::new(),
            auto_produce: false,
        },
        "fallback_ai_scope_default",
        1_250,
    )
    .expect("default fallback scope");
    let policy = store::policy_for_category(
        persistence.connection_ref(),
        "test-client",
        FALLBACK_CATEGORY_ID,
    )
    .expect("load defaulted")
    .expect("policy");
    assert_eq!(
        policy.ai_suggestible_gmail_categories,
        vec![
            EmailTriageGmailCategory::Primary,
            EmailTriageGmailCategory::Updates
        ]
    );

    store::upsert_policy(
        persistence.connection(),
        "test-client",
        "op_test",
        &WorkQueuePolicy {
            category_id: "billing".to_string(),
            create_work_item: true,
            packet_kinds: Vec::new(),
            ai_suggestible_packet_kinds: Vec::new(),
            ai_suggestible_gmail_scope: Default::default(),
            ai_suggestible_gmail_categories: vec![EmailTriageGmailCategory::Primary],
            auto_produce: false,
        },
        "stale_scope_ai_off",
        1_500,
    )
    .expect("stale scope cleared when ai off");
    let billing =
        store::policy_for_category(persistence.connection_ref(), "test-client", "billing")
            .expect("load billing")
            .expect("billing policy");
    assert!(billing.ai_suggestible_gmail_categories.is_empty());

    let err = store::upsert_policy(
        persistence.connection(),
        "test-client",
        "op_test",
        &WorkQueuePolicy {
            category_id: "billing".to_string(),
            create_work_item: true,
            packet_kinds: Vec::new(),
            ai_suggestible_packet_kinds: vec!["follow_up_task".to_string()],
            ai_suggestible_gmail_scope: WorkQueueAiGmailScope::All,
            ai_suggestible_gmail_categories: Vec::new(),
            auto_produce: false,
        },
        "bad_scope",
        2_000,
    )
    .expect_err("non-fallback scope");
    assert!(err
        .to_string()
        .contains("work_queue_ai_gmail_scope_fallback_only"));
}

#[test]
fn policy_turning_work_items_off_clears_outputs_and_ai_suggestions() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    store::upsert_policy(
        conn,
        "test-client",
        "op_test",
        &WorkQueuePolicy {
            category_id: "billing".to_string(),
            create_work_item: false,
            packet_kinds: vec!["crm_activity".to_string()],
            ai_suggestible_packet_kinds: vec!["follow_up_task".to_string()],
            ai_suggestible_gmail_scope: Default::default(),
            ai_suggestible_gmail_categories: Vec::new(),
            auto_produce: false,
        },
        "p1",
        1_000,
    )
    .expect("policy");

    let policy = store::policy_for_category(persistence.connection_ref(), "test-client", "billing")
        .expect("load")
        .expect("policy");
    assert!(!policy.create_work_item);
    assert!(policy.packet_kinds.is_empty());
    assert!(policy.ai_suggestible_packet_kinds.is_empty());
    assert!(policy.ai_suggestible_gmail_categories.is_empty());
    assert_eq!(policy.ai_suggestible_gmail_scope, Default::default());
    assert!(!policy.auto_produce);
}

#[test]
fn packet_kinds_are_editable_until_dismissed() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    store::upsert_policy(
        conn,
        "test-client",
        "op_test",
        &WorkQueuePolicy {
            category_id: "billing".to_string(),
            create_work_item: true,
            packet_kinds: vec![
                "calendar_event_draft".to_string(),
                "crm_activity".to_string(),
            ],
            ai_suggestible_packet_kinds: Vec::new(),
            ai_suggestible_gmail_scope: Default::default(),
            ai_suggestible_gmail_categories: Vec::new(),
            auto_produce: false,
        },
        "p1",
        1_000,
    )
    .expect("policy");
    emit_for_inbound_message(conn, "test-client", &message("m1", "billing"), 2_000).expect("emit");

    // Drop one kind; the catalog still guards what goes on.
    store::update_packet_kinds(
        conn,
        action_ctx("k1", Some(1)),
        "wi_email_m1",
        &["calendar_event_draft".to_string()],
    )
    .expect("set kinds");
    let item = store::get_item_unscoped(persistence.connection_ref(), "test-client", "wi_email_m1")
        .expect("get")
        .expect("exists");
    assert_eq!(
        item.item.packet_kinds,
        vec!["calendar_event_draft".to_string()]
    );
    assert_eq!(item.revision, 2);

    let err = store::update_packet_kinds(
        persistence.connection(),
        action_ctx("k2", Some(2)),
        "wi_email_m1",
        &["made_up_kind".to_string()],
    )
    .expect_err("unknown kind");
    assert!(err.to_string().contains("work_queue_packet_kind_unknown"));

    // Dismissed items refuse kind edits.
    store::apply_item_action(
        persistence.connection(),
        action_ctx("a1", Some(2)),
        "wi_email_m1",
        ItemAction::Dismiss,
    )
    .expect("dismiss");
    let err = store::update_packet_kinds(
        persistence.connection(),
        action_ctx("k3", Some(3)),
        "wi_email_m1",
        &["crm_activity".to_string()],
    )
    .expect_err("dismissed");
    assert!(err.to_string().contains("work_item_kinds_not_editable"));
}

#[test]
fn manual_follow_up_creates_or_updates_email_work_item() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let msg = message("manual1", "billing");

    let created = super::service::add_manual_follow_up_for_email(
        conn,
        ItemActionContext {
            client_id: "test-client",
            actor_id: "op_test",
            scope: &crate::http::OperatorScope::All,
            expected_revision: None,
            idempotency_key: "manual_1",
            now_ms: 2_000,
        },
        &msg,
        &crate::overlay::WorkQueueOverlay::default(),
    )
    .expect("create");
    assert!(matches!(
        created,
        MutationOutcome::Applied { revision: 1, .. }
    ));
    let item = store::get_item_unscoped(
        persistence.connection_ref(),
        "test-client",
        "wi_email_manual1",
    )
    .expect("get")
    .expect("item");
    assert_eq!(item.item.packet_kinds, vec!["follow_up_task".to_string()]);

    let mut existing = message("manual2", "billing");
    existing.subject = Some("Needs CRM log".to_string());
    store::upsert_policy(
        persistence.connection(),
        "test-client",
        "op_test",
        &WorkQueuePolicy {
            category_id: "billing".to_string(),
            create_work_item: true,
            packet_kinds: vec!["crm_activity".to_string()],
            ai_suggestible_packet_kinds: Vec::new(),
            ai_suggestible_gmail_scope: Default::default(),
            ai_suggestible_gmail_categories: Vec::new(),
            auto_produce: false,
        },
        "policy_manual",
        2_500,
    )
    .expect("policy");
    emit_for_inbound_message(persistence.connection(), "test-client", &existing, 3_000)
        .expect("emit");
    let updated = super::service::add_manual_follow_up_for_email(
        persistence.connection(),
        ItemActionContext {
            client_id: "test-client",
            actor_id: "op_test",
            scope: &crate::http::OperatorScope::All,
            expected_revision: None,
            idempotency_key: "manual_2",
            now_ms: 4_000,
        },
        &existing,
        &crate::overlay::WorkQueueOverlay::default(),
    )
    .expect("update");
    assert!(matches!(
        updated,
        MutationOutcome::Applied { revision: 2, .. }
    ));
    let item = store::get_item_unscoped(
        persistence.connection_ref(),
        "test-client",
        "wi_email_manual2",
    )
    .expect("get")
    .expect("item");
    assert_eq!(
        item.item.packet_kinds,
        vec!["crm_activity".to_string(), "follow_up_task".to_string()]
    );
}

#[test]
fn produce_guidance_is_revisioned_and_editable_until_dismissed() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    store::upsert_policy(
        conn,
        "test-client",
        "op_test",
        &billing_policy(true),
        "p1",
        1_000,
    )
    .expect("policy");
    emit_for_inbound_message(conn, "test-client", &message("m1", "billing"), 2_000).expect("emit");

    let outcome = store::update_produce_guidance(
        conn,
        action_ctx("g1", Some(1)),
        "wi_email_m1",
        "  Mention the PO number and keep it concise.  ",
    )
    .expect("set guidance");
    assert!(matches!(
        outcome,
        MutationOutcome::Applied { revision: 2, .. }
    ));
    let item = store::get_item_unscoped(persistence.connection_ref(), "test-client", "wi_email_m1")
        .expect("get")
        .expect("exists");
    assert_eq!(
        item.item.produce_guidance,
        "Mention the PO number and keep it concise."
    );
    assert_eq!(item.revision, 2);

    store::update_produce_guidance(
        persistence.connection(),
        action_ctx("g2", Some(2)),
        "wi_email_m1",
        "",
    )
    .expect("clear guidance");
    let item = store::get_item_unscoped(persistence.connection_ref(), "test-client", "wi_email_m1")
        .expect("get")
        .expect("exists");
    assert!(item.item.produce_guidance.is_empty());
    assert_eq!(item.revision, 3);

    store::apply_item_action(
        persistence.connection(),
        action_ctx("a1", Some(3)),
        "wi_email_m1",
        ItemAction::Dismiss,
    )
    .expect("dismiss");
    let err = store::update_produce_guidance(
        persistence.connection(),
        action_ctx("g3", Some(4)),
        "wi_email_m1",
        "revive this",
    )
    .expect_err("dismissed");
    assert!(err.to_string().contains("work_item_guidance_not_editable"));
}

#[test]
fn feed_decorates_items_with_staged_draft_kinds() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    store::upsert_policy(
        conn,
        "test-client",
        "op_test",
        &WorkQueuePolicy {
            category_id: "billing".to_string(),
            create_work_item: true,
            packet_kinds: vec!["follow_up_task".to_string()],
            ai_suggestible_packet_kinds: Vec::new(),
            ai_suggestible_gmail_scope: Default::default(),
            ai_suggestible_gmail_categories: Vec::new(),
            auto_produce: false,
        },
        "p1",
        1_000,
    )
    .expect("policy");
    assert!(
        emit_for_inbound_message(conn, "test-client", &message("m1", "billing"), 2_000)
            .expect("emit m1")
    );
    assert!(
        emit_for_inbound_message(conn, "test-client", &message("m2", "billing"), 2_100)
            .expect("emit m2")
    );
    store::apply_item_action(
        conn,
        action_ctx("a1", Some(1)),
        "wi_email_m1",
        ItemAction::Accept,
    )
    .expect("accept");
    crate::slices::follow_up_tasks::store::insert_draft(
        conn,
        "test-client",
        "op_test",
        &bos_contracts::follow_up_tasks::FollowUpDraft {
            draft_id: "fud_wi_email_m1_1".to_string(),
            item_id: "wi_email_m1".to_string(),
            source_kind: "email".to_string(),
            source_ref: "m1".to_string(),
            source_user_id: None,
            status: bos_contracts::follow_up_tasks::FollowUpDraftStatus::Staged,
            title: "Pay the bill".to_string(),
            due_date: None,
            context: "Bill is ready.".to_string(),
            provenance: Vec::new(),
            model: "test-model".to_string(),
            confidence: "high".to_string(),
            task_id: None,
            created_at_ms: 3_000,
            updated_at_ms: 3_000,
        },
        "d1",
    )
    .expect("stage draft");

    let feed = queue_feed(
        conn,
        10_000,
        false,
        false,
        &std::collections::HashSet::new(),
    );
    let by_id: std::collections::HashMap<_, _> = feed
        .iter()
        .map(|e| (e.item.item_id.clone(), e.staged_draft_kinds.clone()))
        .collect();
    assert_eq!(by_id["wi_email_m1"], vec!["follow_up_task".to_string()]);
    assert!(by_id["wi_email_m2"].is_empty());

    // Dismissing the item archives the queue row even if a staged draft still
    // exists. The operator cannot open draft panels from a dismissed row, so
    // the feed must not keep it in "needs you" via stale draft decoration.
    store::apply_item_action(
        conn,
        action_ctx("a2", Some(2)),
        "wi_email_m1",
        ItemAction::Dismiss,
    )
    .expect("dismiss");
    let feed = queue_feed(
        conn,
        10_000,
        false,
        false,
        &std::collections::HashSet::new(),
    );
    let dismissed = feed
        .iter()
        .find(|e| e.item.item_id == "wi_email_m1")
        .expect("dismissed item in all feed");
    assert_eq!(dismissed.item.status, WorkItemStatus::Dismissed);
    assert!(dismissed.staged_draft_kinds.is_empty());
}

#[test]
fn feed_hides_source_scoped_staged_draft_kinds_for_other_visible_users() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let mut item = shared_visible_work_item(
        "wi_email_m1",
        Some("user_casey"),
        &["user_casey", "user_jordan"],
    );
    item.packet_kinds = vec!["follow_up_task".to_string()];
    store::insert_item(conn, "test-client", &item).expect("insert item");
    store::apply_item_action(
        conn,
        action_ctx("accept", Some(1)),
        "wi_email_m1",
        ItemAction::Accept,
    )
    .expect("accept");
    crate::slices::follow_up_tasks::store::insert_draft(
        conn,
        "test-client",
        "op_test",
        &bos_contracts::follow_up_tasks::FollowUpDraft {
            draft_id: "fud_wi_email_m1_1".to_string(),
            item_id: "wi_email_m1".to_string(),
            source_kind: "email".to_string(),
            source_ref: "m1".to_string(),
            source_user_id: Some("user_casey".to_string()),
            status: bos_contracts::follow_up_tasks::FollowUpDraftStatus::Staged,
            title: "Call back".to_string(),
            due_date: None,
            context: "Needs a reply.".to_string(),
            provenance: Vec::new(),
            model: "test-model".to_string(),
            confidence: "high".to_string(),
            task_id: None,
            created_at_ms: 3_000,
            updated_at_ms: 3_000,
        },
        "draft",
    )
    .expect("stage draft");

    let all_feed = queue_feed_for_scope(conn, &crate::http::OperatorScope::All);
    let all_item = all_feed
        .iter()
        .find(|entry| entry.item.item_id == "wi_email_m1")
        .expect("all scope item");
    assert_eq!(all_item.staged_draft_kinds, vec!["follow_up_task"]);

    let casey_feed = queue_feed_for_scope(
        conn,
        &crate::http::OperatorScope::User("user_casey".to_string()),
    );
    let casey_item = casey_feed
        .iter()
        .find(|entry| entry.item.item_id == "wi_email_m1")
        .expect("casey scope item");
    assert_eq!(casey_item.staged_draft_kinds, vec!["follow_up_task"]);

    let jordan_feed = queue_feed_for_scope(
        conn,
        &crate::http::OperatorScope::User("user_jordan".to_string()),
    );
    let jordan_item = jordan_feed
        .iter()
        .find(|entry| entry.item.item_id == "wi_email_m1")
        .expect("jordan visible item");
    assert!(jordan_item.staged_draft_kinds.is_empty());
}

#[test]
fn attention_filtered_feed_keeps_only_operator_attention_rows() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    store::upsert_policy(
        conn,
        "test-client",
        "op_test",
        &WorkQueuePolicy {
            category_id: "billing".to_string(),
            create_work_item: true,
            packet_kinds: vec!["follow_up_task".to_string(), "crm_activity".to_string()],
            ai_suggestible_packet_kinds: Vec::new(),
            ai_suggestible_gmail_scope: Default::default(),
            ai_suggestible_gmail_categories: Vec::new(),
            auto_produce: true,
        },
        "p1",
        1_000,
    )
    .expect("policy");
    for id in ["open", "staged", "pending", "failed", "quiet", "dismissed"] {
        emit_for_inbound_message(conn, "test-client", &message(id, "billing"), 2_000)
            .expect("emit");
    }
    for (source, key) in [
        ("staged", "accept_staged"),
        ("pending", "accept_pending"),
        ("failed", "accept_failed"),
        ("quiet", "accept_quiet"),
        ("dismissed", "accept_dismissed"),
    ] {
        store::apply_item_action(
            conn,
            action_ctx(key, Some(1)),
            &format!("wi_email_{source}"),
            ItemAction::Accept,
        )
        .expect("accept");
    }
    store::apply_item_action(
        conn,
        action_ctx("dismiss_after_accept", None),
        "wi_email_dismissed",
        ItemAction::Dismiss,
    )
    .expect("dismiss");
    crate::slices::follow_up_tasks::store::insert_draft(
        conn,
        "test-client",
        "op_test",
        &bos_contracts::follow_up_tasks::FollowUpDraft {
            draft_id: "fud_wi_email_staged_1".to_string(),
            item_id: "wi_email_staged".to_string(),
            source_kind: "email".to_string(),
            source_ref: "staged".to_string(),
            source_user_id: None,
            status: bos_contracts::follow_up_tasks::FollowUpDraftStatus::Staged,
            title: "Follow up".to_string(),
            due_date: None,
            context: "Needs review.".to_string(),
            provenance: Vec::new(),
            model: "test-model".to_string(),
            confidence: "high".to_string(),
            task_id: None,
            created_at_ms: 3_000,
            updated_at_ms: 3_000,
        },
        "draft_staged",
    )
    .expect("stage draft");
    crate::slices::ai_usage::store::insert_usage(
        conn,
        "test-client",
        &usage_insert(
            "aiu_failed_attention",
            "wi_email_failed",
            crate::slices::crm_drafts::service::FILL_PURPOSE,
            false,
            Some("llm_timeout"),
            4_000,
        ),
    )
    .expect("usage failure");
    let mut in_flight = std::collections::HashSet::new();
    in_flight.insert(("wi_email_pending".to_string(), "crm_activity".to_string()));

    let attention = queue_feed_with_attention_filter(conn, 5_000, false, false, true, &in_flight);
    let ids: std::collections::HashSet<_> = attention
        .iter()
        .map(|entry| entry.item.item_id.as_str())
        .collect();
    assert_eq!(
        ids,
        std::collections::HashSet::from([
            "wi_email_open",
            "wi_email_staged",
            "wi_email_pending",
            "wi_email_failed",
        ])
    );
    assert!(attention.iter().all(super::service::needs_attention));
}

#[test]
fn feed_surfaces_failed_produce_attempts_until_retry_succeeds() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    store::upsert_policy(
        conn,
        "test-client",
        "op_test",
        &WorkQueuePolicy {
            category_id: "billing".to_string(),
            create_work_item: true,
            packet_kinds: vec!["invoice_draft".to_string()],
            ai_suggestible_packet_kinds: Vec::new(),
            ai_suggestible_gmail_scope: Default::default(),
            ai_suggestible_gmail_categories: Vec::new(),
            auto_produce: true,
        },
        "p1",
        1_000,
    )
    .expect("policy");
    emit_for_inbound_message(conn, "test-client", &message("m1", "billing"), 2_000).expect("emit");
    store::apply_item_action(
        conn,
        action_ctx("a1", Some(1)),
        "wi_email_m1",
        ItemAction::Accept,
    )
    .expect("accept");

    let now = 10_000;
    crate::slices::ai_usage::store::insert_usage(
        conn,
        "test-client",
        &usage_insert(
            "aiu_fail",
            "wi_email_m1",
            crate::slices::invoice_drafts::service::FILL_PURPOSE,
            false,
            Some("llm_timeout"),
            now,
        ),
    )
    .expect("usage failure");

    let feed = queue_feed(
        conn,
        now + 2,
        false,
        false,
        &std::collections::HashSet::new(),
    );
    let item = feed
        .iter()
        .find(|e| e.item.item_id == "wi_email_m1")
        .expect("item");
    assert_eq!(item.failure_notifications.len(), 1);
    assert_eq!(
        item.failure_notifications[0].packet_kind.as_deref(),
        Some("invoice_draft")
    );
    assert_eq!(
        item.failure_notifications[0].error_code.as_deref(),
        Some("llm_timeout")
    );
    assert_eq!(item.failure_notifications[0].source, "ai_produce");
    assert!(item.failure_notifications[0].diagnostic_id.is_none());
    assert!(item.failure_notifications[0].diagnostic_href.is_none());

    crate::slices::ai_usage::store::insert_usage(
        conn,
        "test-client",
        &usage_insert(
            "aiu_success",
            "wi_email_m1",
            crate::slices::invoice_drafts::service::FILL_PURPOSE,
            true,
            None,
            now + 1,
        ),
    )
    .expect("usage success");
    let feed = queue_feed(
        conn,
        now + 2,
        false,
        false,
        &std::collections::HashSet::new(),
    );
    let item = feed
        .iter()
        .find(|e| e.item.item_id == "wi_email_m1")
        .expect("item");
    assert!(item.failure_notifications.is_empty());
}

#[test]
fn feed_surfaces_running_smart_draft_as_pending_outputs() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    store::upsert_policy(
        conn,
        "test-client",
        "op_test",
        &WorkQueuePolicy {
            category_id: "billing".to_string(),
            create_work_item: true,
            packet_kinds: vec!["email_draft_reply".to_string()],
            ai_suggestible_packet_kinds: Vec::new(),
            ai_suggestible_gmail_scope: Default::default(),
            ai_suggestible_gmail_categories: Vec::new(),
            auto_produce: false,
        },
        "p1",
        1_000,
    )
    .expect("policy");
    emit_for_inbound_message(conn, "test-client", &message("m1", "billing"), 2_000).expect("emit");
    store::apply_item_action(
        conn,
        action_ctx("accept", Some(1)),
        "wi_email_m1",
        ItemAction::Accept,
    )
    .expect("accept");
    insert_packet_proposal_run(
        conn,
        PacketProposalRunFixture {
            run_id: "ppr_running",
            item_id: "wi_email_m1",
            status: PacketProposalRunStatus::Running,
            candidates: &["email_draft_reply", "calendar_event_draft"],
            outcomes: &[],
            error_code: None,
            actor_id: "op_test",
            now_ms: 5_100,
        },
    );

    let feed = queue_feed(conn, 5_200, false, false, &std::collections::HashSet::new());
    let item = feed
        .iter()
        .find(|e| e.item.item_id == "wi_email_m1")
        .expect("item");
    assert_eq!(
        item.pending_produce_kinds,
        vec![
            "email_draft_reply".to_string(),
            "calendar_event_draft".to_string()
        ]
    );
    assert!(item.failure_notifications.is_empty());
}

#[test]
fn feed_hides_automatic_smart_draft_completed_with_no_reviewable_drafts() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    store::upsert_policy(
        conn,
        "test-client",
        "op_test",
        &WorkQueuePolicy {
            category_id: "billing".to_string(),
            create_work_item: true,
            packet_kinds: vec!["email_draft_reply".to_string()],
            ai_suggestible_packet_kinds: Vec::new(),
            ai_suggestible_gmail_scope: Default::default(),
            ai_suggestible_gmail_categories: Vec::new(),
            auto_produce: false,
        },
        "p1",
        1_000,
    )
    .expect("policy");
    emit_for_inbound_message(conn, "test-client", &message("m1", "billing"), 2_000).expect("emit");
    store::apply_item_action(
        conn,
        action_ctx("accept", Some(1)),
        "wi_email_m1",
        ItemAction::Accept,
    )
    .expect("accept");
    insert_packet_proposal_run(
        conn,
        PacketProposalRunFixture {
            run_id: "ppr_no_drafts",
            item_id: "wi_email_m1",
            status: PacketProposalRunStatus::Completed,
            candidates: &["email_draft_reply", "calendar_event_draft"],
            outcomes: &[
                PacketProposalKindOutcome {
                    packet_kind: "email_draft_reply".to_string(),
                    status: PacketProposalKindOutcomeStatus::Unavailable,
                    reason_code: Some(PacketProposalReasonCode::KindNotRequested),
                    message: None,
                    draft_id: None,
                },
                PacketProposalKindOutcome {
                    packet_kind: "calendar_event_draft".to_string(),
                    status: PacketProposalKindOutcomeStatus::Unavailable,
                    reason_code: Some(PacketProposalReasonCode::LowConfidence),
                    message: None,
                    draft_id: None,
                },
            ],
            error_code: None,
            actor_id: "email_ai_triage",
            now_ms: 5_100,
        },
    );

    let feed = queue_feed(conn, 5_200, false, true, &std::collections::HashSet::new());
    let item = feed
        .iter()
        .find(|e| e.item.item_id == "wi_email_m1")
        .expect("item");
    assert!(item.pending_produce_kinds.is_empty());
    assert!(item.failure_notifications.is_empty());
}

#[test]
fn feed_hides_automatic_smart_draft_completed_with_actionable_failure() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    store::upsert_policy(
        conn,
        "test-client",
        "op_test",
        &WorkQueuePolicy {
            category_id: "billing".to_string(),
            create_work_item: true,
            packet_kinds: vec!["email_draft_reply".to_string()],
            ai_suggestible_packet_kinds: Vec::new(),
            ai_suggestible_gmail_scope: Default::default(),
            ai_suggestible_gmail_categories: Vec::new(),
            auto_produce: false,
        },
        "p1",
        1_000,
    )
    .expect("policy");
    emit_for_inbound_message(conn, "test-client", &message("m1", "billing"), 2_000).expect("emit");
    store::apply_item_action(
        conn,
        action_ctx("accept", Some(1)),
        "wi_email_m1",
        ItemAction::Accept,
    )
    .expect("accept");
    insert_packet_proposal_run(
        conn,
        PacketProposalRunFixture {
            run_id: "ppr_auto_gate_failure",
            item_id: "wi_email_m1",
            status: PacketProposalRunStatus::Completed,
            candidates: &["email_draft_reply"],
            outcomes: &[PacketProposalKindOutcome {
                packet_kind: "email_draft_reply".to_string(),
                status: PacketProposalKindOutcomeStatus::RejectedByGate,
                reason_code: Some(PacketProposalReasonCode::GateRejected),
                message: None,
                draft_id: None,
            }],
            error_code: None,
            actor_id: "email_ai_triage",
            now_ms: 5_100,
        },
    );

    let feed = queue_feed(conn, 5_200, false, true, &std::collections::HashSet::new());
    let item = feed
        .iter()
        .find(|e| e.item.item_id == "wi_email_m1")
        .expect("item");
    assert!(item.pending_produce_kinds.is_empty());
    assert!(item.failure_notifications.is_empty());
}

#[test]
fn feed_surfaces_operator_smart_draft_completed_with_no_reviewable_drafts() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    store::upsert_policy(
        conn,
        "test-client",
        "op_test",
        &WorkQueuePolicy {
            category_id: "billing".to_string(),
            create_work_item: true,
            packet_kinds: vec!["email_draft_reply".to_string()],
            ai_suggestible_packet_kinds: Vec::new(),
            ai_suggestible_gmail_scope: Default::default(),
            ai_suggestible_gmail_categories: Vec::new(),
            auto_produce: false,
        },
        "p1",
        1_000,
    )
    .expect("policy");
    emit_for_inbound_message(conn, "test-client", &message("m1", "billing"), 2_000).expect("emit");
    store::apply_item_action(
        conn,
        action_ctx("accept", Some(1)),
        "wi_email_m1",
        ItemAction::Accept,
    )
    .expect("accept");
    insert_packet_proposal_run(
        conn,
        PacketProposalRunFixture {
            run_id: "ppr_no_drafts",
            item_id: "wi_email_m1",
            status: PacketProposalRunStatus::Completed,
            candidates: &["email_draft_reply"],
            outcomes: &[PacketProposalKindOutcome {
                packet_kind: "email_draft_reply".to_string(),
                status: PacketProposalKindOutcomeStatus::RejectedByGate,
                reason_code: Some(PacketProposalReasonCode::GateRejected),
                message: None,
                draft_id: None,
            }],
            error_code: None,
            actor_id: "op_test",
            now_ms: 5_100,
        },
    );

    let feed = queue_feed(conn, 5_200, false, true, &std::collections::HashSet::new());
    let item = feed
        .iter()
        .find(|e| e.item.item_id == "wi_email_m1")
        .expect("item");
    assert!(item.pending_produce_kinds.is_empty());
    assert_eq!(item.failure_notifications.len(), 1);
    let notification = &item.failure_notifications[0];
    assert_eq!(notification.source, "smart_draft");
    assert_eq!(notification.title, "Smart draft produced no drafts");
    assert!(notification.message.contains("Email draft"));
    assert!(notification.message.contains("gate rejected"));
}

#[test]
fn feed_hides_old_smart_draft_failure_while_retry_is_running() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    store::upsert_policy(
        conn,
        "test-client",
        "op_test",
        &WorkQueuePolicy {
            category_id: "billing".to_string(),
            create_work_item: true,
            packet_kinds: vec!["email_draft_reply".to_string()],
            ai_suggestible_packet_kinds: Vec::new(),
            ai_suggestible_gmail_scope: Default::default(),
            ai_suggestible_gmail_categories: Vec::new(),
            auto_produce: false,
        },
        "p1",
        1_000,
    )
    .expect("policy");
    emit_for_inbound_message(conn, "test-client", &message("m1", "billing"), 2_000).expect("emit");
    store::apply_item_action(
        conn,
        action_ctx("accept", Some(1)),
        "wi_email_m1",
        ItemAction::Accept,
    )
    .expect("accept");
    insert_packet_proposal_run(
        conn,
        PacketProposalRunFixture {
            run_id: "ppr_old_no_drafts",
            item_id: "wi_email_m1",
            status: PacketProposalRunStatus::Completed,
            candidates: &["email_draft_reply"],
            outcomes: &[PacketProposalKindOutcome {
                packet_kind: "email_draft_reply".to_string(),
                status: PacketProposalKindOutcomeStatus::RejectedByGate,
                reason_code: Some(PacketProposalReasonCode::GateRejected),
                message: Some("missing grounded reply body".to_string()),
                draft_id: None,
            }],
            error_code: None,
            actor_id: "op_test",
            now_ms: 5_000,
        },
    );
    insert_packet_proposal_run(
        conn,
        PacketProposalRunFixture {
            run_id: "ppr_retry_running",
            item_id: "wi_email_m1",
            status: PacketProposalRunStatus::Running,
            candidates: &["email_draft_reply"],
            outcomes: &[],
            error_code: None,
            actor_id: "op_test",
            now_ms: 5_100,
        },
    );

    let feed = queue_feed(conn, 5_200, false, true, &std::collections::HashSet::new());
    let item = feed
        .iter()
        .find(|e| e.item.item_id == "wi_email_m1")
        .expect("item");
    assert_eq!(item.pending_produce_kinds, vec!["email_draft_reply"]);
    assert!(item.failure_notifications.is_empty());
}

#[test]
fn feed_surfaces_outbox_failures_with_debug_links_when_enabled() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    store::upsert_policy(
        conn,
        "test-client",
        "op_test",
        &WorkQueuePolicy {
            category_id: "billing".to_string(),
            create_work_item: true,
            packet_kinds: vec!["calendar_event_draft".to_string()],
            ai_suggestible_packet_kinds: Vec::new(),
            ai_suggestible_gmail_scope: Default::default(),
            ai_suggestible_gmail_categories: Vec::new(),
            auto_produce: false,
        },
        "p1",
        1_000,
    )
    .expect("policy");
    emit_for_inbound_message(conn, "test-client", &message("m1", "billing"), 2_000).expect("emit");
    store::apply_item_action(
        conn,
        action_ctx("a1", Some(1)),
        "wi_email_m1",
        ItemAction::Accept,
    )
    .expect("accept");

    let draft = bos_contracts::calendar_drafts::CalendarEventDraft {
        draft_id: "ced_wi_email_m1_1".to_string(),
        item_id: "wi_email_m1".to_string(),
        source_kind: "email".to_string(),
        source_ref: "m1".to_string(),
        source_user_id: None,
        status: bos_contracts::calendar_drafts::CalendarDraftStatus::Staged,
        title: "Site visit".to_string(),
        start_at: "2026-06-12T16:00:00-04:00".to_string(),
        end_at: "2026-06-12T17:00:00-04:00".to_string(),
        timezone: Some("America/New_York".to_string()),
        location: None,
        calendar_id: None,
        description: None,
        attendees: Vec::new(),
        send_invitations: false,
        provenance: Vec::new(),
        model: "test-model".to_string(),
        confidence: "high".to_string(),
        outbox_job_id: None,
        created_at_ms: 3_000,
        updated_at_ms: 3_000,
    };
    crate::slices::calendar_drafts::store::insert_draft(
        conn,
        "test-client",
        "op_test",
        &draft,
        "produce_calendar",
    )
    .expect("stage calendar draft");
    let job = crate::slices::calendar_drafts::service::build_approval_job(
        &draft, "op_test", "op_test", 4_000, "primary",
    )
    .expect("approval job");
    crate::slices::calendar_drafts::store::approve_draft(
        conn,
        crate::slices::calendar_drafts::store::DraftActionContext {
            client_id: "test-client",
            actor_id: "op_test",
            scope: &ALL_SCOPE,
            expected_revision: None,
            idempotency_key: "approve_calendar",
            now_ms: 4_000,
        },
        &draft.draft_id,
        &job,
    )
    .expect("approve");
    let claimed =
        crate::outbox::claim_due_jobs(conn, "test-client", None, 60_000, 10, 4_500).expect("claim");
    assert_eq!(claimed.len(), 1);
    crate::outbox::record_attempt(
        conn,
        "test-client",
        &claimed[0],
        &crate::outbox::AttemptOutcome::Terminal {
            error: "calendar auth expired".to_string(),
            result_json: None,
        },
        5_000,
    )
    .expect("record failure");
    let wrong_kind_job = crate::outbox::NewOutboxJob {
        job_id: "obj_wrong_kind_same_id".to_string(),
        provider: "gmail".to_string(),
        capability: "create_report_draft".to_string(),
        payload_json: "{}".to_string(),
        source_entity_kind: "owner_report".to_string(),
        source_entity_id: draft.draft_id.clone(),
        correlation_id: None,
        causation_id: None,
        idempotency_key: "wrong_kind".to_string(),
    };
    crate::store_core::mutate(
        conn,
        crate::store_core::MutationRequest {
            client_id: "test-client",
            entity_kind: "test_outbox_seed",
            entity_id: "wrong_kind_same_id",
            change_kind: "insert",
            actor_id: "test",
            actor_kind: bos_contracts::receipt::ActorKindDto::System,
            expected_revision: None,
            idempotency_key: "seed_wrong_kind",
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: None,
            now_ms: 5_050,
        },
        |tx| crate::outbox::enqueue_within(tx, "test-client", &wrong_kind_job, 5_050),
    )
    .expect("enqueue wrong-kind outbox job");
    let wrong_kind_claimed =
        crate::outbox::claim_due_jobs(conn, "test-client", None, 60_000, 10, 5_060)
            .expect("claim wrong-kind job");
    assert_eq!(wrong_kind_claimed.len(), 1);
    assert_eq!(wrong_kind_claimed[0].job_id, "obj_wrong_kind_same_id");
    crate::outbox::record_attempt(
        conn,
        "test-client",
        &wrong_kind_claimed[0],
        &crate::outbox::AttemptOutcome::Terminal {
            error: "wrong source kind".to_string(),
            result_json: None,
        },
        5_070,
    )
    .expect("record wrong-kind failure");

    let feed = queue_feed(conn, 5_100, false, true, &std::collections::HashSet::new());
    let item = feed
        .iter()
        .find(|e| e.item.item_id == "wi_email_m1")
        .expect("item");
    assert_eq!(item.failure_notifications.len(), 1);
    let notification = &item.failure_notifications[0];
    assert_eq!(notification.source, "provider_delivery");
    assert_eq!(
        notification.packet_kind.as_deref(),
        Some("calendar_event_draft")
    );
    assert_eq!(
        notification.diagnostic_id.as_deref(),
        Some("outbox:obj_ced_wi_email_m1_1")
    );
    assert_eq!(
        notification.diagnostic_href.as_deref(),
        Some("#debug/outbox:obj_ced_wi_email_m1_1")
    );
    assert_eq!(notification.title, "Couldn't deliver draft");
    assert_eq!(
        notification.message,
        "We couldn't deliver your Calendar event draft — open the draft panel to see what happened or try again."
    );
    assert_eq!(
        notification.next_action.as_deref(),
        Some("Open the draft panel to retry or see what went wrong.")
    );
    assert_eq!(notification.error_code.as_deref(), Some("failed_terminal"));
}

#[test]
fn auto_produce_candidates_respect_policy_status_and_prior_drafts() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let policy = |auto_produce: bool| WorkQueuePolicy {
        category_id: "billing".to_string(),
        create_work_item: true,
        packet_kinds: vec!["follow_up_task".to_string(), "crm_activity".to_string()],
        ai_suggestible_packet_kinds: Vec::new(),
        ai_suggestible_gmail_scope: Default::default(),
        ai_suggestible_gmail_categories: Vec::new(),
        auto_produce,
    };
    store::upsert_policy(conn, "test-client", "op_test", &policy(true), "p1", 1_000)
        .expect("policy");
    emit_for_inbound_message(conn, "test-client", &message("m1", "billing"), 2_000).expect("m1");
    emit_for_inbound_message(conn, "test-client", &message("m2", "billing"), 2_100).expect("m2");
    store::apply_item_action(
        conn,
        action_ctx("a1", Some(1)),
        "wi_email_m1",
        ItemAction::Accept,
    )
    .expect("accept m1");

    // Only the ACCEPTED item's kinds are candidates; m2 stays open.
    let candidates = crate::produce::collect_auto_produce_candidates(
        persistence.connection_ref(),
        "test-client",
        10,
    )
    .expect("candidates");
    assert_eq!(
        candidates,
        vec![
            ("wi_email_m1".to_string(), "follow_up_task".to_string()),
            ("wi_email_m1".to_string(), "crm_activity".to_string()),
        ]
    );

    // The limit bounds the cycle's LLM spend.
    let capped = crate::produce::collect_auto_produce_candidates(
        persistence.connection_ref(),
        "test-client",
        1,
    )
    .expect("capped");
    assert_eq!(capped.len(), 1);

    // A prior draft (any status) removes that kind — the pump only fills the
    // FIRST draft, never loops after an operator rejects.
    crate::slices::follow_up_tasks::store::insert_draft(
        persistence.connection(),
        "test-client",
        "op_test",
        &bos_contracts::follow_up_tasks::FollowUpDraft {
            draft_id: "fud_wi_email_m1_1".to_string(),
            item_id: "wi_email_m1".to_string(),
            source_kind: "email".to_string(),
            source_ref: "m1".to_string(),
            source_user_id: None,
            status: bos_contracts::follow_up_tasks::FollowUpDraftStatus::Staged,
            title: "Pay the bill".to_string(),
            due_date: None,
            context: "Bill is ready.".to_string(),
            provenance: Vec::new(),
            model: "test-model".to_string(),
            confidence: "high".to_string(),
            task_id: None,
            created_at_ms: 3_000,
            updated_at_ms: 3_000,
        },
        "d1",
    )
    .expect("stage draft");
    let candidates = crate::produce::collect_auto_produce_candidates(
        persistence.connection_ref(),
        "test-client",
        10,
    )
    .expect("candidates after draft");
    assert_eq!(
        candidates,
        vec![("wi_email_m1".to_string(), "crm_activity".to_string())]
    );

    // The feed surfaces the same signal as "drafting…" — but only when the
    // pump is actually running (flag from the route), never a false promise.
    let feed = queue_feed(
        persistence.connection_ref(),
        10_000,
        true,
        false,
        &std::collections::HashSet::new(),
    );
    let m1 = feed
        .iter()
        .find(|e| e.item.item_id == "wi_email_m1")
        .expect("m1 in feed");
    assert_eq!(m1.staged_draft_kinds, vec!["follow_up_task".to_string()]);
    assert_eq!(m1.pending_produce_kinds, vec!["crm_activity".to_string()]);
    let feed_pump_off = queue_feed(
        persistence.connection_ref(),
        10_000,
        false,
        false,
        &std::collections::HashSet::new(),
    );
    assert!(feed_pump_off
        .iter()
        .all(|e| e.pending_produce_kinds.is_empty()));

    // Manual kickoffs (in-flight registry) surface regardless of the pump
    // flag — a clicked "Draft X" shows drafting… even with auto-produce off.
    let mut in_flight = std::collections::HashSet::new();
    in_flight.insert(("wi_email_m1".to_string(), "crm_activity".to_string()));
    let feed_manual = queue_feed(
        persistence.connection_ref(),
        10_000,
        false,
        false,
        &in_flight,
    );
    let m1_manual = feed_manual
        .iter()
        .find(|e| e.item.item_id == "wi_email_m1")
        .expect("m1");
    assert_eq!(
        m1_manual.pending_produce_kinds,
        vec!["crm_activity".to_string()]
    );
    store::apply_item_action(
        persistence.connection(),
        action_ctx("a2", Some(m1_manual.revision)),
        "wi_email_m1",
        ItemAction::Dismiss,
    )
    .expect("dismiss");
    let feed_dismissed = queue_feed(
        persistence.connection_ref(),
        10_000,
        false,
        false,
        &in_flight,
    );
    let m1_dismissed = feed_dismissed
        .iter()
        .find(|e| e.item.item_id == "wi_email_m1")
        .expect("m1 dismissed");
    assert_eq!(m1_dismissed.item.status, WorkItemStatus::Dismissed);
    assert!(m1_dismissed.pending_produce_kinds.is_empty());

    // Auto-produce off = no candidates, whatever the queue holds.
    store::upsert_policy(
        persistence.connection(),
        "test-client",
        "op_test",
        &policy(false),
        "p2",
        4_000,
    )
    .expect("policy off");
    assert!(crate::produce::collect_auto_produce_candidates(
        persistence.connection_ref(),
        "test-client",
        10,
    )
    .expect("candidates off")
    .is_empty());
}

#[test]
fn item_source_resolves_the_note_behind_an_item() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let note = bos_contracts::operator_notes::OperatorNote {
        note_id: "n1".to_string(),
        body: "Dana called — wants the storefront quote by Friday.".to_string(),
        category_id: "operator_note".to_string(),
        created_by: "op_test".to_string(),
        created_at_ms: 1_000,
    };
    crate::slices::operator_notes::store::insert_note(conn, "test-client", &note, "n1")
        .expect("note");
    crate::slices::operator_notes::service::emit_item_for_note(
        conn,
        "test-client",
        &note,
        &crate::slices::operator_notes::service::default_actions(),
        1_000,
    )
    .expect("emit");

    let source = super::service::item_source(
        persistence.connection_ref(),
        "test-client",
        "wi_operator_note_n1",
        &crate::http::OperatorScope::All,
    )
    .unwrap_or_else(|_| panic!("source resolves"));
    assert_eq!(source.source_kind, "operator_note");
    assert!(source.message.body_excerpt.contains("storefront quote"));
    assert!(source.message.from_addr.is_none());

    assert!(matches!(
        super::service::item_source(
            persistence.connection_ref(),
            "test-client",
            "wi_missing",
            &crate::http::OperatorScope::All,
        ),
        Err(super::service::ItemSourceError::ItemNotFound)
    ));
}

#[test]
fn item_source_exposes_full_html_body_for_display() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    store::upsert_policy(
        conn,
        "test-client",
        "op_test",
        &billing_policy(true),
        "policy-html",
        1_000,
    )
    .expect("policy");
    let mut msg = message("html1", "billing");
    msg.body_excerpt = "Short".to_string();
    msg.body_full = "Invoice ready\nOpen the portal.".to_string();
    crate::slices::email_triage::store::record_inbound_message_with_body_html(
        conn,
        "test-client",
        &msg,
        Some("<html><body><h1>Invoice ready</h1><p>Open the portal.</p></body></html>"),
    )
    .expect("record");
    emit_for_inbound_message(conn, "test-client", &msg, 2_000).expect("emit");

    let source = super::service::item_source(
        persistence.connection_ref(),
        "test-client",
        "wi_email_html1",
        &crate::http::OperatorScope::All,
    )
    .unwrap_or_else(|_| panic!("source resolves"));

    assert_eq!(source.source_body_format, WorkItemSourceBodyFormat::Html);
    assert!(source.source_body.contains("<h1>Invoice ready</h1>"));
    assert_eq!(source.message.body_full, "Invoice ready\nOpen the portal.");
}

#[test]
fn item_source_exposes_html_body_captured_by_gmail_ingest() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    store::upsert_policy(
        conn,
        "test-client",
        "op_test",
        &WorkQueuePolicy {
            category_id: bos_contracts::email_triage::FALLBACK_CATEGORY_ID.to_string(),
            create_work_item: true,
            packet_kinds: vec!["ledger_entry".to_string()],
            ai_suggestible_packet_kinds: Vec::new(),
            ai_suggestible_gmail_scope: Default::default(),
            ai_suggestible_gmail_categories: Vec::new(),
            auto_produce: false,
        },
        "policy-gmail-html",
        1_000,
    )
    .expect("policy");
    crate::slices::email_triage::worker::ingest_messages(
        conn,
        "test-client",
        None,
        &[bos_integrations::gmail_inbox_read::GmailFullMessage {
            message_id: "gmail-html-1".to_string(),
            thread_id: None,
            label_ids: vec![],
            internal_date_epoch_ms: Some(1_000),
            subject: Some("Invoice ready".to_string()),
            from: Some("billing@example.test".to_string()),
            to: None,
            headers: vec![],
            plain_text_body: "Invoice ready\nOpen the portal.".to_string(),
            html_body: Some(
                "<html><body><h1>Invoice ready</h1><p>Open the portal.</p></body></html>"
                    .to_string(),
            ),
            attachments: Vec::new(),
        }],
        2_000,
    )
    .expect("ingest");

    let source = super::service::item_source(
        persistence.connection_ref(),
        "test-client",
        "wi_email_gmail-html-1",
        &crate::http::OperatorScope::All,
    )
    .unwrap_or_else(|_| panic!("source resolves"));

    assert_eq!(source.source_body_format, WorkItemSourceBodyFormat::Html);
    assert!(source.source_body.contains("<h1>Invoice ready</h1>"));
    assert_eq!(source.message.body_full, "Invoice ready\nOpen the portal.");
}

mod attention_enrichment_emission {
    use super::*;
    use bos_contracts::email_identity::{
        AttentionLevel, AttentionSignal, IdentityConfidence, ParsedInbound,
        RepresentedPartyCandidate,
    };
    const CATEGORY_ID: &str = "answering_service";

    fn message(id: &str) -> InboundMessageRecord {
        InboundMessageRecord {
            source_key: id.to_string(),
            message_id: id.to_string(),
            thread_id: None,
            internal_date_ms: Some(1_000),
            from_addr: Some("Platform <noreply@example.test>".to_string()),
            to_addr: None,
            subject: Some("Inbound summary".to_string()),
            body_excerpt: "Body".to_string(),
            body_full: String::new(),
            headers: Vec::new(),
            labels: Vec::new(),
            resolved_category: CATEGORY_ID.to_string(),
            matched_rule_id: None,
            ingested_at_ms: 1_000,
            ai_triage_status: None,
            ai_triage_rationale: None,
            attachments: Vec::new(),
            source_user_id: None,
        }
    }

    fn policy(create_work_item: bool, packet_kinds: Vec<&str>) -> WorkQueuePolicy {
        WorkQueuePolicy {
            category_id: CATEGORY_ID.to_string(),
            create_work_item,
            packet_kinds: packet_kinds.into_iter().map(str::to_string).collect(),
            ai_suggestible_packet_kinds: Vec::new(),
            ai_suggestible_gmail_scope: Default::default(),
            ai_suggestible_gmail_categories: Vec::new(),
            auto_produce: false,
        }
    }

    fn upsert_policy(conn: &mut rusqlite::Connection, policy: WorkQueuePolicy) {
        store::upsert_policy(
            conn,
            "test-client",
            "op_test",
            &policy,
            &format!("policy:{}", policy.create_work_item),
            1_500,
        )
        .expect("policy");
    }

    fn add_enrichment(
        conn: &mut rusqlite::Connection,
        source_key: &str,
        level: AttentionLevel,
        reason_code: &str,
    ) {
        let parsed = ParsedInbound {
            represented_parties: vec![RepresentedPartyCandidate {
                email: Some("alex@example.test".to_string()),
                name: Some("Alex Rivera".to_string()),
                phone: Some("555-0101".to_string()),
                company: Some("Rivera Design".to_string()),
                provenance: "test".to_string(),
                confidence: IdentityConfidence::High,
            }],
            attention_signals: vec![AttentionSignal {
                level,
                reason_code: reason_code.to_string(),
                label: Some(
                    match level {
                        AttentionLevel::Higher => "Needs attention",
                        AttentionLevel::Normal => "Attention",
                        AttentionLevel::Lower => "Lower attention",
                    }
                    .to_string(),
                ),
                detail: Some("operator attention hint".to_string()),
                provenance: "test".to_string(),
            }],
            title_hint: Some("Call from Alex Rivera".to_string()),
            summary_hint: Some("555-0101 | needs follow-up".to_string()),
        };
        crate::slices::email_triage::store::upsert_inbound_enrichment(
            conn,
            crate::slices::email_triage::store::InboundEnrichmentWrite {
                client_id: "test-client",
                source_key,
                parser_id: "test_parser",
                parser_version: "1",
                parsed: &parsed,
                now_ms: 1_900,
            },
        )
        .expect("enrichment");
    }

    #[test]
    fn no_policy_row_means_no_work_item() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();

        assert!(
            !emit_for_inbound_message(conn, "test-client", &message("m1"), 2_000)
                .expect("no policy")
        );
    }

    #[test]
    fn higher_attention_keeps_policy_kinds_and_display_hints() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        upsert_policy(
            conn,
            policy(
                true,
                vec!["crm_activity", "follow_up_task", "calendar_event_draft"],
            ),
        );
        add_enrichment(conn, "m2", AttentionLevel::Higher, "callback_needed");

        assert!(
            emit_for_inbound_message(conn, "test-client", &message("m2"), 2_000).expect("emit")
        );
        let items = store::list_items(
            persistence.connection_ref(),
            "test-client",
            None,
            10,
            &crate::http::OperatorScope::All,
        )
        .expect("list");
        let item = &items[0].item;
        assert_eq!(
            item.packet_kinds,
            vec!["crm_activity", "follow_up_task", "calendar_event_draft"]
        );
        assert_eq!(item.title, "Call from Alex Rivera");
        assert!(item.summary.contains("555-0101"));
        let in_flight = std::collections::HashSet::new();
        let feed = crate::slices::work_queue::service::feed(
            persistence.connection_ref(),
            "test-client",
            None,
            10,
            &crate::http::OperatorScope::All,
            crate::slices::work_queue::service::FeedOptions {
                now_ms: 3_000,
                auto_produce_running: false,
                debug_enabled: false,
                in_flight: &in_flight,
            },
        )
        .expect("feed");
        let attention = feed[0].attention.as_ref().expect("attention summary");
        assert_eq!(attention.level, AttentionLevel::Higher);
        assert_eq!(attention.label, "Needs attention");
    }

    #[test]
    fn lower_attention_drops_deferred_kinds() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        upsert_policy(
            conn,
            policy(
                true,
                vec!["crm_activity", "follow_up_task", "calendar_event_draft"],
            ),
        );
        add_enrichment(conn, "m3", AttentionLevel::Lower, "call_handled_live");

        assert!(
            emit_for_inbound_message(conn, "test-client", &message("m3"), 2_000).expect("emit")
        );

        let items = store::list_items(
            persistence.connection_ref(),
            "test-client",
            None,
            10,
            &crate::http::OperatorScope::All,
        )
        .expect("list");
        assert_eq!(items[0].item.packet_kinds, vec!["crm_activity"]);
    }

    #[test]
    fn normal_attention_keeps_policy_kinds() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        upsert_policy(
            conn,
            policy(
                true,
                vec!["crm_activity", "follow_up_task", "calendar_event_draft"],
            ),
        );
        add_enrichment(conn, "m5", AttentionLevel::Normal, "neutral_signal");

        assert!(
            emit_for_inbound_message(conn, "test-client", &message("m5"), 2_000).expect("emit")
        );

        let items = store::list_items(
            persistence.connection_ref(),
            "test-client",
            None,
            10,
            &crate::http::OperatorScope::All,
        )
        .expect("list");
        assert_eq!(
            items[0].item.packet_kinds,
            vec!["crm_activity", "follow_up_task", "calendar_event_draft"]
        );
    }

    #[test]
    fn silenced_policy_still_wins_over_enrichment() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        upsert_policy(conn, policy(false, Vec::new()));
        add_enrichment(conn, "m4", AttentionLevel::Higher, "callback_needed");

        assert!(
            !emit_for_inbound_message(conn, "test-client", &message("m4"), 2_000)
                .expect("silenced")
        );
    }
}
