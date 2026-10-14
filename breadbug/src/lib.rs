//! The `breadbug` command: pick an app, review exactly what would be
//! reported, optionally add notes, then save it to a file or send it.
//!
//! Nothing leaves the machine unless the user explicitly chooses to send,
//! and what is sent is byte-for-byte what was previewed.

use std::io::{BufRead, Write};
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use bread_bug::config::{self, BugConfig};
use bread_bug::livestack;
use bread_bug::report::{self, AppEntry, BuildOptions, Prepared, ReportType};
use bread_bug::store::{Frame, MarkerKind, Reader};
use bread_bug::system::{installed_apps, InstalledApp};
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
    /// Turn sending reports on or off.
    Config {
        #[command(subcommand)]
        what: ConfigCmd,
    },
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
    pub scrub: ScrubContext,
    /// `None` reads bakery's real state.
    pub installed: Option<Option<Vec<InstalledApp>>>,
    pub include_journal: bool,
    /// Captures the frames of a live process (injectable for tests).
    pub live_capture: fn(u32) -> Option<Vec<Frame>>,
    /// Whether a pid is still a running bread process.
    pub pid_ok: fn(u32) -> bool,
}

impl Env {
    pub fn from_environment() -> Self {
        Self {
            store_root: config::store_root(),
            socket: bread_bug::ipc::default_socket_path(),
            config_path: config::config_path(),
            scrub: ScrubContext::from_environment(),
            installed: None,
            include_journal: true,
            live_capture: livestack::capture,
            pid_ok: livestack::is_bread_process,
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
        prepared.save(path)?;
        writeln!(
            out,
            "Saved to {} (sha256 {}). Nothing was sent.",
            path.display(),
            prepared.sha256
        )?;
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

/// Offer (or, with `--live-stack`, just do) a stack capture of a frozen
/// process. Only for the pid recorded in the app's freeze marker, and only
/// while that process is still a running bread process.
fn maybe_live_stack(
    cli: &Cli,
    env: &Env,
    app: &str,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    interactive: bool,
) -> Result<Vec<Frame>> {
    let pid = Reader::open(&env.store_root)
        .snapshot(app, 0, 0)
        .ok()
        .and_then(|s| {
            s.markers
                .into_iter()
                .rev()
                .find(|m| m.kind != MarkerKind::Recovered)
        })
        .filter(|m| m.kind == MarkerKind::Freeze)
        .and_then(|m| m.pid)
        .filter(|p| (env.pid_ok)(*p));
    let Some(pid) = pid else {
        if cli.live_stack {
            writeln!(
                out,
                "No running frozen {app} process found; skipping the live stack."
            )?;
        }
        return Ok(Vec::new());
    };
    let consent = if cli.live_stack {
        true
    } else if interactive && cli.app.is_none() {
        writeln!(
            out,
            "\n{app} (pid {pid}) looks frozen and is still running. A stack trace shows where it is stuck.\nIt contains function names and offsets only, no memory."
        )?;
        prompt(input, out, "Capture one now and include it? [y/N]: ")?
            .is_some_and(|a| matches!(a.to_lowercase().as_str(), "y" | "yes"))
    } else {
        false
    };
    if !consent {
        return Ok(Vec::new());
    }
    match (env.live_capture)(pid) {
        Some(frames) => Ok(frames),
        None => {
            writeln!(out, "Could not capture a stack (is elfutils' eu-stack installed?). Continuing without it.")?;
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
                p.save(&path)?;
                writeln!(out, "Saved to {}. Nothing was sent.", path.display())?;
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

fn send(p: &Prepared, env: &Env, out: &mut dyn Write) -> Result<()> {
    let cfg = BugConfig::load_from(&env.config_path)?;
    if !cfg.upload_enabled {
        writeln!(
            out,
            "Sending is turned off. Enable it in bos-settings (Bug reports) or run\n  breadbug config upload on\nYou can still save the report to a file."
        )?;
        return Ok(());
    }
    if !p.verify() {
        bail!("report bytes no longer match their hash; refusing to send");
    }
    writeln!(
        out,
        "Sending is not available in this build yet. Nothing was sent."
    )?;
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
