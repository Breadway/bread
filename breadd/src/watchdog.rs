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
//! * dropping apps whose process has exited instead of calling them frozen,
//!   including ones that were already frozen when they died;
//! * tracking the process *instance* (pid plus kernel start time), so a
//!   restarted app or a reused pid is never taken for the one that froze.
//!
//! Nothing here kills or signals a process.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use bread_bug::procid::{self, Change};
use bread_bug::store::ProcessIdentity;

/// How long detection is paused after a stalled tick or a power/monitor event.
pub const GRACE: Duration = Duration::from_secs(10);
/// A tick later than this much past its schedule means the machine stalled.
const STALL: Duration = Duration::from_secs(3);
/// Heartbeats are never expected faster than this, whatever the app claims.
const MIN_INTERVAL: Duration = Duration::from_millis(250);
const DEFAULT_INTERVAL: Duration = Duration::from_secs(2);

/// One process instance of an app. A pid alone is not an identity (pids are
/// reused), so the kernel start time and executable travel with it when they
/// could be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instance {
    pub pid: u32,
    pub identity: Option<ProcessIdentity>,
}

impl Instance {
    /// Whether `other` is the same process instance. Same pid is required;
    /// when both identities are known they must match too (that is what
    /// tells a restarted app, or a reused pid, from the original).
    fn is_same(&self, other: &Instance) -> bool {
        self.pid == other.pid
            && match (&self.identity, &other.identity) {
                (Some(a), Some(b)) => procid::same_instance(a, b),
                _ => true,
            }
    }
}

/// What a liveness probe found for a tracked instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    Alive,
    /// The process exited (or is a zombie).
    Gone,
    /// The pid now belongs to a different process.
    Replaced,
}

struct Beat {
    last: Instant,
    interval: Duration,
    instance: Instance,
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
        instance: Instance,
        silent_for: Duration,
    },
    /// The tracked process is gone (exited, or its pid was taken by another
    /// process) and is no longer tracked. `was_frozen` says whether a freeze
    /// had been reported for it, in which case it must be closed in the store.
    Gone {
        app: String,
        instance: Instance,
        was_frozen: bool,
    },
}

/// A tracked instance that ended or was superseded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retired {
    pub instance: Instance,
    pub was_frozen: bool,
}

/// What one heartbeat did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeartbeatOutcome {
    /// The same instance that was marked frozen is responding again.
    pub recovered: bool,
    /// A different instance of the app took over; the old one is retired.
    /// Never reported as a recovery of the old one.
    pub retired: Option<Retired>,
    /// The app is on the exclusion list and was not tracked.
    pub excluded: bool,
}

/// What a clean-stop announcement did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopOutcome {
    /// The tracked instance announced its own exit and is no longer tracked.
    Stopped(Retired),
    /// Nothing was tracked for the app.
    NotTracked,
    /// A different instance is tracked: an old instance cannot untrack it.
    OtherInstance,
    Excluded,
}

pub struct Watchdog {
    freeze_after: Duration,
    excluded: Mutex<HashSet<String>>,
    inner: Mutex<Inner>,
}

impl Watchdog {
    #[cfg(test)]
    pub fn new(freeze_after: Duration) -> Self {
        Self::with_excluded(freeze_after, &[])
    }

    /// `excluded` apps are never tracked: the opt-out holds before anything
    /// is remembered, marked or published about them.
    pub fn with_excluded(freeze_after: Duration, excluded: &[String]) -> Self {
        Self {
            freeze_after,
            excluded: Mutex::new(excluded.iter().cloned().collect()),
            inner: Mutex::new(Inner::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn is_excluded(&self, app: &str) -> bool {
        self.excluded
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains(app)
    }

    /// Replace the exclusion list at runtime.
    ///
    /// Newly excluded apps stop being tracked immediately, so a pending or
    /// future freeze cannot be reported and no stale state survives until the
    /// process exits. Removing an exclusion does not start tracking: the next
    /// heartbeat does.
    pub fn set_excluded(&self, excluded: &[String]) {
        let next: HashSet<String> = excluded.iter().cloned().collect();
        // Same order as heartbeat/stopping: excluded, then inner. Holding the
        // exclusion lock across the insert/remove makes the check and the
        // track decision atomic against a concurrent reload.
        let mut guard = self.excluded.lock().unwrap_or_else(|p| p.into_inner());
        *guard = next;
        let mut inner = self.lock();
        inner.apps.retain(|app, _| !guard.contains(app));
    }

    /// Record a heartbeat from `instance`.
    ///
    /// The same instance that had been frozen is `recovered`. A heartbeat from
    /// a *different* instance (a restart, or a reused pid) retires the old one
    /// and starts the new one fresh: it is never a recovery of the old one.
    pub fn heartbeat(
        &self,
        app: &str,
        instance: Instance,
        interval: Option<Duration>,
        now: Instant,
    ) -> HeartbeatOutcome {
        let excluded = self.excluded.lock().unwrap_or_else(|p| p.into_inner());
        if excluded.contains(app) {
            return HeartbeatOutcome {
                excluded: true,
                ..HeartbeatOutcome::default()
            };
        }
        let interval = interval.unwrap_or(DEFAULT_INTERVAL).max(MIN_INTERVAL);
        let mut inner = self.lock();
        let mut outcome = HeartbeatOutcome::default();
        if let Some(beat) = inner.apps.get_mut(app) {
            if beat.instance.is_same(&instance) {
                outcome.recovered = beat.frozen;
                beat.last = now;
                beat.interval = interval;
                beat.frozen = false;
                // Keep the richer identity if one was unreadable before.
                if beat.instance.identity.is_none() {
                    beat.instance.identity = instance.identity;
                }
                return outcome;
            }
            outcome.retired = Some(Retired {
                instance: beat.instance.clone(),
                was_frozen: beat.frozen,
            });
        }
        inner.apps.insert(
            app.to_string(),
            Beat {
                last: now,
                interval,
                instance,
                frozen: false,
            },
        );
        outcome
    }

    /// `instance` announced a clean exit. Only the tracked instance can stop
    /// tracking; a late message from an older one is ignored.
    pub fn stopping(&self, app: &str, instance: &Instance) -> StopOutcome {
        let excluded = self.excluded.lock().unwrap_or_else(|p| p.into_inner());
        if excluded.contains(app) {
            return StopOutcome::Excluded;
        }
        let mut inner = self.lock();
        match inner.apps.get(app) {
            None => StopOutcome::NotTracked,
            Some(beat) if !beat.instance.is_same(instance) => StopOutcome::OtherInstance,
            Some(_) => {
                let beat = inner.apps.remove(app).expect("checked above");
                StopOutcome::Stopped(Retired {
                    instance: beat.instance,
                    was_frozen: beat.frozen,
                })
            }
        }
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

    /// Whether `app` is tracked at all.
    #[cfg(test)]
    pub fn is_tracked(&self, app: &str) -> bool {
        self.lock().apps.contains_key(app)
    }

    /// Evaluate all tracked apps. `probe` says whether a tracked instance is
    /// still the same live process. A stalled tick (`since_last_tick` well
    /// above the schedule) starts a grace window instead of reporting
    /// anything.
    ///
    /// Liveness is checked for every silent instance and for every frozen
    /// one, so a frozen process that dies is retired (and reported as
    /// `Gone`) instead of staying "frozen" forever.
    pub fn check(
        &self,
        now: Instant,
        since_last_tick: Duration,
        expected_tick: Duration,
        probe: impl Fn(&Instance) -> Probe,
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
            if silent <= limit && !beat.frozen {
                continue;
            }
            match probe(&beat.instance) {
                Probe::Alive if beat.frozen => {}
                Probe::Alive => {
                    beat.frozen = true;
                    actions.push(Action::Freeze {
                        app: app.clone(),
                        instance: beat.instance.clone(),
                        silent_for: silent,
                    });
                }
                Probe::Gone | Probe::Replaced => gone.push(app.clone()),
            }
        }
        for app in gone {
            if let Some(beat) = inner.apps.remove(&app) {
                actions.push(Action::Gone {
                    app,
                    instance: beat.instance,
                    was_frozen: beat.frozen,
                });
            }
        }
        actions.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
        actions
    }
}

/// The instance behind `pid` right now, as far as `/proc` tells.
pub fn instance_of(pid: u32) -> Instance {
    Instance {
        pid,
        identity: procid::identity_of(pid),
    }
}

/// Probe the real `/proc`. An instance with no recorded identity can only be
/// checked by pid.
pub fn probe(instance: &Instance) -> Probe {
    probe_in(std::path::Path::new("/proc"), instance)
}

fn probe_in(proc_root: &std::path::Path, instance: &Instance) -> Probe {
    match &instance.identity {
        Some(id) => match procid::verify_in(proc_root, instance.pid, id) {
            Ok(()) => Probe::Alive,
            Err(Change::Gone) => Probe::Gone,
            Err(Change::Replaced) => Probe::Replaced,
        },
        None => match procid::read_stat(proc_root, instance.pid) {
            Some(stat) if stat.is_running() => Probe::Alive,
            _ => Probe::Gone,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bread_bug::procid::testutil::fake_proc;

    const TICK: Duration = Duration::from_secs(1);

    fn wd() -> (Watchdog, Instant) {
        (
            Watchdog::with_excluded(Duration::from_secs(15), &[]),
            Instant::now(),
        )
    }

    fn at(base: Instant, secs: u64) -> Instant {
        base + Duration::from_secs(secs)
    }

    /// An instance with a known start time, so same-pid replacement is visible.
    fn inst(pid: u32, start_ticks: u64) -> Instance {
        Instance {
            pid,
            identity: Some(ProcessIdentity {
                start_ticks,
                exe: "/usr/bin/breadclip".into(),
                boot_id: None,
            }),
        }
    }

    fn hb(w: &Watchdog, app: &str, i: &Instance, t: Instant) -> HeartbeatOutcome {
        w.heartbeat(app, i.clone(), None, t)
    }

    fn alive(_: &Instance) -> Probe {
        Probe::Alive
    }

    fn gone(_: &Instance) -> Probe {
        Probe::Gone
    }

    #[test]
    fn healthy_app_is_never_flagged() {
        let (w, t0) = wd();
        let i = inst(10, 1);
        for s in 0..60 {
            hb(&w, "clip", &i, at(t0, s));
            assert!(w.check(at(t0, s), TICK, TICK, alive).is_empty());
        }
    }

    #[test]
    fn silence_past_the_limit_is_a_freeze_once() {
        let (w, t0) = wd();
        let i = inst(10, 1);
        hb(&w, "clip", &i, t0);
        assert!(w.check(at(t0, 15), TICK, TICK, alive).is_empty());
        let a = w.check(at(t0, 16), TICK, TICK, alive);
        assert_eq!(a.len(), 1);
        assert!(
            matches!(&a[0], Action::Freeze { app, instance, .. } if app == "clip" && instance.pid == 10)
        );
        assert!(w.is_frozen("clip"));
        // Already reported: no repeat.
        assert!(w.check(at(t0, 30), TICK, TICK, alive).is_empty());
    }

    #[test]
    fn heartbeat_from_the_same_instance_after_freeze_reports_recovery() {
        let (w, t0) = wd();
        let i = inst(10, 1);
        hb(&w, "clip", &i, t0);
        w.check(at(t0, 20), TICK, TICK, alive);
        let out = hb(&w, "clip", &i, at(t0, 21));
        assert!(out.recovered);
        assert_eq!(out.retired, None);
        assert!(!w.is_frozen("clip"));
        assert!(!hb(&w, "clip", &i, at(t0, 22)).recovered);
    }

    #[test]
    fn slow_interval_apps_get_four_intervals() {
        let (w, t0) = wd();
        w.heartbeat("slow", inst(11, 1), Some(Duration::from_secs(10)), t0);
        assert!(w.check(at(t0, 39), TICK, TICK, alive).is_empty());
        assert_eq!(w.check(at(t0, 41), TICK, TICK, alive).len(), 1);
    }

    #[test]
    fn dead_process_is_dropped_not_frozen() {
        let (w, t0) = wd();
        let i = inst(10, 1);
        hb(&w, "clip", &i, t0);
        let a = w.check(at(t0, 30), TICK, TICK, gone);
        assert_eq!(
            a,
            vec![Action::Gone {
                app: "clip".into(),
                instance: i,
                was_frozen: false
            }]
        );
        assert!(!w.is_frozen("clip"));
        assert!(w.check(at(t0, 60), TICK, TICK, gone).is_empty());
    }

    /// Reproduces the review: PID42 freezes, dies, then PID43 heartbeats.
    /// Before the fix there was no Gone event and PID43 was "recovered".
    #[test]
    fn frozen_instance_that_dies_is_retired_with_a_gone_event() {
        let (w, t0) = wd();
        let old = inst(42, 1);
        hb(&w, "clip", &old, t0);
        assert_eq!(w.check(at(t0, 20), TICK, TICK, alive).len(), 1);
        assert!(w.is_frozen("clip"));

        // The next tick finds it dead, even though it is already frozen.
        let a = w.check(at(t0, 21), TICK, TICK, gone);
        assert_eq!(
            a,
            vec![Action::Gone {
                app: "clip".into(),
                instance: old,
                was_frozen: true
            }]
        );
        assert!(!w.is_tracked("clip"));

        // A new instance is a fresh start, never a recovery.
        let out = hb(&w, "clip", &inst(43, 2), at(t0, 22));
        assert!(!out.recovered);
        assert_eq!(out.retired, None);
        assert!(!w.is_frozen("clip"));
    }

    #[test]
    fn new_instance_heartbeat_before_the_tick_retires_the_old_one_without_recovery() {
        let (w, t0) = wd();
        hb(&w, "clip", &inst(42, 1), t0);
        w.check(at(t0, 20), TICK, TICK, alive);
        assert!(w.is_frozen("clip"));

        // PID43 beats before the watchdog noticed that 42 died.
        let out = hb(&w, "clip", &inst(43, 2), at(t0, 20));
        assert!(!out.recovered, "a different instance is not a recovery");
        let retired = out.retired.expect("old instance retired");
        assert_eq!(retired.instance.pid, 42);
        assert!(retired.was_frozen);
        assert!(!w.is_frozen("clip"));
    }

    #[test]
    fn reused_pid_with_a_new_start_time_is_a_new_instance() {
        let (w, t0) = wd();
        hb(&w, "clip", &inst(42, 100), t0);
        w.check(at(t0, 20), TICK, TICK, alive);
        // Same pid, but the kernel says it started later: a different process.
        let out = hb(&w, "clip", &inst(42, 999), at(t0, 21));
        assert!(!out.recovered);
        assert!(out
            .retired
            .is_some_and(|r| r.was_frozen && r.instance.pid == 42));
    }

    #[test]
    fn a_replaced_pid_is_retired_by_the_tick() {
        let (w, t0) = wd();
        let i = inst(42, 100);
        hb(&w, "clip", &i, t0);
        w.check(at(t0, 20), TICK, TICK, alive);
        let a = w.check(at(t0, 21), TICK, TICK, |_| Probe::Replaced);
        assert!(matches!(
            &a[..],
            [Action::Gone {
                was_frozen: true,
                ..
            }]
        ));
    }

    #[test]
    fn clean_stop_by_the_tracked_instance_is_forgotten() {
        let (w, t0) = wd();
        let i = inst(10, 1);
        hb(&w, "clip", &i, t0);
        assert!(matches!(
            w.stopping("clip", &i),
            StopOutcome::Stopped(Retired {
                was_frozen: false,
                ..
            })
        ));
        assert!(w.check(at(t0, 60), TICK, TICK, alive).is_empty());
        assert_eq!(w.stopping("clip", &i), StopOutcome::NotTracked);
    }

    #[test]
    fn an_old_instance_cannot_stop_a_new_one() {
        let (w, t0) = wd();
        hb(&w, "clip", &inst(43, 2), t0);
        // A late "stopping" from the previous process (pid 42, or pid 43's
        // predecessor with a different start time).
        assert_eq!(w.stopping("clip", &inst(42, 1)), StopOutcome::OtherInstance);
        assert_eq!(w.stopping("clip", &inst(43, 1)), StopOutcome::OtherInstance);
        assert!(w.is_tracked("clip"));
        assert_eq!(w.check(at(t0, 30), TICK, TICK, alive).len(), 1);
    }

    #[test]
    fn stopping_a_frozen_instance_reports_it_was_frozen() {
        let (w, t0) = wd();
        let i = inst(10, 1);
        hb(&w, "clip", &i, t0);
        w.check(at(t0, 20), TICK, TICK, alive);
        assert!(matches!(
            w.stopping("clip", &i),
            StopOutcome::Stopped(Retired {
                was_frozen: true,
                ..
            })
        ));
    }

    #[test]
    fn excluded_apps_are_never_tracked() {
        let w = Watchdog::with_excluded(Duration::from_secs(15), &["clip".to_string()]);
        let t0 = Instant::now();
        let out = w.heartbeat("clip", inst(10, 1), None, t0);
        assert!(out.excluded);
        assert!(!w.is_tracked("clip"));
        assert!(w.check(at(t0, 60), TICK, TICK, alive).is_empty());
        assert_eq!(w.stopping("clip", &inst(10, 1)), StopOutcome::Excluded);
        // Other apps are unaffected.
        assert!(!w.heartbeat("pad", inst(11, 1), None, t0).excluded);
        assert!(w.is_tracked("pad"));
    }

    #[test]
    fn runtime_exclusion_drops_a_tracked_app_and_blocks_new_heartbeats() {
        let (w, t0) = wd();
        hb(&w, "clip", &inst(10, 1), t0);
        assert!(w.is_tracked("clip"));
        w.set_excluded(&["clip".to_string()]);
        assert!(w.is_excluded("clip"));
        assert!(!w.is_tracked("clip"));
        assert!(w.check(at(t0, 60), TICK, TICK, alive).is_empty());
        let out = w.heartbeat("clip", inst(10, 1), None, at(t0, 61));
        assert!(out.excluded);
        assert!(!w.is_tracked("clip"));
        // Removing the exclusion does not resurrect stale tracking: the next
        // heartbeat starts a fresh instance instead of "recovering" one.
        w.set_excluded(&[]);
        assert!(!w.is_tracked("clip"));
        let out = w.heartbeat("clip", inst(10, 1), None, at(t0, 62));
        assert!(!out.excluded && !out.recovered);
        assert!(w.is_tracked("clip"));
    }

    #[test]
    fn stalled_tick_starts_grace_and_forgives_silence() {
        let (w, t0) = wd();
        hb(&w, "clip", &inst(10, 1), t0);
        // The machine was suspended: the next tick arrives 40s late.
        let a = w.check(at(t0, 40), Duration::from_secs(40), TICK, alive);
        assert!(a.is_empty());
        // Still inside the grace window shortly after.
        assert!(w.check(at(t0, 45), TICK, TICK, alive).is_empty());
        // After grace the app must prove itself again; it gets a fresh clock.
        assert!(w.check(at(t0, 52), TICK, TICK, alive).is_empty());
        assert_eq!(w.check(at(t0, 70), TICK, TICK, alive).len(), 1);
    }

    #[test]
    fn explicit_grace_for_hotplug_pauses_detection() {
        let (w, t0) = wd();
        hb(&w, "clip", &inst(10, 1), t0);
        w.grace(at(t0, 20));
        assert!(w.check(at(t0, 25), TICK, TICK, alive).is_empty());
    }

    #[test]
    fn apps_are_tracked_independently() {
        let (w, t0) = wd();
        hb(&w, "clip", &inst(10, 1), t0);
        hb(&w, "pad", &inst(11, 1), t0);
        hb(&w, "pad", &inst(11, 1), at(t0, 18));
        let a = w.check(at(t0, 20), TICK, TICK, alive);
        assert_eq!(a.len(), 1);
        assert!(matches!(&a[0], Action::Freeze { app, .. } if app == "clip"));
    }

    #[test]
    fn interval_is_floored() {
        let (w, t0) = wd();
        w.heartbeat("clip", inst(10, 1), Some(Duration::from_millis(1)), t0);
        // limit = max(15s, 4 * 250ms) = 15s
        assert!(w.check(at(t0, 14), TICK, TICK, alive).is_empty());
    }

    #[test]
    fn a_heartbeat_with_no_readable_identity_matches_by_pid_then_learns_it() {
        let (w, t0) = wd();
        let bare = Instance {
            pid: 10,
            identity: None,
        };
        hb(&w, "clip", &bare, t0);
        // Same pid, now with an identity: the same instance, not a restart.
        let out = hb(&w, "clip", &inst(10, 1), at(t0, 1));
        assert_eq!(out, HeartbeatOutcome::default());
        // The learned identity now distinguishes a later replacement.
        let out = hb(&w, "clip", &inst(10, 2), at(t0, 2));
        assert!(out.retired.is_some());
    }

    #[test]
    fn probe_reads_proc_identity_and_state() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fake_proc(root, 10, "breadclip", 'S', 77, "/usr/bin/breadclip");
        let id = bread_bug::procid::identity_in(root, 10).unwrap();
        let i = Instance {
            pid: 10,
            identity: Some(id),
        };
        assert_eq!(probe_in(root, &i), Probe::Alive);

        fake_proc(root, 10, "breadclip", 'S', 78, "/usr/bin/breadclip");
        assert_eq!(probe_in(root, &i), Probe::Replaced);

        fake_proc(root, 10, "breadclip", 'Z', 77, "/usr/bin/breadclip");
        assert_eq!(probe_in(root, &i), Probe::Gone);
        assert_eq!(probe_in(root, &inst(11, 1)), Probe::Gone);

        // Without a recorded identity only liveness can be checked.
        let bare = Instance {
            pid: 10,
            identity: None,
        };
        fake_proc(root, 10, "x", 'S', 1, "/usr/bin/breadclip");
        assert_eq!(probe_in(root, &bare), Probe::Alive);
        fake_proc(root, 10, "x", 'Z', 1, "/usr/bin/breadclip");
        assert_eq!(probe_in(root, &bare), Probe::Gone);
    }

    #[test]
    fn the_real_proc_sees_this_process_as_alive_with_an_identity() {
        let me = instance_of(std::process::id());
        assert!(me.identity.is_some());
        assert_eq!(probe(&me), Probe::Alive);
    }
}
