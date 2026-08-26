//! Rule evaluation. Ported from agent-monitor-rust's
//! gmail_triage_source_connector resolver: first enabled rule (in priority
//! order) whose conditions match pins the category; otherwise the fallback
//! applies. Matching is case-insensitive; regex is case-insensitive and a
//! non-compiling pattern simply never matches.

use super::facts::{
    fact_label, CrmFactOverrides, CrmFactValue, CrmTextFactValue, FactBag, TriValue,
};
pub use bos_contracts::email_triage::{DryRunResult, MessageView};
use bos_contracts::email_triage::{
    EmailAttachmentEvidenceRequest, EmailAttachmentEvidenceResponse, EmailAttachmentRecord,
    EmailTriageAliasCondition, EmailTriageConditionId, EmailTriageConditionOperator,
    EmailTriageConditionTrace, EmailTriageConditionV2, EmailTriageConditionValue,
    EmailTriageDryRunTrace, EmailTriageMatchMode, EmailTriageRule, EmailTriageRuleTrace,
};
use bos_integrations::gmail_inbox_read::LiveGmailInboxReadClient;
use bos_integrations::ReqwestGmailHttpClient;
use rusqlite::params;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::{Mutex, MutexGuard, OnceLock};

pub const MODEL_BODY_MAX_BYTES: usize = 48 * 1024;
pub const MODEL_BODY_SMALL_MAX_BYTES: usize = 24 * 1024;
const CRM_FACT_POSITIVE_TTL_SECS_DEFAULT: u64 = 6 * 60 * 60;
const CRM_FACT_NEGATIVE_TTL_SECS_DEFAULT: u64 = 30 * 60;
const CRM_FACT_DRY_RUN_BUDGET: usize = 20;

/// Deliver one audited Gmail Move to Trash outbox job. Credentials are bound
/// to the mailbox that supplied the inbound message.
pub fn deliver_gmail_trash(
    state: &crate::http::AppState,
    job: &crate::outbox::ClaimedJob,
    now_ms: u64,
) -> crate::outbox::AttemptOutcome {
    let payload = match serde_json::from_str::<
        bos_integrations::gmail_trash_write::GmailTrashOutboxPayload,
    >(&job.payload_json)
    {
        Ok(payload) => payload,
        Err(err) => {
            return crate::outbox::AttemptOutcome::Terminal {
                error: format!("gmail_trash_payload_invalid:{err}"),
                result_json: None,
            };
        }
    };
    let (oauth, write_enabled) = {
        let persistence = state.persistence.lock();
        let oauth = crate::slices::google_connector::service::resolve_google_oauth(
            persistence.connection_ref(),
            &state.client_id,
            payload.credential_user_id.as_deref(),
        )
        .unwrap_or_default();
        let write_enabled = crate::slices::admin_settings::service::flag(
            persistence.connection_ref(),
            &state.client_id,
            &crate::env_registry::BOS_GMAIL_TRASH_ENABLED,
        )
        .unwrap_or_else(|err| {
            tracing::warn!(error = %err, "gmail trash gate read failed");
            false
        });
        (oauth, write_enabled)
    };
    let Some(oauth) = oauth else {
        return crate::outbox::AttemptOutcome::Retry {
            error: "google_credential_unavailable".to_string(),
            retry_at_ms: now_ms + crate::outbox::retry_backoff_ms(job.attempts),
        };
    };
    let config = bos_integrations::gmail_trash_write::GmailTrashWriteConfig {
        oauth,
        write_enabled,
    };
    let client = match bos_integrations::gmail_trash_write::gmail_trash_execution_client(&config) {
        Ok(client) => client,
        Err(bos_integrations::gmail_trash_write::GmailTrashWriteError::Retryable {
            code,
            retry_after_ms,
            ..
        }) => {
            return crate::outbox::AttemptOutcome::Retry {
                error: code,
                retry_at_ms: now_ms
                    + retry_after_ms
                        .unwrap_or_else(|| crate::outbox::retry_backoff_ms(job.attempts)),
            };
        }
        Err(bos_integrations::gmail_trash_write::GmailTrashWriteError::Permanent {
            code,
            message,
        }) => {
            return crate::outbox::AttemptOutcome::Terminal {
                error: crate::outbox::provider_error_detail(&code, &message),
                result_json: Some(serde_json::json!({ "message": message }).to_string()),
            };
        }
    };
    match client.trash_message(&payload.message_id) {
        Ok(response) => crate::outbox::AttemptOutcome::Delivered {
            result_json: serde_json::json!({
                "dry_run": response.dry_run,
                "provider_object_id": payload.message_id,
                "provider_status": response.reason,
            })
            .to_string(),
        },
        Err(bos_integrations::gmail_trash_write::GmailTrashWriteError::Retryable {
            code,
            retry_after_ms,
            ..
        }) => crate::outbox::AttemptOutcome::Retry {
            error: code,
            retry_at_ms: now_ms
                + retry_after_ms.unwrap_or_else(|| crate::outbox::retry_backoff_ms(job.attempts)),
        },
        Err(bos_integrations::gmail_trash_write::GmailTrashWriteError::Permanent {
            code,
            message,
        }) => crate::outbox::AttemptOutcome::Terminal {
            error: crate::outbox::provider_error_detail(&code, &message),
            result_json: Some(serde_json::json!({ "message": message }).to_string()),
        },
    }
}

pub fn inbound_parser_exists(parser_id: &str) -> bool {
    #[cfg(test)]
    {
        if matches!(parser_id.trim(), "test_call_log" | "test_website_inquiry") {
            return true;
        }
    }
    let _ = parser_id;
    false
}

pub fn select_inbound_parsers(
    parser_ids: &[String],
) -> Vec<&'static dyn bos_profile_api::InboundMessageParser> {
    #[cfg(test)]
    {
        let parsers = test_mutex_lock(test_inbound_parsers()).clone();
        if !parsers.is_empty() {
            return parser_ids
                .iter()
                .filter_map(|parser_id| {
                    let parser_id = parser_id.trim();
                    parsers
                        .iter()
                        .copied()
                        .find(|parser| parser.parser_id() == parser_id)
                })
                .collect();
        }
    }
    let _ = parser_ids;
    Vec::new()
}

#[cfg(test)]
static TEST_INBOUND_PARSERS: OnceLock<
    Mutex<Vec<&'static dyn bos_profile_api::InboundMessageParser>>,
> = OnceLock::new();

#[cfg(test)]
fn test_inbound_parsers() -> &'static Mutex<Vec<&'static dyn bos_profile_api::InboundMessageParser>>
{
    TEST_INBOUND_PARSERS.get_or_init(|| Mutex::new(Vec::new()))
}

#[cfg(test)]
fn test_mutex_lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}

#[cfg(test)]
pub(crate) struct TestInboundParserGuard;

#[cfg(test)]
impl Drop for TestInboundParserGuard {
    fn drop(&mut self) {
        test_mutex_lock(test_inbound_parsers()).clear();
    }
}

#[cfg(test)]
pub(crate) fn set_test_inbound_parsers(
    parsers: Vec<&'static dyn bos_profile_api::InboundMessageParser>,
) -> TestInboundParserGuard {
    *test_mutex_lock(test_inbound_parsers()) = parsers;
    TestInboundParserGuard
}

pub fn parser_input_for_message(message: &MessageView) -> bos_profile_api::InboundParserInput {
    bos_profile_api::InboundParserInput {
        source_key: message
            .message_id
            .as_deref()
            .unwrap_or_default()
            .to_string(),
        message_id: message
            .message_id
            .as_deref()
            .unwrap_or_default()
            .to_string(),
        source_user_id: message.source_user_id.clone(),
        subject: message.subject.clone(),
        from: message.from.clone(),
        to: message.to.clone(),
        body: message.body.clone(),
        labels: message.labels.clone(),
        headers: message.headers.clone(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrmFactKind {
    SenderContact,
    SenderCompany,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrmFactMiss {
    pub kind: CrmFactKind,
    pub subject: String,
    pub fact_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrmFactCacheWrite {
    pub fact_key: String,
    pub value: TriValue,
    pub provider: String,
    pub fetched_at_ms: u64,
    pub expires_at_ms: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct CrmFactTtls {
    pub positive_secs: u64,
    pub negative_secs: u64,
}

impl CrmFactTtls {
    pub fn from_env() -> Self {
        let positive =
            crate::env_registry::string(&crate::env_registry::BOS_EMAIL_TRIAGE_FACT_CACHE_TTL_SECS)
                .and_then(|raw| raw.trim().parse::<u64>().ok())
                .filter(|ttl| *ttl > 0)
                .unwrap_or(CRM_FACT_POSITIVE_TTL_SECS_DEFAULT);
        Self {
            positive_secs: positive,
            negative_secs: positive.min(CRM_FACT_NEGATIVE_TTL_SECS_DEFAULT),
        }
    }

    fn ttl_for(self, value: TriValue) -> u64 {
        match value {
            TriValue::True => self.positive_secs,
            TriValue::False => self.negative_secs,
            TriValue::Unknown => 0,
        }
    }
}

pub trait CrmLiveLookup {
    fn provider(&self) -> &'static str;
    fn contact_exists(&mut self, email: &str) -> TriValue;
    fn company_domain_exists(&mut self, domain: &str) -> TriValue;
}

#[derive(Debug, Default)]
pub struct EnvCrmLiveLookup;

impl CrmLiveLookup for EnvCrmLiveLookup {
    fn provider(&self) -> &'static str {
        configured_crm_provider_name()
    }

    fn contact_exists(&mut self, email: &str) -> TriValue {
        crm_contact_exists_tri(email)
    }

    fn company_domain_exists(&mut self, domain: &str) -> TriValue {
        crm_company_domain_exists_tri(domain)
    }
}

#[derive(Debug)]
pub enum AttachmentEvidenceError {
    MessageNotFound,
    AttachmentNotFound,
    CredentialMissing,
    AttachmentTooLarge,
    Provider(String),
    Io(std::io::Error),
    Store(crate::store_core::StoreError),
    JoinFailed,
}

impl From<crate::store_core::StoreError> for AttachmentEvidenceError {
    fn from(err: crate::store_core::StoreError) -> Self {
        Self::Store(err)
    }
}

pub async fn stage_attachment_evidence(
    state: crate::http::AppState,
    actor_id: String,
    scope: crate::http::OperatorScope,
    source_key: String,
    attachment_id: String,
    request: EmailAttachmentEvidenceRequest,
) -> Result<EmailAttachmentEvidenceResponse, AttachmentEvidenceError> {
    stage_attachment_evidence_inner(
        state,
        actor_id,
        scope,
        source_key,
        attachment_id,
        request,
        false,
    )
    .await
}

pub async fn stage_attachment_evidence_for_launch(
    state: crate::http::AppState,
    actor_id: String,
    scope: crate::http::OperatorScope,
    source_key: String,
    attachment_id: String,
    request: EmailAttachmentEvidenceRequest,
) -> Result<EmailAttachmentEvidenceResponse, AttachmentEvidenceError> {
    stage_attachment_evidence_inner(
        state,
        actor_id,
        scope,
        source_key,
        attachment_id,
        request,
        true,
    )
    .await
}

async fn stage_attachment_evidence_inner(
    state: crate::http::AppState,
    actor_id: String,
    scope: crate::http::OperatorScope,
    source_key: String,
    attachment_id: String,
    request: EmailAttachmentEvidenceRequest,
    allow_target_dir: bool,
) -> Result<EmailAttachmentEvidenceResponse, AttachmentEvidenceError> {
    tokio::task::spawn_blocking(move || {
        stage_attachment_evidence_blocking(
            state,
            actor_id,
            scope,
            source_key,
            attachment_id,
            request,
            allow_target_dir,
        )
    })
    .await
    .map_err(|_| AttachmentEvidenceError::JoinFailed)?
}

fn stage_attachment_evidence_blocking(
    state: crate::http::AppState,
    actor_id: String,
    scope: crate::http::OperatorScope,
    source_key: String,
    attachment_id: String,
    request: EmailAttachmentEvidenceRequest,
    allow_target_dir: bool,
) -> Result<EmailAttachmentEvidenceResponse, AttachmentEvidenceError> {
    let (message, attachment, oauth, max_bytes, retention_days) = {
        let persistence = state.persistence.lock();
        let messages = super::store::inbound_by_source_keys(
            persistence.connection_ref(),
            &state.client_id,
            std::slice::from_ref(&source_key),
            &scope,
        )?;
        let message = messages
            .into_iter()
            .next()
            .ok_or(AttachmentEvidenceError::MessageNotFound)?;
        let attachment = message
            .attachments
            .iter()
            .find(|candidate| candidate.attachment_id == attachment_id)
            .cloned()
            .ok_or(AttachmentEvidenceError::AttachmentNotFound)?;
        let oauth = crate::slices::google_connector::service::resolve_google_oauth(
            persistence.connection_ref(),
            &state.client_id,
            message.source_user_id.as_deref(),
        )?
        .ok_or(AttachmentEvidenceError::CredentialMissing)?;
        let max_bytes = crate::slices::admin_settings::service::usize_or(
            persistence.connection_ref(),
            &state.client_id,
            &crate::env_registry::BOS_AGENT_EVIDENCE_MAX_BYTES,
            10 * 1024 * 1024,
        )?;
        let retention_days = crate::slices::admin_settings::service::usize_or(
            persistence.connection_ref(),
            &state.client_id,
            &crate::env_registry::BOS_AGENT_EVIDENCE_RETENTION_DAYS,
            30,
        )?
        .max(1) as u64;
        (message, attachment, oauth, max_bytes, retention_days)
    };

    if attachment
        .size_bytes
        .is_some_and(|size| size > max_bytes as u64)
    {
        return Err(AttachmentEvidenceError::AttachmentTooLarge);
    }

    let client = LiveGmailInboxReadClient::from_credentials(
        std::sync::Arc::new(ReqwestGmailHttpClient::default()),
        &oauth,
    )
    .map_err(|err| AttachmentEvidenceError::Provider(err.to_string()))?;
    let bytes = client
        .read_attachment(&message.message_id, &attachment_id)
        .map_err(|err| AttachmentEvidenceError::Provider(err.to_string()))?;
    if bytes.len() > max_bytes {
        return Err(AttachmentEvidenceError::AttachmentTooLarge);
    }

    let now_ms = crate::http::now_ms();
    let retention_until_ms = now_ms.saturating_add(retention_days * 24 * 60 * 60 * 1000);
    let evidence_id = evidence_id_for(
        &state.client_id,
        &request.session_id,
        &source_key,
        &attachment_id,
        &request.idempotency_key,
    );
    let filename = safe_filename(&attachment);
    let path = evidence_path(
        allow_target_dir
            .then_some(request.target_dir.as_deref())
            .flatten(),
        &state.client_id,
        &request.session_id,
        &evidence_id,
        &filename,
    );
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(AttachmentEvidenceError::Io)?;
    }
    std::fs::write(&path, &bytes).map_err(AttachmentEvidenceError::Io)?;

    let path_string = path.to_string_lossy().to_string();
    let evidence = super::store::AgentEvidenceFile {
        evidence_id: evidence_id.clone(),
        session_id: request.session_id.clone(),
        item_id: request.item_id.clone(),
        source_ref: source_key.clone(),
        attachment_id: attachment_id.clone(),
        path: path_string.clone(),
        filename: filename.clone(),
        mime_type: attachment.mime_type.clone(),
        size_bytes: bytes.len() as u64,
        retention_until_ms,
    };
    {
        let mut persistence = state.persistence.lock();
        super::store::record_agent_evidence_file(
            persistence.connection(),
            super::store::AgentEvidenceWrite {
                client_id: &state.client_id,
                actor_id: &actor_id,
                evidence: &evidence,
                idempotency_key: &request.idempotency_key,
                now_ms,
            },
        )?;
    }

    Ok(EmailAttachmentEvidenceResponse {
        evidence_id,
        session_id: request.session_id,
        message_id: source_key,
        attachment_id,
        filename,
        mime_type: attachment.mime_type,
        size_bytes: bytes.len() as u64,
        path: path_string,
        retention_until_ms,
    })
}

fn evidence_id_for(
    client_id: &str,
    session_id: &str,
    message_id: &str,
    attachment_id: &str,
    idempotency_key: &str,
) -> String {
    let mut hasher = Sha256::new();
    for part in [
        client_id,
        session_id,
        message_id,
        attachment_id,
        idempotency_key,
    ] {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    let digest = hasher.finalize();
    let mut hash = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        hash.push_str(&format!("{byte:02x}"));
    }
    format!("ev_{hash}")
}

fn evidence_path(
    target_dir: Option<&str>,
    client_id: &str,
    session_id: &str,
    evidence_id: &str,
    filename: &str,
) -> PathBuf {
    let root = target_dir
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| Path::new(value).join(".bos-agent-evidence"))
        .unwrap_or_else(|| {
            PathBuf::from(
                crate::env_registry::string(&crate::env_registry::BOS_AGENT_EVIDENCE_ROOT_DIR)
                    .unwrap_or_else(|| "var/agent-evidence".to_string()),
            )
        });
    root.join(safe_path_segment(client_id))
        .join(safe_path_segment(session_id))
        .join(safe_path_segment(evidence_id))
        .join(filename)
}

fn safe_filename(attachment: &EmailAttachmentRecord) -> String {
    let raw = attachment.filename.trim();
    let raw = if raw.is_empty() { "attachment" } else { raw };
    let sanitized = safe_path_segment(raw);
    if sanitized.is_empty() {
        "attachment".to_string()
    } else {
        sanitized
    }
}

fn safe_path_segment(raw: &str) -> String {
    raw.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('.')
        .chars()
        .take(120)
        .collect()
}

pub fn resolve_category(
    rules: &[EmailTriageRule],
    fallback_category: &str,
    message: &MessageView,
) -> String {
    resolve_rule(rules, message)
        .map(|rule| rule.pinned_category.clone())
        .unwrap_or_else(|| fallback_category.to_string())
}

pub fn resolve_rule<'a>(
    rules: &'a [EmailTriageRule],
    message: &MessageView,
) -> Option<&'a EmailTriageRule> {
    let mut bag = FactBag::new(None, "", message, None, None, CrmFactOverrides::default());
    resolve_rule_with_fact_bag(rules, &mut bag)
}

/// Classify each sample against the given rules (stored + proposed, already
/// merged by the caller).
pub fn dry_run(
    rules: &[EmailTriageRule],
    fallback_category: &str,
    samples: &[MessageView],
) -> Vec<DryRunResult> {
    dry_run_traces(
        rules,
        fallback_category,
        samples,
        vec![CrmFactOverrides::default(); samples.len()],
    )
    .into_iter()
    .map(|trace| DryRunResult {
        resolved_category: trace.resolved_category,
        matched_rule_id: trace.matched_rule_id,
    })
    .collect()
}

pub fn dry_run_traces(
    rules: &[EmailTriageRule],
    fallback_category: &str,
    samples: &[MessageView],
    crm_overrides: Vec<CrmFactOverrides>,
) -> Vec<EmailTriageDryRunTrace> {
    samples
        .iter()
        .enumerate()
        .map(|(index, sample)| {
            let crm = crm_overrides.get(index).cloned().unwrap_or_default();
            let mut bag = FactBag::new(
                None,
                "",
                sample,
                sample.message_id.as_deref(),
                sample.source_user_id.as_deref(),
                crm,
            );
            evaluate_trace(index as u32, rules, fallback_category, &mut bag)
        })
        .collect()
}

pub fn dry_run_traces_with_fact_bags<'a>(
    rules: &[EmailTriageRule],
    fallback_category: &str,
    bags: Vec<FactBag<'a>>,
) -> Vec<EmailTriageDryRunTrace> {
    bags.into_iter()
        .enumerate()
        .map(|(index, mut bag)| evaluate_trace(index as u32, rules, fallback_category, &mut bag))
        .collect()
}

pub(crate) fn merge_rules_for_dry_run(
    stored: Vec<EmailTriageRule>,
    proposed: Vec<EmailTriageRule>,
) -> Vec<EmailTriageRule> {
    let proposed_ids: std::collections::HashSet<String> =
        proposed.iter().map(|rule| rule.rule_id.clone()).collect();
    let mut merged: Vec<EmailTriageRule> = stored
        .into_iter()
        .filter(|rule| !proposed_ids.contains(&rule.rule_id))
        .chain(proposed)
        .collect();
    merged.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then_with(|| a.rule_id.cmp(&b.rule_id))
    });
    merged
}

pub fn rules_need_crm_facts(rules: &[EmailTriageRule]) -> bool {
    rules.iter().filter(|rule| rule.enabled).any(|rule| {
        super::legacy::effective_conditions(rule)
            .iter()
            .any(|condition| {
                matches!(
                    condition.condition_id,
                    EmailTriageConditionId::CrmSenderContactExists
                        | EmailTriageConditionId::CrmSenderCompanyExists
                        | EmailTriageConditionId::CrmSenderDealExists
                        | EmailTriageConditionId::CrmSenderDealStage
                        | EmailTriageConditionId::CrmSenderDealPipeline
                )
            })
    })
}

pub(crate) fn resolve_rule_with_fact_bag<'a>(
    rules: &'a [EmailTriageRule],
    bag: &mut FactBag<'_>,
) -> Option<&'a EmailTriageRule> {
    rules.iter().filter(|rule| rule.enabled).find(|rule| {
        let conditions = super::legacy::effective_conditions(rule);
        !conditions.is_empty()
            && evaluate_conditions(rule.match_mode, &conditions, bag) == TriValue::True
    })
}

fn evaluate_trace(
    sample_index: u32,
    rules: &[EmailTriageRule],
    fallback_category: &str,
    bag: &mut FactBag<'_>,
) -> EmailTriageDryRunTrace {
    let mut rule_traces = Vec::new();
    let mut matched_rule_id = None;
    let mut resolved_category = fallback_category.to_string();
    let mut needs_fact_refresh = false;
    for rule in rules.iter().filter(|rule| rule.enabled) {
        let conditions = super::legacy::effective_conditions(rule);
        if conditions.is_empty() {
            continue;
        }
        let (result, condition_traces) =
            evaluate_conditions_with_trace(rule.match_mode, &conditions, bag);
        if result.is_unknown() {
            needs_fact_refresh = true;
        }
        let matched = result == TriValue::True;
        rule_traces.push(EmailTriageRuleTrace {
            rule_id: rule.rule_id.clone(),
            result: result.to_contract(),
            matched,
            condition_traces,
        });
        if matched {
            matched_rule_id = Some(rule.rule_id.clone());
            resolved_category = rule.pinned_category.clone();
            break;
        }
    }
    EmailTriageDryRunTrace {
        sample_index,
        resolved_category,
        matched_rule_id,
        needs_fact_refresh,
        rule_traces,
        fact_traces: bag.drain_traces(),
    }
}

fn evaluate_conditions(
    mode: EmailTriageMatchMode,
    conditions: &[EmailTriageConditionV2],
    bag: &mut FactBag<'_>,
) -> TriValue {
    evaluate_conditions_with_trace(mode, conditions, bag).0
}

fn evaluate_conditions_with_trace(
    mode: EmailTriageMatchMode,
    conditions: &[EmailTriageConditionV2],
    bag: &mut FactBag<'_>,
) -> (TriValue, Vec<EmailTriageConditionTrace>) {
    let mut traces = Vec::new();
    let mut saw_unknown = false;
    for condition in conditions {
        let result = condition_matches_v2(condition, bag);
        traces.push(EmailTriageConditionTrace {
            condition_id: condition.condition_id,
            label: fact_label(condition.condition_id).to_string(),
            result: result.to_contract(),
            detail: match result {
                TriValue::True => "matched".to_string(),
                TriValue::False => "did not match".to_string(),
                TriValue::Unknown => "couldn't check yet".to_string(),
            },
        });
        match (mode, result) {
            (EmailTriageMatchMode::All, TriValue::False) => return (TriValue::False, traces),
            (EmailTriageMatchMode::Any, TriValue::True) => return (TriValue::True, traces),
            (_, TriValue::Unknown) => saw_unknown = true,
            _ => {}
        }
    }
    if saw_unknown {
        (TriValue::Unknown, traces)
    } else if mode == EmailTriageMatchMode::All {
        (TriValue::True, traces)
    } else {
        (TriValue::False, traces)
    }
}

fn condition_matches_v2(condition: &EmailTriageConditionV2, bag: &mut FactBag<'_>) -> TriValue {
    match condition.condition_id {
        EmailTriageConditionId::QuickKnownCustomer
        | EmailTriageConditionId::QuickNewSalesLead
        | EmailTriageConditionId::QuickBillingFollowup
        | EmailTriageConditionId::QuickExistingWorkThread => {
            let (mode, conditions) = alias_conditions(condition.condition_id);
            evaluate_conditions(mode, &conditions, bag)
        }
        EmailTriageConditionId::MessageLabel => label_condition_matches_v2(bag.labels(), condition),
        EmailTriageConditionId::MessageHeader => {
            let EmailTriageConditionValue::Header { name, value } = &condition.value else {
                return TriValue::False;
            };
            text_op_matches(bag.header_value(name), condition.op, value)
        }
        EmailTriageConditionId::MessageFrom
        | EmailTriageConditionId::MessageTo
        | EmailTriageConditionId::MessageFromEmail
        | EmailTriageConditionId::MessageFromDomain
        | EmailTriageConditionId::MessageSubject
        | EmailTriageConditionId::MessageBody
        | EmailTriageConditionId::SourceAccountUserId
        | EmailTriageConditionId::SourceProvider => {
            text_condition_matches(bag.text_value(condition.condition_id), condition)
        }
        EmailTriageConditionId::CrmSenderDealStage
        | EmailTriageConditionId::CrmSenderDealPipeline => {
            text_values_condition_matches(bag.text_values(condition.condition_id), condition)
        }
        _ => bool_op_matches(bag.fact(condition.condition_id), condition),
    }
}

pub(crate) fn alias_conditions(
    id: EmailTriageConditionId,
) -> (EmailTriageMatchMode, Vec<EmailTriageConditionV2>) {
    super::catalog::condition_catalog()
        .groups
        .into_iter()
        .flat_map(|group| group.items)
        .find(|item| item.condition_id == id)
        .and_then(|item| item.expansion)
        .map(|expansion| {
            (
                expansion.match_mode,
                expansion
                    .conditions
                    .into_iter()
                    .map(alias_condition_to_v2)
                    .collect(),
            )
        })
        .unwrap_or((EmailTriageMatchMode::All, Vec::new()))
}

fn alias_condition_to_v2(condition: EmailTriageAliasCondition) -> EmailTriageConditionV2 {
    EmailTriageConditionV2 {
        condition_id: condition.condition_id,
        op: condition.op,
        value: condition.value,
    }
}

fn label_condition_matches_v2(labels: &[String], condition: &EmailTriageConditionV2) -> TriValue {
    if condition.op == EmailTriageConditionOperator::Exists {
        return if labels.iter().any(|label| !label.trim().is_empty()) {
            TriValue::True
        } else {
            TriValue::False
        };
    }
    let expected = text_value_from_condition(&condition.value);
    if labels
        .iter()
        .any(|label| text_op_matches(Some(label), condition.op, expected) == TriValue::True)
    {
        TriValue::True
    } else {
        TriValue::False
    }
}

fn bool_op_matches(value: TriValue, condition: &EmailTriageConditionV2) -> TriValue {
    if value == TriValue::Unknown {
        return TriValue::Unknown;
    }
    match condition.op {
        EmailTriageConditionOperator::Exists => {
            // For bool facts, "exists" means the fact was checkable.
            TriValue::True
        }
        EmailTriageConditionOperator::IsTrue => value,
        EmailTriageConditionOperator::IsFalse => match value {
            TriValue::True => TriValue::False,
            TriValue::False => TriValue::True,
            TriValue::Unknown => TriValue::Unknown,
        },
        EmailTriageConditionOperator::Equals => match &condition.value {
            EmailTriageConditionValue::Bool(expected) => {
                let actual = value == TriValue::True;
                if actual == *expected {
                    TriValue::True
                } else {
                    TriValue::False
                }
            }
            _ => TriValue::False,
        },
        _ => TriValue::False,
    }
}

fn text_value_from_condition(value: &EmailTriageConditionValue) -> &str {
    match value {
        EmailTriageConditionValue::Text(value)
        | EmailTriageConditionValue::Date(value)
        | EmailTriageConditionValue::Header { value, .. } => value,
        _ => "",
    }
}

fn text_condition_matches(value: Option<&str>, condition: &EmailTriageConditionV2) -> TriValue {
    match condition.op {
        EmailTriageConditionOperator::In => {
            let matched = value.is_some_and(|actual| {
                string_list_values(&condition.value)
                    .iter()
                    .any(|expected| equals_case_insensitive(Some(actual), expected))
            });
            if matched {
                TriValue::True
            } else {
                TriValue::False
            }
        }
        op => text_op_matches(value, op, text_value_from_condition(&condition.value)),
    }
}

fn text_values_condition_matches(
    values: Option<Vec<String>>,
    condition: &EmailTriageConditionV2,
) -> TriValue {
    let Some(values) = values else {
        return TriValue::Unknown;
    };
    if condition.op == EmailTriageConditionOperator::Exists {
        return if values.iter().any(|value| !value.trim().is_empty()) {
            TriValue::True
        } else {
            TriValue::False
        };
    }
    let matched = values
        .iter()
        .any(|value| text_condition_matches(Some(value.as_str()), condition) == TriValue::True);
    if matched {
        TriValue::True
    } else {
        TriValue::False
    }
}

fn string_list_values(value: &EmailTriageConditionValue) -> Vec<String> {
    match value {
        EmailTriageConditionValue::StringList(values) => values
            .iter()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect(),
        EmailTriageConditionValue::Text(value) => value
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn push_distinct_trimmed(values: &mut Vec<String>, raw: Option<&str>) {
    let Some(value) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return;
    };
    if !values
        .iter()
        .any(|existing| existing.eq_ignore_ascii_case(value))
    {
        values.push(value.to_string());
    }
}

fn text_op_matches(
    value: Option<&str>,
    op: EmailTriageConditionOperator,
    expected: &str,
) -> TriValue {
    let matched = match op {
        EmailTriageConditionOperator::Exists => value.is_some_and(|value| !value.trim().is_empty()),
        EmailTriageConditionOperator::Contains => contains_case_insensitive(value, expected),
        EmailTriageConditionOperator::Equals => equals_case_insensitive(value, expected),
        EmailTriageConditionOperator::StartsWith => starts_with_case_insensitive(value, expected),
        EmailTriageConditionOperator::Regex => regex_matches(value, expected),
        _ => false,
    };
    if matched {
        TriValue::True
    } else {
        TriValue::False
    }
}

pub fn crm_provider_budget_per_message() -> usize {
    crate::env_registry::string(
        &crate::env_registry::BOS_EMAIL_TRIAGE_FACT_PROVIDER_BUDGET_PER_MESSAGE,
    )
    .and_then(|raw| raw.trim().parse::<usize>().ok())
    .unwrap_or(2)
}

pub fn crm_dry_run_budget() -> usize {
    CRM_FACT_DRY_RUN_BUDGET
}

pub fn crm_sender_policy(
    conn: &rusqlite::Connection,
    client_id: &str,
) -> super::subjects::CrmSenderPolicy {
    let raw = crate::slices::admin_settings::service::value(
        conn,
        client_id,
        &crate::env_registry::BOS_CRM_CONTEXT_NEUTRAL_SENDER_DOMAINS,
    )
    .ok()
    .flatten();
    super::subjects::CrmSenderPolicy::from_domain_roots(raw.as_deref())
}

pub fn crm_identity_email_for_message(
    from: Option<&str>,
    headers: &[(String, String)],
    policy: &super::subjects::CrmSenderPolicy,
) -> Result<String, &'static str> {
    if let Some(reason) = super::subjects::crm_header_identity_block_reason(headers) {
        return Err(reason);
    }
    super::subjects::crm_lookup_email(from, policy)
}

pub fn crm_identity_email_for_inbound_record(
    record: &super::store::InboundMessageRecord,
    policy: &super::subjects::CrmSenderPolicy,
) -> Result<String, &'static str> {
    crm_identity_email_for_message(record.from_addr.as_deref(), &record.headers, policy)
}

pub fn crm_fact_overrides_from_cache(
    conn: &rusqlite::Connection,
    client_id: &str,
    message: &MessageView,
    now_ms: u64,
) -> (CrmFactOverrides, Vec<CrmFactMiss>) {
    let policy = crm_sender_policy(conn, client_id);
    let lookup_subject = match crm_lookup_subject(conn, client_id, message, &policy) {
        Ok(subject) => subject,
        Err(
            "automated_email_headers"
            | "bulk_email_headers"
            | "mailing_list_headers"
            | "sender_is_platform_or_automation",
        ) => {
            return (
                CrmFactOverrides {
                    sender_contact_exists: Some(CrmFactValue::unknown(
                        "not checked automatically for platform, bulk, or automation senders",
                    )),
                    sender_company_exists: Some(CrmFactValue::unknown(
                        "not checked automatically for platform, bulk, or automation senders",
                    )),
                    sender_deal_exists: Some(CrmFactValue::unknown(
                        "not checked automatically for platform, bulk, or automation senders",
                    )),
                    sender_deal_stages: Some(CrmTextFactValue::unknown(
                        "not checked automatically for platform, bulk, or automation senders",
                    )),
                    sender_deal_pipelines: Some(CrmTextFactValue::unknown(
                        "not checked automatically for platform, bulk, or automation senders",
                    )),
                },
                Vec::new(),
            );
        }
        _ => {
            return (
                CrmFactOverrides {
                    sender_contact_exists: Some(CrmFactValue::unknown(
                        "couldn't check yet (sender email is unclear)",
                    )),
                    sender_company_exists: Some(CrmFactValue::unknown(
                        "couldn't check yet (sender email is unclear)",
                    )),
                    sender_deal_exists: Some(CrmFactValue::unknown(
                        "couldn't check yet (sender email is unclear)",
                    )),
                    sender_deal_stages: Some(CrmTextFactValue::unknown(
                        "couldn't check yet (sender email is unclear)",
                    )),
                    sender_deal_pipelines: Some(CrmTextFactValue::unknown(
                        "couldn't check yet (sender email is unclear)",
                    )),
                },
                Vec::new(),
            );
        }
    };
    let sender_email = lookup_subject.email;
    let sender_domain = email_domain(&sender_email);
    let mut overrides = CrmFactOverrides::default();
    let mut misses = Vec::new();
    if sender_contact_exists_from_snapshot(conn, client_id, &sender_email).unwrap_or(false) {
        overrides.sender_contact_exists = Some(CrmFactValue::cache(
            TriValue::True,
            format!("found {} in CRM contact snapshots", lookup_subject.detail),
        ));
    } else {
        read_one_crm_fact_cache(
            conn,
            client_id,
            CrmFactKind::SenderContact,
            &sender_email,
            now_ms,
            &mut overrides,
            &mut misses,
        );
    }
    apply_crm_deal_snapshot_facts(conn, client_id, &sender_email, &mut overrides);
    if let Some(domain) = sender_domain {
        if sender_company_exists_from_snapshot(conn, client_id, &domain).unwrap_or(false) {
            overrides.sender_company_exists = Some(CrmFactValue::cache(
                TriValue::True,
                format!(
                    "found {} domain in CRM contact snapshots",
                    lookup_subject.detail
                ),
            ));
        } else {
            read_one_crm_fact_cache(
                conn,
                client_id,
                CrmFactKind::SenderCompany,
                &domain,
                now_ms,
                &mut overrides,
                &mut misses,
            );
        }
    } else {
        overrides.sender_company_exists = Some(CrmFactValue::unknown(
            "couldn't check yet (sender company domain is unclear)",
        ));
    }
    (overrides, misses)
}

fn apply_crm_deal_snapshot_facts(
    conn: &rusqlite::Connection,
    client_id: &str,
    sender_email: &str,
    overrides: &mut CrmFactOverrides,
) {
    match sender_deal_facets_from_snapshot(conn, client_id, sender_email) {
        Ok(facets) => {
            let detail = if facets.deal_count == 0 {
                "no associated deals in CRM snapshots".to_string()
            } else {
                format!(
                    "{} associated CRM deal{} in snapshots",
                    facets.deal_count,
                    if facets.deal_count == 1 { "" } else { "s" }
                )
            };
            overrides.sender_deal_exists = Some(CrmFactValue::cache(
                if facets.deal_count > 0 {
                    TriValue::True
                } else {
                    TriValue::False
                },
                detail.clone(),
            ));
            overrides.sender_deal_stages =
                Some(CrmTextFactValue::cache(facets.stages, detail.clone()));
            overrides.sender_deal_pipelines =
                Some(CrmTextFactValue::cache(facets.pipelines, detail));
        }
        Err(err) => {
            tracing::warn!(error = %err, "email triage CRM deal snapshot read failed");
            overrides.sender_deal_exists = Some(CrmFactValue::unknown(
                "couldn't check yet (CRM deal snapshots unavailable)",
            ));
            overrides.sender_deal_stages = Some(CrmTextFactValue::unknown(
                "couldn't check yet (CRM deal snapshots unavailable)",
            ));
            overrides.sender_deal_pipelines = Some(CrmTextFactValue::unknown(
                "couldn't check yet (CRM deal snapshots unavailable)",
            ));
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CrmLookupSubject {
    email: String,
    detail: String,
}

fn crm_lookup_subject(
    conn: &rusqlite::Connection,
    client_id: &str,
    message: &MessageView,
    policy: &super::subjects::CrmSenderPolicy,
) -> Result<CrmLookupSubject, &'static str> {
    if let Some(source_key) = message.message_id.as_deref() {
        if let Ok(Some(identity)) =
            super::store::best_represented_identity(conn, client_id, source_key)
        {
            return Ok(CrmLookupSubject {
                email: identity.email,
                detail: format!("represented contact from {}", identity.parser_id),
            });
        }
    }
    crm_identity_email_for_message(message.from.as_deref(), &message.headers, policy).map(|email| {
        CrmLookupSubject {
            email,
            detail: "sender".to_string(),
        }
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SenderDealFacets {
    deal_count: usize,
    stages: Vec<String>,
    pipelines: Vec<String>,
}

fn sender_contact_exists_from_snapshot(
    conn: &rusqlite::Connection,
    client_id: &str,
    sender_email: &str,
) -> Result<bool, crate::store_core::StoreError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM crm_contact_snapshots \
         WHERE client_id = ?1 AND active = 1 AND lower(email) = lower(?2)",
        params![client_id, sender_email.trim()],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

fn sender_company_exists_from_snapshot(
    conn: &rusqlite::Connection,
    client_id: &str,
    sender_domain: &str,
) -> Result<bool, crate::store_core::StoreError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM crm_contact_snapshots \
         WHERE client_id = ?1 AND active = 1 AND email IS NOT NULL \
           AND instr(email, '@') > 0 \
           AND lower(substr(email, instr(email, '@') + 1)) = lower(?2)",
        params![client_id, sender_domain.trim()],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

fn sender_deal_facets_from_snapshot(
    conn: &rusqlite::Connection,
    client_id: &str,
    sender_email: &str,
) -> Result<SenderDealFacets, crate::store_core::StoreError> {
    let mut stmt = conn.prepare(
        "SELECT stage, pipeline FROM crm_deal_snapshots \
         WHERE client_id = ?1 AND active = 1 AND lower(associated_contact_email) = lower(?2) \
         ORDER BY close_date DESC, provider_deal_id DESC",
    )?;
    let rows = stmt.query_map(params![client_id, sender_email.trim()], |row| {
        Ok((
            row.get::<_, Option<String>>(0)?,
            row.get::<_, Option<String>>(1)?,
        ))
    })?;
    let mut deal_count = 0usize;
    let mut stages = Vec::new();
    let mut pipelines = Vec::new();
    for row in rows {
        let (stage, pipeline) = row?;
        deal_count += 1;
        push_distinct_trimmed(&mut stages, stage.as_deref());
        push_distinct_trimmed(&mut pipelines, pipeline.as_deref());
    }
    Ok(SenderDealFacets {
        deal_count,
        stages,
        pipelines,
    })
}

pub fn resolve_crm_fact_misses<L: CrmLiveLookup>(
    misses: &[CrmFactMiss],
    budget_remaining: &mut usize,
    lookup: &mut L,
    now_ms: u64,
    ttls: CrmFactTtls,
) -> (CrmFactOverrides, Vec<CrmFactCacheWrite>) {
    let mut overrides = CrmFactOverrides::default();
    let mut writes = Vec::new();
    for miss in misses {
        if *budget_remaining == 0 {
            apply_crm_fact_override(
                &mut overrides,
                miss.kind,
                CrmFactValue::unknown("couldn't check yet (rate-limited)"),
            );
            continue;
        }
        *budget_remaining -= 1;
        let value = match miss.kind {
            CrmFactKind::SenderContact => lookup.contact_exists(&miss.subject),
            CrmFactKind::SenderCompany => lookup.company_domain_exists(&miss.subject),
        };
        let fact = if value == TriValue::Unknown {
            CrmFactValue::unknown("couldn't check yet (CRM lookup failed)")
        } else {
            CrmFactValue::live(value)
        };
        apply_crm_fact_override(&mut overrides, miss.kind, fact);
        if value != TriValue::Unknown {
            let ttl = ttls.ttl_for(value);
            writes.push(CrmFactCacheWrite {
                fact_key: miss.fact_key.clone(),
                value,
                provider: lookup.provider().to_string(),
                fetched_at_ms: now_ms,
                expires_at_ms: now_ms.saturating_add(ttl.saturating_mul(1000)),
            });
        }
    }
    (overrides, writes)
}

pub fn merge_crm_fact_overrides(base: &mut CrmFactOverrides, patch: CrmFactOverrides) {
    if patch.sender_contact_exists.is_some() {
        base.sender_contact_exists = patch.sender_contact_exists;
    }
    if patch.sender_company_exists.is_some() {
        base.sender_company_exists = patch.sender_company_exists;
    }
    if patch.sender_deal_exists.is_some() {
        base.sender_deal_exists = patch.sender_deal_exists;
    }
    if patch.sender_deal_stages.is_some() {
        base.sender_deal_stages = patch.sender_deal_stages;
    }
    if patch.sender_deal_pipelines.is_some() {
        base.sender_deal_pipelines = patch.sender_deal_pipelines;
    }
}

pub fn persist_crm_fact_cache_writes(
    conn: &mut rusqlite::Connection,
    client_id: &str,
    writes: &[CrmFactCacheWrite],
) -> Result<(), crate::store_core::StoreError> {
    for write in writes {
        let fact_json = crm_fact_json(write.value);
        let idempotency_key = format!(
            "email-triage-fact-cache:{}:{}",
            write.fact_key, write.fetched_at_ms
        );
        super::store::upsert_cached_fact(
            conn,
            super::store::FactCacheWrite {
                client_id,
                fact_key: &write.fact_key,
                fact_json: &fact_json,
                source_kind: "crm",
                provider: Some(&write.provider),
                fetched_at_ms: write.fetched_at_ms,
                expires_at_ms: write.expires_at_ms,
                last_error: None,
                idempotency_key: &idempotency_key,
                now_ms: write.fetched_at_ms,
            },
        )?;
    }
    Ok(())
}

fn read_one_crm_fact_cache(
    conn: &rusqlite::Connection,
    client_id: &str,
    kind: CrmFactKind,
    subject: &str,
    now_ms: u64,
    overrides: &mut CrmFactOverrides,
    misses: &mut Vec<CrmFactMiss>,
) {
    let fact_key = crm_fact_key(kind, subject);
    match super::store::read_cached_fact(conn, client_id, &fact_key) {
        Ok(Some(row))
            if row.expires_at_ms > now_ms
                && row.last_error.is_none()
                && row.provider.as_deref() == Some(configured_crm_provider_name()) =>
        {
            let value = crm_fact_value_from_json(&row.fact_json).unwrap_or(TriValue::Unknown);
            apply_crm_fact_override(
                overrides,
                kind,
                CrmFactValue::cache(value, cache_hit_detail(row.fetched_at_ms, now_ms)),
            );
        }
        Ok(_) => misses.push(CrmFactMiss {
            kind,
            subject: subject.to_string(),
            fact_key,
        }),
        Err(err) => {
            tracing::warn!(error = %err, "email triage CRM fact cache read failed");
            apply_crm_fact_override(
                overrides,
                kind,
                CrmFactValue::unknown("couldn't check yet (saved lookup unavailable)"),
            );
        }
    }
}

fn apply_crm_fact_override(
    overrides: &mut CrmFactOverrides,
    kind: CrmFactKind,
    fact: CrmFactValue,
) {
    match kind {
        CrmFactKind::SenderContact => overrides.sender_contact_exists = Some(fact),
        CrmFactKind::SenderCompany => overrides.sender_company_exists = Some(fact),
    }
}

fn crm_fact_key(kind: CrmFactKind, subject: &str) -> String {
    let prefix = match kind {
        CrmFactKind::SenderContact => "crm.sender_contact.exists",
        CrmFactKind::SenderCompany => "crm.sender_company.exists",
    };
    format!("{prefix}:{}", subject.trim().to_ascii_lowercase())
}

fn crm_fact_json(value: TriValue) -> String {
    let value = match value {
        TriValue::True => "true",
        TriValue::False => "false",
        TriValue::Unknown => "unknown",
    };
    serde_json::json!({ "value": value }).to_string()
}

fn crm_fact_value_from_json(raw: &str) -> Option<TriValue> {
    let json: serde_json::Value = serde_json::from_str(raw).ok()?;
    match json.get("value")?.as_str()? {
        "true" => Some(TriValue::True),
        "false" => Some(TriValue::False),
        "unknown" => Some(TriValue::Unknown),
        _ => None,
    }
}

fn cache_hit_detail(fetched_at_ms: u64, now_ms: u64) -> String {
    let age_secs = now_ms.saturating_sub(fetched_at_ms) / 1000;
    if age_secs < 60 {
        "from saved lookup (checked just now)".to_string()
    } else if age_secs < 3_600 {
        format!("from saved lookup (checked {}m ago)", age_secs / 60)
    } else {
        format!("from saved lookup (checked {}h ago)", age_secs / 3_600)
    }
}

fn configured_crm_provider_name() -> &'static str {
    match crate::slices::crm_drafts::service::configured_crm_provider() {
        Ok(crate::slices::crm_drafts::service::PROVIDER_ESPOCRM) => {
            crate::slices::crm_drafts::service::PROVIDER_ESPOCRM
        }
        _ => crate::slices::crm_drafts::service::PROVIDER_HUBSPOT,
    }
}

fn contains_case_insensitive(value: Option<&str>, needle: &str) -> bool {
    let needle = needle.trim();
    if needle.is_empty() {
        return false;
    }
    value.is_some_and(|value| {
        value
            .to_ascii_lowercase()
            .contains(&needle.to_ascii_lowercase())
    })
}

fn equals_case_insensitive(value: Option<&str>, expected: &str) -> bool {
    let expected = expected.trim();
    if expected.is_empty() {
        return false;
    }
    value.is_some_and(|value| value.trim().eq_ignore_ascii_case(expected))
}

fn starts_with_case_insensitive(value: Option<&str>, prefix: &str) -> bool {
    let prefix = prefix.trim();
    if prefix.is_empty() {
        return false;
    }
    value.is_some_and(|value| {
        value
            .to_ascii_lowercase()
            .starts_with(&prefix.to_ascii_lowercase())
    })
}

fn regex_matches(value: Option<&str>, pattern: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return false;
    }
    let Ok(regex) = regex::RegexBuilder::new(pattern)
        .case_insensitive(true)
        .build()
    else {
        return false;
    };
    value.is_some_and(|value| regex.is_match(value))
}

fn email_domain(email: &str) -> Option<String> {
    let domain = email
        .split_once('@')?
        .1
        .trim()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if domain.is_empty() || super::subjects::public_mailbox_domain(&domain) {
        return None;
    }
    Some(domain)
}

fn crm_contact_exists_tri(email: &str) -> TriValue {
    match crate::slices::crm_drafts::service::configured_crm_provider() {
        Ok(crate::slices::crm_drafts::service::PROVIDER_HUBSPOT) => {
            let config = bos_integrations::hubspot::HubSpotWriteConfig {
                access_token: crate::env_registry::string(
                    &crate::env_registry::BOS_HUBSPOT_ACCESS_TOKEN,
                ),
                write_enabled: false,
            };
            let Some(client) = bos_integrations::hubspot::hubspot_records_search_client(&config)
            else {
                return TriValue::Unknown;
            };
            match client.find_contact(Some(email), None) {
                Ok(found) => {
                    if found.is_some() {
                        TriValue::True
                    } else {
                        TriValue::False
                    }
                }
                Err(err) => {
                    tracing::warn!(error = ?err, "hubspot contact search failed for triage rule");
                    TriValue::Unknown
                }
            }
        }
        _ => {
            let config = bos_integrations::espocrm::EspoCrmWriteConfig {
                base_url: crate::env_registry::string(&crate::env_registry::BOS_ESPOCRM_BASE_URL),
                api_key: crate::env_registry::string(&crate::env_registry::BOS_ESPOCRM_API_KEY),
                write_enabled: false,
            };
            let Some(client) = bos_integrations::espocrm::espocrm_records_search_client(&config)
            else {
                return TriValue::Unknown;
            };
            match client.find_contact(Some(email), None) {
                Ok(found) => {
                    if found.is_some() {
                        TriValue::True
                    } else {
                        TriValue::False
                    }
                }
                Err(err) => {
                    tracing::warn!(error = ?err, "espocrm contact search failed for triage rule");
                    TriValue::Unknown
                }
            }
        }
    }
}

fn crm_company_domain_exists_tri(domain: &str) -> TriValue {
    match crate::slices::crm_drafts::service::configured_crm_provider() {
        Ok(crate::slices::crm_drafts::service::PROVIDER_HUBSPOT) => {
            let config = bos_integrations::hubspot::HubSpotWriteConfig {
                access_token: crate::env_registry::string(
                    &crate::env_registry::BOS_HUBSPOT_ACCESS_TOKEN,
                ),
                write_enabled: false,
            };
            let Some(client) = bos_integrations::hubspot::hubspot_records_search_client(&config)
            else {
                return TriValue::Unknown;
            };
            match client.find_company_by_domain(domain) {
                Ok(found) => {
                    if found.is_some() {
                        TriValue::True
                    } else {
                        TriValue::False
                    }
                }
                Err(err) => {
                    tracing::warn!(error = ?err, "hubspot company domain search failed for triage rule");
                    TriValue::Unknown
                }
            }
        }
        _ => {
            let config = bos_integrations::espocrm::EspoCrmWriteConfig {
                base_url: crate::env_registry::string(&crate::env_registry::BOS_ESPOCRM_BASE_URL),
                api_key: crate::env_registry::string(&crate::env_registry::BOS_ESPOCRM_API_KEY),
                write_enabled: false,
            };
            let Some(client) = bos_integrations::espocrm::espocrm_records_search_client(&config)
            else {
                return TriValue::Unknown;
            };
            match client.find_account_by_domain(domain) {
                Ok(found) => {
                    if found.is_some() {
                        TriValue::True
                    } else {
                        TriValue::False
                    }
                }
                Err(err) => {
                    tracing::warn!(error = ?err, "espocrm account domain search failed for triage rule");
                    TriValue::Unknown
                }
            }
        }
    }
}

/// Re-run the current rules over every stored inbound message: update rows
/// whose classification changed (receipted), then run work-item emission over
/// ALL stored messages so policy changes backfill regardless of the pump's
/// poll window.
pub fn reclassify_all(
    conn: &mut rusqlite::Connection,
    client_id: &str,
    actor_id: &str,
    fallback_category: &str,
    work_queue_overlay: &crate::overlay::WorkQueueOverlay,
    now_ms: u64,
) -> Result<(u64, u64, u64), crate::store_core::StoreError> {
    reclassify_all_with_email_overlay(
        conn,
        client_id,
        actor_id,
        fallback_category,
        &crate::overlay::EmailTriageOverlay::default(),
        work_queue_overlay,
        now_ms,
    )
}

pub fn reclassify_all_with_email_overlay(
    conn: &mut rusqlite::Connection,
    client_id: &str,
    actor_id: &str,
    fallback_category: &str,
    email_triage_overlay: &crate::overlay::EmailTriageOverlay,
    work_queue_overlay: &crate::overlay::WorkQueueOverlay,
    now_ms: u64,
) -> Result<(u64, u64, u64), crate::store_core::StoreError> {
    use super::store;

    let rules: Vec<EmailTriageRule> = store::list_active(conn, client_id)?
        .into_iter()
        .map(|stored| stored.rule)
        .collect();
    let messages = store::list_all_inbound(conn, client_id)?;
    let mut examined = 0u64;
    let mut reclassified = 0u64;
    let mut emitted = 0u64;
    for message in &messages {
        examined += 1;
        let view = MessageView {
            message_id: Some(message.source_key.clone()),
            source_user_id: message.source_user_id.clone(),
            subject: message.subject.clone(),
            from: message.from_addr.clone(),
            to: message.to_addr.clone(),
            body: Some(raw_body_for_rules(message)),
            labels: message.labels.clone(),
            headers: message.headers.clone(),
        };
        run_inbound_parsers_for_stored_message(
            conn,
            client_id,
            &email_triage_overlay.inbound_parser_ids,
            message,
            &view,
            now_ms,
        )?;
        let crm = if rules_need_crm_facts(&rules) {
            crm_fact_overrides_from_cache(conn, client_id, &view, now_ms).0
        } else {
            CrmFactOverrides::default()
        };
        let mut bag = FactBag::new(
            Some(conn),
            client_id,
            &view,
            Some(&message.source_key),
            message.source_user_id.as_deref(),
            crm,
        );
        let matched_rule = resolve_rule_with_fact_bag(&rules, &mut bag);
        let new_category = matched_rule
            .map(|rule| rule.pinned_category.clone())
            .unwrap_or_else(|| fallback_category.to_string());
        let new_rule_id = matched_rule.map(|rule| rule.rule_id.clone());
        drop(bag);
        let changed = new_category != message.resolved_category
            || new_rule_id.as_deref() != message.matched_rule_id.as_deref();
        let effective = if changed {
            store::update_classification(
                conn,
                client_id,
                actor_id,
                &message.source_key,
                (
                    &message.resolved_category,
                    message.matched_rule_id.as_deref(),
                ),
                (&new_category, new_rule_id.as_deref()),
                now_ms,
            )?;
            reclassified += 1;
            let mut updated = message.clone();
            updated.resolved_category = new_category;
            updated.matched_rule_id = new_rule_id;
            updated
        } else {
            message.clone()
        };
        if crate::slices::work_queue::service::emit_for_inbound_message_with_overlay(
            conn,
            client_id,
            &effective,
            work_queue_overlay,
            now_ms,
        )? {
            emitted += 1;
        }
    }
    Ok((examined, reclassified, emitted))
}

fn run_inbound_parsers_for_stored_message(
    conn: &mut rusqlite::Connection,
    client_id: &str,
    parser_ids: &[String],
    message: &InboundMessageRecord,
    view: &MessageView,
    now_ms: u64,
) -> Result<(), crate::store_core::StoreError> {
    if parser_ids.is_empty() {
        return Ok(());
    }
    let parsers = select_inbound_parsers(parser_ids);
    if parsers.is_empty() {
        return Ok(());
    }
    let mut input = parser_input_for_message(view);
    input.source_key = message.source_key.clone();
    input.message_id = message.message_id.clone();
    for parser in parsers {
        let Some(parsed) = parser.parse(&input) else {
            continue;
        };
        super::store::upsert_inbound_enrichment(
            conn,
            super::store::InboundEnrichmentWrite {
                client_id,
                source_key: &message.source_key,
                parser_id: parser.parser_id(),
                parser_version: parser.parser_version(),
                parsed: &parsed,
                now_ms,
            },
        )?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// AI work-packet suggestions: a bounded typed Classify transform over mail
// whose resolved category policy enables AI suggestions. Deterministic category
// rules always run first and are free; this pass suggests ACTIONS (packet
// kinds) rather than forcing a category, and is quiet below the confidence
// threshold.
// ---------------------------------------------------------------------------

use bos_contracts::email_triage::{CategoryRecord, InboundMessageRecord};
use bos_contracts::work_queue::{PacketKindRecord, WorkQueuePolicy};
use bos_integrations::llm_typed_tasks::{
    TypedLlmAuthority, TypedLlmExecutionPolicy, TypedLlmExecutionRoute, TypedLlmFallbackPolicy,
    TypedLlmProviderPolicy, TypedLlmRawOutputRetention, TypedLlmRedactionPolicy,
    TypedLlmResponseFormat, TypedLlmRetryPolicy, TypedLlmSafetyPolicy, TypedLlmSourceEntity,
    TypedLlmTaskCapabilities, TypedLlmTaskClass, TypedLlmTaskInput, TypedLlmTaskRequest,
    TypedLlmTaskSpec, TypedLlmTextBlock,
};
use serde_json::json;

/// Packet kinds the AI may suggest. These still only create open work items;
/// produce + approval gates remain owned by each vertical, with ownership
/// resolved through the work_queue catalog.
const AI_SUGGESTIBLE_KINDS: &[&str] = &[
    "calendar_event_draft",
    "follow_up_task",
    "invoice_draft",
    "email_draft_reply",
    "crm_activity",
    "crm_record_create",
];

pub fn ai_suggestible_kinds() -> impl Iterator<Item = &'static str> {
    AI_SUGGESTIBLE_KINDS.iter().copied()
}

/// Owning slice for an AI-suggestible kind. AI triage may only offer/emit a
/// kind when that slice is enabled for the current client overlay.
pub fn ai_suggestible_kind_slice(kind_id: &str) -> Option<&'static str> {
    AI_SUGGESTIBLE_KINDS
        .contains(&kind_id)
        .then(|| crate::slices::work_queue::packet_kind_slice(kind_id))
        .flatten()
}

pub fn ai_suggestible_kinds_for_enabled<F>(
    kinds: &[PacketKindRecord],
    slice_enabled: F,
) -> Vec<PacketKindRecord>
where
    F: Fn(&str) -> bool,
{
    kinds
        .iter()
        .filter(|kind| {
            ai_suggestible_kind_slice(&kind.kind_id).is_some_and(&slice_enabled)
                && kind.produce_available
        })
        .cloned()
        .collect()
}

pub fn ai_suggestible_kinds_for_policy(
    enabled_kinds: &[PacketKindRecord],
    policy: Option<&WorkQueuePolicy>,
) -> Vec<PacketKindRecord> {
    let Some(policy) = policy else {
        return Vec::new();
    };
    if !policy.create_work_item {
        return Vec::new();
    }
    // Single switch: the sentinel means "any enabled kind — the AI chooses",
    // so the whole enabled candidate set is offered. Otherwise honor a specific
    // allow-list (back-compat with policies configured before the toggle).
    let allow_all = policy
        .ai_suggestible_packet_kinds
        .iter()
        .any(|allowed| allowed == bos_contracts::work_queue::AI_SUGGEST_ALL_SENTINEL);
    enabled_kinds
        .iter()
        .filter(|kind| {
            allow_all
                || policy
                    .ai_suggestible_packet_kinds
                    .iter()
                    .any(|allowed| allowed == &kind.kind_id)
        })
        .cloned()
        .collect()
}

pub fn retain_enabled_ai_suggestions<F>(suggested_packet_kinds: &mut Vec<String>, slice_enabled: F)
where
    F: Fn(&str) -> bool,
{
    suggested_packet_kinds
        .retain(|kind| ai_suggestible_kind_slice(kind).is_some_and(&slice_enabled));
}

pub const AI_TRIAGE_SCHEMA_REF: &str = "bos.email_triage.ai_triage.v1";
pub const AI_TRIAGE_PURPOSE: &str = "email_ai_triage";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiTriageSuggestion {
    pub suggested_packet_kinds: Vec<String>,
    pub suggested_category: Option<String>,
    pub confidence: AiConfidence,
    pub rationale: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AiConfidence {
    Low,
    Medium,
    High,
}

impl AiConfidence {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "high" => Some(Self::High),
            "medium" => Some(Self::Medium),
            "low" => Some(Self::Low),
            _ => None,
        }
    }
}

pub fn build_ai_triage_request(
    client_id: &str,
    message: &InboundMessageRecord,
    categories: &[CategoryRecord],
    kinds: &[PacketKindRecord],
) -> TypedLlmTaskRequest {
    let suggestible: Vec<_> = kinds
        .iter()
        .filter(|k| ai_suggestible_kind_slice(&k.kind_id).is_some())
        .map(|k| json!({ "kind_id": k.kind_id, "description": k.description }))
        .collect();
    let category_catalog: Vec<_> = categories
        .iter()
        .map(|c| json!({ "category_id": c.category_id, "description": c.description }))
        .collect();
    let body_for_ai = body_for_ai(message);
    TypedLlmTaskRequest {
        task_id: format!("ai_triage_{}", message.source_key),
        correlation_id: format!("ai_triage_{}", message.source_key),
        idempotency_key: format!("ai_triage:{}", message.source_key),
        tenant_or_project_scope: client_id.to_string(),
        source_entity: Some(TypedLlmSourceEntity {
            entity_kind: "email_inbound_message".to_string(),
            entity_id: message.source_key.clone(),
        }),
        spec: TypedLlmTaskSpec {
            task_class: TypedLlmTaskClass::Classify,
            prompt_template_id: "email_ai_triage".to_string(),
            prompt_template_version: "1".to_string(),
            prompt_template_hash: String::new(),
            schema_ref: AI_TRIAGE_SCHEMA_REF.to_string(),
            response_format: TypedLlmResponseFormat::JsonObject,
            max_input_bytes: 64 * 1024,
            max_output_bytes: 8 * 1024,
            max_tokens: 0, // filled from runtime config
            timeout_ms: 0, // filled from runtime config
            capabilities: TypedLlmTaskCapabilities::pure_transformation(),
            authority: TypedLlmAuthority::no_side_effects(),
        },
        input: TypedLlmTaskInput {
            json: json!({
                "instructions": "You are the second-tier email triage pass for a small-business operations dashboard. Deterministic rules already ran and matched nothing. Decide whether this ONE email warrants operator work. Respond with a single JSON object with EXACTLY these fields: suggested_packet_kinds (array of kind_id strings drawn ONLY from packet_kind_catalog; empty array if no work is warranted), suggested_category (a category_id from category_catalog that fits better than the current one, or null), confidence (\"high\" | \"medium\" | \"low\" — how sure you are that the suggestion is genuinely actionable for the operator), rationale (ONE sentence an operator reads to decide). Be conservative: newsletters, promotions, receipts without action, and FYI mail get an empty suggested_packet_kinds. A registration/confirmation for a specific dated event the operator attends warrants calendar_event_draft. A message requiring a later internal reminder warrants follow_up_task. A message that expects an email response warrants email_draft_reply. A message that should be logged to the CRM warrants crm_activity; if it names a new company or contact that likely needs a CRM record, include crm_record_create too.",
                "packet_kind_catalog": suggestible,
                "category_catalog": category_catalog,
                "current_category": message.resolved_category,
            }),
            text_blocks: vec![TypedLlmTextBlock {
                block_id: "email".to_string(),
                text: format!(
                    "From: {}\nTo: {}\nSubject: {}\nLabels: {}\n\n{}",
                    message.from_addr.as_deref().unwrap_or("(unknown)"),
                    message.to_addr.as_deref().unwrap_or("(unknown)"),
                    message.subject.as_deref().unwrap_or("(no subject)"),
                    message.labels.join(", "),
                    body_for_ai
                ),
            }],
        },
        execution_policy: TypedLlmExecutionPolicy {
            default_route: TypedLlmExecutionRoute::Harness, // realigned by the router
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

/// Body text used for LLM-derived classification/produce prompts. Raw
/// body_full remains the stored source of truth; this model-facing copy is
/// byte-budgeted so serialized typed-task inputs stay below provider limits.
pub fn body_for_ai(message: &InboundMessageRecord) -> String {
    body_for_ai_with_byte_limit(message, MODEL_BODY_MAX_BYTES)
}

pub fn body_for_ai_with_byte_limit(
    message: &InboundMessageRecord,
    max_body_bytes: usize,
) -> String {
    truncate_utf8_bytes(
        if message.body_full.trim().is_empty() {
            message.body_excerpt.as_str()
        } else {
            message.body_full.as_str()
        },
        max_body_bytes,
    )
}

fn truncate_utf8_bytes(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

pub fn raw_body_for_rules(message: &InboundMessageRecord) -> String {
    if message.body_full.trim().is_empty() {
        message.body_excerpt.as_str()
    } else {
        message.body_full.as_str()
    }
    .to_string()
}

/// Conservative derived-body cleanup for UI excerpts. It never mutates body_full
/// and is never used for model-facing decision paths.
pub fn display_body_for_excerpt(body: &str) -> String {
    let trimmed = strip_forwarded_and_quoted(body);
    let raw_head: String = body.chars().take(600).collect();
    if trimmed.trim().chars().count() < 40 {
        raw_head
    } else {
        trimmed
    }
}

pub fn strip_forwarded_and_quoted(body: &str) -> String {
    let without_quote = strip_trailing_quote_chain(body);
    collapse_gmail_forward_preamble(&without_quote)
}

fn strip_trailing_quote_chain(body: &str) -> String {
    let lines: Vec<&str> = body.lines().collect();
    let mut cut_at = None;
    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();
        if trimmed.starts_with("On ") && lower.contains(" wrote:") {
            cut_at = Some(idx);
            break;
        }
    }
    if let Some(idx) = cut_at {
        return lines[..idx].join("\n").trim().to_string();
    }

    let mut end = lines.len();
    while end > 0 && lines[end - 1].trim_start().starts_with('>') {
        end -= 1;
    }
    lines[..end].join("\n").trim().to_string()
}

fn collapse_gmail_forward_preamble(body: &str) -> String {
    let lines: Vec<&str> = body.lines().collect();
    let Some(marker_idx) = lines.iter().position(|line| {
        line.trim()
            .contains("---------- Forwarded message ---------")
    }) else {
        return body.trim().to_string();
    };

    let mut idx = marker_idx + 1;
    let mut from_line = None;
    let mut subject_line = None;
    while idx < lines.len() {
        let trimmed = lines[idx].trim();
        if trimmed.is_empty() {
            idx += 1;
            break;
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("from:") {
            from_line = Some(trimmed.to_string());
        } else if lower.starts_with("subject:") {
            subject_line = Some(trimmed.to_string());
        }
        idx += 1;
    }

    let mut out = lines[..marker_idx]
        .iter()
        .map(|line| line.trim_end())
        .filter(|line| !line.trim().is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if let Some(line) = from_line {
        out.push(line);
    }
    if let Some(line) = subject_line {
        out.push(line);
    }
    let forwarded_body = lines[idx..].join("\n").trim().to_string();
    if !forwarded_body.is_empty() {
        out.push(forwarded_body);
    }
    out.join("\n").trim().to_string()
}

/// Parse + domain-validate the model's response. Unknown packet kinds and
/// categories are DROPPED (never trusted); a malformed response is an error
/// the caller records as ai_triage_status = "error".
pub fn parse_ai_triage_response(
    response: &serde_json::Value,
    categories: &[CategoryRecord],
) -> Result<AiTriageSuggestion, String> {
    let confidence = response
        .get("confidence")
        .and_then(serde_json::Value::as_str)
        .and_then(AiConfidence::parse)
        .ok_or("confidence missing or invalid")?;
    let rationale = response
        .get("rationale")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .trim()
        .chars()
        .take(300)
        .collect::<String>();
    let suggested_packet_kinds: Vec<String> = response
        .get("suggested_packet_kinds")
        .and_then(serde_json::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(serde_json::Value::as_str)
                .filter(|kind| ai_suggestible_kind_slice(kind).is_some())
                .map(str::to_string)
                .collect()
        })
        .ok_or("suggested_packet_kinds missing or not an array")?;
    let suggested_category = response
        .get("suggested_category")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|raw| !raw.is_empty() && *raw != "null")
        .filter(|raw| categories.iter().any(|c| c.category_id == *raw))
        .map(str::to_string);
    Ok(AiTriageSuggestion {
        suggested_packet_kinds,
        suggested_category,
        confidence,
        rationale,
    })
}
