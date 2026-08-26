//! Pure, deterministic Tier-0 triage. No LLM, no network. Subject is consumed here
//! and never returned. Output is a classification routing token consumed by the
//! caller's packet machinery (deterministic rules run before any AI classifier).

/// Map (subject, sender domain, label ids) to a classification routing token.
/// Token set is the vocabulary `packet_category` understands. Subject is read
/// only here and never escapes.
pub fn classify(subject: &str, sender_domain: Option<&str>, label_ids: &[String]) -> &'static str {
    let subject_lc = subject.to_ascii_lowercase();
    let labels_lc: Vec<String> = label_ids.iter().map(|l| l.to_ascii_lowercase()).collect();

    if labels_lc
        .iter()
        .any(|l| l.contains("promotions") || l.contains("category_updates"))
    {
        return "informational";
    }
    if contains_any(
        &subject_lc,
        &["unsubscribe", "newsletter", "no-reply", "noreply"],
    ) {
        return "informational";
    }
    if contains_any(
        &subject_lc,
        &[
            "meeting",
            "schedule",
            "scheduling",
            "calendar",
            "call on",
            "available",
        ],
    ) {
        return "calendar";
    }
    if contains_any(&subject_lc, &["quote", "proposal", "pricing", "estimate"]) {
        return "sales";
    }
    if contains_any(
        &subject_lc,
        &["invoice", "payment", "billing", "receipt", "refund"],
    ) {
        return "billing";
    }
    if contains_any(
        &subject_lc,
        &[
            "issue",
            "problem",
            "broken",
            "not working",
            "exception",
            "support",
            "help",
        ],
    ) {
        return "support";
    }
    if contains_any(&subject_lc, &["order", "shipment", "tracking", "delivery"]) {
        return "order";
    }
    let _ = sender_domain; // reserved for future domain-class rules (named hook)
    "human"
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| haystack.contains(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn billing_subject_routes_to_billing() {
        let token = classify(
            "Your invoice #1042 is ready",
            Some("acme.com"),
            &["INBOX".into()],
        );
        assert_eq!(token, "billing");
    }

    #[test]
    fn calendar_subject_routes_to_calendar() {
        let token = classify(
            "Can we schedule a meeting next week?",
            Some("acme.com"),
            &["INBOX".into()],
        );
        assert_eq!(token, "calendar");
    }

    #[test]
    fn support_subject_routes_to_support() {
        let token = classify(
            "Issue with my recent order",
            Some("acme.com"),
            &["INBOX".into()],
        );
        assert_eq!(token, "support");
    }

    #[test]
    fn promotions_category_label_routes_to_informational() {
        let token = classify(
            "Spring sale newsletter",
            None,
            &["CATEGORY_PROMOTIONS".into()],
        );
        assert_eq!(token, "informational");
    }

    #[test]
    fn unknown_subject_falls_back_to_human() {
        let token = classify("hello", Some("acme.com"), &["INBOX".into()]);
        assert_eq!(token, "human");
    }
}
