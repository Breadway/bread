//! The report schema and builder.
//!
//! A report is built once, scrubbed, serialized to bytes and hashed. Those
//! exact bytes are what the user previews, what "save to file" writes and
//! what the upload sends; nothing is regenerated after the preview.

use std::collections::BTreeMap;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use bread_scrub::{ScrubContext, Scrubber};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::runcmd;
use crate::store::{
    valid_app_id, EventRecord, Frame, LogRecord, Marker, MarkerKind, Reader, DAEMON_APP,
};
use crate::system::{installed_apps, InstalledApp, SystemInfo};
use crate::{ipc, store};

/// Bumped when the shape changes in a way the server must know about.
pub const SCHEMA_VERSION: u32 = 1;
/// Longest user note kept, in bytes.
pub const MAX_NOTES_BYTES: usize = 8 * 1024;
/// Hard cap on the serialized payload; the builder trims to fit.
pub const MAX_PAYLOAD_BYTES: usize = 1024 * 1024;
const MAX_JOURNAL_LINE: usize = 2048;
const MAX_MARKERS: usize = 5;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReportType {
    Crash,
    Freeze,
    Behavior,
}

impl std::str::FromStr for ReportType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "crash" => Ok(Self::Crash),
            "freeze" => Ok(Self::Freeze),
            "behavior" | "behaviour" | "wrong" => Ok(Self::Behavior),
            other => Err(format!(
                "unknown report type '{other}': crash, freeze or behavior"
            )),
        }
    }
}

impl ReportType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Crash => "crash",
            Self::Freeze => "freeze",
            Self::Behavior => "behavior",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppInfo {
    pub id: String,
    pub version: Option<String>,
    pub build_id: Option<String>,
}

/// A coarse, non-personal view of the daemon's state.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeSummary {
    pub daemon_running: bool,
    pub uptime_s: Option<u64>,
    pub adapters: BTreeMap<String, String>,
    pub modules_total: Option<u32>,
    pub modules_with_errors: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Report {
    pub schema: u32,
    pub report_type: ReportType,
    pub created_unix: u64,
    pub app: AppInfo,
    pub system: SystemInfo,
    pub installed_apps: Vec<InstalledApp>,
    pub runtime: RuntimeSummary,
    pub markers: Vec<Marker>,
    pub logs: Vec<LogRecord>,
    pub daemon_logs: Vec<LogRecord>,
    pub events: Vec<EventRecord>,
    pub journal: Vec<String>,
    /// Frames of the live process, only when the user agreed to capture it.
    pub live_stack: Vec<Frame>,
    pub notes: Option<String>,
    /// How many values of each kind the scrubber replaced.
    pub redactions: BTreeMap<String, usize>,
}

/// A finished report: the struct, its exact bytes and their SHA-256.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub report: Report,
    pub bytes: Vec<u8>,
    pub sha256: String,
}

impl Prepared {
    pub fn from_report(report: Report) -> Result<Self> {
        let mut bytes = serde_json::to_vec_pretty(&report)?;
        bytes.push(b'\n');
        let sha256 = hex::encode(Sha256::digest(&bytes));
        Ok(Self {
            report,
            bytes,
            sha256,
        })
    }

    /// Whether `bytes` still hash to the value the user was shown.
    pub fn verify(&self) -> bool {
        hex::encode(Sha256::digest(&self.bytes)) == self.sha256
    }

    /// Write the exact bytes to `path` (mode 0600).
    pub fn save(&self, path: &Path) -> Result<()> {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("create {}", path.display()))?;
        f.write_all(&self.bytes)?;
        Ok(())
    }
}

/// Everything the builder needs.
#[derive(Debug, Clone)]
pub struct BuildOptions {
    pub app: String,
    pub report_type: ReportType,
    pub notes: Option<String>,
    pub live_stack: Vec<Frame>,
    pub socket: PathBuf,
    pub store_root: PathBuf,
    pub scrub: ScrubContext,
    pub include_journal: bool,
    pub max_logs: usize,
    pub max_events: usize,
    pub now_unix: u64,
}

impl BuildOptions {
    pub fn new(app: &str, report_type: ReportType) -> Self {
        Self {
            app: app.to_string(),
            report_type,
            notes: None,
            live_stack: Vec::new(),
            socket: ipc::default_socket_path(),
            store_root: crate::config::store_root(),
            scrub: ScrubContext::from_environment(),
            include_journal: true,
            max_logs: 300,
            max_events: 150,
            now_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
        }
    }
}

fn runtime_summary(health: Option<&Value>) -> RuntimeSummary {
    let Some(h) = health else {
        return RuntimeSummary::default();
    };
    let adapters = h
        .get("adapters")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .map(|(k, v)| {
                    let status = v
                        .get("status")
                        .and_then(Value::as_str)
                        .map_or_else(|| v.to_string(), ToString::to_string);
                    (k.clone(), status)
                })
                .collect()
        })
        .unwrap_or_default();
    let modules = h.get("modules").and_then(Value::as_array);
    RuntimeSummary {
        daemon_running: true,
        uptime_s: h
            .get("uptime_ms")
            .and_then(Value::as_u64)
            .map(|ms| ms / 1000),
        adapters,
        modules_total: modules.map(|m| m.len() as u32),
        modules_with_errors: modules.map(|m| {
            m.iter()
                .filter(|x| x.get("last_error").is_some_and(|e| !e.is_null()))
                .count() as u32
        }),
    }
}

fn journal_lines(app: &str) -> Vec<String> {
    let comm = if app == DAEMON_APP {
        "breadd".to_string()
    } else {
        format!("bread{app}")
    };
    let comm_d = format!("_COMM={comm}d");
    let comm = format!("_COMM={comm}");
    let out = runcmd::run(
        "journalctl",
        &[
            "--user",
            "--no-pager",
            "-n",
            "150",
            "-o",
            "cat",
            &comm,
            "+",
            &comm_d,
        ],
        Duration::from_secs(3),
    )
    .unwrap_or_default();
    out.lines()
        .map(|l| {
            let mut l = l.to_string();
            if l.len() > MAX_JOURNAL_LINE {
                let mut cut = MAX_JOURNAL_LINE;
                while !l.is_char_boundary(cut) {
                    cut -= 1;
                }
                l.truncate(cut);
            }
            l
        })
        .collect()
}

fn truncate_notes(notes: &str) -> String {
    let mut n = notes.trim().to_string();
    if n.len() > MAX_NOTES_BYTES {
        let mut cut = MAX_NOTES_BYTES;
        while !n.is_char_boundary(cut) {
            cut -= 1;
        }
        n.truncate(cut);
    }
    n
}

/// Build, scrub, serialize and hash a report.
pub fn build(opts: &BuildOptions) -> Result<Prepared> {
    if !valid_app_id(&opts.app) {
        bail!("invalid app id {:?}", opts.app);
    }
    let reader = Reader::open(&opts.store_root);
    let snap = reader.snapshot(&opts.app, opts.max_logs, opts.max_events)?;
    let daemon = if opts.app == DAEMON_APP {
        store::AppSnapshot::default()
    } else {
        reader.snapshot(DAEMON_APP, opts.max_logs, opts.max_events)?
    };
    let health = ipc::request(
        &opts.socket,
        "health",
        serde_json::json!({}),
        Duration::from_secs(2),
    )
    .ok();

    let (track_apps, version) = match installed_apps() {
        Some((_, apps)) => {
            let v = apps
                .iter()
                .find(|a| a.id == opts.app)
                .map(|a| a.version.clone());
            (apps, v)
        }
        None => (Vec::new(), None),
    };
    let mut markers = snap.markers;
    let start = markers.len().saturating_sub(MAX_MARKERS);
    markers.drain(..start);
    let marker_version = markers.iter().rev().find_map(|m| m.app_version.clone());
    let build_id = markers.iter().rev().find_map(|m| m.build_id.clone());

    let mut events: Vec<EventRecord> = snap.events.into_iter().chain(daemon.events).collect();
    events.sort_by_key(|e| e.t);
    let skip = events.len().saturating_sub(opts.max_events);
    events.drain(..skip);

    // Only the daemon's warnings and errors are worth including for an app.
    let daemon_logs: Vec<LogRecord> = daemon
        .logs
        .into_iter()
        .filter(|l| l.level == "WARN" || l.level == "ERROR")
        .collect();

    let mut report = Report {
        schema: SCHEMA_VERSION,
        report_type: opts.report_type,
        created_unix: opts.now_unix,
        app: AppInfo {
            id: opts.app.clone(),
            version: version.or(marker_version),
            build_id,
        },
        system: SystemInfo::gather(health.as_ref()),
        installed_apps: track_apps,
        runtime: runtime_summary(health.as_ref()),
        markers,
        logs: snap.logs,
        daemon_logs,
        events,
        journal: if opts.include_journal {
            journal_lines(&opts.app)
        } else {
            Vec::new()
        },
        live_stack: opts.live_stack.clone(),
        notes: opts
            .notes
            .as_deref()
            .map(truncate_notes)
            .filter(|n| !n.is_empty()),
        redactions: BTreeMap::new(),
    };

    scrub_report(&mut report, &opts.scrub)?;
    trim_to_fit(&mut report)?;
    Prepared::from_report(report)
}

/// Run the whole report through fresh scrubbers. Build-ids are exempt from
/// the long-hex rules because the server needs them for symbolication. A
/// recognized `system.os_id` may be kept only when it does not equal any
/// configured identity; if a user or hostname is literally `bos` or `arch`,
/// the configured identity wins and is redacted even in that field. All
/// versions, kernels, runtime fields, app fields and free text are always
/// scrubbed.
fn scrub_report(report: &mut Report, ctx: &ScrubContext) -> Result<()> {
    let mut full = Scrubber::new(ctx.clone());
    let os_id = report.system.os_id.clone();
    let os_id_is_identity = os_id.as_ref().is_some_and(|os_id| {
        ctx.username
            .iter()
            .chain(ctx.hostname.iter())
            .chain(ctx.extra_literals.iter())
            .any(|identity| identity.trim().eq_ignore_ascii_case(os_id))
    });
    let ids: Vec<String> = report
        .markers
        .iter()
        .flat_map(|m| {
            m.build_id
                .iter()
                .chain(m.frames.iter().filter_map(|f| f.build_id.as_ref()))
        })
        .chain(report.app.build_id.iter())
        .chain(report.live_stack.iter().filter_map(|f| f.build_id.as_ref()))
        .cloned()
        .collect();
    for id in &ids {
        full.keep_build_id(id);
    }
    // Process identity is stored locally only to guard live capture; it is
    // never part of ECO-66's report schema. Pids are also unnecessary in the
    // report, so omit both before the final whole-report scrub.
    for marker in &mut report.markers {
        marker.pid = None;
        marker.process = None;
    }
    let value = serde_json::to_value(&*report)?;
    let scrubbed = full.scrub_value_preserving_keys(&value);
    *report = serde_json::from_value(scrubbed).context("scrubbed report no longer parses")?;
    if !os_id_is_identity {
        report.system.os_id = os_id;
    }
    report.redactions = full
        .stats()
        .iter()
        .map(|(k, v)| ((*k).to_string(), *v))
        .collect();
    Ok(())
}

/// Drop the least valuable data until the payload fits the cap: journal
/// first, then the oldest events and logs.
fn trim_to_fit(report: &mut Report) -> Result<()> {
    loop {
        let size = serde_json::to_vec_pretty(&*report)?.len();
        if size <= MAX_PAYLOAD_BYTES {
            return Ok(());
        }
        if !report.journal.is_empty() {
            let half = report.journal.len() / 2;
            report.journal.drain(..half.max(1));
        } else if report.events.len() > 10 {
            let half = report.events.len() / 2;
            report.events.drain(..half);
        } else if report.logs.len() > 10 {
            let half = report.logs.len() / 2;
            report.logs.drain(..half);
        } else if report.daemon_logs.len() > 10 {
            let half = report.daemon_logs.len() / 2;
            report.daemon_logs.drain(..half);
        } else {
            bail!("report cannot be trimmed below {MAX_PAYLOAD_BYTES} bytes");
        }
    }
}

/// One row of the app picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppEntry {
    pub app: String,
    pub version: Option<String>,
    /// Kind and time of the newest crash/freeze marker, if any.
    pub last_problem: Option<(MarkerKind, u64)>,
}

/// Apps the user can report on: installed bread apps, with ones that
/// recently crashed or froze first. Without bakery state, apps that have
/// data in the store.
pub fn app_entries(reader: &Reader, installed: Option<&[InstalledApp]>) -> Vec<AppEntry> {
    let candidates: Vec<(String, Option<String>)> = match installed {
        Some(apps) => apps
            .iter()
            .map(|a| (a.id.clone(), Some(a.version.clone())))
            .collect(),
        None => reader.apps().into_iter().map(|a| (a, None)).collect(),
    };
    let mut entries: Vec<AppEntry> = candidates
        .into_iter()
        .map(|(app, version)| {
            let last_problem = reader
                .snapshot(&app, 0, 0)
                .ok()
                .and_then(|s| s.markers.into_iter().rev().find(|m| m.kind.is_problem()))
                .map(|m| (m.kind, m.t));
            AppEntry {
                app,
                version,
                last_problem,
            }
        })
        .collect();
    entries.sort_by(|a, b| match (&a.last_problem, &b.last_problem) {
        (Some((_, ta)), Some((_, tb))) => tb.cmp(ta).then_with(|| a.app.cmp(&b.app)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.app.cmp(&b.app),
    });
    entries
}

#[cfg(test)]
mod tests;
