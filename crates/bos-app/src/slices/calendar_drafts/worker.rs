//! Google Calendar delivery executor for the spine outbox pump
//! (crate::outbox dispatches `provider = "google_calendar"` jobs here).
//! With the gate closed (BOS_GOOGLE_CALENDAR_WRITE_ENABLED unset) every
//! delivery runs the dry-run client — validated, receipted, no provider
//! effect. The provider call itself never holds the persistence lock;
//! `execute_job` is the testable core.

use bos_integrations::google_calendar::events::{
    GoogleCalendarEventCreateAdapterResult, GoogleCalendarEventCreateExecutionContext,
    GoogleCalendarEventCreateProviderOutboxAdapter,
};
use bos_integrations::google_calendar::{
    google_calendar_execution_client, GoogleCalendarWriteConfig,
};
use bos_integrations::GoogleOAuthConfig;

use crate::env_registry;
use crate::http::AppState;
use crate::outbox::{provider_error_detail, retry_backoff_ms, AttemptOutcome, ClaimedJob};

use super::service::{CAPABILITY_CREATE_EVENT, PROVIDER_GOOGLE_CALENDAR};

/// Resolve credentials + gate, then execute. Locks persistence only for the
/// credential read. The credential is the payload's bound user's (the
/// approver); legacy jobs without a binding resolve through the fallback
/// chain (env, then the only stored credential).
pub fn deliver(state: &AppState, job: &ClaimedJob, now_ms: u64) -> AttemptOutcome {
    let credential_user = credential_user_id(&job.payload_json);
    let (oauth, write_enabled) = {
        let persistence = state.persistence.lock();
        let oauth = match credential_user.as_deref() {
            Some(user_id) => crate::slices::google_connector::service::resolve_bound_google_oauth(
                persistence.connection_ref(),
                &state.client_id,
                user_id,
            ),
            None => crate::slices::google_connector::service::resolve_google_oauth(
                persistence.connection_ref(),
                &state.client_id,
                None,
            ),
        }
        .unwrap_or_default();
        let write_enabled = crate::slices::admin_settings::service::flag(
            persistence.connection_ref(),
            &state.client_id,
            &env_registry::BOS_GOOGLE_CALENDAR_WRITE_ENABLED,
        )
        .unwrap_or_else(|err| {
            tracing::warn!(error = %err, "calendar write gate read failed");
            false
        });
        (oauth, write_enabled)
    };
    let calendar_id = env_registry::string(&env_registry::BOS_GOOGLE_CALENDAR_ID)
        .unwrap_or_else(|| "primary".to_string());
    execute_job(job, oauth.as_ref(), write_enabled, &calendar_id, now_ms)
}

/// Which user's credential the job is bound to (None on legacy payloads).
fn credential_user_id(payload_json: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(payload_json)
        .ok()?
        .get("credential_user_id")?
        .as_str()
        .map(str::to_string)
}

/// Execute one claimed job against the (gated) calendar client and map the
/// adapter result onto an outbox attempt outcome.
pub fn execute_job(
    job: &ClaimedJob,
    oauth: Option<&GoogleOAuthConfig>,
    write_enabled: bool,
    calendar_id: &str,
    now_ms: u64,
) -> AttemptOutcome {
    if job.provider != PROVIDER_GOOGLE_CALENDAR || job.capability != CAPABILITY_CREATE_EVENT {
        return AttemptOutcome::Terminal {
            error: format!("outbox_unsupported_job:{}:{}", job.provider, job.capability),
            result_json: None,
        };
    }
    let Some(oauth) = oauth else {
        return AttemptOutcome::Retry {
            error: "google_credential_unavailable".to_string(),
            retry_at_ms: now_ms + retry_backoff_ms(job.attempts),
        };
    };
    let config = GoogleCalendarWriteConfig {
        oauth: oauth.clone(),
        calendar_id: calendar_id.to_string(),
        write_enabled,
    };
    let client = google_calendar_execution_client(&config);
    let adapter = GoogleCalendarEventCreateProviderOutboxAdapter::new(client);
    let context = GoogleCalendarEventCreateExecutionContext {
        provider: job.provider.clone(),
        capability: job.capability.clone(),
        job_id: job.job_id.clone(),
        idempotency_key: job.idempotency_key.clone(),
        approval_receipt_id: None,
        now_epoch_ms: now_ms as i64,
    };
    match adapter.execute(&context, &job.payload_json) {
        GoogleCalendarEventCreateAdapterResult::Success(receipt) => {
            let metadata =
                serde_json::from_str::<serde_json::Value>(&receipt.sanitized_metadata_json).ok();
            let dry_run = metadata
                .as_ref()
                .and_then(|meta| meta.get("dry_run").and_then(serde_json::Value::as_bool));
            let attendee_count = metadata.as_ref().and_then(|meta| {
                meta.get("attendee_count")
                    .and_then(serde_json::Value::as_u64)
            });
            let send_invitations = metadata.as_ref().and_then(|meta| {
                meta.get("send_invitations")
                    .and_then(serde_json::Value::as_bool)
            });
            AttemptOutcome::Delivered {
                result_json: serde_json::json!({
                    "dry_run": dry_run,
                    "attendee_count": attendee_count,
                    "send_invitations": send_invitations,
                    "provider_object_id": receipt.provider_object_id,
                    "provider_status": receipt.provider_status,
                })
                .to_string(),
            }
        }
        GoogleCalendarEventCreateAdapterResult::RetryableFailure(receipt) => {
            AttemptOutcome::Retry {
                error: receipt
                    .provider_error_code
                    .unwrap_or_else(|| "provider_retryable_failure".to_string()),
                retry_at_ms: now_ms
                    + receipt
                        .retry_after_ms
                        .map(|ms| ms as u64)
                        .unwrap_or_else(|| retry_backoff_ms(job.attempts)),
            }
        }
        GoogleCalendarEventCreateAdapterResult::TerminalFailure(receipt) => {
            let code = receipt
                .provider_error_code
                .unwrap_or_else(|| "provider_terminal_failure".to_string());
            let message = serde_json::from_str::<serde_json::Value>(&receipt.raw_response_json)
                .ok()
                .and_then(|value| {
                    value
                        .get("message")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                });
            AttemptOutcome::Terminal {
                error: message
                    .as_deref()
                    .map(|message| provider_error_detail(&code, message))
                    .unwrap_or(code),
                result_json: Some(receipt.sanitized_metadata_json),
            }
        }
    }
}
