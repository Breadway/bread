//! `bread hooks install git` — installs small, non-blocking git hooks that
//! emit normalized events (via the `bread-emit` fire-and-forget binary) on
//! commit and branch-change activity.
//!
//! Design constraints this module is built around:
//!
//! - Never touch a hook file bread doesn't own. Frameworks like Husky or the
//!   `pre-commit` tool, or a developer's own scripts, commonly already
//!   occupy `post-commit` / `post-checkout` / `post-merge`. We only ever
//!   overwrite a hook file if it already carries our marker comment (meaning
//!   we wrote it on a previous install); otherwise we skip it and tell the
//!   user exactly what to add by hand.
//! - Never make git itself slower or block a commit/checkout/merge because
//!   breadd is slow or down. The installed scripts background `bread-emit`
//!   and unconditionally exit 0.
//! - Respect `core.hooksPath`. If the user has repointed hooks elsewhere, we
//!   do not silently write into `.git/hooks` where nothing will ever run
//!   them — see [`install_git`] for the exact behavior.

use anyhow::{bail, Context, Result};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Distinctive marker comment written as the second line of every hook
/// script bread installs. Its presence is how we tell "a hook we installed
/// previously, safe to overwrite" apart from "someone else's hook, hands
/// off." Keep this stable across versions — changing it would make bread
/// think its own previously-installed hooks belong to someone else.
pub const MARKER: &str = "# bread-managed-hook";

/// The three git hooks bread installs, in a stable order for display.
const HOOK_NAMES: [&str; 3] = ["post-commit", "post-checkout", "post-merge"];

/// Outcome of attempting to install a single hook file.
#[derive(Debug, PartialEq, Eq)]
enum HookOutcome {
    Installed,
    Skipped,
}

/// Install bread's git hooks (`post-commit`, `post-checkout`, `post-merge`)
/// into the current working directory's git repository.
///
/// This only ever touches the repo rooted at the current directory (via
/// `git rev-parse`, which correctly follows worktrees/submodules to the
/// real git dir) — never a global `core.hooksPath`, never other repos.
pub fn install_git() -> Result<()> {
    let git_dir = git_dir()?;
    let toplevel = show_toplevel()?;

    if let Some(configured) = hooks_path_override()? {
        print_hooks_path_warning(&configured);
        bail!(
            "bread: refusing to install into '{}/hooks' while core.hooksPath is set to '{}'",
            git_dir.display(),
            configured
        );
    }

    let hooks_dir = git_dir.join("hooks");
    fs::create_dir_all(&hooks_dir)
        .with_context(|| format!("failed to create {}", hooks_dir.display()))?;

    let mut installed = Vec::new();
    let mut skipped = Vec::new();

    for &name in HOOK_NAMES.iter() {
        let path = hooks_dir.join(name);
        let script = hook_script(name);
        match install_one_hook(&path, &script)? {
            HookOutcome::Installed => installed.push(path),
            HookOutcome::Skipped => skipped.push(path),
        }
    }

    print_summary(&toplevel, &installed, &skipped);
    Ok(())
}

/// Install (or skip) a single hook file at `path` with contents `script`.
///
/// Never overwrites an existing file unless it already carries our marker.
fn install_one_hook(path: &Path, script: &str) -> Result<HookOutcome> {
    if path.exists() {
        let existing = fs::read_to_string(path)
            .with_context(|| format!("failed to read existing hook {}", path.display()))?;
        if !is_bread_managed(&existing) {
            eprintln!(
                "bread: '{}' already exists and was not installed by bread — leaving it \
                 untouched.\n  To also emit bread events from it, add this line to the end \
                 of the existing script:\n\n    {}\n",
                path.display(),
                emit_line_for(path.file_name().and_then(|n| n.to_str()).unwrap_or(""))
            );
            return Ok(HookOutcome::Skipped);
        }
        // It's ours from a previous install — safe to overwrite.
    }

    fs::write(path, script).with_context(|| format!("failed to write hook {}", path.display()))?;
    let mut perms = fs::metadata(path)
        .with_context(|| format!("failed to stat {}", path.display()))?
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms)
        .with_context(|| format!("failed to set permissions on {}", path.display()))?;

    Ok(HookOutcome::Installed)
}

/// Whether `contents` was written by a previous bread install (contains the
/// marker comment anywhere in the file).
fn is_bread_managed(contents: &str) -> bool {
    contents.lines().any(|line| line.trim() == MARKER)
}

/// The bare `bread-emit` invocation for a branch checkout, with no guard —
/// callers that already run inside a `[ "$3" = "1" ]` check (our own
/// generated hook script) use this directly.
fn branch_changed_emit_line() -> String {
    "bread-emit bread.git.branch.changed --source git --kind branch.changed --data \
     \"{\\\"repo\\\":\\\"$(git rev-parse --show-toplevel)\\\",\\\"branch\\\":\\\"$(git rev-parse --abbrev-ref HEAD)\\\",\\\"previous_ref\\\":\\\"$1\\\"}\" >/dev/null 2>&1 &"
        .to_string()
}

/// The single `bread-emit` invocation line appropriate for hook `name`,
/// suggested to users who already have their own script at that hook (so it
/// must stand alone, including its own guard where relevant).
fn emit_line_for(name: &str) -> String {
    match name {
        "post-checkout" => format!("[ \"$3\" = \"1\" ] && {}", branch_changed_emit_line()),
        _ => commit_created_emit_line(),
    }
}

/// The `bread-emit` invocation shared by `post-commit` and `post-merge`
/// (both are "HEAD moved to a new commit" signals).
fn commit_created_emit_line() -> String {
    "bread-emit bread.git.commit.created --source git --kind commit.created --data \
     \"{\\\"repo\\\":\\\"$(git rev-parse --show-toplevel)\\\",\\\"sha\\\":\\\"$(git rev-parse HEAD)\\\",\\\"branch\\\":\\\"$(git rev-parse --abbrev-ref HEAD)\\\",\\\"message\\\":\\\"$(git log -1 --pretty=%s | sed 's/\"/\\\\\\\\\"/g')\\\"}\" >/dev/null 2>&1 &"
        .to_string()
}

/// Build the full contents of the hook script for hook `name`.
///
/// Every script: starts with the marker (so future installs recognize it as
/// ours), backgrounds the `bread-emit` call so a slow/down daemon can never
/// delay the git operation, and unconditionally exits 0 so bread being
/// unavailable can never fail a `git commit`/`checkout`/`merge` for the user.
fn hook_script(name: &str) -> String {
    match name {
        "post-commit" => format!(
            "#!/bin/sh\n{marker}\n# Emits bread.git.commit.created on every commit. Backgrounded and\n\
             # always exits 0 so bread can never slow down or block `git commit`.\n\
             {emit}\nexit 0\n",
            marker = MARKER,
            emit = commit_created_emit_line(),
        ),
        "post-checkout" => format!(
            "#!/bin/sh\n{marker}\n# git passes: $1=previous HEAD, $2=new HEAD, $3=1 if a branch\n\
             # checkout (0 for a plain file checkout). Only emit on real branch\n\
             # switches. previous_branch is not resolvable from a ref alone here,\n\
             # so we report the previous HEAD's raw SHA ($1) as previous_ref instead\n\
             # of a branch name.\n\
             if [ \"$3\" = \"1\" ]; then\n  {emit}\nfi\nexit 0\n",
            marker = MARKER,
            emit = branch_changed_emit_line(),
        ),
        "post-merge" => format!(
            "#!/bin/sh\n{marker}\n# A merge moves HEAD to a new commit, same as post-commit; emit the\n\
             # same bread.git.commit.created shape so a merge (fast-forward or not)\n\
             # also surfaces as a commit-created event. Backgrounded and always\n\
             # exits 0 so bread can never slow down or block `git merge`.\n\
             {emit}\nexit 0\n",
            marker = MARKER,
            emit = commit_created_emit_line(),
        ),
        other => unreachable!("unknown hook name: {other}"),
    }
}

/// `git rev-parse --git-dir`, resolved to an absolute path. This is the
/// correct git directory even inside worktrees or submodules (unlike
/// hardcoding `.git`).
fn git_dir() -> Result<PathBuf> {
    let out = run_git(&["rev-parse", "--git-dir"]).context(
        "bread: this does not look like a git repository. Run 'bread hooks install git' from \
         inside a git work tree.",
    )?;
    let raw = PathBuf::from(out);
    if raw.is_absolute() {
        Ok(raw)
    } else {
        // `--git-dir` is often relative to CWD (e.g. ".git"); resolve it.
        std::env::current_dir()
            .map(|cwd| cwd.join(raw))
            .context("failed to resolve current directory")
    }
}

/// `git rev-parse --show-toplevel` — the repo root, used only for display.
fn show_toplevel() -> Result<PathBuf> {
    run_git(&["rev-parse", "--show-toplevel"])
        .map(PathBuf::from)
        .context(
            "bread: this does not look like a git repository. Run 'bread hooks install git' \
             from inside a git work tree.",
        )
}

/// `git config --get core.hooksPath`, if set to something non-default.
/// Returns `Ok(None)` when unset (the common case).
fn hooks_path_override() -> Result<Option<String>> {
    let output = Command::new("git")
        .args(["config", "--get", "core.hooksPath"])
        .output()
        .context("failed to run 'git config --get core.hooksPath' (is git installed?)")?;

    if !output.status.success() {
        // Exit code 1 from `git config --get` means "key not set" — that's
        // the normal, expected case, not an error.
        return Ok(None);
    }

    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() {
        Ok(None)
    } else {
        Ok(Some(value))
    }
}

fn print_hooks_path_warning(configured: &str) {
    eprintln!(
        "bread: this repo has 'core.hooksPath' set to '{configured}', so hooks placed in \
         '.git/hooks' will never run.\n\
         bread will not silently write into '.git/hooks' where they'd be dead code, and it \
         will not write into your configured hooksPath without being asked to.\n\n\
         To proceed, either:\n\
         \x20 - point core.hooksPath back at the default: git config --unset core.hooksPath\n\
         \x20   (or set it explicitly to .git/hooks), then re-run this command; or\n\
         \x20 - install the three hook scripts into '{configured}' yourself (see \
         `bread hooks install git --help` for the exact script contents this command \
         would otherwise write).\n"
    );
}

fn print_summary(toplevel: &Path, installed: &[PathBuf], skipped: &[PathBuf]) {
    println!("bread: git hooks for {}", toplevel.display());
    if installed.is_empty() {
        println!("  installed: (none)");
    } else {
        println!("  installed:");
        for path in installed {
            println!("    {}", path.display());
        }
    }
    if !skipped.is_empty() {
        println!("  skipped (already exist, not bread-managed):");
        for path in skipped {
            println!("    {}", path.display());
        }
    }
}

/// Run `git <args>` in the current directory and return trimmed stdout.
/// Fails if git is missing or the command exits non-zero.
fn run_git(args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .output()
        .with_context(|| format!("failed to run 'git {}' (is git installed?)", args.join(" ")))?;

    if !output.status.success() {
        bail!("'git {}' failed", args.join(" "));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_own_marker() {
        let script = format!("#!/bin/sh\n{MARKER}\necho hi\n");
        assert!(is_bread_managed(&script));
    }

    #[test]
    fn does_not_falsely_detect_marker() {
        let script = "#!/bin/sh\n# some other tool's hook\necho hi\n";
        assert!(!is_bread_managed(script));
    }

    #[test]
    fn marker_must_match_whole_trimmed_line() {
        // A substring match would be a false positive risk; require the
        // trimmed line to equal the marker exactly.
        let script = "#!/bin/sh\n# this mentions bread-managed-hook in passing\n";
        assert!(!is_bread_managed(script));
    }

    #[test]
    fn empty_file_is_not_managed() {
        assert!(!is_bread_managed(""));
    }

    #[test]
    fn all_hook_scripts_start_with_shebang_and_marker() {
        for name in HOOK_NAMES {
            let script = hook_script(name);
            let mut lines = script.lines();
            assert_eq!(lines.next(), Some("#!/bin/sh"));
            assert_eq!(lines.next(), Some(MARKER));
        }
    }

    #[test]
    fn all_hook_scripts_exit_0_unconditionally() {
        for name in HOOK_NAMES {
            let script = hook_script(name);
            assert!(
                script.trim_end().ends_with("exit 0"),
                "hook {name} does not unconditionally exit 0"
            );
        }
    }

    #[test]
    fn post_commit_and_post_merge_emit_commit_created() {
        for name in ["post-commit", "post-merge"] {
            let script = hook_script(name);
            assert!(script.contains("bread.git.commit.created"));
            assert!(script.contains("--kind commit.created"));
            // Must be backgrounded so a slow/down daemon can't block git.
            assert!(script.contains("&\nexit 0") || script.contains(" &\n"));
        }
    }

    #[test]
    fn post_checkout_only_emits_on_branch_checkout() {
        let script = hook_script("post-checkout");
        assert!(script.contains("bread.git.branch.changed"));
        assert!(script.contains("--kind branch.changed"));
        assert!(script.contains("\"$3\" = \"1\""));
    }

    #[test]
    fn hook_script_rejects_unknown_name() {
        let result = std::panic::catch_unwind(|| hook_script("pre-push"));
        assert!(result.is_err());
    }
}
