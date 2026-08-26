use axum::body::Body;
use axum::http::{Request, StatusCode};
use bos_contracts::email_triage::InboundMessageRecord;
use bos_contracts::receipt::ActorKindDto;
use bos_contracts::work_queue::{WorkItem, WorkItemSourceBodyFormat, WorkItemStatus};
use http_body_util::BodyExt;
use rusqlite::params;
use tower::ServiceExt;

use super::{service, store};
use crate::http::{
    build_router, test_support::test_state, test_support::test_state_configured, OperatorScope,
};
use crate::store_core::{self, MutationOutcome, MutationRequest};

const DAY_MS: u64 = 24 * 60 * 60 * 1000;

fn inbound(source_key: &str, ingested_at_ms: u64, body: &str) -> InboundMessageRecord {
    InboundMessageRecord {
        source_key: source_key.to_string(),
        message_id: format!("gmail-{source_key}"),
        thread_id: None,
        internal_date_ms: Some(ingested_at_ms as i64),
        from_addr: Some("customer@example.com".to_string()),
        to_addr: Some("operator@example.com".to_string()),
        subject: Some("Retention test".to_string()),
        body_excerpt: format!("excerpt-{source_key}"),
        body_full: body.to_string(),
        headers: Vec::new(),
        labels: Vec::new(),
        resolved_category: "primary".to_string(),
        matched_rule_id: None,
        ingested_at_ms,
        ai_triage_status: None,
        ai_triage_rationale: None,
        attachments: Vec::new(),
        source_user_id: None,
    }
}

fn receipt_request<'a>(
    client_id: &'a str,
    entity_kind: &'a str,
    entity_id: &'a str,
    change_kind: &'a str,
    idempotency_key: &'a str,
    expected_revision: Option<u64>,
    now_ms: u64,
) -> MutationRequest<'a> {
    MutationRequest {
        client_id,
        entity_kind,
        entity_id,
        change_kind,
        actor_id: "test",
        actor_kind: ActorKindDto::System,
        expected_revision,
        idempotency_key,
        correlation_id: None,
        causation_id: None,
        before_json: Some("{\"before\":1}".to_string()),
        after_json: Some("{\"after\":1}".to_string()),
        now_ms,
    }
}

#[test]
fn cycle_clears_only_strictly_old_bodies_and_preserves_excerpt_fallback() {
    let state = test_state();
    let now = 4_000 * DAY_MS;
    let cutoff = now - 90 * DAY_MS;
    {
        let mut persistence = state.persistence.lock();
        for (source_key, ingested_at_ms, body) in [
            ("old", cutoff - 1, "full old body"),
            ("equal", cutoff, "full equal body"),
            ("new", cutoff + 1, "full new body"),
        ] {
            crate::slices::email_triage::store::record_inbound_message_with_body_html(
                persistence.connection(),
                &state.client_id,
                &inbound(source_key, ingested_at_ms, body),
                Some(&format!("<p>{body}</p>")),
            )
            .expect("ingest");
        }
        crate::slices::work_queue::store::insert_item(
            persistence.connection(),
            &state.client_id,
            &WorkItem {
                item_id: "wi_email_old".to_string(),
                source_kind: "email".to_string(),
                source_ref: "old".to_string(),
                category_id: "primary".to_string(),
                title: "Old item".to_string(),
                summary: "Old summary".to_string(),
                packet_kinds: vec!["email_reply".to_string()],
                status: WorkItemStatus::Open,
                accept_actor: None,
                ai_suggested: false,
                rationale: String::new(),
                produce_guidance: String::new(),
                source_user_id: None,
                assignee_user_id: None,
                visible_to_user_ids: Vec::new(),
                created_at_ms: cutoff - 1,
                updated_at_ms: cutoff - 1,
            },
        )
        .expect("work item");
    }

    let report = service::run_cycle(
        &state,
        &service::RetentionConfig {
            enabled: true,
            interval: std::time::Duration::from_secs(21_600),
            email_body_days: 90,
            receipt_payload_days: 3_650,
            batch_size: 2,
            max_rows_per_cycle: 10,
            incremental_vacuum_pages: 0,
        },
        &service::RunActor::system(),
        "test-cycle",
        now,
    );
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.summary.email_bodies_compacted, 1);

    let persistence = state.persistence.lock();
    let conn = persistence.connection_ref();
    for (source_key, expected_full, expected_html) in [
        ("old", "", ""),
        ("equal", "full equal body", "<p>full equal body</p>"),
        ("new", "full new body", "<p>full new body</p>"),
    ] {
        let row: (String, String) = conn
            .query_row(
                "SELECT body_full, body_html FROM email_inbound_messages \
                 WHERE client_id = ?1 AND source_key = ?2",
                params![&state.client_id, source_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("body row");
        assert_eq!(row, (expected_full.to_string(), expected_html.to_string()));
    }
    let source = match crate::slices::work_queue::service::item_source(
        conn,
        &state.client_id,
        "wi_email_old",
        &OperatorScope::All,
    ) {
        Ok(source) => source,
        Err(_) => panic!("source fallback failed"),
    };
    assert_eq!(source.source_body, "excerpt-old");
    assert_eq!(
        source.source_body_format,
        WorkItemSourceBodyFormat::PlainText
    );
}

#[test]
fn receipt_payload_compaction_is_allowlisted_applied_only_and_keeps_rows() {
    let state = test_state();
    let cutoff = 10_000_u64;
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let old_receipt_id = match store_core::mutate(
        conn,
        receipt_request(
            &state.client_id,
            "email_inbound_message",
            "old",
            "ingest",
            "old-applied",
            None,
            cutoff - 1,
        ),
        |_tx| Ok(()),
    )
    .expect("old applied")
    {
        MutationOutcome::Applied { receipt_id, .. } => receipt_id,
        other => panic!("unexpected old outcome: {other:?}"),
    };
    let equal_receipt_id = match store_core::mutate(
        conn,
        receipt_request(
            &state.client_id,
            "email_inbound_message",
            "equal",
            "ingest",
            "equal-applied",
            None,
            cutoff,
        ),
        |_tx| Ok(()),
    )
    .expect("equal applied")
    {
        MutationOutcome::Applied { receipt_id, .. } => receipt_id,
        other => panic!("unexpected equal outcome: {other:?}"),
    };
    let failed_receipt_id = store_core::record_failed_receipt(
        conn,
        receipt_request(
            &state.client_id,
            "email_inbound_message",
            "failed",
            "ingest",
            "failed-receipt",
            None,
            cutoff - 1,
        ),
        "injected",
    )
    .expect("failed receipt");
    let conflict_receipt_id = match store_core::mutate(
        conn,
        receipt_request(
            &state.client_id,
            "email_inbound_message",
            "conflict",
            "ingest",
            "conflict-receipt",
            Some(7),
            cutoff - 1,
        ),
        |_tx| Ok(()),
    )
    .expect("conflict receipt")
    {
        MutationOutcome::RevisionConflict { receipt_id, .. } => receipt_id,
        other => panic!("unexpected conflict outcome: {other:?}"),
    };
    store_core::mutate(
        conn,
        receipt_request(
            &state.client_id,
            "email_inbound_message",
            "replay-base",
            "ingest",
            "replay-receipt",
            None,
            cutoff,
        ),
        |_tx| Ok(()),
    )
    .expect("replay base");
    let replay_receipt_id = match store_core::mutate(
        conn,
        receipt_request(
            &state.client_id,
            "email_inbound_message",
            "replay-attempt",
            "ingest",
            "replay-receipt",
            None,
            cutoff - 1,
        ),
        |_tx| Ok(()),
    )
    .expect("replayed receipt")
    {
        MutationOutcome::ReplayedIdempotent { receipt_id, .. } => receipt_id,
        other => panic!("unexpected replay outcome: {other:?}"),
    };
    let kickoff_receipt_id = match store_core::mutate(
        conn,
        receipt_request(
            &state.client_id,
            "enrichment_run",
            "kickoff",
            "on_demand_kickoff",
            "kickoff-receipt",
            None,
            cutoff - 1,
        ),
        |_tx| Ok(()),
    )
    .expect("kickoff receipt")
    {
        MutationOutcome::Applied { receipt_id, .. } => receipt_id,
        other => panic!("unexpected kickoff outcome: {other:?}"),
    };
    let retry_receipt_id = match store_core::mutate(
        conn,
        receipt_request(
            &state.client_id,
            crate::outbox::JOB_ENTITY_KIND,
            "operator-retry",
            "retry_requested",
            "operator-retry-receipt",
            None,
            cutoff - 1,
        ),
        |_tx| Ok(()),
    )
    .expect("operator retry receipt")
    {
        MutationOutcome::Applied { receipt_id, .. } => receipt_id,
        other => panic!("unexpected retry outcome: {other:?}"),
    };
    let delivery_receipt_id = match store_core::mutate(
        conn,
        receipt_request(
            &state.client_id,
            crate::outbox::JOB_ENTITY_KIND,
            "delivery-attempt",
            "deliver_succeeded",
            "delivery-attempt-receipt",
            None,
            cutoff - 1,
        ),
        |_tx| Ok(()),
    )
    .expect("delivery attempt receipt")
    {
        MutationOutcome::Applied { receipt_id, .. } => receipt_id,
        other => panic!("unexpected delivery outcome: {other:?}"),
    };
    let before_count = conn
        .query_row("SELECT COUNT(*) FROM receipts", [], |row| {
            row.get::<_, i64>(0)
        })
        .expect("receipt count");
    drop(persistence);

    let candidates = {
        let persistence = state.persistence.lock();
        store_core::receipt_payload_compaction_candidates(
            persistence.connection_ref(),
            &state.client_id,
            cutoff,
            service::RECEIPT_PAYLOAD_ENTITY_KINDS,
            service::RECEIPT_PAYLOAD_RESTRICTED_CHANGE_KINDS,
            50,
        )
        .expect("candidates")
    };
    assert_eq!(
        candidates,
        vec![old_receipt_id.clone(), delivery_receipt_id.clone()]
    );
    let mut persistence = state.persistence.lock();
    let outcome = store_core::compact_receipt_payloads(
        persistence.connection(),
        store_core::ReceiptPayloadCompactionBatch {
            client_id: &state.client_id,
            actor_id: "test",
            actor_kind: ActorKindDto::System,
            cutoff_ms: cutoff,
            allowlisted_entity_kinds: service::RECEIPT_PAYLOAD_ENTITY_KINDS,
            restricted_change_kinds: service::RECEIPT_PAYLOAD_RESTRICTED_CHANGE_KINDS,
            receipt_ids: &candidates,
            mutation_entity_kind: store::RETENTION_ENTITY_KIND,
            mutation_change_kind: store::RECEIPT_PAYLOAD_COMPACTION_CHANGE_KIND,
            entity_id: "receipt-test-batch",
            idempotency_key: "receipt-test-batch",
            correlation_id: Some("test-cycle"),
            causation_id: None,
            now_ms: cutoff + 1,
        },
    )
    .expect("compact");
    assert!(matches!(outcome, MutationOutcome::Applied { .. }));
    let conn = persistence.connection_ref();
    let after_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM receipts", [], |row| row.get(0))
        .expect("after count");
    assert_eq!(
        after_count,
        before_count + 1,
        "only the summary receipt is added"
    );
    let old_payloads: (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT before_json, after_json FROM receipts WHERE receipt_id = ?1",
            [&old_receipt_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("old receipt");
    assert_eq!(old_payloads, (None, None));
    for receipt_id in [
        equal_receipt_id,
        failed_receipt_id,
        conflict_receipt_id,
        replay_receipt_id,
        kickoff_receipt_id,
        retry_receipt_id,
    ] {
        let after_json: Option<String> = conn
            .query_row(
                "SELECT after_json FROM receipts WHERE receipt_id = ?1",
                [&receipt_id],
                |row| row.get(0),
            )
            .expect("preserved receipt");
        assert_eq!(after_json.as_deref(), Some("{\"after\":1}"));
    }
    let delivery_payloads: (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT before_json, after_json FROM receipts WHERE receipt_id = ?1",
            [&delivery_receipt_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("delivery receipt");
    assert_eq!(delivery_payloads, (None, None));
}

#[test]
fn failed_email_batch_is_receipted_and_same_batch_retries_idempotently() {
    let state = test_state();
    let source_keys = vec!["retry-old".to_string()];
    {
        let mut persistence = state.persistence.lock();
        crate::slices::email_triage::store::record_inbound_message_with_body_html(
            persistence.connection(),
            &state.client_id,
            &inbound("retry-old", 1, "retry full body"),
            Some("<p>retry full body</p>"),
        )
        .expect("ingest");
        persistence
            .connection_ref()
            .execute_batch(
                "CREATE TRIGGER fail_retention_email_body \
                 BEFORE UPDATE OF body_full ON email_inbound_messages \
                 BEGIN SELECT RAISE(ABORT, 'injected retention failure'); END;",
            )
            .expect("trigger");
        let failed = crate::slices::email_triage::store::compact_email_bodies(
            persistence.connection(),
            crate::slices::email_triage::store::EmailBodyCompactionBatch {
                client_id: &state.client_id,
                actor_id: "test",
                actor_kind: ActorKindDto::System,
                cutoff_ms: 10,
                source_keys: &source_keys,
                mutation_entity_kind: store::RETENTION_ENTITY_KIND,
                mutation_change_kind: store::EMAIL_BODY_COMPACTION_CHANGE_KIND,
                entity_id: "retry-batch",
                idempotency_key: "retry-batch",
                correlation_id: None,
                causation_id: None,
                now_ms: 20,
            },
        );
        assert!(failed.is_err());
        persistence
            .connection_ref()
            .execute_batch("DROP TRIGGER fail_retention_email_body")
            .expect("drop trigger");

        let applied = crate::slices::email_triage::store::compact_email_bodies(
            persistence.connection(),
            crate::slices::email_triage::store::EmailBodyCompactionBatch {
                client_id: &state.client_id,
                actor_id: "test",
                actor_kind: ActorKindDto::System,
                cutoff_ms: 10,
                source_keys: &source_keys,
                mutation_entity_kind: store::RETENTION_ENTITY_KIND,
                mutation_change_kind: store::EMAIL_BODY_COMPACTION_CHANGE_KIND,
                entity_id: "retry-batch",
                idempotency_key: "retry-batch",
                correlation_id: None,
                causation_id: None,
                now_ms: 21,
            },
        )
        .expect("retry");
        assert!(matches!(applied, MutationOutcome::Applied { .. }));
        let replayed = crate::slices::email_triage::store::compact_email_bodies(
            persistence.connection(),
            crate::slices::email_triage::store::EmailBodyCompactionBatch {
                client_id: &state.client_id,
                actor_id: "test",
                actor_kind: ActorKindDto::System,
                cutoff_ms: 10,
                source_keys: &source_keys,
                mutation_entity_kind: store::RETENTION_ENTITY_KIND,
                mutation_change_kind: store::EMAIL_BODY_COMPACTION_CHANGE_KIND,
                entity_id: "retry-batch",
                idempotency_key: "retry-batch",
                correlation_id: None,
                causation_id: None,
                now_ms: 22,
            },
        )
        .expect("idempotent replay");
        assert!(matches!(
            replayed,
            MutationOutcome::ReplayedIdempotent { .. }
        ));

        let (body_full, body_html): (String, String) = persistence
            .connection_ref()
            .query_row(
                "SELECT body_full, body_html FROM email_inbound_messages \
                 WHERE client_id = ?1 AND source_key = 'retry-old'",
                [&state.client_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("compacted body");
        assert!(body_full.is_empty());
        assert!(body_html.is_empty());
        let outcomes: (i64, i64, i64) = persistence
            .connection_ref()
            .query_row(
                "SELECT \
                 SUM(outcome = 'failed'), SUM(outcome = 'applied'), \
                 SUM(outcome = 'replayed_idempotent') \
                 FROM receipts WHERE idempotency_key = 'retry-batch'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("outcomes");
        assert_eq!(outcomes, (1, 1, 1));
    }
}

#[test]
fn status_reports_reusable_bytes_and_legacy_shrink_flag() {
    let state = test_state();
    let guard = state
        .sync_guards
        .guard(crate::http::Pump::DataRetention)
        .lock()
        .clone();
    let status = service::status(&state, &guard, 100 * DAY_MS).expect("status");
    assert_eq!(
        status.freelist_bytes,
        status.freelist_pages.saturating_mul(status.page_size_bytes)
    );
    assert_eq!(
        status.attended_full_vacuum_required,
        status.auto_vacuum_mode != bos_contracts::data_retention::SqliteAutoVacuumMode::Incremental
    );
}

#[test]
fn manual_kickoff_replay_recovers_original_run_id_from_permanent_payload() {
    let state = test_state();
    let mut persistence = state.persistence.lock();
    let first = store::record_manual_kickoff(
        persistence.connection(),
        &state.client_id,
        store::ManualKickoff {
            run_id: "run-original",
            actor_id: "operator",
            idempotency_key: "manual-idempotency",
            now_ms: 1,
        },
    )
    .expect("kickoff");
    assert!(matches!(first.mutation, MutationOutcome::Applied { .. }));
    let replay = store::record_manual_kickoff(
        persistence.connection(),
        &state.client_id,
        store::ManualKickoff {
            run_id: "run-retry",
            actor_id: "operator",
            idempotency_key: "manual-idempotency",
            now_ms: 2,
        },
    )
    .expect("replay");
    assert!(matches!(
        replay.mutation,
        MutationOutcome::ReplayedIdempotent { .. }
    ));
    assert_eq!(replay.run_id, "run-original");
}

#[tokio::test]
async fn status_and_manual_run_are_operator_reachable_and_admin_gated() {
    let state = test_state_configured(Some("secret"), &["data_retention"]);
    let router = build_router(state);
    let denied = router
        .clone()
        .oneshot(
            Request::get("/api/data-retention/status")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

    let status = router
        .clone()
        .oneshot(
            Request::get("/api/data-retention/status")
                .header("authorization", "Bearer secret")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(status.status(), StatusCode::OK);
    let body = status.into_body().collect().await.expect("body").to_bytes();
    let status: bos_contracts::data_retention::DataRetentionStatus =
        serde_json::from_slice(&body).expect("status json");
    assert_eq!(status.email_body_retention_days, 90);

    let accepted = router
        .oneshot(
            Request::post("/api/data-retention/run")
                .header("authorization", "Bearer secret")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "idempotency_key": "manual-route-test" }).to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
}
