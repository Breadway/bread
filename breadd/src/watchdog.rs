//! Freeze detection for sibling apps.
//!
//! Apps that integrate with breadbug send `bug.heartbeat` from their main
//! loop. If the heartbeats stop while the process is still alive, the app is
//! almost certainly blocked: breadd records a `freeze` marker and publishes
//! `bread.bug.freeze`; when heartbeats resume it records `recovered`.
//!
//! The detection logic is a pure function of (time, heartbeats, liveness)
//! so it can be tested without sleeping. False positives are avoided by:
//!
//! * a floor on the timeout and a multiple of the app's own interval;
//! * monotonic time, which does not advance during suspend;
//! * a global grace window after a stalled tick (suspend/resume, heavy
//!   load) and after power or monitor events (resume, hotplug);
//! * dropping apps whose process has exited instead of calling them frozen.
//!
//! Nothing here kills or signals a process.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long detection is paused after a stalled tick or a power/monitor event.
pub const GRACE: Duration = Duration::from_secs(10);
/// A tick later than this much past its schedule means the machine stalled.
const STALL: Duration = Duration::from_secs(3);
/// Heartbeats are never expected faster than this, whatever the app claims.
const MIN_INTERVAL: Duration = Duration::from_millis(250);
const DEFAULT_INTERVAL: Duration = Duration::from_secs(2);

struct Beat {
    last: Instant,
    interval: Duration,
    pid: u32,
    frozen: bool,
}

#[derive(Default)]
struct Inner {
    apps: HashMap<String, Beat>,
    grace_until: Option<Instant>,
}

/// What the watchdog decided on one check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// The app went silent while its process is alive.
    Freeze {
        app: String,
        pid: u32,
        silent_for: Duration,
    },
    /// The process is gone; it is no longer tracked.
    Gone { app: String },
}

pub struct Watchdog {
    freeze_after: Duration,
    inner: Mutex<Inner>,
}

impl Watchdog {
    pub fn new(freeze_after: Duration) -> Self {
        Self {
            freeze_after,
            inner: Mutex::new(Inner::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Record a heartbeat. Returns `true` when the app had been marked
    /// frozen and has now recovered.
    pub fn heartbeat(&self, app: &str, pid: u32, interval: Option<Duration>, now: Instant) -> bool {
        let interval = interval.unwrap_or(DEFAULT_INTERVAL).max(MIN_INTERVAL);
        let mut inner = self.lock();
        match inner.apps.get_mut(app) {
            Some(beat) => {
                let recovered = beat.frozen;
                beat.last = now;
                beat.interval = interval;
                beat.pid = pid;
                beat.frozen = false;
                recovered
            }
            None => {
                inner.apps.insert(
                    app.to_string(),
                    Beat {
                        last: now,
                        interval,
                        pid,
                        frozen: false,
                    },
                );
                false
            }
        }
    }

    /// The app announced a clean exit; stop watching it.
    pub fn stopping(&self, app: &str) {
        self.lock().apps.remove(app);
    }

    /// Pause detection until `now + GRACE` (resume, monitor hotplug, ...).
    pub fn grace(&self, now: Instant) {
        let mut inner = self.lock();
        let until = now + GRACE;
        inner.grace_until = Some(inner.grace_until.map_or(until, |g| g.max(until)));
    }

    /// Whether `app` is currently marked frozen.
    #[cfg(test)]
    pub fn is_frozen(&self, app: &str) -> bool {
        self.lock().apps.get(app).is_some_and(|b| b.frozen)
    }

    /// Evaluate all tracked apps. `alive(pid)` says whether the process still
    /// exists. A stalled tick (`since_last_tick` well above the schedule)
    /// starts a grace window instead of reporting anything.
    pub fn check(
        &self,
        now: Instant,
        since_last_tick: Duration,
        expected_tick: Duration,
        alive: impl Fn(u32) -> bool,
    ) -> Vec<Action> {
        if since_last_tick > expected_tick + STALL {
            self.grace(now);
        }
        let mut inner = self.lock();
        if inner.grace_until.is_some_and(|g| now < g) {
            // Silence during grace is not evidence of anything: forgive it.
            for beat in inner.apps.values_mut() {
                if beat.last + GRACE < now {
                    beat.last = now;
                }
            }
            return Vec::new();
        }
        let mut actions = Vec::new();
        let mut gone = Vec::new();
        for (app, beat) in inner.apps.iter_mut() {
            let limit = self.freeze_after.max(beat.interval * 4);
            let silent = now.saturating_duration_since(beat.last);
            if silent <= limit || beat.frozen {
                continue;
            }
            if alive(beat.pid) {
                beat.frozen = true;
                actions.push(Action::Freeze {
                    app: app.clone(),
                    pid: beat.pid,
                    silent_for: silent,
                });
            } else {
                gone.push(app.clone());
            }
        }
        for app in gone {
            inner.apps.remove(&app);
            actions.push(Action::Gone { app });
        }
        actions.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
        actions
    }
}

/// Whether `pid` exists and is not a zombie.
pub fn process_alive(pid: u32) -> bool {
    process_alive_in(Path::new("/proc"), pid)
}

fn process_alive_in(proc_root: &Path, pid: u32) -> bool {
    let Ok(stat) = std::fs::read_to_string(proc_root.join(pid.to_string()).join("stat")) else {
        return false;
    };
    // The state is the first field after the parenthesised command name,
    // which itself may contain spaces and parentheses.
    stat.rsplit_once(')')
        .and_then(|(_, rest)| rest.split_whitespace().next().map(|s| s != "Z" && s != "X"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TICK: Duration = Duration::from_secs(1);

    fn wd() -> (Watchdog, Instant) {
        (Watchdog::new(Duration::from_secs(15)), Instant::now())
    }

    fn at(base: Instant, secs: u64) -> Instant {
        base + Duration::from_secs(secs)
    }

    #[test]
    fn healthy_app_is_never_flagged() {
        let (w, t0) = wd();
        for s in 0..60 {
            w.heartbeat("clip", 10, None, at(t0, s));
            assert!(w.check(at(t0, s), TICK, TICK, |_| true).is_empty());
        }
    }

    #[test]
    fn silence_past_the_limit_is_a_freeze_once() {
        let (w, t0) = wd();
        w.heartbeat("clip", 10, None, t0);
        assert!(w.check(at(t0, 15), TICK, TICK, |_| true).is_empty());
        let a = w.check(at(t0, 16), TICK, TICK, |_| true);
        assert_eq!(a.len(), 1);
        assert!(matches!(&a[0], Action::Freeze { app, pid: 10, .. } if app == "clip"));
        assert!(w.is_frozen("clip"));
        // Already reported: no repeat.
        assert!(w.check(at(t0, 30), TICK, TICK, |_| true).is_empty());
    }

    #[test]
    fn heartbeat_after_freeze_reports_recovery() {
        let (w, t0) = wd();
        w.heartbeat("clip", 10, None, t0);
        w.check(at(t0, 20), TICK, TICK, |_| true);
        assert!(w.heartbeat("clip", 10, None, at(t0, 21)));
        assert!(!w.is_frozen("clip"));
        assert!(!w.heartbeat("clip", 10, None, at(t0, 22)));
    }

    #[test]
    fn slow_interval_apps_get_four_intervals() {
        let (w, t0) = wd();
        w.heartbeat("slow", 11, Some(Duration::from_secs(10)), t0);
        assert!(w.check(at(t0, 39), TICK, TICK, |_| true).is_empty());
        assert_eq!(w.check(at(t0, 41), TICK, TICK, |_| true).len(), 1);
    }

    #[test]
    fn dead_process_is_dropped_not_frozen() {
        let (w, t0) = wd();
        w.heartbeat("clip", 10, None, t0);
        let a = w.check(at(t0, 30), TICK, TICK, |_| false);
        assert_eq!(a, vec![Action::Gone { app: "clip".into() }]);
        assert!(!w.is_frozen("clip"));
        assert!(w.check(at(t0, 60), TICK, TICK, |_| false).is_empty());
    }

    #[test]
    fn clean_stop_is_forgotten() {
        let (w, t0) = wd();
        w.heartbeat("clip", 10, None, t0);
        w.stopping("clip");
        assert!(w.check(at(t0, 60), TICK, TICK, |_| true).is_empty());
    }

    #[test]
    fn stalled_tick_starts_grace_and_forgives_silence() {
        let (w, t0) = wd();
        w.heartbeat("clip", 10, None, t0);
        // The machine was suspended: the next tick arrives 40s late.
        let a = w.check(at(t0, 40), Duration::from_secs(40), TICK, |_| true);
        assert!(a.is_empty());
        // Still inside the grace window shortly after.
        assert!(w.check(at(t0, 45), TICK, TICK, |_| true).is_empty());
        // After grace the app must prove itself again; it gets a fresh clock.
        assert!(w.check(at(t0, 52), TICK, TICK, |_| true).is_empty());
        assert_eq!(w.check(at(t0, 70), TICK, TICK, |_| true).len(), 1);
    }

    #[test]
    fn explicit_grace_for_hotplug_pauses_detection() {
        let (w, t0) = wd();
        w.heartbeat("clip", 10, None, t0);
        w.grace(at(t0, 20));
        assert!(w.check(at(t0, 25), TICK, TICK, |_| true).is_empty());
    }

    #[test]
    fn apps_are_tracked_independently() {
        let (w, t0) = wd();
        w.heartbeat("clip", 10, None, t0);
        w.heartbeat("pad", 11, None, t0);
        w.heartbeat("pad", 11, None, at(t0, 18));
        let a = w.check(at(t0, 20), TICK, TICK, |_| true);
        assert_eq!(a.len(), 1);
        assert!(matches!(&a[0], Action::Freeze { app, .. } if app == "clip"));
    }

    #[test]
    fn interval_is_floored() {
        let (w, t0) = wd();
        w.heartbeat("clip", 10, Some(Duration::from_millis(1)), t0);
        // limit = max(15s, 4 * 250ms) = 15s
        assert!(w.check(at(t0, 14), TICK, TICK, |_| true).is_empty());
    }

    #[test]
    fn liveness_reads_proc_state() {
        let dir = tempfile::tempdir().unwrap();
        let proc = dir.path();
        std::fs::create_dir_all(proc.join("10")).unwrap();
        std::fs::write(proc.join("10/stat"), "10 (my (weird) app) S 1 2 3").unwrap();
        std::fs::create_dir_all(proc.join("11")).unwrap();
        std::fs::write(proc.join("11/stat"), "11 (z) Z 1 2 3").unwrap();
        assert!(process_alive_in(proc, 10));
        assert!(!process_alive_in(proc, 11));
        assert!(!process_alive_in(proc, 12));
        assert!(process_alive(std::process::id()));
    }
}
