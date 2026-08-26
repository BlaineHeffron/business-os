use std::collections::HashMap;
use std::convert::TryFrom;
use std::fmt::{self, Display, Formatter};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RevisionToken(u64);

impl RevisionToken {
    pub const fn initial() -> Self {
        Self(0)
    }

    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn value(self) -> u64 {
        self.0
    }

    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

impl Default for RevisionToken {
    fn default() -> Self {
        Self::initial()
    }
}

impl From<u64> for RevisionToken {
    fn from(value: u64) -> Self {
        Self::new(value)
    }
}

impl Display for RevisionToken {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidCurrentStateIdempotencyKey;

impl std::error::Error for InvalidCurrentStateIdempotencyKey {}

impl Display for InvalidCurrentStateIdempotencyKey {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str("CurrentStateIdempotencyKey cannot be empty")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CurrentStateIdempotencyKey(String);

impl CurrentStateIdempotencyKey {
    pub fn new(value: impl Into<String>) -> Self {
        Self::try_new(value).expect("CurrentStateIdempotencyKey must not be empty")
    }

    pub fn try_new(value: impl Into<String>) -> Result<Self, InvalidCurrentStateIdempotencyKey> {
        let value = value.into();
        let normalized = value.trim();
        if normalized.is_empty() {
            return Err(InvalidCurrentStateIdempotencyKey);
        }

        Ok(Self(normalized.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for CurrentStateIdempotencyKey {
    type Error = InvalidCurrentStateIdempotencyKey;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl TryFrom<String> for CurrentStateIdempotencyKey {
    type Error = InvalidCurrentStateIdempotencyKey;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl Display for CurrentStateIdempotencyKey {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl AsRef<str> for CurrentStateIdempotencyKey {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleRevisionError {
    expected: RevisionToken,
    actual: RevisionToken,
}

impl StaleRevisionError {
    pub const fn new(expected: RevisionToken, actual: RevisionToken) -> Self {
        Self { expected, actual }
    }

    pub const fn expected(&self) -> RevisionToken {
        self.expected
    }

    pub const fn actual(&self) -> RevisionToken {
        self.actual
    }
}

impl std::error::Error for StaleRevisionError {}

impl Display for StaleRevisionError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "stale revision: expected {}, actual {}",
            self.expected, self.actual
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppendResult {
    Appended {
        revision: RevisionToken,
        idempotency_key: CurrentStateIdempotencyKey,
    },
    Duplicate {
        revision: RevisionToken,
        idempotency_key: CurrentStateIdempotencyKey,
    },
}

impl AppendResult {
    pub fn appended(revision: RevisionToken, idempotency_key: CurrentStateIdempotencyKey) -> Self {
        Self::Appended {
            revision,
            idempotency_key,
        }
    }

    pub fn duplicate(revision: RevisionToken, idempotency_key: CurrentStateIdempotencyKey) -> Self {
        Self::Duplicate {
            revision,
            idempotency_key,
        }
    }

    pub fn revision(&self) -> RevisionToken {
        match self {
            Self::Appended { revision, .. } => *revision,
            Self::Duplicate { revision, .. } => *revision,
        }
    }

    pub fn idempotency_key(&self) -> &CurrentStateIdempotencyKey {
        match self {
            Self::Appended {
                idempotency_key, ..
            } => idempotency_key,
            Self::Duplicate {
                idempotency_key, ..
            } => idempotency_key,
        }
    }

    pub fn is_appended(&self) -> bool {
        matches!(self, Self::Appended { .. })
    }

    pub fn is_duplicate(&self) -> bool {
        matches!(self, Self::Duplicate { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditLogEntry<E> {
    revision: RevisionToken,
    idempotency_key: CurrentStateIdempotencyKey,
    payload: E,
}

impl<E> AuditLogEntry<E> {
    pub fn new(
        revision: RevisionToken,
        idempotency_key: CurrentStateIdempotencyKey,
        payload: E,
    ) -> Self {
        Self {
            revision,
            idempotency_key,
            payload,
        }
    }

    pub fn revision(&self) -> RevisionToken {
        self.revision
    }

    pub fn idempotency_key(&self) -> &CurrentStateIdempotencyKey {
        &self.idempotency_key
    }

    pub fn payload(&self) -> &E {
        &self.payload
    }

    pub fn into_payload(self) -> E {
        self.payload
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditedCurrentState<S, E> {
    state: S,
    revision: RevisionToken,
    audit_log: Vec<AuditLogEntry<E>>,
    idempotency_keys: HashMap<CurrentStateIdempotencyKey, RevisionToken>,
}

impl<S, E> AuditedCurrentState<S, E> {
    pub fn new(state: S) -> Self {
        Self {
            state,
            revision: RevisionToken::initial(),
            audit_log: Vec::new(),
            idempotency_keys: HashMap::new(),
        }
    }

    pub fn state(&self) -> &S {
        &self.state
    }

    pub fn revision(&self) -> RevisionToken {
        self.revision
    }

    pub fn audit_log(&self) -> &[AuditLogEntry<E>] {
        &self.audit_log
    }

    pub fn append<F>(
        &mut self,
        expected_revision: RevisionToken,
        idempotency_key: CurrentStateIdempotencyKey,
        payload: E,
        apply: F,
    ) -> Result<AppendResult, StaleRevisionError>
    where
        F: FnOnce(&mut S, &E),
    {
        if let Some(revision) = self.idempotency_keys.get(&idempotency_key).copied() {
            return Ok(AppendResult::duplicate(revision, idempotency_key));
        }

        if expected_revision != self.revision {
            return Err(StaleRevisionError::new(expected_revision, self.revision));
        }

        apply(&mut self.state, &payload);

        self.revision = self.revision.next();
        self.idempotency_keys
            .insert(idempotency_key.clone(), self.revision);

        let result = AppendResult::appended(self.revision, idempotency_key.clone());
        self.audit_log
            .push(AuditLogEntry::new(self.revision, idempotency_key, payload));

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AppendResult, AuditedCurrentState, CurrentStateIdempotencyKey, RevisionToken,
        StaleRevisionError,
    };

    #[test]
    fn duplicate_idempotency_key_returns_existing_revision_without_reapplying() {
        let mut state = AuditedCurrentState::<i32, i32>::new(0);
        let key = CurrentStateIdempotencyKey::new(" set-score ");

        let first = state
            .append(RevisionToken::initial(), key.clone(), 7, |state, event| {
                *state += event;
            })
            .expect("first append");

        assert!(first.is_appended());
        assert_eq!(first.revision(), RevisionToken::new(1));
        assert_eq!(*state.state(), 7);
        assert_eq!(state.audit_log().len(), 1);

        let duplicate = state
            .append(RevisionToken::new(1), key.clone(), 100, |state, event| {
                *state += event;
            })
            .expect("duplicate append");

        assert_eq!(
            duplicate,
            AppendResult::duplicate(RevisionToken::new(1), key)
        );
        assert!(duplicate.is_duplicate());
        assert_eq!(*state.state(), 7);
        assert_eq!(state.audit_log().len(), 1);
    }

    #[test]
    fn duplicate_idempotency_key_returns_original_revision_after_intervening_appends() {
        let mut state = AuditedCurrentState::<i32, i32>::new(0);
        let original_key = CurrentStateIdempotencyKey::new("operator-action:event-1");
        let second_key = CurrentStateIdempotencyKey::new("operator-action:event-2");

        state
            .append(
                RevisionToken::initial(),
                original_key.clone(),
                7,
                |state, event| *state += event,
            )
            .expect("first append");
        state
            .append(RevisionToken::new(1), second_key, 3, |state, event| {
                *state += event;
            })
            .expect("second append");

        let duplicate = state
            .append(RevisionToken::new(2), original_key, 100, |state, event| {
                *state += event;
            })
            .expect("duplicate append");

        assert_eq!(
            duplicate,
            AppendResult::duplicate(
                RevisionToken::new(1),
                CurrentStateIdempotencyKey::new("operator-action:event-1")
            )
        );
        assert_eq!(*state.state(), 10);
        assert_eq!(state.audit_log().len(), 2);
    }

    #[test]
    fn stale_revision_rejects_new_idempotency_key_without_mutating_state() {
        let mut state = AuditedCurrentState::<Vec<&'static str>, &'static str>::new(Vec::new());

        state
            .append(
                RevisionToken::initial(),
                CurrentStateIdempotencyKey::new("first"),
                "created",
                |state, event| state.push(event),
            )
            .expect("first append");

        let error = state
            .append(
                RevisionToken::initial(),
                CurrentStateIdempotencyKey::new("second"),
                "stale",
                |state, event| state.push(event),
            )
            .expect_err("stale revision");

        assert_eq!(
            error,
            StaleRevisionError::new(RevisionToken::initial(), RevisionToken::new(1))
        );
        assert_eq!(state.state(), &vec!["created"]);
        assert_eq!(state.audit_log().len(), 1);
    }

    #[test]
    fn idempotency_key_rejects_blank_values_and_trims_valid_values() {
        assert!(CurrentStateIdempotencyKey::try_new("  ").is_err());
        assert_eq!(
            CurrentStateIdempotencyKey::new("  projection:1  ").as_str(),
            "projection:1"
        );
    }
}
