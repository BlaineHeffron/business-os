//! Tri-valued rule facts for email triage V2 evaluation.

use bos_contracts::email_triage::{
    EmailTriageConditionId, EmailTriageFactSource, EmailTriageFactTrace, EmailTriageTriValue,
    MessageView,
};
use rusqlite::{params, Connection, OptionalExtension};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriValue {
    True,
    False,
    Unknown,
}

impl TriValue {
    pub const fn is_unknown(self) -> bool {
        matches!(self, Self::Unknown)
    }

    pub const fn to_contract(self) -> EmailTriageTriValue {
        match self {
            Self::True => EmailTriageTriValue::True,
            Self::False => EmailTriageTriValue::False,
            Self::Unknown => EmailTriageTriValue::Unknown,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SenderSubjects {
    pub email: Option<String>,
    pub domain: Option<String>,
    pub unresolved: bool,
}

pub fn resolve_subjects(from: Option<&str>) -> SenderSubjects {
    let Some(email) = super::subjects::first_normalized_email(from) else {
        return SenderSubjects {
            unresolved: true,
            ..Default::default()
        };
    };
    let domain = email_domain(&email);
    SenderSubjects {
        email: Some(email),
        domain,
        unresolved: false,
    }
}

#[derive(Debug, Clone, Default)]
pub struct CrmFactOverrides {
    pub sender_contact_exists: Option<CrmFactValue>,
    pub sender_company_exists: Option<CrmFactValue>,
    pub sender_deal_exists: Option<CrmFactValue>,
    pub sender_deal_stages: Option<CrmTextFactValue>,
    pub sender_deal_pipelines: Option<CrmTextFactValue>,
}

#[derive(Debug, Clone)]
pub struct CrmFactValue {
    pub value: TriValue,
    pub source: EmailTriageFactSource,
    pub detail: String,
}

impl CrmFactValue {
    pub fn live(value: TriValue) -> Self {
        Self {
            value,
            source: EmailTriageFactSource::CrmLive,
            detail: "checked CRM just now".to_string(),
        }
    }

    pub fn cache(value: TriValue, detail: String) -> Self {
        Self {
            value,
            source: EmailTriageFactSource::CrmCache,
            detail,
        }
    }

    pub fn unknown(detail: impl Into<String>) -> Self {
        Self {
            value: TriValue::Unknown,
            source: EmailTriageFactSource::NotChecked,
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CrmTextFactValue {
    pub values: Vec<String>,
    pub source: EmailTriageFactSource,
    pub detail: String,
    pub unknown: bool,
}

impl CrmTextFactValue {
    pub fn cache(values: Vec<String>, detail: String) -> Self {
        Self {
            values,
            source: EmailTriageFactSource::CrmCache,
            detail,
            unknown: false,
        }
    }

    pub fn unknown(detail: impl Into<String>) -> Self {
        Self {
            values: Vec::new(),
            source: EmailTriageFactSource::NotChecked,
            detail: detail.into(),
            unknown: true,
        }
    }

    fn tri_value(&self) -> TriValue {
        if self.unknown {
            TriValue::Unknown
        } else if self.values.is_empty() {
            TriValue::False
        } else {
            TriValue::True
        }
    }
}

pub struct FactBag<'a> {
    conn: Option<&'a Connection>,
    client_id: &'a str,
    message: &'a MessageView,
    message_id: Option<&'a str>,
    source_user_id: Option<&'a str>,
    provider: &'a str,
    subjects: SenderSubjects,
    crm: CrmFactOverrides,
    traces: Vec<EmailTriageFactTrace>,
}

impl<'a> FactBag<'a> {
    pub fn new(
        conn: Option<&'a Connection>,
        client_id: &'a str,
        message: &'a MessageView,
        message_id: Option<&'a str>,
        source_user_id: Option<&'a str>,
        crm: CrmFactOverrides,
    ) -> Self {
        Self {
            conn,
            client_id,
            message,
            message_id,
            source_user_id,
            // Gmail is the only ingest provider wired in v1. When MessageView
            // carries provider metadata, pass it here instead of defaulting.
            provider: "gmail",
            subjects: resolve_subjects(message.from.as_deref()),
            crm,
            traces: Vec::new(),
        }
    }

    pub fn fact(&mut self, id: EmailTriageConditionId) -> TriValue {
        let (value, source, detail) = match id {
            EmailTriageConditionId::MessageFrom => {
                exists_text(self.message.from.as_deref(), EmailTriageFactSource::Message)
            }
            EmailTriageConditionId::MessageTo => {
                exists_text(self.message.to.as_deref(), EmailTriageFactSource::Message)
            }
            EmailTriageConditionId::MessageFromEmail => exists_text(
                self.subjects.email.as_deref(),
                EmailTriageFactSource::Message,
            ),
            EmailTriageConditionId::MessageFromDomain => exists_text(
                self.subjects.domain.as_deref(),
                EmailTriageFactSource::Message,
            ),
            EmailTriageConditionId::MessageFromDomainIsBusiness => self.business_domain(),
            EmailTriageConditionId::MessageSubject => exists_text(
                self.message.subject.as_deref(),
                EmailTriageFactSource::Message,
            ),
            EmailTriageConditionId::MessageBody => {
                exists_text(self.message.body.as_deref(), EmailTriageFactSource::Message)
            }
            EmailTriageConditionId::MessageLabel => {
                let value = if self
                    .message
                    .labels
                    .iter()
                    .any(|label| !label.trim().is_empty())
                {
                    TriValue::True
                } else {
                    TriValue::False
                };
                (
                    value,
                    EmailTriageFactSource::Message,
                    "message labels".to_string(),
                )
            }
            EmailTriageConditionId::MessageHeader => {
                let value = if self
                    .message
                    .headers
                    .iter()
                    .any(|(name, value)| !name.trim().is_empty() && !value.trim().is_empty())
                {
                    TriValue::True
                } else {
                    TriValue::False
                };
                (
                    value,
                    EmailTriageFactSource::Message,
                    "message headers".to_string(),
                )
            }
            EmailTriageConditionId::SourceAccountUserId => {
                exists_text(self.source_user_id, EmailTriageFactSource::Source)
            }
            EmailTriageConditionId::SourceProvider => {
                exists_text(Some(self.provider), EmailTriageFactSource::Source)
            }
            EmailTriageConditionId::CrmSenderContactExists => (
                self.crm
                    .sender_contact_exists
                    .as_ref()
                    .map(|fact| fact.value)
                    .unwrap_or(TriValue::Unknown),
                self.crm
                    .sender_contact_exists
                    .as_ref()
                    .map(|fact| fact.source)
                    .unwrap_or(EmailTriageFactSource::NotChecked),
                self.crm
                    .sender_contact_exists
                    .as_ref()
                    .map(|fact| fact.detail.clone())
                    .unwrap_or_else(|| "couldn't check yet".to_string()),
            ),
            EmailTriageConditionId::CrmSenderCompanyExists => (
                self.crm
                    .sender_company_exists
                    .as_ref()
                    .map(|fact| fact.value)
                    .unwrap_or(TriValue::Unknown),
                self.crm
                    .sender_company_exists
                    .as_ref()
                    .map(|fact| fact.source)
                    .unwrap_or(EmailTriageFactSource::NotChecked),
                self.crm
                    .sender_company_exists
                    .as_ref()
                    .map(|fact| fact.detail.clone())
                    .unwrap_or_else(|| "couldn't check yet".to_string()),
            ),
            EmailTriageConditionId::CrmSenderDealExists => (
                self.crm
                    .sender_deal_exists
                    .as_ref()
                    .map(|fact| fact.value)
                    .unwrap_or(TriValue::Unknown),
                self.crm
                    .sender_deal_exists
                    .as_ref()
                    .map(|fact| fact.source)
                    .unwrap_or(EmailTriageFactSource::NotChecked),
                self.crm
                    .sender_deal_exists
                    .as_ref()
                    .map(|fact| fact.detail.clone())
                    .unwrap_or_else(|| "couldn't check yet".to_string()),
            ),
            EmailTriageConditionId::CrmSenderDealStage
            | EmailTriageConditionId::CrmSenderDealPipeline => self.crm_text_fact(id),
            EmailTriageConditionId::AccountingSenderCustomerExists => self.accounting_customer(),
            EmailTriageConditionId::AccountingSenderHasOpenInvoice => {
                self.accounting_invoice(false)
            }
            EmailTriageConditionId::AccountingSenderHasOverdueInvoice => {
                self.accounting_invoice(true)
            }
            EmailTriageConditionId::WorkflowThreadHasOpenItem => self.workflow_open_item(),
            EmailTriageConditionId::QuickKnownCustomer
            | EmailTriageConditionId::QuickNewSalesLead
            | EmailTriageConditionId::QuickBillingFollowup
            | EmailTriageConditionId::QuickExistingWorkThread => (
                TriValue::Unknown,
                EmailTriageFactSource::NotChecked,
                "quick pick expands during rule evaluation".to_string(),
            ),
        };
        self.traces.push(EmailTriageFactTrace {
            condition_id: id,
            label: fact_label(id).to_string(),
            value: value.to_contract(),
            source,
            detail,
        });
        value
    }

    pub fn drain_traces(&mut self) -> Vec<EmailTriageFactTrace> {
        std::mem::take(&mut self.traces)
    }

    pub fn text_value(&self, id: EmailTriageConditionId) -> Option<&str> {
        match id {
            EmailTriageConditionId::MessageFrom => self.message.from.as_deref(),
            EmailTriageConditionId::MessageTo => self.message.to.as_deref(),
            EmailTriageConditionId::MessageFromEmail => self.subjects.email.as_deref(),
            EmailTriageConditionId::MessageFromDomain => self.subjects.domain.as_deref(),
            EmailTriageConditionId::MessageFromDomainIsBusiness => None,
            EmailTriageConditionId::MessageSubject => self.message.subject.as_deref(),
            EmailTriageConditionId::MessageBody => self.message.body.as_deref(),
            EmailTriageConditionId::SourceAccountUserId => self.source_user_id,
            EmailTriageConditionId::SourceProvider => Some(self.provider),
            _ => None,
        }
    }

    pub fn text_values(&mut self, id: EmailTriageConditionId) -> Option<Vec<String>> {
        match id {
            EmailTriageConditionId::CrmSenderDealStage => {
                self.record_crm_text_trace(id, self.crm.sender_deal_stages.clone())
            }
            EmailTriageConditionId::CrmSenderDealPipeline => {
                self.record_crm_text_trace(id, self.crm.sender_deal_pipelines.clone())
            }
            _ => self.text_value(id).map(|value| vec![value.to_string()]),
        }
    }

    pub fn labels(&self) -> &[String] {
        &self.message.labels
    }

    pub fn header_value(&self, header_name: &str) -> Option<&str> {
        self.message
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(header_name.trim()))
            .map(|(_, value)| value.as_str())
    }

    fn business_domain(&self) -> (TriValue, EmailTriageFactSource, String) {
        let Some(domain) = self.subjects.domain.as_deref() else {
            return (
                TriValue::Unknown,
                EmailTriageFactSource::Message,
                "couldn't check yet (sender domain is unclear)".to_string(),
            );
        };
        if super::subjects::public_mailbox_domain(domain) {
            (
                TriValue::False,
                EmailTriageFactSource::Message,
                "sender uses a consumer mailbox domain".to_string(),
            )
        } else {
            (
                TriValue::True,
                EmailTriageFactSource::Message,
                "sender uses a business email domain".to_string(),
            )
        }
    }

    fn crm_text_fact(
        &self,
        id: EmailTriageConditionId,
    ) -> (TriValue, EmailTriageFactSource, String) {
        let fact = match id {
            EmailTriageConditionId::CrmSenderDealStage => self.crm.sender_deal_stages.as_ref(),
            EmailTriageConditionId::CrmSenderDealPipeline => {
                self.crm.sender_deal_pipelines.as_ref()
            }
            _ => None,
        };
        let Some(fact) = fact else {
            return (
                TriValue::Unknown,
                EmailTriageFactSource::NotChecked,
                "couldn't check yet".to_string(),
            );
        };
        (fact.tri_value(), fact.source, fact.detail.clone())
    }

    fn record_crm_text_trace(
        &mut self,
        id: EmailTriageConditionId,
        fact: Option<CrmTextFactValue>,
    ) -> Option<Vec<String>> {
        let fact = fact?;
        self.traces.push(EmailTriageFactTrace {
            condition_id: id,
            label: fact_label(id).to_string(),
            value: fact.tri_value().to_contract(),
            source: fact.source,
            detail: fact.detail.clone(),
        });
        if fact.unknown {
            None
        } else {
            Some(fact.values)
        }
    }

    fn accounting_customer(&self) -> (TriValue, EmailTriageFactSource, String) {
        let Some(conn) = self.conn else {
            return (
                TriValue::Unknown,
                EmailTriageFactSource::AccountingSnapshot,
                "couldn't check yet (local snapshot unavailable)".to_string(),
            );
        };
        let Some(email) = self.subjects.email.as_deref() else {
            return (
                TriValue::Unknown,
                EmailTriageFactSource::AccountingSnapshot,
                "couldn't check yet (sender email is unclear)".to_string(),
            );
        };
        match conn
            .query_row(
                "SELECT 1 FROM accounting_customer_snapshots \
                 WHERE client_id = ?1 AND lower(email) = lower(?2) AND active = 1 LIMIT 1",
                params![self.client_id, email],
                |_| Ok(()),
            )
            .optional()
        {
            Ok(Some(())) => (
                TriValue::True,
                EmailTriageFactSource::AccountingSnapshot,
                "found in accounting snapshots".to_string(),
            ),
            Ok(None) => (
                TriValue::False,
                EmailTriageFactSource::AccountingSnapshot,
                "not found in accounting snapshots".to_string(),
            ),
            Err(_) => (
                TriValue::Unknown,
                EmailTriageFactSource::AccountingSnapshot,
                "couldn't check yet (local snapshot error)".to_string(),
            ),
        }
    }

    fn accounting_invoice(&self, overdue: bool) -> (TriValue, EmailTriageFactSource, String) {
        let Some(conn) = self.conn else {
            return (
                TriValue::Unknown,
                EmailTriageFactSource::AccountingSnapshot,
                "couldn't check yet (local snapshot unavailable)".to_string(),
            );
        };
        let Some(email) = self.subjects.email.as_deref() else {
            return (
                TriValue::Unknown,
                EmailTriageFactSource::AccountingSnapshot,
                "couldn't check yet (sender email is unclear)".to_string(),
            );
        };
        let due_clause = if overdue {
            "AND i.due_date IS NOT NULL AND date(i.due_date) < date('now')"
        } else {
            ""
        };
        let sql = format!(
            "SELECT 1 FROM accounting_customer_snapshots c \
             JOIN accounting_invoice_snapshots i \
               ON i.client_id = c.client_id AND i.customer_id = c.provider_customer_id \
             WHERE c.client_id = ?1 AND lower(c.email) = lower(?2) \
               AND c.active = 1 AND i.balance_cents > 0 AND i.voided = 0 {due_clause} LIMIT 1"
        );
        match conn
            .query_row(&sql, params![self.client_id, email], |_| Ok(()))
            .optional()
        {
            Ok(Some(())) => (
                TriValue::True,
                EmailTriageFactSource::AccountingSnapshot,
                if overdue {
                    "found overdue invoice in accounting snapshots"
                } else {
                    "found open invoice in accounting snapshots"
                }
                .to_string(),
            ),
            Ok(None) => (
                TriValue::False,
                EmailTriageFactSource::AccountingSnapshot,
                "not found in accounting snapshots".to_string(),
            ),
            Err(_) => (
                TriValue::Unknown,
                EmailTriageFactSource::AccountingSnapshot,
                "couldn't check yet (local snapshot error)".to_string(),
            ),
        }
    }

    fn workflow_open_item(&self) -> (TriValue, EmailTriageFactSource, String) {
        let Some(conn) = self.conn else {
            return (
                TriValue::Unknown,
                EmailTriageFactSource::Workflow,
                "couldn't check yet (work queue unavailable)".to_string(),
            );
        };
        let Some(message_id) = self.message_id else {
            return (
                TriValue::Unknown,
                EmailTriageFactSource::Workflow,
                "couldn't check yet (message id unavailable)".to_string(),
            );
        };
        match conn
            .query_row(
                "SELECT 1 FROM work_items \
                 WHERE client_id = ?1 AND source_kind = 'email' AND source_ref = ?2 \
                   AND status IN ('open', 'accepted') LIMIT 1",
                params![self.client_id, message_id],
                |_| Ok(()),
            )
            .optional()
        {
            Ok(Some(())) => (
                TriValue::True,
                EmailTriageFactSource::Workflow,
                "open work already exists for this email".to_string(),
            ),
            Ok(None) => (
                TriValue::False,
                EmailTriageFactSource::Workflow,
                "no open work exists for this email".to_string(),
            ),
            Err(_) => (
                TriValue::Unknown,
                EmailTriageFactSource::Workflow,
                "couldn't check yet (work queue error)".to_string(),
            ),
        }
    }
}

fn exists_text(
    value: Option<&str>,
    source: EmailTriageFactSource,
) -> (TriValue, EmailTriageFactSource, String) {
    (
        if value.is_some_and(|value| !value.trim().is_empty()) {
            TriValue::True
        } else {
            TriValue::False
        },
        source,
        "read from message".to_string(),
    )
}

pub fn fact_label(id: EmailTriageConditionId) -> &'static str {
    match id {
        EmailTriageConditionId::MessageFrom => "From line",
        EmailTriageConditionId::MessageTo => "To line",
        EmailTriageConditionId::MessageFromEmail => "Sender email",
        EmailTriageConditionId::MessageFromDomain => "Sender domain",
        EmailTriageConditionId::MessageFromDomainIsBusiness => {
            "Sender uses a business email domain"
        }
        EmailTriageConditionId::MessageSubject => "Subject",
        EmailTriageConditionId::MessageBody => "Message body",
        EmailTriageConditionId::MessageLabel => "Mailbox label",
        EmailTriageConditionId::MessageHeader => "Message header",
        EmailTriageConditionId::SourceAccountUserId => "Connected mailbox user",
        EmailTriageConditionId::SourceProvider => "Mail provider",
        EmailTriageConditionId::CrmSenderContactExists => "Sender is a saved contact",
        EmailTriageConditionId::CrmSenderCompanyExists => "Sender's company is a known account",
        EmailTriageConditionId::CrmSenderDealExists => "Sender has an associated deal",
        EmailTriageConditionId::CrmSenderDealStage => "Sender deal stage",
        EmailTriageConditionId::CrmSenderDealPipeline => "Sender deal pipeline",
        EmailTriageConditionId::AccountingSenderCustomerExists => {
            "Sender is an accounting customer"
        }
        EmailTriageConditionId::AccountingSenderHasOpenInvoice => "Sender has an open invoice",
        EmailTriageConditionId::AccountingSenderHasOverdueInvoice => {
            "Sender has an overdue invoice"
        }
        EmailTriageConditionId::WorkflowThreadHasOpenItem => "This email already has open work",
        EmailTriageConditionId::QuickKnownCustomer => "Known customer",
        EmailTriageConditionId::QuickNewSalesLead => "New sales lead",
        EmailTriageConditionId::QuickBillingFollowup => "Billing follow-up",
        EmailTriageConditionId::QuickExistingWorkThread => "Existing work thread",
    }
}

fn email_domain(email: &str) -> Option<String> {
    let domain = email
        .split_once('@')?
        .1
        .trim()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    (!domain.is_empty()).then_some(domain)
}
