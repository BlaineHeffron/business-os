//! Thin HTTP handlers: parse, auth, call store/service, serialize.

use super::store::{self, RuleAction, RuleMutationContext};
use super::{catalog, service};
use crate::http::{error_response, mutation_response, now_ms, AppState};
use crate::store_core::StoreError;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bos_contracts::email_triage::{
    AiRetriageResetRequest, AiRetriageResetResponse, AiRetriageResetScope, CategoriesListResponse,
    CategoryDeleteRequest, CategoryUpsertRequest, EmailAttachmentEvidenceRequest,
    EmailManualFollowUpRequest, EmailTrashRequest, EmailTriageDryRunRequest,
    EmailTriageDryRunResponse, EmailTriageGmailCategory, EmailTriageInboxDefaults,
    EmailTriageInboxOptionsResponse, EmailTriageInboxResponse, EmailTriageInboxSettingsResponse,
    EmailTriageInboxSettingsUpdateRequest, EmailTriageRuleActionKind, EmailTriageRuleActionRequest,
    EmailTriageRuleUpsertRequest, EmailTriageRulesListResponse, ReclassifyResponse,
    RuleWithRevision, FALLBACK_CATEGORY_ID,
};
use serde::Deserialize;

const DEFAULT_INBOX_LIMIT: u32 = 100;
const MAX_INBOX_LIMIT: u32 = 500;
const LEGACY_MAILBOX_QUERY_VALUE: &str = "__legacy";

#[derive(Debug, Default, Deserialize)]
struct InboxQuery {
    #[serde(default)]
    categories: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    dashboard_categories: Option<String>,
    #[serde(default)]
    dashboard_category: Option<String>,
    #[serde(default)]
    labels: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    source_user_id: Option<String>,
    #[serde(default)]
    mailbox: Option<String>,
    #[serde(default)]
    crm_match: Option<String>,
    #[serde(default)]
    crm_deal_stages: Option<String>,
    #[serde(default)]
    crm_deal_stage: Option<String>,
    #[serde(default)]
    crm_deal_pipelines: Option<String>,
    #[serde(default)]
    crm_deal_pipeline: Option<String>,
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    search: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/email-triage/rules", get(rules_list).post(rule_upsert))
        .route(
            "/api/email-triage/rules/{rule_id}/action",
            post(rule_action),
        )
        .route("/api/email-triage/dry-run", post(dry_run))
        .route(
            "/api/email-triage/condition-catalog",
            get(condition_catalog),
        )
        .route("/api/email-triage/inbox", get(inbox_list))
        .route("/api/email-triage/inbox/options", get(inbox_options))
        .route(
            "/api/email-triage/inbox/settings",
            get(inbox_settings).post(update_inbox_settings),
        )
        .route(
            "/api/email-triage/inbox/{message_id}/follow-up",
            post(inbox_follow_up),
        )
        .route(
            "/api/email-triage/inbox/{message_id}/trash",
            post(inbox_trash),
        )
        .route(
            "/api/email-triage/inbox/{message_id}/attachments/{attachment_id}/evidence",
            post(stage_attachment_evidence),
        )
        .route(
            "/api/email-triage/categories",
            get(categories_list).post(category_upsert),
        )
        .route(
            "/api/email-triage/categories/{category_id}/delete",
            post(category_delete),
        )
        .route("/api/email-triage/reclassify", post(reclassify))
        .route(
            "/api/email-triage/ai-retriage-reset",
            post(ai_retriage_reset),
        )
}

async fn condition_catalog(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    Json(catalog::condition_catalog()).into_response()
}

/// Clear AI-triage verdicts so the pump re-examines old mail (e.g. after the
/// category catalog or packet kinds changed since the AI last looked).
async fn ai_retriage_reset(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<AiRetriageResetRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if let Err(denied) = auth.require_all_scope() {
        return *denied;
    }
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let scope = match request.scope {
        AiRetriageResetScope::Message => {
            let source_key = request
                .source_key
                .as_deref()
                .or(request.message_id.as_deref())
                .map(str::trim);
            match source_key {
                Some(source_key) if !source_key.is_empty() => {
                    store::AiRetriageScope::Message(source_key.to_string())
                }
                _ => return error_response(StatusCode::BAD_REQUEST, "source_key_required"),
            }
        }
        AiRetriageResetScope::Stale => store::AiRetriageScope::Stale,
        AiRetriageResetScope::All => store::AiRetriageScope::All,
    };
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let mut persistence = match state.persistence_or_busy() {
        Ok(persistence) => persistence,
        Err(response) => return *response,
    };
    match store::reset_ai_triage(
        persistence.connection(),
        &state.client_id,
        &actor_id,
        &scope,
        &request.idempotency_key,
        now_ms(),
    ) {
        Ok(reset) => Json(AiRetriageResetResponse { reset }).into_response(),
        Err(err) => store_error_response(err),
    }
}

/// Re-run the current rules over all stored mail and backfill work items.
/// Synchronous: pilot volumes are small; this becomes a job when they aren't.
async fn reclassify(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if let Err(denied) = auth.require_all_scope() {
        return *denied;
    }
    let mut persistence = match state.persistence_or_busy() {
        Ok(persistence) => persistence,
        Err(response) => return *response,
    };
    match service::reclassify_all_with_email_overlay(
        persistence.connection(),
        &state.client_id,
        "operator",
        FALLBACK_CATEGORY_ID,
        &state.email_triage_overlay,
        &state.work_queue_overlay,
        now_ms(),
    ) {
        Ok((examined, reclassified, work_items_emitted)) => Json(ReclassifyResponse {
            examined,
            reclassified,
            work_items_emitted,
        })
        .into_response(),
        Err(err) => store_error_response(err),
    }
}

async fn categories_list(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    let mut persistence = match state.persistence_or_busy() {
        Ok(persistence) => persistence,
        Err(response) => return *response,
    };
    match store::list_categories(persistence.connection(), &state.client_id, now_ms()) {
        Ok(categories) => Json(CategoriesListResponse { categories }).into_response(),
        Err(err) => store_error_response(err),
    }
}

async fn category_upsert(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CategoryUpsertRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let mut persistence = match state.persistence_or_busy() {
        Ok(persistence) => persistence,
        Err(response) => return *response,
    };
    let result = match request.policy.as_ref() {
        Some(policy) => store::upsert_category_with_policy(
            persistence.connection(),
            &state.client_id,
            &actor_id,
            &request.category,
            policy,
            &request.idempotency_key,
            now_ms(),
        ),
        None => store::upsert_category(
            persistence.connection(),
            &state.client_id,
            &actor_id,
            &request.category,
            &request.idempotency_key,
            now_ms(),
        ),
    };
    match result {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

async fn stage_attachment_evidence(
    State(state): State<AppState>,
    Path((message_id, attachment_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<EmailAttachmentEvidenceRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    if request.session_id.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "agent_session_id_required");
    }
    let actor_id = auth.actor_or(None);
    let scope = auth.scope.clone();
    match service::stage_attachment_evidence(
        state,
        actor_id,
        scope,
        message_id,
        attachment_id,
        request,
    )
    .await
    {
        Ok(response) => Json(response).into_response(),
        Err(err) => attachment_evidence_error_response(err),
    }
}

pub(crate) fn attachment_evidence_error_response(
    err: service::AttachmentEvidenceError,
) -> Response {
    match err {
        service::AttachmentEvidenceError::MessageNotFound => {
            error_response(StatusCode::NOT_FOUND, "email_inbound_message_not_found")
        }
        service::AttachmentEvidenceError::AttachmentNotFound => {
            error_response(StatusCode::NOT_FOUND, "email_attachment_not_found")
        }
        service::AttachmentEvidenceError::CredentialMissing => {
            error_response(StatusCode::UNPROCESSABLE_ENTITY, "gmail_credential_missing")
        }
        service::AttachmentEvidenceError::AttachmentTooLarge => {
            error_response(StatusCode::PAYLOAD_TOO_LARGE, "email_attachment_too_large")
        }
        service::AttachmentEvidenceError::Provider(err) => {
            tracing::warn!(error = %err, "gmail attachment evidence fetch failed");
            error_response(StatusCode::BAD_GATEWAY, "email_attachment_fetch_failed")
        }
        service::AttachmentEvidenceError::Io(err) => {
            tracing::warn!(error = %err, "agent evidence file write failed");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "agent_evidence_write_failed",
            )
        }
        service::AttachmentEvidenceError::Store(err) => store_error_response(err),
        service::AttachmentEvidenceError::JoinFailed => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "agent_evidence_stage_join_failed",
        ),
    }
}

async fn category_delete(
    State(state): State<AppState>,
    Path(category_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<CategoryDeleteRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let mut persistence = match state.persistence_or_busy() {
        Ok(persistence) => persistence,
        Err(response) => return *response,
    };
    match store::delete_category(
        persistence.connection(),
        &state.client_id,
        &actor_id,
        &category_id,
        &request.idempotency_key,
        now_ms(),
    ) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

async fn inbox_list(
    State(state): State<AppState>,
    Query(query): Query<InboxQuery>,
    headers: HeaderMap,
) -> Response {
    let scope = match state.require_scope(&headers) {
        Ok(scope) => scope,
        Err(denied) => return *denied,
    };
    let filter = match inbox_filter_from_query(&query) {
        Ok(filter) => filter,
        Err(code) => return error_response(StatusCode::BAD_REQUEST, code),
    };
    let limit = query
        .limit
        .unwrap_or(DEFAULT_INBOX_LIMIT)
        .clamp(1, MAX_INBOX_LIMIT) as usize;
    let persistence = match state.persistence_or_busy() {
        Ok(persistence) => persistence,
        Err(response) => return *response,
    };
    match store::list_recent_inbound(
        persistence.connection_ref(),
        &state.client_id,
        limit,
        &scope,
        &filter,
    ) {
        Ok(messages) => Json(EmailTriageInboxResponse { messages }).into_response(),
        Err(err) => store_error_response(err),
    }
}

async fn inbox_options(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    let persistence = match state.persistence_or_busy() {
        Ok(persistence) => persistence,
        Err(response) => return *response,
    };
    match store::inbox_options(persistence.connection_ref(), &state.client_id, &auth.scope) {
        Ok(options) => Json(EmailTriageInboxOptionsResponse {
            categories: options.categories,
            visible_gmail_categories: options.visible_gmail_categories,
            dashboard_categories: options.dashboard_categories,
            labels: options.labels,
            mailboxes: options.mailboxes,
            crm_deal_stages: options.crm_deal_stages,
            crm_deal_pipelines: options.crm_deal_pipelines,
            defaults: inbox_defaults_for_actor(&state, &auth.actor_id),
        })
        .into_response(),
        Err(err) => store_error_response(err),
    }
}

async fn inbox_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    let persistence = match state.persistence_or_busy() {
        Ok(persistence) => persistence,
        Err(response) => return *response,
    };
    match store::get_inbox_settings(persistence.connection_ref(), &state.client_id) {
        Ok(stored) => Json(EmailTriageInboxSettingsResponse {
            revision: stored.as_ref().and_then(|settings| settings.revision),
            visible_gmail_categories: stored
                .map(|settings| settings.visible_gmail_categories)
                .unwrap_or_else(store::default_visible_gmail_categories),
        })
        .into_response(),
        Err(err) => store_error_response(err),
    }
}

async fn update_inbox_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<EmailTriageInboxSettingsUpdateRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let mut persistence = match state.persistence_or_busy() {
        Ok(persistence) => persistence,
        Err(response) => return *response,
    };
    match store::replace_inbox_settings(
        persistence.connection(),
        &state.client_id,
        &actor_id,
        &request,
        now_ms(),
    ) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

fn inbox_filter_from_query(query: &InboxQuery) -> Result<store::InboxFilter, &'static str> {
    let mut categories = Vec::new();
    for raw in comma_values(query.categories.as_deref())
        .into_iter()
        .chain(comma_values(query.category.as_deref()))
    {
        categories.push(parse_gmail_category(&raw).ok_or("email_triage_category_filter_invalid")?);
    }
    categories.sort_by_key(|category| *category as u8);
    categories.dedup();

    let mut dashboard_categories = Vec::new();
    for raw in comma_values(query.dashboard_categories.as_deref())
        .into_iter()
        .chain(comma_values(query.dashboard_category.as_deref()))
    {
        if !bos_contracts::email_triage::validate_category_id(&raw) {
            return Err("email_triage_dashboard_category_filter_invalid");
        }
        dashboard_categories.push(raw);
    }
    dashboard_categories.sort();
    dashboard_categories.dedup();

    let mut labels = comma_values(query.labels.as_deref());
    labels.extend(comma_values(query.label.as_deref()));
    labels.sort_by_key(|label| label.to_ascii_lowercase());
    labels.dedup_by(|a, b| a.eq_ignore_ascii_case(b));

    let mut source_user_ids = Vec::new();
    for raw in comma_values(query.source_user_id.as_deref())
        .into_iter()
        .chain(comma_values(query.mailbox.as_deref()))
    {
        if raw == LEGACY_MAILBOX_QUERY_VALUE {
            source_user_ids.push(None);
        } else {
            source_user_ids.push(Some(raw));
        }
    }
    source_user_ids.sort();
    source_user_ids.dedup();

    let search = query
        .q
        .as_deref()
        .or(query.search.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(512).collect());

    let crm_match = match query.crm_match.as_deref().map(str::trim) {
        Some("has_contact") => Some(store::InboxCrmMatchFilter::HasContact),
        Some("no_match") => Some(store::InboxCrmMatchFilter::NoMatch),
        Some("has_deal") => Some(store::InboxCrmMatchFilter::HasDeal),
        Some("") | None => None,
        Some(_) => return Err("email_triage_crm_match_filter_invalid"),
    };

    let mut crm_deal_stages = comma_values(query.crm_deal_stages.as_deref());
    crm_deal_stages.extend(comma_values(query.crm_deal_stage.as_deref()));
    normalize_crm_facet_values(&mut crm_deal_stages);

    let mut crm_deal_pipelines = comma_values(query.crm_deal_pipelines.as_deref());
    crm_deal_pipelines.extend(comma_values(query.crm_deal_pipeline.as_deref()));
    normalize_crm_facet_values(&mut crm_deal_pipelines);

    Ok(store::InboxFilter {
        categories,
        dashboard_categories,
        labels,
        source_user_ids,
        search,
        crm_match,
        crm_deal_stages,
        crm_deal_pipelines,
    })
}

fn normalize_crm_facet_values(values: &mut Vec<String>) {
    values.retain(|value| !value.trim().is_empty());
    for value in values.iter_mut() {
        *value = value.trim().chars().take(128).collect();
    }
    values.sort_by_key(|value| value.to_ascii_lowercase());
    values.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
}

fn inbox_defaults_for_actor(state: &AppState, actor_id: &str) -> EmailTriageInboxDefaults {
    let overlay = state.email_triage_overlay.as_ref();
    let default = overlay
        .inbox_defaults
        .iter()
        .find(|entry| entry.user_id.trim() == actor_id)
        .or_else(|| {
            overlay
                .inbox_defaults
                .iter()
                .find(|entry| entry.user_id.trim().is_empty())
        });
    let Some(default) = default else {
        return EmailTriageInboxDefaults {
            categories: Vec::new(),
            label: None,
            source_user_id: None,
            limit: DEFAULT_INBOX_LIMIT,
        };
    };
    EmailTriageInboxDefaults {
        categories: default.categories.clone(),
        label: default
            .label
            .as_deref()
            .map(str::trim)
            .filter(|label| !label.is_empty())
            .map(str::to_string),
        source_user_id: default
            .source_user_id
            .as_deref()
            .map(str::trim)
            .filter(|source_user_id| !source_user_id.is_empty())
            .map(str::to_string),
        limit: default
            .limit
            .unwrap_or(DEFAULT_INBOX_LIMIT)
            .clamp(1, MAX_INBOX_LIMIT),
    }
}

fn comma_values(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn parse_gmail_category(raw: &str) -> Option<EmailTriageGmailCategory> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "primary" | "personal" | "category_personal" => Some(EmailTriageGmailCategory::Primary),
        "updates" | "category_updates" => Some(EmailTriageGmailCategory::Updates),
        "social" | "category_social" => Some(EmailTriageGmailCategory::Social),
        "promotions" | "category_promotions" => Some(EmailTriageGmailCategory::Promotions),
        "forums" | "category_forums" => Some(EmailTriageGmailCategory::Forums),
        _ => None,
    }
}

async fn inbox_follow_up(
    State(state): State<AppState>,
    Path(message_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<EmailManualFollowUpRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    let scope = auth.scope.clone();
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let mut persistence = match state.persistence_or_busy() {
        Ok(persistence) => persistence,
        Err(response) => return *response,
    };
    let messages = match store::inbound_by_source_keys(
        persistence.connection_ref(),
        &state.client_id,
        std::slice::from_ref(&message_id),
        &scope,
    ) {
        Ok(messages) => messages,
        Err(err) => return store_error_response(err),
    };
    let Some(message) = messages.into_iter().next() else {
        return error_response(StatusCode::NOT_FOUND, "email_inbound_message_not_found");
    };
    match crate::slices::work_queue::service::add_manual_follow_up_for_email(
        persistence.connection(),
        crate::slices::work_queue::store::ItemActionContext {
            client_id: &state.client_id,
            actor_id: &actor_id,
            scope: &scope,
            expected_revision: None,
            idempotency_key: &request.idempotency_key,
            now_ms: now_ms(),
        },
        &message,
        &state.work_queue_overlay,
    ) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => crate::http::store_error_response("work_queue", err),
    }
}

async fn inbox_trash(
    State(state): State<AppState>,
    Path(message_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<EmailTrashRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let mut persistence = match state.persistence_or_busy() {
        Ok(persistence) => persistence,
        Err(response) => return *response,
    };
    let messages = match store::inbound_by_source_keys(
        persistence.connection_ref(),
        &state.client_id,
        std::slice::from_ref(&message_id),
        &auth.scope,
    ) {
        Ok(messages) => messages,
        Err(err) => return store_error_response(err),
    };
    let Some(message) = messages.into_iter().next() else {
        return error_response(StatusCode::NOT_FOUND, "email_inbound_message_not_found");
    };
    match store::request_gmail_trash(
        persistence.connection(),
        crate::slices::work_queue::store::ItemActionContext {
            client_id: &state.client_id,
            actor_id: &actor_id,
            scope: &auth.scope,
            expected_revision: request.expected_revision,
            idempotency_key: &request.idempotency_key,
            now_ms: now_ms(),
        },
        &message,
    ) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

async fn rules_list(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    let persistence = match state.persistence_or_busy() {
        Ok(persistence) => persistence,
        Err(response) => return *response,
    };
    match store::list_active(persistence.connection_ref(), &state.client_id) {
        Ok(rules) => Json(EmailTriageRulesListResponse {
            rules: rules
                .into_iter()
                .map(|stored| RuleWithRevision {
                    rule: stored.rule,
                    revision: stored.revision,
                })
                .collect(),
        })
        .into_response(),
        Err(err) => store_error_response(err),
    }
}

async fn rule_upsert(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<EmailTriageRuleUpsertRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let mut persistence = match state.persistence_or_busy() {
        Ok(persistence) => persistence,
        Err(response) => return *response,
    };
    let ctx = RuleMutationContext {
        client_id: &state.client_id,
        actor_id: &actor_id,
        expected_revision: request.expected_revision,
        idempotency_key: &request.idempotency_key,
        correlation_id: None,
        now_ms: now_ms(),
    };
    match store::upsert(persistence.connection(), ctx, &request.rule) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

async fn rule_action(
    State(state): State<AppState>,
    Path(rule_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<EmailTriageRuleActionRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let action = match request.action {
        EmailTriageRuleActionKind::Enable => RuleAction::Enable,
        EmailTriageRuleActionKind::Disable => RuleAction::Disable,
        EmailTriageRuleActionKind::Delete => RuleAction::Delete,
    };
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let mut persistence = match state.persistence_or_busy() {
        Ok(persistence) => persistence,
        Err(response) => return *response,
    };
    let ctx = RuleMutationContext {
        client_id: &state.client_id,
        actor_id: &actor_id,
        expected_revision: request.expected_revision,
        idempotency_key: &request.idempotency_key,
        correlation_id: None,
        now_ms: now_ms(),
    };
    match store::apply_action(persistence.connection(), ctx, &rule_id, action) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => store_error_response(err),
    }
}

async fn dry_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<EmailTriageDryRunRequest>,
) -> Response {
    if let Err(denied) = state.require_operator(&headers) {
        return *denied;
    }
    let persistence = match state.persistence_or_busy() {
        Ok(persistence) => persistence,
        Err(response) => return *response,
    };
    let stored = match store::list_active(persistence.connection_ref(), &state.client_id) {
        Ok(stored) => stored,
        Err(err) => return store_error_response(err),
    };
    let merged = service::merge_rules_for_dry_run(
        stored.into_iter().map(|stored| stored.rule).collect(),
        request.proposed_rules,
    );
    let (mut contexts, cache_misses) = if service::rules_need_crm_facts(&merged) {
        request
            .samples
            .iter()
            .map(|sample| {
                service::crm_fact_overrides_from_cache(
                    persistence.connection_ref(),
                    &state.client_id,
                    sample,
                    now_ms(),
                )
            })
            .unzip::<_, _, Vec<_>, Vec<_>>()
    } else {
        (Vec::new(), Vec::new())
    };
    drop(persistence);
    if !cache_misses.is_empty() {
        let mut lookup = service::EnvCrmLiveLookup;
        let mut budget = service::crm_dry_run_budget();
        let ttls = service::CrmFactTtls::from_env();
        let mut writes = Vec::new();
        for (index, misses) in cache_misses.iter().enumerate() {
            let (patch, mut sample_writes) =
                service::resolve_crm_fact_misses(misses, &mut budget, &mut lookup, now_ms(), ttls);
            if let Some(context) = contexts.get_mut(index) {
                service::merge_crm_fact_overrides(context, patch);
            }
            writes.append(&mut sample_writes);
        }
        if !writes.is_empty() {
            let mut persistence = match state.persistence_or_busy() {
                Ok(persistence) => persistence,
                Err(response) => return *response,
            };
            if let Err(err) = service::persist_crm_fact_cache_writes(
                persistence.connection(),
                &state.client_id,
                &writes,
            ) {
                return store_error_response(err);
            }
        }
    }
    let persistence = match state.persistence_or_busy() {
        Ok(persistence) => persistence,
        Err(response) => return *response,
    };
    let bags = request
        .samples
        .iter()
        .enumerate()
        .map(|(index, sample)| {
            let crm = contexts.get(index).cloned().unwrap_or_default();
            crate::slices::email_triage::facts::FactBag::new(
                Some(persistence.connection_ref()),
                &state.client_id,
                sample,
                sample.message_id.as_deref(),
                sample.source_user_id.as_deref(),
                crm,
            )
        })
        .collect();
    let traces = service::dry_run_traces_with_fact_bags(
        &merged,
        request
            .fallback_category
            .as_deref()
            .unwrap_or(FALLBACK_CATEGORY_ID),
        bags,
    );
    let results = traces
        .iter()
        .map(|trace| bos_contracts::email_triage::DryRunResult {
            resolved_category: trace.resolved_category.clone(),
            matched_rule_id: trace.matched_rule_id.clone(),
        })
        .collect();
    Json(EmailTriageDryRunResponse { results, traces }).into_response()
}

fn store_error_response(err: StoreError) -> Response {
    crate::http::store_error_response("email_triage", err)
}
