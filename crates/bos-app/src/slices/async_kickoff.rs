//! Shared idempotent async-kickoff coordinator for slice-owned background work.
//!
//! This is the one path for the recurring shape:
//! validate read-only preconditions -> suppress duplicate/capacity races ->
//! record an idempotent kickoff receipt through the owning slice/store ->
//! spawn work only for a newly-applied kickoff.
//!
//! It does not replace `store_core`; callers provide the durable mutation
//! closure so receipts still belong to the owning slice/entity.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum KickoffCapacity {
    Unbounded,
    Limited {
        group: &'static str,
        max_concurrent: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KickoffSpec<'a> {
    pub slice_id: &'a str,
    pub draft_id: &'a str,
    pub planned_run_id: &'a str,
    pub capacity: KickoffCapacity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecordedKickoff {
    pub run_id: String,
    pub replayed: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum KickoffDecision {
    Spawn { run_id: String, guard: KickoffGuard },
    Replayed { run_id: String },
    AlreadyRunning { run_id: String },
    CapacityExceeded,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct KickoffGuard {
    key: String,
    capacity_group: Option<&'static str>,
}

impl Drop for KickoffGuard {
    fn drop(&mut self) {
        let mut state = kickoff_state().lock();
        state.by_key.remove(&self.key);
        if let Some(group) = self.capacity_group {
            if let Some(active) = state.by_group.get_mut(group) {
                *active = active.saturating_sub(1);
                if *active == 0 {
                    state.by_group.remove(group);
                }
            }
        }
    }
}

pub(crate) fn begin<E>(
    spec: KickoffSpec<'_>,
    record: impl FnOnce() -> Result<RecordedKickoff, E>,
) -> Result<KickoffDecision, E> {
    let guard = match begin_guard(&spec) {
        GuardBegin::Started(guard) => guard,
        GuardBegin::AlreadyRunning(run_id) => {
            return Ok(KickoffDecision::AlreadyRunning { run_id });
        }
        GuardBegin::CapacityExceeded => return Ok(KickoffDecision::CapacityExceeded),
    };

    let recorded = record()?;
    if recorded.replayed {
        drop(guard);
        return Ok(KickoffDecision::Replayed {
            run_id: recorded.run_id,
        });
    }

    Ok(KickoffDecision::Spawn {
        run_id: recorded.run_id,
        guard,
    })
}

enum GuardBegin {
    Started(KickoffGuard),
    AlreadyRunning(String),
    CapacityExceeded,
}

fn begin_guard(spec: &KickoffSpec<'_>) -> GuardBegin {
    let key = kickoff_key(spec.slice_id, spec.draft_id);
    let mut state = kickoff_state().lock();
    if let Some(active_run_id) = state.by_key.get(&key) {
        return GuardBegin::AlreadyRunning(active_run_id.clone());
    }

    let capacity_group = match spec.capacity {
        KickoffCapacity::Unbounded => None,
        KickoffCapacity::Limited {
            group,
            max_concurrent,
        } => {
            let active = *state.by_group.get(group).unwrap_or(&0);
            if active >= max_concurrent.max(1) {
                return GuardBegin::CapacityExceeded;
            }
            state.by_group.insert(group, active + 1);
            Some(group)
        }
    };

    state
        .by_key
        .insert(key.clone(), spec.planned_run_id.to_string());
    GuardBegin::Started(KickoffGuard {
        key,
        capacity_group,
    })
}

fn kickoff_key(slice_id: &str, draft_id: &str) -> String {
    format!("{slice_id}:{draft_id}")
}

#[derive(Default)]
struct KickoffState {
    by_key: HashMap<String, String>,
    by_group: HashMap<&'static str, usize>,
}

fn kickoff_state() -> &'static Mutex<KickoffState> {
    static IN_FLIGHT: OnceLock<Mutex<KickoffState>> = OnceLock::new();
    IN_FLIGHT.get_or_init(|| Mutex::new(KickoffState::default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(run_id: &str, replayed: bool) -> Result<RecordedKickoff, ()> {
        Ok(RecordedKickoff {
            run_id: run_id.to_string(),
            replayed,
        })
    }

    fn spec<'a>(draft_id: &'a str, run_id: &'a str, capacity: KickoffCapacity) -> KickoffSpec<'a> {
        KickoffSpec {
            slice_id: "test_slice",
            draft_id,
            planned_run_id: run_id,
            capacity,
        }
    }

    #[test]
    fn duplicate_same_draft_returns_active_run_before_capacity_check() {
        let guard = match begin(
            spec(
                "draft_same_before_capacity",
                "run_first",
                KickoffCapacity::Limited {
                    group: "test_capacity_same_draft",
                    max_concurrent: 1,
                },
            ),
            || record("run_first", false),
        )
        .expect("begin")
        {
            KickoffDecision::Spawn { guard, .. } => guard,
            other => panic!("unexpected first decision: {other:?}"),
        };

        assert_eq!(
            begin(
                spec(
                    "draft_same_before_capacity",
                    "run_retry",
                    KickoffCapacity::Limited {
                        group: "test_capacity_same_draft",
                        max_concurrent: 1,
                    },
                ),
                || record("run_retry", false),
            )
            .expect("retry"),
            KickoffDecision::AlreadyRunning {
                run_id: "run_first".to_string(),
            }
        );
        drop(guard);
    }

    #[test]
    fn capacity_exceeded_does_not_call_record() {
        let guard = match begin(
            spec(
                "draft_capacity_first",
                "run_first",
                KickoffCapacity::Limited {
                    group: "test_capacity_exceeded",
                    max_concurrent: 1,
                },
            ),
            || record("run_first", false),
        )
        .expect("begin")
        {
            KickoffDecision::Spawn { guard, .. } => guard,
            other => panic!("unexpected first decision: {other:?}"),
        };

        let mut called = false;
        assert_eq!(
            begin(
                spec(
                    "draft_capacity_second",
                    "run_second",
                    KickoffCapacity::Limited {
                        group: "test_capacity_exceeded",
                        max_concurrent: 1,
                    },
                ),
                || {
                    called = true;
                    record("run_second", false)
                },
            )
            .expect("capacity"),
            KickoffDecision::CapacityExceeded
        );
        assert!(!called, "capacity refusal must not record a kickoff");
        drop(guard);
    }

    #[test]
    fn replay_releases_guard_without_spawning() {
        assert_eq!(
            begin(
                spec(
                    "draft_replay_release",
                    "run_replay",
                    KickoffCapacity::Unbounded
                ),
                || record("run_original", true),
            )
            .expect("replay"),
            KickoffDecision::Replayed {
                run_id: "run_original".to_string(),
            }
        );

        let guard = match begin(
            spec(
                "draft_replay_release",
                "run_after",
                KickoffCapacity::Unbounded,
            ),
            || record("run_after", false),
        )
        .expect("after replay")
        {
            KickoffDecision::Spawn { guard, .. } => guard,
            other => panic!("guard was not released after replay: {other:?}"),
        };
        drop(guard);
    }
}
