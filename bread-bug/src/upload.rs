//! Sending reports, with an offline queue, a local history and an
//! anonymous rotating install id.
//!
//! Only previewed bytes are ever sent: [`Prepared::verify`] is checked
//! immediately before every transmission. The install id is random, lives
//! only on this machine and rotates monthly; the server stores only a keyed
//! hash of it.

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::report::Prepared;

/// The install id is replaced after this many seconds.
pub const INSTALL_ID_LIFETIME_SECS: u64 = 30 * 24 * 3600;
/// Longest wait between retries of a queued report.
const MAX_BACKOFF_SECS: u64 = 6 * 3600;
/// Most reports kept in the offline queue.
pub const MAX_QUEUED: usize = 20;
const MAX_RESPONSE_BYTES: u64 = 64 * 1024;

/// `~/.local/state/bread/breadbug`.
pub fn state_dir() -> PathBuf {
    crate::config::store_root()
        .parent()
        .map_or_else(|| PathBuf::from("breadbug"), |p| p.join("breadbug"))
}

fn private_dir(dir: &Path) -> Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("create {}", dir.display()))
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .with_context(|| format!("write {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

// ---------------------------------------------------------------- install id

#[derive(Debug, Serialize, Deserialize)]
struct InstallFile {
    id: String,
    created: u64,
}

/// The anonymous install id (32 hex), created on first use and replaced
/// when it is older than [`INSTALL_ID_LIFETIME_SECS`].
pub fn install_id(dir: &Path, now: u64) -> Result<String> {
    private_dir(dir)?;
    let path = dir.join("install.json");
    if let Ok(text) = fs::read_to_string(&path) {
        if let Ok(f) = serde_json::from_str::<InstallFile>(&text) {
            let fresh = now.saturating_sub(f.created) < INSTALL_ID_LIFETIME_SECS;
            if fresh && f.id.len() == 32 && f.id.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Ok(f.id);
            }
        }
    }
    let mut raw = [0u8; 16];
    fs::File::open("/dev/urandom")?.read_exact(&mut raw)?;
    let id = hex::encode(raw);
    write_private(
        &path,
        serde_json::to_string(&InstallFile {
            id: id.clone(),
            created: now,
        })?
        .as_bytes(),
    )?;
    Ok(id)
}

// ------------------------------------------------------------------- history

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HistoryKind {
    Sent,
    Saved,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryEntry {
    pub t: u64,
    pub kind: HistoryKind,
    pub app: String,
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<String>,
    /// Needed to delete the report from the server later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deletion_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// The fix version the server reported, once fixed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixed_in: Option<String>,
    /// Whether the user was already told this report was fixed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub told_fixed: bool,
}

pub struct History {
    path: PathBuf,
}

impl History {
    pub fn open(dir: &Path) -> Result<Self> {
        private_dir(dir)?;
        Ok(Self {
            path: dir.join("history.jsonl"),
        })
    }

    pub fn list(&self) -> Vec<HistoryEntry> {
        fs::read_to_string(&self.path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    fn rewrite(&self, entries: &[HistoryEntry]) -> Result<()> {
        let mut text = String::new();
        for e in entries {
            text.push_str(&serde_json::to_string(e)?);
            text.push('\n');
        }
        write_private(&self.path, text.as_bytes())
    }

    pub fn append(&self, entry: &HistoryEntry) -> Result<()> {
        let mut all = self.list();
        all.push(entry.clone());
        // Bounded: keep the newest 200.
        let skip = all.len().saturating_sub(200);
        self.rewrite(&all[skip..])
    }

    pub fn find(&self, receipt: &str) -> Option<HistoryEntry> {
        self.list()
            .into_iter()
            .find(|e| e.receipt.as_deref() == Some(receipt))
    }

    pub fn set_status(&self, receipt: &str, status: &str, fixed_in: Option<&str>) -> Result<()> {
        let mut all = self.list();
        for e in all
            .iter_mut()
            .filter(|e| e.receipt.as_deref() == Some(receipt))
        {
            e.status = Some(status.to_string());
            e.fixed_in = fixed_in.map(String::from);
        }
        self.rewrite(&all)
    }

    pub fn mark_told_fixed(&self, receipt: &str) -> Result<()> {
        let mut all = self.list();
        for e in all
            .iter_mut()
            .filter(|e| e.receipt.as_deref() == Some(receipt))
        {
            e.told_fixed = true;
        }
        self.rewrite(&all)
    }

    pub fn remove(&self, receipt: &str) -> Result<bool> {
        let all = self.list();
        let kept: Vec<_> = all
            .iter()
            .filter(|e| e.receipt.as_deref() != Some(receipt))
            .cloned()
            .collect();
        let removed = kept.len() != all.len();
        if removed {
            self.rewrite(&kept)?;
        }
        Ok(removed)
    }
}

// --------------------------------------------------------------------- client

/// The coarse state of a sent report, as the server reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteStatus {
    /// `received`, `duplicate`, `investigating`, `fixed` or `closed`.
    pub status: String,
    /// The version the fix shipped in, when the server recorded one.
    pub fixed_in: Option<String>,
}

/// What the server said about an accepted report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accepted {
    pub receipt: String,
    /// `None` when the server recognised an identical earlier send.
    pub deletion_token: Option<String>,
}

#[derive(Debug)]
pub enum SendError {
    /// Worth trying again later: network down, timeouts, 429, 5xx.
    Retriable(String),
    /// The server refused it; retrying will not help.
    Rejected(String),
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Retriable(m) => write!(f, "could not reach the server ({m})"),
            Self::Rejected(m) => write!(f, "the server rejected the report ({m})"),
        }
    }
}

impl std::error::Error for SendError {}

pub struct Client {
    base: String,
    agent: ureq::Agent,
}

/// https only, except for loopback (tests and local development).
fn endpoint_ok(url: &str) -> bool {
    url.starts_with("https://")
        || url.starts_with("http://127.0.0.1")
        || url.starts_with("http://localhost")
}

impl Client {
    pub fn new(endpoint: &str) -> Result<Self> {
        let base = endpoint.trim_end_matches('/').to_string();
        if !endpoint_ok(&base) {
            bail!("endpoint must be https (got {endpoint})");
        }
        let agent = ureq::AgentBuilder::new()
            .redirects(0) // never follow a redirect to another host
            .timeout_connect(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .user_agent("breadbug")
            .build();
        Ok(Self { base, agent })
    }

    fn read_body(resp: ureq::Response) -> String {
        let mut s = String::new();
        let _ = resp
            .into_reader()
            .take(MAX_RESPONSE_BYTES)
            .read_to_string(&mut s);
        s
    }

    /// With redirects disabled a 3xx comes back as `Ok`; it is a refusal.
    fn ensure_success(resp: ureq::Response) -> Result<ureq::Response, SendError> {
        if (200..300).contains(&resp.status()) {
            Ok(resp)
        } else {
            Err(SendError::Rejected(format!(
                "unexpected HTTP {}",
                resp.status()
            )))
        }
    }

    fn classify(e: ureq::Error) -> SendError {
        match e {
            ureq::Error::Status(code, resp) => {
                let body = Self::read_body(resp);
                let msg = serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| v.get("error").and_then(|e| e.as_str().map(String::from)))
                    .unwrap_or_else(|| format!("HTTP {code}"));
                if code == 429 || code >= 500 {
                    SendError::Retriable(msg)
                } else {
                    SendError::Rejected(msg)
                }
            }
            ureq::Error::Transport(t) => SendError::Retriable(t.to_string()),
        }
    }

    /// Send the previewed bytes. Refuses if they no longer match the hash.
    pub fn send(&self, prepared: &Prepared, install: &str) -> Result<Accepted, SendError> {
        if !prepared.verify() {
            return Err(SendError::Rejected(
                "report bytes no longer match the previewed hash".into(),
            ));
        }
        let resp = self
            .agent
            .post(&format!("{}/v1/reports", self.base))
            .set("Content-Type", "application/json")
            .set("X-Breadbug-Install", install)
            .set("X-Breadbug-Sha256", &prepared.sha256)
            .send_bytes(&prepared.bytes)
            .map_err(Self::classify)
            .and_then(Self::ensure_success)?;
        let body = Self::read_body(resp);
        let v: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| SendError::Retriable(format!("bad reply: {e}")))?;
        let receipt = v
            .get("receipt")
            .and_then(|r| r.as_str())
            .filter(|r| r.len() == 32 && r.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| SendError::Retriable("reply had no receipt".into()))?
            .to_string();
        Ok(Accepted {
            receipt,
            deletion_token: v
                .get("deletion_token")
                .and_then(|t| t.as_str())
                .map(String::from),
        })
    }

    /// Delete a sent report using its deletion token.
    pub fn delete(&self, receipt: &str, token: &str) -> Result<bool, SendError> {
        match self
            .agent
            .delete(&format!("{}/v1/reports/{receipt}", self.base))
            .set("Authorization", &format!("Bearer {token}"))
            .call()
        {
            Ok(resp) => Self::ensure_success(resp).map(|_| true),
            Err(ureq::Error::Status(404, _)) => Ok(false),
            Err(e) => Err(Self::classify(e)),
        }
    }

    /// The coarse status of a report, or `None` if the server forgot it.
    pub fn status(&self, receipt: &str) -> Result<Option<RemoteStatus>, SendError> {
        match self
            .agent
            .get(&format!("{}/v1/reports/{receipt}/status", self.base))
            .call()
        {
            Ok(resp) => {
                let resp = Self::ensure_success(resp)?;
                let v: serde_json::Value = serde_json::from_str(&Self::read_body(resp))
                    .map_err(|e| SendError::Retriable(e.to_string()))?;
                Ok(v.get("status")
                    .and_then(|s| s.as_str())
                    .map(|status| RemoteStatus {
                        status: status.chars().take(32).collect(),
                        fixed_in: v
                            .get("fixed_in")
                            .and_then(|s| s.as_str())
                            .map(|f| f.chars().take(64).collect()),
                    }))
            }
            Err(ureq::Error::Status(404, _)) => Ok(None),
            Err(e) => Err(Self::classify(e)),
        }
    }
}

// ---------------------------------------------------------------------- queue

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QueueMeta {
    pub sha256: String,
    pub app: String,
    pub queued_at: u64,
    pub attempts: u32,
    pub next_try: u64,
}

/// Reports waiting for the network. Each is two files: `<sha>.json` (the
/// exact previewed bytes) and `<sha>.meta`.
pub struct Queue {
    dir: PathBuf,
}

fn backoff(attempts: u32) -> u64 {
    (60u64.saturating_mul(1u64 << attempts.min(20))).min(MAX_BACKOFF_SECS)
}

impl Queue {
    pub fn open(state: &Path) -> Result<Self> {
        let dir = state.join("queue");
        private_dir(&dir)?;
        Ok(Self { dir })
    }

    pub fn list(&self) -> Vec<QueueMeta> {
        let mut out: Vec<QueueMeta> = fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "meta"))
            .filter_map(|e| serde_json::from_str(&fs::read_to_string(e.path()).ok()?).ok())
            .collect();
        out.sort_by_key(|m| m.queued_at);
        out
    }

    pub fn enqueue(&self, p: &Prepared, now: u64) -> Result<bool> {
        if self.list().len() >= MAX_QUEUED {
            return Ok(false);
        }
        write_private(&self.dir.join(format!("{}.json", p.sha256)), &p.bytes)?;
        let meta = QueueMeta {
            sha256: p.sha256.clone(),
            app: p.report.app.id.clone(),
            queued_at: now,
            attempts: 0,
            next_try: now + backoff(0),
        };
        write_private(
            &self.dir.join(format!("{}.meta", p.sha256)),
            serde_json::to_string(&meta)?.as_bytes(),
        )?;
        Ok(true)
    }

    pub fn cancel(&self, sha: &str) -> bool {
        if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return false;
        }
        let a = fs::remove_file(self.dir.join(format!("{sha}.json"))).is_ok();
        let b = fs::remove_file(self.dir.join(format!("{sha}.meta"))).is_ok();
        a || b
    }

    pub fn clear(&self) -> usize {
        self.list()
            .iter()
            .filter(|m| self.cancel(&m.sha256))
            .count()
    }

    /// Try every queued report whose backoff has elapsed (or all, with
    /// `force`). Reports the server rejects are dropped; retriable failures
    /// back off. Returns `(sent, still_queued, dropped)`.
    pub fn flush(
        &self,
        client: &Client,
        history: &History,
        install: &str,
        now: u64,
        force: bool,
    ) -> Result<(usize, usize, usize)> {
        let (mut sent, mut dropped) = (0, 0);
        for mut meta in self.list() {
            if !force && meta.next_try > now {
                continue;
            }
            let bytes = match fs::read(self.dir.join(format!("{}.json", meta.sha256))) {
                Ok(b) => b,
                Err(_) => {
                    self.cancel(&meta.sha256);
                    dropped += 1;
                    continue;
                }
            };
            let report = serde_json::from_slice(&bytes)
                .map_err(|e| anyhow!("queued report unreadable: {e}"));
            let Ok(report) = report else {
                self.cancel(&meta.sha256);
                dropped += 1;
                continue;
            };
            let prepared = Prepared {
                sha256: meta.sha256.clone(),
                report,
                bytes,
            };
            match client.send(&prepared, install) {
                Ok(acc) => {
                    history.append(&HistoryEntry {
                        t: now,
                        kind: HistoryKind::Sent,
                        app: meta.app.clone(),
                        sha256: meta.sha256.clone(),
                        receipt: Some(acc.receipt),
                        deletion_token: acc.deletion_token,
                        path: None,
                        status: Some("received".into()),
                        fixed_in: None,
                        told_fixed: false,
                    })?;
                    self.cancel(&meta.sha256);
                    sent += 1;
                }
                Err(SendError::Rejected(_)) => {
                    self.cancel(&meta.sha256);
                    dropped += 1;
                }
                Err(SendError::Retriable(_)) => {
                    meta.attempts += 1;
                    meta.next_try = now + backoff(meta.attempts);
                    write_private(
                        &self.dir.join(format!("{}.meta", meta.sha256)),
                        serde_json::to_string(&meta)?.as_bytes(),
                    )?;
                }
            }
        }
        Ok((sent, self.list().len(), dropped))
    }
}

#[cfg(test)]
mod tests;
