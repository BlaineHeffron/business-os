//! Call input domain logic. This slice does not capture or transcribe calls;
//! it only accepts selected artifacts after explicit consent/fit configuration.

use bos_contracts::call_inputs::{
    CallInputItem, CallInputSourceConfig, CallInputStageRequest, CallInputsDriveSettingsResponse,
    CallInputsDriveSettingsUpdateRequest, CallInputsRouting, CallInputsStatusResponse,
};
use bos_contracts::email_triage::InboundMessageRecord;
use bos_contracts::source::{EvidenceUsagePolicy, SourceKind};

use crate::overlay::CallInputsOverlay;
use crate::store_core::StoreError;

pub const CATEGORY_ID: &str = "call_input";

pub fn status(overlay: &CallInputsOverlay) -> CallInputsStatusResponse {
    let enabled_sources = overlay
        .sources
        .iter()
        .filter(|source| source_ready(source))
        .count();
    let pending_sources = overlay
        .sources
        .iter()
        .filter(|source| !source_ready(source))
        .count();
    CallInputsStatusResponse {
        configured: enabled_sources > 0,
        enabled_sources,
        pending_sources,
        sources: overlay.sources.clone(),
        routing: overlay.routing.clone(),
    }
}

pub fn drive_settings_response(
    conn: &rusqlite::Connection,
    client_id: &str,
    user_id: &str,
) -> Result<CallInputsDriveSettingsResponse, StoreError> {
    let stored = super::store::get_drive_settings(conn, client_id)?;
    let credential_user_id = stored
        .as_ref()
        .and_then(|settings| settings.credential_user_id.clone());
    let oauth = if stored
        .as_ref()
        .and_then(|settings| settings.drive_folder_id.as_ref())
        .is_some()
    {
        crate::slices::google_connector::service::resolve_google_oauth_for_owner(
            conn,
            client_id,
            credential_user_id.as_deref(),
        )?
    } else {
        crate::slices::google_connector::service::resolve_google_oauth(
            conn,
            client_id,
            Some(user_id),
        )?
    };
    let drive_scope_granted = oauth.as_ref().map(|config| {
        config.scopes.is_empty()
            || bos_integrations::google_oauth::has_scope(
                config,
                crate::slices::google_connector::service::DRIVE_READONLY_SCOPE,
            )
    });
    Ok(CallInputsDriveSettingsResponse {
        revision: stored.as_ref().and_then(|settings| settings.revision),
        credential_user_id,
        drive_folder_id: stored
            .as_ref()
            .and_then(|settings| settings.drive_folder_id.clone()),
        drive_folder_name: stored
            .as_ref()
            .and_then(|settings| settings.drive_folder_name.clone()),
        ingestion_enabled: stored
            .as_ref()
            .map(|settings| settings.ingestion_enabled)
            .unwrap_or(false),
        interval_secs: stored.as_ref().and_then(|settings| settings.interval_secs),
        credential_connected: oauth.is_some(),
        drive_scope_granted,
    })
}

pub fn replace_drive_settings(
    conn: &mut rusqlite::Connection,
    client_id: &str,
    actor_id: &str,
    credential_user_id: Option<&str>,
    request: &CallInputsDriveSettingsUpdateRequest,
    now_ms: u64,
) -> Result<crate::store_core::MutationOutcome, StoreError> {
    super::store::replace_drive_settings(
        conn,
        client_id,
        actor_id,
        credential_user_id,
        request,
        now_ms,
    )
}

pub fn resolve_enabled_source<'a>(
    overlay: &'a CallInputsOverlay,
    source_id: &str,
) -> Result<&'a CallInputSourceConfig, StoreError> {
    let source = overlay
        .sources
        .iter()
        .find(|candidate| candidate.source_id == source_id)
        .ok_or_else(|| StoreError::Domain("call_source_not_configured".to_string()))?;
    if !source.enabled {
        return Err(StoreError::Domain("call_source_not_enabled".to_string()));
    }
    if source
        .consent_basis
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .is_empty()
    {
        return Err(StoreError::Domain(
            "call_source_consent_missing".to_string(),
        ));
    }
    Ok(source)
}

pub fn input_from_stage(
    request: &CallInputStageRequest,
    source: &CallInputSourceConfig,
    now_ms: u64,
) -> Result<CallInputItem, StoreError> {
    let title = request.title.trim();
    let summary = request.summary.trim();
    if title.is_empty() {
        return Err(StoreError::Domain("call_input_title_empty".to_string()));
    }
    if summary.is_empty() {
        return Err(StoreError::Domain("call_input_summary_empty".to_string()));
    }
    let transcript = request.transcript_text.trim();
    if transcript.is_empty() {
        return Err(StoreError::Domain(
            "call_input_transcript_required".to_string(),
        ));
    }
    let source_ref = request.source_ref.trim();
    if source_ref.is_empty() {
        return Err(StoreError::Domain(
            "call_input_source_ref_required".to_string(),
        ));
    }
    let recording_ref = normalized_recording_ref(request, source)?;
    recording_ref
        .validate_for_ai_consumption()
        .map_err(|code| StoreError::Domain(code.to_string()))?;
    let clean_key = request.idempotency_key.trim();
    if clean_key.is_empty() {
        return Err(StoreError::Domain("idempotency_key_required".to_string()));
    }
    Ok(CallInputItem {
        call_input_id: format!("call_{}", clean_key),
        source_id: source.source_id.clone(),
        source_ref: source_ref.chars().take(500).collect(),
        input_kind: request.input_kind,
        status: bos_contracts::call_inputs::CallInputStatus::Staged,
        title: title.chars().take(140).collect(),
        summary: summary.chars().take(2_000).collect(),
        caller_name: trimmed_optional_with_limit(request.caller_name.as_deref(), 240),
        caller_phone: trimmed_optional_with_limit(request.caller_phone.as_deref(), 120),
        caller_email: trimmed_optional_with_limit(request.caller_email.as_deref(), 240),
        transcript_text: transcript.chars().take(20_000).collect(),
        recording_ref,
        transcription_meta: request.transcription_meta.clone(),
        occurred_at_ms: request.occurred_at_ms,
        captured_at_ms: request.captured_at_ms.or(Some(now_ms)),
        work_item_id: None,
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
    })
}

pub fn routing_packet_kinds(routing: &CallInputsRouting) -> Vec<String> {
    if routing.packet_kinds.is_empty() {
        return vec![
            "crm_activity".to_string(),
            "follow_up_task".to_string(),
            "calendar_event_draft".to_string(),
            "email_draft_reply".to_string(),
        ];
    }
    routing.packet_kinds.clone()
}

pub fn resolve_packet_kinds(
    requested: &[String],
    routing: &CallInputsRouting,
    slice_enabled: impl Fn(&str) -> bool,
) -> Result<Vec<String>, StoreError> {
    let source = if requested.is_empty() {
        routing_packet_kinds(routing)
    } else {
        requested.to_vec()
    };
    let mut resolved = Vec::new();
    for raw in source {
        let kind = raw.trim();
        if kind.is_empty() || resolved.iter().any(|existing| existing == kind) {
            continue;
        }
        let Some(owner) = crate::slices::work_queue::packet_kind_slice(kind) else {
            return Err(StoreError::Domain(format!(
                "work_queue_packet_kind_unknown:{kind}"
            )));
        };
        if !slice_enabled(owner) {
            return Err(StoreError::Domain(format!(
                "work_queue_packet_kind_disabled:{kind}"
            )));
        }
        resolved.push(kind.to_string());
    }
    if resolved.is_empty() {
        return Err(StoreError::Domain(
            "call_input_packet_kinds_required".to_string(),
        ));
    }
    Ok(resolved)
}

pub fn source_view(
    input: &CallInputItem,
    source: Option<&CallInputSourceConfig>,
) -> InboundMessageRecord {
    let mut body = input.summary.clone();
    if let Some(name) = input.caller_name.as_deref() {
        body.push_str("\n\nCaller: ");
        body.push_str(name);
    }
    if let Some(phone) = input.caller_phone.as_deref() {
        body.push_str("\nPhone: ");
        body.push_str(phone);
    }
    if let Some(email) = input.caller_email.as_deref() {
        body.push_str("\nEmail: ");
        body.push_str(email);
    }
    body.push_str("\n\nTranscript:\n");
    body.push_str(&input.transcript_text);
    body.push_str("\n\nProvenance:\nSource: ");
    body.push_str(&input.recording_ref.source.display_name);
    if let Some(url) = input.recording_ref.item_url.as_deref() {
        body.push_str("\nReference: ");
        body.push_str(url);
    }
    if let Some(consent) = source.and_then(|source| source.consent_basis.as_deref()) {
        body.push_str("\nConsent basis: ");
        body.push_str(consent.trim());
    }
    let source_label = source
        .map(|source| source.display_name.clone())
        .unwrap_or_else(|| input.recording_ref.source.display_name.clone());
    InboundMessageRecord {
        source_key: input.call_input_id.clone(),
        message_id: input.call_input_id.clone(),
        thread_id: None,
        internal_date_ms: input.occurred_at_ms.map(|ms| ms as i64),
        from_addr: input.caller_email.clone().or(Some(source_label)),
        to_addr: None,
        subject: Some(input.title.clone()),
        body_excerpt: body.clone(),
        body_full: body,
        headers: Vec::new(),
        labels: vec![CATEGORY_ID.to_string()],
        resolved_category: CATEGORY_ID.to_string(),
        matched_rule_id: None,
        ingested_at_ms: input.created_at_ms,
        ai_triage_status: None,
        ai_triage_rationale: None,
        attachments: Vec::new(),
        source_user_id: None,
    }
}

fn source_ready(source: &CallInputSourceConfig) -> bool {
    source.enabled
        && source
            .consent_basis
            .as_deref()
            .map(str::trim)
            .is_some_and(|basis| !basis.is_empty())
}

fn normalized_recording_ref(
    request: &CallInputStageRequest,
    source: &CallInputSourceConfig,
) -> Result<bos_contracts::source::EvidenceRecord, StoreError> {
    let mut record = request.recording_ref.clone();
    record.source.source_id = source.source_id.clone();
    record.source.kind = SourceKind::Call;
    record.source.display_name = source.display_name.clone();
    if record.evidence_quote.trim().is_empty() {
        record.evidence_quote = request.transcript_text.chars().take(1_000).collect();
    }
    record.policy = EvidenceUsagePolicy::approved_source_import();
    if record.captured_at_ms.is_none() {
        record.captured_at_ms = request.captured_at_ms;
    }
    Ok(record)
}

fn trimmed_optional_with_limit(raw: Option<&str>, limit: usize) -> Option<String> {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(limit).collect())
}
