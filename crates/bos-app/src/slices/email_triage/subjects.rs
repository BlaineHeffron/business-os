//! Sender-address helpers shared by email triage fact resolution and CRM lookup.

pub fn normalized_email_addresses(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or_default()
        .split([',', ';'])
        .filter_map(|part| {
            let trimmed = part.trim();
            if trimmed.is_empty() {
                return None;
            }
            let candidate = trimmed
                .rsplit_once('<')
                .and_then(|(_, rest)| rest.split_once('>').map(|(email, _)| email))
                .unwrap_or(trimmed)
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_ascii_lowercase();
            is_plausible_email(&candidate).then_some(candidate)
        })
        .collect()
}

pub fn first_normalized_email(raw: Option<&str>) -> Option<String> {
    normalized_email_addresses(raw).into_iter().next()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SenderIdentityFacts {
    pub email: String,
    pub local_part: String,
    pub domain: String,
    pub automation_local_part: bool,
    pub header_block_reason: Option<&'static str>,
}

pub fn sender_identity_facts(
    raw: Option<&str>,
    headers: &[(String, String)],
) -> Option<SenderIdentityFacts> {
    let email = first_normalized_email(raw)?;
    let (local, domain) = email.split_once('@')?;
    Some(SenderIdentityFacts {
        email: email.clone(),
        local_part: local.to_string(),
        domain: domain.to_string(),
        automation_local_part: automation_local_part(local),
        header_block_reason: crm_header_identity_block_reason(headers),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrmSenderPolicy {
    neutral_domain_roots: Vec<String>,
}

impl CrmSenderPolicy {
    pub fn from_domain_roots(raw: Option<&str>) -> Self {
        let neutral_domain_roots = raw
            .unwrap_or_default()
            .split([',', ';', '\n'])
            .filter_map(normalize_domain_root)
            .collect();
        Self {
            neutral_domain_roots,
        }
    }

    pub fn is_neutral_sender(&self, email: &str) -> bool {
        let Some((local, domain)) = email.split_once('@') else {
            return false;
        };
        automation_local_part(local)
            && self
                .neutral_domain_roots
                .iter()
                .any(|root| domain_matches_root(domain, root))
    }

    pub fn neutral_domain_roots(&self) -> &[String] {
        &self.neutral_domain_roots
    }
}

pub fn crm_lookup_email(
    raw: Option<&str>,
    policy: &CrmSenderPolicy,
) -> Result<String, &'static str> {
    let Some(email) = first_normalized_email(raw) else {
        return Err("sender_email_unclear");
    };
    if policy.is_neutral_sender(&email) {
        return Err("sender_is_platform_or_automation");
    }
    Ok(email)
}

pub fn crm_header_identity_block_reason(headers: &[(String, String)]) -> Option<&'static str> {
    for (name, value) in headers {
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim().to_ascii_lowercase();
        if name == "auto-submitted" && !value.is_empty() && value != "no" {
            return Some("automated_email_headers");
        }
        if matches!(
            name.as_str(),
            "list-id" | "list-owner" | "list-post" | "list-unsubscribe"
        ) {
            return Some("mailing_list_headers");
        }
        if name == "precedence" && matches!(value.as_str(), "bulk" | "junk" | "list") {
            return Some("bulk_email_headers");
        }
    }
    None
}

pub fn is_plausible_email(value: &str) -> bool {
    let Some((local, domain)) = value.split_once('@') else {
        return false;
    };
    !local.trim().is_empty()
        && !domain.trim().is_empty()
        && domain.contains('.')
        && !domain.contains(' ')
}

pub fn public_mailbox_domain(domain: &str) -> bool {
    matches!(
        domain,
        "gmail.com"
            | "googlemail.com"
            | "outlook.com"
            | "hotmail.com"
            | "live.com"
            | "msn.com"
            | "yahoo.com"
            | "icloud.com"
            | "me.com"
            | "mac.com"
            | "aol.com"
            | "proton.me"
            | "protonmail.com"
    )
}

fn normalize_domain_root(raw: &str) -> Option<String> {
    let domain = raw.trim().trim_end_matches('.').to_ascii_lowercase();
    if domain.is_empty() || domain.contains('@') || domain.contains(' ') || !domain.contains('.') {
        return None;
    }
    Some(domain)
}

fn domain_matches_root(domain: &str, root: &str) -> bool {
    let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
    domain == root || domain.ends_with(&format!(".{root}"))
}

fn automation_local_part(local: &str) -> bool {
    let normalized = local
        .trim()
        .trim_matches('"')
        .to_ascii_lowercase()
        .replace(['_', '.'], "-");
    if ["do-not-reply", "donotreply", "no-reply", "noreply"]
        .iter()
        .any(|prefix| {
            normalized == *prefix
                || normalized
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with(['+', '-']))
        })
    {
        return true;
    }
    let first_token = normalized.split(['+', '-']).next().unwrap_or_default();
    matches!(
        first_token,
        "automated"
            | "automation"
            | "auto"
            | "bounce"
            | "bounces"
            | "email"
            | "mailer"
            | "mail"
            | "notification"
            | "notifications"
            | "postmaster"
            | "receipt"
            | "receipts"
            | "robot"
            | "update"
            | "updates"
    )
}
