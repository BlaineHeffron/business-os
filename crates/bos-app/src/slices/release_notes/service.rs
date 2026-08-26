//! Release note domain helpers.

use bos_contracts::release_notes::{ReleaseNote, ReleaseNoteCreateRequest};

use crate::env_registry;

pub const DEFAULT_TITLE: &str = "What's new";
const MAX_TITLE_CHARS: usize = 120;
const MAX_SUMMARY_CHARS: usize = 600;
const MAX_BODY_CHARS: usize = 4_000;

pub fn webhook_secret_from_env() -> Option<String> {
    env_registry::string(&env_registry::BOS_RELEASE_NOTES_WEBHOOK_SECRET)
}

pub fn verify_webhook_bearer(
    authorization_header: Option<&str>,
    secret: &str,
) -> Result<(), &'static str> {
    let Some(token) = authorization_header
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Err("webhook_token_invalid");
    };
    if !constant_time_eq(token.as_bytes(), secret.as_bytes()) {
        return Err("webhook_token_invalid");
    }
    Ok(())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (left, right) in left.iter().zip(right) {
        diff |= left ^ right;
    }
    diff == 0
}

pub fn note_from_request(
    request: &ReleaseNoteCreateRequest,
    now_ms: u64,
) -> Result<ReleaseNote, &'static str> {
    if request.idempotency_key.trim().is_empty() {
        return Err("idempotency_key_required");
    }
    let release_note_id = request
        .release_note_id
        .as_deref()
        .unwrap_or(&request.idempotency_key)
        .trim();
    if release_note_id.is_empty() {
        return Err("release_note_id_required");
    }
    let summary =
        trim_required(&request.summary, MAX_SUMMARY_CHARS).ok_or("release_note_summary_empty")?;
    let title =
        trim_optional(&request.title, MAX_TITLE_CHARS).unwrap_or_else(|| DEFAULT_TITLE.to_string());
    Ok(ReleaseNote {
        release_note_id: release_note_id.to_string(),
        title,
        summary,
        body: request
            .body
            .as_deref()
            .and_then(|body| trim_optional(body, MAX_BODY_CHARS)),
        build_sha: request
            .build_sha
            .as_deref()
            .and_then(|build_sha| trim_optional(build_sha, 80)),
        created_at_ms: now_ms,
    })
}

fn trim_required(value: &str, max_chars: usize) -> Option<String> {
    trim_optional(value, max_chars)
}

fn trim_optional(value: &str, max_chars: usize) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.chars().take(max_chars).collect())
}
