//! The `breadbug` command: pick an app, review exactly what would be
//! reported, optionally add notes, then save it to a file or send it.
//!
//! Nothing leaves the machine unless the user explicitly chooses to send,
//! and what is sent is byte-for-byte what was previewed.

use std::io::{BufRead, Write};
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use bread_bug::config::{self, BugConfig};
use bread_bug::livestack::{self, Refusal};
use bread_bug::report::{self, AppEntry, BuildOptions, Prepared, ReportType};
use bread_bug::store::{Frame, MarkerKind, ProcessIdentity, Reader};
use bread_bug::system::{installed_apps, InstalledApp};
use bread_bug::upload::{
    install_id, state_dir, Client, History, HistoryEntry, HistoryKind, Queue, SendError,
};
use bread_scrub::ScrubContext;
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "breadbug",
    version,
    about = "Review and optionally send a crash, freeze or bug report for a bread app",
    after_help = "With no arguments, breadbug walks you through picking an app and reviewing the report.\nNothing is sent unless you choose to send it."
)]
pub struct Cli {
    /// App to report on (e.g. clip, pad, breadd). Omit to pick interactively.
    #[arg(long)]
    pub app: Option<String>,

    /// What went wrong: crash, freeze or behavior.
    #[arg(long = "type", value_name = "TYPE")]
    pub report_type: Option<ReportType>,

    /// Your own description of the problem. It is scrubbed like everything else.
    #[arg(long)]
    pub notes: Option<String>,

    /// Print the exact report that would be sent, then exit.
    #[arg(long)]
    pub dry_run: bool,

    /// Save the exact report to this file instead of sending it.
    #[arg(long, value_name = "PATH")]
    pub save: Option<PathBuf>,

    /// Send the previewed report (needs upload to be enabled in settings).
    #[arg(long)]
    pub send: bool,

    /// For a frozen app that is still running, include a stack trace of it
    /// (frames only, taken now with your consent).
    #[arg(long)]
    pub live_stack: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List the apps you can report on.
    List,
    /// Reports you have sent or saved. `--refresh` asks the server for their status.
    History {
        #[arg(long)]
        refresh: bool,
    },
    /// Delete a report you sent from the server (needs the receipt id).
    Delete { receipt: String },
    /// Reports waiting to be sent because the server was unreachable.
    Queue {
        #[command(subcommand)]
        action: Option<QueueCmd>,
    },
    /// Turn sending reports on or off.
    Config {
        #[command(subcommand)]
        what: ConfigCmd,
    },
}

#[derive(Debug, Subcommand)]
pub enum QueueCmd {
    /// Try to send everything in the queue now.
    Flush,
    /// Drop one queued report (by the id `queue` shows).
    Cancel { id: String },
    /// Drop everything in the queue.
    Clear,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCmd {
    /// Allow or forbid sending reports (saving to a file is always allowed).
    Upload { state: Toggle },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Toggle {
    On,
    Off,
}

/// Where breadbug reads and writes things; overridable for tests.
#[derive(Debug, Clone)]
pub struct Env {
    pub store_root: PathBuf,
    pub socket: PathBuf,
    pub config_path: PathBuf,
    /// Install id, history and the offline queue live here.
    pub state_dir: PathBuf,
    pub scrub: ScrubContext,
    /// `None` reads bakery's real state.
    pub installed: Option<Option<Vec<InstalledApp>>>,
    pub include_journal: bool,
    /// Captures the frames of a live process (injectable for tests). It must
    /// verify `(app, pid, identity)` itself immediately before attaching.
    pub live_capture: LiveCapture,
    /// Whether the recorded instance is still the process behind its pid
    /// (injectable for tests). Used before offering a capture at all.
    pub live_check: LiveCheck,
}

/// See [`Env::live_capture`].
pub type LiveCapture = fn(&str, u32, &ProcessIdentity) -> Result<Vec<Frame>, Refusal>;
/// See [`Env::live_check`].
pub type LiveCheck = fn(&str, u32, &ProcessIdentity) -> Result<(), Refusal>;

impl Env {
    pub fn from_environment() -> Self {
        Self {
            store_root: config::store_root(),
            socket: bread_bug::ipc::default_socket_path(),
            config_path: config::config_path(),
            state_dir: state_dir(),
            scrub: ScrubContext::from_environment(),
            installed: None,
            include_journal: true,
            live_capture: livestack::capture,
            live_check: livestack::check_target,
        }
    }

    fn installed_apps(&self) -> Option<Vec<InstalledApp>> {
        match &self.installed {
            Some(fixed) => fixed.clone(),
            None => installed_apps().map(|(_, apps)| apps),
        }
    }
}

pub fn run(
    cli: &Cli,
    env: &Env,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    interactive: bool,
) -> Result<()> {
    match &cli.command {
        Some(Command::List) => return list(env, out),
        Some(Command::History { refresh }) => return history(env, *refresh, out),
        Some(Command::Delete { receipt }) => return delete(env, receipt, out),
        Some(Command::Queue { action }) => return queue(env, action.as_ref(), out),
        Some(Command::Config {
            what: ConfigCmd::Upload { state },
        }) => return set_upload(env, *state == Toggle::On, out),
        None => {}
    }

    let entries = entries(env);
    let (app, default_type) = match &cli.app {
        Some(app) => {
            let default = entries
                .iter()
                .find(|e| &e.app == app)
                .map_or(ReportType::Behavior, default_type_for);
            (app.clone(), default)
        }
        None if interactive => match pick_app(&entries, input, out)? {
            Some(e) => (e.app.clone(), default_type_for(e)),
            None => return Ok(()),
        },
        None => bail!("no --app given and not running interactively; see `breadbug list`"),
    };

    let report_type = match cli.report_type {
        Some(t) => t,
        None if interactive && cli.app.is_none() => ask_type(default_type, input, out)?,
        None => default_type,
    };
    let notes = match &cli.notes {
        Some(n) => Some(n.clone()),
        None if interactive && cli.app.is_none() => ask_notes(input, out)?,
        None => None,
    };

    let mut opts = BuildOptions::new(&app, report_type);
    opts.store_root = env.store_root.clone();
    opts.socket = env.socket.clone();
    opts.scrub = env.scrub.clone();
    opts.include_journal = env.include_journal;
    opts.notes = notes;
    if report_type == ReportType::Freeze {
        opts.live_stack = maybe_live_stack(cli, env, &app, input, out, interactive)?;
    }
    let prepared = report::build(&opts).context("build report")?;

    if cli.dry_run {
        out.write_all(&prepared.bytes)?;
        return Ok(());
    }

    if let Some(path) = &cli.save {
        save(&prepared, path, env, out)?;
        return Ok(());
    }

    if cli.send {
        print_summary(&prepared, out)?;
        return send(&prepared, env, out);
    }

    if !interactive {
        print_summary(&prepared, out)?;
        writeln!(
            out,
            "\nNothing was sent. Use --save PATH, --send or --dry-run."
        )?;
        return Ok(());
    }
    review_loop(&prepared, env, input, out)
}

/// Why the app has no capturable frozen process, or the target if it has.
///
/// Only the app's *latest* marker counts. A `recovered`, `stopped` (clean
/// exit, death or replacement) or later crash marker closes the freeze, so an
/// old freeze is never walked back to. The marker's recorded process identity
/// (pid, start time, executable) must still match the live process.
fn live_target(env: &Env, app: &str) -> Result<(u32, ProcessIdentity), Refusal> {
    let marker = Reader::open(&env.store_root)
        .snapshot(app, 0, 0)
        .ok()
        .and_then(|s| s.markers.into_iter().next_back())
        .filter(|m| m.kind == MarkerKind::Freeze)
        .ok_or(Refusal::NoActiveFreeze)?;
    let (Some(pid), Some(identity)) = (marker.pid, marker.process) else {
        return Err(Refusal::NoIdentity);
    };
    (env.live_check)(app, pid, &identity)?;
    Ok((pid, identity))
}

/// Offer (or, with `--live-stack`, just do) a stack capture of a frozen
/// process. Only for the instance recorded in the app's latest marker, only
/// while it is still that instance, and checked again after consent and right
/// before attaching.
fn maybe_live_stack(
    cli: &Cli,
    env: &Env,
    app: &str,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    interactive: bool,
) -> Result<Vec<Frame>> {
    let (pid, identity) = match live_target(env, app) {
        Ok(target) => target,
        Err(why) => {
            if cli.live_stack {
                writeln!(out, "Not capturing a live stack for {app}: {why}.")?;
            }
            return Ok(Vec::new());
        }
    };
    let consent = if cli.live_stack {
        true
    } else if interactive && cli.app.is_none() {
        writeln!(
            out,
            "\n{app} (pid {pid}) looks frozen and is still running. A stack trace shows where it is stuck.\nIt contains module names, offsets and build ids only, no memory, registers or environment."
        )?;
        prompt(input, out, "Capture one now and include it? [y/N]: ")?
            .is_some_and(|a| matches!(a.to_lowercase().as_str(), "y" | "yes"))
    } else {
        false
    };
    if !consent {
        return Ok(Vec::new());
    }
    // Time has passed (the user was reading a prompt): the capture function
    // verifies the instance again immediately before it attaches.
    match (env.live_capture)(app, pid, &identity) {
        Ok(frames) => Ok(frames),
        Err(why) => {
            writeln!(
                out,
                "Not capturing a live stack for {app}: {why}. Continuing without it."
            )?;
            Ok(Vec::new())
        }
    }
}

fn entries(env: &Env) -> Vec<AppEntry> {
    let reader = Reader::open(&env.store_root);
    report::app_entries(&reader, env.installed_apps().as_deref())
}

fn default_type_for(e: &AppEntry) -> ReportType {
    match e.last_problem {
        Some((MarkerKind::Freeze, _)) => ReportType::Freeze,
        Some(_) => ReportType::Crash,
        None => ReportType::Behavior,
    }
}

fn list(env: &Env, out: &mut dyn Write) -> Result<()> {
    let entries = entries(env);
    if entries.is_empty() {
        writeln!(out, "No bread apps found.")?;
    }
    for e in entries {
        writeln!(out, "{}", describe(&e))?;
    }
    Ok(())
}

fn describe(e: &AppEntry) -> String {
    let version = e
        .version
        .as_deref()
        .map_or(String::new(), |v| format!(" {v}"));
    let problem = match e.last_problem {
        Some((kind, _)) => format!("  (last problem: {kind:?})").to_lowercase(),
        None => String::new(),
    };
    format!("{}{version}{problem}", e.app)
}

fn pick_app<'a>(
    entries: &'a [AppEntry],
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<Option<&'a AppEntry>> {
    if entries.is_empty() {
        writeln!(out, "No bread apps found to report on.")?;
        return Ok(None);
    }
    writeln!(out, "Which app has the problem?\n")?;
    for (i, e) in entries.iter().enumerate() {
        writeln!(out, "  {:>2}) {}", i + 1, describe(e))?;
    }
    loop {
        let Some(line) = prompt(input, out, "\nNumber (or q to quit): ")? else {
            return Ok(None);
        };
        if matches!(line.as_str(), "q" | "quit") {
            return Ok(None);
        }
        if let Some(e) = line
            .parse::<usize>()
            .ok()
            .and_then(|n| entries.get(n.wrapping_sub(1)))
        {
            return Ok(Some(e));
        }
        writeln!(out, "Please enter a number from the list.")?;
    }
}

fn ask_type(
    default: ReportType,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<ReportType> {
    loop {
        let q = format!(
            "What happened? crash / freeze / behavior [{}]: ",
            default.as_str()
        );
        let Some(line) = prompt(input, out, &q)? else {
            return Ok(default);
        };
        if line.is_empty() {
            return Ok(default);
        }
        match line.parse() {
            Ok(t) => return Ok(t),
            Err(e) => writeln!(out, "{e}")?,
        }
    }
}

fn ask_notes(input: &mut dyn BufRead, out: &mut dyn Write) -> Result<Option<String>> {
    writeln!(
        out,
        "\nAnything you want to add? One line, optional. Do not paste passwords or private text;\nit is scrubbed, but you are the best judge of what is private."
    )?;
    Ok(prompt(input, out, "Notes: ")?.filter(|s| !s.is_empty()))
}

/// Print `msg`, read one trimmed line. `None` at end of input.
fn prompt(input: &mut dyn BufRead, out: &mut dyn Write, msg: &str) -> Result<Option<String>> {
    write!(out, "{msg}")?;
    out.flush()?;
    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    Ok(Some(line.trim().to_string()))
}

fn print_summary(p: &Prepared, out: &mut dyn Write) -> Result<()> {
    let r = &p.report;
    writeln!(
        out,
        "\nReport for {} ({})",
        r.app.id,
        r.report_type.as_str()
    )?;
    writeln!(out, "  sha256  {}", p.sha256)?;
    writeln!(out, "  size    {:.1} KiB\n", p.bytes.len() as f64 / 1024.0)?;
    let rows = [
        ("crash/freeze markers", r.markers.len()),
        ("app log lines", r.logs.len()),
        ("daemon warnings", r.daemon_logs.len()),
        ("event names", r.events.len()),
        ("journal lines", r.journal.len()),
        ("live stack frames", r.live_stack.len()),
        ("installed apps listed", r.installed_apps.len()),
    ];
    for (name, n) in rows {
        writeln!(out, "  {name:<22} {n}")?;
    }
    if let Some(notes) = &r.notes {
        writeln!(out, "\n  your notes: {notes}")?;
    }
    if r.redactions.is_empty() {
        writeln!(out, "\n  The scrubber replaced no values.")?;
    } else {
        let parts: Vec<String> = r
            .redactions
            .iter()
            .map(|(k, v)| format!("{v} {k}"))
            .collect();
        writeln!(out, "\n  The scrubber replaced: {}", parts.join(", "))?;
    }
    Ok(())
}

fn review_loop(
    p: &Prepared,
    env: &Env,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<()> {
    print_summary(p, out)?;
    writeln!(
        out,
        "\nNothing has been sent. This is exactly what would be sent."
    )?;
    loop {
        let Some(choice) = prompt(
            input,
            out,
            "\n[v] view everything  [f] save to file  [s] send  [c] cancel: ",
        )?
        else {
            return Ok(());
        };
        match choice.to_lowercase().as_str() {
            "v" | "view" => {
                out.write_all(&p.bytes)?;
            }
            "f" | "file" | "save" => {
                let default = format!(
                    "breadbug-{}-{}.json",
                    p.report.app.id, p.report.created_unix
                );
                let Some(path) = prompt(input, out, &format!("File [{default}]: "))? else {
                    return Ok(());
                };
                let path = PathBuf::from(if path.is_empty() { default } else { path });
                save(p, &path, env, out)?;
                return Ok(());
            }
            "s" | "send" => return send(p, env, out),
            "c" | "cancel" | "q" | "quit" => {
                writeln!(out, "Cancelled. Nothing was sent.")?;
                return Ok(());
            }
            _ => writeln!(out, "Please choose v, f, s or c.")?,
        }
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn save(p: &Prepared, path: &std::path::Path, env: &Env, out: &mut dyn Write) -> Result<()> {
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
        });
    }
    writeln!(
        out,
        "Saved to {} (sha256 {}). Nothing was sent.",
        path.display(),
        p.sha256
    )?;
    Ok(())
}

/// Network use needs the user's opt-in; returns the client if allowed.
fn client_if_enabled(env: &Env, out: &mut dyn Write) -> Result<Option<Client>> {
    let cfg = BugConfig::load_from(&env.config_path)?;
    if !cfg.upload_enabled {
        writeln!(
            out,
            "Sending is turned off. Enable it in bos-settings (Bug reports) or run\n  breadbug config upload on\nYou can still save the report to a file."
        )?;
        return Ok(None);
    }
    Ok(Some(Client::new(&cfg.endpoint)?))
}

fn send(p: &Prepared, env: &Env, out: &mut dyn Write) -> Result<()> {
    let Some(client) = client_if_enabled(env, out)? else {
        return Ok(());
    };
    if !p.verify() {
        bail!("report bytes no longer match their hash; refusing to send");
    }
    let t = now();
    let install = install_id(&env.state_dir, t)?;
    let history = History::open(&env.state_dir)?;
    match client.send(p, &install) {
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
            })?;
            writeln!(out, "\nSent. Thank you. Your receipt: {}", acc.receipt)?;
            if acc.deletion_token.is_some() {
                writeln!(
                    out,
                    "To delete it from the server later: breadbug delete {}",
                    acc.receipt
                )?;
            }
        }
        Err(SendError::Retriable(why)) => {
            let queue = Queue::open(&env.state_dir)?;
            if queue.enqueue(p, t)? {
                writeln!(
                    out,
                    "\nCould not reach the server ({why}). The report is queued and will be retried; see `breadbug queue`."
                )?;
            } else {
                writeln!(
                    out,
                    "\nCould not reach the server ({why}) and the queue is full. Save the report to a file instead."
                )?;
            }
        }
        Err(e @ SendError::Rejected(_)) => {
            writeln!(
                out,
                "\n{e}. Nothing more will be tried; you can save the report to a file instead."
            )?;
        }
    }
    Ok(())
}

fn history(env: &Env, refresh: bool, out: &mut dyn Write) -> Result<()> {
    let h = History::open(&env.state_dir)?;
    if refresh {
        if let Some(client) = client_if_enabled(env, out)? {
            for e in h.list() {
                let Some(r) = e.receipt.as_deref() else {
                    continue;
                };
                match client.status(r) {
                    Ok(Some(status)) => h.set_status(r, &status)?,
                    Ok(None) => h.set_status(r, "unknown to the server")?,
                    Err(_) => {}
                }
            }
        }
    }
    let all = h.list();
    if all.is_empty() {
        writeln!(out, "No reports sent or saved yet.")?;
    }
    for e in all {
        match e.kind {
            HistoryKind::Sent => writeln!(
                out,
                "sent   {}  {}  receipt {}  {}",
                e.t,
                e.app,
                e.receipt.as_deref().unwrap_or("?"),
                e.status.as_deref().unwrap_or("")
            )?,
            HistoryKind::Saved => writeln!(
                out,
                "saved  {}  {}  {}",
                e.t,
                e.app,
                e.path.as_deref().unwrap_or("?")
            )?,
        }
    }
    Ok(())
}

fn delete(env: &Env, receipt: &str, out: &mut dyn Write) -> Result<()> {
    let h = History::open(&env.state_dir)?;
    let Some(entry) = h.find(receipt) else {
        bail!("no report with receipt {receipt} in your history");
    };
    let Some(token) = entry.deletion_token else {
        bail!("this report has no deletion token on this machine (it was recognised as an earlier send)");
    };
    let Some(client) = client_if_enabled(env, out)? else {
        return Ok(());
    };
    match client.delete(receipt, &token) {
        Ok(found) => {
            h.remove(receipt)?;
            writeln!(
                out,
                "{}",
                if found {
                    "Deleted from the server."
                } else {
                    "The server no longer had it (already deleted or expired). Removed from your history."
                }
            )?;
            Ok(())
        }
        Err(e) => bail!("{e}"),
    }
}

fn queue(env: &Env, action: Option<&QueueCmd>, out: &mut dyn Write) -> Result<()> {
    let q = Queue::open(&env.state_dir)?;
    match action {
        None => {
            let items = q.list();
            if items.is_empty() {
                writeln!(out, "Nothing is waiting to be sent.")?;
            }
            for m in items {
                writeln!(
                    out,
                    "{}  {}  queued {}  attempts {}",
                    &m.sha256[..12],
                    m.app,
                    m.queued_at,
                    m.attempts
                )?;
            }
        }
        Some(QueueCmd::Clear) => {
            writeln!(out, "Dropped {} queued report(s).", q.clear())?;
        }
        Some(QueueCmd::Cancel { id }) => {
            let full = q
                .list()
                .into_iter()
                .find(|m| m.sha256.starts_with(id.as_str()));
            match full {
                Some(m) if id.len() >= 6 && q.cancel(&m.sha256) => writeln!(out, "Dropped.")?,
                _ => bail!("no queued report matches '{id}'"),
            }
        }
        Some(QueueCmd::Flush) => {
            let Some(client) = client_if_enabled(env, out)? else {
                return Ok(());
            };
            let t = now();
            let install = install_id(&env.state_dir, t)?;
            let history = History::open(&env.state_dir)?;
            let (sent, left, dropped) = q.flush(&client, &history, &install, t, true)?;
            writeln!(
                out,
                "Sent {sent}, {left} still queued, {dropped} rejected and dropped."
            )?;
        }
    }
    Ok(())
}

fn set_upload(env: &Env, on: bool, out: &mut dyn Write) -> Result<()> {
    let mut cfg = BugConfig::load_from(&env.config_path)?;
    cfg.upload_enabled = on;
    cfg.save_to(&env.config_path)?;
    writeln!(
        out,
        "Sending reports is now {}. Reports are never sent without you choosing to send them.",
        if on { "allowed" } else { "off" }
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
