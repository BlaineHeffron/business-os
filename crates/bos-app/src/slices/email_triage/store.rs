//! Rule persistence. All mutations flow through store_core::mutate, so
//! revision checks, idempotency replay, and receipts come from the spine —
//! this file only owns the slice's own table.

pub use bos_contracts::email_triage::InboundMessageRecord;
use bos_contracts::email_triage::{
    validate_category_id, CategoryRecord, EmailAttachmentRecord, EmailTriageCrmDealFacetOption,
    EmailTriageDashboardCategoryOption, EmailTriageGmailCategory, EmailTriageGmailCategoryOption,
    EmailTriageInboxSettingsUpdateRequest, EmailTriageLabelOption, EmailTriageMailboxOption,
    EmailTriageRule, FALLBACK_CATEGORY_ID,
};
use bos_contracts::receipt::ActorKindDto;
use bos_contracts::work_queue::WorkQueuePolicy;
use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use crate::http::{OperatorScope, SHARED_OPERATOR_ACTOR};
use crate::store_core::{self, MutationOutcome, MutationRequest, StoreError};

pub const GMAIL_TRASH_CAPABILITY: &str = "trash_message";

pub const ENTITY_KIND: &str = "email_triage_rule";
pub const INBOUND_ENTITY_KIND: &str = "email_inbound_message";
pub const CATEGORY_ENTITY_KIND: &str = "email_triage_category";
pub const FACT_CACHE_ENTITY_KIND: &str = "email_triage_fact_cache";
pub const INBOUND_ENRICHMENT_ENTITY_KIND: &str = "email_inbound_enrichment";
pub const INBOX_SETTINGS_ENTITY_KIND: &str = "email_triage_inbox_settings";
pub const INBOX_SETTINGS_ENTITY_ID: &str = "email_triage_inbox_settings";
pub const AGENT_EVIDENCE_ENTITY_KIND: &str = "agent_evidence_file";
pub const GMAIL_INGEST_CURSOR_ENTITY_KIND: &str = "gmail_ingest_cursor";
const GMAIL_INGEST_ACTOR: &str = "gmail_ingest_pump";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GmailIngestCursor {
    pub query_hash: String,
    pub next_page_token: Option<String>,
}

pub fn gmail_ingest_query_hash(query: &str) -> String {
    short_hash(query.trim())
}

pub fn get_gmail_ingest_cursor(
    conn: &Connection,
    client_id: &str,
    account_ref: &str,
) -> Result<Option<GmailIngestCursor>, StoreError> {
    conn.query_row(
        "SELECT query_hash, next_page_token FROM gmail_ingest_cursors \
         WHERE client_id = ?1 AND account_ref = ?2",
        params![client_id, account_ref],
        |row| {
            Ok(GmailIngestCursor {
                query_hash: row.get(0)?,
                next_page_token: row.get(1)?,
            })
        },
    )
    .optional()
    .map_err(StoreError::from)
}

pub fn put_gmail_ingest_cursor(
    conn: &mut Connection,
    client_id: &str,
    account_ref: &str,
    cursor: &GmailIngestCursor,
    now_ms: u64,
) -> Result<bool, StoreError> {
    if get_gmail_ingest_cursor(conn, client_id, account_ref)?.as_ref() == Some(cursor) {
        return Ok(false);
    }
    let token_hash = cursor
        .next_page_token
        .as_deref()
        .map(short_hash)
        .unwrap_or_else(|| "complete".to_string());
    let idempotency_key = format!(
        "gmail_ingest_cursor:{}:{}:{}:{now_ms}",
        short_hash(account_ref),
        cursor.query_hash,
        token_hash
    );
    let after = serde_json::json!({
        "account_ref_hash": short_hash(account_ref),
        "query_hash": cursor.query_hash,
        "has_next_page": cursor.next_page_token.is_some(),
    })
    .to_string();
    let owned_client_id = client_id.to_string();
    let owned_account_ref = account_ref.to_string();
    let owned_cursor = cursor.clone();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: GMAIL_INGEST_CURSOR_ENTITY_KIND,
            entity_id: account_ref,
            change_kind: "advance",
            actor_id: GMAIL_INGEST_ACTOR,
            actor_kind: ActorKindDto::System,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO gmail_ingest_cursors \
                 (client_id, account_ref, query_hash, next_page_token, last_advanced_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT (client_id, account_ref) DO UPDATE SET \
                   query_hash = excluded.query_hash, \
                   next_page_token = excluded.next_page_token, \
                   last_advanced_at_ms = excluded.last_advanced_at_ms",
                params![
                    owned_client_id,
                    owned_account_ref,
                    owned_cursor.query_hash,
                    owned_cursor.next_page_token,
                    now_ms as i64,
                ],
            )?;
            Ok(())
        },
    )?;
    Ok(true)
}

/// Bound on the stored body excerpt (chars). Full bodies are persisted
/// separately for server-side AI/produce grounding.
const BODY_EXCERPT_MAX_CHARS: usize = 600;
pub const BODY_FULL_MAX_CHARS: usize = 64 * 1024;
const BODY_HTML_MAX_CHARS: usize = 64 * 1024;
const SAFE_HEADER_VALUE_MAX_CHARS: usize = 512;
const SAFE_HEADER_MAX_COUNT: usize = 32;
const SAFE_HEADER_NAMES: &[&str] = &[
    "auto-submitted",
    "cc",
    "delivered-to",
    "feedback-id",
    "in-reply-to",
    "list-id",
    "list-owner",
    "list-post",
    "list-unsubscribe",
    "message-id",
    "precedence",
    "references",
    "x-auto-response-suppress",
    "x-original-to",
    "x-mailer",
];

pub fn source_key_for(source_user_id: Option<&str>, message_id: &str) -> String {
    match source_user_id
        .map(str::trim)
        .filter(|user_id| !user_id.is_empty())
    {
        Some(user_id) => format!("gmail:user:{}:message:{message_id}", short_hash(user_id)),
        None => message_id.to_string(),
    }
}

fn normalized_source_key(record: &InboundMessageRecord) -> String {
    if record.source_key.trim().is_empty() {
        source_key_for(record.source_user_id.as_deref(), &record.message_id)
    } else {
        record.source_key.clone()
    }
}

fn sender_identity_facts_for_record(
    record: &InboundMessageRecord,
) -> Option<super::subjects::SenderIdentityFacts> {
    super::subjects::sender_identity_facts(record.from_addr.as_deref(), &record.headers)
}

pub(crate) fn safe_headers(headers: &[(String, String)]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (name, value) in headers {
        if out.len() >= SAFE_HEADER_MAX_COUNT {
            break;
        }
        let normalized_name = name.trim().to_ascii_lowercase();
        if !SAFE_HEADER_NAMES.contains(&normalized_name.as_str()) {
            continue;
        }
        let value = value
            .trim()
            .chars()
            .filter(|ch| !ch.is_control() || *ch == '\t')
            .take(SAFE_HEADER_VALUE_MAX_CHARS)
            .collect::<String>();
        if value.is_empty() {
            continue;
        }
        out.push((normalized_name, value));
    }
    out
}

fn short_hash(raw: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(raw.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[derive(Debug, Clone)]
pub struct RuleMutationContext<'a> {
    pub client_id: &'a str,
    pub actor_id: &'a str,
    pub expected_revision: Option<u64>,
    pub idempotency_key: &'a str,
    pub correlation_id: Option<&'a str>,
    pub now_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredRule {
    pub rule: EmailTriageRule,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedFact {
    pub fact_key: String,
    pub fact_json: String,
    pub source_kind: String,
    pub provider: Option<String>,
    pub fetched_at_ms: u64,
    pub expires_at_ms: u64,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InboxFilter {
    pub categories: Vec<EmailTriageGmailCategory>,
    pub dashboard_categories: Vec<String>,
    pub labels: Vec<String>,
    pub source_user_ids: Vec<Option<String>>,
    pub search: Option<String>,
    pub crm_match: Option<InboxCrmMatchFilter>,
    pub crm_deal_stages: Vec<String>,
    pub crm_deal_pipelines: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboxCrmMatchFilter {
    HasContact,
    NoMatch,
    HasDeal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxOptions {
    pub categories: Vec<EmailTriageGmailCategoryOption>,
    pub visible_gmail_categories: Vec<EmailTriageGmailCategory>,
    pub dashboard_categories: Vec<EmailTriageDashboardCategoryOption>,
    pub labels: Vec<EmailTriageLabelOption>,
    pub mailboxes: Vec<EmailTriageMailboxOption>,
    pub crm_deal_stages: Vec<EmailTriageCrmDealFacetOption>,
    pub crm_deal_pipelines: Vec<EmailTriageCrmDealFacetOption>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredInboxSettings {
    pub visible_gmail_categories: Vec<EmailTriageGmailCategory>,
    pub revision: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct FactCacheWrite<'a> {
    pub client_id: &'a str,
    pub fact_key: &'a str,
    pub fact_json: &'a str,
    pub source_kind: &'a str,
    pub provider: Option<&'a str>,
    pub fetched_at_ms: u64,
    pub expires_at_ms: u64,
    pub last_error: Option<&'a str>,
    pub idempotency_key: &'a str,
    pub now_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundEnrichment {
    pub source_key: String,
    pub parser_id: String,
    pub parsed: bos_contracts::email_identity::ParsedInbound,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone)]
pub struct InboundEnrichmentWrite<'a> {
    pub client_id: &'a str,
    pub source_key: &'a str,
    pub parser_id: &'a str,
    pub parser_version: &'a str,
    pub parsed: &'a bos_contracts::email_identity::ParsedInbound,
    pub now_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepresentedIdentity {
    pub email: String,
    pub parser_id: String,
    pub provenance: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredRepresentedIdentity {
    email: Option<String>,
    domain: Option<String>,
}

/// Active (non-deleted) rules in evaluation order: priority ASC, rule_id ASC.
pub fn list_active(conn: &Connection, client_id: &str) -> Result<Vec<StoredRule>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT r.rule_json, r.enabled, COALESCE(er.revision, 0) FROM email_triage_rules r \
         LEFT JOIN entity_revisions er \
           ON er.client_id = r.client_id AND er.entity_kind = ?2 AND er.entity_id = r.rule_id \
         WHERE r.client_id = ?1 AND r.deleted = 0 \
         ORDER BY r.priority ASC, r.rule_id ASC",
    )?;
    let rows = stmt.query_map(params![client_id, ENTITY_KIND], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, bool>(1)?,
            row.get::<_, i64>(2)? as u64,
        ))
    })?;
    let mut rules = Vec::new();
    for row in rows {
        let (json, enabled, revision) = row?;
        let mut rule: EmailTriageRule = serde_json::from_str(&json)
            .map_err(|err| StoreError::Domain(format!("stored rule corrupt: {err}")))?;
        // The enabled column is authoritative (enable/disable actions update it
        // without rewriting rule_json).
        rule.enabled = enabled;
        rules.push(StoredRule { rule, revision });
    }
    Ok(rules)
}

pub fn cleanup_legacy_rule_json(conn: &mut Connection, now_ms: u64) -> Result<usize, StoreError> {
    let rows = legacy_cleanup_candidates(conn)?;
    let mut changed = 0;
    for (client_id, rule_id, before_json) in rows {
        let mut rule: EmailTriageRule = serde_json::from_str(&before_json)
            .map_err(|err| StoreError::Domain(format!("stored rule corrupt: {err}")))?;
        if rule.conditions.is_empty() {
            continue;
        }
        rule.conditions_v2 = super::legacy::effective_conditions(&rule);
        rule.conditions.clear();
        rule.validate()
            .map_err(|err| StoreError::Domain(err.code().to_string()))?;
        let after_json = serde_json::to_string(&rule)
            .map_err(|err| StoreError::Domain(format!("serialize rule: {err}")))?;
        if before_json == after_json {
            continue;
        }
        let write_json = after_json.clone();
        let write_client_id = client_id.clone();
        let write_rule_id = rule_id.clone();
        let idempotency_key =
            format!("email-triage-rule-legacy-cleanup-0062:{client_id}:{rule_id}");
        store_core::mutate(
            conn,
            MutationRequest {
                client_id: &client_id,
                entity_kind: ENTITY_KIND,
                entity_id: &rule_id,
                change_kind: "legacy_cleanup",
                actor_id: "email_triage_legacy_cleanup",
                actor_kind: ActorKindDto::System,
                expected_revision: None,
                idempotency_key: &idempotency_key,
                correlation_id: None,
                causation_id: None,
                before_json: Some(before_json),
                after_json: Some(after_json),
                now_ms,
            },
            move |tx| {
                tx.execute(
                    "UPDATE email_triage_rules SET rule_json = ?3, updated_at_ms = ?4 \
                     WHERE client_id = ?1 AND rule_id = ?2",
                    params![write_client_id, write_rule_id, write_json, now_ms as i64],
                )?;
                Ok(())
            },
        )?;
        changed += 1;
    }
    Ok(changed)
}

fn legacy_cleanup_candidates(
    conn: &Connection,
) -> Result<Vec<(String, String, String)>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT client_id, rule_id, rule_json FROM email_triage_rules \
         WHERE json_array_length(json_extract(rule_json, '$.conditions')) > 0 \
         ORDER BY client_id, rule_id",
    )?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn read_cached_fact(
    conn: &Connection,
    client_id: &str,
    fact_key: &str,
) -> Result<Option<CachedFact>, StoreError> {
    use rusqlite::OptionalExtension;
    conn.query_row(
        "SELECT fact_key, fact_json, source_kind, provider, fetched_at_ms, expires_at_ms, last_error \
         FROM email_triage_fact_cache WHERE client_id = ?1 AND fact_key = ?2",
        params![client_id, fact_key],
        |row| {
            Ok(CachedFact {
                fact_key: row.get(0)?,
                fact_json: row.get(1)?,
                source_kind: row.get(2)?,
                provider: row.get(3)?,
                fetched_at_ms: row.get::<_, i64>(4)? as u64,
                expires_at_ms: row.get::<_, i64>(5)? as u64,
                last_error: row.get(6)?,
            })
        },
    )
    .optional()
    .map_err(StoreError::from)
}

pub fn upsert_cached_fact(
    conn: &mut Connection,
    write: FactCacheWrite<'_>,
) -> Result<MutationOutcome, StoreError> {
    let before = current_fact_json(conn, write.client_id, write.fact_key)?;
    let after = serde_json::json!({
        "fact_key": write.fact_key,
        "fact_json": write.fact_json,
        "source_kind": write.source_kind,
        "provider": write.provider,
        "fetched_at_ms": write.fetched_at_ms,
        "expires_at_ms": write.expires_at_ms,
        "last_error": write.last_error,
    })
    .to_string();
    let fact_key = write.fact_key.to_string();
    let fact_json = write.fact_json.to_string();
    let source_kind = write.source_kind.to_string();
    let provider = write.provider.map(str::to_string);
    let last_error = write.last_error.map(str::to_string);
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: write.client_id,
            entity_kind: FACT_CACHE_ENTITY_KIND,
            entity_id: write.fact_key,
            change_kind: "upsert",
            actor_id: "email_triage_fact_resolver",
            actor_kind: ActorKindDto::System,
            expected_revision: None,
            idempotency_key: write.idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: before,
            after_json: Some(after),
            now_ms: write.now_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO email_triage_fact_cache \
                 (client_id, fact_key, fact_json, source_kind, provider, fetched_at_ms, expires_at_ms, last_error) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
                 ON CONFLICT (client_id, fact_key) DO UPDATE SET \
                   fact_json = excluded.fact_json, source_kind = excluded.source_kind, \
                   provider = excluded.provider, fetched_at_ms = excluded.fetched_at_ms, \
                   expires_at_ms = excluded.expires_at_ms, last_error = excluded.last_error",
                params![
                    write.client_id,
                    fact_key,
                    fact_json,
                    source_kind,
                    provider,
                    write.fetched_at_ms as i64,
                    write.expires_at_ms as i64,
                    last_error,
                ],
            )?;
            Ok(())
        },
    )
}

fn current_fact_json(
    conn: &Connection,
    client_id: &str,
    fact_key: &str,
) -> Result<Option<String>, StoreError> {
    use rusqlite::OptionalExtension;
    conn.query_row(
        "SELECT json_object( \
           'fact_key', fact_key, \
           'fact_json', fact_json, \
           'source_kind', source_kind, \
           'provider', provider, \
           'fetched_at_ms', fetched_at_ms, \
           'expires_at_ms', expires_at_ms, \
           'last_error', last_error) \
         FROM email_triage_fact_cache WHERE client_id = ?1 AND fact_key = ?2",
        params![client_id, fact_key],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(StoreError::from)
}

pub fn upsert(
    conn: &mut Connection,
    ctx: RuleMutationContext<'_>,
    rule: &EmailTriageRule,
) -> Result<MutationOutcome, StoreError> {
    rule.validate()
        .map_err(|err| StoreError::Domain(err.code().to_string()))?;
    if !category_exists(conn, ctx.client_id, &rule.pinned_category)? {
        return Err(StoreError::Domain(
            "email_triage_category_unknown".to_string(),
        ));
    }
    let before = current_rule_json(conn, ctx.client_id, &rule.rule_id)?;
    let after = serde_json::to_string(rule)
        .map_err(|err| StoreError::Domain(format!("serialize rule: {err}")))?;
    let after_for_write = after.clone();
    let owned_rule_id = rule.rule_id.clone();
    let priority = rule.priority;
    let enabled = rule.enabled;
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: ENTITY_KIND,
            entity_id: &rule.rule_id,
            change_kind: "upsert",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: ctx.correlation_id,
            causation_id: None,
            before_json: before,
            after_json: Some(after),
            now_ms: ctx.now_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO email_triage_rules \
                 (client_id, rule_id, rule_json, priority, enabled, deleted, updated_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6) \
                 ON CONFLICT (client_id, rule_id) DO UPDATE SET \
                   rule_json = excluded.rule_json, priority = excluded.priority, \
                   enabled = excluded.enabled, deleted = 0, \
                   updated_at_ms = excluded.updated_at_ms",
                params![
                    ctx.client_id,
                    owned_rule_id,
                    after_for_write,
                    priority,
                    enabled,
                    ctx.now_ms as i64
                ],
            )?;
            Ok(())
        },
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleAction {
    Enable,
    Disable,
    Delete,
}

impl RuleAction {
    pub fn change_kind(self) -> &'static str {
        match self {
            Self::Enable => "enable",
            Self::Disable => "disable",
            Self::Delete => "delete",
        }
    }
}

pub fn apply_action(
    conn: &mut Connection,
    ctx: RuleMutationContext<'_>,
    rule_id: &str,
    action: RuleAction,
) -> Result<MutationOutcome, StoreError> {
    let before = current_rule_json(conn, ctx.client_id, rule_id)?;
    if before.is_none() {
        return Err(StoreError::Domain(
            "email_triage_rule_not_found".to_string(),
        ));
    }
    let owned_rule_id = rule_id.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: ENTITY_KIND,
            entity_id: rule_id,
            change_kind: action.change_kind(),
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: ctx.correlation_id,
            causation_id: None,
            before_json: before,
            after_json: None,
            now_ms: ctx.now_ms,
        },
        move |tx| {
            let set_clause = match action {
                RuleAction::Enable => "enabled = 1",
                RuleAction::Disable => "enabled = 0",
                RuleAction::Delete => "deleted = 1",
            };
            tx.execute(
                &format!(
                    "UPDATE email_triage_rules SET {set_clause}, updated_at_ms = ?3 \
                     WHERE client_id = ?1 AND rule_id = ?2"
                ),
                params![ctx.client_id, owned_rule_id, ctx.now_ms as i64],
            )?;
            Ok(())
        },
    )
}

fn current_rule_json(
    conn: &Connection,
    client_id: &str,
    rule_id: &str,
) -> Result<Option<String>, StoreError> {
    use rusqlite::OptionalExtension;
    let json = conn
        .query_row(
            "SELECT rule_json FROM email_triage_rules \
             WHERE client_id = ?1 AND rule_id = ?2 AND deleted = 0",
            params![client_id, rule_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    Ok(json)
}

/// Source keys (from the candidate set) already ingested for this client —
/// the pump filters these out BEFORE mutating, so re-polls stay receipt-quiet.
pub fn existing_source_keys(
    conn: &Connection,
    client_id: &str,
    candidates: &[String],
) -> Result<std::collections::HashSet<String>, StoreError> {
    let mut existing = std::collections::HashSet::new();
    let mut stmt = conn
        .prepare("SELECT 1 FROM email_inbound_messages WHERE client_id = ?1 AND source_key = ?2")?;
    for source_key in candidates {
        if stmt.exists(params![client_id, source_key])? {
            existing.insert(source_key.clone());
        }
    }
    Ok(existing)
}

/// Candidate source keys mapped to the stored source key already representing
/// the same mailbox message. The legacy `(message_id, source_user_id)` arm keeps
/// rows migrated from the pre-source-key schema receipt-quiet on the first
/// post-upgrade poll.
pub fn existing_source_key_matches(
    conn: &Connection,
    client_id: &str,
    candidates: &[(String, String, Option<String>)],
) -> Result<std::collections::HashMap<String, String>, StoreError> {
    let mut existing = std::collections::HashMap::new();
    let mut stmt = conn.prepare(
        "SELECT source_key FROM email_inbound_messages \
         WHERE client_id = ?1 \
           AND (source_key = ?2 \
                OR (message_id = ?3 AND ((?4 IS NULL AND source_user_id IS NULL) \
                                         OR source_user_id = ?4))) \
         ORDER BY CASE WHEN source_key = ?2 THEN 0 ELSE 1 END, source_key ASC \
         LIMIT 1",
    )?;
    for (candidate_source_key, message_id, source_user_id) in candidates {
        let stored = stmt
            .query_row(
                params![client_id, candidate_source_key, message_id, source_user_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if let Some(stored_source_key) = stored {
            existing.insert(candidate_source_key.clone(), stored_source_key);
        }
    }
    Ok(existing)
}

/// Persist one classified inbound message through the receipt spine
/// (actor = system, idempotency key derived from the mailbox-scoped source key).
pub fn record_inbound_message(
    conn: &mut Connection,
    client_id: &str,
    record: &InboundMessageRecord,
) -> Result<MutationOutcome, StoreError> {
    record_inbound_message_with_body_html(conn, client_id, record, None)
}

pub fn record_inbound_message_with_body_html(
    conn: &mut Connection,
    client_id: &str,
    record: &InboundMessageRecord,
    body_html: Option<&str>,
) -> Result<MutationOutcome, StoreError> {
    let source_key = normalized_source_key(record);
    let idempotency_key = format!("ingest:{source_key}");
    let headers = safe_headers(&record.headers);
    let mut after_record = record.clone();
    after_record.source_key = source_key.clone();
    after_record.headers = headers.clone();
    let after = serde_json::to_string(&after_record)
        .map_err(|err| StoreError::Domain(format!("serialize inbound message: {err}")))?;
    let labels_json = serde_json::to_string(&record.labels)
        .map_err(|err| StoreError::Domain(format!("serialize labels: {err}")))?;
    let attachments_json = serde_json::to_string(&record.attachments)
        .map_err(|err| StoreError::Domain(format!("serialize attachments: {err}")))?;
    let headers_json = serde_json::to_string(&headers)
        .map_err(|err| StoreError::Domain(format!("serialize headers: {err}")))?;
    let mut row = record.clone();
    row.headers = headers;
    let sender_identity = sender_identity_facts_for_record(&row);
    let sender_email = sender_identity.as_ref().map(|facts| facts.email.clone());
    let sender_local_part = sender_identity
        .as_ref()
        .map(|facts| facts.local_part.clone());
    let sender_domain = sender_identity.as_ref().map(|facts| facts.domain.clone());
    let sender_automation_local_part = sender_identity
        .as_ref()
        .is_some_and(|facts| facts.automation_local_part);
    let sender_header_identity_blocked = sender_identity
        .as_ref()
        .is_some_and(|facts| facts.header_block_reason.is_some());
    let sender_identity_block_reason = sender_identity
        .as_ref()
        .and_then(|facts| facts.header_block_reason)
        .map(str::to_string);
    let source_key_for_write = source_key.clone();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: INBOUND_ENTITY_KIND,
            entity_id: &source_key,
            change_kind: "ingest",
            actor_id: "gmail_ingest_pump",
            actor_kind: ActorKindDto::System,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms: record.ingested_at_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO email_inbound_messages \
                 (client_id, source_key, message_id, thread_id, internal_date_ms, from_addr, to_addr, \
                  sender_email, sender_local_part, sender_domain, sender_automation_local_part, \
                  sender_header_identity_blocked, sender_identity_block_reason, subject, \
                  body_excerpt, body_full, body_html, labels_json, headers_json, attachments_json, \
                  resolved_category, matched_rule_id, ingested_at_ms, source_user_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24) \
                 ON CONFLICT (client_id, source_key) DO NOTHING",
                params![
                    client_id,
                    source_key_for_write,
                    row.message_id,
                    row.thread_id,
                    row.internal_date_ms,
                    row.from_addr,
                    row.to_addr,
                    sender_email,
                    sender_local_part,
                    sender_domain,
                    sender_automation_local_part,
                    sender_header_identity_blocked,
                    sender_identity_block_reason,
                    row.subject,
                    truncate_chars(&row.body_excerpt, BODY_EXCERPT_MAX_CHARS),
                    truncate_chars(&row.body_full, BODY_FULL_MAX_CHARS),
                    body_html
                        .map(|value| truncate_chars(value, BODY_HTML_MAX_CHARS))
                        .unwrap_or_default(),
                    labels_json,
                    headers_json,
                    attachments_json,
                    row.resolved_category,
                    row.matched_rule_id,
                    row.ingested_at_ms as i64,
                    row.source_user_id
                ],
            )?;
            Ok(())
        },
    )
}

#[derive(Debug, Clone)]
pub struct EmailBodyCompactionBatch<'a> {
    pub client_id: &'a str,
    pub actor_id: &'a str,
    pub actor_kind: ActorKindDto,
    pub cutoff_ms: u64,
    pub source_keys: &'a [String],
    pub mutation_entity_kind: &'a str,
    pub mutation_change_kind: &'a str,
    pub entity_id: &'a str,
    pub idempotency_key: &'a str,
    pub correlation_id: Option<&'a str>,
    pub causation_id: Option<&'a str>,
    pub now_ms: u64,
}

pub fn email_body_compaction_candidates(
    conn: &Connection,
    client_id: &str,
    cutoff_ms: u64,
    limit: usize,
) -> Result<Vec<String>, StoreError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT source_key FROM email_inbound_messages \
         WHERE client_id = ?1 AND ingested_at_ms < ?2 \
           AND (body_full <> '' OR body_html <> '') \
         ORDER BY ingested_at_ms ASC, source_key ASC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![client_id, cutoff_ms as i64, limit as i64], |row| {
        row.get::<_, String>(0)
    })?;
    let mut source_keys = Vec::new();
    for row in rows {
        source_keys.push(row?);
    }
    Ok(source_keys)
}

pub fn eligible_email_body_count(
    conn: &Connection,
    client_id: &str,
    cutoff_ms: u64,
) -> Result<u64, StoreError> {
    conn.query_row(
        "SELECT COUNT(*) FROM email_inbound_messages \
         WHERE client_id = ?1 AND ingested_at_ms < ?2 \
           AND (body_full <> '' OR body_html <> '')",
        params![client_id, cutoff_ms as i64],
        |row| row.get::<_, i64>(0),
    )
    .map(|count| count as u64)
    .map_err(Into::into)
}

/// Clear full email bodies from one exact batch. The source row, bounded
/// excerpt, headers, labels, attachment metadata, and every reference remain.
pub fn compact_email_bodies(
    conn: &mut Connection,
    batch: EmailBodyCompactionBatch<'_>,
) -> Result<MutationOutcome, StoreError> {
    if batch.source_keys.is_empty() {
        return Err(StoreError::Domain(
            "email_body_compaction_batch_empty".to_string(),
        ));
    }
    let first_source_key = batch.source_keys.first().cloned().unwrap_or_default();
    let last_source_key = batch.source_keys.last().cloned().unwrap_or_default();
    let after_json = serde_json::json!({
        "operation": "email_body_compaction",
        "cutoff_ms": batch.cutoff_ms,
        "rows_compacted": batch.source_keys.len(),
        "first_source_key": first_source_key,
        "last_source_key": last_source_key,
    })
    .to_string();
    let placeholders = vec!["?"; batch.source_keys.len()].join(", ");
    let sql = format!(
        "UPDATE email_inbound_messages SET body_full = '', body_html = '' \
         WHERE client_id = ? AND ingested_at_ms < ? \
           AND (body_full <> '' OR body_html <> '') \
           AND source_key IN ({placeholders})"
    );
    let expected_rows = batch.source_keys.len();
    let client_id = batch.client_id.to_string();
    let cutoff_ms = batch.cutoff_ms;
    let source_keys = batch.source_keys.to_vec();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: batch.client_id,
            entity_kind: batch.mutation_entity_kind,
            entity_id: batch.entity_id,
            change_kind: batch.mutation_change_kind,
            actor_id: batch.actor_id,
            actor_kind: batch.actor_kind,
            expected_revision: None,
            idempotency_key: batch.idempotency_key,
            correlation_id: batch.correlation_id,
            causation_id: batch.causation_id,
            before_json: None,
            after_json: Some(after_json),
            now_ms: batch.now_ms,
        },
        move |tx| {
            let mut values = Vec::with_capacity(source_keys.len() + 2);
            values.push(Value::Text(client_id));
            values.push(Value::Integer(cutoff_ms as i64));
            values.extend(source_keys.into_iter().map(Value::Text));
            let changed = tx.execute(&sql, params_from_iter(values.iter()))?;
            if changed != expected_rows {
                return Err(StoreError::Domain(format!(
                    "email_body_compaction_race:expected={expected_rows}:changed={changed}"
                )));
            }
            Ok(())
        },
    )
}

pub fn inbound_body_html(
    conn: &Connection,
    client_id: &str,
    source_key: &str,
) -> Result<Option<String>, StoreError> {
    let html = conn
        .query_row(
            "SELECT body_html FROM email_inbound_messages \
             WHERE client_id = ?1 AND source_key = ?2",
            params![client_id, source_key],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(StoreError::from)?;
    Ok(html
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty()))
}

pub fn upsert_inbound_enrichment(
    conn: &mut Connection,
    write: InboundEnrichmentWrite<'_>,
) -> Result<MutationOutcome, StoreError> {
    let parsed_json = serde_json::to_string(write.parsed)
        .map_err(|err| StoreError::Domain(format!("serialize inbound enrichment: {err}")))?;
    let result_hash = short_hash(&parsed_json);
    let entity_id = inbound_enrichment_entity_id(write.source_key, write.parser_id);
    let idempotency_key = format!(
        "email-enrichment:{}:{}:{}:{}",
        write.source_key, write.parser_id, write.parser_version, result_hash
    );
    let before_json =
        existing_inbound_enrichment_json(conn, write.client_id, write.source_key, write.parser_id)?;
    let after_json = serde_json::json!({
        "source_key": write.source_key,
        "parser_id": write.parser_id,
        "parser_version": write.parser_version,
        "parsed": write.parsed,
    })
    .to_string();
    let owned_client = write.client_id.to_string();
    let owned_source_key = write.source_key.to_string();
    let owned_parser_id = write.parser_id.to_string();
    let owned_parsed_json = parsed_json;
    let now_ms = write.now_ms;
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: write.client_id,
            entity_kind: INBOUND_ENRICHMENT_ENTITY_KIND,
            entity_id: &entity_id,
            change_kind: "upsert",
            actor_id: "email_inbound_parser",
            actor_kind: ActorKindDto::System,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            correlation_id: None,
            causation_id: Some(write.source_key),
            before_json,
            after_json: Some(after_json),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO email_inbound_enrichments \
                 (client_id, source_key, parser_id, parsed_json, created_at_ms, updated_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5) \
                 ON CONFLICT (client_id, source_key, parser_id) DO UPDATE SET \
                   parsed_json = excluded.parsed_json, updated_at_ms = excluded.updated_at_ms",
                params![
                    owned_client,
                    owned_source_key,
                    owned_parser_id,
                    owned_parsed_json,
                    now_ms as i64
                ],
            )?;
            Ok(())
        },
    )
}

fn existing_inbound_enrichment_json(
    conn: &Connection,
    client_id: &str,
    source_key: &str,
    parser_id: &str,
) -> Result<Option<String>, StoreError> {
    let existing = conn
        .query_row(
            "SELECT parsed_json FROM email_inbound_enrichments \
             WHERE client_id = ?1 AND source_key = ?2 AND parser_id = ?3",
            params![client_id, source_key, parser_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    Ok(existing.map(|parsed| {
        serde_json::json!({
            "source_key": source_key,
            "parser_id": parser_id,
            "parsed": serde_json::from_str::<serde_json::Value>(&parsed)
                .unwrap_or(serde_json::Value::Null),
        })
        .to_string()
    }))
}

pub fn list_inbound_enrichments(
    conn: &Connection,
    client_id: &str,
    source_key: &str,
) -> Result<Vec<InboundEnrichment>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT source_key, parser_id, parsed_json, created_at_ms, updated_at_ms \
         FROM email_inbound_enrichments \
         WHERE client_id = ?1 AND source_key = ?2 \
         ORDER BY parser_id ASC",
    )?;
    let rows = stmt.query_map(params![client_id, source_key], |row| {
        let parsed_json: String = row.get("parsed_json")?;
        Ok(InboundEnrichment {
            source_key: row.get("source_key")?,
            parser_id: row.get("parser_id")?,
            parsed: serde_json::from_str(&parsed_json).unwrap_or_default(),
            created_at_ms: row.get::<_, i64>("created_at_ms")? as u64,
            updated_at_ms: row.get::<_, i64>("updated_at_ms")? as u64,
        })
    })?;
    let mut enrichments = Vec::new();
    for row in rows {
        enrichments.push(row?);
    }
    Ok(enrichments)
}

pub fn best_represented_identity(
    conn: &Connection,
    client_id: &str,
    source_key: &str,
) -> Result<Option<RepresentedIdentity>, StoreError> {
    for enrichment in list_inbound_enrichments(conn, client_id, source_key)? {
        for party in enrichment.parsed.represented_parties {
            let Some(email) = super::subjects::first_normalized_email(party.email.as_deref())
            else {
                continue;
            };
            return Ok(Some(RepresentedIdentity {
                email,
                parser_id: enrichment.parser_id,
                provenance: party.provenance,
            }));
        }
    }
    Ok(None)
}

fn represented_identity_domain(email: &str) -> Option<String> {
    email
        .split_once('@')
        .map(|(_, domain)| domain.trim().to_ascii_lowercase())
        .filter(|domain| !domain.is_empty())
}

fn represented_identity_json(identity: &StoredRepresentedIdentity) -> String {
    serde_json::json!({
        "represented_email": identity.email,
        "represented_domain": identity.domain,
    })
    .to_string()
}

fn current_represented_identity(
    conn: &Connection,
    client_id: &str,
    source_key: &str,
) -> Result<StoredRepresentedIdentity, StoreError> {
    conn.query_row(
        "SELECT represented_email, represented_domain \
         FROM email_inbound_messages \
         WHERE client_id = ?1 AND source_key = ?2",
        params![client_id, source_key],
        |row| {
            Ok(StoredRepresentedIdentity {
                email: row.get("represented_email")?,
                domain: row.get("represented_domain")?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| StoreError::Domain("email_inbound_message_not_found".to_string()))
}

pub fn refresh_represented_identity(
    conn: &mut Connection,
    client_id: &str,
    source_key: &str,
    now_ms: u64,
) -> Result<Option<MutationOutcome>, StoreError> {
    let before = current_represented_identity(conn, client_id, source_key)?;
    let resolved_email = best_represented_identity(conn, client_id, source_key)?.map(|identity| {
        super::subjects::first_normalized_email(Some(&identity.email)).unwrap_or(identity.email)
    });
    let after = StoredRepresentedIdentity {
        domain: resolved_email
            .as_deref()
            .and_then(represented_identity_domain),
        email: resolved_email.clone(),
    };
    let before_json = represented_identity_json(&before);
    let after_json = represented_identity_json(&after);
    if before == after {
        return Ok(None);
    }
    let idempotency_key = format!(
        "represented-identity:{source_key}:{now_ms}:{}",
        resolved_email.as_deref().unwrap_or("none")
    );
    let owned_client = client_id.to_string();
    let owned_source_key = source_key.to_string();
    let owned_email = after.email.clone();
    let owned_domain = after.domain.clone();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: INBOUND_ENTITY_KIND,
            entity_id: source_key,
            change_kind: "represented_identity",
            actor_id: "email_inbound_parser",
            actor_kind: ActorKindDto::System,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            correlation_id: None,
            causation_id: Some(source_key),
            before_json: Some(before_json),
            after_json: Some(after_json),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE email_inbound_messages \
                 SET represented_email = ?3, represented_domain = ?4 \
                 WHERE client_id = ?1 AND source_key = ?2",
                params![owned_client, owned_source_key, owned_email, owned_domain],
            )?;
            Ok(())
        },
    )
    .map(Some)
}

pub fn strongest_attention_signal(
    conn: &Connection,
    client_id: &str,
    source_key: &str,
) -> Result<Option<bos_contracts::email_identity::AttentionSignal>, StoreError> {
    let mut selected: Option<bos_contracts::email_identity::AttentionSignal> = None;
    for enrichment in list_inbound_enrichments(conn, client_id, source_key)? {
        for signal in enrichment.parsed.attention_signals {
            selected = match selected {
                None => Some(signal),
                Some(existing) => Some(stronger_attention(existing, signal)),
            };
        }
    }
    Ok(selected)
}

pub fn enrichment_display_hints(
    conn: &Connection,
    client_id: &str,
    source_key: &str,
) -> Result<(Option<String>, Option<String>), StoreError> {
    let mut title = None;
    let mut summary = None;
    for enrichment in list_inbound_enrichments(conn, client_id, source_key)? {
        if title.is_none() {
            title = trimmed_nonempty(enrichment.parsed.title_hint.as_deref()).map(str::to_string);
        }
        if summary.is_none() {
            summary =
                trimmed_nonempty(enrichment.parsed.summary_hint.as_deref()).map(str::to_string);
        }
        if title.is_some() && summary.is_some() {
            break;
        }
    }
    Ok((title, summary))
}

fn stronger_attention(
    left: bos_contracts::email_identity::AttentionSignal,
    right: bos_contracts::email_identity::AttentionSignal,
) -> bos_contracts::email_identity::AttentionSignal {
    use bos_contracts::email_identity::AttentionLevel;
    let rank = |level: AttentionLevel| match level {
        AttentionLevel::Lower => 0,
        AttentionLevel::Normal => 1,
        AttentionLevel::Higher => 2,
    };
    if rank(right.level) > rank(left.level) {
        right
    } else {
        left
    }
}

fn trimmed_nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn inbound_enrichment_entity_id(source_key: &str, parser_id: &str) -> String {
    format!("{source_key}:{parser_id}")
}

pub fn update_inbound_attachments(
    conn: &mut Connection,
    client_id: &str,
    source_key: &str,
    attachments: &[EmailAttachmentRecord],
    idempotency_key: &str,
    now_ms: u64,
) -> Result<MutationOutcome, StoreError> {
    let attachments_json = serde_json::to_string(attachments)
        .map_err(|err| StoreError::Domain(format!("serialize attachments: {err}")))?;
    let after_json = serde_json::json!({
        "source_key": source_key,
        "attachments": attachments,
    })
    .to_string();
    let owned_client = client_id.to_string();
    let owned_source_key = source_key.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: INBOUND_ENTITY_KIND,
            entity_id: source_key,
            change_kind: "update_attachments",
            actor_id: "gmail_ingest_pump",
            actor_kind: ActorKindDto::System,
            expected_revision: None,
            idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(after_json),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE email_inbound_messages SET attachments_json = ?3 \
                 WHERE client_id = ?1 AND source_key = ?2",
                params![owned_client, owned_source_key, attachments_json],
            )?;
            Ok(())
        },
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentEvidenceFile {
    pub evidence_id: String,
    pub session_id: String,
    pub item_id: Option<String>,
    pub source_ref: String,
    pub attachment_id: String,
    pub path: String,
    pub filename: String,
    pub mime_type: Option<String>,
    pub size_bytes: u64,
    pub retention_until_ms: u64,
}

pub struct AgentEvidenceWrite<'a> {
    pub client_id: &'a str,
    pub actor_id: &'a str,
    pub evidence: &'a AgentEvidenceFile,
    pub idempotency_key: &'a str,
    pub now_ms: u64,
}

pub fn record_agent_evidence_file(
    conn: &mut Connection,
    write: AgentEvidenceWrite<'_>,
) -> Result<MutationOutcome, StoreError> {
    let after_json = serde_json::json!({
        "evidence_id": write.evidence.evidence_id,
        "session_id": write.evidence.session_id,
        "item_id": write.evidence.item_id,
        "source_kind": "email",
        "source_ref": write.evidence.source_ref,
        "attachment_id": write.evidence.attachment_id,
        "path": write.evidence.path,
        "filename": write.evidence.filename,
        "mime_type": write.evidence.mime_type,
        "size_bytes": write.evidence.size_bytes,
        "retention_until_ms": write.evidence.retention_until_ms,
    })
    .to_string();
    let owned_client = write.client_id.to_string();
    let evidence = write.evidence.clone();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: write.client_id,
            entity_kind: AGENT_EVIDENCE_ENTITY_KIND,
            entity_id: &write.evidence.evidence_id,
            change_kind: "stage",
            actor_id: write.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: None,
            idempotency_key: write.idempotency_key,
            correlation_id: Some(&write.evidence.session_id),
            causation_id: None,
            before_json: None,
            after_json: Some(after_json),
            now_ms: write.now_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO agent_evidence_files \
                 (client_id, evidence_id, session_id, item_id, source_kind, source_ref, \
                  attachment_id, path, filename, mime_type, size_bytes, created_at_ms, \
                  last_used_at_ms, retention_until_ms, deleted_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, 'email', ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11, ?12, NULL) \
                 ON CONFLICT (client_id, evidence_id) DO UPDATE SET \
                   last_used_at_ms = excluded.last_used_at_ms, \
                   retention_until_ms = MAX(agent_evidence_files.retention_until_ms, excluded.retention_until_ms), \
                   deleted_at_ms = NULL",
                params![
                    owned_client,
                    evidence.evidence_id,
                    evidence.session_id,
                    evidence.item_id,
                    evidence.source_ref,
                    evidence.attachment_id,
                    evidence.path,
                    evidence.filename,
                    evidence.mime_type,
                    evidence.size_bytes as i64,
                    write.now_ms as i64,
                    evidence.retention_until_ms as i64,
                ],
            )?;
            Ok(())
        },
    )
}

pub fn due_agent_evidence_files(
    conn: &Connection,
    client_id: &str,
    now_ms: u64,
    limit: usize,
) -> Result<Vec<(String, String)>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT evidence_id, path FROM agent_evidence_files \
         WHERE client_id = ?1 AND deleted_at_ms IS NULL AND retention_until_ms <= ?2 \
         ORDER BY retention_until_ms ASC, evidence_id ASC LIMIT ?3",
    )?;
    let rows = stmt.query_map(
        params![client_id, now_ms as i64, limit.max(1) as i64],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let mut files = Vec::new();
    for row in rows {
        files.push(row?);
    }
    Ok(files)
}

pub fn agent_evidence_paths_for_session(
    conn: &Connection,
    client_id: &str,
    session_id: &str,
) -> Result<Vec<String>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT path FROM agent_evidence_files \
         WHERE client_id = ?1 AND session_id = ?2 AND deleted_at_ms IS NULL \
         ORDER BY created_at_ms ASC, evidence_id ASC",
    )?;
    let rows = stmt.query_map(params![client_id, session_id], |row| row.get(0))?;
    let mut paths = Vec::new();
    for row in rows {
        paths.push(row?);
    }
    Ok(paths)
}

pub fn mark_agent_evidence_deleted(
    conn: &mut Connection,
    client_id: &str,
    evidence_id: &str,
    path: &str,
    now_ms: u64,
) -> Result<MutationOutcome, StoreError> {
    let after_json = serde_json::json!({
        "evidence_id": evidence_id,
        "path": path,
        "deleted_at_ms": now_ms,
    })
    .to_string();
    let owned_client = client_id.to_string();
    let owned_id = evidence_id.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: AGENT_EVIDENCE_ENTITY_KIND,
            entity_id: evidence_id,
            change_kind: "delete_expired",
            actor_id: "agent_evidence_cleanup",
            actor_kind: ActorKindDto::System,
            expected_revision: None,
            idempotency_key: &format!("agent-evidence-delete:{evidence_id}:{now_ms}"),
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(after_json),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE agent_evidence_files SET deleted_at_ms = ?3 \
                 WHERE client_id = ?1 AND evidence_id = ?2 AND deleted_at_ms IS NULL",
                params![owned_client, owned_id, now_ms as i64],
            )?;
            Ok(())
        },
    )
}

/// Rewrite stored raw Gmail label IDS ("Label_13") to display names using
/// the freshly fetched label map — the one-time correction for mail ingested
/// before names were resolved at the source. Unmapped ids stay as-is;
/// unchanged rows write nothing; changed rows mutate receipted with a
/// content-derived idempotency key so re-runs replay quietly.
pub fn relabel_inbound_messages(
    conn: &mut Connection,
    client_id: &str,
    source_user_id: Option<&str>,
    label_names: &std::collections::HashMap<String, String>,
    now_ms: u64,
) -> Result<usize, StoreError> {
    if label_names.is_empty() {
        return Ok(0);
    }
    let candidates: Vec<(String, String)> = {
        // Label ids are per Gmail account, so only this account's rows are
        // candidates. NULL rows (pre-multi-user / env-credential) are
        // included: they predate a second account, so the map is theirs.
        let mut stmt = conn.prepare(
            // '_' is a LIKE wildcard; overmatching is fine — unchanged rows
            // no-op below.
            "SELECT source_key, labels_json FROM email_inbound_messages \
             WHERE client_id = ?1 AND labels_json LIKE '%Label_%' \
               AND (source_user_id IS NULL OR source_user_id = ?2)",
        )?;
        let rows = stmt.query_map(params![client_id, source_user_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut found = Vec::new();
        for row in rows {
            found.push(row?);
        }
        found
    };
    let mut updated = 0;
    for (source_key, labels_json) in candidates {
        let labels: Vec<String> = serde_json::from_str(&labels_json).unwrap_or_default();
        let resolved: Vec<String> = labels
            .iter()
            .map(|label| label_names.get(label).unwrap_or(label).clone())
            .collect();
        if resolved == labels {
            continue;
        }
        let resolved_json = serde_json::to_string(&resolved)
            .map_err(|err| StoreError::Domain(format!("serialize labels: {err}")))?;
        let idempotency_key = format!("relabel:{source_key}:{resolved_json}");
        let owned_client = client_id.to_string();
        let owned_source_key = source_key.clone();
        let owned_resolved = resolved_json.clone();
        store_core::mutate(
            conn,
            MutationRequest {
                client_id,
                entity_kind: INBOUND_ENTITY_KIND,
                entity_id: &source_key,
                change_kind: "relabel",
                actor_id: "gmail_ingest_pump",
                actor_kind: ActorKindDto::System,
                expected_revision: None,
                idempotency_key: &idempotency_key,
                correlation_id: None,
                causation_id: None,
                before_json: Some(format!("{{\"labels\":{labels_json}}}")),
                after_json: Some(format!("{{\"labels\":{resolved_json}}}")),
                now_ms,
            },
            move |tx| {
                tx.execute(
                    "UPDATE email_inbound_messages SET labels_json = ?3 \
                     WHERE client_id = ?1 AND source_key = ?2",
                    params![owned_client, owned_source_key, owned_resolved],
                )?;
                Ok(())
            },
        )?;
        updated += 1;
    }
    Ok(updated)
}

/// Recent classified inbound messages, newest first by Gmail internal date.
pub fn list_recent_inbound(
    conn: &Connection,
    client_id: &str,
    limit: usize,
    scope: &OperatorScope,
    filter: &InboxFilter,
) -> Result<Vec<InboundMessageRecord>, StoreError> {
    let bounded_limit = limit.max(1);
    let crm_sender_policy = super::service::crm_sender_policy(conn, client_id);
    let neutral_domain_roots = crm_sender_policy.neutral_domain_roots();
    let mut where_clauses = vec![
        "client_id = ?".to_string(),
        "NOT EXISTS (SELECT 1 FROM json_each(labels_json) WHERE value = 'TRASH')".to_string(),
    ];
    let mut binds = vec![Value::Text(client_id.to_string())];
    add_scope_filter(&mut where_clauses, &mut binds, "source_user_id", scope);
    add_gmail_category_filter(&mut where_clauses, &mut binds, &filter.categories);
    add_dashboard_category_filter(&mut where_clauses, &mut binds, &filter.dashboard_categories);
    add_json_label_filter(&mut where_clauses, &mut binds, &filter.labels);
    add_source_user_filter(
        &mut where_clauses,
        &mut binds,
        &filter.source_user_ids,
        scope,
    );
    add_crm_match_filter(
        &mut where_clauses,
        &mut binds,
        filter.crm_match,
        neutral_domain_roots,
    );
    add_crm_deal_facet_filter(
        &mut where_clauses,
        &mut binds,
        "stage",
        &filter.crm_deal_stages,
        neutral_domain_roots,
    );
    add_crm_deal_facet_filter(
        &mut where_clauses,
        &mut binds,
        "pipeline",
        &filter.crm_deal_pipelines,
        neutral_domain_roots,
    );
    add_search_filter(&mut where_clauses, &mut binds, filter.search.as_deref());
    binds.push(Value::Integer(bounded_limit as i64));
    let sql = format!(
        "SELECT source_key, message_id, thread_id, internal_date_ms, from_addr, to_addr, subject, \
         body_excerpt, body_full, labels_json, headers_json, attachments_json, resolved_category, \
         matched_rule_id, ingested_at_ms, ai_triage_status, ai_triage_rationale, source_user_id \
         FROM email_inbound_messages \
         WHERE {} \
         ORDER BY internal_date_ms DESC, source_key DESC LIMIT ?",
        where_clauses.join(" AND ")
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(binds.iter()), inbound_from_row)?;
    let mut records = Vec::new();
    for row in rows {
        records.push(row?);
    }
    Ok(records)
}

pub fn inbox_options(
    conn: &Connection,
    client_id: &str,
    scope: &OperatorScope,
) -> Result<InboxOptions, StoreError> {
    let category_counts = inbox_category_counts(conn, client_id, scope)?;
    let dashboard_category_counts = inbox_dashboard_category_counts(conn, client_id, scope)?;
    let label_counts = inbox_label_counts(conn, client_id, scope)?;
    let mailbox_counts = inbox_mailbox_counts(conn, client_id, scope)?;
    let crm_deal_stages = inbox_crm_deal_facet_counts(conn, client_id, scope, "stage")?;
    let crm_deal_pipelines = inbox_crm_deal_facet_counts(conn, client_id, scope, "pipeline")?;
    let user_names = operator_user_names(conn, client_id)?;
    Ok(InboxOptions {
        categories: gmail_categories()
            .iter()
            .copied()
            .map(|category| EmailTriageGmailCategoryOption {
                category,
                display_name: gmail_category_display_name(category).to_string(),
                count: *category_counts.get(&category).unwrap_or(&0),
            })
            .collect(),
        visible_gmail_categories: get_inbox_settings(conn, client_id)?
            .map(|settings| settings.visible_gmail_categories)
            .unwrap_or_else(default_visible_gmail_categories),
        dashboard_categories: dashboard_category_counts
            .into_iter()
            .map(|(category_id, count)| EmailTriageDashboardCategoryOption { category_id, count })
            .collect(),
        labels: label_counts
            .into_iter()
            .map(|(label, count)| EmailTriageLabelOption { label, count })
            .collect(),
        mailboxes: collapse_shared_operator_mailbox_counts(mailbox_counts, scope)
            .into_iter()
            .map(|(source_user_id, count)| {
                let display_name = source_user_id
                    .as_deref()
                    .and_then(|user_id| user_names.get(user_id).cloned())
                    .unwrap_or_else(|| {
                        source_user_id
                            .clone()
                            .unwrap_or_else(|| "Shared mailbox".to_string())
                    });
                EmailTriageMailboxOption {
                    source_user_id,
                    display_name,
                    count,
                }
            })
            .collect(),
        crm_deal_stages,
        crm_deal_pipelines,
    })
}

pub fn inbox_system_label_count(
    conn: &Connection,
    client_id: &str,
    scope: &OperatorScope,
    label: &str,
) -> Result<u32, StoreError> {
    let label = label.trim().to_ascii_uppercase();
    if label.is_empty() {
        return Ok(0);
    }
    let mut where_clauses = vec![
        "client_id = ?".to_string(),
        "NOT EXISTS (SELECT 1 FROM json_each(labels_json) WHERE value = 'TRASH')".to_string(),
    ];
    let mut binds = vec![Value::Text(client_id.to_string())];
    add_scope_filter(&mut where_clauses, &mut binds, "source_user_id", scope);
    binds.push(Value::Text(label));
    let sql = format!(
        "SELECT COUNT(DISTINCT source_key) FROM email_inbound_messages \
         WHERE {} AND EXISTS (SELECT 1 FROM json_each(labels_json) WHERE value = ?)",
        where_clauses.join(" AND ")
    );
    let count: i64 = conn.query_row(&sql, params_from_iter(binds.iter()), |row| row.get(0))?;
    Ok(count.max(0) as u32)
}

pub fn default_visible_gmail_categories() -> Vec<EmailTriageGmailCategory> {
    gmail_categories().to_vec()
}

pub fn get_inbox_settings(
    conn: &Connection,
    client_id: &str,
) -> Result<Option<StoredInboxSettings>, StoreError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT visible_gmail_categories_json FROM email_triage_inbox_settings \
             WHERE client_id = ?1",
            params![client_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(raw) = raw else {
        return Ok(None);
    };
    let parsed: Vec<EmailTriageGmailCategory> = serde_json::from_str(&raw)
        .map_err(|err| StoreError::Domain(format!("stored inbox settings corrupt: {err}")))?;
    let visible_gmail_categories = normalize_visible_gmail_categories(&parsed)?;
    let revision = store_core::current_revision(
        conn,
        client_id,
        INBOX_SETTINGS_ENTITY_KIND,
        INBOX_SETTINGS_ENTITY_ID,
    )?;
    Ok(Some(StoredInboxSettings {
        visible_gmail_categories,
        revision,
    }))
}

pub fn replace_inbox_settings(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    request: &EmailTriageInboxSettingsUpdateRequest,
    now_ms: u64,
) -> Result<MutationOutcome, StoreError> {
    let visible_gmail_categories =
        normalize_visible_gmail_categories(&request.visible_gmail_categories)?;
    let before_json = get_inbox_settings(conn, client_id)?
        .and_then(|settings| serde_json::to_string(&settings.visible_gmail_categories).ok());
    let after_json = serde_json::to_string(&visible_gmail_categories)
        .map_err(|err| StoreError::Domain(format!("serialize inbox settings: {err}")))?;
    let write_json = after_json.clone();
    let owned_client = client_id.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: INBOX_SETTINGS_ENTITY_KIND,
            entity_id: INBOX_SETTINGS_ENTITY_ID,
            change_kind: "replace",
            actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: request.expected_revision,
            idempotency_key: &request.idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json,
            after_json: Some(after_json),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO email_triage_inbox_settings \
                 (client_id, visible_gmail_categories_json, updated_at_ms) \
                 VALUES (?1, ?2, ?3) \
                 ON CONFLICT(client_id) DO UPDATE SET \
                   visible_gmail_categories_json = excluded.visible_gmail_categories_json, \
                   updated_at_ms = excluded.updated_at_ms",
                params![owned_client, write_json, now_ms as i64],
            )?;
            Ok(())
        },
    )
}

fn normalize_visible_gmail_categories(
    raw: &[EmailTriageGmailCategory],
) -> Result<Vec<EmailTriageGmailCategory>, StoreError> {
    let mut visible = Vec::new();
    for category in gmail_categories() {
        if raw.iter().any(|candidate| candidate == category) {
            visible.push(*category);
        }
    }
    if visible.is_empty() {
        return Err(StoreError::Domain(
            "email_triage_visible_gmail_categories_empty".to_string(),
        ));
    }
    Ok(visible)
}

fn add_scope_filter(
    clauses: &mut Vec<String>,
    binds: &mut Vec<Value>,
    col: &str,
    scope: &OperatorScope,
) {
    match scope {
        OperatorScope::All => {}
        OperatorScope::User(user_id) => {
            clauses.push(format!("{col} = ?"));
            binds.push(Value::Text(user_id.clone()));
        }
    }
}

fn add_dashboard_category_filter(
    clauses: &mut Vec<String>,
    binds: &mut Vec<Value>,
    dashboard_categories: &[String],
) {
    let categories: Vec<String> = dashboard_categories
        .iter()
        .map(|category| category.trim().to_string())
        .filter(|category| !category.is_empty())
        .collect();
    if categories.is_empty() {
        return;
    }
    let placeholders = std::iter::repeat_n("?", categories.len())
        .collect::<Vec<_>>()
        .join(", ");
    clauses.push(format!("resolved_category IN ({placeholders})"));
    binds.extend(categories.into_iter().map(Value::Text));
}

fn add_json_label_filter(clauses: &mut Vec<String>, binds: &mut Vec<Value>, labels: &[String]) {
    let labels: Vec<String> = labels
        .iter()
        .map(|label| label.trim().to_string())
        .filter(|label| !label.is_empty())
        .collect();
    if labels.is_empty() {
        return;
    }
    let placeholders = std::iter::repeat_n("?", labels.len())
        .collect::<Vec<_>>()
        .join(", ");
    clauses.push(format!(
        "EXISTS (SELECT 1 FROM json_each(labels_json) WHERE value IN ({placeholders}))"
    ));
    binds.extend(labels.into_iter().map(Value::Text));
}

fn add_gmail_category_filter(
    clauses: &mut Vec<String>,
    binds: &mut Vec<Value>,
    categories: &[EmailTriageGmailCategory],
) {
    if categories.is_empty() {
        return;
    }
    let mut parts = Vec::new();
    for category in categories {
        if *category == EmailTriageGmailCategory::Primary {
            parts.push(format!(
                "(EXISTS (SELECT 1 FROM json_each(labels_json) WHERE value = ?) \
                  OR (EXISTS (SELECT 1 FROM json_each(labels_json) WHERE value = 'INBOX') \
                    AND NOT EXISTS (SELECT 1 FROM json_each(labels_json) WHERE value IN ({}))))",
                gmail_category_label_list_sql()
            ));
            binds.push(Value::Text(gmail_category_label(*category).to_string()));
        } else {
            parts.push("EXISTS (SELECT 1 FROM json_each(labels_json) WHERE value = ?)".to_string());
            binds.push(Value::Text(gmail_category_label(*category).to_string()));
        }
    }
    clauses.push(format!("({})", parts.join(" OR ")));
}

fn add_source_user_filter(
    clauses: &mut Vec<String>,
    binds: &mut Vec<Value>,
    source_user_ids: &[Option<String>],
    scope: &OperatorScope,
) {
    let mut parts = Vec::new();
    for source_user_id in source_user_ids {
        match source_user_id {
            Some(user_id)
                if matches!(scope, OperatorScope::All)
                    && user_id.trim() == SHARED_OPERATOR_ACTOR =>
            {
                parts.push("(source_user_id = ? OR source_user_id IS NULL)".to_string());
                binds.push(Value::Text(SHARED_OPERATOR_ACTOR.to_string()));
            }
            Some(user_id) if !user_id.trim().is_empty() => {
                parts.push("source_user_id = ?".to_string());
                binds.push(Value::Text(user_id.trim().to_string()));
            }
            None => parts.push("source_user_id IS NULL".to_string()),
            Some(_) => {}
        }
    }
    if !parts.is_empty() {
        clauses.push(format!("({})", parts.join(" OR ")));
    }
}

fn add_crm_match_filter(
    clauses: &mut Vec<String>,
    binds: &mut Vec<Value>,
    filter: Option<InboxCrmMatchFilter>,
    neutral_domain_roots: &[String],
) {
    let Some(filter) = filter else {
        return;
    };
    match filter {
        InboxCrmMatchFilter::HasContact => {
            clauses.push(crm_contact_exists_sql(None, neutral_domain_roots, binds));
        }
        InboxCrmMatchFilter::HasDeal => {
            clauses.push(crm_deal_exists_sql(None, neutral_domain_roots, binds));
        }
        InboxCrmMatchFilter::NoMatch => {
            let contact_exists = crm_contact_exists_sql(None, neutral_domain_roots, binds);
            let deal_exists = crm_deal_exists_sql(None, neutral_domain_roots, binds);
            clauses.push(format!("NOT ({contact_exists})"));
            clauses.push(format!("NOT ({deal_exists})"));
        }
    }
}

fn add_crm_deal_facet_filter(
    clauses: &mut Vec<String>,
    binds: &mut Vec<Value>,
    facet_column: &str,
    values: &[String],
    neutral_domain_roots: &[String],
) {
    let values: Vec<String> = values
        .iter()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect();
    if values.is_empty() {
        return;
    }
    let column = match facet_column {
        "stage" => "stage",
        "pipeline" => "pipeline",
        _ => return,
    };
    let placeholders = std::iter::repeat_n("?", values.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sender_is_customer = crm_sender_email_customer_sql(None, neutral_domain_roots, binds);
    clauses.push(format!(
        "EXISTS (SELECT 1 FROM crm_deal_snapshots d \
          WHERE d.client_id = email_inbound_messages.client_id AND d.active = 1 \
            AND lower(d.associated_contact_email) = {} \
            AND {sender_is_customer} \
            AND lower(COALESCE(d.{column}, '')) IN ({placeholders}))",
        crm_effective_email_sql(None)
    ));
    binds.extend(values.into_iter().map(Value::Text));
}

fn crm_contact_exists_sql(
    message_alias: Option<&str>,
    neutral_domain_roots: &[String],
    binds: &mut Vec<Value>,
) -> String {
    let prefix = message_alias
        .map(|alias| format!("{alias}."))
        .unwrap_or_default();
    let sender_is_customer =
        crm_sender_email_customer_sql(message_alias, neutral_domain_roots, binds);
    format!(
        "EXISTS (SELECT 1 FROM crm_contact_snapshots c \
          WHERE c.client_id = {prefix}client_id AND c.active = 1 \
            AND lower(c.email) = {} AND {sender_is_customer})",
        crm_effective_email_sql(message_alias)
    )
}

fn crm_deal_exists_sql(
    message_alias: Option<&str>,
    neutral_domain_roots: &[String],
    binds: &mut Vec<Value>,
) -> String {
    let prefix = message_alias
        .map(|alias| format!("{alias}."))
        .unwrap_or_default();
    let sender_is_customer =
        crm_sender_email_customer_sql(message_alias, neutral_domain_roots, binds);
    format!(
        "EXISTS (SELECT 1 FROM crm_deal_snapshots d \
          WHERE d.client_id = {prefix}client_id AND d.active = 1 \
            AND lower(d.associated_contact_email) = {} AND {sender_is_customer})",
        crm_effective_email_sql(message_alias)
    )
}

fn crm_sender_email_sql(message_alias: Option<&str>) -> String {
    message_alias
        .map(|alias| format!("{alias}.sender_email"))
        .unwrap_or_else(|| "sender_email".to_string())
}

fn crm_effective_email_sql(message_alias: Option<&str>) -> String {
    let represented = crm_sender_column_sql(message_alias, "represented_email");
    let sender = crm_sender_email_sql(message_alias);
    format!("COALESCE(NULLIF({represented}, ''), {sender})")
}

fn crm_effective_domain_sql(message_alias: Option<&str>) -> String {
    let represented = crm_sender_column_sql(message_alias, "represented_domain");
    let sender = crm_sender_column_sql(message_alias, "sender_domain");
    format!("COALESCE(NULLIF({represented}, ''), {sender})")
}

fn crm_sender_column_sql(message_alias: Option<&str>, column: &str) -> String {
    message_alias
        .map(|alias| format!("{alias}.{column}"))
        .unwrap_or_else(|| column.to_string())
}

fn crm_sender_email_customer_sql(
    message_alias: Option<&str>,
    neutral_domain_roots: &[String],
    binds: &mut Vec<Value>,
) -> String {
    let email = crm_sender_email_sql(message_alias);
    let represented_email = crm_sender_column_sql(message_alias, "represented_email");
    let domain = crm_effective_domain_sql(message_alias);
    let automation = crm_sender_column_sql(message_alias, "sender_automation_local_part");
    let header_blocked = crm_sender_column_sql(message_alias, "sender_header_identity_blocked");
    let represented_customer =
        format!("{represented_email} IS NOT NULL AND instr({represented_email}, '@') > 1");
    let mut domain_parts = Vec::new();
    for root in neutral_domain_roots {
        domain_parts.push(format!("({domain} = ? OR {domain} LIKE ?)"));
        binds.push(Value::Text(root.clone()));
        binds.push(Value::Text(format!("%.{}", root)));
    }
    if domain_parts.is_empty() {
        return format!(
            "(({represented_customer}) OR \
             ({email} IS NOT NULL AND instr({email}, '@') > 1 \
              AND COALESCE({header_blocked}, 0) = 0))"
        );
    }
    format!(
        "(({represented_customer}) OR \
         ({email} IS NOT NULL AND instr({email}, '@') > 1 \
          AND COALESCE({header_blocked}, 0) = 0 \
          AND NOT (COALESCE({automation}, 0) = 1 AND ({}))))",
        domain_parts.join(" OR ")
    )
}

fn add_search_filter(clauses: &mut Vec<String>, binds: &mut Vec<Value>, search: Option<&str>) {
    let terms = search_terms(search.unwrap_or_default());
    if terms.is_empty() {
        return;
    }
    for term in terms {
        let pattern = format!("%{}%", escape_like(&term));
        clauses.push(
            "(COALESCE(from_addr, '') LIKE ? ESCAPE '\\' \
              OR COALESCE(to_addr, '') LIKE ? ESCAPE '\\' \
              OR COALESCE(subject, '') LIKE ? ESCAPE '\\' \
              OR COALESCE(body_excerpt, '') LIKE ? ESCAPE '\\' \
              OR COALESCE(body_full, '') LIKE ? ESCAPE '\\' \
              OR COALESCE(resolved_category, '') LIKE ? ESCAPE '\\' \
              OR COALESCE(matched_rule_id, '') LIKE ? ESCAPE '\\' \
              OR EXISTS (SELECT 1 FROM json_each(labels_json) WHERE value LIKE ? ESCAPE '\\'))"
                .to_string(),
        );
        for _ in 0..8 {
            binds.push(Value::Text(pattern.clone()));
        }
    }
}

fn search_terms(raw: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;
    for ch in raw.trim().chars() {
        match ch {
            '"' => {
                in_quote = !in_quote;
                if !in_quote {
                    let term = current.trim();
                    if !term.is_empty() {
                        terms.push(term.chars().take(128).collect());
                    }
                    current.clear();
                }
            }
            ch if ch.is_whitespace() && !in_quote => {
                let term = current.trim();
                if !term.is_empty() {
                    terms.push(term.chars().take(128).collect());
                }
                current.clear();
            }
            _ => current.push(ch),
        }
        if terms.len() >= 8 {
            break;
        }
    }
    let term = current.trim();
    if terms.len() < 8 && !term.is_empty() {
        terms.push(term.chars().take(128).collect());
    }
    terms
}

fn escape_like(raw: &str) -> String {
    raw.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn collapse_shared_operator_mailbox_counts(
    mut counts: std::collections::BTreeMap<Option<String>, u32>,
    scope: &OperatorScope,
) -> std::collections::BTreeMap<Option<String>, u32> {
    if !matches!(scope, OperatorScope::All) {
        return counts;
    }
    let legacy_count = counts.remove(&None).unwrap_or(0);
    if legacy_count == 0 {
        return counts;
    }
    *counts
        .entry(Some(SHARED_OPERATOR_ACTOR.to_string()))
        .or_insert(0) += legacy_count;
    counts
}

fn inbox_category_counts(
    conn: &Connection,
    client_id: &str,
    scope: &OperatorScope,
) -> Result<std::collections::HashMap<EmailTriageGmailCategory, u32>, StoreError> {
    let mut where_clauses = vec![
        "client_id = ?".to_string(),
        "NOT EXISTS (SELECT 1 FROM json_each(labels_json) WHERE value = 'TRASH')".to_string(),
    ];
    let mut binds = vec![Value::Text(client_id.to_string())];
    add_scope_filter(&mut where_clauses, &mut binds, "source_user_id", scope);
    let sql = format!(
        "SELECT json_each.value, COUNT(DISTINCT source_key) \
         FROM email_inbound_messages, json_each(labels_json) \
         WHERE {} AND json_each.value IN ('CATEGORY_PERSONAL', 'CATEGORY_UPDATES', \
           'CATEGORY_SOCIAL', 'CATEGORY_PROMOTIONS', 'CATEGORY_FORUMS') \
         GROUP BY json_each.value",
        where_clauses.join(" AND ")
    );
    let mut counts = gmail_categories()
        .iter()
        .copied()
        .map(|category| (category, 0))
        .collect::<std::collections::HashMap<_, _>>();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(binds.iter()), |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (label, count) = row?;
        if let Some(category) = gmail_category_from_label(&label) {
            counts.insert(category, count.max(0) as u32);
        }
    }
    let primary_fallback_sql = format!(
        "SELECT COUNT(*) FROM email_inbound_messages \
         WHERE {} \
           AND EXISTS (SELECT 1 FROM json_each(labels_json) WHERE value = 'INBOX') \
           AND NOT EXISTS (SELECT 1 FROM json_each(labels_json) WHERE value IN ({}))",
        where_clauses.join(" AND "),
        gmail_category_label_list_sql()
    );
    let primary_fallback_count: i64 = conn.query_row(
        &primary_fallback_sql,
        params_from_iter(binds.iter()),
        |row| row.get(0),
    )?;
    if primary_fallback_count > 0 {
        let current = counts
            .get(&EmailTriageGmailCategory::Primary)
            .copied()
            .unwrap_or(0);
        counts.insert(
            EmailTriageGmailCategory::Primary,
            current.saturating_add(primary_fallback_count as u32),
        );
    }
    Ok(counts)
}

fn inbox_dashboard_category_counts(
    conn: &Connection,
    client_id: &str,
    scope: &OperatorScope,
) -> Result<std::collections::BTreeMap<String, u32>, StoreError> {
    let mut where_clauses = vec![
        "client_id = ?".to_string(),
        "NOT EXISTS (SELECT 1 FROM json_each(labels_json) WHERE value = 'TRASH')".to_string(),
    ];
    let mut binds = vec![Value::Text(client_id.to_string())];
    add_scope_filter(&mut where_clauses, &mut binds, "source_user_id", scope);
    let sql = format!(
        "SELECT resolved_category, COUNT(*) FROM email_inbound_messages \
         WHERE {} GROUP BY resolved_category ORDER BY resolved_category COLLATE NOCASE ASC",
        where_clauses.join(" AND ")
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(binds.iter()), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?.max(0) as u32,
        ))
    })?;
    let mut counts = std::collections::BTreeMap::new();
    for row in rows {
        let (category_id, count) = row?;
        counts.insert(category_id, count);
    }
    Ok(counts)
}

fn inbox_label_counts(
    conn: &Connection,
    client_id: &str,
    scope: &OperatorScope,
) -> Result<std::collections::BTreeMap<String, u32>, StoreError> {
    let mut where_clauses = vec![
        "client_id = ?".to_string(),
        "NOT EXISTS (SELECT 1 FROM json_each(labels_json) WHERE value = 'TRASH')".to_string(),
    ];
    let mut binds = vec![Value::Text(client_id.to_string())];
    add_scope_filter(&mut where_clauses, &mut binds, "source_user_id", scope);
    let sql = format!(
        "SELECT json_each.value, COUNT(DISTINCT source_key) \
         FROM email_inbound_messages, json_each(labels_json) \
         WHERE {} \
         GROUP BY json_each.value ORDER BY json_each.value COLLATE NOCASE ASC",
        where_clauses.join(" AND ")
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(binds.iter()), |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    let mut counts = std::collections::BTreeMap::new();
    for row in rows {
        let (label, count) = row?;
        if is_operator_label(&label) {
            counts.insert(label, count.max(0) as u32);
        }
    }
    Ok(counts)
}

fn inbox_mailbox_counts(
    conn: &Connection,
    client_id: &str,
    scope: &OperatorScope,
) -> Result<std::collections::BTreeMap<Option<String>, u32>, StoreError> {
    let mut where_clauses = vec![
        "client_id = ?".to_string(),
        "NOT EXISTS (SELECT 1 FROM json_each(labels_json) WHERE value = 'TRASH')".to_string(),
    ];
    let mut binds = vec![Value::Text(client_id.to_string())];
    add_scope_filter(&mut where_clauses, &mut binds, "source_user_id", scope);
    let sql = format!(
        "SELECT source_user_id, COUNT(*) FROM email_inbound_messages \
         WHERE {} GROUP BY source_user_id ORDER BY source_user_id COLLATE NOCASE ASC",
        where_clauses.join(" AND ")
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(binds.iter()), |row| {
        Ok((
            row.get::<_, Option<String>>(0)?,
            row.get::<_, i64>(1)?.max(0) as u32,
        ))
    })?;
    let mut counts = std::collections::BTreeMap::new();
    for row in rows {
        let (source_user_id, count) = row?;
        counts.insert(source_user_id, count);
    }
    Ok(counts)
}

fn inbox_crm_deal_facet_counts(
    conn: &Connection,
    client_id: &str,
    scope: &OperatorScope,
    facet_column: &str,
) -> Result<Vec<EmailTriageCrmDealFacetOption>, StoreError> {
    let column = match facet_column {
        "stage" => "stage",
        "pipeline" => "pipeline",
        _ => {
            return Err(StoreError::Domain(
                "email_triage_crm_deal_facet_invalid".to_string(),
            ));
        }
    };
    let crm_sender_policy = super::service::crm_sender_policy(conn, client_id);
    let neutral_domain_roots = crm_sender_policy.neutral_domain_roots();
    let mut where_clauses = vec![
        "m.client_id = ?".to_string(),
        "NOT EXISTS (SELECT 1 FROM json_each(m.labels_json) WHERE value = 'TRASH')".to_string(),
    ];
    let mut binds = vec![Value::Text(client_id.to_string())];
    add_scope_filter(&mut where_clauses, &mut binds, "m.source_user_id", scope);
    where_clauses.push(crm_sender_email_customer_sql(
        Some("m"),
        neutral_domain_roots,
        &mut binds,
    ));
    let sql = format!(
        "SELECT d.{column}, COUNT(DISTINCT m.source_key) \
         FROM email_inbound_messages m \
         JOIN crm_deal_snapshots d \
           ON d.client_id = m.client_id AND d.active = 1 \
          AND lower(d.associated_contact_email) = {} \
         WHERE {} AND d.{column} IS NOT NULL AND trim(d.{column}) <> '' \
         GROUP BY d.{column} \
         ORDER BY COUNT(DISTINCT m.source_key) DESC, d.{column} COLLATE NOCASE ASC \
         LIMIT 100",
        crm_effective_email_sql(Some("m")),
        where_clauses.join(" AND ")
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(binds.iter()), |row| {
        Ok(EmailTriageCrmDealFacetOption {
            value: row.get(0)?,
            count: row.get::<_, i64>(1)?.max(0) as u32,
        })
    })?;
    let mut options = Vec::new();
    for row in rows {
        options.push(row?);
    }
    Ok(options)
}

fn inbound_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<InboundMessageRecord> {
    Ok(InboundMessageRecord {
        source_key: row.get("source_key")?,
        message_id: row.get("message_id")?,
        thread_id: row.get("thread_id")?,
        internal_date_ms: row.get("internal_date_ms")?,
        from_addr: row.get("from_addr")?,
        to_addr: row.get("to_addr")?,
        subject: row.get("subject")?,
        body_excerpt: row.get("body_excerpt")?,
        body_full: row.get("body_full")?,
        labels: serde_json::from_str(&row.get::<_, String>("labels_json")?).unwrap_or_default(),
        headers: headers_from_row(row)?,
        attachments: attachments_from_row(row)?,
        resolved_category: row.get("resolved_category")?,
        matched_rule_id: row.get("matched_rule_id")?,
        ingested_at_ms: row.get::<_, i64>("ingested_at_ms")? as u64,
        ai_triage_status: row.get("ai_triage_status")?,
        ai_triage_rationale: row.get("ai_triage_rationale")?,
        source_user_id: row.get("source_user_id")?,
    })
}

fn attachments_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Vec<EmailAttachmentRecord>> {
    let raw: String = row.get("attachments_json")?;
    Ok(serde_json::from_str(&raw).unwrap_or_default())
}

fn headers_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Vec<(String, String)>> {
    let raw: String = row.get("headers_json")?;
    Ok(serde_json::from_str(&raw).unwrap_or_default())
}

fn gmail_category_from_label(label: &str) -> Option<EmailTriageGmailCategory> {
    match label.trim().to_ascii_uppercase().as_str() {
        "CATEGORY_PERSONAL" => Some(EmailTriageGmailCategory::Primary),
        "CATEGORY_UPDATES" => Some(EmailTriageGmailCategory::Updates),
        "CATEGORY_SOCIAL" => Some(EmailTriageGmailCategory::Social),
        "CATEGORY_PROMOTIONS" => Some(EmailTriageGmailCategory::Promotions),
        "CATEGORY_FORUMS" => Some(EmailTriageGmailCategory::Forums),
        _ => None,
    }
}

fn gmail_category_label(category: EmailTriageGmailCategory) -> &'static str {
    match category {
        EmailTriageGmailCategory::Primary => "CATEGORY_PERSONAL",
        EmailTriageGmailCategory::Updates => "CATEGORY_UPDATES",
        EmailTriageGmailCategory::Social => "CATEGORY_SOCIAL",
        EmailTriageGmailCategory::Promotions => "CATEGORY_PROMOTIONS",
        EmailTriageGmailCategory::Forums => "CATEGORY_FORUMS",
    }
}

fn gmail_category_label_list_sql() -> &'static str {
    "'CATEGORY_PERSONAL', 'CATEGORY_UPDATES', 'CATEGORY_SOCIAL', 'CATEGORY_PROMOTIONS', \
     'CATEGORY_FORUMS'"
}

fn gmail_category_display_name(category: EmailTriageGmailCategory) -> &'static str {
    match category {
        EmailTriageGmailCategory::Primary => "Primary",
        EmailTriageGmailCategory::Updates => "Updates",
        EmailTriageGmailCategory::Social => "Social",
        EmailTriageGmailCategory::Promotions => "Promotions",
        EmailTriageGmailCategory::Forums => "Forums",
    }
}

fn gmail_categories() -> &'static [EmailTriageGmailCategory] {
    &[
        EmailTriageGmailCategory::Primary,
        EmailTriageGmailCategory::Updates,
        EmailTriageGmailCategory::Social,
        EmailTriageGmailCategory::Promotions,
        EmailTriageGmailCategory::Forums,
    ]
}

fn is_operator_label(label: &str) -> bool {
    let normalized = label.trim().to_ascii_uppercase();
    !matches!(
        normalized.as_str(),
        "" | "INBOX"
            | "UNREAD"
            | "SENT"
            | "DRAFT"
            | "SPAM"
            | "TRASH"
            | "STARRED"
            | "IMPORTANT"
            | "CHAT"
            | "CATEGORY_PERSONAL"
            | "CATEGORY_SOCIAL"
            | "CATEGORY_PROMOTIONS"
            | "CATEGORY_UPDATES"
            | "CATEGORY_FORUMS"
    )
}

fn operator_user_names(
    conn: &Connection,
    client_id: &str,
) -> Result<std::collections::HashMap<String, String>, StoreError> {
    let mut stmt =
        conn.prepare("SELECT user_id, display_name FROM operator_users WHERE client_id = ?1")?;
    let rows = stmt.query_map(params![client_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut names = std::collections::HashMap::new();
    for row in rows {
        let (user_id, display_name) = row?;
        names.insert(user_id, display_name);
    }
    Ok(names)
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

/// Default categories seeded the first time a client's registry is read.
/// Descriptions are written for the future AI classifier, not just humans.
const DEFAULT_CATEGORIES: &[(&str, &str, &str, &str, i32, bool)] = &[
    (
        FALLBACK_CATEGORY_ID,
        "Inbound email",
        "General inbound email that doesn't fit a more specific category.",
        "#38bdf8",
        10,
        true,
    ),
    (
        "operator_note",
        "Operator note",
        "Internal notes written by the operator, not external correspondence.",
        "#f59e0b",
        20,
        false,
    ),
];

/// Active categories in sort order, lazily seeding any default the client has
/// never had a row for (receipted, actor system). Per-id check — a default
/// added in a later release backfills on existing deployments, while a
/// default the operator deleted stays deleted (the soft-deleted row remains).
pub fn list_categories(
    conn: &mut Connection,
    client_id: &str,
    now_ms: u64,
) -> Result<Vec<CategoryRecord>, StoreError> {
    let known_ids: std::collections::HashSet<String> = {
        let mut stmt =
            conn.prepare("SELECT category_id FROM email_triage_categories WHERE client_id = ?1")?;
        let rows = stmt.query_map(params![client_id], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<_, _>>()?
    };
    for (id, name, description, color, sort, is_system) in DEFAULT_CATEGORIES {
        if known_ids.contains(*id) {
            continue;
        }
        let record = CategoryRecord {
            category_id: (*id).to_string(),
            display_name: (*name).to_string(),
            description: (*description).to_string(),
            color: (*color).to_string(),
            sort: *sort,
            is_system: *is_system,
            default_agent_dir: String::new(),
            default_agent_context: String::new(),
        };
        let seed_key = format!("seed:{}", record.category_id);
        write_category(conn, client_id, "system_seed", &record, &seed_key, now_ms)?;
    }
    let mut stmt = conn.prepare(
        "SELECT category_id, display_name, description, color, sort, is_system, \
         default_agent_dir, default_agent_context \
         FROM email_triage_categories WHERE client_id = ?1 AND deleted = 0 \
         ORDER BY sort ASC, category_id ASC",
    )?;
    let rows = stmt.query_map(params![client_id], |row| {
        Ok(CategoryRecord {
            category_id: row.get(0)?,
            display_name: row.get(1)?,
            description: row.get(2)?,
            color: row.get(3)?,
            sort: row.get(4)?,
            is_system: row.get(5)?,
            default_agent_dir: row.get(6)?,
            default_agent_context: row.get(7)?,
        })
    })?;
    let mut categories = Vec::new();
    for row in rows {
        categories.push(row?);
    }
    Ok(categories)
}

pub fn category_by_id(
    conn: &Connection,
    client_id: &str,
    category_id: &str,
) -> Result<Option<CategoryRecord>, StoreError> {
    use rusqlite::OptionalExtension;
    let category = conn
        .query_row(
            "SELECT category_id, display_name, description, color, sort, is_system, \
             default_agent_dir, default_agent_context \
             FROM email_triage_categories \
             WHERE client_id = ?1 AND category_id = ?2 AND deleted = 0",
            params![client_id, category_id],
            |row| {
                Ok(CategoryRecord {
                    category_id: row.get(0)?,
                    display_name: row.get(1)?,
                    description: row.get(2)?,
                    color: row.get(3)?,
                    sort: row.get(4)?,
                    is_system: row.get(5)?,
                    default_agent_dir: row.get(6)?,
                    default_agent_context: row.get(7)?,
                })
            },
        )
        .optional()?;
    Ok(category)
}

pub fn category_is_active(
    conn: &Connection,
    client_id: &str,
    category_id: &str,
) -> Result<bool, StoreError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM email_triage_categories \
         WHERE client_id = ?1 AND category_id = ?2 AND deleted = 0",
        params![client_id, category_id],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

pub fn upsert_category(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    record: &CategoryRecord,
    idempotency_key: &str,
    now_ms: u64,
) -> Result<MutationOutcome, StoreError> {
    if !validate_category_id(&record.category_id) {
        return Err(StoreError::Domain(
            "email_triage_category_id_invalid".to_string(),
        ));
    }
    if record.display_name.trim().is_empty() {
        return Err(StoreError::Domain(
            "email_triage_category_name_required".to_string(),
        ));
    }
    // is_system is server-owned: preserve the existing flag, never accept it
    // from the wire (the fallback category must stay protected).
    let existing_system: Option<bool> = {
        use rusqlite::OptionalExtension;
        conn.query_row(
            "SELECT is_system FROM email_triage_categories              WHERE client_id = ?1 AND category_id = ?2",
            params![client_id, record.category_id],
            |row| row.get(0),
        )
        .optional()?
    };
    let mut sanitized = record.clone();
    sanitized.is_system = existing_system.unwrap_or(false);
    write_category(
        conn,
        client_id,
        actor_id,
        &sanitized,
        idempotency_key,
        now_ms,
    )
}

pub fn upsert_category_with_policy(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    record: &CategoryRecord,
    policy: &WorkQueuePolicy,
    idempotency_key: &str,
    now_ms: u64,
) -> Result<MutationOutcome, StoreError> {
    if !validate_category_id(&record.category_id) {
        return Err(StoreError::Domain(
            "email_triage_category_id_invalid".to_string(),
        ));
    }
    if record.display_name.trim().is_empty() {
        return Err(StoreError::Domain(
            "email_triage_category_name_required".to_string(),
        ));
    }
    if policy.category_id != record.category_id {
        return Err(StoreError::Domain(
            "email_triage_category_policy_mismatch".to_string(),
        ));
    }
    let policy = crate::slices::work_queue::store::sanitize_policy(policy)?;
    let mut category = record.clone();
    category.is_system = false;
    let after = serde_json::json!({ "category": category, "policy": policy }).to_string();
    let owned_client = client_id.to_string();
    let entity_id = category.category_id.clone();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: CATEGORY_ENTITY_KIND,
            entity_id: &entity_id,
            change_kind: "create_with_policy",
            actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: None,
            idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO email_triage_categories \
                 (client_id, category_id, display_name, description, color, sort, is_system, \
                  default_agent_dir, default_agent_context, deleted, updated_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?8, 0, ?9)",
                params![
                    owned_client,
                    category.category_id,
                    category.display_name,
                    category.description,
                    category.color,
                    category.sort,
                    category.default_agent_dir.trim(),
                    category.default_agent_context.trim(),
                    now_ms as i64
                ],
            )?;
            crate::slices::work_queue::store::write_policy_tx(tx, &owned_client, &policy, now_ms)
        },
    )
}

fn write_category_tx(
    tx: &rusqlite::Transaction<'_>,
    client_id: &str,
    row: &CategoryRecord,
    now_ms: u64,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO email_triage_categories \
         (client_id, category_id, display_name, description, color, sort, is_system, \
          default_agent_dir, default_agent_context, deleted, updated_at_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, ?10) \
         ON CONFLICT (client_id, category_id) DO UPDATE SET \
           display_name = excluded.display_name, \
           description = excluded.description, \
           color = excluded.color, sort = excluded.sort, deleted = 0, \
           default_agent_dir = excluded.default_agent_dir, \
           default_agent_context = excluded.default_agent_context, \
           updated_at_ms = excluded.updated_at_ms",
        params![
            client_id,
            row.category_id,
            row.display_name,
            row.description,
            row.color,
            row.sort,
            row.is_system,
            row.default_agent_dir.trim(),
            row.default_agent_context.trim(),
            now_ms as i64
        ],
    )?;
    Ok(())
}

fn write_category(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    record: &CategoryRecord,
    idempotency_key: &str,
    now_ms: u64,
) -> Result<MutationOutcome, StoreError> {
    let after = serde_json::to_string(record)
        .map_err(|err| StoreError::Domain(format!("serialize category: {err}")))?;
    let row = record.clone();
    let owned_client = client_id.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: CATEGORY_ENTITY_KIND,
            entity_id: &record.category_id,
            change_kind: "upsert",
            actor_id,
            actor_kind: if actor_id == "system_seed" {
                ActorKindDto::System
            } else {
                ActorKindDto::Operator
            },
            expected_revision: None,
            idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms,
        },
        move |tx| write_category_tx(tx, &owned_client, &row, now_ms),
    )
}

/// Soft-delete a category. Refused for system categories and while any
/// non-deleted rule still pins it (historical inbox rows keep the raw id).
pub fn delete_category(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    category_id: &str,
    idempotency_key: &str,
    now_ms: u64,
) -> Result<MutationOutcome, StoreError> {
    use rusqlite::OptionalExtension;
    let is_system: Option<bool> = conn
        .query_row(
            "SELECT is_system FROM email_triage_categories              WHERE client_id = ?1 AND category_id = ?2 AND deleted = 0",
            params![client_id, category_id],
            |row| row.get(0),
        )
        .optional()?;
    match is_system {
        None => {
            return Err(StoreError::Domain(
                "email_triage_category_not_found".to_string(),
            ))
        }
        Some(true) => {
            return Err(StoreError::Domain(
                "email_triage_category_is_system".to_string(),
            ))
        }
        Some(false) => {}
    }
    let pinned_by: i64 = conn.query_row(
        "SELECT COUNT(*) FROM email_triage_rules          WHERE client_id = ?1 AND deleted = 0          AND json_extract(rule_json, '$.pinned_category') = ?2",
        params![client_id, category_id],
        |row| row.get(0),
    )?;
    if pinned_by > 0 {
        return Err(StoreError::Domain(
            "email_triage_category_in_use".to_string(),
        ));
    }
    let owned_client = client_id.to_string();
    let owned_category = category_id.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: CATEGORY_ENTITY_KIND,
            entity_id: category_id,
            change_kind: "delete",
            actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: None,
            idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: None,
            now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE email_triage_categories SET deleted = 1, updated_at_ms = ?3                  WHERE client_id = ?1 AND category_id = ?2",
                params![owned_client, owned_category, now_ms as i64],
            )?;
            Ok(())
        },
    )
}

fn category_exists(
    conn: &Connection,
    client_id: &str,
    category_id: &str,
) -> Result<bool, StoreError> {
    // An empty registry means defaults haven't been seeded yet; accept the
    // default ids so rule creation doesn't depend on a prior categories read.
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM email_triage_categories WHERE client_id = ?1 AND deleted = 0",
        params![client_id],
        |row| row.get(0),
    )?;
    if count == 0 {
        return Ok(DEFAULT_CATEGORIES.iter().any(|(id, ..)| *id == category_id));
    }
    let found: i64 = conn.query_row(
        "SELECT COUNT(*) FROM email_triage_categories          WHERE client_id = ?1 AND category_id = ?2 AND deleted = 0",
        params![client_id, category_id],
        |row| row.get(0),
    )?;
    Ok(found > 0)
}

/// Stored records for the given source keys (used by the pump to re-run work
/// item emission over already-ingested messages when policy changes).
pub fn inbound_by_source_keys(
    conn: &Connection,
    client_id: &str,
    source_keys: &[String],
    scope: &OperatorScope,
) -> Result<Vec<InboundMessageRecord>, StoreError> {
    let (scope_pred, scope_all, scope_user) = scope.sql_filter("source_user_id", 3, 4);
    let mut records = Vec::new();
    let mut stmt = conn.prepare(&format!(
        "SELECT source_key, message_id, thread_id, internal_date_ms, from_addr, to_addr, subject, \
         body_excerpt, body_full, labels_json, headers_json, attachments_json, resolved_category, \
         matched_rule_id, ingested_at_ms, ai_triage_status, ai_triage_rationale, source_user_id \
         FROM email_inbound_messages \
         WHERE client_id = ?1 AND source_key = ?2 AND {scope_pred}",
    ))?;
    for source_key in source_keys {
        let row = {
            use rusqlite::OptionalExtension;
            stmt.query_row(
                params![client_id, source_key, scope_all, scope_user],
                |row| {
                    Ok(InboundMessageRecord {
                        source_key: row.get("source_key")?,
                        message_id: row.get("message_id")?,
                        thread_id: row.get("thread_id")?,
                        internal_date_ms: row.get("internal_date_ms")?,
                        from_addr: row.get("from_addr")?,
                        to_addr: row.get("to_addr")?,
                        subject: row.get("subject")?,
                        body_excerpt: row.get("body_excerpt")?,
                        body_full: row.get("body_full")?,
                        labels: serde_json::from_str(&row.get::<_, String>("labels_json")?)
                            .unwrap_or_default(),
                        headers: headers_from_row(row)?,
                        attachments: attachments_from_row(row)?,
                        resolved_category: row.get("resolved_category")?,
                        matched_rule_id: row.get("matched_rule_id")?,
                        ingested_at_ms: row.get::<_, i64>("ingested_at_ms")? as u64,
                        ai_triage_status: row.get("ai_triage_status")?,
                        ai_triage_rationale: row.get("ai_triage_rationale")?,
                        source_user_id: row.get("source_user_id")?,
                    })
                },
            )
            .optional()?
        };
        if let Some(record) = row {
            records.push(record);
        }
    }
    Ok(records)
}

/// Record an explicit operator trash request, hide the message from the local
/// inbox, dismiss any queue item for it, and enqueue the Gmail effect in the
/// same transaction. The message record remains as auditable source evidence.
pub fn request_gmail_trash(
    conn: &mut Connection,
    ctx: crate::slices::work_queue::store::ItemActionContext<'_>,
    message: &InboundMessageRecord,
) -> Result<MutationOutcome, StoreError> {
    let existing_item: Option<(String, String)> = conn
        .query_row(
            "SELECT item_id, status FROM work_items \
             WHERE client_id = ?1 AND source_kind = 'email' AND source_ref = ?2 LIMIT 1",
            params![ctx.client_id, message.source_key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (entity_kind, entity_id, before_status) = existing_item
        .as_ref()
        .map(|(item_id, status)| {
            (
                crate::slices::work_queue::store::ITEM_ENTITY_KIND,
                item_id.as_str(),
                Some(status.as_str()),
            )
        })
        .unwrap_or((INBOUND_ENTITY_KIND, message.source_key.as_str(), None));
    if existing_item.is_none() && ctx.expected_revision.is_some() {
        return Err(StoreError::Domain(
            "email_trash_expected_revision_without_work_item".to_string(),
        ));
    }

    let mut labels = message.labels.clone();
    labels.retain(|label| label != "INBOX");
    if !labels.iter().any(|label| label == "TRASH") {
        labels.push("TRASH".to_string());
    }
    let labels_json = serde_json::to_string(&labels)
        .map_err(|err| StoreError::Domain(format!("serialize trash labels: {err}")))?;
    let provider_idempotency_key = format!("gmail-trash:{}", message.source_key);
    let payload = bos_integrations::gmail_trash_write::GmailTrashOutboxPayload {
        idempotency_key: provider_idempotency_key.clone(),
        credential_user_id: message.source_user_id.clone(),
        message_id: message.message_id.clone(),
    };
    let job = crate::outbox::NewOutboxJob {
        job_id: format!(
            "ogt_{}_{}",
            short_hash(&message.source_key),
            short_hash(ctx.idempotency_key)
        ),
        provider: crate::slices::email_drafts::service::PROVIDER_GMAIL.to_string(),
        capability: GMAIL_TRASH_CAPABILITY.to_string(),
        payload_json: serde_json::to_string(&payload)
            .map_err(|err| StoreError::Domain(format!("serialize Gmail trash payload: {err}")))?,
        source_entity_kind: entity_kind.to_string(),
        source_entity_id: entity_id.to_string(),
        correlation_id: existing_item.as_ref().map(|(item_id, _)| item_id.clone()),
        causation_id: None,
        idempotency_key: provider_idempotency_key,
    };
    let owned_client = ctx.client_id.to_string();
    let owned_source = message.source_key.clone();
    let owned_item = existing_item.as_ref().map(|(item_id, _)| item_id.clone());
    let owned_job = job.clone();
    let after_json = serde_json::json!({
        "gmail_trash_requested": true,
        "status": owned_item.as_ref().map(|_| "dismissed"),
        "outbox_job_id": job.job_id,
    })
    .to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind,
            entity_id,
            change_kind: "trash_email",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: existing_item.as_ref().map(|(item_id, _)| item_id.as_str()),
            causation_id: None,
            before_json: Some(
                serde_json::json!({
                    "labels": message.labels,
                    "status": before_status,
                })
                .to_string(),
            ),
            after_json: Some(after_json),
            now_ms: ctx.now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE email_inbound_messages SET labels_json = ?3 \
                 WHERE client_id = ?1 AND source_key = ?2",
                params![owned_client, owned_source, labels_json],
            )?;
            if let Some(item_id) = owned_item {
                tx.execute(
                    "UPDATE work_items SET status = 'dismissed', accept_actor = NULL, updated_at_ms = ?3 \
                     WHERE client_id = ?1 AND item_id = ?2",
                    params![ctx.client_id, item_id, ctx.now_ms as i64],
                )?;
            }
            crate::outbox::enqueue_within(tx, ctx.client_id, &owned_job, ctx.now_ms)
        },
    )
}

pub fn list_inbound_messages_for_reprocess(
    conn: &Connection,
    client_id: &str,
    limit: usize,
    offset: usize,
) -> Result<Vec<InboundMessageRecord>, StoreError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT source_key, message_id, thread_id, internal_date_ms, from_addr, to_addr, subject, \
         body_excerpt, body_full, labels_json, headers_json, attachments_json, resolved_category, \
         matched_rule_id, ingested_at_ms, ai_triage_status, ai_triage_rationale, source_user_id \
         FROM email_inbound_messages \
         WHERE client_id = ?1 \
         ORDER BY COALESCE(internal_date_ms, ingested_at_ms) DESC, source_key DESC \
         LIMIT ?2 OFFSET ?3",
    )?;
    let rows = stmt.query_map(
        params![client_id, limit as i64, offset as i64],
        inbound_from_row,
    )?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

pub fn inbound_by_thread_id(
    conn: &Connection,
    client_id: &str,
    thread_id: &str,
    scope: &OperatorScope,
    limit: usize,
) -> Result<Vec<InboundMessageRecord>, StoreError> {
    let thread_id = thread_id.trim();
    if thread_id.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let (scope_pred, scope_all, scope_user) = scope.sql_filter("source_user_id", 4, 5);
    let mut stmt = conn.prepare(&format!(
        "SELECT source_key, message_id, thread_id, internal_date_ms, from_addr, to_addr, subject, \
         body_excerpt, body_full, labels_json, headers_json, attachments_json, resolved_category, \
         matched_rule_id, ingested_at_ms, ai_triage_status, ai_triage_rationale, source_user_id \
         FROM email_inbound_messages \
         WHERE client_id = ?1 AND thread_id = ?2 AND {scope_pred} \
         ORDER BY COALESCE(internal_date_ms, ingested_at_ms) ASC, source_key ASC \
         LIMIT ?3",
    ))?;
    let rows = stmt.query_map(
        params![client_id, thread_id, limit as i64, scope_all, scope_user],
        inbound_from_row,
    )?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

pub fn inbound_by_sender(
    conn: &Connection,
    client_id: &str,
    sender: &str,
    scope: &OperatorScope,
    limit: usize,
) -> Result<Vec<InboundMessageRecord>, StoreError> {
    let Some(sender) = super::subjects::first_normalized_email(Some(sender)) else {
        return Ok(Vec::new());
    };
    if limit == 0 {
        return Ok(Vec::new());
    }
    let display_sender_marker = format!("<{sender}>");
    let (scope_pred, scope_all, scope_user) = scope.sql_filter("source_user_id", 5, 6);
    let mut stmt = conn.prepare(&format!(
        "SELECT source_key, message_id, thread_id, internal_date_ms, from_addr, to_addr, subject, \
         body_excerpt, body_full, labels_json, headers_json, attachments_json, resolved_category, \
         matched_rule_id, ingested_at_ms, ai_triage_status, ai_triage_rationale, source_user_id \
         FROM email_inbound_messages \
         WHERE client_id = ?1 \
           AND (lower(COALESCE(from_addr, '')) = ?2 \
                OR instr(lower(COALESCE(from_addr, '')), ?4) > 0) \
           AND {scope_pred} \
         ORDER BY COALESCE(internal_date_ms, ingested_at_ms) DESC, source_key DESC \
         LIMIT ?3",
    ))?;
    let rows = stmt.query_map(
        params![
            client_id,
            sender,
            limit as i64,
            display_sender_marker,
            scope_all,
            scope_user
        ],
        inbound_from_row,
    )?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

/// All stored inbound messages for reclassification (bounded; the pilot
/// volume is far below this).
pub fn list_all_inbound(
    conn: &Connection,
    client_id: &str,
) -> Result<Vec<InboundMessageRecord>, StoreError> {
    list_recent_inbound(
        conn,
        client_id,
        10_000,
        &OperatorScope::All,
        &InboxFilter::default(),
    )
}

/// Update a stored message's classification (operator-triggered re-run of the
/// rules). Receipted with before/after category + rule.
pub fn update_classification(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    source_key: &str,
    before: (&str, Option<&str>),
    after: (&str, Option<&str>),
    now_ms: u64,
) -> Result<MutationOutcome, StoreError> {
    let idempotency_key = format!("reclassify:{source_key}:{now_ms}");
    let owned_client = client_id.to_string();
    let owned_source_key = source_key.to_string();
    let (after_category, after_rule) = (after.0.to_string(), after.1.map(str::to_string));
    let before_json = serde_json::json!({
        "resolved_category": before.0,
        "matched_rule_id": before.1,
    })
    .to_string();
    let after_json = serde_json::json!({
        "resolved_category": after.0,
        "matched_rule_id": after.1,
    })
    .to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: INBOUND_ENTITY_KIND,
            entity_id: source_key,
            change_kind: "reclassify",
            actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: Some(before_json),
            after_json: Some(after_json),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE email_inbound_messages                  SET resolved_category = ?3, matched_rule_id = ?4                  WHERE client_id = ?1 AND source_key = ?2",
                params![owned_client, owned_source_key, after_category, after_rule],
            )?;
            Ok(())
        },
    )
}

fn ai_gmail_scope_sql(labels_json_expr: &str, matched_rule_expr: &str) -> String {
    format!(
        "(p.category_id <> '{FALLBACK_CATEGORY_ID}' \
          OR {matched_rule_expr} IS NOT NULL \
          OR p.ai_suggestible_gmail_scope = 'all' \
          OR EXISTS (\
            SELECT 1 FROM json_each({labels_json_expr}) labels \
            JOIN json_each(p.ai_suggestible_gmail_categories_json) scope \
              ON (scope.value = 'primary' AND labels.value = 'CATEGORY_PERSONAL') \
              OR (scope.value = 'updates' AND labels.value = 'CATEGORY_UPDATES') \
              OR (scope.value = 'social' AND labels.value = 'CATEGORY_SOCIAL') \
              OR (scope.value = 'promotions' AND labels.value = 'CATEGORY_PROMOTIONS') \
              OR (scope.value = 'forums' AND labels.value = 'CATEGORY_FORUMS')))"
    )
}

/// Messages whose category policy allows AI-suggested work packets and whose
/// AI suggestion pass has not examined them yet. Deterministic packet kinds
/// are an additive baseline, not an AI eligibility requirement, so AI-only
/// categories belong in this batch too. Oldest first so the backlog drains
/// deterministically.
pub fn list_unexamined_ai_suggestible(
    conn: &Connection,
    client_id: &str,
    limit: usize,
) -> Result<Vec<InboundMessageRecord>, StoreError> {
    let scope_sql = ai_gmail_scope_sql("m.labels_json", "m.matched_rule_id");
    let sql = format!(
        "SELECT m.source_key, m.message_id, m.thread_id, m.internal_date_ms, m.from_addr, m.to_addr, \
            m.subject, m.body_excerpt, m.body_full, m.labels_json, m.headers_json, m.attachments_json, \
            m.resolved_category, m.matched_rule_id, m.ingested_at_ms, m.ai_triage_status, \
            m.ai_triage_rationale, m.source_user_id \
         FROM email_inbound_messages m \
         JOIN work_queue_policies p \
           ON p.client_id = m.client_id AND p.category_id = m.resolved_category \
         WHERE m.client_id = ?1 \
           AND m.ai_triage_status IS NULL \
           AND NOT EXISTS ( \
             SELECT 1 FROM json_each(m.labels_json) excluded_labels \
             WHERE excluded_labels.value IN ('SPAM', 'TRASH')) \
           AND p.create_work_item = 1 \
           AND p.ai_suggestible_packet_kinds_json <> '[]' \
           AND {scope_sql} \
         ORDER BY m.ingested_at_ms ASC, m.source_key ASC LIMIT ?2"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![client_id, limit as i64], |row| {
        Ok(InboundMessageRecord {
            source_key: row.get("source_key")?,
            message_id: row.get("message_id")?,
            thread_id: row.get("thread_id")?,
            internal_date_ms: row.get("internal_date_ms")?,
            from_addr: row.get("from_addr")?,
            to_addr: row.get("to_addr")?,
            subject: row.get("subject")?,
            body_excerpt: row.get("body_excerpt")?,
            body_full: row.get("body_full")?,
            labels: serde_json::from_str(&row.get::<_, String>("labels_json")?).unwrap_or_default(),
            headers: headers_from_row(row)?,
            attachments: attachments_from_row(row)?,
            resolved_category: row.get("resolved_category")?,
            matched_rule_id: row.get("matched_rule_id")?,
            ingested_at_ms: row.get::<_, i64>("ingested_at_ms")? as u64,
            ai_triage_status: row.get("ai_triage_status")?,
            ai_triage_rationale: row.get("ai_triage_rationale")?,
            source_user_id: row.get("source_user_id")?,
        })
    })?;
    let mut records = Vec::new();
    for row in rows {
        records.push(row?);
    }
    Ok(records)
}

/// Fallback-category messages no rule matched and the AI pass hasn't examined
/// yet. Kept for tests/diagnostics that need the original fallback-only shape.
pub fn list_untriaged_fallback(
    conn: &Connection,
    client_id: &str,
    fallback_category: &str,
    limit: usize,
) -> Result<Vec<InboundMessageRecord>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT source_key, message_id, thread_id, internal_date_ms, from_addr, to_addr, subject, \
         body_excerpt, body_full, labels_json, headers_json, attachments_json, resolved_category, \
         matched_rule_id, ingested_at_ms, ai_triage_status, ai_triage_rationale, source_user_id \
         FROM email_inbound_messages \
         WHERE client_id = ?1 AND resolved_category = ?2 AND matched_rule_id IS NULL \
           AND ai_triage_status IS NULL \
         ORDER BY ingested_at_ms ASC, source_key ASC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![client_id, fallback_category, limit as i64], |row| {
        Ok(InboundMessageRecord {
            source_key: row.get("source_key")?,
            message_id: row.get("message_id")?,
            thread_id: row.get("thread_id")?,
            internal_date_ms: row.get("internal_date_ms")?,
            from_addr: row.get("from_addr")?,
            to_addr: row.get("to_addr")?,
            subject: row.get("subject")?,
            body_excerpt: row.get("body_excerpt")?,
            body_full: row.get("body_full")?,
            labels: serde_json::from_str(&row.get::<_, String>("labels_json")?).unwrap_or_default(),
            headers: headers_from_row(row)?,
            attachments: attachments_from_row(row)?,
            resolved_category: row.get("resolved_category")?,
            matched_rule_id: row.get("matched_rule_id")?,
            ingested_at_ms: row.get::<_, i64>("ingested_at_ms")? as u64,
            ai_triage_status: row.get("ai_triage_status")?,
            ai_triage_rationale: row.get("ai_triage_rationale")?,
            source_user_id: row.get("source_user_id")?,
        })
    })?;
    let mut records = Vec::new();
    for row in rows {
        records.push(row?);
    }
    Ok(records)
}

/// Record the AI work-packet suggestion outcome on the message row (actor =
/// agent). The receipt carries the suggestion payload; the row only carries
/// status + rationale. Idempotency keys on (message, generation): one verdict
/// per generation, and an operator reset bumps the generation so a fresh
/// verdict can land.
pub fn set_ai_triage_result(
    conn: &mut Connection,
    client_id: &str,
    source_key: &str,
    status: &str,
    rationale: Option<&str>,
    suggestion_json: Option<String>,
    now_ms: u64,
) -> Result<MutationOutcome, StoreError> {
    let generation: i64 = conn.query_row(
        "SELECT ai_triage_generation FROM email_inbound_messages \
         WHERE client_id = ?1 AND source_key = ?2",
        params![client_id, source_key],
        |row| row.get(0),
    )?;
    let idempotency_key = format!("ai_triage:{source_key}:gen{generation}");
    let owned_client = client_id.to_string();
    let owned_source_key = source_key.to_string();
    let owned_status = status.to_string();
    let owned_rationale = rationale.map(str::to_string);
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: INBOUND_ENTITY_KIND,
            entity_id: source_key,
            change_kind: "ai_triage",
            actor_id: "ai_triage_pass",
            actor_kind: ActorKindDto::Agent,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: suggestion_json,
            now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE email_inbound_messages \
                 SET ai_triage_status = ?3, ai_triage_rationale = ?4, ai_triaged_at_ms = ?5 \
                 WHERE client_id = ?1 AND source_key = ?2",
                params![
                    owned_client,
                    owned_source_key,
                    owned_status,
                    owned_rationale,
                    now_ms as i64
                ],
            )?;
            Ok(())
        },
    )
}

/// Which already-examined messages an AI re-triage reset clears.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AiRetriageScope {
    /// One message, whatever its verdict.
    Message(String),
    /// `no_suggestion` older than the newest category change, plus errors.
    Stale,
    /// Every `no_suggestion` and `error` verdict.
    All,
}

/// Clear AI-triage verdicts so the pump re-examines the messages: status and
/// rationale go back to NULL and the generation bumps (which re-keys the next
/// verdict's idempotency). One batch mutation, one receipt carrying scope and
/// count. Returns how many messages were reset.
pub fn reset_ai_triage(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    scope: &AiRetriageScope,
    idempotency_key: &str,
    now_ms: u64,
) -> Result<u64, StoreError> {
    let scope_sql = ai_gmail_scope_sql(
        "email_inbound_messages.labels_json",
        "email_inbound_messages.matched_rule_id",
    );
    let (where_clause, scope_label, scope_param): (String, &str, Option<String>) = match scope {
        AiRetriageScope::Message(message_id) => (
            "source_key = ?2 AND ai_triage_status IS NOT NULL".to_string(),
            "message",
            Some(message_id.clone()),
        ),
        AiRetriageScope::Stale => (
            format!(
                "EXISTS (\
              SELECT 1 FROM work_queue_policies p \
              WHERE p.client_id = email_inbound_messages.client_id \
                AND p.category_id = email_inbound_messages.resolved_category \
                AND p.create_work_item = 1 \
                AND p.ai_suggestible_packet_kinds_json <> '[]' \
                AND {scope_sql}) \
             AND (\
               ai_triage_status = 'error' OR (\
                 ai_triage_status = 'no_suggestion' AND ai_triaged_at_ms < (\
                   SELECT MAX(updated_at_ms) FROM (\
                     SELECT COALESCE(MAX(updated_at_ms), 0) AS updated_at_ms \
                     FROM email_triage_categories \
                     WHERE client_id = ?2 AND deleted = 0 \
                     UNION ALL \
                     SELECT COALESCE(MAX(updated_at_ms), 0) AS updated_at_ms \
                     FROM work_queue_policies \
                     WHERE client_id = ?2))))"
            ),
            "stale",
            Some(client_id.to_string()),
        ),
        AiRetriageScope::All => (
            format!(
                "EXISTS (\
              SELECT 1 FROM work_queue_policies p \
              WHERE p.client_id = email_inbound_messages.client_id \
                AND p.category_id = email_inbound_messages.resolved_category \
                AND p.create_work_item = 1 \
                AND p.ai_suggestible_packet_kinds_json <> '[]' \
                AND {scope_sql}) \
             AND ai_triage_status IN ('no_suggestion', 'error')"
            ),
            "all",
            None,
        ),
    };
    let count_sql = format!(
        "SELECT COUNT(*) FROM email_inbound_messages WHERE client_id = ?1 AND {where_clause}"
    );
    let count: i64 = match &scope_param {
        Some(param) => conn.query_row(&count_sql, params![client_id, param], |row| row.get(0))?,
        None => conn.query_row(&count_sql, params![client_id], |row| row.get(0))?,
    };
    if count == 0 {
        return Ok(0);
    }
    let update_sql = format!(
        "UPDATE email_inbound_messages \
         SET ai_triage_status = NULL, ai_triage_rationale = NULL, ai_triaged_at_ms = NULL, \
             ai_triage_generation = ai_triage_generation + 1 \
         WHERE client_id = ?1 AND {where_clause}"
    );
    let after_json = serde_json::json!({
        "scope": scope_label,
        "reset": count,
    })
    .to_string();
    let owned_client = client_id.to_string();
    let owned_param = scope_param.clone();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: "email_ai_triage_reset",
            entity_id: idempotency_key,
            change_kind: "reset",
            actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: None,
            idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(after_json),
            now_ms,
        },
        move |tx| {
            match &owned_param {
                Some(param) => tx.execute(&update_sql, params![owned_client, param])?,
                None => tx.execute(&update_sql, params![owned_client])?,
            };
            Ok(())
        },
    )?;
    Ok(count as u64)
}

/// Inbound messages in one category within an epoch-ms window (start
/// inclusive, end exclusive; message time = Gmail internal date, falling
/// back to ingest time). The owner digest's call-volume read.
pub fn count_inbound_in_category_between(
    conn: &Connection,
    client_id: &str,
    category: &str,
    start_ms: u64,
    end_ms: u64,
) -> Result<u64, StoreError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM email_inbound_messages \
         WHERE client_id = ?1 AND resolved_category = ?2 \
           AND COALESCE(internal_date_ms, ingested_at_ms) >= ?3 \
           AND COALESCE(internal_date_ms, ingested_at_ms) < ?4",
        params![client_id, category, start_ms as i64, end_ms as i64],
        |row| row.get(0),
    )?;
    Ok(count as u64)
}

pub fn list_inbound_in_category_between(
    conn: &Connection,
    client_id: &str,
    category: &str,
    start_ms: u64,
    end_ms: u64,
) -> Result<Vec<InboundMessageRecord>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT source_key, message_id, thread_id, internal_date_ms, from_addr, to_addr, subject, \
         body_excerpt, body_full, labels_json, headers_json, attachments_json, resolved_category, \
         matched_rule_id, ingested_at_ms, ai_triage_status, ai_triage_rationale, source_user_id \
         FROM email_inbound_messages \
         WHERE client_id = ?1 AND resolved_category = ?2 \
           AND COALESCE(internal_date_ms, ingested_at_ms) >= ?3 \
           AND COALESCE(internal_date_ms, ingested_at_ms) < ?4 \
         ORDER BY COALESCE(internal_date_ms, ingested_at_ms) DESC, source_key DESC",
    )?;
    let rows = stmt.query_map(
        params![client_id, category, start_ms as i64, end_ms as i64],
        inbound_from_row,
    )?;
    let mut records = Vec::new();
    for row in rows {
        records.push(row?);
    }
    Ok(records)
}
