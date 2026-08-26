use std::error::Error;
use std::fmt::{self, Display, Formatter};

use crate::{CorrelationId, ExecutionContext};

pub type AppResult<T> = Result<T, AppError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    InvalidInput,
    InvalidState,
    Conflict,
    ConcurrentModification,
    NotFound,
    Policy,
    ExternalDependency,
    Timeout,
    BudgetExceeded,
    Unauthorized,
    Transient,
    Unexpected,
}

pub type AppErrorKind = ErrorCode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryClass {
    Never,
    AfterReload,
    Backoff,
}

impl ErrorCode {
    pub fn default_retry_class(self) -> RetryClass {
        match self {
            Self::ConcurrentModification => RetryClass::AfterReload,
            Self::ExternalDependency | Self::Timeout | Self::Transient => RetryClass::Backoff,
            Self::InvalidInput
            | Self::InvalidState
            | Self::Conflict
            | Self::NotFound
            | Self::Policy
            | Self::BudgetExceeded
            | Self::Unauthorized
            | Self::Unexpected => RetryClass::Never,
        }
    }
}

impl Display for ErrorCode {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::InvalidInput => "invalid_input",
            Self::InvalidState => "invalid_state",
            Self::Conflict => "conflict",
            Self::ConcurrentModification => "concurrent_modification",
            Self::NotFound => "not_found",
            Self::Policy => "policy",
            Self::ExternalDependency => "external_dependency",
            Self::Timeout => "timeout",
            Self::BudgetExceeded => "budget_exceeded",
            Self::Unauthorized => "unauthorized",
            Self::Transient => "transient",
            Self::Unexpected => "unexpected",
        };
        f.write_str(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppError {
    kind: ErrorCode,
    code: &'static str,
    message: String,
    retry: RetryClass,
    correlation_id: CorrelationId,
}

impl AppError {
    pub fn new(
        kind: ErrorCode,
        code: &'static str,
        message: impl Into<String>,
        correlation_id: CorrelationId,
    ) -> Self {
        Self {
            kind,
            code,
            message: message.into(),
            retry: kind.default_retry_class(),
            correlation_id,
        }
    }

    pub fn from_context(
        kind: ErrorCode,
        code: &'static str,
        message: impl Into<String>,
        ctx: &ExecutionContext,
    ) -> Self {
        Self::new(kind, code, message, ctx.correlation_id.clone())
    }

    pub fn conflict(
        code: &'static str,
        message: impl Into<String>,
        correlation_id: CorrelationId,
    ) -> Self {
        Self::new(ErrorCode::Conflict, code, message, correlation_id)
    }

    pub fn concurrent_modification(
        code: &'static str,
        message: impl Into<String>,
        correlation_id: CorrelationId,
    ) -> Self {
        Self::new(
            ErrorCode::ConcurrentModification,
            code,
            message,
            correlation_id,
        )
    }

    pub fn invalid_input(
        code: &'static str,
        message: impl Into<String>,
        correlation_id: CorrelationId,
    ) -> Self {
        Self::new(ErrorCode::InvalidInput, code, message, correlation_id)
    }

    pub fn invalid_input_from_context(
        code: &'static str,
        message: impl Into<String>,
        ctx: &ExecutionContext,
    ) -> Self {
        Self::from_context(ErrorCode::InvalidInput, code, message, ctx)
    }

    pub fn not_found(
        code: &'static str,
        message: impl Into<String>,
        correlation_id: CorrelationId,
    ) -> Self {
        Self::new(ErrorCode::NotFound, code, message, correlation_id)
    }

    pub fn policy(
        code: &'static str,
        message: impl Into<String>,
        correlation_id: CorrelationId,
    ) -> Self {
        Self::new(ErrorCode::Policy, code, message, correlation_id)
    }

    pub fn transient(
        code: &'static str,
        message: impl Into<String>,
        correlation_id: CorrelationId,
    ) -> Self {
        Self::new(ErrorCode::Transient, code, message, correlation_id)
    }

    pub fn unexpected(
        code: &'static str,
        message: impl Into<String>,
        correlation_id: CorrelationId,
    ) -> Self {
        Self::new(ErrorCode::Unexpected, code, message, correlation_id)
    }

    pub fn with_retry(mut self, retry: RetryClass) -> Self {
        self.retry = retry;
        self
    }

    pub fn kind(&self) -> ErrorCode {
        self.kind
    }

    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn retry(&self) -> RetryClass {
        self.retry
    }

    pub fn correlation_id(&self) -> &CorrelationId {
        &self.correlation_id
    }
}

impl Display for AppError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{} [{}]: {}", self.code, self.kind, self.message)
    }
}

impl Error for AppError {}

#[cfg(test)]
mod tests {
    use crate::CorrelationId;

    use super::{AppError, ErrorCode, RetryClass};

    #[test]
    fn assigns_default_retry_class() {
        let error = AppError::new(
            ErrorCode::Transient,
            "temporary_failure",
            "transient",
            CorrelationId::new("corr_1"),
        );
        assert_eq!(error.retry(), RetryClass::Backoff);
    }

    #[test]
    fn invalid_input_requires_correlation_id() {
        let error =
            AppError::invalid_input("bad_request", "invalid field", CorrelationId::new("corr_2"));
        assert_eq!(error.kind(), ErrorCode::InvalidInput);
        assert_eq!(error.code(), "bad_request");
        assert_eq!(error.correlation_id().as_str(), "corr_2");
    }

    #[test]
    fn conflict_does_not_retry_by_default() {
        let error = AppError::conflict(
            "thread_conflict",
            "thread already closed",
            CorrelationId::new("corr_3"),
        );
        assert_eq!(error.retry(), RetryClass::Never);
    }

    #[test]
    fn concurrent_modification_requires_reload_before_retry() {
        let error = AppError::concurrent_modification(
            "optimistic_lock_failed",
            "aggregate version moved",
            CorrelationId::new("corr_4"),
        );
        assert_eq!(error.retry(), RetryClass::AfterReload);
    }
}
