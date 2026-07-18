//! Polls a configured set of project roots for git dirty/clean transitions
//! and ahead/behind-upstream changes.
//!
//! Scope note: this adapter owns exactly two event families —
//! `state.dirty`/`state.clean` and `ahead_behind.changed`. It deliberately
//! does **not** emit anything for HEAD changes as such (commits, checkouts,
//! branch switches), even though `git status`/`rev-list` are re-run against
//! every tracked repo on every tick and will absolutely see those
//! transitions too. A separate, hook-based path (`post-commit`/
//! `post-checkout` invoking a CLI tool) owns `bread.git.commit.created` /
//! `bread.git.branch.changed`; if this poller also emitted on the same
//! transitions, both paths would fire for one real-world event. (An earlier
//! version of this file tried to skip the subprocess check on ticks where
//! `.git/HEAD`/`.git/index` hadn't changed mtime, as an optimization — that
//! silently broke the primary use case, since a plain working-tree edit
//! never touches either file. Every repo is checked every tick now; see the
//! comment at the top of the poll loop below.)
//!
//! Structurally this mirrors `power.rs`: a plain `tokio::time::interval`
//! poll loop with no external socket, looping forever and relying on the
//! supervisor's outer `tokio::select!` (in `Manager::spawn_adapter`) against
//! `shutdown_rx` to cancel the future — this adapter does not check
//! `tx.is_closed()` itself, and channel-send failures are propagated with
//! `?` exactly as `power.rs` does.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use bread_shared::{expand_path, now_unix_ms, AdapterSource, RawEvent};
use serde_json::json;
use tokio::process::Command;
use tokio::sync::{mpsc, Semaphore};
use tokio::time::{interval, Duration};
use tracing::{debug, warn};

use crate::adapters::Adapter;

/// Default poll interval: frequent enough to feel responsive for a dirty/
/// clean or ahead/behind indicator, infrequent enough that idle repos cost
/// nothing beyond a couple of `stat(2)` calls per tick.
const DEFAULT_POLL_INTERVAL_SECS: u64 = 2;

/// Concurrency cap: caps how many `git` subprocesses may be running at once
/// across all repos that changed in a given tick. A `Semaphore` is used
/// (rather than fully sequential processing) because typical dev machines
/// have many idle repos under e.g. `~/Projects/*`, and processing a burst of
/// simultaneously-touched repos (e.g. right after a `git fetch --all` across
/// a monorepo forest, or a batch checkout script) one at a time would add
/// unnecessary tail latency; 4 keeps subprocess fan-out modest without
/// meaningfully serializing typical ticks (where usually 0-1 repos changed).
const MAX_CONCURRENT_GIT_OPS: usize = 4;

/// Polls a set of project roots for git dirty/clean and ahead/behind-upstream
/// transitions.
///
/// Construct with [`GitAdapter::new`], passing root path patterns such as
/// `["~/Projects/*"]`. Patterns are expanded via [`bread_shared::expand_path`]
/// (for a leading `~`) and, if the final path segment is a literal `*`,
/// glob-expanded one level deep via `std::fs::read_dir` on the parent
/// directory (no glob crate dependency; only a single trailing `*` segment
/// is supported, matching the spec this adapter was built against).
#[derive(Clone)]
pub struct GitAdapter {
    root_patterns: Vec<String>,
    poll_interval: Duration,
}

impl GitAdapter {
    /// Uses [`DEFAULT_POLL_INTERVAL_SECS`].
    pub fn new(root_patterns: Vec<String>) -> Self {
        Self::with_interval(
            root_patterns,
            Duration::from_secs(DEFAULT_POLL_INTERVAL_SECS),
        )
    }

    /// Same as [`GitAdapter::new`] but with an explicit poll interval —
    /// primarily for tests, but also available if a future config key wants
    /// to expose it.
    pub fn with_interval(root_patterns: Vec<String>, poll_interval: Duration) -> Self {
        Self {
            root_patterns,
            poll_interval,
        }
    }
}

#[async_trait]
impl Adapter for GitAdapter {
    fn name(&self) -> &'static str {
        "git"
    }

    async fn run(&self, tx: mpsc::Sender<RawEvent>) -> Result<()> {
        debug!("git adapter started");

        let mut tracks = discover_repos(&self.root_patterns);
        if tracks.is_empty() {
            debug!("git adapter: no git repositories found under configured roots");
        }

        let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_GIT_OPS));
        let mut ticker = interval(self.poll_interval);

        loop {
            ticker.tick().await;

            // Every tracked repo is checked every tick. An earlier version of
            // this gated the check on `.git/HEAD`/`.git/index` mtime changing
            // first, but that misses the single most common transition this
            // adapter exists to report: plain working-tree edits (creating,
            // editing, or deleting a file) make `git status --porcelain`
            // dirty without ever touching HEAD or the index, so that gate
            // silently never fired for it. Subprocess fan-out is still
            // bounded by the semaphore below, and `state.dirty`/`state.clean`/
            // `ahead_behind.changed` are only actually emitted on a real
            // transition (see the apply pass), so an unchanged repo costs a
            // `git status --porcelain` (and occasionally `rev-list`) per
            // tick, not an emitted event.
            let needs_check: Vec<usize> = (0..tracks.len()).collect();

            // --- Check pass: bounded-concurrency git subprocesses. ---
            let mut handles = Vec::with_capacity(needs_check.len());
            for idx in needs_check {
                let repo_path = tracks[idx].path.clone();
                let permit = semaphore.clone();
                handles.push(tokio::spawn(async move {
                    let _permit = permit.acquire_owned().await;
                    let dirty = check_dirty(&repo_path).await;
                    let ahead_behind = check_ahead_behind(&repo_path).await;
                    (idx, dirty, ahead_behind)
                }));
            }

            // --- Apply pass: sequential, so per-repo last-known state
            // doesn't need its own lock. ---
            for handle in handles {
                let (idx, dirty_result, ahead_behind_result) = match handle.await {
                    Ok(result) => result,
                    Err(e) => {
                        warn!("git adapter: check task failed to join: {e}");
                        continue;
                    }
                };

                let track = &mut tracks[idx];

                match dirty_result {
                    Ok(dirty) => {
                        if track.last_dirty != Some(dirty) {
                            let kind = if dirty { "state.dirty" } else { "state.clean" };
                            track.last_dirty = Some(dirty);
                            tx.send(RawEvent {
                                source: AdapterSource::Git,
                                kind: kind.to_string(),
                                payload: json!({ "repo": track.path.to_string_lossy() }),
                                timestamp: now_unix_ms(),
                            })
                            .await?;
                        }
                    }
                    Err(e) => warn!(
                        "git adapter: status check failed for {}: {e}",
                        track.path.display()
                    ),
                }

                match ahead_behind_result {
                    Ok(Some((branch, ahead, behind))) => {
                        if track.last_ahead_behind != Some((ahead, behind)) {
                            track.last_ahead_behind = Some((ahead, behind));
                            tx.send(RawEvent {
                                source: AdapterSource::Git,
                                kind: "ahead_behind.changed".to_string(),
                                payload: json!({
                                    "repo": track.path.to_string_lossy(),
                                    "ahead": ahead,
                                    "behind": behind,
                                    "branch": branch,
                                }),
                                timestamp: now_unix_ms(),
                            })
                            .await?;
                        }
                    }
                    // No upstream configured for the current branch — not an
                    // error, just nothing to report for this repo this tick.
                    Ok(None) => {}
                    Err(e) => debug!(
                        "git adapter: ahead/behind check failed for {}: {e}",
                        track.path.display()
                    ),
                }
            }
        }
    }
}

/// Per-repo tracking state carried between poll ticks.
struct RepoTrack {
    /// Worktree root — what gets passed to `git -C <path>` and reported in
    /// event payloads.
    path: PathBuf,
    last_dirty: Option<bool>,
    last_ahead_behind: Option<(u32, u32)>,
}

impl RepoTrack {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            last_dirty: None,
            last_ahead_behind: None,
        }
    }
}

/// Expands configured root patterns and resolves each concrete root to a
/// [`RepoTrack`] if (and only if) it's a recognizable git checkout. Roots
/// that aren't git repos are skipped with a debug log, not an error — the
/// configured glob is expected to sweep up non-repo directories routinely
/// (e.g. `~/Projects/*` catching a README-only folder).
fn discover_repos(root_patterns: &[String]) -> Vec<RepoTrack> {
    let mut tracks = Vec::new();
    for root in expand_roots(root_patterns) {
        match resolve_git_dir(&root) {
            Some(_git_dir) => tracks.push(RepoTrack::new(root)),
            None => debug!(
                "git adapter: {} is not a git repository, skipping",
                root.display()
            ),
        }
    }
    tracks
}

/// Expands `~` (via `bread_shared::expand_path`) and a single trailing `*`
/// path segment (via a one-level `std::fs::read_dir` on the parent
/// directory — no glob crate). Patterns without a trailing `*` are used
/// literally. Only directories are kept when expanding a `*`.
fn expand_roots(patterns: &[String]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for pattern in patterns {
        let expanded = expand_path(pattern);
        if expanded.file_name().map(|n| n == "*").unwrap_or(false) {
            let parent = expanded.parent().unwrap_or_else(|| Path::new("."));
            match std::fs::read_dir(parent) {
                Ok(entries) => {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.is_dir() {
                            out.push(path);
                        }
                    }
                }
                Err(e) => debug!("git adapter: cannot glob {}: {e}", parent.display()),
            }
        } else {
            out.push(expanded);
        }
    }
    out
}

/// Resolves `<root>/.git` to the actual git directory to use for `HEAD`/
/// `index` stats, handling both shapes:
/// - a plain directory (the common case) — used as-is;
/// - a `.git` *file* containing `gitdir: <path>` (worktrees and submodules)
///   — the pointer is read and resolved (relative to `root` if not
///   absolute), and canonicalized on a best-effort basis.
///
/// Chose to fully resolve the worktree/submodule case rather than skip it,
/// since it's a common setup (git worktrees especially) and the extra
/// parsing is small. Returns `None` if `<root>/.git` doesn't exist or is
/// some other unrecognized shape.
fn resolve_git_dir(root: &Path) -> Option<PathBuf> {
    let dotgit = root.join(".git");
    let meta = std::fs::symlink_metadata(&dotgit).ok()?;

    if meta.is_dir() {
        return Some(dotgit);
    }

    if meta.is_file() {
        let content = std::fs::read_to_string(&dotgit).ok()?;
        let gitdir_str = parse_gitdir_file(&content)?;
        let gitdir_path = PathBuf::from(gitdir_str);
        let resolved = if gitdir_path.is_absolute() {
            gitdir_path
        } else {
            root.join(gitdir_path)
        };
        return Some(std::fs::canonicalize(&resolved).unwrap_or(resolved));
    }

    None
}

/// Parses the contents of a worktree/submodule `.git` file, e.g.
/// `gitdir: /home/user/Projects/repo/.git/worktrees/feature\n`, returning
/// the path portion.
fn parse_gitdir_file(content: &str) -> Option<&str> {
    content
        .lines()
        .find_map(|line| line.trim().strip_prefix("gitdir:"))
        .map(|rest| rest.trim())
}

/// Runs `git -C <repo> status --porcelain` and reports whether the repo is
/// dirty (any output) or clean (empty output).
async fn check_dirty(repo: &Path) -> Result<bool> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["status", "--porcelain"])
        .output()
        .await
        .map_err(|e| anyhow!("failed to spawn git status: {e}"))?;

    if !output.status.success() {
        return Err(anyhow!(
            "git status exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(!stdout.trim().is_empty())
}

/// Runs `git -C <repo> rev-list --left-right --count HEAD...@{upstream}` and
/// `git -C <repo> rev-parse --abbrev-ref HEAD`, returning
/// `Some((branch, ahead, behind))`. Returns `Ok(None)` (not an error) when
/// there's no upstream configured for the current branch, since that's the
/// expected/common state for plenty of repos, not a failure.
async fn check_ahead_behind(repo: &Path) -> Result<Option<(String, u32, u32)>> {
    let rev_list = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-list", "--left-right", "--count", "HEAD...@{upstream}"])
        .output()
        .await
        .map_err(|e| anyhow!("failed to spawn git rev-list: {e}"))?;

    if !rev_list.status.success() {
        // Most commonly: "no upstream configured for branch". Treat any
        // non-zero exit here as "nothing to report", not an adapter error.
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&rev_list.stdout);
    let Some((ahead, behind)) = parse_ahead_behind(&stdout) else {
        return Err(anyhow!("unexpected git rev-list output: {stdout:?}"));
    };

    let branch_output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .await
        .map_err(|e| anyhow!("failed to spawn git rev-parse: {e}"))?;

    let branch = if branch_output.status.success() {
        String::from_utf8_lossy(&branch_output.stdout)
            .trim()
            .to_string()
    } else {
        "HEAD".to_string()
    };

    Ok(Some((branch, ahead, behind)))
}

/// Parses `git rev-list --left-right --count HEAD...@{upstream}` output.
///
/// With `HEAD...@{upstream}`, `HEAD` is the left side and `@{upstream}` is
/// the right side, so `--left-right --count` prints "<left-only-count>
/// <right-only-count>" — i.e. "<ahead> <behind>", in that order,
/// whitespace-separated (typically a single tab).
fn parse_ahead_behind(output: &str) -> Option<(u32, u32)> {
    let mut parts = output.split_whitespace();
    let ahead = parts.next()?.parse::<u32>().ok()?;
    let behind = parts.next()?.parse::<u32>().ok()?;
    Some((ahead, behind))
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse_ahead_behind ---

    #[test]
    fn parse_ahead_behind_parses_tab_separated_counts() {
        assert_eq!(parse_ahead_behind("3\t2\n"), Some((3, 2)));
    }

    #[test]
    fn parse_ahead_behind_parses_space_separated_counts() {
        assert_eq!(parse_ahead_behind("0 5"), Some((0, 5)));
    }

    #[test]
    fn parse_ahead_behind_handles_zero_zero() {
        assert_eq!(parse_ahead_behind("0\t0"), Some((0, 0)));
    }

    #[test]
    fn parse_ahead_behind_rejects_malformed_output() {
        assert_eq!(parse_ahead_behind(""), None);
        assert_eq!(parse_ahead_behind("only-one"), None);
        assert_eq!(parse_ahead_behind("not a number\t2"), None);
    }

    // --- parse_gitdir_file ---

    #[test]
    fn parse_gitdir_file_extracts_path() {
        assert_eq!(
            parse_gitdir_file("gitdir: /home/user/repo/.git/worktrees/feature\n"),
            Some("/home/user/repo/.git/worktrees/feature")
        );
    }

    #[test]
    fn parse_gitdir_file_trims_whitespace() {
        assert_eq!(
            parse_gitdir_file("gitdir:   ../.git/modules/sub  \n"),
            Some("../.git/modules/sub")
        );
    }

    #[test]
    fn parse_gitdir_file_rejects_unrecognized_content() {
        assert_eq!(parse_gitdir_file("not a gitdir pointer\n"), None);
        assert_eq!(parse_gitdir_file(""), None);
    }

    // --- expand_roots ---

    #[test]
    fn expand_roots_uses_literal_path_without_trailing_star() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("myrepo");
        std::fs::create_dir(&repo).unwrap();

        let pattern = repo.to_string_lossy().to_string();
        let roots = expand_roots(&[pattern]);

        assert_eq!(roots, vec![repo]);
    }

    #[test]
    fn expand_roots_globs_single_trailing_star() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo_a = dir.path().join("repo-a");
        let repo_b = dir.path().join("repo-b");
        std::fs::create_dir(&repo_a).unwrap();
        std::fs::create_dir(&repo_b).unwrap();
        // A stray file alongside the directories must not be treated as a root.
        std::fs::write(dir.path().join("not-a-dir.txt"), b"hi").unwrap();

        let pattern = format!("{}/*", dir.path().to_string_lossy());
        let mut roots = expand_roots(&[pattern]);
        roots.sort();

        let mut expected = vec![repo_a, repo_b];
        expected.sort();
        assert_eq!(roots, expected);
    }

    #[test]
    fn expand_roots_skips_unreadable_glob_parent_without_panicking() {
        let pattern = "/definitely/does/not/exist/*".to_string();
        let roots = expand_roots(&[pattern]);
        assert!(roots.is_empty());
    }

    // --- resolve_git_dir ---

    #[test]
    fn resolve_git_dir_finds_standard_directory_git() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        let dotgit = repo.join(".git");
        std::fs::create_dir_all(&dotgit).unwrap();

        assert_eq!(resolve_git_dir(&repo), Some(dotgit));
    }

    #[test]
    fn resolve_git_dir_resolves_worktree_style_git_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let main_repo_gitdir = dir.path().join("main-repo").join(".git");
        std::fs::create_dir_all(&main_repo_gitdir).unwrap();

        let worktree = dir.path().join("worktree-checkout");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", main_repo_gitdir.to_string_lossy()),
        )
        .unwrap();

        let resolved = resolve_git_dir(&worktree).expect("should resolve worktree gitdir");
        assert_eq!(
            std::fs::canonicalize(&resolved).unwrap(),
            std::fs::canonicalize(&main_repo_gitdir).unwrap()
        );
    }

    #[test]
    fn resolve_git_dir_returns_none_for_non_repo() {
        let dir = tempfile::tempdir().expect("tempdir");
        let not_a_repo = dir.path().join("just-a-folder");
        std::fs::create_dir(&not_a_repo).unwrap();

        assert_eq!(resolve_git_dir(&not_a_repo), None);
    }
}
