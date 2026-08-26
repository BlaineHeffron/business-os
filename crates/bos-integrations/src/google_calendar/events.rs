//! Calendar event-create outbox adapter: deserializes the outbox payload,
//! executes through a [`GoogleCalendarExecutionClient`], and maps the outcome
//! to success / retryable / terminal receipts for the delivery spine.

use super::{
    GoogleCalendarApprovalMetadata, GoogleCalendarEventWriteOperation,
    GoogleCalendarEventWriteOutboxPayload, GoogleCalendarEventWriteRequest,
    GoogleCalendarExecutionClient, GoogleCalendarWriteError,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoogleCalendarEventCreateOutboxPayload {
    pub calendar_id: String,
    pub idempotency_key: String,
    /// Operator user whose Google credential delivers this write (the
    /// approver). None = legacy jobs from before per-user credentials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_user_id: Option<String>,
    pub approval: GoogleCalendarApprovalMetadata,
    pub summary: String,
    pub description: Option<String>,
    pub start_at: String,
    pub end_at: String,
    pub timezone: Option<String>,
    pub attendees: Vec<String>,
    #[serde(default)]
    pub send_invitations: bool,
    pub expected_revision: Option<u64>,
}

impl From<GoogleCalendarEventCreateOutboxPayload> for GoogleCalendarEventWriteOutboxPayload {
    fn from(payload: GoogleCalendarEventCreateOutboxPayload) -> Self {
        Self {
            operation: GoogleCalendarEventWriteOperation::Create,
            calendar_id: payload.calendar_id,
            event_id: None,
            idempotency_key: payload.idempotency_key,
            approval: payload.approval,
            summary: payload.summary,
            description: payload.description,
            start_at: payload.start_at,
            end_at: payload.end_at,
            timezone: payload.timezone,
            attendees: payload.attendees,
            send_invitations: payload.send_invitations,
            expected_etag: None,
            expected_revision: payload.expected_revision,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoogleCalendarEventCreateExecutionContext {
    pub provider: String,
    pub capability: String,
    pub job_id: String,
    pub idempotency_key: String,
    pub approval_receipt_id: Option<String>,
    pub now_epoch_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoogleCalendarEventCreateAdapterReceipt {
    pub provider_object_id: Option<String>,
    pub provider_status: String,
    pub normalized_status: String,
    pub retryable: bool,
    pub provider_error_code: Option<String>,
    pub provider_request_id: Option<String>,
    pub raw_response_json: String,
    pub sanitized_metadata_json: String,
    pub retry_after_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoogleCalendarEventCreateAdapterResult {
    Success(GoogleCalendarEventCreateAdapterReceipt),
    RetryableFailure(GoogleCalendarEventCreateAdapterReceipt),
    TerminalFailure(GoogleCalendarEventCreateAdapterReceipt),
}

pub struct GoogleCalendarEventCreateProviderOutboxAdapter<C> {
    client: C,
}

impl<C> GoogleCalendarEventCreateProviderOutboxAdapter<C> {
    pub fn new(client: C) -> Self {
        Self { client }
    }
}

impl<C> GoogleCalendarEventCreateProviderOutboxAdapter<C>
where
    C: GoogleCalendarExecutionClient,
{
    pub fn execute(
        &self,
        context: &GoogleCalendarEventCreateExecutionContext,
        payload_json: &str,
    ) -> GoogleCalendarEventCreateAdapterResult {
        if context.provider != "google_calendar" || context.capability != "create_event" {
            return calendar_event_terminal_receipt(
                context,
                "google_calendar_create_event_outbox_operation_mismatch",
                "google calendar create event adapter received unsupported job",
                None,
            );
        }
        let payload =
            match serde_json::from_str::<GoogleCalendarEventCreateOutboxPayload>(payload_json) {
                Ok(payload) => GoogleCalendarEventWriteOutboxPayload::from(payload),
                Err(error) => {
                    return calendar_event_terminal_receipt(
                        context,
                        "google_calendar_create_event_outbox_payload_invalid",
                        &format!("google calendar create event payload invalid: {error}"),
                        None,
                    );
                }
            };
        let request = GoogleCalendarEventWriteRequest {
            operation: payload.operation,
            calendar_id: payload.calendar_id,
            event_id: payload.event_id,
            idempotency_key: payload.idempotency_key,
            approval: payload.approval,
            summary: payload.summary,
            description: payload.description,
            start_at: payload.start_at,
            end_at: payload.end_at,
            timezone: payload.timezone,
            attendees: payload.attendees,
            send_invitations: payload.send_invitations,
            expected_etag: payload.expected_etag,
            expected_revision: payload.expected_revision,
        };
        let attendee_count = request.attendees.len();
        let send_invitations = request.send_invitations;
        match self.client.write_event(&request) {
            Ok(response) => {
                let metadata = serde_json::json!({
                    "provider": context.provider,
                    "capability": context.capability,
                    "job_id": context.job_id,
                    "idempotency_key": context.idempotency_key,
                    "approval_receipt_id": context.approval_receipt_id,
                    "calendar_id": response.calendar_id,
                    "event_id": response.event_id,
                    "etag": response.etag,
                    "revision": response.revision,
                    "approval_id": response.approval.approval_id,
                    "dry_run": response.status.dry_run,
                    "degraded": response.status.degraded,
                    "execution_reason": response.status.reason,
                    "attendee_count": attendee_count,
                    "send_invitations": send_invitations,
                });
                GoogleCalendarEventCreateAdapterResult::Success(
                    GoogleCalendarEventCreateAdapterReceipt {
                        provider_object_id: Some(response.event_id),
                        provider_status: "event_created".to_string(),
                        normalized_status: "succeeded".to_string(),
                        retryable: false,
                        provider_error_code: None,
                        provider_request_id: None,
                        raw_response_json: metadata.to_string(),
                        sanitized_metadata_json: metadata.to_string(),
                        retry_after_ms: None,
                    },
                )
            }
            Err(GoogleCalendarWriteError::Retryable {
                code,
                message,
                retry_after_ms,
            }) => calendar_event_retryable_receipt(
                context,
                code.as_str(),
                message.as_str(),
                retry_after_ms,
            ),
            Err(GoogleCalendarWriteError::Permanent { code, message }) => {
                calendar_event_terminal_receipt(context, code.as_str(), message.as_str(), None)
            }
            Err(GoogleCalendarWriteError::Conflict {
                code,
                message,
                provider_state,
            }) => calendar_event_terminal_receipt(
                context,
                code.as_str(),
                message.as_str(),
                Some(serde_json::json!({
                    "provider_state": provider_state,
                })),
            ),
        }
    }
}

fn calendar_event_retryable_receipt(
    context: &GoogleCalendarEventCreateExecutionContext,
    code: &str,
    message: &str,
    retry_after_ms: Option<i64>,
) -> GoogleCalendarEventCreateAdapterResult {
    let retry_after_ms = retry_after_ms.or(Some(30_000));
    let metadata = serde_json::json!({
        "provider": context.provider,
        "capability": context.capability,
        "job_id": context.job_id,
        "idempotency_key": context.idempotency_key,
        "approval_receipt_id": context.approval_receipt_id,
        "status": "retryable_failure",
        "retry_after_ms": retry_after_ms,
    });
    GoogleCalendarEventCreateAdapterResult::RetryableFailure(
        GoogleCalendarEventCreateAdapterReceipt {
            provider_object_id: None,
            provider_status: "provider_retryable_failure".to_string(),
            normalized_status: "retry_scheduled".to_string(),
            retryable: true,
            provider_error_code: Some(code.to_string()),
            provider_request_id: None,
            raw_response_json: serde_json::json!({
                "code": code,
                "message": message,
            })
            .to_string(),
            sanitized_metadata_json: metadata.to_string(),
            retry_after_ms,
        },
    )
}

fn calendar_event_terminal_receipt(
    context: &GoogleCalendarEventCreateExecutionContext,
    code: &str,
    message: &str,
    extra_metadata: Option<serde_json::Value>,
) -> GoogleCalendarEventCreateAdapterResult {
    let mut metadata = serde_json::json!({
        "provider": context.provider,
        "capability": context.capability,
        "job_id": context.job_id,
        "idempotency_key": context.idempotency_key,
        "approval_receipt_id": context.approval_receipt_id,
        "status": "terminal_failure",
    });
    if let (Some(object), Some(extra)) = (metadata.as_object_mut(), extra_metadata) {
        object.insert("provider_context".to_string(), extra);
    }
    GoogleCalendarEventCreateAdapterResult::TerminalFailure(
        GoogleCalendarEventCreateAdapterReceipt {
            provider_object_id: None,
            provider_status: "failed_provider_validation".to_string(),
            normalized_status: "failed_terminal".to_string(),
            retryable: false,
            provider_error_code: Some(code.to_string()),
            provider_request_id: None,
            raw_response_json: serde_json::json!({
                "code": code,
                "message": message,
            })
            .to_string(),
            sanitized_metadata_json: metadata.to_string(),
            retry_after_ms: None,
        },
    )
}

#[cfg(test)]
mod live_integration_tests {
    use super::*;
    use crate::google_calendar::live::{FakeCalendarHttp, LiveGoogleCalendarClient};
    use std::sync::Arc;

    fn context() -> GoogleCalendarEventCreateExecutionContext {
        GoogleCalendarEventCreateExecutionContext {
            provider: "google_calendar".into(),
            capability: "create_event".into(),
            job_id: "job-1".into(),
            idempotency_key: "idem-1".into(),
            approval_receipt_id: Some("appr-1".into()),
            now_epoch_ms: 0,
        }
    }

    fn payload() -> String {
        serde_json::json!({
            "calendar_id": "primary",
            "idempotency_key": "idem-1",
            "approval": {"approval_id":"appr-1","approved_by":"jordan","approved_at":"2026-05-31T10:00:00Z"},
            "summary": "Follow up",
            "description": null,
            "start_at": "2026-06-01T15:00:00Z",
            "end_at": "2026-06-01T15:30:00Z",
            "timezone": "America/New_York",
            "attendees": ["jordan@example.test"],
            "expected_revision": null
        })
        .to_string()
    }

    #[test]
    fn live_create_event_yields_success_receipt_with_event_id() {
        let http = Arc::new(FakeCalendarHttp::new(vec![(
            200,
            serde_json::json!({"id":"evt-42","etag":"e42"}),
        )]));
        let adapter = GoogleCalendarEventCreateProviderOutboxAdapter::new(
            LiveGoogleCalendarClient::for_test(http, "tok".into()),
        );
        match adapter.execute(&context(), &payload()) {
            GoogleCalendarEventCreateAdapterResult::Success(receipt) => {
                assert_eq!(receipt.provider_object_id.as_deref(), Some("evt-42"));
                assert_eq!(receipt.normalized_status, "succeeded");
                let metadata: serde_json::Value =
                    serde_json::from_str(&receipt.sanitized_metadata_json).expect("metadata");
                assert_eq!(metadata["attendee_count"], 1);
                assert_eq!(metadata["send_invitations"], false);
            }
            other => panic!("expected success, got {other:?}"),
        }
    }

    #[test]
    fn malformed_payload_yields_terminal_failure() {
        let http = Arc::new(FakeCalendarHttp::new(vec![]));
        let adapter = GoogleCalendarEventCreateProviderOutboxAdapter::new(
            LiveGoogleCalendarClient::for_test(http, "tok".into()),
        );
        match adapter.execute(&context(), "{not json") {
            GoogleCalendarEventCreateAdapterResult::TerminalFailure(_) => {}
            other => panic!("expected terminal failure, got {other:?}"),
        }
    }

    struct RetryAfterClient;

    impl super::super::GoogleCalendarExecutionClient for RetryAfterClient {
        fn write_event(
            &self,
            _request: &super::super::GoogleCalendarEventWriteRequest,
        ) -> Result<
            super::super::GoogleCalendarEventWriteResponse,
            super::super::GoogleCalendarWriteError,
        > {
            Err(super::super::GoogleCalendarWriteError::Retryable {
                code: "google_calendar_event_create_unavailable".to_string(),
                message: "status 429 reason=rateLimitExceeded".to_string(),
                retry_after_ms: Some(42_000),
            })
        }
    }

    #[test]
    fn retryable_receipt_preserves_provider_retry_after() {
        let adapter = GoogleCalendarEventCreateProviderOutboxAdapter::new(RetryAfterClient);
        match adapter.execute(&context(), &payload()) {
            GoogleCalendarEventCreateAdapterResult::RetryableFailure(receipt) => {
                assert_eq!(receipt.retry_after_ms, Some(42_000));
                let metadata: serde_json::Value =
                    serde_json::from_str(&receipt.sanitized_metadata_json).expect("metadata");
                assert_eq!(metadata["retry_after_ms"], 42_000);
            }
            other => panic!("expected retryable failure, got {other:?}"),
        }
    }
}
