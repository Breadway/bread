use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use bread_shared::{expand_path, now_unix_ms, AdapterSource, RawEvent};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::json;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use super::Adapter;

/// Files/directories directly inside a watched root whose presence marks it
/// as a recognizable project.
const MARKERS: [&str; 4] = [".git", "Cargo.toml", "package.json", "go.mod"];

/// Directory names that are excluded from `file.changed` noise anywhere in a
/// watched tree. `.git` and `node_modules` are fully silent; `target`,
/// `dist`, and `build` additionally get a `build_artifact.created` signal
/// when a new file appears inside them.
const EXCLUDED_DIRS: [&str; 5] = [".git", "target", "node_modules", "dist", "build"];

/// Debounce window for `file.changed` — editors frequently perform several
/// writes (temp file + rename, fsync, etc.) for a single logical save.
const DEBOUNCE_WINDOW: Duration = Duration::from_millis(300);

/// Watches a set of project-root glob patterns for filesystem activity and
/// reports project detection, plain file changes, and build-artifact
/// creation as [`RawEvent`]s.
#[derive(Clone)]
pub struct FilesystemAdapter {
    /// Raw, unexpanded root patterns as configured (e.g. `["~/Projects/*"]`).
    roots: Vec<String>,
}

impl FilesystemAdapter {
    pub fn new(roots: Vec<String>) -> Self {
        Self { roots }
    }

    /// Expand `~` and a single `*` glob segment in each configured pattern
    /// into concrete directory paths. Patterns are not required to exist on
    /// disk yet — existence is checked by the caller.
    fn resolve_roots(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for pattern in &self.roots {
            let expanded = expand_path(pattern);
            out.extend(expand_glob(&expanded));
        }
        out
    }

    /// Scan each concrete root for project markers and emit a `"detected"`
    /// event for any root that has at least one. Mirrors the
    /// `enumerate_existing` convention used by the udev/bluetooth adapters:
    /// called once before `run()`, best-effort, never fails the daemon.
    pub async fn enumerate_existing(&self, tx: &mpsc::Sender<RawEvent>) {
        for root in self.resolve_roots() {
            if !root.is_dir() {
                debug!(
                    "filesystem: root {} does not exist, skipping enumeration",
                    root.display()
                );
                continue;
            }

            let markers = detect_markers(&root);
            if markers.is_empty() {
                continue;
            }

            let _ = tx
                .send(RawEvent {
                    source: AdapterSource::Filesystem,
                    kind: "detected".to_string(),
                    payload: json!({
                        "root": root.to_string_lossy(),
                        "markers": markers,
                    }),
                    timestamp: now_unix_ms(),
                })
                .await;
        }
    }
}

#[async_trait]
impl Adapter for FilesystemAdapter {
    fn name(&self) -> &'static str {
        "filesystem"
    }

    async fn run(&self, tx: mpsc::Sender<RawEvent>) -> Result<()> {
        let mut existing_roots = Vec::new();
        for root in self.resolve_roots() {
            if root.is_dir() {
                existing_roots.push(root);
            } else {
                warn!(
                    "filesystem: configured root {} does not exist, skipping",
                    root.display()
                );
            }
        }

        if existing_roots.is_empty() {
            debug!("filesystem adapter: no existing roots to watch");
            return Ok(());
        }

        run_watch(existing_roots, tx).await
    }
}

/// Sets up a recursive `notify` watch on each root and bridges its
/// synchronous callback into the async `tx` channel via a blocking task.
///
/// Roots that fail to watch (e.g. inotify watch-limit exhaustion) are logged
/// and skipped; the adapter keeps watching whatever roots succeeded.
async fn run_watch(roots: Vec<PathBuf>, tx: mpsc::Sender<RawEvent>) -> Result<()> {
    let (std_tx, std_rx) = std_mpsc::channel::<notify::Result<Event>>();

    let mut watcher: RecommendedWatcher = notify::recommended_watcher(move |res| {
        let _ = std_tx.send(res);
    })?;

    let mut watched_roots = Vec::new();
    for root in &roots {
        match watcher.watch(root, RecursiveMode::Recursive) {
            Ok(()) => watched_roots.push(root.clone()),
            Err(e) => {
                warn!(
                    "filesystem: failed to watch {} ({e}), skipping this root",
                    root.display()
                );
            }
        }
    }

    if watched_roots.is_empty() {
        warn!("filesystem adapter: no roots could be watched, exiting");
        return Ok(());
    }

    // The blocking task owns the receiving end and the debounce table; it
    // runs until `std_rx` disconnects (watcher dropped) or `tx` is closed
    // (daemon shutting down). `watcher` is kept alive in this async fn's
    // stack across the await below so its background thread keeps feeding
    // `std_rx` for as long as this future is polled.
    tokio::task::spawn_blocking(move || {
        let mut last_seen: HashMap<PathBuf, Instant> = HashMap::new();
        while let Ok(res) = std_rx.recv() {
            let event = match res {
                Ok(event) => event,
                Err(e) => {
                    debug!("filesystem watch error: {e}");
                    continue;
                }
            };

            for path in &event.paths {
                let Some(root) = watched_roots.iter().find(|r| path.starts_with(r)) else {
                    continue;
                };

                if let Some(raw) = classify(root, path, &event.kind, &mut last_seen) {
                    if tx.blocking_send(raw).is_err() {
                        return;
                    }
                }
            }
        }
    })
    .await?;

    drop(watcher);
    Ok(())
}

/// Classifies a single notify path event into a `RawEvent`, or `None` if it
/// should be silent (inside `.git`/`node_modules`, or debounced).
fn classify(
    root: &Path,
    path: &Path,
    kind: &EventKind,
    last_seen: &mut HashMap<PathBuf, Instant>,
) -> Option<RawEvent> {
    let relative = path.strip_prefix(root).unwrap_or(path);

    match excluded_dir_component(relative) {
        Some("target" | "dist" | "build") => {
            if matches!(kind, EventKind::Create(_)) {
                Some(RawEvent {
                    source: AdapterSource::Filesystem,
                    kind: "build_artifact.created".to_string(),
                    payload: json!({
                        "path": path.to_string_lossy(),
                        "project_root": root.to_string_lossy(),
                    }),
                    timestamp: now_unix_ms(),
                })
            } else {
                None
            }
        }
        // `.git` / `node_modules`: fully silent, never emit.
        Some(_) => None,
        None => {
            let now = Instant::now();
            if let Some(last) = last_seen.get(path) {
                if now.duration_since(*last) < DEBOUNCE_WINDOW {
                    return None;
                }
            }
            last_seen.insert(path.to_path_buf(), now);

            Some(RawEvent {
                source: AdapterSource::Filesystem,
                kind: "file.changed".to_string(),
                payload: json!({
                    "path": path.to_string_lossy(),
                    "project_root": root.to_string_lossy(),
                }),
                timestamp: now_unix_ms(),
            })
        }
    }
}

/// Returns the first excluded directory name found anywhere among the
/// components of `relative`, or `None` if it isn't under any of them.
fn excluded_dir_component(relative: &Path) -> Option<&'static str> {
    for component in relative.components() {
        if let std::path::Component::Normal(name) = component {
            let name = name.to_str().unwrap_or("");
            if let Some(excluded) = EXCLUDED_DIRS.iter().find(|e| **e == name) {
                return Some(excluded);
            }
        }
    }
    None
}

/// Checks `root` for the presence of any recognized project marker
/// directly inside it (not recursively).
fn detect_markers(root: &Path) -> Vec<String> {
    MARKERS
        .iter()
        .filter(|marker| root.join(marker).exists())
        .map(|marker| marker.to_string())
        .collect()
}

/// Expands a single `*` path component (if present) into every directory
/// entry of its parent. Patterns without a `*` are returned unchanged.
/// Only one level of globbing is supported, matching the config contract
/// (e.g. `~/Projects/*`, not `~/Projects/**`).
fn expand_glob(path: &Path) -> Vec<PathBuf> {
    let components: Vec<_> = path.components().collect();
    let Some(star_idx) = components.iter().position(|c| c.as_os_str() == "*") else {
        return vec![path.to_path_buf()];
    };

    let parent: PathBuf = components[..star_idx].iter().collect();
    let suffix: PathBuf = components[star_idx + 1..].iter().collect();

    let Ok(entries) = std::fs::read_dir(&parent) else {
        debug!(
            "filesystem: cannot read {} to expand glob pattern",
            parent.display()
        );
        return Vec::new();
    };

    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|p| p.is_dir())
        .map(|p| {
            if suffix.as_os_str().is_empty() {
                p
            } else {
                p.join(&suffix)
            }
        })
        .collect();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn excluded_dir_component_finds_git_at_top_level() {
        assert_eq!(excluded_dir_component(Path::new(".git/HEAD")), Some(".git"));
    }

    #[test]
    fn excluded_dir_component_finds_target_nested_deeply() {
        assert_eq!(
            excluded_dir_component(Path::new("crates/foo/target/debug/build/out.o")),
            Some("target")
        );
    }

    #[test]
    fn excluded_dir_component_finds_node_modules() {
        assert_eq!(
            excluded_dir_component(Path::new("web/node_modules/lodash/index.js")),
            Some("node_modules")
        );
    }

    #[test]
    fn excluded_dir_component_none_for_ordinary_source_file() {
        assert_eq!(excluded_dir_component(Path::new("src/main.rs")), None);
    }

    #[test]
    fn classify_silent_under_git() {
        let mut last_seen = HashMap::new();
        let root = Path::new("/proj");
        let path = Path::new("/proj/.git/HEAD");
        let result = classify(
            root,
            path,
            &EventKind::Create(notify::event::CreateKind::File),
            &mut last_seen,
        );
        assert!(result.is_none());
    }

    #[test]
    fn classify_silent_under_node_modules() {
        let mut last_seen = HashMap::new();
        let root = Path::new("/proj");
        let path = Path::new("/proj/node_modules/foo/index.js");
        let result = classify(
            root,
            path,
            &EventKind::Modify(notify::event::ModifyKind::Any),
            &mut last_seen,
        );
        assert!(result.is_none());
    }

    #[test]
    fn classify_build_artifact_created_in_target() {
        let mut last_seen = HashMap::new();
        let root = Path::new("/proj");
        let path = Path::new("/proj/target/debug/breadd");
        let result = classify(
            root,
            path,
            &EventKind::Create(notify::event::CreateKind::File),
            &mut last_seen,
        );
        let event = result.expect("expected build_artifact.created event");
        assert_eq!(event.kind, "build_artifact.created");
        assert_eq!(event.payload["path"], json!("/proj/target/debug/breadd"));
        assert_eq!(event.payload["project_root"], json!("/proj"));
    }

    #[test]
    fn classify_silent_for_modify_in_target_not_create() {
        let mut last_seen = HashMap::new();
        let root = Path::new("/proj");
        let path = Path::new("/proj/target/debug/breadd");
        let result = classify(
            root,
            path,
            &EventKind::Modify(notify::event::ModifyKind::Data(
                notify::event::DataChange::Any,
            )),
            &mut last_seen,
        );
        assert!(result.is_none());
    }

    #[test]
    fn classify_file_changed_for_ordinary_source_file() {
        let mut last_seen = HashMap::new();
        let root = Path::new("/proj");
        let path = Path::new("/proj/src/main.rs");
        let result = classify(
            root,
            path,
            &EventKind::Modify(notify::event::ModifyKind::Any),
            &mut last_seen,
        );
        let event = result.expect("expected file.changed event");
        assert_eq!(event.kind, "file.changed");
        assert_eq!(event.payload["path"], json!("/proj/src/main.rs"));
        assert_eq!(event.payload["project_root"], json!("/proj"));
    }

    #[test]
    fn classify_debounces_rapid_repeat_events_for_same_path() {
        let mut last_seen = HashMap::new();
        let root = Path::new("/proj");
        let path = Path::new("/proj/src/main.rs");
        let kind = EventKind::Modify(notify::event::ModifyKind::Any);

        let first = classify(root, path, &kind, &mut last_seen);
        assert!(first.is_some());

        let second = classify(root, path, &kind, &mut last_seen);
        assert!(second.is_none(), "second rapid event should be debounced");
    }

    #[test]
    fn detect_markers_finds_cargo_toml() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
        let markers = detect_markers(dir.path());
        assert_eq!(markers, vec!["Cargo.toml".to_string()]);
    }

    #[test]
    fn detect_markers_finds_multiple() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join("package.json"), "{}").unwrap();
        let mut markers = detect_markers(dir.path());
        markers.sort();
        let mut expected = vec![".git".to_string(), "package.json".to_string()];
        expected.sort();
        assert_eq!(markers, expected);
    }

    #[test]
    fn detect_markers_empty_for_plain_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(detect_markers(dir.path()).is_empty());
    }

    #[test]
    fn expand_glob_returns_single_path_unchanged_without_star() {
        let path = Path::new("/home/user/Projects/bread");
        assert_eq!(expand_glob(path), vec![path.to_path_buf()]);
    }

    #[test]
    fn expand_glob_expands_star_to_subdirectories() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("alpha")).unwrap();
        fs::create_dir(dir.path().join("beta")).unwrap();
        fs::write(dir.path().join("not-a-dir.txt"), "x").unwrap();

        let pattern = dir.path().join("*");
        let mut results = expand_glob(&pattern);
        results.sort();

        let mut expected = vec![dir.path().join("alpha"), dir.path().join("beta")];
        expected.sort();
        assert_eq!(results, expected);
    }

    #[test]
    fn expand_glob_returns_empty_for_nonexistent_parent() {
        let pattern = Path::new("/definitely/does/not/exist/*");
        assert!(expand_glob(pattern).is_empty());
    }
}
