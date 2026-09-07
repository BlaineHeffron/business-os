//! Authenticated webhook ingress: persist inbound mail and open a work item
//! without the AI triage pass.

use super::service::{self, MessageView};
use super::store::{self, InboundMessageRecord};
use crate::env_registry;
use crate::http::OperatorScope;
use crate::store_core::StoreError;
use axum::http::HeaderMap;
use bos_contracts::email_triage::{
    EmailIngressRequest, EmailIngressResponse, FALLBACK_CATEGORY_ID,
};
use rusqlite::Connection;
use sha2::{Digest, Sha256};

pub const AI_SKIP_STATUS: &str = "skipped";
pub const AI_SKIP_RATIONALE: &str = "Webhook ingress skips AI classification.";
const HOOK_TOKEN_HEADER: &str = "x-bos-hook-token";
const WEBHOOK_LABEL: &str = "webhook";

pub fn webhook_secret_from_env() -> Option<String> {
    env_registry::string(&env_registry::BOS_EMAIL_INGRESS_WEBHOOK_SECRET)
}

pub fn verify_hook_token(headers: &HeaderMap, secret: &str) -> Result<(), &'static str> {
    let bearer = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    if verify_bearer(bearer, secret).is_ok() {
        return Ok(());
    }
    let header_token = headers
        .get(HOOK_TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match header_token {
        Some(token) if constant_time_eq(token.as_bytes(), secret.as_bytes()) => Ok(()),
        _ => Err("webhook_token_invalid"),
    }
}

fn verify_bearer(authorization_header: Option<&str>, secret: &str) -> Result<(), &'static str> {
    let Some(token) = authorization_header
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Err("webhook_token_invalid");
    };
    if !constant_time_eq(token.as_bytes(), secret.as_bytes()) {
        return Err("webhook_token_invalid");
    }
    Ok(())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (left, right) in left.iter().zip(right) {
        diff |= left ^ right;
    }
    diff == 0
}

#[derive(Debug)]
pub enum IngressError {
    FromRequired,
    RuleIdUnknown,
    ClientIdMismatch,
    Store(StoreError),
}

impl IngressError {
    pub fn code(&self) -> &str {
        match self {
            Self::FromRequired => "from_required",
            Self::RuleIdUnknown => "rule_id_unknown",
            Self::ClientIdMismatch => "client_id_mismatch",
            Self::Store(StoreError::Domain(code)) => code,
            Self::Store(_) => "storage_failure",
        }
    }
}

impl From<StoreError> for IngressError {
    fn from(err: StoreError) -> Self {
        Self::Store(err)
    }
}

pub fn ingest(
    conn: &mut Connection,
    client_id: &str,
    overlay: &crate::overlay::WorkQueueOverlay,
    request: &EmailIngressRequest,
    now_ms: u64,
) -> Result<EmailIngressResponse, IngressError> {
    if let Some(provided) = nonempty(request.client_id.as_deref()) {
        if provided != client_id {
            return Err(IngressError::ClientIdMismatch);
        }
    }
    let nested = request.message.as_ref();
    let from = first_nonempty([
        request.from.as_deref(),
        nested.and_then(|m| m.from.as_deref()),
    ])
    .ok_or(IngressError::FromRequired)?;
    let to = first_nonempty([request.to.as_deref(), nested.and_then(|m| m.to.as_deref())]);
    let subject = first_nonempty([
        request.subject.as_deref(),
        nested.and_then(|m| m.subject.as_deref()),
    ]);
    let thread_id = first_nonempty([
        request.thread_id.as_deref(),
        nested.and_then(|m| m.thread_id.as_deref()),
    ]);
    let snippet = first_nonempty([
        request.snippet.as_deref(),
        nested.and_then(|m| m.snippet.as_deref()),
    ]);
    let body = first_nonempty([
        request.body.as_deref(),
        nested.and_then(|m| m.body.as_deref()),
        snippet.as_deref(),
    ])
    .unwrap_or_default();
    let message_id = first_nonempty([
        request.message_id.as_deref(),
        nested.and_then(|m| m.message_id.as_deref()),
        request.idempotency_key.as_deref(),
    ])
    .unwrap_or_else(|| {
        generated_message_id(
            &from,
            subject.as_deref().unwrap_or(""),
            thread_id.as_deref().unwrap_or(""),
            snippet.as_deref().unwrap_or(""),
        )
    });
    let source_key = webhook_source_key(&message_id);
    let existing = store::existing_source_keys(conn, client_id, std::slice::from_ref(&source_key))?;
    if existing.contains(&source_key) {
        let record = store::inbound_by_source_keys(
            conn,
            client_id,
            std::slice::from_ref(&source_key),
            &OperatorScope::All,
        )?
        .into_iter()
        .next();
        let Some(record) = record else {
            return Err(StoreError::Sqlite(
                "inbound row missing for existing source_key".to_string(),
            )
            .into());
        };
        skip_ai(conn, client_id, &record.source_key, now_ms)?;
        let item_id = emit_work_item(conn, client_id, overlay, &record, now_ms)?;
        return Ok(EmailIngressResponse {
            accepted: true,
            duplicate: true,
            source_key: record.source_key,
            message_id: record.message_id,
            item_id,
            category_id: record.resolved_category,
            matched_rule_id: record.matched_rule_id,
        });
    }

    let rules: Vec<_> = store::list_active(conn, client_id)?
        .into_iter()
        .map(|stored| stored.rule)
        .collect();
    let requested_rule_id = nonempty(request.rule_id.as_deref());
    let (resolved_category, matched_rule_id) = if let Some(rule_id) = requested_rule_id {
        let matched = rules
            .iter()
            .find(|rule| rule.enabled && rule.rule_id == rule_id)
            .ok_or(IngressError::RuleIdUnknown)?;
        (
            matched.pinned_category.clone(),
            Some(matched.rule_id.clone()),
        )
    } else {
        let view = MessageView {
            message_id: Some(message_id.clone()),
            source_user_id: None,
            subject: subject.clone(),
            from: Some(from.clone()),
            to: to.clone(),
            body: Some(body.clone()),
            labels: vec![WEBHOOK_LABEL.to_string()],
            headers: Vec::new(),
        };
        let matched = service::resolve_rule(&rules, &view);
        (
            matched
                .map(|rule| rule.pinned_category.clone())
                .unwrap_or_else(|| FALLBACK_CATEGORY_ID.to_string()),
            matched.map(|rule| rule.rule_id.clone()),
        )
    };
    let display_body = service::display_body_for_excerpt(&body);
    let record = InboundMessageRecord {
        source_key: source_key.clone(),
        message_id: message_id.clone(),
        thread_id,
        internal_date_ms: None,
        from_addr: Some(from),
        to_addr: to,
        subject,
        body_excerpt: display_body,
        body_full: body,
        headers: Vec::new(),
        labels: vec![WEBHOOK_LABEL.to_string()],
        resolved_category,
        matched_rule_id,
        ingested_at_ms: now_ms,
        ai_triage_status: None,
        ai_triage_rationale: None,
        attachments: Vec::new(),
        source_user_id: None,
    };
    store::record_inbound_message(conn, client_id, &record)?;
    skip_ai(conn, client_id, &record.source_key, now_ms)?;
    let item_id = emit_work_item(conn, client_id, overlay, &record, now_ms)?;
    Ok(EmailIngressResponse {
        accepted: true,
        duplicate: false,
        source_key: record.source_key,
        message_id: record.message_id,
        item_id,
        category_id: record.resolved_category,
        matched_rule_id: record.matched_rule_id,
    })
}

fn skip_ai(
    conn: &mut Connection,
    client_id: &str,
    source_key: &str,
    now_ms: u64,
) -> Result<(), StoreError> {
    let status: Option<String> = conn.query_row(
        "SELECT ai_triage_status FROM email_inbound_messages \
         WHERE client_id = ?1 AND source_key = ?2",
        rusqlite::params![client_id, source_key],
        |row| row.get(0),
    )?;
    if status.as_deref().is_some_and(|value| !value.is_empty()) {
        return Ok(());
    }
    store::set_ai_triage_result(
        conn,
        client_id,
        source_key,
        AI_SKIP_STATUS,
        Some(AI_SKIP_RATIONALE),
        None,
        now_ms,
    )?;
    Ok(())
}

fn emit_work_item(
    conn: &mut Connection,
    client_id: &str,
    overlay: &crate::overlay::WorkQueueOverlay,
    record: &InboundMessageRecord,
    now_ms: u64,
) -> Result<Option<String>, StoreError> {
    crate::slices::work_queue::service::emit_for_inbound_message_with_overlay(
        conn, client_id, record, overlay, now_ms,
    )?;
    Ok(crate::slices::work_queue::store::get_item_for_source(
        conn,
        client_id,
        crate::slices::work_queue::SOURCE_KIND_EMAIL,
        &record.source_key,
    )?
    .map(|existing| existing.item.item_id))
}

fn webhook_source_key(message_id: &str) -> String {
    format!("webhook:{message_id}")
}

fn first_nonempty<'a>(candidates: impl IntoIterator<Item = Option<&'a str>>) -> Option<String> {
    candidates.into_iter().find_map(nonempty)
}

fn nonempty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn generated_message_id(from: &str, subject: &str, thread_id: &str, snippet: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(from.as_bytes());
    hasher.update([0]);
    hasher.update(subject.as_bytes());
    hasher.update([0]);
    hasher.update(thread_id.as_bytes());
    hasher.update([0]);
    hasher.update(snippet.as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        hex.push_str(&format!("{byte:02x}"));
    }
    format!("wh-{hex}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::{
        build_router,
        test_support::{test_state, test_state_configured, EnvGuard},
    };
    use crate::persistence::Persistence;
    use crate::slices::email_triage::store::{self, RuleMutationContext};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use bos_contracts::email_triage::{
        EmailTriageCondition, EmailTriageField, EmailTriageMatchMode, EmailTriageOperator,
        EmailTriageRule,
    };
    use bos_contracts::work_queue::WorkQueuePolicy;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    const CLIENT: &str = "test-client";
    const SECRET_ENV: &str = "BOS_EMAIL_INGRESS_WEBHOOK_SECRET";
    const SECRET: &str = "ingress-secret";

    fn policy(create: bool) -> WorkQueuePolicy {
        WorkQueuePolicy {
            category_id: FALLBACK_CATEGORY_ID.to_string(),
            create_work_item: create,
            packet_kinds: vec!["follow_up_task".to_string()],
            ai_suggestible_packet_kinds: vec!["follow_up_task".to_string()],
            ai_suggestible_gmail_scope: Default::default(),
            ai_suggestible_gmail_categories: Vec::new(),
            auto_produce: false,
        }
    }

    fn flat_payload() -> serde_json::Value {
        serde_json::json!({
            "from": "Sinead <sineadpatience@me.com>",
            "subject": "Site change",
            "threadId": "thr-1",
            "messageId": "msg-1",
            "snippet": "The homepage copy changed."
        })
    }

    async fn post(
        router: axum::Router,
        auth: Option<(&str, &str)>,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let mut builder =
            Request::post("/api/webhooks/email-ingress").header("content-type", "application/json");
        if let Some((name, value)) = auth {
            builder = builder.header(name, value);
        }
        let response = router
            .oneshot(
                builder
                    .body(Body::from(serde_json::to_vec(&body).expect("json")))
                    .expect("request"),
            )
            .await
            .expect("response");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::json!({}));
        (status, json)
    }

    #[test]
    fn missing_or_wrong_token_is_rejected() {
        let mut headers = HeaderMap::new();
        assert_eq!(
            verify_hook_token(&headers, SECRET),
            Err("webhook_token_invalid")
        );
        headers.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer other".parse().unwrap(),
        );
        assert_eq!(
            verify_hook_token(&headers, SECRET),
            Err("webhook_token_invalid")
        );
        headers.insert(HOOK_TOKEN_HEADER, SECRET.parse().unwrap());
        assert!(verify_hook_token(&headers, SECRET).is_ok());
    }

    #[tokio::test]
    async fn unset_secret_404s() {
        let _unset = EnvGuard::unset(SECRET_ENV);
        let router = build_router(test_state());
        let (status, body) =
            post(router, Some(("authorization", "Bearer x")), flat_payload()).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], "route_not_found");
    }

    #[tokio::test]
    async fn operator_token_and_missing_auth_401() {
        let _env = EnvGuard::set(SECRET_ENV, SECRET);
        let router = build_router(test_state_configured(Some("operator-token"), &[]));
        let (status, body) = post(
            router.clone(),
            Some(("authorization", "Bearer operator-token")),
            flat_payload(),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"], "webhook_token_invalid");
        let (status, _) = post(router, None, flat_payload()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn webhook_creates_work_item_skips_ai_and_is_idempotent() {
        let _env = EnvGuard::set(SECRET_ENV, SECRET);
        let state = test_state();
        {
            let mut persistence = state.persistence();
            crate::slices::work_queue::store::upsert_policy(
                persistence.connection(),
                CLIENT,
                "op_test",
                &policy(true),
                "ingress-policy",
                1_000,
            )
            .expect("policy");
        }
        let router = build_router(state.clone());
        let (status, body) = post(
            router.clone(),
            Some(("authorization", &format!("Bearer {SECRET}"))),
            flat_payload(),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(body["duplicate"], false);
        assert_eq!(body["messageId"], "msg-1");
        assert_eq!(body["sourceKey"], "webhook:msg-1");
        assert_eq!(body["itemId"], "wi_email_webhook:msg-1");
        assert_eq!(body["categoryId"], FALLBACK_CATEGORY_ID);

        {
            let persistence = state.persistence();
            let conn = persistence.connection_ref();
            let stored = store::inbound_by_source_keys(
                conn,
                CLIENT,
                &["webhook:msg-1".to_string()],
                &OperatorScope::All,
            )
            .expect("stored");
            assert_eq!(stored[0].ai_triage_status.as_deref(), Some(AI_SKIP_STATUS));
            assert!(store::list_unexamined_ai_suggestible(conn, CLIENT, 10)
                .expect("batch")
                .is_empty());
        }

        let (status, replay) =
            post(router, Some(("x-bos-hook-token", SECRET)), flat_payload()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(replay["duplicate"], true);
        assert_eq!(replay["itemId"], body["itemId"]);
    }

    #[tokio::test]
    async fn nested_payload_without_policy_ingests_without_work_item() {
        let _env = EnvGuard::set(SECRET_ENV, SECRET);
        let router = build_router(test_state());
        let nested = serde_json::json!({
            "source": "business-os.gmail-webhook-trigger",
            "version": 1,
            "receivedAt": "2026-09-07T15:00:00.000Z",
            "query": "from:ada@example.com",
            "message": {
                "messageId": "nested-1",
                "threadId": "thr-n",
                "from": "Ada <ada@example.com>",
                "subject": "Ping",
                "snippet": "hello",
                "date": "2026-09-07T15:00:00.000Z"
            }
        });
        let (status, body) = post(
            router,
            Some(("authorization", &format!("Bearer {SECRET}"))),
            nested,
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(body["messageId"], "nested-1");
        assert_eq!(body["sourceKey"], "webhook:nested-1");
        assert!(body["itemId"].is_null());
    }

    #[test]
    fn rule_id_pins_category_without_matching_conditions() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        let rule = EmailTriageRule {
            rule_id: "site_change".into(),
            conditions: vec![EmailTriageCondition {
                field: EmailTriageField::Subject,
                op: EmailTriageOperator::Contains,
                value: "will-not-match".into(),
                header_name: None,
            }],
            conditions_v2: Vec::new(),
            match_mode: EmailTriageMatchMode::All,
            priority: 10,
            enabled: true,
            pinned_category: "operator_note".into(),
        };
        store::upsert(
            conn,
            RuleMutationContext {
                client_id: CLIENT,
                actor_id: "op_test",
                expected_revision: None,
                idempotency_key: "rule",
                correlation_id: None,
                now_ms: 1_000,
            },
            &rule,
        )
        .expect("rule");
        crate::slices::work_queue::store::upsert_policy(
            conn,
            CLIENT,
            "op_test",
            &WorkQueuePolicy {
                category_id: "operator_note".to_string(),
                create_work_item: true,
                packet_kinds: vec!["follow_up_task".to_string()],
                ai_suggestible_packet_kinds: vec!["follow_up_task".to_string()],
                ai_suggestible_gmail_scope: Default::default(),
                ai_suggestible_gmail_categories: Vec::new(),
                auto_produce: false,
            },
            "note-policy",
            1_500,
        )
        .expect("policy");
        let request = EmailIngressRequest {
            from: Some("Ada <ada@example.com>".into()),
            subject: Some("Unrelated".into()),
            message_id: Some("pin-1".into()),
            rule_id: Some("site_change".into()),
            ..Default::default()
        };
        let result = ingest(
            conn,
            CLIENT,
            &crate::overlay::WorkQueueOverlay::default(),
            &request,
            2_000,
        )
        .expect("ingest");
        assert_eq!(result.category_id, "operator_note");
        assert_eq!(result.matched_rule_id.as_deref(), Some("site_change"));
        assert!(result.item_id.is_some());
        let stored = store::inbound_by_source_keys(
            conn,
            CLIENT,
            std::slice::from_ref(&result.source_key),
            &OperatorScope::All,
        )
        .expect("stored");
        assert_eq!(stored[0].ai_triage_status.as_deref(), Some(AI_SKIP_STATUS));
        assert!(
            store::list_unexamined_ai_suggestible(conn, CLIENT, 10)
                .expect("batch")
                .is_empty(),
            "webhook-sourced mail must not enter the AI triage batch"
        );
    }
}
