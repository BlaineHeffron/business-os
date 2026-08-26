//! One-way in-memory upgrade from legacy rule conditions to V2 conditions.

use bos_contracts::email_triage::{
    EmailTriageCondition, EmailTriageConditionId, EmailTriageConditionOperator,
    EmailTriageConditionV2, EmailTriageConditionValue, EmailTriageField, EmailTriageOperator,
    EmailTriageRule,
};

pub fn effective_conditions(rule: &EmailTriageRule) -> Vec<EmailTriageConditionV2> {
    if !rule.conditions_v2.is_empty() {
        return rule.conditions_v2.clone();
    }
    rule.conditions.iter().map(upgrade_condition).collect()
}

pub fn upgrade_condition(condition: &EmailTriageCondition) -> EmailTriageConditionV2 {
    let condition_id = match condition.field {
        EmailTriageField::Label => EmailTriageConditionId::MessageLabel,
        EmailTriageField::From => EmailTriageConditionId::MessageFrom,
        EmailTriageField::To => EmailTriageConditionId::MessageTo,
        EmailTriageField::Subject => EmailTriageConditionId::MessageSubject,
        EmailTriageField::Body => EmailTriageConditionId::MessageBody,
        EmailTriageField::Header => EmailTriageConditionId::MessageHeader,
        EmailTriageField::SenderInCrmContacts => EmailTriageConditionId::CrmSenderContactExists,
        EmailTriageField::SenderDomainInCrmCompanies => {
            EmailTriageConditionId::CrmSenderCompanyExists
        }
    };
    let op = match (condition.field, condition.op) {
        (
            EmailTriageField::SenderInCrmContacts | EmailTriageField::SenderDomainInCrmCompanies,
            EmailTriageOperator::Exists,
        ) => EmailTriageConditionOperator::IsTrue,
        (_, op) => match op {
            EmailTriageOperator::Contains => EmailTriageConditionOperator::Contains,
            EmailTriageOperator::Equals => EmailTriageConditionOperator::Equals,
            EmailTriageOperator::StartsWith => EmailTriageConditionOperator::StartsWith,
            EmailTriageOperator::Regex => EmailTriageConditionOperator::Regex,
            EmailTriageOperator::Exists => EmailTriageConditionOperator::Exists,
        },
    };
    let value = match condition.field {
        EmailTriageField::Header => EmailTriageConditionValue::Header {
            name: condition.header_name.clone().unwrap_or_default(),
            value: condition.value.clone(),
        },
        EmailTriageField::SenderInCrmContacts | EmailTriageField::SenderDomainInCrmCompanies => {
            if condition.op == EmailTriageOperator::Equals {
                EmailTriageConditionValue::Bool(condition.value.trim().eq_ignore_ascii_case("true"))
            } else {
                EmailTriageConditionValue::Empty
            }
        }
        _ => EmailTriageConditionValue::Text(condition.value.clone()),
    };
    EmailTriageConditionV2 {
        condition_id,
        op,
        value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bos_contracts::email_triage::{EmailTriageCondition, EmailTriageField};

    #[test]
    fn legacy_from_to_map_to_raw_message_facts() {
        let from = upgrade_condition(&EmailTriageCondition {
            field: EmailTriageField::From,
            op: EmailTriageOperator::Contains,
            value: "Acme".to_string(),
            header_name: None,
        });
        let to = upgrade_condition(&EmailTriageCondition {
            field: EmailTriageField::To,
            op: EmailTriageOperator::Contains,
            value: "Ops".to_string(),
            header_name: None,
        });
        assert_eq!(from.condition_id, EmailTriageConditionId::MessageFrom);
        assert_eq!(to.condition_id, EmailTriageConditionId::MessageTo);
    }
}
