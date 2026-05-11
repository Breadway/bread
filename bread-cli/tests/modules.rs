use bread_cli::modules_mgmt;
use std::fs;
use tempfile::TempDir;

/// Helper: create a minimal valid module directory in `dir` with given name.
fn make_module_dir(dir: &std::path::Path, name: &str, version: &str) -> std::path::PathBuf {
    let module_dir = dir.join(name);
    fs::create_dir_all(&module_dir).unwrap();
    let manifest = format!(
        r#"name = "{name}"
version = "{version}"
description = "Test module"
author = "test"
source = "/tmp/test"
installed_at = ""
"#
    );
    fs::write(module_dir.join("bread.module.toml"), manifest).unwrap();
    fs::write(module_dir.join("init.lua"), "-- test\n").unwrap();
    module_dir
}

#[test]
fn install_from_local_succeeds_with_manifest() {
    let src_tmp = TempDir::new().unwrap();
    let modules_tmp = TempDir::new().unwrap();

    make_module_dir(src_tmp.path(), "mymod", "1.2.3");
    let src = src_tmp.path().join("mymod");

    let result =
        modules_mgmt::install_from_local(&src, "test:mymod", modules_tmp.path());

    assert!(result.is_ok(), "install failed: {:?}", result.err());
    let manifest = result.unwrap();
    assert_eq!(manifest.name, "mymod");
    assert_eq!(manifest.version, "1.2.3");

    // Module directory must exist in modules dir
    assert!(modules_tmp.path().join("mymod").exists());
    assert!(modules_tmp.path().join("mymod").join("bread.module.toml").exists());
    assert!(modules_tmp.path().join("mymod").join("init.lua").exists());
}

#[test]
fn install_from_local_fails_without_manifest() {
    let src_tmp = TempDir::new().unwrap();
    let modules_tmp = TempDir::new().unwrap();

    // No bread.module.toml in src
    let src = src_tmp.path();
    fs::write(src.join("init.lua"), "-- no manifest\n").unwrap();

    let result = modules_mgmt::install_from_local(src, "test:nomod", modules_tmp.path());
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("bread.module.toml"),
        "expected error about bread.module.toml, got: {msg}"
    );
}

#[test]
fn remove_deletes_module_directory() {
    let modules_tmp = TempDir::new().unwrap();
    make_module_dir(modules_tmp.path(), "delme", "0.1.0");

    // Verify it exists before removal
    assert!(modules_tmp.path().join("delme").exists());

    let result = modules_mgmt::remove_module("delme", modules_tmp.path());
    assert!(result.is_ok(), "remove failed: {:?}", result.err());
    assert!(!modules_tmp.path().join("delme").exists());
}

#[test]
fn remove_nonexistent_errors() {
    let modules_tmp = TempDir::new().unwrap();
    let result = modules_mgmt::remove_module("ghost", modules_tmp.path());
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("ghost"), "expected error mentioning module name, got: {msg}");
}

#[test]
fn list_reads_manifests_from_disk() {
    let modules_tmp = TempDir::new().unwrap();
    make_module_dir(modules_tmp.path(), "alpha", "1.0.0");
    make_module_dir(modules_tmp.path(), "beta", "2.0.0");

    // Add a non-module dir (no manifest) — should be ignored
    fs::create_dir_all(modules_tmp.path().join("notamodule")).unwrap();

    let modules = modules_mgmt::list_modules(modules_tmp.path()).unwrap();
    assert_eq!(modules.len(), 2);
    assert_eq!(modules[0].name, "alpha");
    assert_eq!(modules[1].name, "beta");
}

#[test]
fn manifest_written_correctly_on_install() {
    let src_tmp = TempDir::new().unwrap();
    let modules_tmp = TempDir::new().unwrap();

    make_module_dir(src_tmp.path(), "installtest", "3.0.0");
    let src = src_tmp.path().join("installtest");

    let manifest =
        modules_mgmt::install_from_local(&src, "github:test/installtest", modules_tmp.path())
            .unwrap();

    // All required fields must be present and non-empty
    assert_eq!(manifest.name, "installtest");
    assert_eq!(manifest.version, "3.0.0");
    assert!(!manifest.description.is_empty());
    assert!(!manifest.author.is_empty());
    assert_eq!(manifest.source, "github:test/installtest");
    assert!(!manifest.installed_at.is_empty());

    // installed_at must be valid RFC 3339
    let parsed = chrono::DateTime::parse_from_rfc3339(&manifest.installed_at);
    assert!(
        parsed.is_ok(),
        "installed_at '{}' is not valid RFC 3339",
        manifest.installed_at
    );

    // Verify the on-disk manifest also has all fields
    let on_disk = modules_mgmt::read_module_manifest("installtest", modules_tmp.path()).unwrap();
    assert_eq!(on_disk.name, manifest.name);
    assert_eq!(on_disk.installed_at, manifest.installed_at);
    assert_eq!(on_disk.source, "github:test/installtest");
}
