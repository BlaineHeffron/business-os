use bos_profile_api::{
    QuoteProfileConfig, QuoteProfileDraft, QuoteProfileError, QuoteProfileInput,
    QuoteProfileLineItem, QuoteProfileRun, QuoteProfileStep, QuoteProfileStepKind,
    QuoteWorkflowProfile, TracedValue,
};

pub const BUILT_IN_PROFILE_ID: &str = "built_in";

static BUILT_IN_PROFILE: BuiltInQuoteProfile = BuiltInQuoteProfile;

pub fn select_profile(id: &str) -> Option<&'static dyn QuoteWorkflowProfile> {
    match id {
        BUILT_IN_PROFILE_ID => Some(&BUILT_IN_PROFILE),
        _ => None,
    }
}

pub fn available_profile_ids() -> Vec<&'static str> {
    vec![BUILT_IN_PROFILE_ID]
}

pub fn validate_profile_config(
    profile_id: &str,
    raw_config: serde_json::Value,
) -> Result<(), String> {
    let Some(profile) = select_profile(profile_id) else {
        return Err(format!(
            "unknown quote_workflows.profile '{profile_id}' (available: {})",
            available_profile_ids().join(", ")
        ));
    };
    profile
        .parse_config(raw_config)
        .map(|_| ())
        .map_err(|err| err.to_string())
}

struct BuiltInQuoteProfile;

impl QuoteWorkflowProfile for BuiltInQuoteProfile {
    fn profile_id(&self) -> &'static str {
        BUILT_IN_PROFILE_ID
    }

    fn parse_config(
        &self,
        raw: serde_json::Value,
    ) -> Result<QuoteProfileConfig, QuoteProfileError> {
        if !raw.is_null() && !raw.as_object().is_some_and(|obj| obj.is_empty()) {
            return Err(QuoteProfileError::new(
                "quote_profile_config_invalid",
                "built_in quote profile does not accept profile-specific config",
            ));
        }
        Ok(QuoteProfileConfig {
            profile_id: self.profile_id().to_string(),
            settings: serde_json::Value::Null,
        })
    }

    fn run(
        &self,
        input: QuoteProfileInput,
        _config: QuoteProfileConfig,
    ) -> Result<QuoteProfileRun, QuoteProfileError> {
        validate_input(&input)?;
        let source_step = QuoteProfileStep {
            node: "gather_source".to_string(),
            kind: QuoteProfileStepKind::Read,
            inputs: vec![trace("source_ref", &input.source_ref, None, None, None)],
            outputs: vec![
                trace("customer_name", &input.customer_name, None, None, None),
                trace("request_text", &input.request_text, None, None, None),
            ],
            decision: Some("source_snapshot_captured".to_string()),
        };

        let line_items = parse_line_items(&input)?;
        let parse_step = QuoteProfileStep {
            node: "parse_request".to_string(),
            kind: QuoteProfileStepKind::Deterministic,
            inputs: vec![trace("request_text", &input.request_text, None, None, None)],
            outputs: vec![trace(
                "line_item_count",
                line_items.len(),
                Some("count"),
                None,
                None,
            )],
            decision: Some(format!("parsed_{}_line_items", line_items.len())),
        };

        validate_grounding(&input, &line_items)?;
        let grounding_step = QuoteProfileStep {
            node: "validate_grounding".to_string(),
            kind: QuoteProfileStepKind::Grounding,
            inputs: line_items
                .iter()
                .map(|item| {
                    trace(
                        format!("source_quote:{}", item.sku),
                        &item.source_quote,
                        None,
                        None,
                        Some("request_text"),
                    )
                })
                .collect(),
            outputs: vec![trace("grounded", true, None, None, None)],
            decision: Some("all_line_items_grounded_to_source_quotes".to_string()),
        };

        let policy_notes = policy_review(&line_items);
        let subtotal_cents: i64 = line_items.iter().map(|item| item.total_cents).sum();
        let policy_step = QuoteProfileStep {
            node: "policy".to_string(),
            kind: QuoteProfileStepKind::Policy,
            inputs: vec![trace(
                "subtotal",
                subtotal_cents,
                Some("cents"),
                Some("sum(line_items.total_cents)"),
                None,
            )],
            outputs: policy_notes
                .iter()
                .enumerate()
                .map(|(index, note)| {
                    trace(format!("policy_note_{}", index + 1), note, None, None, None)
                })
                .collect(),
            decision: Some("ready_for_operator_review".to_string()),
        };

        let draft = QuoteProfileDraft {
            summary: format!("Quote for {}", input.customer_name),
            line_items,
            policy_notes,
        };
        let stage_step = QuoteProfileStep {
            node: "stage_draft".to_string(),
            kind: QuoteProfileStepKind::Stage,
            inputs: vec![trace("summary", &draft.summary, None, None, None)],
            outputs: vec![trace(
                "subtotal",
                draft
                    .line_items
                    .iter()
                    .map(|item| item.total_cents)
                    .sum::<i64>(),
                Some("cents"),
                None,
                None,
            )],
            decision: Some("quote_draft_staged".to_string()),
        };

        Ok(QuoteProfileRun {
            steps: vec![
                source_step,
                parse_step,
                grounding_step,
                policy_step,
                stage_step,
            ],
            draft,
        })
    }
}

fn validate_input(input: &QuoteProfileInput) -> Result<(), QuoteProfileError> {
    if input.customer_name.trim().is_empty() {
        return Err(QuoteProfileError::new(
            "quote_customer_required",
            "quote customer is required",
        ));
    }
    if input.request_text.trim().is_empty() {
        return Err(QuoteProfileError::new(
            "quote_request_text_required",
            "quote request text is required",
        ));
    }
    Ok(())
}

fn parse_line_items(
    input: &QuoteProfileInput,
) -> Result<Vec<QuoteProfileLineItem>, QuoteProfileError> {
    let mut items = Vec::new();
    for (index, raw) in input.request_text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let quantity = first_u32(line).unwrap_or(1);
        let unit_cents = first_money_cents(line).ok_or_else(|| {
            QuoteProfileError::new(
                "quote_line_amount_required",
                "quote line amount is required",
            )
        })?;
        let total_cents = unit_cents * i64::from(quantity);
        items.push(QuoteProfileLineItem {
            sku: format!("LINE-{}", index + 1),
            product_line: None,
            description: line.to_string(),
            quantity,
            unit_cents,
            total_cents,
            source_quote: line.to_string(),
        });
    }
    if items.is_empty() {
        return Err(QuoteProfileError::new(
            "quote_line_item_required",
            "quote request must contain at least one line item",
        ));
    }
    Ok(items)
}

fn validate_grounding(
    input: &QuoteProfileInput,
    items: &[QuoteProfileLineItem],
) -> Result<(), QuoteProfileError> {
    for item in items {
        if item.source_quote.trim().is_empty() || !input.request_text.contains(&item.source_quote) {
            return Err(QuoteProfileError::new(
                "quote_line_not_grounded",
                "quote line is not grounded in the request text",
            ));
        }
        if item.total_cents <= 0 {
            return Err(QuoteProfileError::new(
                "quote_line_amount_invalid",
                "quote line amount must be positive",
            ));
        }
    }
    Ok(())
}

fn policy_review(items: &[QuoteProfileLineItem]) -> Vec<String> {
    let subtotal: i64 = items.iter().map(|item| item.total_cents).sum();
    if subtotal >= 100_000 {
        vec![
            "Subtotal is at least $1,000; operator should confirm margin before sending."
                .to_string(),
        ]
    } else {
        vec!["No deterministic policy blocks found.".to_string()]
    }
}

fn first_u32(line: &str) -> Option<u32> {
    line.split(|ch: char| !ch.is_ascii_digit())
        .find(|part| !part.is_empty())
        .and_then(|part| part.parse().ok())
}

fn first_money_cents(line: &str) -> Option<i64> {
    let bytes = line.as_bytes();
    let dollar = bytes.iter().position(|byte| *byte == b'$')?;
    let tail = &line[dollar + 1..];
    let raw = tail
        .chars()
        .take_while(|ch| ch.is_ascii_digit() || *ch == '.' || *ch == ',')
        .collect::<String>()
        .replace(',', "");
    if raw.is_empty() {
        return None;
    }
    let mut parts = raw.split('.');
    let dollars: i64 = parts.next()?.parse().ok()?;
    let cents = parts
        .next()
        .map(|part| {
            let mut cents = part.chars().take(2).collect::<String>();
            while cents.len() < 2 {
                cents.push('0');
            }
            cents.parse::<i64>().ok()
        })
        .unwrap_or(Some(0))?;
    Some(dollars * 100 + cents)
}

fn trace(
    label: impl Into<String>,
    value: impl serde::Serialize,
    unit: Option<&str>,
    formula: Option<&str>,
    source: Option<&str>,
) -> TracedValue {
    TracedValue {
        label: label.into(),
        value: serde_json::to_value(value).unwrap_or(serde_json::Value::Null),
        unit: unit.map(str::to_string),
        formula: formula.map(str::to_string),
        source: source.map(str::to_string),
    }
}
