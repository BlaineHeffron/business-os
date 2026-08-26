//! Client overlay loader: the per-client deployment profile (TOML directory,
//! `BOS_CLIENT_OVERLAY_DIR`). Ported MINIMALLY from agent-monitor-rust's
//! ClientDeploymentProfile: identity, enabled slices, and category/rule/
//! policy seeds. Everything else (themes, secret refs, provider configs)
//! arrives with the feature that needs it.
//!
//! Posture:
//! - No overlay dir = the built-in dev profile (all slices, no seeds).
//! - A present-but-broken overlay FAILS STARTUP — a client instance must
//!   never silently run with the wrong identity or seeds.
//! - Seeds are upserted through the slice stores at startup. Idempotency
//!   keys include a content hash, so unchanged seeds replay quietly and an
//!   edited seed applies exactly once per content version.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use bos_contracts::call_inputs::{CallInputSourceConfig, CallInputsRouting};
use bos_contracts::email_triage::{CategoryRecord, EmailTriageGmailCategory, EmailTriageRule};
use bos_contracts::lead_discovery::{
    LeadDiscoveryCriteria, LeadDiscoverySourceConfig, LeadDiscoverySourceKind,
};
use bos_contracts::work_queue::WorkQueuePolicy;
use rusqlite::Connection;
use serde::Deserialize;

use crate::env_registry;
use crate::store_core::{self, StoreError};

pub const OVERLAY_FILE: &str = "client.toml";
const SEED_ACTOR: &str = "client_overlay_loader";

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ClientOverlay {
    pub schema_version: u32,
    pub identity: OverlayIdentity,
    #[serde(default)]
    pub slices: OverlaySlices,
    /// Category seeds (upserted before rules so rule validation sees them).
    #[serde(default)]
    pub categories: Vec<CategoryRecord>,
    /// Deterministic triage rule seeds.
    #[serde(default)]
    pub rules: Vec<EmailTriageRule>,
    /// Email triage dashboard defaults. Ingestion remains controlled by env;
    /// these settings only shape the operator's inbox view.
    #[serde(default)]
    pub email_triage: EmailTriageOverlay,
    /// Per-category work-queue policy seeds.
    #[serde(default)]
    pub policies: Vec<WorkQueuePolicy>,
    /// Queue routing policy for shared operational inboxes.
    #[serde(default)]
    pub work_queue: WorkQueueOverlay,
    /// Drive RAG corpus pointer defaults (drive_corpus slice). Env vars
    /// (BOS_DRIVE_CORPUS_*) override these per field when set.
    #[serde(default)]
    pub drive_corpus: Option<DriveCorpusOverlay>,
    /// Google Search Console traffic source defaults. Env vars
    /// (BOS_SEARCH_CONSOLE_*) override these per field when set.
    #[serde(default)]
    pub search_console: Option<SearchConsoleOverlay>,
    /// Company background + owner voice, seeded into `client_profile` and used
    /// to ground outward-facing LLM tasks (tone/context only).
    #[serde(default)]
    pub company_profile: Option<CompanyProfileOverlay>,
    /// Quote-workflow profile selection and profile-specific config. The host
    /// validates the selected profile at startup; profile crates never read
    /// overlays directly.
    #[serde(default)]
    pub quote_workflows: QuoteWorkflowsOverlay,
    /// Owner-report cadence, recipient, presentation, and call-volume defaults.
    /// Env vars (BOS_REPORT_DIGEST_* and BOS_OWNER_REPORT_CALL_VOLUME_*) override
    /// these per field when set.
    #[serde(default)]
    pub owner_reports: Option<OwnerReportsOverlay>,
    /// Accounting view visibility. Env BOS_ACCOUNTING_VISIBILITY_POLICY
    /// overrides this deployment default.
    #[serde(default)]
    pub accounting: AccountingOverlay,
    /// Approved-source lead discovery config. Empty sources = explicitly
    /// pending; the slice exposes that state but performs no discovery.
    #[serde(default)]
    pub lead_discovery: LeadDiscoveryOverlay,
    /// Consent/fit-gated call input sources. Empty sources = explicitly
    /// pending; the slice exposes that state but stages nothing.
    #[serde(default)]
    pub call_inputs: CallInputsOverlay,
    /// Customer tier sync defaults. Env BOS_SHOPIFY_TIER_MAPPING_JSON
    /// overrides these mappings when set.
    #[serde(default)]
    pub customer_tier_sync: CustomerTierSyncOverlay,
}

/// Company-background seed. All fields optional; blanks store as NULL and
/// ground nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct CompanyProfileOverlay {
    #[serde(default)]
    pub company_name: String,
    #[serde(default)]
    pub bio: String,
    #[serde(default)]
    pub industry: String,
    #[serde(default)]
    pub website: String,
    /// Owner/operator voice line outward-facing drafts speak in.
    #[serde(default)]
    pub persona: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct DriveCorpusOverlay {
    /// Folder ids whose direct children form the corpus.
    #[serde(default)]
    pub folder_ids: Vec<String>,
    #[serde(default)]
    pub include_file_ids: Vec<String>,
    #[serde(default)]
    pub exclude_file_ids: Vec<String>,
    /// Case-insensitive name patterns (`*` wildcard).
    #[serde(default)]
    pub exclude_name_patterns: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct SearchConsoleOverlay {
    /// Search Console property id/url, e.g. sc-domain:example.com or
    /// https://www.example.com/.
    #[serde(default)]
    pub property_url: String,
    /// Case-insensitive query patterns; `*` anchors like shell globs. These
    /// define branded/non-branded cuts per client.
    #[serde(default)]
    pub branded_query_patterns: Vec<String>,
    /// Operator user whose Google credential should read this property.
    #[serde(default)]
    pub user_id: String,
    /// Recent finalized days to refresh each cycle.
    #[serde(default)]
    pub sync_days: Option<u32>,
    /// Numeric GA4 property id for behavior/acquisition/conversion reporting.
    /// Search Console domain/url properties are separate from GA4 properties.
    #[serde(default)]
    pub ga4_property_id: String,
    /// Extra referrer-spam domains excluded from GA4 reporting views in
    /// addition to the vendored community list. Raw GA4 snapshots are kept.
    #[serde(default)]
    pub analytics_excluded_referrer_domains: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct OwnerReportsOverlay {
    /// Operator user ids allowed to view, generate, and email owner reports.
    /// Empty means any authenticated operator can use the owner-report surface.
    #[serde(default)]
    pub allowed_operator_user_ids: Vec<String>,
    /// Explicit scheduled-delivery gate. Generation can run without delivery.
    #[serde(default)]
    pub delivery_enabled: bool,
    /// Owner recipients for the Gmail draft.
    #[serde(default)]
    pub recipients: Vec<String>,
    /// Recipient-specific presentation rules. Use this for owners who should
    /// receive operating metrics but not financial metrics.
    #[serde(default)]
    pub recipient_profiles: Vec<OwnerReportRecipientOverlay>,
    /// Monday, Tuesday, ...; unset disables weekly scheduled delivery.
    #[serde(default)]
    pub weekly_weekday: Option<String>,
    /// 1-31; unset disables MTD scheduled delivery.
    #[serde(default)]
    pub mtd_day: Option<u8>,
    /// Ordered metric ids for the email body. Empty = slice default.
    #[serde(default)]
    pub metrics: Vec<String>,
    /// Subject prefix before the generated period title.
    #[serde(default)]
    pub subject_prefix: Option<String>,
    #[serde(default)]
    pub call_volume: CallVolumeMetricOverlay,
    /// Client report-assembly profile id (e.g. how parser-owned call reason
    /// codes are bucketed for the call-volume KPI). Unset = generic assembly
    /// only. Must name a profile available in this build.
    #[serde(default)]
    pub report_profile: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct OwnerReportRecipientOverlay {
    /// Addresses this profile applies to.
    #[serde(default)]
    pub recipients: Vec<String>,
    /// Ordered metric ids for these recipients. Empty = default metrics.
    #[serde(default)]
    pub metrics: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct CallVolumeMetricOverlay {
    /// Email triage category whose inbound messages count as call summaries.
    #[serde(default)]
    pub category_id: String,
    /// Operator-facing KPI label.
    #[serde(default)]
    pub label: String,
    /// Coverage wording, e.g. whether the summaries represent all calls.
    #[serde(default)]
    pub source_label: String,
    /// Gmail label name used by the deployment to identify this source.
    #[serde(default)]
    pub gmail_label: String,
    /// Gmail query/source selector. Stored for operator honesty; ingestion still
    /// uses the email_triage slice's normal Gmail ingest query.
    #[serde(default)]
    pub gmail_query: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct EmailTriageOverlay {
    #[serde(default)]
    pub inbound_parser_ids: Vec<String>,
    #[serde(default)]
    pub inbox_defaults: Vec<EmailTriageInboxDefaultOverlay>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct EmailTriageInboxDefaultOverlay {
    /// Empty user_id applies to the shared/all-scope operator.
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub categories: Vec<EmailTriageGmailCategory>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub source_user_id: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct WorkQueueOverlay {
    #[serde(default)]
    pub shared_inboxes: std::collections::BTreeMap<String, SharedInboxOverlay>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct SharedInboxOverlay {
    #[serde(default)]
    pub match_to: Vec<String>,
    #[serde(default)]
    pub visible_to_user_ids: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct AccountingOverlay {
    #[serde(default)]
    pub visibility_policy: Option<AccountingVisibilityPolicy>,
    #[serde(default)]
    pub metric_basis: AccountingMetricBasisOverlay,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct AccountingMetricBasisOverlay {
    /// gross_margin | adjusted_gross_sales | invoice_totals. Empty = default.
    #[serde(default)]
    pub basis: String,
    /// Operator-facing label. Empty = basis default.
    #[serde(default)]
    pub label: String,
    /// Optional imported monthly baseline for the configured basis, in cents.
    #[serde(default)]
    pub baseline_cents: Option<i64>,
    /// Imported current-period deductions for adjusted_gross_sales, in cents.
    /// These are generic deployment inputs; client-specific values live in
    /// a private overlay repository, not BusinessOS.
    #[serde(default)]
    pub freight_cents: Option<i64>,
    #[serde(default)]
    pub taxes_cents: Option<i64>,
    #[serde(default)]
    pub insurance_cents: Option<i64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountingVisibilityPolicy {
    AuthorizerOnly,
    AdminOnly,
    #[default]
    Shared,
}

impl AccountingVisibilityPolicy {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "authorizer_only" => Some(Self::AuthorizerOnly),
            "admin_only" => Some(Self::AdminOnly),
            "shared" => Some(Self::Shared),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::AuthorizerOnly => "authorizer_only",
            Self::AdminOnly => "admin_only",
            Self::Shared => "shared",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct QuoteWorkflowsOverlay {
    #[serde(default = "default_quote_profile")]
    pub profile: String,
    #[serde(default)]
    pub config: Option<toml::Value>,
    #[serde(default)]
    pub guardrails: Option<toml::Value>,
}

impl Default for QuoteWorkflowsOverlay {
    fn default() -> Self {
        Self {
            profile: default_quote_profile(),
            config: None,
            guardrails: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct LeadDiscoveryOverlay {
    #[serde(default)]
    pub sources: Vec<LeadDiscoverySourceConfig>,
    #[serde(default)]
    pub criteria: LeadDiscoveryCriteria,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct CallInputsOverlay {
    #[serde(default)]
    pub sources: Vec<CallInputSourceConfig>,
    #[serde(default)]
    pub routing: CallInputsRouting,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct CustomerTierSyncOverlay {
    #[serde(default)]
    pub copy_qbo_tier: bool,
    #[serde(default)]
    pub metafield_namespace: Option<String>,
    #[serde(default)]
    pub metafield_key: Option<String>,
    #[serde(default)]
    pub write_tag: bool,
    #[serde(default)]
    pub tag_prefix: Option<String>,
    #[serde(default)]
    pub tier_mappings: BTreeMap<String, CustomerTierSyncTargetOverlay>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct CustomerTierSyncTargetOverlay {
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub metafield_namespace: Option<String>,
    #[serde(default)]
    pub metafield_key: Option<String>,
    #[serde(default)]
    pub metafield_value: Option<String>,
    #[serde(default)]
    pub segment_query: Option<String>,
}

impl CustomerTierSyncTargetOverlay {
    fn has_target(&self) -> bool {
        self.tag
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
            || (self
                .metafield_namespace
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
                && self
                    .metafield_key
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty())
                && self
                    .metafield_value
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct OverlayIdentity {
    pub client_id: String,
    pub display_name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct OverlaySlices {
    /// Slice ids to enable. Empty/omitted = all slices (dev posture).
    #[serde(default)]
    pub enabled: Vec<String>,
}

#[derive(Debug)]
pub enum OverlayError {
    Io(String),
    Parse(String),
    Invalid(String),
    Seed(String),
}

impl std::fmt::Display for OverlayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(msg) => write!(f, "overlay io error: {msg}"),
            Self::Parse(msg) => write!(f, "overlay parse error: {msg}"),
            Self::Invalid(msg) => write!(f, "overlay invalid: {msg}"),
            Self::Seed(msg) => write!(f, "overlay seed error: {msg}"),
        }
    }
}

impl std::error::Error for OverlayError {}

/// Load the overlay named by BOS_CLIENT_OVERLAY_DIR. `Ok(None)` = unset (dev
/// profile); a set-but-unloadable overlay is an error the caller treats as
/// fatal at startup.
pub fn load_from_env() -> Result<Option<ClientOverlay>, OverlayError> {
    match env_registry::string(&env_registry::BOS_CLIENT_OVERLAY_DIR) {
        Some(dir) if !dir.trim().is_empty() => load_from_dir(Path::new(dir.trim())).map(Some),
        _ => Ok(None),
    }
}

pub fn load_from_dir(dir: &Path) -> Result<ClientOverlay, OverlayError> {
    let path = dir.join(OVERLAY_FILE);
    let raw = std::fs::read_to_string(&path)
        .map_err(|err| OverlayError::Io(format!("{}: {err}", path.display())))?;
    parse(&raw)
}

pub fn parse(raw: &str) -> Result<ClientOverlay, OverlayError> {
    let overlay: ClientOverlay =
        toml::from_str(raw).map_err(|err| OverlayError::Parse(err.to_string()))?;
    validate(&overlay)?;
    Ok(overlay)
}

fn validate(overlay: &ClientOverlay) -> Result<(), OverlayError> {
    if overlay.schema_version != 1 {
        return Err(OverlayError::Invalid(format!(
            "unsupported schema_version {}",
            overlay.schema_version
        )));
    }
    if overlay.identity.client_id.trim().is_empty() {
        return Err(OverlayError::Invalid("identity.client_id is empty".into()));
    }
    let known: BTreeSet<&str> = crate::slices::registry().iter().map(|s| s.id).collect();
    for slice_id in &overlay.slices.enabled {
        if !known.contains(slice_id.as_str()) {
            return Err(OverlayError::Invalid(format!(
                "slices.enabled names unknown slice '{slice_id}' (known: {})",
                known.iter().copied().collect::<Vec<_>>().join(", ")
            )));
        }
    }
    for rule in &overlay.rules {
        rule.validate()
            .map_err(|err| OverlayError::Invalid(format!("rule '{}': {err:?}", rule.rule_id)))?;
    }
    for policy in &overlay.policies {
        for kind in &policy.packet_kinds {
            if !crate::slices::work_queue::packet_kind_exists(kind) {
                return Err(OverlayError::Invalid(format!(
                    "policy '{}' names unknown packet kind '{kind}'",
                    policy.category_id
                )));
            }
        }
    }
    validate_email_triage(&overlay.email_triage)?;
    validate_owner_reports(overlay.owner_reports.as_ref())?;
    validate_work_queue(&overlay.work_queue)?;
    validate_lead_discovery(&overlay.lead_discovery)?;
    validate_call_inputs(&overlay.call_inputs)?;
    validate_customer_tier_sync(&overlay.customer_tier_sync)?;
    let profile_id = overlay.quote_workflows.profile.trim();
    if profile_id.is_empty() {
        return Err(OverlayError::Invalid(
            "quote_workflows.profile is empty".into(),
        ));
    }
    crate::slices::quote_workflows::profiles::validate_profile_config(
        profile_id,
        quote_profile_config_json(&overlay.quote_workflows),
    )
    .map_err(OverlayError::Invalid)?;
    crate::slices::quote_workflows::service::validate_guardrail_config_json(
        quote_guardrail_config_json(&overlay.quote_workflows),
    )
    .map_err(OverlayError::Invalid)?;
    Ok(())
}

fn validate_email_triage(email_triage: &EmailTriageOverlay) -> Result<(), OverlayError> {
    let mut seen = BTreeSet::new();
    for parser_id in &email_triage.inbound_parser_ids {
        let parser_id = parser_id.trim();
        if parser_id.is_empty() {
            return Err(OverlayError::Invalid(
                "email_triage.inbound_parser_ids contains an empty parser id".into(),
            ));
        }
        if !seen.insert(parser_id.to_string()) {
            return Err(OverlayError::Invalid(format!(
                "email_triage inbound parser id '{parser_id}' is duplicated"
            )));
        }
        if !crate::slices::email_triage::service::inbound_parser_exists(parser_id) {
            return Err(OverlayError::Invalid(format!(
                "email_triage inbound parser id '{parser_id}' is not available in this build"
            )));
        }
    }
    Ok(())
}

fn validate_owner_reports(owner_reports: Option<&OwnerReportsOverlay>) -> Result<(), OverlayError> {
    let Some(profile_id) = owner_reports
        .and_then(|overlay| overlay.report_profile.as_deref())
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
    else {
        return Ok(());
    };
    if !crate::slices::owner_reports::service::report_profile_exists(profile_id) {
        return Err(OverlayError::Invalid(format!(
            "owner_reports.report_profile '{profile_id}' is not available in this build"
        )));
    }
    Ok(())
}

fn validate_work_queue(work_queue: &WorkQueueOverlay) -> Result<(), OverlayError> {
    for (id, shared) in &work_queue.shared_inboxes {
        if id.trim().is_empty() {
            return Err(OverlayError::Invalid(
                "work_queue.shared_inboxes key is empty".into(),
            ));
        }
        if shared.match_to.iter().all(|value| value.trim().is_empty()) {
            return Err(OverlayError::Invalid(format!(
                "work_queue.shared_inboxes.{id} has no match_to addresses"
            )));
        }
        if shared
            .visible_to_user_ids
            .iter()
            .all(|value| value.trim().is_empty())
        {
            return Err(OverlayError::Invalid(format!(
                "work_queue.shared_inboxes.{id} has no visible_to_user_ids"
            )));
        }
    }
    Ok(())
}

fn validate_lead_discovery(lead: &LeadDiscoveryOverlay) -> Result<(), OverlayError> {
    let mut seen = BTreeSet::new();
    for source in &lead.sources {
        let id = source.source_id.trim();
        if id.is_empty() {
            return Err(OverlayError::Invalid(
                "lead_discovery source_id is empty".into(),
            ));
        }
        if !seen.insert(id.to_string()) {
            return Err(OverlayError::Invalid(format!(
                "lead_discovery source_id '{id}' is duplicated"
            )));
        }
        if source.display_name.trim().is_empty() {
            return Err(OverlayError::Invalid(format!(
                "lead_discovery source '{id}' display_name is empty"
            )));
        }
        if source.enabled && !source.approved {
            return Err(OverlayError::Invalid(format!(
                "lead_discovery source '{id}' is enabled but not approved"
            )));
        }
        if source.auto_poll {
            if matches!(source.kind, LeadDiscoverySourceKind::FacebookGroup) {
                return Err(OverlayError::Invalid(format!(
                    "lead_discovery source '{id}' facebook_group cannot auto_poll"
                )));
            }
            if !source.approved {
                return Err(OverlayError::Invalid(format!(
                    "lead_discovery source '{id}' auto_poll requires approval"
                )));
            }
            let feed = source
                .feed_url
                .as_deref()
                .or(source.url.as_deref())
                .map(str::trim)
                .unwrap_or("");
            if feed.is_empty() {
                return Err(OverlayError::Invalid(format!(
                    "lead_discovery source '{id}' auto_poll requires a feed_url or url"
                )));
            }
        }
        if matches!(
            source.kind,
            LeadDiscoverySourceKind::Forum
                | LeadDiscoverySourceKind::Reddit
                | LeadDiscoverySourceKind::FacebookGroup
        ) && source
            .url
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
        {
            return Err(OverlayError::Invalid(format!(
                "lead_discovery source '{id}' needs a url"
            )));
        }
    }
    for kind in &lead.criteria.routing_packet_kinds {
        if !crate::slices::work_queue::packet_kind_exists(kind) {
            return Err(OverlayError::Invalid(format!(
                "lead_discovery routing names unknown packet kind '{kind}'"
            )));
        }
    }
    Ok(())
}

fn validate_call_inputs(call_inputs: &CallInputsOverlay) -> Result<(), OverlayError> {
    let mut seen = BTreeSet::new();
    for source in &call_inputs.sources {
        let id = source.source_id.trim();
        if id.is_empty() {
            return Err(OverlayError::Invalid(
                "call_inputs source_id is empty".into(),
            ));
        }
        if !seen.insert(id.to_string()) {
            return Err(OverlayError::Invalid(format!(
                "call_inputs source_id '{id}' is duplicated"
            )));
        }
        if source.display_name.trim().is_empty() {
            return Err(OverlayError::Invalid(format!(
                "call_inputs source '{id}' display_name is empty"
            )));
        }
        if source.enabled
            && source
                .consent_basis
                .as_deref()
                .map(str::trim)
                .unwrap_or_default()
                .is_empty()
        {
            return Err(OverlayError::Invalid(format!(
                "call_inputs source '{id}' is enabled without a recorded consent_basis"
            )));
        }
    }
    for kind in &call_inputs.routing.packet_kinds {
        if !crate::slices::work_queue::packet_kind_exists(kind) {
            return Err(OverlayError::Invalid(format!(
                "call_inputs routing names unknown packet kind '{kind}'"
            )));
        }
    }
    Ok(())
}

fn validate_customer_tier_sync(config: &CustomerTierSyncOverlay) -> Result<(), OverlayError> {
    if config.copy_qbo_tier {
        let has_metafield_target = config
            .metafield_namespace
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
            && config
                .metafield_key
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty());
        if !has_metafield_target && !config.write_tag {
            return Err(OverlayError::Invalid(
                "customer_tier_sync copy_qbo_tier needs a metafield target or write_tag=true"
                    .into(),
            ));
        }
    }
    for (tier, target) in &config.tier_mappings {
        if tier.trim().is_empty() {
            return Err(OverlayError::Invalid(
                "customer_tier_sync.tier_mappings key is empty".into(),
            ));
        }
        if !target.has_target() {
            return Err(OverlayError::Invalid(format!(
                "customer_tier_sync tier '{tier}' needs a tag or complete metafield target"
            )));
        }
    }
    Ok(())
}

/// True when `slice_id` should run under this (possibly absent) overlay.
/// No overlay, or an overlay with an empty enabled list, enables everything.
pub fn slice_enabled(overlay: Option<&ClientOverlay>, slice_id: &str) -> bool {
    match overlay {
        Some(overlay) if !overlay.slices.enabled.is_empty() => overlay
            .slices
            .enabled
            .iter()
            .any(|enabled| enabled == slice_id),
        _ => true,
    }
}

/// Upsert the overlay's seeds through the slice stores (receipted). Content-
/// hashed idempotency keys keep unchanged seeds replay-quiet across boots.
/// Order matters: categories before rules (rule upsert validates its pinned
/// category) before policies.
pub fn apply_seeds(
    conn: &mut Connection,
    overlay: &ClientOverlay,
    now_ms: u64,
) -> Result<(), OverlayError> {
    let client_id = overlay.identity.client_id.as_str();
    let seed_err = |what: &str, err: StoreError| OverlayError::Seed(format!("{what}: {err}"));

    // Ensure system defaults (the fallback category) exist first.
    crate::slices::email_triage::store::list_categories(conn, client_id, now_ms)
        .map_err(|err| seed_err("seed system categories", err))?;

    retire_call_log_if_seed_owned(conn, client_id, now_ms)
        .map_err(|err| seed_err("retire call_log", err))?;

    for category in &overlay.categories {
        if seed_should_skip_existing(
            conn,
            client_id,
            crate::slices::email_triage::store::CATEGORY_ENTITY_KIND,
            &category.category_id,
        )
        .map_err(|err| seed_err(&format!("category '{}'", category.category_id), err))?
        {
            continue;
        }
        let key = seed_key("category", &category.category_id, category);
        crate::slices::email_triage::store::upsert_category(
            conn, client_id, SEED_ACTOR, category, &key, now_ms,
        )
        .map_err(|err| seed_err(&format!("category '{}'", category.category_id), err))?;
    }
    for rule in &overlay.rules {
        if seed_should_skip_existing(
            conn,
            client_id,
            crate::slices::email_triage::store::ENTITY_KIND,
            &rule.rule_id,
        )
        .map_err(|err| seed_err(&format!("rule '{}'", rule.rule_id), err))?
        {
            continue;
        }
        let key = seed_key("rule", &rule.rule_id, rule);
        crate::slices::email_triage::store::upsert(
            conn,
            crate::slices::email_triage::store::RuleMutationContext {
                client_id,
                actor_id: SEED_ACTOR,
                expected_revision: None,
                idempotency_key: &key,
                correlation_id: None,
                now_ms,
            },
            rule,
        )
        .map_err(|err| seed_err(&format!("rule '{}'", rule.rule_id), err))?;
    }
    for policy in &overlay.policies {
        if seed_should_skip_existing(
            conn,
            client_id,
            crate::slices::work_queue::store::POLICY_ENTITY_KIND,
            &policy.category_id,
        )
        .map_err(|err| seed_err(&format!("policy '{}'", policy.category_id), err))?
        {
            continue;
        }
        let key = seed_key("policy", &policy.category_id, policy);
        crate::slices::work_queue::store::upsert_policy(
            conn, client_id, SEED_ACTOR, policy, &key, now_ms,
        )
        .map_err(|err| seed_err(&format!("policy '{}'", policy.category_id), err))?;
    }
    if let Some(company) = &overlay.company_profile {
        let to_opt = |s: &str| {
            let s = s.trim();
            (!s.is_empty()).then(|| s.to_string())
        };
        let profile = bos_contracts::client_profile::ClientProfile {
            client_id: client_id.to_string(),
            company_name: to_opt(&company.company_name),
            bio: to_opt(&company.bio),
            industry: to_opt(&company.industry),
            website: to_opt(&company.website),
            persona: to_opt(&company.persona),
        };
        let key = seed_key("client_profile", client_id, &profile);
        crate::slices::client_profile::store::upsert_profile(
            conn, client_id, SEED_ACTOR, &profile, &key, now_ms,
        )
        .map_err(|err| seed_err("client profile", err))?;
    }
    Ok(())
}

fn seed_should_skip_existing(
    conn: &Connection,
    client_id: &str,
    entity_kind: &str,
    entity_id: &str,
) -> Result<bool, StoreError> {
    Ok(
        store_core::latest_applied_receipt_actor(conn, client_id, entity_kind, entity_id)?
            .is_some_and(|actor| !is_seed_actor(&actor)),
    )
}

fn is_seed_actor(actor_id: &str) -> bool {
    matches!(actor_id, SEED_ACTOR | "system" | "system_seed")
}

fn retire_call_log_if_seed_owned(
    conn: &mut Connection,
    client_id: &str,
    now_ms: u64,
) -> Result<(), StoreError> {
    if !crate::slices::email_triage::store::category_is_active(conn, client_id, "call_log")? {
        return Ok(());
    }
    if seed_should_skip_existing(
        conn,
        client_id,
        crate::slices::email_triage::store::CATEGORY_ENTITY_KIND,
        "call_log",
    )? {
        return Ok(());
    }

    let rules = crate::slices::email_triage::store::list_active(conn, client_id)?;
    for rule in rules
        .iter()
        .filter(|stored| stored.rule.pinned_category == "call_log")
    {
        if seed_should_skip_existing(
            conn,
            client_id,
            crate::slices::email_triage::store::ENTITY_KIND,
            &rule.rule.rule_id,
        )? {
            continue;
        }
        let key = format!("overlay:retire:rule:{}", rule.rule.rule_id);
        crate::slices::email_triage::store::apply_action(
            conn,
            crate::slices::email_triage::store::RuleMutationContext {
                client_id,
                actor_id: SEED_ACTOR,
                expected_revision: None,
                idempotency_key: &key,
                correlation_id: None,
                now_ms,
            },
            &rule.rule.rule_id,
            crate::slices::email_triage::store::RuleAction::Delete,
        )?;
    }

    let remaining_operator_pin = crate::slices::email_triage::store::list_active(conn, client_id)?
        .into_iter()
        .any(|stored| stored.rule.pinned_category == "call_log");
    if remaining_operator_pin {
        return Ok(());
    }

    let key = "overlay:retire:category:call_log";
    crate::slices::email_triage::store::delete_category(
        conn, client_id, SEED_ACTOR, "call_log", key, now_ms,
    )?;
    Ok(())
}

/// "overlay:<kind>:<id>:<sha256 of the serialized seed, truncated>" — stable
/// across boots, changes exactly when the seed's content changes.
fn seed_key<T: serde::Serialize>(kind: &str, id: &str, seed: &T) -> String {
    use sha2::{Digest, Sha256};
    let serialized = serde_json::to_string(seed).unwrap_or_default();
    let digest = Sha256::digest(serialized.as_bytes());
    let mut hash = String::with_capacity(16);
    for byte in &digest[..8] {
        hash.push_str(&format!("{byte:02x}"));
    }
    format!("overlay:{kind}:{id}:{hash}")
}

fn default_quote_profile() -> String {
    crate::slices::quote_workflows::profiles::BUILT_IN_PROFILE_ID.to_string()
}

pub fn quote_profile_config_json(overlay: &QuoteWorkflowsOverlay) -> serde_json::Value {
    overlay
        .config
        .as_ref()
        .map(toml_to_json)
        .unwrap_or(serde_json::Value::Null)
}

pub fn quote_guardrail_config_json(overlay: &QuoteWorkflowsOverlay) -> serde_json::Value {
    overlay
        .guardrails
        .as_ref()
        .map(toml_to_json)
        .unwrap_or(serde_json::Value::Null)
}

fn toml_to_json(value: &toml::Value) -> serde_json::Value {
    match value {
        toml::Value::String(value) => serde_json::Value::String(value.clone()),
        toml::Value::Integer(value) => serde_json::Value::Number((*value).into()),
        toml::Value::Float(value) => serde_json::Number::from_f64(*value)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        toml::Value::Boolean(value) => serde_json::Value::Bool(*value),
        toml::Value::Datetime(value) => serde_json::Value::String(value.to_string()),
        toml::Value::Array(values) => {
            serde_json::Value::Array(values.iter().map(toml_to_json).collect())
        }
        toml::Value::Table(table) => serde_json::Value::Object(
            table
                .iter()
                .map(|(key, value)| (key.clone(), toml_to_json(value)))
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::Persistence;

    const DEMO_MINIMAL: &str = r##"
schema_version = 1

[identity]
client_id = "Example Company"
display_name = "Example Company"

[slices]
enabled = ["ai_usage", "email_triage", "google_connector", "work_queue", "follow_up_tasks"]

[[categories]]
category_id = "call_summary"
display_name = "Call summary"
description = "Call summary emails from an answering service: caller name, phone, message, and whether a callback was requested."
color = "#ec4899"
sort = 10
is_system = false

[[rules]]
rule_id = "call_summary_from_addr"
pinned_category = "call_summary"
match_mode = "all"
priority = 10

[[rules.conditions]]
field = "from"
op = "contains"
value = "noreply@example.test"

[[policies]]
category_id = "call_summary"
create_work_item = true
packet_kinds = ["follow_up_task", "calendar_event_draft"]
"##;

    #[test]
    fn parses_and_validates_a_minimal_overlay() {
        let overlay = parse(DEMO_MINIMAL).expect("parse");
        assert_eq!(overlay.identity.client_id, "Example Company");
        assert_eq!(
            overlay.quote_workflows.profile,
            crate::slices::quote_workflows::profiles::BUILT_IN_PROFILE_ID
        );
        assert_eq!(overlay.slices.enabled.len(), 5);
        assert_eq!(overlay.categories[0].category_id, "call_summary");
        assert_eq!(overlay.rules[0].rule_id, "call_summary_from_addr");
        assert!(overlay.rules[0].enabled, "enabled defaults true");
        assert_eq!(overlay.policies[0].packet_kinds.len(), 2);
    }

    #[test]
    fn rejects_unknown_slice_packet_kind_and_bad_schema() {
        let unknown_slice = DEMO_MINIMAL.replace("\"work_queue\"", "\"warp_drive\"");
        assert!(matches!(
            parse(&unknown_slice),
            Err(OverlayError::Invalid(_))
        ));

        let unknown_kind = DEMO_MINIMAL.replace("follow_up_task\",", "teleport\",");
        assert!(matches!(
            parse(&unknown_kind),
            Err(OverlayError::Invalid(_))
        ));

        let bad_version = DEMO_MINIMAL.replace("schema_version = 1", "schema_version = 9");
        assert!(matches!(parse(&bad_version), Err(OverlayError::Invalid(_))));

        let invalid_rule = DEMO_MINIMAL.replace(
            "[[rules.conditions]]\nfield = \"from\"\nop = \"contains\"\nvalue = \"noreply@example.test\"\n",
            "",
        );
        assert!(
            matches!(parse(&invalid_rule), Err(OverlayError::Invalid(_))),
            "rule without conditions must fail overlay validation"
        );

        let bad_quote_profile =
            format!("{DEMO_MINIMAL}\n[quote_workflows]\nprofile = \"missing_profile\"\n");
        assert!(
            matches!(parse(&bad_quote_profile), Err(OverlayError::Invalid(_))),
            "unknown quote workflow profile must fail overlay validation"
        );

        let invalid_builtin_config = format!(
            "{DEMO_MINIMAL}\n[quote_workflows]\nprofile = \"built_in\"\n[quote_workflows.config]\nrate = 42\n"
        );
        assert!(
            matches!(
                parse(&invalid_builtin_config),
                Err(OverlayError::Invalid(_))
            ),
            "built_in profile rejects non-empty profile config"
        );
    }

    #[test]
    fn call_inputs_enabled_source_requires_consent_basis() {
        let overlay = format!(
            r#"{DEMO_MINIMAL}

[[call_inputs.sources]]
source_id = "demo_selected_transcripts"
display_name = "Demo selected transcripts"
kind = "drive_transcript"
enabled = true
"#
        );
        let err = parse(&overlay).expect_err("enabled source without consent_basis rejected");
        assert!(err
            .to_string()
            .contains("enabled without a recorded consent_basis"));
    }

    #[test]
    fn slice_enablement_defaults_open_and_filters_when_listed() {
        assert!(slice_enabled(None, "email_triage"));
        let overlay = parse(DEMO_MINIMAL).expect("parse");
        assert!(slice_enabled(Some(&overlay), "email_triage"));
        assert!(
            !slice_enabled(Some(&overlay), "calendar_drafts"),
            "slice missing from enabled list is off"
        );

        let mut all_default = overlay.clone();
        all_default.slices.enabled.clear();
        assert!(slice_enabled(Some(&all_default), "calendar_drafts"));
    }

    #[test]
    fn company_profile_parses_seeds_and_maps_blanks_to_null() {
        let raw = format!(
            "{DEMO_MINIMAL}\n[company_profile]\ncompany_name = \"Example Company\"\nbio = \"Local painters.\"\nindustry = \"\"\npersona = \"Warm and plain-spoken.\"\n"
        );
        let overlay = parse(&raw).expect("parse");
        let company = overlay
            .company_profile
            .as_ref()
            .expect("company_profile present");
        assert_eq!(company.company_name, "Example Company");

        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        apply_seeds(conn, &overlay, 1_000).expect("first boot");
        apply_seeds(conn, &overlay, 2_000).expect("replay");

        let profile = crate::slices::client_profile::store::load_profile(conn, "Example Company")
            .expect("load")
            .expect("seeded");
        assert_eq!(profile.company_name.as_deref(), Some("Example Company"));
        assert_eq!(profile.bio.as_deref(), Some("Local painters."));
        assert_eq!(profile.persona.as_deref(), Some("Warm and plain-spoken."));
        // Blank overlay field stores as NULL, not "".
        assert_eq!(profile.industry, None);
    }

    #[test]
    fn no_company_profile_section_seeds_nothing() {
        let overlay = parse(DEMO_MINIMAL).expect("parse");
        assert!(overlay.company_profile.is_none());
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        apply_seeds(conn, &overlay, 1_000).expect("boot");
        assert!(
            crate::slices::client_profile::store::load_profile(conn, "Example Company")
                .expect("load")
                .is_none()
        );
    }

    #[test]
    fn seeds_apply_once_and_replay_quietly_on_reboot() {
        let overlay = parse(DEMO_MINIMAL).expect("parse");
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();

        apply_seeds(conn, &overlay, 1_000).expect("first boot");
        apply_seeds(conn, &overlay, 2_000).expect("second boot");

        // Effective state: category, rule, and policy each exist once.
        let categories =
            crate::slices::email_triage::store::list_categories(conn, "Example Company", 3_000)
                .expect("categories");
        assert_eq!(
            categories
                .iter()
                .filter(|c| c.category_id == "call_summary")
                .count(),
            1
        );
        let rules = crate::slices::email_triage::store::list_active(conn, "Example Company")
            .expect("rules");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].rule.pinned_category, "call_summary");
        assert_eq!(
            rules[0].revision, 1,
            "unchanged rule seed must not re-apply on reboot"
        );
        let policies = crate::slices::work_queue::store::list_policies(conn, "Example Company")
            .expect("policies");
        assert_eq!(policies.len(), 1);

        // A changed seed applies exactly once more.
        let mut edited = overlay.clone();
        edited.rules[0].priority = 20;
        apply_seeds(conn, &edited, 4_000).expect("edited boot");
        let rules = crate::slices::email_triage::store::list_active(conn, "Example Company")
            .expect("rules");
        assert_eq!(rules[0].rule.priority, 20);
        assert_eq!(rules[0].revision, 2);
    }

    #[test]
    fn operator_edited_seed_rows_survive_reapply() {
        let overlay = parse(DEMO_MINIMAL).expect("parse");
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();

        apply_seeds(conn, &overlay, 1_000).expect("first boot");

        let mut operator_category = overlay.categories[0].clone();
        operator_category.display_name = "Ruby calls - operator".to_string();
        crate::slices::email_triage::store::upsert_category(
            conn,
            "Example Company",
            "op_test",
            &operator_category,
            "op_category",
            2_000,
        )
        .expect("operator category edit");

        let mut operator_rule = overlay.rules[0].clone();
        operator_rule.priority = 99;
        crate::slices::email_triage::store::upsert(
            conn,
            crate::slices::email_triage::store::RuleMutationContext {
                client_id: "Example Company",
                actor_id: "op_test",
                expected_revision: None,
                idempotency_key: "op_rule",
                correlation_id: None,
                now_ms: 2_100,
            },
            &operator_rule,
        )
        .expect("operator rule edit");

        let mut operator_policy = overlay
            .policies
            .iter()
            .find(|policy| policy.category_id == "call_summary")
            .expect("ruby policy seed")
            .clone();
        operator_policy.create_work_item = false;
        operator_policy.packet_kinds.clear();
        crate::slices::work_queue::store::upsert_policy(
            conn,
            "Example Company",
            "op_test",
            &operator_policy,
            "op_policy",
            2_200,
        )
        .expect("operator policy edit");

        let mut changed_overlay = overlay.clone();
        changed_overlay.categories[0].display_name = "Call summary - overlay".to_string();
        changed_overlay.rules[0].priority = 5;
        changed_overlay
            .policies
            .iter_mut()
            .find(|policy| policy.category_id == "call_summary")
            .expect("ruby policy seed")
            .create_work_item = true;
        apply_seeds(conn, &changed_overlay, 3_000).expect("reapply");

        let categories =
            crate::slices::email_triage::store::list_categories(conn, "Example Company", 3_100)
                .expect("categories");
        let category = categories
            .iter()
            .find(|category| category.category_id == "call_summary")
            .expect("ruby category");
        assert_eq!(category.display_name, "Ruby calls - operator");

        let rules = crate::slices::email_triage::store::list_active(conn, "Example Company")
            .expect("rules");
        let rule = rules
            .iter()
            .find(|stored| stored.rule.rule_id == "call_summary_from_addr")
            .expect("ruby rule");
        assert_eq!(rule.rule.priority, 99);

        let policies = crate::slices::work_queue::store::list_policies(conn, "Example Company")
            .expect("policies");
        let policy = policies
            .iter()
            .find(|policy| policy.category_id == "call_summary")
            .expect("ruby policy");
        assert!(!policy.create_work_item);
    }

    #[test]
    fn seed_owned_call_log_category_and_rules_are_retired() {
        let overlay = parse(DEMO_MINIMAL).expect("parse");
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();

        let old_category = CategoryRecord {
            category_id: "call_log".to_string(),
            display_name: "Call log".to_string(),
            description: "Old seed".to_string(),
            color: "#8b5cf6".to_string(),
            sort: 30,
            is_system: false,
            default_agent_dir: String::new(),
            default_agent_context: String::new(),
        };
        crate::slices::email_triage::store::upsert_category(
            conn,
            "Example Company",
            "system_seed",
            &old_category,
            "old_category",
            1_000,
        )
        .expect("old category");
        let old_rule = EmailTriageRule {
            rule_id: "old_call_log_rule".to_string(),
            conditions: vec![bos_contracts::email_triage::EmailTriageCondition {
                field: bos_contracts::email_triage::EmailTriageField::Label,
                op: bos_contracts::email_triage::EmailTriageOperator::Equals,
                value: "Ruby Call Summary".to_string(),
                header_name: None,
            }],
            conditions_v2: Vec::new(),
            match_mode: bos_contracts::email_triage::EmailTriageMatchMode::All,
            priority: 10,
            enabled: true,
            pinned_category: "call_log".to_string(),
        };
        crate::slices::email_triage::store::upsert(
            conn,
            crate::slices::email_triage::store::RuleMutationContext {
                client_id: "Example Company",
                actor_id: SEED_ACTOR,
                expected_revision: None,
                idempotency_key: "old_rule",
                correlation_id: None,
                now_ms: 1_100,
            },
            &old_rule,
        )
        .expect("old rule");

        apply_seeds(conn, &overlay, 2_000).expect("retire");

        let categories =
            crate::slices::email_triage::store::list_categories(conn, "Example Company", 2_100)
                .expect("categories");
        assert!(!categories
            .iter()
            .any(|category| category.category_id == "call_log"));
        let rules = crate::slices::email_triage::store::list_active(conn, "Example Company")
            .expect("rules");
        assert!(!rules
            .iter()
            .any(|stored| stored.rule.pinned_category == "call_log"));
    }

    #[test]
    fn operator_owned_call_log_category_is_not_retired() {
        let overlay = parse(DEMO_MINIMAL).expect("parse");
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();

        let old_category = CategoryRecord {
            category_id: "call_log".to_string(),
            display_name: "Operator call log".to_string(),
            description: "Operator kept this category".to_string(),
            color: "#8b5cf6".to_string(),
            sort: 30,
            is_system: false,
            default_agent_dir: String::new(),
            default_agent_context: String::new(),
        };
        crate::slices::email_triage::store::upsert_category(
            conn,
            "Example Company",
            "op_test",
            &old_category,
            "op_call_log",
            1_000,
        )
        .expect("operator category");

        apply_seeds(conn, &overlay, 2_000).expect("reapply");

        let categories =
            crate::slices::email_triage::store::list_categories(conn, "Example Company", 2_100)
                .expect("categories");
        let call_log = categories
            .iter()
            .find(|category| category.category_id == "call_log")
            .expect("operator call_log remains");
        assert_eq!(call_log.display_name, "Operator call log");
    }
}
