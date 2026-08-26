//! Google Calendar event-write integration (create only), ported from
//! agent-monitor-rust (`google_calendar_events.rs` / `google_calendar_live.rs`
//! plus the minimal write-model types from its `google.rs`).
//!
//! Config-driven: credentials/gating arrive as [`GoogleCalendarWriteConfig`]
//! built by the caller (bos-app env_registry / google_connector slice) — this
//! module never reads env vars. Writes are approval-gated upstream; the
//! `write_enabled` flag here is the execution gate (disabled => dry-run client).

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const MAX_CALENDAR_ATTENDEES: usize = 25;

pub fn normalize_calendar_attendees(attendees: &[String]) -> Result<Vec<String>, &'static str> {
    let mut seen = HashSet::new();
    let mut normalized = Vec::with_capacity(attendees.len());
    for raw in attendees {
        let address = raw.trim();
        if !valid_calendar_attendee(address) {
            return Err("google_calendar_attendee_invalid");
        }
        let key = address.to_ascii_lowercase();
        if seen.insert(key) {
            normalized.push(address.to_string());
        }
    }
    if normalized.len() > MAX_CALENDAR_ATTENDEES {
        return Err("google_calendar_attendee_limit_exceeded");
    }
    Ok(normalized)
}

fn valid_calendar_attendee(address: &str) -> bool {
    if address.is_empty()
        || address.len() > 254
        || address.contains(char::is_whitespace)
        || address.contains(['<', '>', ',', ';'])
    {
        return false;
    }
    let mut parts = address.split('@');
    let Some(local) = parts.next() else {
        return false;
    };
    let Some(domain) = parts.next() else {
        return false;
    };
    if parts.next().is_some()
        || local.is_empty()
        || local.len() > 64
        || domain.is_empty()
        || !domain.contains('.')
        || domain.starts_with('.')
        || domain.ends_with('.')
    {
        return false;
    }
    domain.split('.').all(|label| {
        !label.is_empty()
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    })
}

use crate::google_oauth::GoogleOAuthConfig;

pub mod events;
pub mod live;

pub use live::{google_calendar_execution_client, LiveGoogleCalendarClient};

/// Caller-supplied configuration for the calendar write path. Resolution of
/// OAuth material (connector account, state file, overlay) is the caller's job.
#[derive(Debug, Clone)]
pub struct GoogleCalendarWriteConfig {
    pub oauth: GoogleOAuthConfig,
    /// Target calendar; defaults to `"primary"` via [`GoogleCalendarWriteConfig::new`].
    pub calendar_id: String,
    /// Execution gate. `false` => [`google_calendar_execution_client`] returns
    /// the dry-run client (predecessor gated this on GOOGLE_CALENDAR_WRITE_ENABLED).
    pub write_enabled: bool,
}

impl GoogleCalendarWriteConfig {
    /// Writes disabled, `calendar_id = "primary"`.
    pub fn new(oauth: GoogleOAuthConfig) -> Self {
        Self {
            oauth,
            calendar_id: "primary".to_string(),
            write_enabled: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoogleCalendarApprovalMetadata {
    pub approval_id: String,
    pub approved_by: String,
    pub approved_at: String,
}

impl GoogleCalendarApprovalMetadata {
    pub(crate) fn is_complete(&self) -> bool {
        !self.approval_id.trim().is_empty()
            && !self.approved_by.trim().is_empty()
            && !self.approved_at.trim().is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoogleCalendarEventWriteOperation {
    Create,
    Update,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoogleCalendarEventWriteOutboxPayload {
    pub operation: GoogleCalendarEventWriteOperation,
    pub calendar_id: String,
    pub event_id: Option<String>,
    pub idempotency_key: String,
    pub approval: GoogleCalendarApprovalMetadata,
    pub summary: String,
    pub description: Option<String>,
    pub start_at: String,
    pub end_at: String,
    pub timezone: Option<String>,
    pub attendees: Vec<String>,
    #[serde(default)]
    pub send_invitations: bool,
    pub expected_etag: Option<String>,
    pub expected_revision: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoogleCalendarEventWriteRequest {
    pub operation: GoogleCalendarEventWriteOperation,
    pub calendar_id: String,
    pub event_id: Option<String>,
    pub idempotency_key: String,
    pub approval: GoogleCalendarApprovalMetadata,
    pub summary: String,
    pub description: Option<String>,
    pub start_at: String,
    pub end_at: String,
    pub timezone: Option<String>,
    pub attendees: Vec<String>,
    pub send_invitations: bool,
    pub expected_etag: Option<String>,
    pub expected_revision: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoogleCalendarExecutionStatus {
    pub executed: bool,
    pub dry_run: bool,
    pub degraded: bool,
    pub reason: Option<String>,
}

impl GoogleCalendarExecutionStatus {
    pub fn dry_run(reason: impl Into<String>) -> Self {
        Self {
            executed: false,
            dry_run: true,
            degraded: false,
            reason: Some(reason.into()),
        }
    }

    pub fn executed(reason: impl Into<String>) -> Self {
        Self {
            executed: true,
            dry_run: false,
            degraded: false,
            reason: Some(reason.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoogleCalendarEventWriteResponse {
    pub status: GoogleCalendarExecutionStatus,
    pub calendar_id: String,
    pub event_id: String,
    pub etag: String,
    pub revision: Option<u64>,
    pub approval: GoogleCalendarApprovalMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoogleCalendarProviderState {
    pub provider_id: String,
    pub calendar_id: String,
    pub event_id: Option<String>,
    pub current_etag: Option<String>,
    pub degraded: bool,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoogleCalendarWriteError {
    Retryable {
        code: String,
        message: String,
        retry_after_ms: Option<i64>,
    },
    Permanent {
        code: String,
        message: String,
    },
    Conflict {
        code: String,
        message: String,
        provider_state: Box<GoogleCalendarProviderState>,
    },
}

pub trait GoogleCalendarExecutionClient: Send + Sync {
    fn write_event(
        &self,
        request: &GoogleCalendarEventWriteRequest,
    ) -> Result<GoogleCalendarEventWriteResponse, GoogleCalendarWriteError>;
}

impl GoogleCalendarExecutionClient for Box<dyn GoogleCalendarExecutionClient> {
    fn write_event(
        &self,
        request: &GoogleCalendarEventWriteRequest,
    ) -> Result<GoogleCalendarEventWriteResponse, GoogleCalendarWriteError> {
        (**self).write_event(request)
    }
}

/// Validates like the live client but never touches the network; returns a
/// deterministic dry-run response. The factory falls back to this whenever
/// writes are disabled or credentials/scopes are unusable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DryRunGoogleCalendarClient;

impl GoogleCalendarExecutionClient for DryRunGoogleCalendarClient {
    fn write_event(
        &self,
        request: &GoogleCalendarEventWriteRequest,
    ) -> Result<GoogleCalendarEventWriteResponse, GoogleCalendarWriteError> {
        validate_calendar_write_request(request)?;
        if !request.approval.is_complete() {
            return Err(GoogleCalendarWriteError::Permanent {
                code: "google_calendar_approval_missing".to_string(),
                message: "google calendar write approval metadata is incomplete".to_string(),
            });
        }
        if request.idempotency_key.trim().is_empty() {
            return Err(GoogleCalendarWriteError::Permanent {
                code: "google_calendar_idempotency_key_missing".to_string(),
                message: "google calendar write idempotency key is required".to_string(),
            });
        }
        if matches!(request.operation, GoogleCalendarEventWriteOperation::Update)
            && request
                .expected_etag
                .as_deref()
                .map(str::trim)
                .unwrap_or("")
                .is_empty()
        {
            return Err(GoogleCalendarWriteError::Conflict {
                code: "google_calendar_etag_required".to_string(),
                message: "google calendar update requires an expected etag".to_string(),
                provider_state: Box::new(GoogleCalendarProviderState {
                    provider_id: "google_calendar".to_string(),
                    calendar_id: request.calendar_id.clone(),
                    event_id: request.event_id.clone(),
                    current_etag: None,
                    degraded: true,
                    reason: "missing_expected_etag".to_string(),
                }),
            });
        }
        let event_id = request.event_id.clone().unwrap_or_else(|| {
            format!(
                "dry-run-calendar-event-{}",
                stable_token(&request.idempotency_key)
            )
        });
        let etag = match request.operation {
            GoogleCalendarEventWriteOperation::Create => {
                format!("dry-run-etag-{}", stable_token(&request.idempotency_key))
            }
            GoogleCalendarEventWriteOperation::Update => format!(
                "dry-run-etag-next-{}",
                stable_token(request.expected_etag.as_deref().unwrap_or("update"))
            ),
        };
        Ok(GoogleCalendarEventWriteResponse {
            status: GoogleCalendarExecutionStatus::dry_run("google_calendar_dry_run"),
            calendar_id: request.calendar_id.clone(),
            event_id,
            etag,
            revision: request.expected_revision.map(|revision| revision + 1),
            approval: request.approval.clone(),
        })
    }
}

pub fn validate_calendar_write_request(
    request: &GoogleCalendarEventWriteRequest,
) -> Result<(), GoogleCalendarWriteError> {
    let normalized = normalize_calendar_attendees(&request.attendees).map_err(|code| {
        GoogleCalendarWriteError::Permanent {
            code: code.to_string(),
            message: "google calendar attendee list is invalid".to_string(),
        }
    })?;
    if normalized != request.attendees {
        return Err(GoogleCalendarWriteError::Permanent {
            code: "google_calendar_attendee_not_normalized".to_string(),
            message: "google calendar attendee list is not normalized".to_string(),
        });
    }
    if request.send_invitations && request.attendees.is_empty() {
        return Err(GoogleCalendarWriteError::Permanent {
            code: "google_calendar_invitation_attendees_required".to_string(),
            message: "calendar invitations require at least one attendee".to_string(),
        });
    }
    Ok(())
}

fn stable_token(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod attendee_tests {
    use super::*;

    fn request(attendees: Vec<String>, send_invitations: bool) -> GoogleCalendarEventWriteRequest {
        GoogleCalendarEventWriteRequest {
            operation: GoogleCalendarEventWriteOperation::Create,
            calendar_id: "primary".to_string(),
            event_id: None,
            idempotency_key: "attendee-test".to_string(),
            approval: GoogleCalendarApprovalMetadata {
                approval_id: "approval".to_string(),
                approved_by: "operator".to_string(),
                approved_at: "2026-07-30T12:00:00Z".to_string(),
            },
            summary: "Review".to_string(),
            description: None,
            start_at: "2026-07-30T12:00:00Z".to_string(),
            end_at: "2026-07-30T13:00:00Z".to_string(),
            timezone: None,
            attendees,
            send_invitations,
            expected_etag: None,
            expected_revision: None,
        }
    }

    #[test]
    fn attendee_normalization_trims_deduplicates_and_preserves_first_case() {
        assert_eq!(
            normalize_calendar_attendees(&[
                " Coach@example.test ".to_string(),
                "coach@example.test".to_string(),
                "Guest@example.test".to_string(),
            ])
            .expect("valid"),
            vec!["Coach@example.test", "Guest@example.test"]
        );
        let repeated = vec!["guest@example.test".to_string(); MAX_CALENDAR_ATTENDEES + 1];
        assert_eq!(
            normalize_calendar_attendees(&repeated).expect("duplicates count once"),
            vec!["guest@example.test"]
        );
    }

    #[test]
    fn attendee_normalization_rejects_malformed_and_excessive_lists() {
        assert_eq!(
            normalize_calendar_attendees(&["not-an-email".to_string()]),
            Err("google_calendar_attendee_invalid")
        );
        let excessive = (0..=MAX_CALENDAR_ATTENDEES)
            .map(|index| format!("guest{index}@example.test"))
            .collect::<Vec<_>>();
        assert_eq!(
            normalize_calendar_attendees(&excessive),
            Err("google_calendar_attendee_limit_exceeded")
        );
    }

    #[test]
    fn provider_boundary_rejects_non_normalized_and_empty_invitation_payloads() {
        let non_normalized = request(vec![" guest@example.test ".to_string()], false);
        assert!(matches!(
            validate_calendar_write_request(&non_normalized),
            Err(GoogleCalendarWriteError::Permanent { code, .. })
                if code == "google_calendar_attendee_not_normalized"
        ));
        let empty_invitation = request(Vec::new(), true);
        assert!(matches!(
            validate_calendar_write_request(&empty_invitation),
            Err(GoogleCalendarWriteError::Permanent { code, .. })
                if code == "google_calendar_invitation_attendees_required"
        ));
    }
}
