//! Rate limiting for connection-level IPC failures.
//!
//! A client that connects and immediately disconnects produces the same
//! `Broken pipe` error every few seconds. Logging each one at WARN floods the
//! journal (and breadbug's daemon-warning count) with a condition the daemon
//! cannot act on. This module keeps the first failure and one aggregate
//! warning per peer and error per interval, and pushes repeats to DEBUG.
//!
//! It deliberately keys on peer plus error text: a *different* error from the
//! same peer (or the same error from another peer) is never suppressed, so a
//! real second fault is not hidden behind a noisy first one.

use std::collections::HashMap;
use std::fmt;
use std::time::{Duration, Instant};

/// How long a peer's identical failure is suppressed after a WARN. Five
/// minutes turns a 2-second reconnect loop (roughly 150 lines per peer per
/// five minutes) into one line, while still surfacing a persistent fault.
pub(crate) const DEFAULT_LOG_INTERVAL: Duration = Duration::from_secs(300);

/// Upper bound on tracked keys, so churning client PIDs cannot grow the map
/// without limit. Stale entries are dropped when the cap is reached.
const MAX_TRACKED_KEYS: usize = 256;

/// Identity attached to a connection attempt, from `SO_PEERCRED`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum PeerId {
    /// Linux: the peer process id (`pid_t`).
    Pid(i32),
    /// Fallback when the platform exposes credentials but no pid.
    Uid { uid: u32, gid: u32 },
    /// Peer credentials unavailable; still rate-limited, just unlabelled.
    Unknown,
}

impl fmt::Display for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PeerId::Pid(pid) => write!(f, "pid {pid}"),
            PeerId::Uid { uid, gid } => write!(f, "uid {uid} gid {gid}"),
            PeerId::Unknown => f.write_str("unknown peer"),
        }
    }
}

/// What the caller should do with a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureLogDecision {
    /// Log at WARN. `suppressed` is the number of identical failures hidden
    /// since the previous warning (0 for a first failure).
    Warn { suppressed: u64 },
    /// Already warned about this peer and error within the interval.
    Suppressed,
}

#[derive(Debug)]
struct Entry {
    last_warn: Instant,
    suppressed: u64,
}

/// Per-peer, per-error failure log used by the IPC accept loop.
#[derive(Debug)]
pub(crate) struct ConnectionFailureLog {
    interval: Duration,
    entries: HashMap<(PeerId, String), Entry>,
}

impl Default for ConnectionFailureLog {
    fn default() -> Self {
        Self::new(DEFAULT_LOG_INTERVAL)
    }
}

impl ConnectionFailureLog {
    pub(crate) fn new(interval: Duration) -> Self {
        Self {
            interval,
            entries: HashMap::new(),
        }
    }

    /// Record a failure observed at `now` and decide whether to warn.
    ///
    /// `now` is injected so tests do not depend on wall-clock sleeps.
    pub(crate) fn record(&mut self, peer: PeerId, error: &str, now: Instant) -> FailureLogDecision {
        self.prune(now);

        let key = (peer, error.to_string());
        if let Some(entry) = self.entries.get_mut(&key) {
            if now.saturating_duration_since(entry.last_warn) < self.interval {
                entry.suppressed = entry.suppressed.saturating_add(1);
                return FailureLogDecision::Suppressed;
            }

            let suppressed = std::mem::replace(&mut entry.suppressed, 0);
            entry.last_warn = now;
            return FailureLogDecision::Warn { suppressed };
        }

        self.entries.insert(
            key,
            Entry {
                last_warn: now,
                suppressed: 0,
            },
        );
        FailureLogDecision::Warn { suppressed: 0 }
    }

    /// Drop entries that are no longer eligible for suppression, and enforce
    /// the hard cap by evicting the least recently warned key.
    fn prune(&mut self, now: Instant) {
        if self.entries.len() < MAX_TRACKED_KEYS {
            return;
        }

        self.entries
            .retain(|_, entry| now.saturating_duration_since(entry.last_warn) < self.interval);

        while self.entries.len() >= MAX_TRACKED_KEYS {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_warn)
                .map(|(key, _)| key.clone());
            match oldest {
                Some(key) => {
                    self.entries.remove(&key);
                }
                None => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BROKEN_PIPE: &str = "Broken pipe (os error 32)";
    const RESET: &str = "Connection reset by peer (os error 104)";

    #[test]
    fn first_failure_warns() {
        let start = Instant::now();
        let mut log = ConnectionFailureLog::new(DEFAULT_LOG_INTERVAL);

        assert_eq!(
            log.record(PeerId::Pid(42), BROKEN_PIPE, start),
            FailureLogDecision::Warn { suppressed: 0 }
        );
    }

    #[test]
    fn repeated_identical_failures_are_suppressed_then_summarized() {
        let start = Instant::now();
        let mut log = ConnectionFailureLog::new(DEFAULT_LOG_INTERVAL);

        assert_eq!(
            log.record(PeerId::Pid(42), BROKEN_PIPE, start),
            FailureLogDecision::Warn { suppressed: 0 }
        );

        for second in 1..=100 {
            assert_eq!(
                log.record(
                    PeerId::Pid(42),
                    BROKEN_PIPE,
                    start + Duration::from_secs(second)
                ),
                FailureLogDecision::Suppressed,
                "repeat {second} should not warn again"
            );
        }

        // One aggregate warning once the interval has elapsed.
        assert_eq!(
            log.record(
                PeerId::Pid(42),
                BROKEN_PIPE,
                start + DEFAULT_LOG_INTERVAL + Duration::from_secs(1)
            ),
            FailureLogDecision::Warn { suppressed: 100 }
        );
    }

    #[test]
    fn different_peer_still_warns() {
        let start = Instant::now();
        let mut log = ConnectionFailureLog::new(DEFAULT_LOG_INTERVAL);

        assert_eq!(
            log.record(PeerId::Pid(42), BROKEN_PIPE, start),
            FailureLogDecision::Warn { suppressed: 0 }
        );
        assert_eq!(
            log.record(PeerId::Pid(42), BROKEN_PIPE, start + Duration::from_secs(1)),
            FailureLogDecision::Suppressed
        );
        assert_eq!(
            log.record(PeerId::Pid(43), BROKEN_PIPE, start + Duration::from_secs(2)),
            FailureLogDecision::Warn { suppressed: 0 }
        );
    }

    #[test]
    fn different_error_from_same_peer_still_warns() {
        let start = Instant::now();
        let mut log = ConnectionFailureLog::new(DEFAULT_LOG_INTERVAL);

        assert_eq!(
            log.record(PeerId::Pid(42), BROKEN_PIPE, start),
            FailureLogDecision::Warn { suppressed: 0 }
        );
        assert_eq!(
            log.record(PeerId::Pid(42), RESET, start + Duration::from_secs(1)),
            FailureLogDecision::Warn { suppressed: 0 }
        );
    }

    #[test]
    fn unknown_peer_is_rate_limited_separately() {
        let start = Instant::now();
        let mut log = ConnectionFailureLog::new(DEFAULT_LOG_INTERVAL);

        assert_eq!(
            log.record(PeerId::Unknown, BROKEN_PIPE, start),
            FailureLogDecision::Warn { suppressed: 0 }
        );
        assert_eq!(
            log.record(PeerId::Unknown, BROKEN_PIPE, start + Duration::from_secs(1)),
            FailureLogDecision::Suppressed
        );
    }

    #[test]
    fn tracked_keys_stay_bounded() {
        let start = Instant::now();
        let mut log = ConnectionFailureLog::new(DEFAULT_LOG_INTERVAL);

        for pid in 0..(MAX_TRACKED_KEYS as i32 * 4) {
            log.record(PeerId::Pid(pid), BROKEN_PIPE, start);
        }

        assert!(log.entries.len() <= MAX_TRACKED_KEYS);
    }
}
