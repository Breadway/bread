//! The on-disk log/event/marker store.
//!
//! breadd owns the single writer ([`Store`]); the `breadbug` binary reads
//! the same files through [`Reader`], so reports can be built even when
//! breadd is down or has just crashed. Everything is scrubbed on the way in
//! (see `bread-scrub`), so raw personal data never reaches disk.
//!
//! Layout under the store root:
//!
//! ```text
//! apps/<app>/log.jsonl      recent log lines          (+ log.jsonl.1)
//! apps/<app>/events.jsonl   recent event names/shapes (+ events.jsonl.1)
//! apps/<app>/markers.jsonl  crash/freeze markers      (+ markers.jsonl.1)
//! ```
//!
//! Each file is capped; at the cap it rotates to `<file>.1`, replacing the
//! previous rotation, so an app never holds more than two generations per
//! file. A global cap drops the oldest data first. Directories are 0700 and
//! files 0600.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use anyhow::{bail, Context, Result};
use bread_scrub::{EventAllowlist, ScrubContext, Scrubber};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{BugConfig, StoreLimits};

const LOG_FILE: &str = "log.jsonl";
const EVENT_FILE: &str = "events.jsonl";
const MARKER_FILE: &str = "markers.jsonl";
/// Most markers kept in a snapshot; they are small but not unbounded.
const MAX_MARKERS_IN_SNAPSHOT: usize = 32;
/// Most frames kept on one marker.
const MAX_FRAMES: usize = 128;

/// Pseudo-app id for daemon logs and events that belong to no sibling app.
pub const DAEMON_APP: &str = "breadd";

/// One stored log line.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogRecord {
    pub t: u64,
    pub level: String,
    pub msg: String,
}

/// One stored event: its name and the allowlisted, scrubbed payload fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EventRecord {
    pub t: u64,
    pub name: String,
    #[serde(default)]
    pub data: Value,
}

/// What a marker records.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MarkerKind {
    /// A Rust panic caught by the in-app hook.
    Panic,
    /// A fatal signal (segfault, abort) from native code.
    Crash,
    /// The app stopped sending heartbeats.
    Freeze,
    /// A frozen app started responding again.
    Recovered,
}

/// A stack frame. Never carries memory contents: module, offset in the
/// module, the module's build-id and (optionally) a symbol name.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Frame {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
}

/// A crash or freeze marker.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Marker {
    pub t: u64,
    pub kind: MarkerKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frames: Vec<Frame>,
}

impl Marker {
    pub fn new(kind: MarkerKind, t: u64) -> Self {
        Self {
            t,
            kind,
            app_version: None,
            build_id: None,
            message: None,
            location: None,
            signal: None,
            frames: Vec::new(),
        }
    }
}

/// Everything stored for one app, oldest first.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AppSnapshot {
    pub app: String,
    pub logs: Vec<LogRecord>,
    pub events: Vec<EventRecord>,
    pub markers: Vec<Marker>,
}

/// Disk usage summary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Usage {
    pub total_bytes: u64,
    pub per_app: Vec<(String, u64)>,
}

/// App ids are short lowercase identifiers; this is also the path-traversal
/// guard, since the id becomes a directory name.
pub fn valid_app_id(app: &str) -> bool {
    let mut chars = app.chars();
    app.len() <= 32
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// The event fields that are safe to keep. Everything else is dropped by the
/// scrubber, leaving only the event name and plain key names.
pub fn default_event_allowlist() -> EventAllowlist {
    EventAllowlist::new()
        .allow("bread.power.**", &["ac_connected", "battery_percent"])
        .allow("bread.network.*", &["online"])
        .allow("bread.device.*", &["subsystem", "vendor_id", "product_id"])
        .allow("bread.bluetooth.**", &["subsystem"])
}

/// Which store app an event belongs to: `bread.<app>.*` for a known sibling
/// app, otherwise the daemon pseudo-app.
pub fn app_for_event(event: &str, known_apps: &[&str]) -> String {
    let mut segs = event.split('.');
    if segs.next() == Some("bread") {
        if let Some(seg) = segs.next() {
            if known_apps.contains(&seg) {
                return seg.to_string();
            }
        }
    }
    DAEMON_APP.to_string()
}

fn normalize_level(level: &str) -> &'static str {
    match level.to_ascii_uppercase().as_str() {
        "TRACE" => "TRACE",
        "DEBUG" => "DEBUG",
        "WARN" | "WARNING" => "WARN",
        "ERROR" | "ERR" | "FATAL" => "ERROR",
        _ => "INFO",
    }
}

fn truncate_to(s: &mut String, max: usize) {
    if s.len() > max {
        let mut cut = max;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
}

fn is_build_id(s: &str) -> bool {
    (8..=128).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

struct Inner {
    limits: StoreLimits,
    excluded: HashSet<String>,
    scrubber: Scrubber,
    allow: EventAllowlist,
    sizes: HashMap<PathBuf, u64>,
}

/// The single writer. Cheap to share behind an `Arc`.
pub struct Store {
    root: PathBuf,
    inner: Mutex<Inner>,
}

impl Store {
    /// Open (creating if needed) a store rooted at `root`.
    pub fn open(root: impl Into<PathBuf>, cfg: &BugConfig, ctx: ScrubContext) -> Result<Self> {
        let root = root.into();
        make_private_dir(&root.join("apps"))?;
        let store = Self {
            root,
            inner: Mutex::new(Inner {
                limits: cfg.store.clone(),
                excluded: cfg.excluded_apps.iter().cloned().collect(),
                scrubber: Scrubber::new(ctx),
                allow: default_event_allowlist(),
                sizes: HashMap::new(),
            }),
        };
        // Apps excluded since the last run must not leave data behind.
        for app in &cfg.excluded_apps {
            if valid_app_id(app) {
                store.wipe_app(app)?;
            }
        }
        // A new daemon must not reuse numbers still present in retained
        // records from an earlier ingest scope. Include rotations and all
        // apps because a report can combine app and daemon records.
        store.reserve_retained_markers()?;
        Ok(store)
    }

    fn reserve_retained_markers(&self) -> Result<()> {
        let mut inner = self.lock();
        for app in fs::read_dir(self.root.join("apps"))? {
            let app = app?;
            if !app.file_type()?.is_dir() {
                continue;
            }
            for file in [LOG_FILE, EVENT_FILE, MARKER_FILE] {
                for name in [file.to_string(), format!("{file}.1")] {
                    let path = app.path().join(name);
                    let f = match fs::File::open(&path) {
                        Ok(f) => f,
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(e) => {
                            return Err(e).with_context(|| format!("read {}", path.display()))
                        }
                    };
                    for line in BufReader::new(f).split(b'\n') {
                        let line = line?;
                        if let Ok(value) = serde_json::from_slice::<Value>(&line) {
                            inner.scrubber.reserve_value_markers(&value);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Replace the event allowlist (tests, future config).
    pub fn set_event_allowlist(&self, allow: EventAllowlist) {
        self.lock().allow = allow;
    }

    /// Replace the exclusion list; newly excluded apps are wiped now.
    pub fn set_excluded(&self, apps: &[String]) -> Result<()> {
        let newly: Vec<String> = {
            let mut inner = self.lock();
            let new: HashSet<String> = apps.iter().cloned().collect();
            let newly = new.difference(&inner.excluded).cloned().collect();
            inner.excluded = new;
            newly
        };
        for app in newly {
            if valid_app_id(&app) {
                self.wipe_app(&app)?;
            }
        }
        Ok(())
    }

    pub fn is_excluded(&self, app: &str) -> bool {
        self.lock().excluded.contains(app)
    }

    /// Store one log line. Returns `Ok(false)` if the app is excluded.
    pub fn append_log(&self, app: &str, level: &str, msg: &str, t: u64) -> Result<bool> {
        check_app(app)?;
        let mut inner = self.lock();
        if inner.excluded.contains(app) {
            return Ok(false);
        }
        let mut msg = inner.scrubber.scrub_text(msg);
        truncate_to(&mut msg, inner.limits.max_line_bytes);
        let rec = LogRecord {
            t,
            level: normalize_level(level).to_string(),
            msg,
        };
        self.write_line(&mut inner, app, LOG_FILE, &serde_json::to_string(&rec)?)?;
        Ok(true)
    }

    /// Store one event: its name and the allowlisted fields of `data`.
    pub fn append_event(&self, app: &str, name: &str, data: &Value, t: u64) -> Result<bool> {
        check_app(app)?;
        let mut inner = self.lock();
        if inner.excluded.contains(app) {
            return Ok(false);
        }
        let allow = inner.allow.clone();
        let mut name_s = inner.scrubber.scrub_text(name);
        truncate_to(&mut name_s, 128);
        let data = inner.scrubber.scrub_event_payload(name, data, &allow);
        let rec = EventRecord {
            t,
            name: name_s,
            data,
        };
        self.write_line(&mut inner, app, EVENT_FILE, &serde_json::to_string(&rec)?)?;
        Ok(true)
    }

    /// Store a crash/freeze marker after scrubbing its text and frames.
    pub fn record_marker(&self, app: &str, mut marker: Marker) -> Result<bool> {
        check_app(app)?;
        let mut inner = self.lock();
        if inner.excluded.contains(app) {
            return Ok(false);
        }
        let max = inner.limits.max_line_bytes;
        let keep_ids: Vec<String> = marker
            .build_id
            .iter()
            .chain(marker.frames.iter().filter_map(|f| f.build_id.as_ref()))
            .filter(|b| is_build_id(b))
            .cloned()
            .collect();
        for id in &keep_ids {
            inner.scrubber.keep_build_id(id);
        }
        let scrub = |s: &mut Option<String>, inner: &mut Inner| {
            if let Some(v) = s.as_mut() {
                *v = inner.scrubber.scrub_text(v);
                truncate_to(v, max);
            }
        };
        scrub(&mut marker.message, &mut inner);
        scrub(&mut marker.location, &mut inner);
        scrub(&mut marker.app_version, &mut inner);
        scrub(&mut marker.signal, &mut inner);
        marker.build_id = marker.build_id.filter(|b| is_build_id(b));
        marker.frames.truncate(MAX_FRAMES);
        for f in &mut marker.frames {
            scrub(&mut f.module, &mut inner);
            scrub(&mut f.symbol, &mut inner);
            f.build_id = f.build_id.take().filter(|b| is_build_id(b));
        }
        self.write_line(
            &mut inner,
            app,
            MARKER_FILE,
            &serde_json::to_string(&marker)?,
        )?;
        Ok(true)
    }

    /// Delete everything stored for one app.
    pub fn wipe_app(&self, app: &str) -> Result<()> {
        check_app(app)?;
        let mut inner = self.lock();
        let dir = self.app_dir(app);
        match fs::remove_dir_all(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("remove {}", dir.display())),
        }
        inner.sizes.retain(|p, _| !p.starts_with(&dir));
        Ok(())
    }

    /// Delete the whole store ("delete local data").
    pub fn wipe_all(&self) -> Result<()> {
        let mut inner = self.lock();
        let apps = self.root.join("apps");
        match fs::remove_dir_all(&apps) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("remove {}", apps.display())),
        }
        inner.sizes.clear();
        inner.scrubber.reset();
        make_private_dir(&apps)
    }

    /// Current disk usage.
    pub fn usage(&self) -> Result<Usage> {
        usage_of(&self.root)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn app_dir(&self, app: &str) -> PathBuf {
        self.root.join("apps").join(app)
    }

    fn write_line(&self, inner: &mut Inner, app: &str, file: &str, line: &str) -> Result<()> {
        let dir = self.app_dir(app);
        if !dir.exists() {
            self.make_room_for_new_app(inner)?;
            make_private_dir(&dir)?;
        }
        let path = dir.join(file);
        let cap = inner.limits.max_bytes_per_file;
        let add = line.len() as u64 + 1;
        let size = match inner.sizes.get(&path) {
            Some(s) => *s,
            None => fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
        };
        let mut size = size;
        if size > 0 && size + add > cap {
            let rotated = dir.join(format!("{file}.1"));
            match fs::rename(&path, &rotated) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).with_context(|| format!("rotate {}", path.display())),
            }
            size = 0;
            self.enforce_total(inner)?;
        }
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        f.write_all(line.as_bytes())?;
        f.write_all(b"\n")?;
        inner.sizes.insert(path, size + add);
        Ok(())
    }

    /// Drop the least recently written app directory when at the app cap.
    fn make_room_for_new_app(&self, inner: &mut Inner) -> Result<()> {
        let apps_dir = self.root.join("apps");
        let mut dirs: Vec<(SystemTime, PathBuf)> = fs::read_dir(&apps_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .map(|e| {
                let mtime = newest_mtime(&e.path());
                (mtime, e.path())
            })
            .collect();
        dirs.sort();
        while dirs.len() >= inner.limits.max_apps && !dirs.is_empty() {
            let (_, oldest) = dirs.remove(0);
            let _ = fs::remove_dir_all(&oldest);
            inner.sizes.retain(|p, _| !p.starts_with(&oldest));
        }
        Ok(())
    }

    /// Keep the whole store under `max_total_bytes`, oldest data first.
    fn enforce_total(&self, inner: &mut Inner) -> Result<()> {
        let mut files = Vec::new();
        let mut total = 0u64;
        for app in fs::read_dir(self.root.join("apps"))?.filter_map(|e| e.ok()) {
            for f in fs::read_dir(app.path()).into_iter().flatten().flatten() {
                let Ok(meta) = f.metadata() else { continue };
                let rotated = f.file_name().to_string_lossy().ends_with(".1");
                let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                total += meta.len();
                files.push((!rotated, mtime, f.path(), meta.len()));
            }
        }
        // Rotated generations go before live files; older before newer.
        files.sort();
        for (_, _, path, len) in files {
            if total <= inner.limits.max_total_bytes {
                break;
            }
            if fs::remove_file(&path).is_ok() {
                total = total.saturating_sub(len);
                inner.sizes.remove(&path);
            }
        }
        Ok(())
    }
}

fn check_app(app: &str) -> Result<()> {
    if !valid_app_id(app) {
        bail!("invalid app id {app:?}");
    }
    Ok(())
}

fn make_private_dir(dir: &Path) -> Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("create {}", dir.display()))
}

fn newest_mtime(dir: &Path) -> SystemTime {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.metadata().ok()?.modified().ok())
        .max()
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

fn usage_of(root: &Path) -> Result<Usage> {
    let mut usage = Usage::default();
    let apps_dir = root.join("apps");
    let Ok(rd) = fs::read_dir(&apps_dir) else {
        return Ok(usage);
    };
    for app in rd.flatten() {
        let bytes: u64 = fs::read_dir(app.path())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|f| f.metadata().ok())
            .map(|m| m.len())
            .sum();
        usage.total_bytes += bytes;
        usage
            .per_app
            .push((app.file_name().to_string_lossy().into_owned(), bytes));
    }
    usage.per_app.sort();
    Ok(usage)
}

/// Read-only access to a store directory. Needs no running breadd.
pub struct Reader {
    root: PathBuf,
}

impl Reader {
    pub fn open(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Apps that have any stored data.
    pub fn apps(&self) -> Vec<String> {
        let mut apps: Vec<String> = fs::read_dir(self.root.join("apps"))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|a| valid_app_id(a))
            .collect();
        apps.sort();
        apps
    }

    pub fn usage(&self) -> Result<Usage> {
        usage_of(&self.root)
    }

    /// The last `max_logs` log lines, last `max_events` events and the most
    /// recent markers for `app`, oldest first. Unparsable lines (a torn
    /// final write) are skipped.
    pub fn snapshot(&self, app: &str, max_logs: usize, max_events: usize) -> Result<AppSnapshot> {
        check_app(app)?;
        let dir = self.root.join("apps").join(app);
        Ok(AppSnapshot {
            app: app.to_string(),
            logs: read_tail(&dir, LOG_FILE, max_logs),
            events: read_tail(&dir, EVENT_FILE, max_events),
            markers: read_tail(&dir, MARKER_FILE, MAX_MARKERS_IN_SNAPSHOT),
        })
    }

    /// The most recent marker for `app`, if any.
    pub fn last_marker(&self, app: &str) -> Option<Marker> {
        let dir = self.root.join("apps").join(app);
        read_tail::<Marker>(&dir, MARKER_FILE, 1).pop()
    }
}

fn read_tail<T: for<'de> Deserialize<'de>>(dir: &Path, file: &str, max: usize) -> Vec<T> {
    let mut out: VecDeque<T> = VecDeque::new();
    for name in [format!("{file}.1"), file.to_string()] {
        let Ok(f) = fs::File::open(dir.join(name)) else {
            continue;
        };
        for line in BufReader::new(f).split(b'\n').map_while(|l| l.ok()) {
            let Ok(rec) = serde_json::from_slice::<T>(&line) else {
                continue;
            };
            if out.len() >= max {
                out.pop_front();
            }
            if max > 0 {
                out.push_back(rec);
            }
        }
    }
    out.into()
}

#[cfg(test)]
mod tests;
