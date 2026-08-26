use std::sync::OnceLock;

use bos_contracts::quote_workflows::{
    QuoteApprovalRoute, QuoteDraftActionKind, QuoteGuardrailEvaluation, QuoteGuardrailFinding,
    QuoteGuardrailSeverity, QuoteGuardrailStatus, QuoteLineItem, QuoteWorkflowInspection,
    QuoteWorkflowRunResponse, WorkflowRunStatus, WorkflowTraceValue,
};
use bos_profile_api::{
    QuoteProfileDraft, QuoteProfileInput, QuoteProfileLineItem, QuoteProfileStep,
    QuoteProfileStepKind, TracedValue,
};
use sha2::{Digest, Sha256};
use tokio::sync::{Semaphore, SemaphorePermit};

use crate::outbox::{AttemptOutcome, ClaimedJob, NewOutboxJob};
use crate::store_core::StoreError;

use super::profiles;
use super::store::{
    self, DraftActionContext, QuoteWorkflowInput, RunFinishContext, StepRecord, Trace,
    TraceResumeContext, TraceStartContext, CAPABILITY_STAGE_QUOTE_DRAFT, PROVIDER_QUOTE_WORKFLOW,
};

const MAX_CONCURRENT_RUNS: usize = 2;
static RUN_SEMAPHORE: OnceLock<Semaphore> = OnceLock::new();

pub struct QuoteRunContext<'a> {
    pub client_id: &'a str,
    pub actor_id: &'a str,
    pub profile_id: &'a str,
    pub profile_config_json: serde_json::Value,
    pub guardrail_config_json: serde_json::Value,
    pub request_idempotency_key: &'a str,
    pub now_ms: u64,
}

pub struct StartedQuoteRun {
    client_id: String,
    actor_id: String,
    run_id: String,
    profile_id: String,
    profile_config_json: serde_json::Value,
    guardrail_config_json: serde_json::Value,
    input: QuoteWorkflowInput,
    request_idempotency_key: String,
    start_receipt_id: String,
    now_ms: u64,
}

pub struct PreparedQuoteRun {
    started: StartedQuoteRun,
    profile_run: bos_profile_api::QuoteProfileRun,
}

pub struct QuoteRunPermit {
    _permit: SemaphorePermit<'static>,
}

pub type QuotePrepareError = Box<(StartedQuoteRun, StoreError)>;

pub fn run_quote_builder(
    conn: &mut rusqlite::Connection,
    input: QuoteWorkflowInput,
    ctx: QuoteRunContext<'_>,
) -> Result<QuoteWorkflowRunResponse, StoreError> {
    let run_id = quote_run_id(ctx.request_idempotency_key);
    if let Some(response) = existing_run_response(conn, ctx.client_id, &run_id)? {
        return Ok(response);
    }
    let permit = try_acquire_quote_run()?;
    let started = start_quote_builder(conn, input, ctx)?;
    let prepared = match prepare_quote_builder(started, permit) {
        Ok(prepared) => prepared,
        Err(failure) => {
            let (started, err) = *failure;
            fail_started_quote_builder(conn, &started, &err, started.now_ms + 1)?;
            return Err(err);
        }
    };
    persist_prepared_quote_builder(conn, prepared)
}

pub fn quote_run_id(idempotency_key: &str) -> String {
    run_id_for(idempotency_key)
}

pub fn existing_run_response(
    conn: &rusqlite::Connection,
    client_id: &str,
    run_id: &str,
) -> Result<Option<QuoteWorkflowRunResponse>, StoreError> {
    let Some(run) = store::get_run(conn, client_id, run_id)? else {
        return Ok(None);
    };
    let draft = store::draft_for_run(conn, client_id, run_id)?;
    Ok(Some(QuoteWorkflowRunResponse { run, draft }))
}

pub fn try_acquire_quote_run() -> Result<QuoteRunPermit, StoreError> {
    let semaphore = RUN_SEMAPHORE.get_or_init(|| Semaphore::new(MAX_CONCURRENT_RUNS));
    semaphore
        .try_acquire()
        .map(|permit| QuoteRunPermit { _permit: permit })
        .map_err(|_| StoreError::Domain("quote_workflow_busy".to_string()))
}

pub fn start_quote_builder(
    conn: &mut rusqlite::Connection,
    input: QuoteWorkflowInput,
    ctx: QuoteRunContext<'_>,
) -> Result<StartedQuoteRun, StoreError> {
    let run_id = quote_run_id(ctx.request_idempotency_key);
    let trace = Trace::start(
        conn,
        TraceStartContext {
            client_id: ctx.client_id,
            actor_id: ctx.actor_id,
            run_id: &run_id,
            profile_id: ctx.profile_id,
            idempotency_key: &format!("{run_id}:start"),
            now_ms: ctx.now_ms,
        },
        &input,
    )?;
    let start_receipt_id = trace
        .last_receipt_id()
        .ok_or_else(|| StoreError::Domain("quote_workflow_start_receipt_missing".to_string()))?
        .to_string();
    drop(trace);
    Ok(StartedQuoteRun {
        client_id: ctx.client_id.to_string(),
        actor_id: ctx.actor_id.to_string(),
        run_id,
        profile_id: ctx.profile_id.to_string(),
        profile_config_json: ctx.profile_config_json,
        guardrail_config_json: ctx.guardrail_config_json,
        input,
        request_idempotency_key: ctx.request_idempotency_key.to_string(),
        start_receipt_id,
        now_ms: ctx.now_ms,
    })
}

pub fn prepare_quote_builder(
    started: StartedQuoteRun,
    _permit: QuoteRunPermit,
) -> Result<PreparedQuoteRun, QuotePrepareError> {
    let profile = match profiles::select_profile(&started.profile_id) {
        Some(profile) => profile,
        None => {
            let code = format!("quote_profile_unknown:{}", started.profile_id);
            return Err(prepare_error(started, StoreError::Domain(code)));
        }
    };
    let profile_config = match profile.parse_config(started.profile_config_json.clone()) {
        Ok(config) => config,
        Err(err) => return Err(prepare_error(started, StoreError::Domain(err.code))),
    };
    let profile_run = match profile.run(profile_input(&started.input), profile_config) {
        Ok(run) => run,
        Err(err) => return Err(prepare_error(started, StoreError::Domain(err.code))),
    };
    if let Err(err) = validate_profile_run(&started.input, &profile_run) {
        return Err(prepare_error(started, err));
    }
    if let Err(err) = validate_profile_steps(&profile_run.steps) {
        return Err(prepare_error(started, err));
    }
    Ok(PreparedQuoteRun {
        started,
        profile_run,
    })
}

fn prepare_error(started: StartedQuoteRun, err: StoreError) -> QuotePrepareError {
    Box::new((started, err))
}

pub fn fail_started_quote_builder(
    conn: &mut rusqlite::Connection,
    started: &StartedQuoteRun,
    err: &StoreError,
    now_ms: u64,
) -> Result<String, StoreError> {
    let error_code = match err {
        StoreError::Domain(code) => code.as_str(),
        StoreError::Sqlite(_) => "store_sqlite_error",
    };
    store::finish_run(
        conn,
        RunFinishContext {
            client_id: &started.client_id,
            actor_id: &started.actor_id,
            run_id: &started.run_id,
            causation_id: Some(&started.start_receipt_id),
            now_ms,
        },
        WorkflowRunStatus::Failed,
        serde_json::json!({
            "profile_id": started.profile_id,
            "error_code": error_code,
        }),
    )
}

pub fn persist_prepared_quote_builder(
    conn: &mut rusqlite::Connection,
    prepared: PreparedQuoteRun,
) -> Result<QuoteWorkflowRunResponse, StoreError> {
    let started = prepared.started;
    debug_assert_eq!(
        quote_run_id(&started.request_idempotency_key),
        started.run_id
    );
    let mut trace = Trace::resume(
        conn,
        TraceResumeContext {
            client_id: &started.client_id,
            actor_id: &started.actor_id,
            run_id: &started.run_id,
            last_receipt_id: &started.start_receipt_id,
        },
    );

    let stage_offset = prepared.profile_run.steps.len() as u64;
    let stage_step =
        persist_profile_steps(&mut trace, &prepared.profile_run.steps, started.now_ms + 1)?;
    let draft = draft_from_profile_run(
        &started.run_id,
        &started.input,
        prepared.profile_run.draft,
        &started.guardrail_config_json,
        started.now_ms + stage_offset,
    )?;
    trace.stage_draft(&draft, stage_step, started.now_ms + stage_offset)?;
    trace.finish(
        WorkflowRunStatus::Staged,
        serde_json::json!({
            "draft_id": draft.draft_id,
            "profile_id": started.profile_id,
        }),
        started.now_ms + stage_offset + 1,
    )?;
    drop(trace);

    let run = store::get_run(conn, &started.client_id, &started.run_id)?
        .ok_or_else(|| StoreError::Domain("quote_workflow_run_missing".to_string()))?;
    let draft = store::draft_for_run(conn, &started.client_id, &started.run_id)?;
    Ok(QuoteWorkflowRunResponse { run, draft })
}

pub fn inspect_run(
    conn: &rusqlite::Connection,
    client_id: &str,
    run_id: &str,
) -> Result<Option<QuoteWorkflowInspection>, StoreError> {
    let Some(run) = store::get_run(conn, client_id, run_id)? else {
        return Ok(None);
    };
    let ids = vec![run_id.to_string()];
    Ok(Some(QuoteWorkflowInspection {
        run,
        steps: store::steps_for_run(conn, client_id, run_id)?,
        draft: store::draft_for_run(conn, client_id, run_id)?,
        receipts: crate::store_core::receipts_by_correlation(conn, client_id, &ids, 200)?,
        outbox_jobs: crate::outbox::jobs_by_correlation(conn, client_id, &ids, 50)?,
    }))
}

pub fn apply_draft_action(
    conn: &mut rusqlite::Connection,
    ctx: DraftActionContext<'_>,
    draft_id: &str,
    action: QuoteDraftActionKind,
) -> Result<crate::store_core::MutationOutcome, StoreError> {
    match action {
        QuoteDraftActionKind::Approve => {
            let draft = store::get_draft(conn, ctx.client_id, draft_id)?
                .ok_or_else(|| StoreError::Domain("quote_draft_not_found".to_string()))?
                .draft;
            require_guardrail_approver(&draft.guardrails, ctx.actor_id)?;
            let job = NewOutboxJob {
                job_id: format!("qwj_{}", draft.run_id),
                provider: PROVIDER_QUOTE_WORKFLOW.to_string(),
                capability: CAPABILITY_STAGE_QUOTE_DRAFT.to_string(),
                payload_json: store::quote_payload(&draft)?,
                source_entity_kind: store::DRAFT_ENTITY_KIND.to_string(),
                source_entity_id: draft.draft_id.clone(),
                correlation_id: Some(draft.run_id.clone()),
                causation_id: None,
                idempotency_key: format!("quote_outbox:{}", draft.run_id),
            };
            store::approve_draft(conn, ctx, draft_id, &job)
        }
        QuoteDraftActionKind::Reject => store::reject_draft(conn, ctx, draft_id),
    }
}

pub fn validate_guardrail_config_json(config_json: serde_json::Value) -> Result<(), String> {
    parse_guardrail_config(&config_json)
        .map(|_| ())
        .map_err(|err| err.to_string())
}

pub fn deliver(job: &ClaimedJob, now_ms: u64) -> AttemptOutcome {
    AttemptOutcome::Delivered {
        result_json: serde_json::json!({
            "dry_run": true,
            "provider_object_id": format!("quote_draft:{}", job.job_id),
            "delivered_at_ms": now_ms,
        })
        .to_string(),
    }
}

fn persist_profile_steps(
    trace: &mut Trace<'_>,
    steps: &[QuoteProfileStep],
    now_ms: u64,
) -> Result<StepRecord, StoreError> {
    validate_profile_steps(steps)?;
    let stage_index = steps
        .iter()
        .position(|step| step.kind == QuoteProfileStepKind::Stage)
        .ok_or_else(|| StoreError::Domain("quote_profile_stage_step_missing".to_string()))?;
    if stage_index + 1 != steps.len() {
        return Err(StoreError::Domain(
            "quote_profile_stage_step_must_be_last".to_string(),
        ));
    }
    for (index, step) in steps.iter().take(stage_index).enumerate() {
        trace.step(step_record(step)?, now_ms + index as u64)?;
    }
    step_record(&steps[stage_index])
}

fn validate_profile_steps(steps: &[QuoteProfileStep]) -> Result<(), StoreError> {
    let stage_index = steps
        .iter()
        .position(|step| step.kind == QuoteProfileStepKind::Stage)
        .ok_or_else(|| StoreError::Domain("quote_profile_stage_step_missing".to_string()))?;
    if stage_index + 1 != steps.len() {
        return Err(StoreError::Domain(
            "quote_profile_stage_step_must_be_last".to_string(),
        ));
    }
    for step in steps {
        if step.node.trim().is_empty() {
            return Err(StoreError::Domain(
                "quote_profile_step_node_required".to_string(),
            ));
        }
    }
    Ok(())
}

pub(super) fn validate_profile_run(
    input: &QuoteWorkflowInput,
    run: &bos_profile_api::QuoteProfileRun,
) -> Result<(), StoreError> {
    if run.draft.summary.trim().is_empty() {
        return Err(StoreError::Domain("quote_summary_required".to_string()));
    }
    if run.draft.line_items.is_empty() {
        return Err(StoreError::Domain("quote_line_item_required".to_string()));
    }
    for item in &run.draft.line_items {
        validate_profile_line(input, item)?;
    }
    Ok(())
}

fn validate_profile_line(
    input: &QuoteWorkflowInput,
    item: &QuoteProfileLineItem,
) -> Result<(), StoreError> {
    if item.sku.trim().is_empty() {
        return Err(StoreError::Domain("quote_line_sku_required".to_string()));
    }
    if item.description.trim().is_empty() {
        return Err(StoreError::Domain(
            "quote_line_description_required".to_string(),
        ));
    }
    if item.quantity == 0 {
        return Err(StoreError::Domain(
            "quote_line_quantity_invalid".to_string(),
        ));
    }
    if item.unit_cents <= 0 || item.total_cents <= 0 {
        return Err(StoreError::Domain("quote_line_amount_invalid".to_string()));
    }
    let expected_total = item
        .unit_cents
        .checked_mul(i64::from(item.quantity))
        .ok_or_else(|| StoreError::Domain("quote_line_total_overflow".to_string()))?;
    if item.total_cents != expected_total {
        return Err(StoreError::Domain("quote_line_total_mismatch".to_string()));
    }
    if item.source_quote.trim().is_empty() || !input.request_text.contains(&item.source_quote) {
        return Err(StoreError::Domain("quote_line_not_grounded".to_string()));
    }
    Ok(())
}

fn step_record(step: &QuoteProfileStep) -> Result<StepRecord, StoreError> {
    Ok(StepRecord {
        node: step.node.clone(),
        node_kind: step_kind_str(step.kind).to_string(),
        input_hash: Some(hash_json(&step.inputs)?),
        output_hash: Some(hash_json(&step.outputs)?),
        decision: step.decision.clone(),
        inputs: step.inputs.iter().cloned().map(trace_value).collect(),
        outputs: step.outputs.iter().cloned().map(trace_value).collect(),
        llm_usage_json: None,
        latency_ms: 0,
        status: "succeeded".to_string(),
        error_code: None,
    })
}

fn step_kind_str(kind: QuoteProfileStepKind) -> &'static str {
    match kind {
        QuoteProfileStepKind::Read => "read",
        QuoteProfileStepKind::Deterministic => "deterministic",
        QuoteProfileStepKind::Grounding => "grounding",
        QuoteProfileStepKind::Policy => "policy",
        QuoteProfileStepKind::Stage => "stage",
    }
}

fn draft_from_profile_run(
    run_id: &str,
    input: &QuoteWorkflowInput,
    draft: QuoteProfileDraft,
    guardrail_config_json: &serde_json::Value,
    now_ms: u64,
) -> Result<bos_contracts::quote_workflows::QuoteDraft, StoreError> {
    let line_items = draft
        .line_items
        .into_iter()
        .map(line_item)
        .collect::<Vec<_>>();
    let guardrails = evaluate_guardrails(input, &line_items, guardrail_config_json)?;
    Ok(store::draft_from_interpretation(
        run_id,
        input,
        line_items,
        guardrails,
        draft.policy_notes,
        draft.summary,
        now_ms,
    ))
}

fn profile_input(input: &QuoteWorkflowInput) -> QuoteProfileInput {
    QuoteProfileInput {
        source_kind: input.source_kind.clone(),
        source_ref: input.source_ref.clone(),
        customer_name: input.customer_name.clone(),
        customer_tier: input.customer_tier.clone(),
        request_text: input.request_text.clone(),
    }
}

fn line_item(item: QuoteProfileLineItem) -> QuoteLineItem {
    QuoteLineItem {
        sku: item.sku,
        product_line: item.product_line,
        description: item.description,
        quantity: item.quantity,
        unit_cents: item.unit_cents,
        total_cents: item.total_cents,
        source_quote: item.source_quote,
    }
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
struct QuoteGuardrailConfig {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    routine_max_discount_bps: Option<u32>,
    #[serde(default)]
    major_change_threshold_bps: Option<u32>,
    #[serde(default)]
    major_change_approver_id: Option<String>,
    #[serde(default)]
    price_lists: Vec<QuoteGuardrailPrice>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
struct QuoteGuardrailPrice {
    sku: String,
    #[serde(default)]
    product_line: Option<String>,
    #[serde(default)]
    customer_tier: Option<String>,
    unit_cents: i64,
}

fn evaluate_guardrails(
    input: &QuoteWorkflowInput,
    line_items: &[QuoteLineItem],
    config_json: &serde_json::Value,
) -> Result<QuoteGuardrailEvaluation, StoreError> {
    let config = parse_guardrail_config(config_json)?;
    let config_snapshot_json = serde_json::to_value(&config)
        .map_err(|err| StoreError::Domain(format!("serialize quote guardrails config: {err}")))?;
    let config_hash = hash_json(&config_snapshot_json)?;
    if !config.enabled {
        return Ok(QuoteGuardrailEvaluation {
            status: QuoteGuardrailStatus::WithinGuardrails,
            config_hash,
            findings: Vec::new(),
            approval_routes: Vec::new(),
            config_snapshot_json,
        });
    }

    let mut findings = Vec::new();
    for line in line_items {
        let Some(price) = matching_price(&config.price_lists, input, line) else {
            if !config.price_lists.is_empty() {
                findings.push(QuoteGuardrailFinding {
                    code: "quote_price_list_missing".to_string(),
                    severity: QuoteGuardrailSeverity::Review,
                    message: format!("No configured price-list entry matched SKU {}", line.sku),
                    line_sku: Some(line.sku.clone()),
                    product_line: line.product_line.clone(),
                    customer_tier: input.customer_tier.clone(),
                    list_unit_cents: None,
                    quoted_unit_cents: Some(line.unit_cents),
                    delta_bps: None,
                    required_approver_id: None,
                });
            }
            continue;
        };
        if price.unit_cents <= 0 {
            return Err(StoreError::Domain(
                "quote_guardrail_price_unit_cents_invalid".to_string(),
            ));
        }
        let delta_bps = price_delta_bps(price.unit_cents, line.unit_cents)?;
        let discount_bps = delta_bps.max(0) as u32;
        if config
            .routine_max_discount_bps
            .is_some_and(|max| discount_bps > max)
        {
            findings.push(QuoteGuardrailFinding {
                code: "quote_discount_exceeds_routine_max".to_string(),
                severity: QuoteGuardrailSeverity::ApprovalRequired,
                message: format!(
                    "SKU {} is discounted {} bps from the configured tier price",
                    line.sku, discount_bps
                ),
                line_sku: Some(line.sku.clone()),
                product_line: line
                    .product_line
                    .clone()
                    .or_else(|| price.product_line.clone()),
                customer_tier: input
                    .customer_tier
                    .clone()
                    .or_else(|| price.customer_tier.clone()),
                list_unit_cents: Some(price.unit_cents),
                quoted_unit_cents: Some(line.unit_cents),
                delta_bps: Some(delta_bps),
                required_approver_id: config.major_change_approver_id.clone(),
            });
        }
        if config
            .major_change_threshold_bps
            .is_some_and(|threshold| delta_bps.unsigned_abs() > threshold)
        {
            findings.push(QuoteGuardrailFinding {
                code: "quote_major_price_change".to_string(),
                severity: QuoteGuardrailSeverity::Major,
                message: format!(
                    "SKU {} differs from the configured tier price by {} bps",
                    line.sku,
                    delta_bps.unsigned_abs()
                ),
                line_sku: Some(line.sku.clone()),
                product_line: line
                    .product_line
                    .clone()
                    .or_else(|| price.product_line.clone()),
                customer_tier: input
                    .customer_tier
                    .clone()
                    .or_else(|| price.customer_tier.clone()),
                list_unit_cents: Some(price.unit_cents),
                quoted_unit_cents: Some(line.unit_cents),
                delta_bps: Some(delta_bps),
                required_approver_id: config.major_change_approver_id.clone(),
            });
        }
    }

    let mut approval_routes = Vec::new();
    for approver_id in findings
        .iter()
        .filter_map(|finding| finding.required_approver_id.as_deref())
    {
        if approval_routes
            .iter()
            .all(|route: &QuoteApprovalRoute| route.approver_id != approver_id)
        {
            approval_routes.push(QuoteApprovalRoute {
                approver_id: approver_id.to_string(),
                reason: "quote_guardrail_escalation".to_string(),
            });
        }
    }
    let status = if findings.iter().any(|finding| {
        matches!(
            finding.severity,
            QuoteGuardrailSeverity::ApprovalRequired | QuoteGuardrailSeverity::Major
        )
    }) {
        QuoteGuardrailStatus::NeedsApproval
    } else {
        QuoteGuardrailStatus::WithinGuardrails
    };
    Ok(QuoteGuardrailEvaluation {
        status,
        config_hash,
        findings,
        approval_routes,
        config_snapshot_json,
    })
}

fn parse_guardrail_config(
    config_json: &serde_json::Value,
) -> Result<QuoteGuardrailConfig, StoreError> {
    let config: QuoteGuardrailConfig = if config_json.is_null() {
        QuoteGuardrailConfig::default()
    } else {
        serde_json::from_value(config_json.clone())
            .map_err(|err| StoreError::Domain(format!("quote_guardrail_config_invalid: {err}")))?
    };
    for price in &config.price_lists {
        if price.sku.trim().is_empty() {
            return Err(StoreError::Domain(
                "quote_guardrail_price_sku_required".to_string(),
            ));
        }
        if price.unit_cents <= 0 {
            return Err(StoreError::Domain(
                "quote_guardrail_price_unit_cents_invalid".to_string(),
            ));
        }
    }
    Ok(config)
}

fn matching_price<'a>(
    prices: &'a [QuoteGuardrailPrice],
    input: &QuoteWorkflowInput,
    line: &QuoteLineItem,
) -> Option<&'a QuoteGuardrailPrice> {
    let mut best = None;
    let mut best_score = 0;
    for price in prices {
        if price.sku != line.sku
            || !option_matches(price.product_line.as_deref(), line.product_line.as_deref())
            || !option_matches(
                price.customer_tier.as_deref(),
                input.customer_tier.as_deref(),
            )
        {
            continue;
        }
        let score = price_specificity(price);
        if best.is_none() || score > best_score {
            best = Some(price);
            best_score = score;
        }
    }
    best
}

fn option_matches(configured: Option<&str>, actual: Option<&str>) -> bool {
    configured.is_none_or(|configured| actual.is_some_and(|actual| configured == actual))
}

fn price_specificity(price: &QuoteGuardrailPrice) -> u8 {
    u8::from(price.product_line.is_some()) + u8::from(price.customer_tier.is_some())
}

fn price_delta_bps(list_unit_cents: i64, quoted_unit_cents: i64) -> Result<i32, StoreError> {
    let numerator = (list_unit_cents - quoted_unit_cents)
        .checked_mul(10_000)
        .ok_or_else(|| StoreError::Domain("quote_guardrail_delta_overflow".to_string()))?;
    i32::try_from(numerator / list_unit_cents)
        .map_err(|_| StoreError::Domain("quote_guardrail_delta_overflow".to_string()))
}

fn require_guardrail_approver(
    guardrails: &QuoteGuardrailEvaluation,
    actor_id: &str,
) -> Result<(), StoreError> {
    if guardrails.status != QuoteGuardrailStatus::NeedsApproval {
        return Ok(());
    }
    if guardrails.approval_routes.is_empty()
        || guardrails
            .approval_routes
            .iter()
            .any(|route| route.approver_id == actor_id)
    {
        return Ok(());
    }
    Err(StoreError::Domain(
        "quote_guardrail_approval_required".to_string(),
    ))
}

fn trace_value(value: TracedValue) -> WorkflowTraceValue {
    WorkflowTraceValue {
        label: value.label,
        value: value.value,
        unit: value.unit,
        formula: value.formula,
        source: value.source,
    }
}

fn run_id_for(idempotency_key: &str) -> String {
    let digest = Sha256::digest(idempotency_key.as_bytes());
    format!("qwr_{:x}", digest)[..20].to_string()
}

fn hash_json(value: &impl serde::Serialize) -> Result<String, StoreError> {
    serde_json::to_string(value)
        .map(|raw| hash_str(&raw))
        .map_err(|err| StoreError::Domain(format!("hash json: {err}")))
}

fn hash_str(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
