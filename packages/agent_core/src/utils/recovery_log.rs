use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const WARNING_INTERVAL_MS: u64 = 60_000;
const ENTRY_TTL_MS: u64 = 10 * 60_000;

#[derive(Clone, Default)]
pub(crate) struct RecoveryLog {
    entries: Arc<Mutex<HashMap<String, Entry>>>,
}

#[derive(Clone, Copy)]
struct Entry {
    first_failure_at: u64,
    last_failure_at: u64,
    last_warning_at: u64,
    suppressed: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FailureLog {
    Warn,
    Debug,
    WarnWithRepeats(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Recovery {
    pub(crate) unavailable_for_ms: u64,
    pub(crate) suppressed: u64,
}

impl RecoveryLog {
    pub(crate) fn record_failure(&self, key: String, now_ms: u64) -> FailureLog {
        let mut entries = self.entries.lock().expect("recovery log lock poisoned");
        entries.retain(|_, entry| now_ms.saturating_sub(entry.last_failure_at) < ENTRY_TTL_MS);

        let Some(entry) = entries.get_mut(&key) else {
            entries.insert(
                key,
                Entry {
                    first_failure_at: now_ms,
                    last_failure_at: now_ms,
                    last_warning_at: now_ms,
                    suppressed: 0,
                },
            );
            return FailureLog::Warn;
        };

        entry.last_failure_at = now_ms;
        if now_ms.saturating_sub(entry.last_warning_at) >= WARNING_INTERVAL_MS {
            let repeats = entry.suppressed + 1;
            entry.last_warning_at = now_ms;
            entry.suppressed = 0;
            FailureLog::WarnWithRepeats(repeats)
        } else {
            entry.suppressed += 1;
            FailureLog::Debug
        }
    }

    pub(crate) fn record_recovery(&self, key: &str, now_ms: u64) -> Option<Recovery> {
        let entry = self
            .entries
            .lock()
            .expect("recovery log lock poisoned")
            .remove(key)?;
        if now_ms.saturating_sub(entry.last_failure_at) >= ENTRY_TTL_MS {
            return None;
        }
        Some(Recovery {
            unavailable_for_ms: now_ms.saturating_sub(entry.first_failure_at),
            suppressed: entry.suppressed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{FailureLog, RecoveryLog};

    #[test]
    fn deduplicates_and_reports_recovery() {
        let log = RecoveryLog::default();
        assert_eq!(log.record_failure("tunnel".into(), 1_000), FailureLog::Warn);
        assert_eq!(
            log.record_failure("tunnel".into(), 2_000),
            FailureLog::Debug
        );
        assert_eq!(
            log.record_failure("tunnel".into(), 61_000),
            FailureLog::WarnWithRepeats(2)
        );

        let recovery = log.record_recovery("tunnel", 66_000).unwrap();
        assert_eq!(recovery.unavailable_for_ms, 65_000);
        assert_eq!(recovery.suppressed, 0);
        assert!(log.record_recovery("tunnel", 67_000).is_none());
    }
}
