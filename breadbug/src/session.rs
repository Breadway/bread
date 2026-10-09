//! The user-facing operations behind the reporter, with structured results.
//!
//! The CLI prints these results; the GUI shows them. Keeping the logic here
//! (not in either front end) means both behave identically: nothing is sent
//! unless sending is on, the previewed bytes are what is sent, an unreachable
//! server queues the report, and a rejection is final.

use anyhow::{bail, Result};
use bread_bug::config::BugConfig;
use bread_bug::report::{build, AppEntry, BuildOptions, Prepared, ReportType};
use bread_bug::store::{Frame, MarkerKind, Reader};
use bread_bug::upload::{install_id, Client, History, HistoryEntry, HistoryKind, Queue, SendError};

use crate::Env;

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// What happened when the user chose Send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendOutcome {
    /// Sending is turned off in settings; nothing was done.
    Disabled,
    /// Accepted. `deletable` is whether a deletion token was kept.
    Sent { receipt: String, deletable: bool },
    /// The server was unreachable; the exact report is queued for retry.
    Queued { why: String },
    /// Unreachable and the queue is full; nothing was kept.
    QueueFull { why: String },
    /// The server refused it; retrying will not help.
    Rejected { why: String },
}

/// Whether the user has allowed sending.
pub fn upload_enabled(env: &Env) -> Result<bool> {
    Ok(BugConfig::load_from(&env.config_path)?.upload_enabled)
}

/// The apps the user can report on.
pub fn apps(env: &Env) -> Vec<AppEntry> {
    crate::entries(env)
}

/// The report type a problem on `app` most likely is.
pub fn default_type(env: &Env, app: &str) -> bread_bug::report::ReportType {
    apps(env).iter().find(|e| e.app == app).map_or(
        bread_bug::report::ReportType::Behavior,
        crate::default_type_for,
    )
}

/// Save the exact previewed bytes to `path` and remember it in the history.
pub fn save(env: &Env, p: &Prepared, path: &std::path::Path) -> Result<()> {
    p.save(path)?;
    // Remembering that a file was saved is a convenience; never fail on it.
    if let Ok(h) = History::open(&env.state_dir) {
        let _ = h.append(&HistoryEntry {
            t: now(),
            kind: HistoryKind::Saved,
            app: p.report.app.id.clone(),
            sha256: p.sha256.clone(),
            receipt: None,
            deletion_token: None,
            path: Some(path.display().to_string()),
            status: None,
            fixed_in: None,
            told_fixed: false,
        });
    }
    Ok(())
}

/// Send the previewed report, if the user has allowed sending.
pub fn send(env: &Env, p: &Prepared) -> Result<SendOutcome> {
    let cfg = BugConfig::load_from(&env.config_path)?;
    if !cfg.upload_enabled {
        return Ok(SendOutcome::Disabled);
    }
    let client = Client::new(&cfg.endpoint)?;
    if !p.verify() {
        bail!("report bytes no longer match their hash; refusing to send");
    }
    let t = now();
    let install = install_id(&env.state_dir, t)?;
    let history = History::open(&env.state_dir)?;
    Ok(match client.send(p, &install) {
        Ok(acc) => {
            history.append(&HistoryEntry {
                t,
                kind: HistoryKind::Sent,
                app: p.report.app.id.clone(),
                sha256: p.sha256.clone(),
                receipt: Some(acc.receipt.clone()),
                deletion_token: acc.deletion_token.clone(),
                path: None,
                status: Some("received".into()),
                fixed_in: None,
                told_fixed: false,
            })?;
            SendOutcome::Sent {
                receipt: acc.receipt,
                deletable: acc.deletion_token.is_some(),
            }
        }
        Err(SendError::Retriable(why)) => {
            if Queue::open(&env.state_dir)?.enqueue(p, t)? {
                SendOutcome::Queued { why }
            } else {
                SendOutcome::QueueFull { why }
            }
        }
        Err(SendError::Rejected(why)) => SendOutcome::Rejected { why },
    })
}

/// The pid of a frozen, still-running bread process for `app`, if there is
/// one a stack could be taken from (with the user's consent).
pub fn live_stack_pid(env: &Env, app: &str) -> Option<u32> {
    Reader::open(&env.store_root)
        .snapshot(app, 0, 0)
        .ok()?
        .markers
        .into_iter()
        .rev()
        .find(|m| m.kind != MarkerKind::Recovered)
        .filter(|m| m.kind == MarkerKind::Freeze)
        .and_then(|m| m.pid)
        .filter(|p| (env.pid_ok)(*p))
}

/// Build the report for the previewed choices. Slow (it runs helper
/// commands), so front ends call it off the UI thread.
pub fn build_report(
    env: &Env,
    app: &str,
    report_type: ReportType,
    notes: Option<String>,
    live_stack_pid: Option<u32>,
) -> Result<Prepared> {
    let mut o = BuildOptions::new(app, report_type);
    o.store_root = env.store_root.clone();
    o.socket = env.socket.clone();
    o.scrub = env.scrub.clone();
    o.include_journal = env.include_journal;
    o.notes = notes.filter(|n| !n.trim().is_empty());
    if let Some(pid) = live_stack_pid {
        o.live_stack = (env.live_capture)(pid).unwrap_or_else(Vec::<Frame>::new);
    }
    build(&o)
}
