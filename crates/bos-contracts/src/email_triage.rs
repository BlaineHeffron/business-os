//! Email triage rule contracts. Ported from agent-monitor-rust's
//! dm-contracts email_triage_rule module; `InputCategory` generalized
//! (client-specific category names stay in client overlays, not here).

use serde::{Deserialize, Serialize};

/// The category every classified input lands in. Categories are DATA, not an
/// enum: operators define them per client, and downstream stages key off the
/// id — packet policy (category -> permitted outputs), AI classification (the
/// catalog + descriptions form the classifier schema), and UI grouping.
pub const FALLBACK_CATEGORY_ID: &str = "inbound_email";

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategoryRecord {
    /// Stable slug; rules and messages reference this.
    pub category_id: String,
    pub display_name: String,
    /// Operator-facing meaning. Doubles as the definition handed to the AI
    /// classifier when no deterministic rule matches — write it for an LLM.
    pub description: String,
    /// UI badge color (hex like "#38bdf8").
    pub color: String,
    pub sort: i32,
    /// System categories (the fallback) cannot be deleted.
    pub is_system: bool,
    /// Default working directory for work-item agent launches in this category.
    /// Empty means the server uses its built-in operator-workspace fallback.
    #[serde(default)]
    pub default_agent_dir: String,
    /// Editable notes prefilled into the Queue launch-agent panel.
    #[serde(default)]
    pub default_agent_context: String,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategoriesListResponse {
    pub categories: Vec<CategoryRecord>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategoryUpsertRequest {
    pub category: CategoryRecord,
    /// Optional policy written atomically with the category. Used by flows that
    /// create both records as one operator action.
    #[serde(default)]
    pub policy: Option<crate::work_queue::WorkQueuePolicy>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategoryDeleteRequest {
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

pub fn validate_category_id(raw: &str) -> bool {
    !raw.is_empty()
        && raw.len() <= 64
        && raw
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EmailTriageRuleSet {
    pub schema_version: u32,
    pub rules: Vec<EmailTriageRule>,
    pub fallback_category: String,
}

impl Default for EmailTriageRuleSet {
    fn default() -> Self {
        Self {
            schema_version: 1,
            rules: Vec::new(),
            fallback_category: FALLBACK_CATEGORY_ID.to_string(),
        }
    }
}

impl EmailTriageRuleSet {
    pub fn validate(&self) -> Result<(), EmailTriageRuleValidationError> {
        if self.schema_version != 1 {
            return Err(EmailTriageRuleValidationError::SchemaVersion);
        }
        for rule in &self.rules {
            rule.validate()?;
        }
        Ok(())
    }
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EmailTriageRule {
    pub rule_id: String,
    pub conditions: Vec<EmailTriageCondition>,
    #[serde(default)]
    pub conditions_v2: Vec<EmailTriageConditionV2>,
    pub match_mode: EmailTriageMatchMode,
    pub priority: i32,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub pinned_category: String,
}

impl Default for EmailTriageRule {
    fn default() -> Self {
        Self {
            rule_id: String::new(),
            conditions: Vec::new(),
            conditions_v2: Vec::new(),
            match_mode: EmailTriageMatchMode::All,
            priority: 0,
            enabled: true,
            pinned_category: FALLBACK_CATEGORY_ID.to_string(),
        }
    }
}

impl EmailTriageRule {
    pub fn validate(&self) -> Result<(), EmailTriageRuleValidationError> {
        if self.rule_id.trim().is_empty() {
            return Err(EmailTriageRuleValidationError::BlankRuleId);
        }
        if self.conditions.is_empty() && self.conditions_v2.is_empty() {
            return Err(EmailTriageRuleValidationError::EmptyConditions);
        }
        for condition in &self.conditions {
            condition.validate()?;
        }
        for condition in &self.conditions_v2 {
            condition.validate()?;
        }
        if !validate_category_id(&self.pinned_category) {
            return Err(EmailTriageRuleValidationError::InvalidCategoryId);
        }
        Ok(())
    }
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EmailTriageCondition {
    pub field: EmailTriageField,
    pub op: EmailTriageOperator,
    pub value: String,
    pub header_name: Option<String>,
}

impl Default for EmailTriageCondition {
    fn default() -> Self {
        Self {
            field: EmailTriageField::Subject,
            op: EmailTriageOperator::Contains,
            value: String::new(),
            header_name: None,
        }
    }
}

impl EmailTriageCondition {
    fn validate(&self) -> Result<(), EmailTriageRuleValidationError> {
        if self.op != EmailTriageOperator::Exists && self.value.trim().is_empty() {
            return Err(EmailTriageRuleValidationError::ConditionValueRequired);
        }
        match self.field {
            EmailTriageField::Header => {
                if self
                    .header_name
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .is_none()
                {
                    return Err(EmailTriageRuleValidationError::HeaderNameRequired);
                }
            }
            _ => {
                if self
                    .header_name
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .is_some()
                {
                    return Err(EmailTriageRuleValidationError::HeaderNameNotAllowed);
                }
            }
        }
        Ok(())
    }
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EmailTriageConditionV2 {
    pub condition_id: EmailTriageConditionId,
    pub op: EmailTriageConditionOperator,
    pub value: EmailTriageConditionValue,
}

impl Default for EmailTriageConditionV2 {
    fn default() -> Self {
        Self {
            condition_id: EmailTriageConditionId::MessageSubject,
            op: EmailTriageConditionOperator::Contains,
            value: EmailTriageConditionValue::Text(String::new()),
        }
    }
}

impl EmailTriageConditionV2 {
    fn validate(&self) -> Result<(), EmailTriageRuleValidationError> {
        if self.op.requires_value() && self.value.is_empty() {
            return Err(EmailTriageRuleValidationError::ConditionValueRequired);
        }
        if matches!(self.condition_id, EmailTriageConditionId::MessageHeader)
            && !matches!(self.value, EmailTriageConditionValue::Header { .. })
        {
            return Err(EmailTriageRuleValidationError::HeaderNameRequired);
        }
        Ok(())
    }
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EmailTriageConditionId {
    #[serde(rename = "message.from")]
    MessageFrom,
    #[serde(rename = "message.to")]
    MessageTo,
    #[serde(rename = "message.from.email")]
    MessageFromEmail,
    #[serde(rename = "message.from.domain")]
    MessageFromDomain,
    #[serde(rename = "message.from.domain.is_business")]
    MessageFromDomainIsBusiness,
    #[serde(rename = "message.subject")]
    #[default]
    MessageSubject,
    #[serde(rename = "message.body")]
    MessageBody,
    #[serde(rename = "message.label")]
    MessageLabel,
    #[serde(rename = "message.header")]
    MessageHeader,
    #[serde(rename = "source.account.user_id")]
    SourceAccountUserId,
    #[serde(rename = "source.provider")]
    SourceProvider,
    #[serde(rename = "crm.sender_contact.exists")]
    CrmSenderContactExists,
    #[serde(rename = "crm.sender_company.exists")]
    CrmSenderCompanyExists,
    #[serde(rename = "crm.sender_deal.exists")]
    CrmSenderDealExists,
    #[serde(rename = "crm.sender_deal.stage")]
    CrmSenderDealStage,
    #[serde(rename = "crm.sender_deal.pipeline")]
    CrmSenderDealPipeline,
    #[serde(rename = "accounting.sender_customer.exists")]
    AccountingSenderCustomerExists,
    #[serde(rename = "accounting.sender_customer.has_open_invoice")]
    AccountingSenderHasOpenInvoice,
    #[serde(rename = "accounting.sender_customer.has_overdue_invoice")]
    AccountingSenderHasOverdueInvoice,
    #[serde(rename = "workflow.thread_has_open_item")]
    WorkflowThreadHasOpenItem,
    #[serde(rename = "quick.known_customer")]
    QuickKnownCustomer,
    #[serde(rename = "quick.new_sales_lead")]
    QuickNewSalesLead,
    #[serde(rename = "quick.billing_followup")]
    QuickBillingFollowup,
    #[serde(rename = "quick.existing_work_thread")]
    QuickExistingWorkThread,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmailTriageConditionValue {
    Text(String),
    Bool(bool),
    StringList(Vec<String>),
    Number(i64),
    MoneyCents(i64),
    Date(String),
    Header {
        name: String,
        value: String,
    },
    #[default]
    Empty,
}

impl EmailTriageConditionValue {
    fn is_empty(&self) -> bool {
        match self {
            Self::Text(value) | Self::Date(value) => value.trim().is_empty(),
            Self::StringList(values) => values.iter().all(|value| value.trim().is_empty()),
            Self::Header { name, value } => name.trim().is_empty() || value.trim().is_empty(),
            Self::Bool(_) | Self::Number(_) | Self::MoneyCents(_) => false,
            Self::Empty => true,
        }
    }
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmailTriageConditionOperator {
    #[default]
    Contains,
    Equals,
    StartsWith,
    Regex,
    Exists,
    IsTrue,
    IsFalse,
    In,
    GreaterThan,
    LessThan,
    AtLeast,
    AtMost,
}

impl EmailTriageConditionOperator {
    const fn requires_value(self) -> bool {
        !matches!(self, Self::Exists | Self::IsTrue | Self::IsFalse)
    }
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmailTriageConditionValueKind {
    Text,
    Bool,
    StringList,
    Number,
    MoneyCents,
    Date,
    Header,
    Empty,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmailTriageConditionGroupKind {
    Quick,
    Message,
    Source,
    Crm,
    Accounting,
    Workflow,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailTriageConditionGroup {
    pub group: EmailTriageConditionGroupKind,
    pub label: String,
    pub items: Vec<EmailTriageConditionCatalogItem>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailTriageConditionCatalogItem {
    pub condition_id: EmailTriageConditionId,
    pub label: String,
    pub description: String,
    pub group: EmailTriageConditionGroupKind,
    pub value_kind: EmailTriageConditionValueKind,
    pub supported_ops: Vec<EmailTriageConditionOperator>,
    pub fact_dependencies: Vec<EmailTriageConditionId>,
    pub provider_dependency: Option<EmailTriageProviderDependency>,
    pub expansion: Option<EmailTriageAliasExpansion>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailTriageAliasExpansion {
    pub match_mode: EmailTriageMatchMode,
    pub conditions: Vec<EmailTriageAliasCondition>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailTriageAliasCondition {
    pub condition_id: EmailTriageConditionId,
    pub op: EmailTriageConditionOperator,
    pub value: EmailTriageConditionValue,
    pub label: String,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmailTriageProviderDependency {
    Crm,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailTriageConditionCatalogResponse {
    pub groups: Vec<EmailTriageConditionGroup>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmailTriageMatchMode {
    #[default]
    All,
    Any,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmailTriageField {
    Label,
    From,
    To,
    #[default]
    Subject,
    Body,
    Header,
    SenderInCrmContacts,
    SenderDomainInCrmCompanies,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmailTriageOperator {
    #[default]
    Contains,
    Equals,
    StartsWith,
    Regex,
    Exists,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmailTriageRuleValidationError {
    SchemaVersion,
    BlankRuleId,
    EmptyConditions,
    ConditionValueRequired,
    HeaderNameRequired,
    HeaderNameNotAllowed,
    InvalidCategoryId,
}

impl EmailTriageRuleValidationError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::SchemaVersion => "email_triage_rule_set_schema_version_invalid",
            Self::BlankRuleId => "email_triage_rule_id_required",
            Self::EmptyConditions => "email_triage_rule_conditions_required",
            Self::ConditionValueRequired => "email_triage_condition_value_required",
            Self::HeaderNameRequired => "email_triage_header_name_required",
            Self::HeaderNameNotAllowed => "email_triage_header_name_not_allowed",
            Self::InvalidCategoryId => "email_triage_category_id_invalid",
        }
    }
}

const fn default_true() -> bool {
    true
}

/// The fields of a message the rules can see (dry-run samples + live ingest).
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MessageView {
    /// Optional stored inbound message id. Dry-run samples omit this unless
    /// they came from the inbox read model.
    pub message_id: Option<String>,
    /// Optional connected mailbox user id for source-account conditions.
    pub source_user_id: Option<String>,
    pub subject: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub body: Option<String>,
    pub labels: Vec<String>,
    pub headers: Vec<(String, String)>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DryRunResult {
    pub resolved_category: String,
    pub matched_rule_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleWithRevision {
    pub rule: EmailTriageRule,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub revision: u64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailTriageRulesListResponse {
    pub rules: Vec<RuleWithRevision>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailTriageRuleUpsertRequest {
    pub rule: EmailTriageRule,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmailTriageRuleActionKind {
    Enable,
    Disable,
    Delete,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailTriageRuleActionRequest {
    pub action: EmailTriageRuleActionKind,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailTriageDryRunRequest {
    /// Proposed rules overlay stored rules with the same id, then evaluate in
    /// live priority order.
    #[serde(default)]
    pub proposed_rules: Vec<EmailTriageRule>,
    #[serde(default)]
    pub fallback_category: Option<String>,
    pub samples: Vec<MessageView>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailTriageDryRunResponse {
    pub results: Vec<DryRunResult>,
    #[serde(default)]
    pub traces: Vec<EmailTriageDryRunTrace>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailTriageDryRunTrace {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub sample_index: u32,
    pub resolved_category: String,
    pub matched_rule_id: Option<String>,
    pub needs_fact_refresh: bool,
    pub rule_traces: Vec<EmailTriageRuleTrace>,
    pub fact_traces: Vec<EmailTriageFactTrace>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailTriageRuleTrace {
    pub rule_id: String,
    pub result: EmailTriageTriValue,
    pub matched: bool,
    pub condition_traces: Vec<EmailTriageConditionTrace>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailTriageConditionTrace {
    pub condition_id: EmailTriageConditionId,
    pub label: String,
    pub result: EmailTriageTriValue,
    pub detail: String,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailTriageFactTrace {
    pub condition_id: EmailTriageConditionId,
    pub label: String,
    pub value: EmailTriageTriValue,
    pub source: EmailTriageFactSource,
    pub detail: String,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmailTriageTriValue {
    True,
    False,
    Unknown,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmailTriageFactSource {
    Message,
    Source,
    AccountingSnapshot,
    Workflow,
    CrmCache,
    CrmLive,
    NotChecked,
}

/// Gmail inbox tab categories. Gmail exposes these as system label ids; the UI
/// treats them as first-class tabs.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmailTriageGmailCategory {
    Primary,
    Updates,
    Social,
    Promotions,
    Forums,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailTriageGmailCategoryOption {
    pub category: EmailTriageGmailCategory,
    pub display_name: String,
    pub count: u32,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailTriageDashboardCategoryOption {
    pub category_id: String,
    pub count: u32,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailTriageLabelOption {
    pub label: String,
    pub count: u32,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailTriageMailboxOption {
    #[serde(default)]
    pub source_user_id: Option<String>,
    pub display_name: String,
    pub count: u32,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailTriageCrmDealFacetOption {
    pub value: String,
    pub count: u32,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailTriageInboxDefaults {
    pub categories: Vec<EmailTriageGmailCategory>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub source_user_id: Option<String>,
    pub limit: u32,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailTriageInboxOptionsResponse {
    pub categories: Vec<EmailTriageGmailCategoryOption>,
    pub visible_gmail_categories: Vec<EmailTriageGmailCategory>,
    pub dashboard_categories: Vec<EmailTriageDashboardCategoryOption>,
    pub labels: Vec<EmailTriageLabelOption>,
    pub mailboxes: Vec<EmailTriageMailboxOption>,
    #[serde(default)]
    pub crm_deal_stages: Vec<EmailTriageCrmDealFacetOption>,
    #[serde(default)]
    pub crm_deal_pipelines: Vec<EmailTriageCrmDealFacetOption>,
    pub defaults: EmailTriageInboxDefaults,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailTriageInboxSettingsResponse {
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub revision: Option<u64>,
    pub visible_gmail_categories: Vec<EmailTriageGmailCategory>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailTriageInboxSettingsUpdateRequest {
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
    pub visible_gmail_categories: Vec<EmailTriageGmailCategory>,
}

/// Safe metadata for an inbound Gmail attachment. This intentionally carries
/// provider ids and display metadata only; bytes are fetched lazily through an
/// audited BusinessOS route when an operator or launched agent needs them.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailAttachmentRecord {
    pub attachment_id: String,
    #[serde(default)]
    pub part_id: Option<String>,
    pub filename: String,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub size_bytes: Option<u64>,
    #[serde(default)]
    pub inline: bool,
    #[serde(default)]
    pub content_id: Option<String>,
}

/// A classified inbound message as served by the inbox read model.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboundMessageRecord {
    /// BusinessOS-stable source identity. Legacy rows equal `message_id`; newer
    /// per-user Gmail rows include the mailbox provenance so identical Gmail ids
    /// from different connected users do not collide.
    #[serde(default)]
    pub source_key: String,
    /// Raw Gmail message id. Kept raw for provider fetch/reply calls.
    pub message_id: String,
    pub thread_id: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub internal_date_ms: Option<i64>,
    pub from_addr: Option<String>,
    pub to_addr: Option<String>,
    pub subject: Option<String>,
    pub body_excerpt: String,
    /// Raw persisted body for server-side AI/produce grounding. API responses
    /// keep using body_excerpt so list/source UI stays bounded.
    #[serde(default, skip_serializing)]
    #[cfg_attr(feature = "ts", ts(skip))]
    pub body_full: String,
    /// Safe, bounded headers retained for server-side automated-sender rules.
    /// Not serialized to the browser; raw/full header payloads stay provider-side.
    #[serde(default, skip_serializing)]
    #[cfg_attr(feature = "ts", ts(skip))]
    pub headers: Vec<(String, String)>,
    pub labels: Vec<String>,
    pub resolved_category: String,
    pub matched_rule_id: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub ingested_at_ms: u64,
    /// Tier-2 AI triage state: null = not yet examined, "suggested",
    /// "no_suggestion", or "error".
    #[serde(default)]
    pub ai_triage_status: Option<String>,
    #[serde(default)]
    pub ai_triage_rationale: Option<String>,
    /// Attachments are metadata only. Attachment bytes stay in Gmail until an
    /// explicit, audited evidence-staging request fetches one.
    #[serde(default)]
    pub attachments: Vec<EmailAttachmentRecord>,
    /// Operator user whose connected Gmail account this message came from.
    /// Null = ingested before multi-user accounts or via the env credential.
    #[serde(default)]
    pub source_user_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailTriageInboxResponse {
    pub messages: Vec<InboundMessageRecord>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailManualFollowUpRequest {
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

/// Explicit operator request to move one source message to Gmail Trash.
/// This is separate from queue dismissal, which only changes BusinessOS state.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailTrashRequest {
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailAttachmentEvidenceRequest {
    pub session_id: String,
    #[serde(default)]
    pub item_id: Option<String>,
    #[serde(default)]
    pub target_dir: Option<String>,
    pub idempotency_key: String,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailAttachmentEvidenceResponse {
    pub evidence_id: String,
    pub session_id: String,
    pub message_id: String,
    pub attachment_id: String,
    pub filename: String,
    pub mime_type: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub size_bytes: u64,
    pub path: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub retention_until_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_set_round_trips() {
        let set = EmailTriageRuleSet {
            schema_version: 1,
            rules: vec![EmailTriageRule {
                rule_id: "call-log".into(),
                conditions: vec![EmailTriageCondition {
                    field: EmailTriageField::Subject,
                    op: EmailTriageOperator::Contains,
                    value: "call log".into(),
                    header_name: None,
                }],
                conditions_v2: Vec::new(),
                match_mode: EmailTriageMatchMode::All,
                priority: 10,
                enabled: true,
                pinned_category: "call_log".to_string(),
            }],
            fallback_category: FALLBACK_CATEGORY_ID.to_string(),
        };
        set.validate().expect("valid");
        let json = serde_json::to_string(&set).expect("serialize");
        let back: EmailTriageRuleSet = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(set, back);
    }

    #[test]
    fn condition_catalog_ids_round_trip_as_stable_strings() {
        let id = EmailTriageConditionId::WorkflowThreadHasOpenItem;
        let json = serde_json::to_string(&id).expect("serialize id");
        assert_eq!(json, "\"workflow.thread_has_open_item\"");
        let back: EmailTriageConditionId = serde_json::from_str(&json).expect("deserialize id");
        assert_eq!(back, id);
    }

    #[test]
    fn validation_rejects_blank_rule_id_and_empty_conditions() {
        let blank = EmailTriageRule::default();
        assert_eq!(
            blank.validate(),
            Err(EmailTriageRuleValidationError::BlankRuleId)
        );
        let no_conditions = EmailTriageRule {
            rule_id: "r1".into(),
            ..Default::default()
        };
        assert_eq!(
            no_conditions.validate(),
            Err(EmailTriageRuleValidationError::EmptyConditions)
        );
    }

    #[test]
    fn header_field_requires_header_name_and_others_forbid_it() {
        let header_missing = EmailTriageCondition {
            field: EmailTriageField::Header,
            op: EmailTriageOperator::Contains,
            value: "x".into(),
            header_name: None,
        };
        let rule = EmailTriageRule {
            rule_id: "r1".into(),
            conditions: vec![header_missing],
            ..Default::default()
        };
        assert_eq!(
            rule.validate(),
            Err(EmailTriageRuleValidationError::HeaderNameRequired)
        );

        let subject_with_header = EmailTriageCondition {
            field: EmailTriageField::Subject,
            op: EmailTriageOperator::Contains,
            value: "x".into(),
            header_name: Some("X-Custom".into()),
        };
        let rule = EmailTriageRule {
            rule_id: "r2".into(),
            conditions: vec![subject_with_header],
            ..Default::default()
        };
        assert_eq!(
            rule.validate(),
            Err(EmailTriageRuleValidationError::HeaderNameNotAllowed)
        );
    }
}

/// Result of re-running the current rules over all stored inbound messages.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReclassifyResponse {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub examined: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub reclassified: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub work_items_emitted: u64,
}

/// Which AI-triage verdicts a reset clears for re-examination.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiRetriageResetScope {
    /// One specific message (any verdict).
    Message,
    /// `no_suggestion` verdicts older than the newest category change, plus
    /// all errors — "the catalog changed; re-examine what AI saw nothing in".
    Stale,
    /// Every `no_suggestion` and `error` verdict.
    All,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiRetriageResetRequest {
    pub scope: AiRetriageResetScope,
    /// Required when scope is `message`. This is the BusinessOS source key,
    /// not the raw provider message id.
    #[serde(default)]
    pub source_key: Option<String>,
    /// Legacy alias for `source_key`.
    #[serde(default)]
    pub message_id: Option<String>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiRetriageResetResponse {
    /// Messages whose verdict was cleared (the next pump cycle re-examines
    /// them, up to its per-cycle budget).
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub reset: u64,
}
