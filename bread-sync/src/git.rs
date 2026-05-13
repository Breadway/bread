use anyhow::{Context, Result};
use git2::{
    build::CheckoutBuilder, Cred, FetchOptions, IndexAddOption, PushOptions, RemoteCallbacks,
    Repository, Signature, StatusOptions,
};
use std::path::{Path, PathBuf};

/// Wraps a git2 repository with sync-specific operations.
pub struct SyncRepo {
    repo: Repository,
    pub path: PathBuf,
}

impl SyncRepo {
    /// Open an existing repository at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        let repo = Repository::open(path)
            .with_context(|| format!("failed to open git repo at {}", path.display()))?;
        Ok(Self {
            repo,
            path: path.to_path_buf(),
        })
    }

    /// Clone `url` into `path`.
    pub fn clone_from(url: &str, path: &Path) -> Result<Self> {
        let fetch_opts = make_fetch_options();
        let mut builder = git2::build::RepoBuilder::new();
        builder.fetch_options(fetch_opts);
        let repo = builder
            .clone(url, path)
            .with_context(|| format!("failed to clone {} into {}", url, path.display()))?;
        Ok(Self {
            repo,
            path: path.to_path_buf(),
        })
    }

    /// Open the repo at `path` if it exists; otherwise clone from `url`.
    pub fn open_or_clone(url: &str, path: &Path) -> Result<Self> {
        if path.exists() {
            Self::open(path)
        } else {
            std::fs::create_dir_all(path)?;
            Self::clone_from(url, path)
        }
    }

    /// Initialize a new empty repository at `path` with `main` as the initial branch.
    pub fn init(path: &Path) -> Result<Self> {
        std::fs::create_dir_all(path)?;
        let mut opts = git2::RepositoryInitOptions::new();
        opts.initial_head("main");
        let repo = Repository::init_opts(path, &opts)
            .with_context(|| format!("failed to init git repo at {}", path.display()))?;
        Ok(Self {
            repo,
            path: path.to_path_buf(),
        })
    }

    /// Stage all changes (equivalent to `git add -A`).
    pub fn stage_all(&self) -> Result<()> {
        let mut index = self.repo.index().context("failed to get git index")?;
        index
            .add_all(["*"].iter(), IndexAddOption::DEFAULT, None)
            .context("failed to stage changes")?;
        index.write().context("failed to write git index")?;
        Ok(())
    }

    /// Create a commit. Returns `None` if there are no staged changes.
    pub fn commit(&self, message: &str) -> Result<Option<git2::Oid>> {
        self.stage_all()?;

        let mut index = self.repo.index()?;
        let tree_id = index.write_tree()?;

        // Check if tree matches current HEAD (nothing to commit)
        if let Ok(head) = self.repo.head() {
            if let Ok(head_commit) = head.peel_to_commit() {
                if head_commit.tree_id() == tree_id {
                    return Ok(None);
                }
            }
        }

        let tree = self.repo.find_tree(tree_id)?;
        let sig = Signature::now("Bread Sync", "bread@localhost")?;

        let oid = match self.repo.head() {
            Ok(head) => {
                let parent = head.peel_to_commit()?;
                self.repo
                    .commit(Some("HEAD"), &sig, &sig, message, &tree, &[&parent])?
            }
            Err(_) => {
                // First commit — no parents
                self.repo
                    .commit(Some("HEAD"), &sig, &sig, message, &tree, &[])?
            }
        };

        Ok(Some(oid))
    }

    /// Push `branch` to `remote_name`.
    pub fn push(&self, remote_name: &str, branch: &str) -> Result<()> {
        let mut remote = self
            .repo
            .find_remote(remote_name)
            .with_context(|| format!("remote '{}' not found", remote_name))?;

        let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
        let mut push_opts = PushOptions::new();
        let callbacks = make_callbacks();
        push_opts.remote_callbacks(callbacks);
        remote
            .push(&[refspec.as_str()], Some(&mut push_opts))
            .context("git push failed")?;
        Ok(())
    }

    /// Fetch `branch` from `remote_name` without merging.
    pub fn fetch(&self, remote_name: &str, branch: &str) -> Result<()> {
        let mut remote = self
            .repo
            .find_remote(remote_name)
            .with_context(|| format!("remote '{}' not found", remote_name))?;
        let mut fetch_opts = make_fetch_options();
        remote
            .fetch(&[branch], Some(&mut fetch_opts), None)
            .context("git fetch failed")?;
        Ok(())
    }

    /// Fetch and fast-forward merge. Errors on non-fast-forward.
    pub fn pull(&self, remote_name: &str, branch: &str) -> Result<()> {
        self.fetch(remote_name, branch)?;

        let fetch_head = self
            .repo
            .find_reference("FETCH_HEAD")
            .context("FETCH_HEAD not found after fetch")?;
        let fetch_commit = self
            .repo
            .reference_to_annotated_commit(&fetch_head)
            .context("failed to get annotated commit from FETCH_HEAD")?;

        let (analysis, _) = self
            .repo
            .merge_analysis(&[&fetch_commit])
            .context("merge analysis failed")?;

        if analysis.is_up_to_date() {
            return Ok(());
        }

        if analysis.is_fast_forward() {
            let target_id = fetch_commit.id();
            let ref_name = format!("refs/heads/{branch}");
            match self.repo.find_reference(&ref_name) {
                Ok(mut r) => {
                    r.set_target(target_id, "fast-forward pull")?;
                }
                Err(_) => {
                    self.repo
                        .reference(&ref_name, target_id, true, "fast-forward pull")?;
                }
            }
            self.repo.set_head(&ref_name)?;
            self.repo
                .checkout_head(Some(CheckoutBuilder::default().force()))
                .context("checkout failed during pull")?;
            Ok(())
        } else {
            anyhow::bail!(
                "bread: sync conflict — resolve manually in {}",
                self.path.display()
            )
        }
    }

    /// Returns true if working tree has no uncommitted changes.
    pub fn is_clean(&self) -> Result<bool> {
        Ok(self.local_changes()?.is_empty())
    }

    /// Returns list of (status_char, path) for working-tree changes vs HEAD.
    pub fn local_changes(&self) -> Result<Vec<(char, String)>> {
        let mut status_opts = StatusOptions::new();
        status_opts
            .include_untracked(true)
            .recurse_untracked_dirs(true);

        let statuses = self
            .repo
            .statuses(Some(&mut status_opts))
            .context("failed to get git status")?;

        let mut out = Vec::new();
        for entry in statuses.iter() {
            let s = entry.status();
            let ch = if s.contains(git2::Status::INDEX_NEW) || s.contains(git2::Status::WT_NEW) {
                'A'
            } else if s.contains(git2::Status::INDEX_DELETED)
                || s.contains(git2::Status::WT_DELETED)
            {
                'D'
            } else {
                'M'
            };
            if let Some(path) = entry.path() {
                out.push((ch, path.to_string()));
            }
        }
        Ok(out)
    }

    /// Returns list of (status_char, path) for changes on remote not yet pulled.
    pub fn remote_changes(&self, remote_name: &str, branch: &str) -> Result<Vec<(char, String)>> {
        // We compare HEAD to remote/branch
        let remote_ref = format!("refs/remotes/{remote_name}/{branch}");
        let remote_oid = match self.repo.find_reference(&remote_ref) {
            Ok(r) => r.peel_to_commit()?.id(),
            Err(_) => return Ok(vec![]),
        };

        let head_commit = match self.repo.head() {
            Ok(h) => h.peel_to_commit()?.id(),
            Err(_) => return Ok(vec![]),
        };

        if head_commit == remote_oid {
            return Ok(vec![]);
        }

        let head_tree = self.repo.find_commit(head_commit)?.tree()?;
        let remote_tree = self.repo.find_commit(remote_oid)?.tree()?;

        let diff = self
            .repo
            .diff_tree_to_tree(Some(&head_tree), Some(&remote_tree), None)
            .context("failed to compute remote diff")?;

        let mut out = Vec::new();
        for delta in diff.deltas() {
            let ch = match delta.status() {
                git2::Delta::Added => 'A',
                git2::Delta::Deleted => 'D',
                _ => 'M',
            };
            if let Some(path) = delta.new_file().path() {
                out.push((ch, path.to_string_lossy().to_string()));
            }
        }
        Ok(out)
    }

    /// Return a unified diff string of working tree vs HEAD.
    pub fn working_diff(&self) -> Result<String> {
        let head_tree = match self.repo.head() {
            Ok(h) => Some(h.peel_to_tree()?),
            Err(_) => None,
        };

        let diff = self
            .repo
            .diff_tree_to_workdir_with_index(head_tree.as_ref(), None)
            .context("failed to compute working diff")?;

        let mut out = String::new();
        diff.print(git2::DiffFormat::Patch, |_delta, _hunk, line| {
            let prefix = match line.origin() {
                '+' | '-' | ' ' => line.origin().to_string(),
                _ => String::new(),
            };
            out.push_str(&prefix);
            if let Ok(s) = std::str::from_utf8(line.content()) {
                out.push_str(s);
            }
            true
        })
        .context("failed to format diff")?;

        Ok(out)
    }

    /// Return a unified diff string between HEAD and remote branch HEAD.
    pub fn remote_diff(&self, remote_name: &str, branch: &str) -> Result<String> {
        let remote_ref = format!("refs/remotes/{remote_name}/{branch}");
        let remote_oid = self
            .repo
            .find_reference(&remote_ref)
            .and_then(|r| r.peel_to_commit())
            .map(|c| c.id())
            .ok();

        let head_tree = match self.repo.head() {
            Ok(h) => Some(h.peel_to_tree()?),
            Err(_) => None,
        };
        let remote_tree = remote_oid
            .and_then(|id| self.repo.find_commit(id).ok())
            .and_then(|c| c.tree().ok());

        let diff = self
            .repo
            .diff_tree_to_tree(head_tree.as_ref(), remote_tree.as_ref(), None)
            .context("failed to compute remote diff")?;

        let mut out = String::new();
        diff.print(git2::DiffFormat::Patch, |_delta, _hunk, line| {
            let prefix = match line.origin() {
                '+' | '-' | ' ' => line.origin().to_string(),
                _ => String::new(),
            };
            out.push_str(&prefix);
            if let Ok(s) = std::str::from_utf8(line.content()) {
                out.push_str(s);
            }
            true
        })
        .context("failed to format remote diff")?;

        Ok(out)
    }

    /// Set a named remote.
    pub fn set_remote(&self, name: &str, url: &str) -> Result<()> {
        let _ = self.repo.remote_delete(name);
        self.repo
            .remote(name, url)
            .with_context(|| format!("failed to set remote {name}"))?;
        Ok(())
    }

    /// Return the timestamp of the last commit, or None if no commits.
    pub fn last_commit_time(&self) -> Option<chrono::DateTime<chrono::Local>> {
        let head = self.repo.head().ok()?;
        let commit = head.peel_to_commit().ok()?;
        let t = commit.time();
        // git2::Time uses seconds-from-epoch and offset-in-minutes
        let naive = chrono::DateTime::from_timestamp(t.seconds(), 0)?;
        Some(naive.with_timezone(&chrono::Local))
    }
}

fn make_callbacks<'a>() -> RemoteCallbacks<'a> {
    let mut cb = RemoteCallbacks::new();
    cb.credentials(|_url, username_from_url, allowed_types| {
        if allowed_types.contains(git2::CredentialType::SSH_KEY) {
            return Cred::ssh_key_from_agent(username_from_url.unwrap_or("git"));
        }
        Cred::default()
    });
    cb
}

fn make_fetch_options<'a>() -> FetchOptions<'a> {
    let mut opts = FetchOptions::new();
    opts.remote_callbacks(make_callbacks());
    opts
}
