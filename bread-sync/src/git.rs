use std::path::Path;

use anyhow::{anyhow, Result};

/// Open an existing repo or initialise a new one at `path`.
pub fn init_or_open(path: &Path) -> Result<git2::Repository> {
    if path.join(".git").exists() || is_bare(path) {
        Ok(git2::Repository::open(path)?)
    } else {
        std::fs::create_dir_all(path)?;
        Ok(git2::Repository::init(path)?)
    }
}

/// Clone `url` to `path` if `path` is not already a repo, otherwise open it.
pub fn clone_or_open(url: &str, path: &Path) -> Result<git2::Repository> {
    if path.join(".git").exists() || is_bare(path) {
        return Ok(git2::Repository::open(path)?);
    }
    let mut builder = git2::build::RepoBuilder::new();
    let mut fetch_opts = git2::FetchOptions::new();
    fetch_opts.remote_callbacks(make_callbacks());
    builder.fetch_options(fetch_opts);
    std::fs::create_dir_all(path)?;
    Ok(builder.clone(url, path)?)
}

/// Stage every tracked and untracked change (equivalent to `git add -A`).
pub fn stage_all(repo: &git2::Repository) -> Result<()> {
    let mut index = repo.index()?;
    index.add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)?;
    // Remove entries for deleted files
    index.update_all(["*"].iter(), None)?;
    index.write()?;
    Ok(())
}

/// Returns `true` if the index has staged changes compared to HEAD (or repo is new).
pub fn has_changes(repo: &git2::Repository) -> Result<bool> {
    let mut index = repo.index()?;
    index.read(false)?;

    // New repo with no commits yet
    if repo.head().is_err() {
        return Ok(index.len() > 0);
    }

    let head = repo.head()?.peel_to_tree()?;
    let diff = repo.diff_tree_to_index(Some(&head), Some(&index), None)?;
    Ok(diff.deltas().count() > 0)
}

/// Commit all staged changes with `message`. Returns the new commit OID.
pub fn commit(repo: &git2::Repository, message: &str) -> Result<git2::Oid> {
    let mut index = repo.index()?;
    let tree_id = index.write_tree()?;
    let tree = repo.find_tree(tree_id)?;
    let sig = repo.signature().unwrap_or_else(|_| {
        git2::Signature::now("bread", "bread@localhost").expect("signature")
    });

    let oid = if let Ok(head) = repo.head() {
        let parent = head.peel_to_commit()?;
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[&parent])?
    } else {
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[])?
    };
    Ok(oid)
}

/// Push `branch` to `remote_name` (defaults to "origin").
pub fn push(repo: &git2::Repository, remote_name: &str, branch: &str) -> Result<()> {
    let mut remote = repo.find_remote(remote_name)?;
    let mut opts = git2::PushOptions::new();
    opts.remote_callbacks(make_callbacks());
    remote.push(
        &[&format!("refs/heads/{branch}:refs/heads/{branch}")],
        Some(&mut opts),
    )?;
    Ok(())
}

/// Fetch from `remote_name` without merging.
pub fn fetch(repo: &git2::Repository, remote_name: &str) -> Result<()> {
    let mut remote = repo.find_remote(remote_name)?;
    let mut opts = git2::FetchOptions::new();
    opts.remote_callbacks(make_callbacks());
    remote.fetch(&[] as &[&str], Some(&mut opts), None)?;
    Ok(())
}

/// Fetch and fast-forward merge from `remote_name/branch`. Errors on conflict.
pub fn pull(repo: &git2::Repository, remote_name: &str, branch: &str) -> Result<()> {
    fetch(repo, remote_name)?;

    let fetch_head = repo
        .find_reference(&format!("refs/remotes/{remote_name}/{branch}"))
        .map_err(|_| anyhow!("remote branch {remote_name}/{branch} not found after fetch"))?;
    let fetch_commit = repo.reference_to_annotated_commit(&fetch_head)?;

    let analysis = repo.merge_analysis(&[&fetch_commit])?;
    if analysis.0.is_up_to_date() {
        return Ok(());
    }
    if !analysis.0.is_fast_forward() {
        return Err(anyhow!(
            "sync conflict — resolve manually in {}",
            repo.workdir()
                .unwrap_or_else(|| Path::new("?"))
                .display()
        ));
    }

    // Fast-forward: update HEAD and checkout
    let head_ref = repo.find_reference("HEAD")?;
    let resolved = head_ref.resolve()?;
    let refname = resolved.name().unwrap_or("HEAD").to_string();
    repo.find_reference(&refname)?
        .set_target(fetch_commit.id(), "fast-forward")?;
    repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))?;
    Ok(())
}

/// Add a remote named `name` pointing at `url`, or update it if it already exists.
pub fn set_remote(repo: &git2::Repository, name: &str, url: &str) -> Result<()> {
    if repo.find_remote(name).is_ok() {
        repo.remote_set_url(name, url)?;
    } else {
        repo.remote(name, url)?;
    }
    Ok(())
}

/// Return working-tree diff against HEAD as a unified diff string.
pub fn diff_workdir(repo: &git2::Repository) -> Result<String> {
    let mut buf = Vec::new();
    if let Ok(head_tree) = repo.head().and_then(|h| h.peel_to_tree()) {
        let diff = repo.diff_tree_to_workdir_with_index(Some(&head_tree), None)?;
        diff.print(git2::DiffFormat::Patch, |_, _, line| {
            buf.extend_from_slice(line.content());
            true
        })?;
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Return diff between HEAD and `remote/branch` as a unified diff string.
pub fn diff_remote(repo: &git2::Repository, remote_name: &str, branch: &str) -> Result<String> {
    let remote_ref = format!("refs/remotes/{remote_name}/{branch}");
    let remote_tree = repo
        .find_reference(&remote_ref)
        .map_err(|_| anyhow!("remote ref {remote_ref} not found — try fetching first"))?
        .peel_to_tree()?;
    let local_tree = repo.head()?.peel_to_tree()?;
    let diff = repo.diff_tree_to_tree(Some(&local_tree), Some(&remote_tree), None)?;
    let mut buf = Vec::new();
    diff.print(git2::DiffFormat::Patch, |_, _, line| {
        buf.extend_from_slice(line.content());
        true
    })?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Return a list of `(status_char, path)` for the working tree.
pub fn status_lines(repo: &git2::Repository) -> Result<Vec<(char, String)>> {
    let statuses = repo.statuses(None)?;
    let mut out = Vec::new();
    for entry in statuses.iter() {
        let path = entry.path().unwrap_or("?").to_string();
        let flag = entry.status();
        let ch = if flag.contains(git2::Status::INDEX_NEW) || flag.contains(git2::Status::WT_NEW) {
            'A'
        } else if flag.contains(git2::Status::INDEX_DELETED) || flag.contains(git2::Status::WT_DELETED) {
            'D'
        } else {
            'M'
        };
        out.push((ch, path));
    }
    Ok(out)
}

/// Returns true if the local HEAD is behind the remote.
pub fn remote_has_changes(repo: &git2::Repository, remote_name: &str, branch: &str) -> bool {
    let remote_ref = format!("refs/remotes/{remote_name}/{branch}");
    let Ok(remote_ref) = repo.find_reference(&remote_ref) else {
        return false;
    };
    let Ok(remote_commit) = remote_ref.peel_to_commit() else {
        return false;
    };
    let Ok(local_commit) = repo.head().and_then(|h| h.peel_to_commit()) else {
        return false;
    };
    remote_commit.id() != local_commit.id()
}

/// Timestamp of the HEAD commit (or "never").
pub fn last_commit_time(repo: &git2::Repository) -> String {
    let Ok(commit) = repo.head().and_then(|h| h.peel_to_commit()) else {
        return "never".to_string();
    };
    let ts = commit.time().seconds();
    let dt = chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0)
        .unwrap_or_else(chrono::Utc::now);
    dt.format("%Y-%m-%d %H:%M:%S").to_string()
}

fn is_bare(path: &Path) -> bool {
    path.join("HEAD").exists() && path.join("objects").exists()
}

fn make_callbacks<'a>() -> git2::RemoteCallbacks<'a> {
    let mut callbacks = git2::RemoteCallbacks::new();
    callbacks.credentials(|_url, username_from_url, allowed_types| {
        if allowed_types.contains(git2::CredentialType::SSH_KEY) {
            git2::Cred::ssh_key_from_agent(username_from_url.unwrap_or("git"))
        } else if allowed_types.contains(git2::CredentialType::DEFAULT) {
            git2::Cred::default()
        } else {
            Err(git2::Error::from_str(
                "no supported credential type (SSH agent or default)",
            ))
        }
    });
    callbacks
}
