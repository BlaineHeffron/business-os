//! Slice tests: extract parsing/validation, the stage → approve → outbox
//! lifecycle, and dry-run delivery. LLM interactions are tested at the
//! parse/build seams only — no live calls ever run from tests.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use bos_contracts::calendar_drafts::CalendarDraftStatus;
use bos_contracts::operator_notes::OperatorNote;
use bos_contracts::operator_users::OperatorUser;
use bos_contracts::work_queue::{WorkItem, WorkItemStatus};
use bos_integrations::google_calendar::events::GoogleCalendarEventCreateOutboxPayload;
use bos_integrations::GoogleOAuthConfig;
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

use super::service::{self, ExtractOutcome};
use super::store::{self, DraftActionContext};
use super::worker;
use crate::http::{
    build_router,
    test_support::{test_state_configured, EnvGuard},
    OperatorScope,
};
use crate::outbox::{self, AttemptOutcome};
use crate::persistence::Persistence;
use crate::store_core::{MutationOutcome, StoreError};

const CLIENT: &str = "test-client";

fn bg_message() -> bos_contracts::email_triage::InboundMessageRecord {
    bos_contracts::email_triage::InboundMessageRecord {
        source_key: "m_bg".to_string(),
        message_id: "m_bg".to_string(),
        thread_id: None,
        internal_date_ms: Some(1_781_000_000_000),
        from_addr: Some("a@test".to_string()),
        to_addr: Some("b@test".to_string()),
        subject: Some("Meet".to_string()),
        body_excerpt: "Let's meet Tuesday at 2pm.".to_string(),
        body_full: String::new(),
        headers: Vec::new(),
        labels: Vec::new(),
        resolved_category: "operator_note".to_string(),
        matched_rule_id: None,
        ingested_at_ms: 1_000,
        ai_triage_status: None,
        ai_triage_rationale: None,
        attachments: Vec::new(),
        source_user_id: None,
    }
}

#[test]
fn event_extract_request_includes_background_when_present() {
    use bos_integrations::llm_typed_tasks::TypedLlmTextBlock;
    let item = accepted_item();
    let message = bg_message();

    let plain = service::build_event_extract_request(
        CLIENT,
        &item,
        &message,
        &json!({ "background": null }),
        1,
    );
    assert!(!plain
        .input
        .text_blocks
        .iter()
        .any(|b| b.block_id == "background"));

    let block = TypedLlmTextBlock {
        block_id: "background".to_string(),
        text: "Company: Example Company".to_string(),
    };
    let context = json!({ "background": serde_json::to_value(&block).unwrap() });
    let grounded = service::build_event_extract_request(CLIENT, &item, &message, &context, 1);
    let backgrounds: Vec<_> = grounded
        .input
        .text_blocks
        .iter()
        .filter(|b| b.block_id == "background")
        .collect();
    assert_eq!(backgrounds.len(), 1);
    assert_eq!(backgrounds[0].text, "Company: Example Company");
}

fn accepted_item() -> WorkItem {
    WorkItem {
        item_id: "wi_email_m1".to_string(),
        source_kind: "email".to_string(),
        source_ref: "m1".to_string(),
        category_id: "events".to_string(),
        title: "Soccer registration confirmed".to_string(),
        summary: "From coach@business-76e9de2c7e.test — practice Friday".to_string(),
        packet_kinds: vec!["calendar_event_draft".to_string()],
        status: WorkItemStatus::Accepted,
        accept_actor: Some(bos_contracts::work_queue::WorkItemAcceptActor::Operator),
        ai_suggested: true,
        rationale: "registration for a dated event".to_string(),
        produce_guidance: String::new(),
        source_user_id: None,
        assignee_user_id: None,
        visible_to_user_ids: Vec::new(),
        created_at_ms: 1_000,
        updated_at_ms: 1_000,
    }
}

fn valid_extract_response() -> serde_json::Value {
    json!({
        "extractable": true,
        "title": "Soccer practice",
        "start_at": "2026-06-12T16:00:00-04:00",
        "end_at": "2026-06-12T17:00:00-04:00",
        "timezone": "America/New_York",
        "location": "Field 3, Riverside Park",
        "description": "First practice of the season.",
        "attendees": [],
        "confidence": "high",
        "provenance": [
            {"field": "title", "quote": "soccer practice"},
            {"field": "start_at", "quote": "Friday June 12 at 4pm"},
            {"field": "end_at", "quote": ""},
            {"field": "bogus_field", "quote": "dropped"}
        ]
    })
}

#[test]
fn attendee_extract_requires_quote_in_email_evidence_and_supporting_address() {
    let response = json!({
        "extractable": true,
        "title": "Planning call",
        "start_at": "2026-06-12T16:00:00-04:00",
        "end_at": "2026-06-12T17:00:00-04:00",
        "timezone": "America/New_York",
        "location": null,
        "description": null,
        "attendees": [
            {"email": "Coach@business-76e9de2c7e.test", "quote": "Cc: Coach@business-76e9de2c7e.test, helper@example.test"},
            {"email": "invented@example.test", "quote": "invented@example.test"},
            {"email": "other@example.test", "quote": "Cc: Coach@business-76e9de2c7e.test, helper@example.test"},
            {"email": "coach@example.test", "quote": "xcoach@example.test"},
            {"email": "guest@example.test", "quote": "guest@example.test.invalid"}
        ],
        "confidence": "high",
        "provenance": []
    });
    let evidence = "From: sender@example.test\nTo: operator@example.test\nCc: Coach@business-76e9de2c7e.test, helper@example.test\nSubject: Planning call";
    let extract = match service::parse_event_extract_response_with_evidence(
        &response,
        None,
        Some(evidence),
    ) {
        Ok(ExtractOutcome::Event(extract)) => *extract,
        other => panic!("expected event, got {other:?}"),
    };
    assert_eq!(extract.attendees, vec!["Coach@business-76e9de2c7e.test"]);
    assert_eq!(
        extract.provenance[0].field,
        "attendee:Coach@business-76e9de2c7e.test"
    );
}

#[test]
fn attendee_extract_keeps_first_twenty_five_grounded_addresses() {
    let addresses = (0..=bos_integrations::google_calendar::MAX_CALENDAR_ATTENDEES)
        .map(|index| format!("guest{index}@example.test"))
        .collect::<Vec<_>>();
    let evidence = addresses.join(", ");
    let attendees = addresses
        .iter()
        .map(|email| json!({"email": email, "quote": email}))
        .collect::<Vec<_>>();
    let response = json!({
        "extractable": true,
        "title": "Planning call",
        "start_at": "2026-06-12T16:00:00-04:00",
        "end_at": "2026-06-12T17:00:00-04:00",
        "timezone": "America/New_York",
        "location": null,
        "description": null,
        "attendees": attendees,
        "confidence": "high",
        "provenance": []
    });
    let extract =
        match service::parse_event_extract_response_with_evidence(&response, None, Some(&evidence))
        {
            Ok(ExtractOutcome::Event(extract)) => *extract,
            other => panic!("expected event, got {other:?}"),
        };
    assert_eq!(
        extract.attendees.len(),
        bos_integrations::google_calendar::MAX_CALENDAR_ATTENDEES
    );
    assert_eq!(extract.attendees.first(), Some(&addresses[0]));
    assert_eq!(
        extract.attendees.last(),
        Some(&addresses[bos_integrations::google_calendar::MAX_CALENDAR_ATTENDEES - 1])
    );
}

fn staged_draft(conn: &mut rusqlite::Connection) -> String {
    staged_draft_with_source(conn, "wi_email_m1", None, "produce_1")
}

fn staged_draft_with_source(
    conn: &mut rusqlite::Connection,
    item_id: &str,
    source_user_id: Option<&str>,
    idempotency_key: &str,
) -> String {
    staged_draft_with_source_kind(conn, item_id, "email", source_user_id, idempotency_key)
}

fn staged_draft_with_source_kind(
    conn: &mut rusqlite::Connection,
    item_id: &str,
    source_kind: &str,
    source_user_id: Option<&str>,
    idempotency_key: &str,
) -> String {
    let item = accepted_item();
    let item = WorkItem {
        item_id: item_id.to_string(),
        source_kind: source_kind.to_string(),
        source_user_id: source_user_id.map(str::to_string),
        ..item
    };
    let extract = match service::parse_event_extract_response(&valid_extract_response()) {
        Ok(ExtractOutcome::Event(extract)) => *extract,
        other => panic!("expected event, got {other:?}"),
    };
    let draft = service::draft_from_extract(&item, &extract, 1, "test-model", 2_000);
    store::insert_draft(conn, CLIENT, "op_test", &draft, idempotency_key).expect("stage");
    draft.draft_id
}

fn personal_operator(user_id: &str, default_calendar_id: Option<&str>) -> OperatorUser {
    OperatorUser {
        user_id: user_id.to_string(),
        display_name: user_id.to_string(),
        active: true,
        archived_at_ms: None,
        default_calendar_id: default_calendar_id.map(str::to_string),
        created_at_ms: 1_000,
        updated_at_ms: 1_000,
    }
}

fn create_operator(
    conn: &mut rusqlite::Connection,
    user_id: &str,
    token: &str,
    default_calendar_id: Option<&str>,
) {
    crate::slices::operator_users::store::create_user(
        conn,
        CLIENT,
        "operator",
        &personal_operator(user_id, None),
        token,
        &format!("create_{user_id}"),
    )
    .expect("operator user");
    if let Some(calendar_id) = default_calendar_id {
        crate::slices::operator_users::store::set_default_calendar(
            conn,
            crate::slices::operator_users::store::UserActionContext {
                client_id: CLIENT,
                actor_id: "operator",
                expected_revision: None,
                idempotency_key: &format!("calendar_{user_id}"),
                now_ms: 2_000,
            },
            user_id,
            Some(calendar_id),
        )
        .expect("default calendar");
    }
}

fn store_gmail_credential(conn: &mut rusqlite::Connection, user_id: &str) {
    crate::slices::google_connector::store::store_credential(
        conn,
        CLIENT,
        user_id,
        crate::slices::google_connector::SERVICE_GMAIL,
        "refresh-token",
        &["https://www.googleapis.com/auth/gmail.compose".to_string()],
        3_000,
    )
    .expect("credential");
}

fn store_operator_note(conn: &mut rusqlite::Connection, created_by: &str) {
    crate::slices::operator_notes::store::insert_note(
        conn,
        CLIENT,
        &OperatorNote {
            note_id: "m1".to_string(),
            body: "Meet Tuesday at 2pm.".to_string(),
            category_id: "operator_note".to_string(),
            created_by: created_by.to_string(),
            created_at_ms: 1_000,
        },
        &format!("note_{created_by}"),
    )
    .expect("operator note");
}

async fn response_error(response: axum::response::Response) -> String {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json body");
    body.get("error")
        .and_then(serde_json::Value::as_str)
        .expect("error code")
        .to_string()
}

async fn approve_calendar_draft(
    router: axum::Router,
    draft_id: &str,
    actor_id: &str,
    key: &str,
) -> axum::response::Response {
    router
        .oneshot(
            Request::post(format!("/api/calendar-drafts/{draft_id}/action"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "action": "approve",
                        "expected_revision": null,
                        "idempotency_key": key,
                        "actor_id": actor_id
                    })
                    .to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("response")
}

// --- service: parse + validate ---

#[test]
fn parse_valid_extract_keeps_known_provenance_only() {
    let extract = match service::parse_event_extract_response(&valid_extract_response()) {
        Ok(ExtractOutcome::Event(extract)) => *extract,
        other => panic!("expected event, got {other:?}"),
    };
    assert_eq!(extract.title, "Soccer practice");
    assert_eq!(extract.confidence, "high");
    let fields: Vec<&str> = extract
        .provenance
        .iter()
        .map(|p| p.field.as_str())
        .collect();
    assert_eq!(fields, vec!["title", "start_at", "end_at"]);
}

#[test]
fn parse_unextractable_returns_no_event_with_reason() {
    let response = json!({"extractable": false, "reason": "newsletter, no dated event"});
    match service::parse_event_extract_response(&response) {
        Ok(ExtractOutcome::NoEvent { reason }) => {
            assert_eq!(reason, "newsletter, no dated event");
        }
        other => panic!("expected NoEvent, got {other:?}"),
    }
}

#[test]
fn parse_rejects_missing_fields_and_bad_timestamps() {
    let mut missing_title = valid_extract_response();
    missing_title["title"] = json!("");
    assert!(service::parse_event_extract_response(&missing_title).is_err());

    let mut bad_start = valid_extract_response();
    bad_start["start_at"] = json!("June 12 at 4pm");
    assert!(service::parse_event_extract_response(&bad_start).is_err());

    // Offset-less timestamps are rejected: Google would shift them silently.
    let mut no_offset = valid_extract_response();
    no_offset["start_at"] = json!("2026-06-12T16:00:00");
    assert!(service::parse_event_extract_response(&no_offset).is_err());

    let mut bad_confidence = valid_extract_response();
    bad_confidence["confidence"] = json!("certain");
    assert!(service::parse_event_extract_response(&bad_confidence).is_err());
}

#[test]
fn rfc3339_validator_accepts_offsets_and_zulu() {
    for ok in [
        "2026-06-12T16:00:00-04:00",
        "2026-06-12T16:00:00+05:30",
        "2026-06-12T16:00:00Z",
        "2026-06-12T16:00:00.123Z",
    ] {
        assert!(service::is_rfc3339_with_offset(ok), "{ok} should pass");
    }
    for bad in [
        "2026-06-12T16:00:00",
        "2026-06-12 16:00:00Z",
        "2026-06-12T16:00:00.-04:00",
        "tomorrow at 4",
        "2026-06-12T16:00:00-0400",
    ] {
        assert!(!service::is_rfc3339_with_offset(bad), "{bad} should fail");
    }
}

#[test]
fn epoch_to_rfc3339_is_correct() {
    assert_eq!(
        crate::produce::epoch_ms_to_rfc3339_utc(0),
        "1970-01-01T00:00:00Z"
    );
    // 2026-06-10 12:34:56 UTC (`date -u -d '2026-06-10T12:34:56Z' +%s`)
    assert_eq!(
        crate::produce::epoch_ms_to_rfc3339_utc(1_781_094_896_000),
        "2026-06-10T12:34:56Z"
    );
}

#[test]
fn produce_guards_require_accepted_item_with_kind() {
    let mut open_item = accepted_item();
    open_item.status = WorkItemStatus::Open;
    assert!(crate::produce::validate_item_for_kind(&open_item, service::PACKET_KIND).is_err());

    let mut wrong_kind = accepted_item();
    wrong_kind.packet_kinds = vec!["follow_up_task".to_string()];
    assert!(crate::produce::validate_item_for_kind(&wrong_kind, service::PACKET_KIND).is_err());

    assert!(crate::produce::validate_item_for_kind(&accepted_item(), service::PACKET_KIND).is_ok());
}

// --- store: stage → approve/reject lifecycle ---

#[test]
fn draft_from_extract_inherits_item_source_user() {
    let mut item = accepted_item();
    item.source_user_id = Some("user_jordan".to_string());
    let extract = match service::parse_event_extract_response(&valid_extract_response()) {
        Ok(ExtractOutcome::Event(extract)) => *extract,
        other => panic!("expected event, got {other:?}"),
    };

    let draft = service::draft_from_extract(&item, &extract, 1, "test-model", 2_000);

    assert_eq!(draft.source_user_id.as_deref(), Some("user_jordan"));
}

#[test]
fn list_and_get_drafts_apply_operator_scope() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let null_id = staged_draft_with_source(conn, "wi_null", None, "produce_null");
    let u1_id = staged_draft_with_source(conn, "wi_u1", Some("u1"), "produce_u1");
    let u2_id = staged_draft_with_source(conn, "wi_u2", Some("u2"), "produce_u2");

    let all = store::list_drafts(conn, CLIENT, None, 10, &OperatorScope::All).expect("list all");
    let all_ids: std::collections::HashSet<_> = all
        .iter()
        .map(|entry| entry.draft.draft_id.as_str())
        .collect();
    assert!(all_ids.contains(null_id.as_str()));
    assert!(all_ids.contains(u1_id.as_str()));
    assert!(all_ids.contains(u2_id.as_str()));

    let u1_scope = OperatorScope::User("u1".to_string());
    let u1 = store::list_drafts(conn, CLIENT, None, 10, &u1_scope).expect("list u1");
    assert_eq!(u1.len(), 1);
    assert_eq!(u1[0].draft.draft_id, u1_id);

    assert!(store::get_draft(conn, CLIENT, &u1_id, &u1_scope)
        .expect("get own")
        .is_some());
    assert!(store::get_draft(conn, CLIENT, &u2_id, &u1_scope)
        .expect("get other")
        .is_none());
    assert!(store::get_draft(conn, CLIENT, &null_id, &u1_scope)
        .expect("get null")
        .is_none());
}

#[test]
fn draft_mutations_reject_cross_scope_access() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let update_id = staged_draft_with_source(conn, "wi_update_u2", Some("u2"), "produce_update_u2");
    let reject_id = staged_draft_with_source(conn, "wi_reject_u2", Some("u2"), "produce_reject_u2");
    let approve_id =
        staged_draft_with_source(conn, "wi_approve_u2", Some("u2"), "produce_approve_u2");
    let u1_scope = OperatorScope::User("u1".to_string());
    let cross_scope = |key: &'static str| DraftActionContext {
        client_id: CLIENT,
        actor_id: "u1",
        scope: &u1_scope,
        expected_revision: None,
        idempotency_key: key,
        now_ms: 5_000,
    };

    let edit = store::CalendarDraftEdit {
        title: "Blocked edit".to_string(),
        start_at: "2026-06-12T16:00:00-04:00".to_string(),
        end_at: "2026-06-12T17:00:00-04:00".to_string(),
        timezone: Some("America/New_York".to_string()),
        location: None,
        description: None,
        calendar_id: None,
        attendees: Vec::new(),
        send_invitations: false,
    };
    let err = store::update_draft(conn, cross_scope("update_cross"), &update_id, &edit)
        .expect_err("cross-scope update rejected");
    assert!(matches!(err, StoreError::Domain(code) if code == "scope_forbidden"));

    let err = store::reject_draft(conn, cross_scope("reject_cross"), &reject_id)
        .expect_err("cross-scope reject rejected");
    assert!(matches!(err, StoreError::Domain(code) if code == "scope_forbidden"));

    let draft = store::get_draft(conn, CLIENT, &approve_id, &OperatorScope::All)
        .expect("get")
        .expect("exists");
    let job = service::build_approval_job(&draft.draft, "u2", "u1", 5_000, "primary").expect("job");
    let err = store::approve_draft(conn, cross_scope("approve_cross"), &approve_id, &job)
        .expect_err("cross-scope approve rejected");
    assert!(matches!(err, StoreError::Domain(code) if code == "scope_forbidden"));
}

#[test]
fn second_active_draft_for_item_is_refused() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    staged_draft(conn);

    let item = accepted_item();
    let extract = match service::parse_event_extract_response(&valid_extract_response()) {
        Ok(ExtractOutcome::Event(extract)) => *extract,
        other => panic!("expected event, got {other:?}"),
    };
    let second = service::draft_from_extract(&item, &extract, 2, "test-model", 3_000);
    let err = store::insert_draft(conn, CLIENT, "op_test", &second, "produce_2")
        .expect_err("second active draft must be refused");
    match err {
        StoreError::Domain(code) => assert_eq!(code, "calendar_draft_already_active"),
        other => panic!("expected domain error, got {other:?}"),
    }
}

#[test]
fn approve_flips_status_and_enqueues_outbox_job_atomically() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let draft_id = staged_draft(conn);

    let draft = store::get_draft(conn, CLIENT, &draft_id, &OperatorScope::All)
        .expect("get")
        .expect("exists");
    let job = service::build_approval_job(&draft.draft, "op_test", "op_test", 5_000, "primary")
        .expect("job");
    let outcome = store::approve_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "op_test",
            scope: &OperatorScope::All,
            expected_revision: Some(draft.revision),
            idempotency_key: "approve_1",
            now_ms: 5_000,
        },
        &draft_id,
        &job,
    )
    .expect("approve");
    assert!(matches!(outcome, MutationOutcome::Applied { .. }));

    let approved = store::get_draft(conn, CLIENT, &draft_id, &OperatorScope::All)
        .expect("get")
        .expect("exists");
    assert_eq!(approved.draft.status, CalendarDraftStatus::Approved);
    assert_eq!(
        approved.draft.outbox_job_id.as_deref(),
        Some(job.job_id.as_str())
    );
    let summary = approved.outbox_job.expect("job summary joined");
    assert_eq!(summary.status, outbox::STATUS_PENDING);

    // Approving again is not staged anymore → domain error.
    let again = store::approve_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "op_test",
            scope: &OperatorScope::All,
            expected_revision: None,
            idempotency_key: "approve_2",
            now_ms: 6_000,
        },
        &draft_id,
        &job,
    );
    assert!(again.is_err(), "double approve must be refused");
}

#[test]
fn approval_payload_snapshots_attendees_and_invitation_choice() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let draft_id = staged_draft(conn);
    let current = store::get_draft(conn, CLIENT, &draft_id, &OperatorScope::All)
        .expect("get")
        .expect("draft");
    let edit = store::CalendarDraftEdit {
        title: current.draft.title.clone(),
        start_at: current.draft.start_at.clone(),
        end_at: current.draft.end_at.clone(),
        timezone: current.draft.timezone.clone(),
        location: current.draft.location.clone(),
        description: current.draft.description.clone(),
        calendar_id: current.draft.calendar_id.clone(),
        attendees: vec![
            " Coach@business-76e9de2c7e.test ".to_string(),
            "coach@business-76e9de2c7e.test".to_string(),
            "Guest@example.test".to_string(),
        ],
        send_invitations: true,
    };
    store::update_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "op_test",
            scope: &OperatorScope::All,
            expected_revision: Some(current.revision),
            idempotency_key: "edit_attendees",
            now_ms: 4_000,
        },
        &draft_id,
        &edit,
    )
    .expect("edit");
    let saved = store::get_draft(conn, CLIENT, &draft_id, &OperatorScope::All)
        .expect("get")
        .expect("draft");
    assert_eq!(
        saved.draft.attendees,
        vec!["Coach@business-76e9de2c7e.test", "Guest@example.test"]
    );
    let job = service::build_approval_job(&saved.draft, "op_test", "op_test", 5_000, "primary")
        .expect("job");
    let payload: GoogleCalendarEventCreateOutboxPayload =
        serde_json::from_str(&job.payload_json).expect("payload");
    assert_eq!(payload.attendees, saved.draft.attendees);
    assert!(payload.send_invitations);
}

#[test]
fn edit_updates_staged_event_fields_and_validates_times() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let draft_id = staged_draft(conn);
    let ctx = |key: &'static str, rev: Option<u64>| DraftActionContext {
        client_id: CLIENT,
        actor_id: "op_test",
        scope: &OperatorScope::All,
        expected_revision: rev,
        idempotency_key: key,
        now_ms: 5_000,
    };
    let edit = |start: &str| store::CalendarDraftEdit {
        title: "Soccer practice (rescheduled)".to_string(),
        start_at: start.to_string(),
        end_at: "2026-06-13T18:00:00-04:00".to_string(),
        timezone: Some("America/New_York".to_string()),
        location: Some("  ".to_string()), // blank → cleared
        description: Some("Moved to Saturday.".to_string()),
        calendar_id: Some("ops@group.calendar.google.com".to_string()),
        attendees: Vec::new(),
        send_invitations: false,
    };

    let err = store::update_draft(conn, ctx("e1", Some(1)), &draft_id, &edit("Saturday 5pm"))
        .expect_err("bad start");
    assert!(err.to_string().contains("calendar_draft_start_invalid"));

    store::update_draft(
        conn,
        ctx("e2", Some(1)),
        &draft_id,
        &edit("2026-06-13T17:00:00-04:00"),
    )
    .expect("edit");
    let drafts = store::list_drafts(
        persistence.connection_ref(),
        CLIENT,
        None,
        10,
        &OperatorScope::All,
    )
    .expect("list");
    let edited = &drafts[0].draft;
    assert_eq!(edited.title, "Soccer practice (rescheduled)");
    assert_eq!(edited.start_at, "2026-06-13T17:00:00-04:00");
    assert_eq!(edited.location, None);
    assert_eq!(edited.description.as_deref(), Some("Moved to Saturday."));
    assert_eq!(
        edited.calendar_id.as_deref(),
        Some("ops@group.calendar.google.com")
    );
    assert_eq!(drafts[0].revision, 2);
}

#[test]
fn approval_job_targets_picked_calendar_or_default() {
    let item = accepted_item();
    let extract = match service::parse_event_extract_response(&valid_extract_response()) {
        Ok(ExtractOutcome::Event(extract)) => *extract,
        other => panic!("expected event, got {other:?}"),
    };
    let mut draft = service::draft_from_extract(&item, &extract, 1, "test-model", 2_000);

    // No picked calendar → the server default applies.
    let job = service::build_approval_job(&draft, "op_test", "op_test", 5_000, "ops-default@cal")
        .expect("job");
    let payload: serde_json::Value = serde_json::from_str(&job.payload_json).expect("payload");
    assert_eq!(payload["calendar_id"], json!("ops-default@cal"));

    // Picked calendar wins.
    draft.calendar_id = Some("shared@group.calendar.google.com".to_string());
    let job = service::build_approval_job(&draft, "op_test", "op_test", 5_000, "ops-default@cal")
        .expect("job");
    let payload: serde_json::Value = serde_json::from_str(&job.payload_json).expect("payload");
    assert_eq!(
        payload["calendar_id"],
        json!("shared@group.calendar.google.com")
    );
}

#[test]
fn reject_frees_the_item_for_reproduce() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let draft_id = staged_draft(conn);

    store::reject_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "op_test",
            scope: &OperatorScope::All,
            expected_revision: None,
            idempotency_key: "reject_1",
            now_ms: 5_000,
        },
        &draft_id,
    )
    .expect("reject");

    assert!(store::active_draft_for_item(conn, CLIENT, "wi_email_m1")
        .expect("query")
        .is_none());

    // Re-produce attempt 2 now succeeds.
    let item = accepted_item();
    let extract = match service::parse_event_extract_response(&valid_extract_response()) {
        Ok(ExtractOutcome::Event(extract)) => *extract,
        other => panic!("expected event, got {other:?}"),
    };
    let second = service::draft_from_extract(&item, &extract, 2, "test-model", 6_000);
    store::insert_draft(conn, CLIENT, "op_test", &second, "produce_2")
        .expect("re-produce after reject");
}

#[test]
fn revision_conflict_on_stale_approve() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let draft_id = staged_draft(conn);
    let draft = store::get_draft(conn, CLIENT, &draft_id, &OperatorScope::All)
        .expect("get")
        .expect("exists");
    let job = service::build_approval_job(&draft.draft, "op_test", "op_test", 5_000, "primary")
        .expect("job");
    let outcome = store::approve_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "op_test",
            scope: &OperatorScope::All,
            expected_revision: Some(draft.revision + 7),
            idempotency_key: "approve_stale",
            now_ms: 5_000,
        },
        &draft_id,
        &job,
    )
    .expect("conflict path returns Ok");
    assert!(matches!(outcome, MutationOutcome::RevisionConflict { .. }));

    // Conflict must NOT have enqueued the job.
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM outbox_jobs", [], |row| row.get(0))
        .expect("count");
    assert_eq!(count, 0);
}

// --- worker: gated delivery ---

fn fake_oauth() -> GoogleOAuthConfig {
    GoogleOAuthConfig {
        client_id: "app".to_string(),
        client_secret: "secret".to_string(),
        refresh_token: "refresh".to_string(),
        scopes: vec!["https://www.googleapis.com/auth/calendar.events".to_string()],
        token_url: None,
    }
}

#[test]
fn approved_draft_delivers_dry_run_while_gate_closed() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let draft_id = staged_draft(conn);
    let draft = store::get_draft(conn, CLIENT, &draft_id, &OperatorScope::All)
        .expect("get")
        .expect("exists");
    let job = service::build_approval_job(&draft.draft, "op_test", "op_test", 5_000, "primary")
        .expect("job");
    store::approve_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "op_test",
            scope: &OperatorScope::All,
            expected_revision: None,
            idempotency_key: "approve_1",
            now_ms: 5_000,
        },
        &draft_id,
        &job,
    )
    .expect("approve");

    let claimed = outbox::claim_due_jobs(conn, CLIENT, None, 60_000, 10, 6_000).expect("claim");
    assert_eq!(claimed.len(), 1);

    // write_enabled = false → dry-run client; no network, no provider write.
    let outcome = worker::execute_job(&claimed[0], Some(&fake_oauth()), false, "primary", 6_000);
    let AttemptOutcome::Delivered { result_json } = &outcome else {
        panic!("expected delivered, got {outcome:?}");
    };
    let result: serde_json::Value = serde_json::from_str(result_json).expect("json");
    assert_eq!(result["dry_run"], json!(true));
    assert_eq!(result["attendee_count"], json!(0));
    assert_eq!(result["send_invitations"], json!(false));

    let status =
        outbox::record_attempt(conn, CLIENT, &claimed[0], &outcome, 6_500).expect("record");
    assert_eq!(status, outbox::STATUS_DELIVERED);

    let final_draft = store::get_draft(conn, CLIENT, &draft_id, &OperatorScope::All)
        .expect("get")
        .expect("exists");
    let summary = final_draft.outbox_job.expect("summary");
    assert_eq!(summary.status, outbox::STATUS_DELIVERED);
    assert_eq!(summary.dry_run, Some(true));
}

#[test]
fn delivery_without_credential_schedules_retry() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let draft_id = staged_draft(conn);
    let draft = store::get_draft(conn, CLIENT, &draft_id, &OperatorScope::All)
        .expect("get")
        .expect("exists");
    let job = service::build_approval_job(&draft.draft, "op_test", "op_test", 5_000, "primary")
        .expect("job");
    store::approve_draft(
        conn,
        DraftActionContext {
            client_id: CLIENT,
            actor_id: "op_test",
            scope: &OperatorScope::All,
            expected_revision: None,
            idempotency_key: "approve_1",
            now_ms: 5_000,
        },
        &draft_id,
        &job,
    )
    .expect("approve");
    let claimed = outbox::claim_due_jobs(conn, CLIENT, None, 60_000, 10, 6_000).expect("claim");

    let outcome = worker::execute_job(&claimed[0], None, false, "primary", 6_000);
    assert!(
        matches!(outcome, AttemptOutcome::Retry { .. }),
        "missing credential must retry, got {outcome:?}"
    );
}

#[test]
fn unsupported_job_is_terminal() {
    let job = outbox::ClaimedJob {
        job_id: "obj_x".to_string(),
        provider: "hubspot".to_string(),
        capability: "create_contact".to_string(),
        payload_json: "{}".to_string(),
        attempts: 0,
        source_entity_kind: "x".to_string(),
        source_entity_id: "x".to_string(),
        correlation_id: None,
        idempotency_key: "k".to_string(),
    };
    let outcome = worker::execute_job(&job, Some(&fake_oauth()), false, "primary", 1_000);
    assert!(matches!(outcome, AttemptOutcome::Terminal { .. }));
}

#[test]
fn approval_job_folds_location_into_description() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let draft_id = staged_draft(conn);
    let draft = store::get_draft(conn, CLIENT, &draft_id, &OperatorScope::All)
        .expect("get")
        .expect("exists");
    let job = service::build_approval_job(&draft.draft, "op_test", "op_test", 5_000, "primary")
        .expect("job");
    let payload: serde_json::Value = serde_json::from_str(&job.payload_json).expect("payload");
    assert_eq!(payload["summary"], json!("Soccer practice"));
    assert_eq!(payload["calendar_id"], json!("primary"));
    let description = payload["description"].as_str().expect("description");
    assert!(description.starts_with("Location: Field 3, Riverside Park"));
    assert!(description.contains("First practice of the season."));
    assert_eq!(payload["approval"]["approved_by"], json!("op_test"));
    assert_eq!(
        payload["approval"]["approved_at"],
        json!("1970-01-01T00:00:05Z")
    );
}

#[tokio::test]
async fn approval_route_binds_calendar_job_to_source_user_credential() {
    let state = test_state_configured(None, &[]);
    let draft_id = {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        create_operator(conn, "approver", "tok_approver", Some("approver-calendar"));
        create_operator(conn, "u2", "tok_u2", Some("u2-calendar"));
        store_gmail_credential(conn, "u2");
        staged_draft_with_source(conn, "wi_route_u2", Some("u2"), "produce_route_u2")
    };
    let router = build_router(state.clone());

    let response = approve_calendar_draft(router, &draft_id, "approver", "approve_route_u2").await;
    assert_eq!(response.status(), StatusCode::OK);

    let mut persistence = state.persistence.lock();
    let claimed = outbox::claim_due_jobs(
        persistence.connection(),
        CLIENT,
        None,
        60_000,
        10,
        i64::MAX as u64,
    )
    .expect("claim");
    assert_eq!(claimed.len(), 1);
    let payload: GoogleCalendarEventCreateOutboxPayload =
        serde_json::from_str(&claimed[0].payload_json).expect("payload");
    assert_eq!(payload.credential_user_id.as_deref(), Some("u2"));
    assert_eq!(payload.calendar_id, "u2-calendar");
    assert_eq!(payload.approval.approved_by, "approver");
}

#[tokio::test]
async fn approval_route_uses_approver_credential_for_mcp_operator_note() {
    let _env = EnvGuard::set_many(&[
        ("BOS_GMAIL_OAUTH_CLIENT_ID", "client-id"),
        ("BOS_GMAIL_OAUTH_CLIENT_SECRET", "client-secret"),
    ]);
    let state = test_state_configured(None, &[]);
    let draft_id = {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        create_operator(conn, "approver", "tok_approver", Some("approver-calendar"));
        store_gmail_credential(conn, "approver");
        store_operator_note(conn, "mcp:user_mcp");
        staged_draft_with_source_kind(
            conn,
            "wi_operator_note_mcp",
            crate::slices::work_queue::SOURCE_KIND_OPERATOR_NOTE,
            Some("user_mcp"),
            "produce_operator_note_mcp",
        )
    };
    let router = build_router(state.clone());

    let response =
        approve_calendar_draft(router, &draft_id, "approver", "approve_operator_note_mcp").await;
    assert_eq!(response.status(), StatusCode::OK);

    let mut persistence = state.persistence.lock();
    let claimed = outbox::claim_due_jobs(
        persistence.connection(),
        CLIENT,
        None,
        60_000,
        10,
        i64::MAX as u64,
    )
    .expect("claim");
    assert_eq!(claimed.len(), 1);
    let payload: GoogleCalendarEventCreateOutboxPayload =
        serde_json::from_str(&claimed[0].payload_json).expect("payload");
    assert_eq!(payload.credential_user_id.as_deref(), Some("approver"));
    assert_eq!(payload.calendar_id, "approver-calendar");
    assert_eq!(payload.approval.approved_by, "approver");
}

#[tokio::test]
async fn approval_route_uses_legacy_google_fallback_for_mcp_operator_note() {
    let _env = EnvGuard::set_many(&[
        ("BOS_GMAIL_OAUTH_CLIENT_ID", "client-id"),
        ("BOS_GMAIL_OAUTH_CLIENT_SECRET", "client-secret"),
        ("BOS_GMAIL_OAUTH_REFRESH_TOKEN", "legacy-refresh-token"),
    ]);
    let state = test_state_configured(None, &[]);
    let draft_id = {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        create_operator(conn, "approver", "tok_approver", Some("approver-calendar"));
        store_operator_note(conn, "mcp:user_mcp");
        staged_draft_with_source_kind(
            conn,
            "wi_operator_note_mcp_fallback",
            crate::slices::work_queue::SOURCE_KIND_OPERATOR_NOTE,
            Some("user_mcp"),
            "produce_operator_note_mcp_fallback",
        )
    };
    let router = build_router(state.clone());

    let response = approve_calendar_draft(
        router,
        &draft_id,
        "approver",
        "approve_operator_note_mcp_fallback",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let mut persistence = state.persistence.lock();
    let claimed = outbox::claim_due_jobs(
        persistence.connection(),
        CLIENT,
        None,
        60_000,
        10,
        i64::MAX as u64,
    )
    .expect("claim");
    assert_eq!(claimed.len(), 1);
    let payload: GoogleCalendarEventCreateOutboxPayload =
        serde_json::from_str(&claimed[0].payload_json).expect("payload");
    assert_eq!(payload.credential_user_id.as_deref(), Some("approver"));
    assert_eq!(payload.calendar_id, "approver-calendar");
}

#[tokio::test]
async fn approval_route_does_not_borrow_another_users_credential_for_mcp_note() {
    let _env = EnvGuard::set_many(&[
        ("BOS_GMAIL_OAUTH_CLIENT_ID", "client-id"),
        ("BOS_GMAIL_OAUTH_CLIENT_SECRET", "client-secret"),
    ]);
    let state = test_state_configured(None, &[]);
    let draft_id = {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        create_operator(conn, "approver", "tok_approver", Some("approver-calendar"));
        create_operator(conn, "other", "tok_other", Some("other-calendar"));
        store_gmail_credential(conn, "other");
        store_operator_note(conn, "mcp:user_mcp");
        staged_draft_with_source_kind(
            conn,
            "wi_operator_note_mcp_wrong_credential",
            crate::slices::work_queue::SOURCE_KIND_OPERATOR_NOTE,
            Some("user_mcp"),
            "produce_operator_note_mcp_wrong_credential",
        )
    };
    let router = build_router(state);

    let response = approve_calendar_draft(
        router,
        &draft_id,
        "approver",
        "approve_operator_note_mcp_wrong_credential",
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        response_error(response).await,
        "google_credential_unavailable"
    );
}

#[tokio::test]
async fn approval_route_keeps_manual_operator_note_bound_to_source_user() {
    let _env = EnvGuard::set_many(&[
        ("BOS_GMAIL_OAUTH_CLIENT_ID", "client-id"),
        ("BOS_GMAIL_OAUTH_CLIENT_SECRET", "client-secret"),
    ]);
    let state = test_state_configured(None, &[]);
    let draft_id = {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        create_operator(conn, "author", "tok_author", Some("author-calendar"));
        create_operator(conn, "approver", "tok_approver", Some("approver-calendar"));
        store_gmail_credential(conn, "author");
        store_gmail_credential(conn, "approver");
        store_operator_note(conn, "author");
        staged_draft_with_source_kind(
            conn,
            "wi_operator_note_manual",
            crate::slices::work_queue::SOURCE_KIND_OPERATOR_NOTE,
            Some("author"),
            "produce_operator_note_manual",
        )
    };
    let router = build_router(state.clone());

    let response = approve_calendar_draft(
        router,
        &draft_id,
        "approver",
        "approve_operator_note_manual",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let mut persistence = state.persistence.lock();
    let claimed = outbox::claim_due_jobs(
        persistence.connection(),
        CLIENT,
        None,
        60_000,
        10,
        i64::MAX as u64,
    )
    .expect("claim");
    let payload: GoogleCalendarEventCreateOutboxPayload =
        serde_json::from_str(&claimed[0].payload_json).expect("payload");
    assert_eq!(payload.credential_user_id.as_deref(), Some("author"));
    assert_eq!(payload.calendar_id, "author-calendar");
}

#[tokio::test]
async fn approval_route_rejects_source_bound_calendar_without_source_credential() {
    let state = test_state_configured(None, &[]);
    let draft_id = {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        create_operator(conn, "approver", "tok_approver", Some("approver-calendar"));
        create_operator(conn, "u2", "tok_u2", Some("u2-calendar"));
        staged_draft_with_source(
            conn,
            "wi_route_missing",
            Some("u2"),
            "produce_route_missing",
        )
    };
    let router = build_router(state.clone());

    let response =
        approve_calendar_draft(router, &draft_id, "approver", "approve_route_missing").await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        response_error(response).await,
        "source_user_credential_unavailable"
    );

    let mut persistence = state.persistence.lock();
    let claimed = outbox::claim_due_jobs(
        persistence.connection(),
        CLIENT,
        None,
        60_000,
        10,
        i64::MAX as u64,
    )
    .expect("claim");
    assert!(claimed.is_empty());
}

#[tokio::test]
async fn approval_route_keeps_calendar_legacy_approver_fallback_for_null_draft() {
    let state = test_state_configured(None, &[]);
    let draft_id = {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        create_operator(conn, "approver", "tok_approver", Some("approver-calendar"));
        staged_draft_with_source(conn, "wi_route_null", None, "produce_route_null")
    };
    let router = build_router(state.clone());

    let response =
        approve_calendar_draft(router, &draft_id, "approver", "approve_route_null").await;
    assert_eq!(response.status(), StatusCode::OK);

    let mut persistence = state.persistence.lock();
    let claimed = outbox::claim_due_jobs(
        persistence.connection(),
        CLIENT,
        None,
        60_000,
        10,
        i64::MAX as u64,
    )
    .expect("claim");
    assert_eq!(claimed.len(), 1);
    let payload: GoogleCalendarEventCreateOutboxPayload =
        serde_json::from_str(&claimed[0].payload_json).expect("payload");
    assert_eq!(payload.credential_user_id.as_deref(), Some("approver"));
    assert_eq!(payload.calendar_id, "approver-calendar");
    assert_eq!(payload.approval.approved_by, "approver");
}
