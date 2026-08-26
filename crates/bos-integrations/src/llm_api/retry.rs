//! Bounded retry policy evaluation for the direct-API backend.
//! Ported verbatim from agent-monitor-rust `direct_llm/retry.rs`.

use crate::llm_typed_tasks::TypedLlmRetryPolicy;
use bos_kernel::{AppError, AppResult, CorrelationId, RetryClass};
use std::collections::BTreeMap;
use std::thread;
use std::time::{Duration, Instant};

pub(crate) trait DirectLlmRetrySleeper: Send + Sync {
    fn sleep(&self, delay: Duration);
}

pub(crate) struct ThreadDirectLlmRetrySleeper;

impl DirectLlmRetrySleeper for ThreadDirectLlmRetrySleeper {
    fn sleep(&self, delay: Duration) {
        thread::sleep(delay);
    }
}

pub(crate) fn validate_retry_policy(policy: &TypedLlmRetryPolicy) -> AppResult<()> {
    if policy.max_attempts == 0 {
        return Err(AppError::invalid_input(
            "direct_llm_retry_policy_invalid",
            "direct LLM retry policy max_attempts must be at least 1",
            CorrelationId::generate(),
        ));
    }
    if policy.max_attempts > 1 && policy.max_elapsed_ms == 0 {
        return Err(AppError::invalid_input(
            "direct_llm_retry_policy_invalid",
            "direct LLM retry policy max_elapsed_ms must be positive when retries are enabled",
            CorrelationId::generate(),
        ));
    }
    Ok(())
}

pub(crate) fn status_retry_delay(
    status: u16,
    headers: &BTreeMap<String, String>,
    policy: &TypedLlmRetryPolicy,
) -> Option<Duration> {
    if !is_retryable_status(status) {
        return None;
    }
    retry_after_delay(headers).or_else(|| Some(Duration::from_millis(policy.backoff_ms)))
}

pub(crate) fn app_error_retry_delay(
    error: &AppError,
    policy: &TypedLlmRetryPolicy,
) -> Option<Duration> {
    if error.retry() != RetryClass::Backoff {
        return None;
    }
    Some(Duration::from_millis(policy.backoff_ms))
}

pub(crate) fn retry_allowed(
    policy: &TypedLlmRetryPolicy,
    attempts_used: u8,
    started: Instant,
    delay: Duration,
) -> bool {
    if attempts_used >= policy.max_attempts {
        return false;
    }
    if policy.max_elapsed_ms == 0 {
        return true;
    }
    let max_elapsed = Duration::from_millis(policy.max_elapsed_ms);
    match started.elapsed().checked_add(delay) {
        Some(projected_elapsed) => projected_elapsed <= max_elapsed,
        None => false,
    }
}

fn is_retryable_status(status: u16) -> bool {
    status == 429 || (500..=599).contains(&status)
}

fn retry_after_delay(headers: &BTreeMap<String, String>) -> Option<Duration> {
    let value = headers.get("retry-after")?.trim();
    if value.is_empty() {
        return None;
    }
    let seconds = value.parse::<u64>().ok()?;
    seconds.checked_mul(1_000).map(Duration::from_millis)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_typed_tasks::TypedLlmRetryPolicy;

    fn policy() -> TypedLlmRetryPolicy {
        TypedLlmRetryPolicy {
            max_attempts: 3,
            backoff_ms: 25,
            max_elapsed_ms: 100,
        }
    }

    #[test]
    fn status_retry_delay_prefers_retry_after_seconds() {
        let headers = BTreeMap::from([("retry-after".to_string(), "2".to_string())]);

        let delay = status_retry_delay(429, &headers, &policy());

        assert_eq!(delay, Some(Duration::from_millis(2_000)));
    }

    #[test]
    fn status_retry_delay_uses_policy_backoff_for_retryable_status() {
        let delay = status_retry_delay(503, &BTreeMap::new(), &policy());

        assert_eq!(delay, Some(Duration::from_millis(25)));
    }

    #[test]
    fn status_retry_delay_rejects_non_retryable_status() {
        let delay = status_retry_delay(400, &BTreeMap::new(), &policy());

        assert!(delay.is_none());
    }

    #[test]
    fn retry_allowed_stops_at_max_attempts() {
        assert!(!retry_allowed(
            &policy(),
            3,
            Instant::now(),
            Duration::from_millis(0)
        ));
    }

    #[test]
    fn retry_allowed_stops_when_delay_exceeds_elapsed_budget() {
        assert!(!retry_allowed(
            &policy(),
            1,
            Instant::now(),
            Duration::from_millis(101)
        ));
    }
}
