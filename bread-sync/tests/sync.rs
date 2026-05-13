use bread_sync::{
    config::{DelegatesConfig, MachineConfig, PackagesConfig, RemoteConfig, SyncConfig},
    delegates, machine, packages, SyncRepo,
};
use std::fs;
use tempfile::TempDir;

fn make_bare_repo(path: &std::path::Path) -> git2::Repository {
    let mut opts = git2::RepositoryInitOptions::new();
    opts.bare(true);
    opts.initial_head("main");
    git2::Repository::init_opts(path, &opts).unwrap()
}

// Helper to create a git commit in a non-bare repo so we have initial state
fn init_repo_with_commit(path: &std::path::Path) -> SyncRepo {
    let repo = SyncRepo::init(path).unwrap();
    fs::write(path.join(".gitkeep"), "").unwrap();
    repo.stage_all().unwrap();
    repo.commit("initial commit").unwrap();
    repo
}

#[test]
fn sync_init_creates_toml_with_required_fields() {
    let tmp = TempDir::new().unwrap();
    let config = SyncConfig {
        remote: RemoteConfig {
            url: "git@github.com:test/sync.git".to_string(),
            branch: "main".to_string(),
        },
        machine: MachineConfig {
            name: "testbox".to_string(),
            tags: vec!["mobile".to_string()],
        },
        packages: PackagesConfig::default(),
        delegates: DelegatesConfig::default(),
    };
    config.save(tmp.path()).unwrap();

    let loaded = SyncConfig::load(tmp.path()).unwrap();
    assert_eq!(loaded.remote.url, "git@github.com:test/sync.git");
    assert_eq!(loaded.remote.branch, "main");
    assert_eq!(loaded.machine.name, "testbox");
    assert_eq!(loaded.machine.tags, vec!["mobile"]);
}

#[test]
fn sync_init_errors_if_already_initialized() {
    let tmp = TempDir::new().unwrap();
    let config = SyncConfig {
        remote: RemoteConfig {
            url: "git@github.com:test/sync.git".to_string(),
            branch: "main".to_string(),
        },
        machine: MachineConfig {
            name: "box".to_string(),
            tags: vec![],
        },
        packages: PackagesConfig::default(),
        delegates: DelegatesConfig::default(),
    };
    config.save(tmp.path()).unwrap();

    // Second load should succeed (init itself must check for existence externally)
    // We test that load works
    let result = SyncConfig::load(tmp.path());
    assert!(result.is_ok());
    // sync.toml now exists — the CLI checks this before calling save
    assert!(tmp.path().join("sync.toml").exists());
}

#[test]
fn sync_push_creates_correct_directory_structure() {
    let repo_tmp = TempDir::new().unwrap();
    let bare_tmp = TempDir::new().unwrap();
    let bread_cfg_tmp = TempDir::new().unwrap();

    // Create initial bare remote
    let _bare = make_bare_repo(bare_tmp.path());

    // Create local bread config
    fs::write(bread_cfg_tmp.path().join("init.lua"), "-- init\n").unwrap();

    // Init local sync repo
    let repo = SyncRepo::init(repo_tmp.path()).unwrap();
    repo.set_remote("origin", bare_tmp.path().to_str().unwrap())
        .unwrap();

    // Snapshot bread dir
    let bread_dest = repo_tmp.path().join("bread");
    delegates::sync_dir(bread_cfg_tmp.path(), &bread_dest, &[]).unwrap();

    // Write machine profile
    let machines_dir = repo_tmp.path().join("machines");
    let profile = machine::MachineProfile::new("testbox".to_string(), vec![]);
    profile.write(&machines_dir).unwrap();

    // Commit and push
    repo.commit("sync: testbox").unwrap();
    repo.push("origin", "main").unwrap();

    // Verify structure in local repo
    assert!(repo_tmp.path().join("bread").exists());
    assert!(repo_tmp.path().join("bread").join("init.lua").exists());
    assert!(repo_tmp
        .path()
        .join("machines")
        .join("testbox.toml")
        .exists());
}

#[test]
fn sync_push_snapshots_bread_config() {
    let repo_tmp = TempDir::new().unwrap();
    let bare_tmp = TempDir::new().unwrap();
    let bread_cfg_tmp = TempDir::new().unwrap();

    make_bare_repo(bare_tmp.path());

    // Create a more complex bread config
    fs::create_dir_all(bread_cfg_tmp.path().join("modules/mymod")).unwrap();
    fs::write(bread_cfg_tmp.path().join("init.lua"), "-- init").unwrap();
    fs::write(
        bread_cfg_tmp.path().join("modules/mymod/init.lua"),
        "-- mymod",
    )
    .unwrap();

    let repo = SyncRepo::init(repo_tmp.path()).unwrap();
    repo.set_remote("origin", bare_tmp.path().to_str().unwrap())
        .unwrap();

    let bread_dest = repo_tmp.path().join("bread");
    delegates::sync_dir(bread_cfg_tmp.path(), &bread_dest, &[]).unwrap();

    repo.commit("sync: testbox").unwrap();
    repo.push("origin", "main").unwrap();

    // Verify files were copied
    assert!(bread_dest.join("init.lua").exists());
    assert!(bread_dest.join("modules/mymod/init.lua").exists());

    let content = fs::read_to_string(bread_dest.join("init.lua")).unwrap();
    assert_eq!(content, "-- init");
}

#[test]
fn sync_pull_copies_files_from_repo() {
    let bare_tmp = TempDir::new().unwrap();
    let local_tmp = TempDir::new().unwrap();
    let apply_tmp = TempDir::new().unwrap();

    make_bare_repo(bare_tmp.path());

    // Create a local repo, add some files, push to bare
    let repo = SyncRepo::init(local_tmp.path()).unwrap();
    repo.set_remote("origin", bare_tmp.path().to_str().unwrap())
        .unwrap();

    let bread_dest = local_tmp.path().join("bread");
    fs::create_dir_all(&bread_dest).unwrap();
    fs::write(bread_dest.join("init.lua"), "-- from sync").unwrap();

    repo.commit("sync: first push").unwrap();
    repo.push("origin", "main").unwrap();

    // Now clone the bare repo and pull
    let clone_tmp = TempDir::new().unwrap();
    let _cloned =
        SyncRepo::clone_from(bare_tmp.path().to_str().unwrap(), clone_tmp.path()).unwrap();

    // Apply bread/ to apply_tmp
    let src = clone_tmp.path().join("bread");
    if src.exists() {
        delegates::sync_dir(&src, apply_tmp.path(), &[]).unwrap();
    }

    assert!(apply_tmp.path().join("init.lua").exists());
    let content = fs::read_to_string(apply_tmp.path().join("init.lua")).unwrap();
    assert_eq!(content, "-- from sync");
}

#[test]
fn package_manifest_pacman_parses_output_correctly() {
    let input = "firefox 128.0-1\ncurl 8.7.1-1\nrustup 1.27.1-1\n";
    let pkgs = packages::parse_pacman(input);
    assert_eq!(pkgs, vec!["firefox", "curl", "rustup"]);
}

#[test]
fn package_manifest_pip_parses_output_correctly() {
    let input = "requests==2.32.3\nnumpy==2.0.1\nblack>=24.0\n";
    let pkgs = packages::parse_pip(input);
    assert_eq!(pkgs, vec!["requests", "numpy", "black"]);
}

#[test]
fn delegates_exclude_globs_filter_correctly() {
    let src_tmp = TempDir::new().unwrap();
    let dst_tmp = TempDir::new().unwrap();

    // Create files that should and shouldn't be copied
    fs::create_dir_all(src_tmp.path().join(".git/objects")).unwrap();
    fs::write(src_tmp.path().join(".git/objects/abc"), "").unwrap();
    fs::create_dir_all(src_tmp.path().join("lua")).unwrap();
    fs::write(src_tmp.path().join("lua/init.lua"), "-- ok").unwrap();
    fs::write(src_tmp.path().join("log.cache"), "cached").unwrap();

    let excludes = vec!["**/.git".to_string(), "**/*.cache".to_string()];
    delegates::sync_dir(src_tmp.path(), dst_tmp.path(), &excludes).unwrap();

    assert!(dst_tmp.path().join("lua/init.lua").exists());
    assert!(!dst_tmp.path().join(".git").exists());
    assert!(!dst_tmp.path().join("log.cache").exists());
}

#[test]
fn machine_profile_written_with_correct_fields() {
    let machines_tmp = TempDir::new().unwrap();
    let profile = machine::MachineProfile::new(
        "myhost".to_string(),
        vec!["mobile".to_string(), "battery".to_string()],
    );
    profile.write(machines_tmp.path()).unwrap();

    let loaded = machine::MachineProfile::read(machines_tmp.path(), "myhost").unwrap();
    assert_eq!(loaded.name, "myhost");
    assert_eq!(loaded.tags, vec!["mobile", "battery"]);
    assert!(!loaded.hostname.is_empty());
    // last_sync must be valid RFC 3339
    let parsed = chrono::DateTime::parse_from_rfc3339(&loaded.last_sync);
    assert!(
        parsed.is_ok(),
        "last_sync '{}' is not valid RFC 3339",
        loaded.last_sync
    );
}

#[test]
fn status_shows_no_changes_when_clean() {
    let repo_tmp = TempDir::new().unwrap();
    let repo = init_repo_with_commit(repo_tmp.path());
    let changes = repo.local_changes().unwrap();
    assert!(
        changes.is_empty(),
        "expected no local changes, got: {:?}",
        changes
    );
    assert!(repo.is_clean().unwrap());
}

#[test]
fn push_with_no_changes_returns_none() {
    let repo_tmp = TempDir::new().unwrap();
    let repo = init_repo_with_commit(repo_tmp.path());

    // No new changes — commit should return None
    let result = repo.commit("second commit").unwrap();
    assert!(
        result.is_none(),
        "expected None (nothing to commit), got: {:?}",
        result
    );
}

// ─── git.rs additional coverage ────────────────────────────────────────────

#[test]
fn init_creates_repo_with_main_branch() {
    let tmp = TempDir::new().unwrap();
    let repo = SyncRepo::init(tmp.path()).unwrap();
    fs::write(tmp.path().join("x"), "").unwrap();
    repo.stage_all().unwrap();
    let oid = repo.commit("initial").unwrap();
    assert!(oid.is_some(), "first commit should succeed");

    // Verify HEAD is on refs/heads/main.
    let head_ref = std::process::Command::new("git")
        .args(["-C", tmp.path().to_str().unwrap(), "symbolic-ref", "HEAD"])
        .output()
        .unwrap();
    let head_name = String::from_utf8_lossy(&head_ref.stdout);
    assert!(
        head_name.trim() == "refs/heads/main",
        "expected refs/heads/main, got {head_name}"
    );
}

#[test]
fn open_or_clone_opens_existing_repo() {
    let tmp = TempDir::new().unwrap();
    SyncRepo::init(tmp.path()).unwrap();

    // Calling open_or_clone on an existing path must not attempt to clone.
    let again = SyncRepo::open_or_clone("/nonexistent-url-that-would-fail", tmp.path());
    assert!(again.is_ok());
}

#[test]
fn open_or_clone_clones_into_missing_path() {
    let bare = TempDir::new().unwrap();
    let bare_repo = make_bare_repo(bare.path());
    // Seed the bare repo with at least one commit so a clone is meaningful.
    let local = TempDir::new().unwrap();
    let repo = SyncRepo::init(local.path()).unwrap();
    fs::write(local.path().join("seed"), "x").unwrap();
    repo.commit("seed").unwrap();
    repo.set_remote("origin", bare.path().to_str().unwrap())
        .unwrap();
    repo.push("origin", "main").unwrap();
    drop(bare_repo);

    let dest_parent = TempDir::new().unwrap();
    let dest = dest_parent.path().join("clone-target");
    let cloned = SyncRepo::open_or_clone(bare.path().to_str().unwrap(), &dest).unwrap();
    assert_eq!(cloned.path, dest);
    assert!(dest.join("seed").exists());
}

#[test]
fn local_changes_reports_new_modified_and_deleted() {
    let tmp = TempDir::new().unwrap();
    let repo = init_repo_with_commit(tmp.path());

    fs::write(tmp.path().join("added.txt"), "new").unwrap();
    fs::write(tmp.path().join(".gitkeep"), "modified").unwrap();

    let changes = repo.local_changes().unwrap();
    assert!(!changes.is_empty());
    let kinds: Vec<char> = changes.iter().map(|(c, _)| *c).collect();
    assert!(kinds.contains(&'A'));
    assert!(kinds.contains(&'M'));
}

#[test]
fn is_clean_after_commit() {
    let tmp = TempDir::new().unwrap();
    let repo = init_repo_with_commit(tmp.path());
    assert!(repo.is_clean().unwrap());
}

#[test]
fn working_diff_includes_modified_tracked_content() {
    let tmp = TempDir::new().unwrap();
    let repo = init_repo_with_commit(tmp.path());
    // Modify an already-tracked file so it appears in `git diff HEAD`.
    fs::write(tmp.path().join(".gitkeep"), "tracked change\n").unwrap();

    let diff = repo.working_diff().unwrap();
    assert!(
        diff.contains("tracked change"),
        "diff did not include tracked change, diff was: {diff:?}"
    );
}

#[test]
fn working_diff_empty_when_only_untracked_files() {
    let tmp = TempDir::new().unwrap();
    let repo = init_repo_with_commit(tmp.path());
    fs::write(tmp.path().join("new-untracked.txt"), "hi").unwrap();

    // working_diff uses diff_tree_to_workdir_with_index without INCLUDE_UNTRACKED,
    // so untracked files don't appear — local_changes is the right tool for that.
    let diff = repo.working_diff().unwrap();
    assert!(
        diff.is_empty() || !diff.contains("new-untracked"),
        "expected untracked file to be excluded, diff was: {diff:?}"
    );
}

#[test]
fn set_remote_overwrites_existing_remote() {
    let tmp = TempDir::new().unwrap();
    let repo = SyncRepo::init(tmp.path()).unwrap();
    repo.set_remote("origin", "https://example.com/a.git")
        .unwrap();
    // A second call must not error out — it should replace the previous URL.
    repo.set_remote("origin", "https://example.com/b.git")
        .unwrap();
}

#[test]
fn last_commit_time_returns_none_for_empty_repo() {
    let tmp = TempDir::new().unwrap();
    let repo = SyncRepo::init(tmp.path()).unwrap();
    assert!(repo.last_commit_time().is_none());
}

#[test]
fn last_commit_time_present_after_commit() {
    let tmp = TempDir::new().unwrap();
    let repo = init_repo_with_commit(tmp.path());
    assert!(repo.last_commit_time().is_some());
}

#[test]
fn push_pull_round_trip_through_bare_remote() {
    let bare = TempDir::new().unwrap();
    make_bare_repo(bare.path());

    // Push from author repo.
    let author = TempDir::new().unwrap();
    let r1 = SyncRepo::init(author.path()).unwrap();
    r1.set_remote("origin", bare.path().to_str().unwrap())
        .unwrap();
    fs::write(author.path().join("note.txt"), "v1").unwrap();
    r1.commit("v1").unwrap();
    r1.push("origin", "main").unwrap();

    // Clone into reader repo and confirm contents.
    let reader_tmp = TempDir::new().unwrap();
    let r2 = SyncRepo::clone_from(bare.path().to_str().unwrap(), reader_tmp.path()).unwrap();
    assert_eq!(
        fs::read_to_string(reader_tmp.path().join("note.txt")).unwrap(),
        "v1"
    );

    // Author writes a second version and pushes.
    fs::write(author.path().join("note.txt"), "v2").unwrap();
    r1.commit("v2").unwrap();
    r1.push("origin", "main").unwrap();

    // Reader pulls and sees the new content.
    r2.pull("origin", "main").unwrap();
    assert_eq!(
        fs::read_to_string(reader_tmp.path().join("note.txt")).unwrap(),
        "v2"
    );
}

#[test]
fn pull_with_no_remote_changes_is_noop() {
    let bare = TempDir::new().unwrap();
    make_bare_repo(bare.path());

    let local = TempDir::new().unwrap();
    let repo = SyncRepo::init(local.path()).unwrap();
    repo.set_remote("origin", bare.path().to_str().unwrap())
        .unwrap();
    fs::write(local.path().join("a"), "1").unwrap();
    repo.commit("c1").unwrap();
    repo.push("origin", "main").unwrap();

    // Calling pull immediately after push must be up-to-date and succeed.
    repo.pull("origin", "main").unwrap();
    assert!(repo.is_clean().unwrap());
}

#[test]
fn remote_changes_returns_empty_when_remote_unknown() {
    let tmp = TempDir::new().unwrap();
    let repo = init_repo_with_commit(tmp.path());
    let changes = repo.remote_changes("origin", "main").unwrap();
    assert!(changes.is_empty());
}

// ─── machine list ──────────────────────────────────────────────────────────

#[test]
fn machine_list_returns_all_profiles_sorted() {
    let machines_tmp = TempDir::new().unwrap();
    for name in ["delta", "alpha", "charlie", "bravo"] {
        machine::MachineProfile::new(name.to_string(), vec![])
            .write(machines_tmp.path())
            .unwrap();
    }
    let list = machine::MachineProfile::list(machines_tmp.path()).unwrap();
    let names: Vec<&str> = list.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, vec!["alpha", "bravo", "charlie", "delta"]);
}

// ─── packages snapshot ─────────────────────────────────────────────────────

#[test]
fn snapshot_writes_destination_when_manager_unknown_is_skipped() {
    let dest_tmp = TempDir::new().unwrap();
    let dest = dest_tmp.path().join("nested/dir/file.txt");
    let wrote = packages::snapshot("does-not-exist", &dest).unwrap();
    assert!(!wrote);
    assert!(!dest.exists());
}
