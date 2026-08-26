//! Static condition catalog for operator-authored email triage rules.

use bos_contracts::email_triage::{
    EmailTriageAliasCondition, EmailTriageAliasExpansion, EmailTriageConditionCatalogItem,
    EmailTriageConditionCatalogResponse, EmailTriageConditionGroup, EmailTriageConditionGroupKind,
    EmailTriageConditionId, EmailTriageConditionOperator, EmailTriageConditionValue,
    EmailTriageConditionValueKind, EmailTriageMatchMode, EmailTriageProviderDependency,
};

pub fn condition_catalog() -> EmailTriageConditionCatalogResponse {
    EmailTriageConditionCatalogResponse {
        groups: vec![
            group(
                EmailTriageConditionGroupKind::Quick,
                "Quick picks",
                vec![
                    alias(
                        EmailTriageConditionId::QuickKnownCustomer,
                        "Known customer",
                        "The sender is already known in CRM or accounting.",
                        EmailTriageMatchMode::Any,
                        vec![
                            alias_true(
                                EmailTriageConditionId::CrmSenderContactExists,
                                "Sender is a saved contact",
                            ),
                            alias_true(
                                EmailTriageConditionId::CrmSenderCompanyExists,
                                "Sender's company is a known account",
                            ),
                            alias_true(
                                EmailTriageConditionId::AccountingSenderCustomerExists,
                                "Sender is an accounting customer",
                            ),
                        ],
                    ),
                    alias(
                        EmailTriageConditionId::QuickNewSalesLead,
                        "New sales lead",
                        "The message looks like a new opportunity from someone not already known.",
                        EmailTriageMatchMode::All,
                        vec![
                            alias_true(
                                EmailTriageConditionId::MessageFromDomainIsBusiness,
                                "Sender uses a business email domain",
                            ),
                            alias_condition(
                                EmailTriageConditionId::CrmSenderContactExists,
                                EmailTriageConditionOperator::IsFalse,
                                EmailTriageConditionValue::Empty,
                                "Sender is not already a saved contact",
                            ),
                            alias_condition(
                                EmailTriageConditionId::CrmSenderCompanyExists,
                                EmailTriageConditionOperator::IsFalse,
                                EmailTriageConditionValue::Empty,
                                "Sender's company is not already known",
                            ),
                            alias_condition(
                                EmailTriageConditionId::AccountingSenderCustomerExists,
                                EmailTriageConditionOperator::IsFalse,
                                EmailTriageConditionValue::Empty,
                                "Sender is not already an accounting customer",
                            ),
                            alias_condition(
                                EmailTriageConditionId::MessageBody,
                                EmailTriageConditionOperator::Regex,
                                EmailTriageConditionValue::Text(
                                    "(?i)(quote|estimate|project|pricing|proposal)".to_string(),
                                ),
                                "Message mentions buying or project interest",
                            ),
                        ],
                    ),
                    alias(
                        EmailTriageConditionId::QuickBillingFollowup,
                        "Billing follow-up",
                        "A known customer is writing about an invoice or payment.",
                        EmailTriageMatchMode::All,
                        vec![
                            alias_true(
                                EmailTriageConditionId::AccountingSenderCustomerExists,
                                "Sender is an accounting customer",
                            ),
                            alias_true(
                                EmailTriageConditionId::AccountingSenderHasOpenInvoice,
                                "Sender has an open invoice",
                            ),
                            alias_condition(
                                EmailTriageConditionId::MessageBody,
                                EmailTriageConditionOperator::Regex,
                                EmailTriageConditionValue::Text(
                                    "(?i)(invoice|payment|balance|past due|overdue)".to_string(),
                                ),
                                "Message mentions invoice or payment",
                            ),
                        ],
                    ),
                    alias(
                        EmailTriageConditionId::QuickExistingWorkThread,
                        "Existing work thread",
                        "This email is already tied to an open work item.",
                        EmailTriageMatchMode::All,
                        vec![alias_true(
                            EmailTriageConditionId::WorkflowThreadHasOpenItem,
                            "This email already has open work",
                        )],
                    ),
                ],
            ),
            group(
                EmailTriageConditionGroupKind::Message,
                "Message",
                vec![
                    text(
                        EmailTriageConditionId::MessageFrom,
                        "From line",
                        "The raw From line, including display name and email address.",
                        EmailTriageConditionGroupKind::Message,
                    ),
                    text(
                        EmailTriageConditionId::MessageTo,
                        "To line",
                        "The raw To line, including display name and email address.",
                        EmailTriageConditionGroupKind::Message,
                    ),
                    text(
                        EmailTriageConditionId::MessageFromEmail,
                        "Sender email",
                        "The sender's parsed email address.",
                        EmailTriageConditionGroupKind::Message,
                    ),
                    text(
                        EmailTriageConditionId::MessageFromDomain,
                        "Sender domain",
                        "The sender's parsed email domain.",
                        EmailTriageConditionGroupKind::Message,
                    ),
                    bool_fact(
                        EmailTriageConditionId::MessageFromDomainIsBusiness,
                        "Sender uses a business email domain",
                        "The sender's email domain is a company domain, not a consumer mailbox such as Gmail, Outlook, or Yahoo.",
                        EmailTriageConditionGroupKind::Message,
                        None,
                    ),
                    text(
                        EmailTriageConditionId::MessageSubject,
                        "Subject",
                        "Words in the email subject.",
                        EmailTriageConditionGroupKind::Message,
                    ),
                    text(
                        EmailTriageConditionId::MessageBody,
                        "Message body",
                        "Words in the email body.",
                        EmailTriageConditionGroupKind::Message,
                    ),
                    item(
                        EmailTriageConditionId::MessageLabel,
                        "Mailbox label",
                        "A label already attached to the message.",
                        EmailTriageConditionGroupKind::Message,
                        EmailTriageConditionValueKind::Text,
                        text_ops(),
                        vec![],
                        None,
                        None,
                    ),
                    item(
                        EmailTriageConditionId::MessageHeader,
                        "Message header",
                        "A named email header and its value.",
                        EmailTriageConditionGroupKind::Message,
                        EmailTriageConditionValueKind::Header,
                        text_ops(),
                        vec![],
                        None,
                        None,
                    ),
                ],
            ),
            group(
                EmailTriageConditionGroupKind::Source,
                "Source",
                vec![
                    text(
                        EmailTriageConditionId::SourceAccountUserId,
                        "Connected mailbox user",
                        "Which connected mailbox received the message.",
                        EmailTriageConditionGroupKind::Source,
                    ),
                    item(
                        EmailTriageConditionId::SourceProvider,
                        "Mail provider",
                        "The service that delivered the message.",
                        EmailTriageConditionGroupKind::Source,
                        EmailTriageConditionValueKind::Text,
                        vec![
                            EmailTriageConditionOperator::Equals,
                            EmailTriageConditionOperator::In,
                        ],
                        vec![],
                        None,
                        None,
                    ),
                ],
            ),
            group(
                EmailTriageConditionGroupKind::Crm,
                "CRM",
                vec![
                    bool_fact(
                        EmailTriageConditionId::CrmSenderContactExists,
                        "Sender is a saved contact",
                        "A CRM contact exists for the sender.",
                        EmailTriageConditionGroupKind::Crm,
                        Some(EmailTriageProviderDependency::Crm),
                    ),
                    bool_fact(
                        EmailTriageConditionId::CrmSenderCompanyExists,
                        "Sender's company is a known account",
                        "A CRM company exists for the sender's business domain.",
                        EmailTriageConditionGroupKind::Crm,
                        Some(EmailTriageProviderDependency::Crm),
                    ),
                    bool_fact(
                        EmailTriageConditionId::CrmSenderDealExists,
                        "Sender has an associated deal",
                        "At least one active cached CRM deal is associated with the sender.",
                        EmailTriageConditionGroupKind::Crm,
                        Some(EmailTriageProviderDependency::Crm),
                    ),
                    item(
                        EmailTriageConditionId::CrmSenderDealStage,
                        "Sender deal stage",
                        "A stage from any cached CRM deal associated with the sender.",
                        EmailTriageConditionGroupKind::Crm,
                        EmailTriageConditionValueKind::StringList,
                        vec![
                            EmailTriageConditionOperator::Equals,
                            EmailTriageConditionOperator::In,
                            EmailTriageConditionOperator::Contains,
                            EmailTriageConditionOperator::Exists,
                        ],
                        vec![EmailTriageConditionId::CrmSenderDealExists],
                        Some(EmailTriageProviderDependency::Crm),
                        None,
                    ),
                    item(
                        EmailTriageConditionId::CrmSenderDealPipeline,
                        "Sender deal pipeline",
                        "A pipeline from any cached CRM deal associated with the sender.",
                        EmailTriageConditionGroupKind::Crm,
                        EmailTriageConditionValueKind::StringList,
                        vec![
                            EmailTriageConditionOperator::Equals,
                            EmailTriageConditionOperator::In,
                            EmailTriageConditionOperator::Contains,
                            EmailTriageConditionOperator::Exists,
                        ],
                        vec![EmailTriageConditionId::CrmSenderDealExists],
                        Some(EmailTriageProviderDependency::Crm),
                        None,
                    ),
                ],
            ),
            group(
                EmailTriageConditionGroupKind::Accounting,
                "Accounting",
                vec![
                    bool_fact(
                        EmailTriageConditionId::AccountingSenderCustomerExists,
                        "Sender is an accounting customer",
                        "A customer snapshot exists for the sender.",
                        EmailTriageConditionGroupKind::Accounting,
                        None,
                    ),
                    bool_fact(
                        EmailTriageConditionId::AccountingSenderHasOpenInvoice,
                        "Sender has an open invoice",
                        "The sender has at least one invoice still open.",
                        EmailTriageConditionGroupKind::Accounting,
                        None,
                    ),
                    bool_fact(
                        EmailTriageConditionId::AccountingSenderHasOverdueInvoice,
                        "Sender has an overdue invoice",
                        "The sender has at least one invoice past due.",
                        EmailTriageConditionGroupKind::Accounting,
                        None,
                    ),
                ],
            ),
            group(
                EmailTriageConditionGroupKind::Workflow,
                "Workflow",
                vec![bool_fact(
                    EmailTriageConditionId::WorkflowThreadHasOpenItem,
                    "This email already has open work",
                    "An open work item already exists for this email thread.",
                    EmailTriageConditionGroupKind::Workflow,
                    None,
                )],
            ),
        ],
    }
}

fn group(
    group: EmailTriageConditionGroupKind,
    label: &str,
    items: Vec<EmailTriageConditionCatalogItem>,
) -> EmailTriageConditionGroup {
    EmailTriageConditionGroup {
        group,
        label: label.to_string(),
        items,
    }
}

fn text(
    condition_id: EmailTriageConditionId,
    label: &str,
    description: &str,
    group: EmailTriageConditionGroupKind,
) -> EmailTriageConditionCatalogItem {
    item(
        condition_id,
        label,
        description,
        group,
        EmailTriageConditionValueKind::Text,
        text_ops(),
        vec![],
        None,
        None,
    )
}

fn bool_fact(
    condition_id: EmailTriageConditionId,
    label: &str,
    description: &str,
    group: EmailTriageConditionGroupKind,
    provider_dependency: Option<EmailTriageProviderDependency>,
) -> EmailTriageConditionCatalogItem {
    item(
        condition_id,
        label,
        description,
        group,
        EmailTriageConditionValueKind::Bool,
        vec![
            EmailTriageConditionOperator::IsTrue,
            EmailTriageConditionOperator::IsFalse,
            EmailTriageConditionOperator::Exists,
        ],
        vec![],
        provider_dependency,
        None,
    )
}

fn alias(
    condition_id: EmailTriageConditionId,
    label: &str,
    description: &str,
    match_mode: EmailTriageMatchMode,
    conditions: Vec<EmailTriageAliasCondition>,
) -> EmailTriageConditionCatalogItem {
    let fact_dependencies = conditions
        .iter()
        .map(|condition| condition.condition_id)
        .collect();
    item(
        condition_id,
        label,
        description,
        EmailTriageConditionGroupKind::Quick,
        EmailTriageConditionValueKind::Empty,
        vec![EmailTriageConditionOperator::Exists],
        fact_dependencies,
        None,
        Some(EmailTriageAliasExpansion {
            match_mode,
            conditions,
        }),
    )
}

fn alias_condition(
    condition_id: EmailTriageConditionId,
    op: EmailTriageConditionOperator,
    value: EmailTriageConditionValue,
    label: &str,
) -> EmailTriageAliasCondition {
    EmailTriageAliasCondition {
        condition_id,
        op,
        value,
        label: label.to_string(),
    }
}

fn alias_true(condition_id: EmailTriageConditionId, label: &str) -> EmailTriageAliasCondition {
    alias_condition(
        condition_id,
        EmailTriageConditionOperator::IsTrue,
        EmailTriageConditionValue::Empty,
        label,
    )
}

#[allow(clippy::too_many_arguments)]
fn item(
    condition_id: EmailTriageConditionId,
    label: &str,
    description: &str,
    group: EmailTriageConditionGroupKind,
    value_kind: EmailTriageConditionValueKind,
    supported_ops: Vec<EmailTriageConditionOperator>,
    fact_dependencies: Vec<EmailTriageConditionId>,
    provider_dependency: Option<EmailTriageProviderDependency>,
    expansion: Option<EmailTriageAliasExpansion>,
) -> EmailTriageConditionCatalogItem {
    EmailTriageConditionCatalogItem {
        condition_id,
        label: label.to_string(),
        description: description.to_string(),
        group,
        value_kind,
        supported_ops,
        fact_dependencies,
        provider_dependency,
        expansion,
    }
}

fn text_ops() -> Vec<EmailTriageConditionOperator> {
    vec![
        EmailTriageConditionOperator::Contains,
        EmailTriageConditionOperator::Equals,
        EmailTriageConditionOperator::StartsWith,
        EmailTriageConditionOperator::Regex,
        EmailTriageConditionOperator::Exists,
    ]
}
