use std::convert::TryFrom;
use std::fmt::{self, Display, Formatter};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidId {
    type_name: &'static str,
}

impl InvalidId {
    fn new(type_name: &'static str) -> Self {
        Self { type_name }
    }

    pub fn type_name(&self) -> &'static str {
        self.type_name
    }
}

impl std::error::Error for InvalidId {}

impl Display for InvalidId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{} cannot be empty", self.type_name)
    }
}

macro_rules! define_id {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self::try_new(value).expect(concat!(stringify!($name), " must not be empty"))
            }

            pub fn try_new(value: impl Into<String>) -> Result<Self, InvalidId> {
                let value = value.into();
                let normalized = value.trim();
                if normalized.is_empty() {
                    return Err(InvalidId::new(stringify!($name)));
                }

                Ok(Self(normalized.to_owned()))
            }

            #[allow(dead_code)]
            pub(crate) fn new_unchecked(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<&str> for $name {
            type Error = InvalidId;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::try_new(value)
            }
        }

        impl TryFrom<String> for $name {
            type Error = InvalidId;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::try_new(value)
            }
        }

        impl Display for $name {
            fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }
    };
}

define_id!(CommandId);
define_id!(EventId);
define_id!(ThreadId);
define_id!(ParticipantId);
define_id!(MessageId);
define_id!(DeliveryId);
define_id!(AgentSessionId);
define_id!(WorkflowId);
define_id!(ExecutionId);
define_id!(CorrelationId);
define_id!(CausationId);
define_id!(IdempotencyKey);
define_id!(LeaseId);
define_id!(OutboxMessageId);

impl CorrelationId {
    pub fn generate() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::time::{SystemTime, UNIX_EPOCH};

        static SEQUENCE: AtomicU64 = AtomicU64::new(1);

        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_millis();
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        Self::new(format!("corr_{timestamp_ms:x}_{sequence:x}"))
    }
}

#[cfg(test)]
mod tests {
    use std::convert::TryFrom;

    use super::{CorrelationId, IdempotencyKey};

    #[test]
    fn rejects_blank_ids() {
        assert!(IdempotencyKey::try_new("   ").is_err());
    }

    #[test]
    fn trims_valid_ids() {
        let id = CorrelationId::new(" corr_1 ");
        assert_eq!(id.as_str(), "corr_1");
    }

    #[test]
    fn critical_ids_support_fallible_construction() {
        assert!(CorrelationId::try_from("   ").is_err());
        assert!(IdempotencyKey::try_from(String::from("")).is_err());
    }
}
