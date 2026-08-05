//! Repo maintenance tasks that don't belong in the `breadd`/`bread-cli`
//! binaries themselves.
//!
//! Currently just `check-docs`: a drift detector between the actual
//! `bread.*` Lua binding surface + IPC method surface + `bread` CLI command
//! surface (as implemented in `breadd/src/lua/mod.rs`, `breadd/src/ipc/mod.rs`,
//! and `bread-cli/src/main.rs`) and the checked-in registry at
//! `api-schema.toml`, cross-referenced against `Documentation.md` (Lua/IPC)
//! and `README.md` (CLI commands). See `api-schema.toml`'s header comment for
//! why this exists and why it's a *drift detector*, not a doc generator.
//!
//! The CLI-command coverage exists specifically because README's "CLI
//! reference" section drifted from `Documentation.md`/the real `bread-cli`
//! source (missing `modules audit`, `hooks install-shell`/`install-git`,
//! `events --tree`) — a live instance of exactly the drift this whole
//! workstream exists to prevent, found and fixed by hand once the schema
//! didn't cover it yet. See `api-schema.toml`'s header for how CLI commands
//! are named and versioned differently from the Lua/IPC entries.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use serde::Deserialize;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("check-docs") => match run_check_docs() {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(err) => {
                eprintln!("xtask check-docs: error: {err:#}");
                ExitCode::FAILURE
            }
        },
        Some(other) => {
            eprintln!("unknown xtask command '{other}'\n{}", usage());
            ExitCode::FAILURE
        }
        None => {
            eprintln!("{}", usage());
            ExitCode::FAILURE
        }
    }
}

fn usage() -> &'static str {
    "usage: cargo run -p xtask -- <command>\n\n\
     Available commands:\n  \
     check-docs   verify api-schema.toml matches breadd's Lua/IPC surface and Documentation.md"
}

fn run_check_docs() -> Result<bool> {
    let root = repo_root()?;

    let lua_src = std::fs::read_to_string(root.join("breadd/src/lua/mod.rs"))
        .context("reading breadd/src/lua/mod.rs")?;
    let ipc_src = std::fs::read_to_string(root.join("breadd/src/ipc/mod.rs"))
        .context("reading breadd/src/ipc/mod.rs")?;
    let cli_src = std::fs::read_to_string(root.join("bread-cli/src/main.rs"))
        .context("reading bread-cli/src/main.rs")?;
    let schema_toml = std::fs::read_to_string(root.join("api-schema.toml"))
        .context("reading api-schema.toml")?;
    let doc_md =
        std::fs::read_to_string(root.join("Documentation.md")).context("reading Documentation.md")?;
    let readme_md =
        std::fs::read_to_string(root.join("README.md")).context("reading README.md")?;

    let report = check(&lua_src, &ipc_src, &cli_src, &schema_toml, &doc_md, &readme_md)?;
    report.print();
    Ok(report.is_clean())
}

fn repo_root() -> Result<PathBuf> {
    // xtask's own Cargo.toml lives at <repo_root>/xtask/Cargo.toml, so its
    // parent is the workspace root regardless of the caller's cwd (`cargo
    // run -p xtask` sets CARGO_MANIFEST_DIR to xtask/, not the invocation dir).
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .context("xtask has no parent directory (unexpected workspace layout)")
}

// ---------------------------------------------------------------------------
// Schema

#[derive(Debug, Deserialize)]
struct Schema {
    #[serde(default)]
    entry: Vec<SchemaEntry>,
}

#[derive(Debug, Clone, Deserialize)]
struct SchemaEntry {
    name: String,
    kind: String,
    #[serde(default)]
    since: String,
}

const VALID_KINDS: &[&str] = &["lua_function", "lua_table", "ipc_method", "cli_command"];

// ---------------------------------------------------------------------------
// Extraction: breadd/src/lua/mod.rs -> the current bread.* Lua API surface.
//
// This is deliberately line/substring scanning, not a real Lua or Rust
// parser (see api-schema.toml's header comment) — precise enough because
// `install_api` and its `install_*_helpers` siblings follow a small, stable
// set of textual patterns:
//
//   1. `bread.set("name", ...)`        - a top-level binding (function or,
//                                         for the known sub-table variables
//                                         below, a table).
//   2. `<x>_tbl.set("name", ...)`      - a method nested under table `<x>`.
//   3. `function _bread.name(` /
//      `function bread.name(`          - a plain-Lua-defined top-level
//                                         function (log/warn/error/debounce).
//   4. `bread.name = function`         - ditto, assignment form
//                                         (spawn/wait/wait_any/wait_all).
//   5. `bread.workflow = {}` and
//      `bread.workflow.name = function` - the `bread.workflow` table and its
//                                          members.

/// Sub-table variables built in `install_api` and registered onto the
/// `bread` global, mapped to the dotted parent name they hang off.
/// Deliberately excludes `module_tbl` / `store_tbl`: those back the
/// per-module `M` object returned by `bread.module(...)` (i.e. `M.store.get`
/// etc.), which is a different namespace from `bread.*` itself.
const TABLE_VARS: &[(&str, &str)] = &[
    ("state_tbl", "state"),
    ("profile_tbl", "profile"),
    ("hyprland_tbl", "hyprland"),
    ("widget_tbl", "widget"),
    ("machine_tbl", "machine"),
    ("fs_tbl", "fs"),
    ("json_tbl", "json"),
    ("bluetooth_tbl", "bluetooth"),
];

/// Read a bare identifier (`[A-Za-z0-9_]+`) starting at byte offset `start`.
fn ident_at(text: &str, start: usize) -> &str {
    let rest = &text[start..];
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    &rest[..end]
}

/// Read up to (not including) the next `"` starting at byte offset `start`.
fn ident_upto_quote(text: &str, start: usize) -> &str {
    let rest = &text[start..];
    let end = rest.find('"').unwrap_or(rest.len());
    &rest[..end]
}

fn extract_lua_bindings(src: &str) -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();

    // 1. Top-level `bread.set("name", ...)`.
    for (idx, _) in src.match_indices("bread.set(\"") {
        let name = ident_upto_quote(src, idx + "bread.set(\"".len());
        if name.is_empty() || name.starts_with("__") {
            continue; // internal Rust<->Lua bridge fn (e.g. __log_info), not public API
        }
        let is_table = TABLE_VARS.iter().any(|(_, parent)| *parent == name);
        out.insert((
            (if is_table { "lua_table" } else { "lua_function" }).to_string(),
            name.to_string(),
        ));
    }

    // 2. Nested `<x>_tbl.set("name", ...)` for the known sub-tables.
    for (var, parent) in TABLE_VARS {
        let prefix = format!("{var}.set(\"");
        for (idx, _) in src.match_indices(prefix.as_str()) {
            let name = ident_upto_quote(src, idx + prefix.len());
            if name.is_empty() {
                continue;
            }
            out.insert(("lua_function".to_string(), format!("{parent}.{name}")));
        }
    }

    // 3. `function _bread.name(` / `function bread.name(`.
    for prefix in ["function _bread.", "function bread."] {
        for (idx, _) in src.match_indices(prefix) {
            let name = ident_at(src, idx + prefix.len());
            if !name.is_empty() {
                out.insert(("lua_function".to_string(), name.to_string()));
            }
        }
    }

    // 4. `bread.name = function` (top-level only; `bread.workflow.*` is
    //    handled separately in step 5 since it's a nested table's members).
    for (idx, _) in src.match_indices("bread.") {
        let name_start = idx + "bread.".len();
        let name = ident_at(src, name_start);
        if name.is_empty() || name == "workflow" {
            continue;
        }
        if src[name_start + name.len()..].trim_start().starts_with("= function") {
            out.insert(("lua_function".to_string(), name.to_string()));
        }
    }

    // 5. `bread.workflow = {}` and `bread.workflow.name = function`.
    if src.contains("bread.workflow = {}") {
        out.insert(("lua_table".to_string(), "workflow".to_string()));
    }
    for (idx, _) in src.match_indices("bread.workflow.") {
        let name_start = idx + "bread.workflow.".len();
        let name = ident_at(src, name_start);
        if name.is_empty() {
            continue;
        }
        if src[name_start + name.len()..].trim_start().starts_with("= function") {
            out.insert(("lua_function".to_string(), format!("workflow.{name}")));
        }
    }

    out
}

// ---------------------------------------------------------------------------
// Extraction: breadd/src/ipc/mod.rs -> the current IPC method surface.

fn extract_ipc_methods(src: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();

    // `events.subscribe` is special-cased ahead of the dispatch `match`
    // (it upgrades the connection to a streaming socket instead of
    // returning a single response), so it never appears as a match arm.
    if src.contains("req.method == \"events.subscribe\"") {
        out.insert("events.subscribe".to_string());
    }

    let Some(match_start) = src.find("match req.method.as_str() {") else {
        return out;
    };

    // Track brace depth so a `"literal" => ...` pattern belonging to some
    // *other*, nested match inside an arm's body (e.g. the `"emit"` arm's
    // own `match source_str { "terminal" => ..., "git" => ... }`) isn't
    // mistaken for a top-level IPC method arm. Only depth == 1 (directly
    // inside the outer `match req.method.as_str() { ... }`) counts.
    let mut depth: i32 = 0;
    for line in src[match_start..].lines() {
        let pre_depth = depth;
        let trimmed = line.trim_start();

        if pre_depth == 1 {
            if let Some(rest) = trimmed.strip_prefix('"') {
                if let Some(end) = rest.find('"') {
                    let name = &rest[..end];
                    if rest[end + 1..].trim_start().starts_with("=>") {
                        out.insert(name.to_string());
                    }
                }
            }
        }

        depth += line.matches('{').count() as i32;
        depth -= line.matches('}').count() as i32;
        if pre_depth >= 1 && depth <= 0 {
            break; // closed the outer match block
        }
    }

    out
}

// ---------------------------------------------------------------------------
// Extraction: bread-cli/src/main.rs -> the current `bread` CLI command
// surface.
//
// Same deliberate textual-scanning approach as the Lua/IPC extractors
// above, not a real Rust/clap-derive-macro-expanding parser.

/// Top-level `Commands` variants that delegate to a nested subcommand enum
/// via `#[command(subcommand)] subcommand: XCommand` rather than being a
/// leaf command themselves — each such variant's own leaf commands are
/// named `<variant-kebab>.<subcommand-kebab>` (e.g. `modules.audit`,
/// `hooks.install-shell`) instead of the group variant itself being a leaf.
/// Update this list if `bread-cli/src/main.rs` gains another subcommand
/// group (the same kind of manual-update-needed list as `TABLE_VARS` above).
const SUBCOMMAND_GROUPS: &[(&str, &str)] = &[("Modules", "ModulesCommand"), ("Hooks", "HooksCommand")];

/// clap's default rename for a derived `Subcommand` variant: PascalCase ->
/// kebab-case (`ProfileList` -> `profile-list`, `InstallShell` ->
/// `install-shell`). This is what actually shows up as `bread <name>` on
/// the command line and in README's CLI reference.
fn kebab(pascal: &str) -> String {
    let mut out = String::new();
    for (i, c) in pascal.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i != 0 {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Scan a `#[derive(Subcommand, ...)] enum <enum_name> { ... }` block for
/// its variant names, in source order. Depth-tracked the same way
/// `extract_ipc_methods` tracks match-arm depth, so a variant's own nested
/// struct-style fields (and their attributes/doc comments) aren't mistaken
/// for sibling variants.
fn extract_enum_variants(src: &str, enum_name: &str) -> Vec<String> {
    let marker = format!("enum {enum_name} {{");
    let Some(start) = src.find(&marker) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    let mut depth: i32 = 0;
    for line in src[start..].lines() {
        let pre_depth = depth;
        let trimmed = line.trim_start();

        if pre_depth == 1 {
            let name_end = trimmed
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(trimmed.len());
            let name = &trimmed[..name_end];
            if !name.is_empty() && name.starts_with(|c: char| c.is_ascii_uppercase()) {
                out.push(name.to_string());
            }
        }

        depth += line.matches('{').count() as i32;
        depth -= line.matches('}').count() as i32;
        if pre_depth >= 1 && depth <= 0 {
            break; // closed the enum block
        }
    }
    out
}

fn extract_cli_commands(src: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for variant in extract_enum_variants(src, "Commands") {
        if let Some((_, sub_enum)) = SUBCOMMAND_GROUPS.iter().find(|(v, _)| *v == variant) {
            let group = kebab(&variant);
            for sub in extract_enum_variants(src, sub_enum) {
                out.insert(format!("{group}.{}", kebab(&sub)));
            }
        } else {
            out.insert(kebab(&variant));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Documentation.md cross-checks

/// Slice out a `## `-level section (from its heading up to, but not
/// including, the next `## `-level heading).
fn section<'a>(doc: &'a str, heading: &str) -> &'a str {
    let Some(start) = doc.find(heading) else {
        return "";
    };
    let rest = &doc[start..];
    match rest[3..].find("\n## ") {
        Some(i) => &rest[..i + 3],
        None => rest,
    }
}

fn lua_api_section(doc: &str) -> &str {
    section(doc, "## Dictionary: Lua API")
}

fn ipc_section(doc: &str) -> &str {
    section(doc, "## Dictionary: IPC protocol")
}

// ---------------------------------------------------------------------------
// README.md cross-check

fn readme_cli_section(readme: &str) -> &str {
    section(readme, "## CLI reference")
}

// ---------------------------------------------------------------------------
// Report

#[derive(Debug, Default)]
struct CheckReport {
    /// In code, not in api-schema.toml.
    missing_from_schema: Vec<(String, String)>,
    /// In api-schema.toml, no longer in code.
    stale_in_schema: Vec<(String, String)>,
    /// In api-schema.toml (and in code), but Documentation.md has no
    /// heading/row for it (lua_function/lua_table/ipc_method only).
    undocumented: Vec<(String, String)>,
    /// In api-schema.toml (and in code), but README's "CLI reference"
    /// section has no `bread <name>` line for it (cli_command only).
    missing_from_readme: Vec<(String, String)>,
    /// Schema entries with an unrecognized `kind`.
    bad_kind: Vec<(String, String)>,
    /// Schema entries with an empty `since`.
    missing_since: Vec<(String, String)>,
}

impl CheckReport {
    fn is_clean(&self) -> bool {
        self.missing_from_schema.is_empty()
            && self.stale_in_schema.is_empty()
            && self.undocumented.is_empty()
            && self.missing_from_readme.is_empty()
            && self.bad_kind.is_empty()
            && self.missing_since.is_empty()
    }

    fn print(&self) {
        if self.is_clean() {
            println!("check-docs: OK — api-schema.toml matches breadd's Lua/IPC/CLI surface, Documentation.md, and README.md.");
            return;
        }

        if !self.bad_kind.is_empty() {
            println!("Schema entries with an unrecognized `kind` (expected one of {VALID_KINDS:?}):");
            for (kind, name) in &self.bad_kind {
                println!("  - {name} (kind = \"{kind}\")");
            }
            println!();
        }

        if !self.missing_from_schema.is_empty() {
            println!("Added but undocumented in api-schema.toml (present in code, missing from schema):");
            for (kind, name) in &self.missing_from_schema {
                println!("  - [{kind}] {name}");
            }
            println!();
        }

        if !self.stale_in_schema.is_empty() {
            println!("Stale in api-schema.toml (no longer found in code):");
            for (kind, name) in &self.stale_in_schema {
                println!("  - [{kind}] {name}");
            }
            println!();
        }

        if !self.undocumented.is_empty() {
            println!("In api-schema.toml but missing from Documentation.md (no `#### bread.<name>` heading for a lua_function/lua_table, or no `<name>` row in the IPC Methods table):");
            for (kind, name) in &self.undocumented {
                println!("  - [{kind}] {name}");
            }
            println!();
        }

        if !self.missing_from_readme.is_empty() {
            println!("In api-schema.toml but missing from README.md's \"CLI reference\" section (no `bread <name>` line):");
            for (kind, name) in &self.missing_from_readme {
                println!("  - [{kind}] {name}");
            }
            println!();
        }

        if !self.missing_since.is_empty() {
            println!("Schema entries with an empty `since`:");
            for (kind, name) in &self.missing_since {
                println!("  - [{kind}] {name}");
            }
            println!();
        }

        println!("check-docs: FAILED — see above.");
    }
}

fn check(
    lua_src: &str,
    ipc_src: &str,
    cli_src: &str,
    schema_toml: &str,
    doc_md: &str,
    readme_md: &str,
) -> Result<CheckReport> {
    let schema: Schema = toml::from_str(schema_toml).context("parsing api-schema.toml")?;

    let mut report = CheckReport::default();

    let mut schema_set: BTreeSet<(String, String)> = BTreeSet::new();
    for entry in &schema.entry {
        if !VALID_KINDS.contains(&entry.kind.as_str()) {
            report.bad_kind.push((entry.kind.clone(), entry.name.clone()));
            continue;
        }
        if entry.since.trim().is_empty() {
            report
                .missing_since
                .push((entry.kind.clone(), entry.name.clone()));
        }
        schema_set.insert((entry.kind.clone(), entry.name.clone()));
    }

    // --- code vs schema: Lua ---
    let code_lua = extract_lua_bindings(lua_src);
    let schema_lua: BTreeSet<(String, String)> = schema_set
        .iter()
        .filter(|(kind, _)| kind == "lua_function" || kind == "lua_table")
        .cloned()
        .collect();

    for entry in code_lua.difference(&schema_lua) {
        report.missing_from_schema.push(entry.clone());
    }
    for entry in schema_lua.difference(&code_lua) {
        report.stale_in_schema.push(entry.clone());
    }

    // --- code vs schema: IPC ---
    let code_ipc = extract_ipc_methods(ipc_src);
    let schema_ipc: BTreeSet<String> = schema_set
        .iter()
        .filter(|(kind, _)| kind == "ipc_method")
        .map(|(_, name)| name.clone())
        .collect();

    for name in code_ipc.difference(&schema_ipc) {
        report
            .missing_from_schema
            .push(("ipc_method".to_string(), name.clone()));
    }
    for name in schema_ipc.difference(&code_ipc) {
        report
            .stale_in_schema
            .push(("ipc_method".to_string(), name.clone()));
    }

    // --- code vs schema: CLI ---
    let code_cli = extract_cli_commands(cli_src);
    let schema_cli: BTreeSet<String> = schema_set
        .iter()
        .filter(|(kind, _)| kind == "cli_command")
        .map(|(_, name)| name.clone())
        .collect();

    for name in code_cli.difference(&schema_cli) {
        report
            .missing_from_schema
            .push(("cli_command".to_string(), name.clone()));
    }
    for name in schema_cli.difference(&code_cli) {
        report
            .stale_in_schema
            .push(("cli_command".to_string(), name.clone()));
    }

    // --- schema vs Documentation.md (lua_function/lua_table/ipc_method only —
    // cli_command is checked against README.md separately, below) ---
    let lua_section = lua_api_section(doc_md);
    let ipc_tbl_section = ipc_section(doc_md);
    for entry in &schema.entry {
        if !VALID_KINDS.contains(&entry.kind.as_str()) || entry.kind == "cli_command" {
            continue; // already reported above, or checked against README instead
        }
        let documented = if entry.kind == "ipc_method" {
            ipc_tbl_section.contains(&format!("`{}`", entry.name))
        } else {
            lua_section.contains(&format!("bread.{}", entry.name))
        };
        if !documented {
            report
                .undocumented
                .push((entry.kind.clone(), entry.name.clone()));
        }
    }

    // --- schema vs README.md (cli_command only) ---
    let readme_cli = readme_cli_section(readme_md);
    for entry in &schema.entry {
        if entry.kind != "cli_command" {
            continue;
        }
        let cmd_text = format!("bread {}", entry.name.replace('.', " "));
        if !readme_cli.contains(cmd_text.as_str()) {
            report
                .missing_from_readme
                .push((entry.kind.clone(), entry.name.clone()));
        }
    }

    report.missing_from_schema.sort();
    report.stale_in_schema.sort();
    report.undocumented.sort();
    report.missing_from_readme.sort();
    report.bad_kind.sort();
    report.missing_since.sort();

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LUA_SRC: &str = r#"
        bread.set("on", on_fn)?;
        bread.set("state", state_tbl)?;
        state_tbl.set("get", get_fn)?;
        state_tbl.set("watch", watch_fn)?;
        bread.set("__log_info", info_fn)?;
        function bread.debounce(delay_ms, fn)
        bread.spawn = function(fn)
        bread.workflow = {}
        bread.workflow.define = function(name, fn)
    "#;

    const IPC_SRC: &str = r#"
        if req.method == "events.subscribe" {
        }
        let result = match req.method.as_str() {
            "ping" => Ok(json!({ "ok": true })),
            "emit" => {
                let source = match source_str {
                    "terminal" => AdapterSource::Terminal,
                    "git" => AdapterSource::Git,
                    other => AdapterSource::App(other.to_string()),
                };
                Ok(json!({ "emitted": true }))
            }
            _ => Err("unknown method".to_string()),
        };
    "#;

    const DOC_MD: &str = r#"
## Dictionary: Lua API

#### `bread.on(pattern, fn) -> id`
#### `bread.state.get(path)`
#### `bread.state.watch(path, fn) -> id`
#### `bread.debounce(delay_ms, fn) -> wrapped_fn`
#### `bread.spawn(fn)`
### Workflows
#### `bread.workflow.define(name, fn)`

## Dictionary: IPC protocol

| Method | Params | Description |
|--------|--------|-------------|
| `ping` | - | Connectivity check |
| `emit` | - | Inject an event |
| `events.subscribe` | - | Upgrade to streaming mode |
"#;

    /// Mirrors the real `bread-cli/src/main.rs` shape closely enough to
    /// exercise both a flat leaf command (`Ping`) and a subcommand group
    /// (`Modules` -> `ModulesCommand`), including a struct-style variant
    /// with fields (`Info { name: String }`).
    const CLI_SRC: &str = r#"
enum Commands {
    /// Health check daemon connectivity
    Ping,
    /// Manage installed Lua modules
    Modules {
        #[command(subcommand)]
        subcommand: ModulesCommand,
    },
}

enum ModulesCommand {
    /// List all installed modules
    List,
    /// Show full manifest details for a module
    Info { name: String },
}
"#;

    const README_MD: &str = r#"
## CLI reference

```bash
bread ping
bread modules list
bread modules info <name>
```

---

## Module system
"#;

    fn schema_toml_for(entries: &[(&str, &str, &str)]) -> String {
        let mut out = String::new();
        for (name, kind, since) in entries {
            out.push_str(&format!(
                "[[entry]]\nname = \"{name}\"\nkind = \"{kind}\"\nsince = \"{since}\"\n\n"
            ));
        }
        out
    }

    fn full_schema() -> String {
        schema_toml_for(&[
            ("on", "lua_function", "1.0"),
            ("state", "lua_table", "1.0"),
            ("state.get", "lua_function", "1.0"),
            ("state.watch", "lua_function", "1.0"),
            ("debounce", "lua_function", "1.0"),
            ("spawn", "lua_function", "1.0"),
            ("workflow", "lua_table", "1.2"),
            ("workflow.define", "lua_function", "1.2"),
            ("ping", "ipc_method", "1.0"),
            ("emit", "ipc_method", "1.0"),
            ("events.subscribe", "ipc_method", "1.0"),
            ("ping", "cli_command", "0.7"),
            ("modules.list", "cli_command", "0.7"),
            ("modules.info", "cli_command", "0.7"),
        ])
    }

    #[test]
    fn extracts_lua_bindings_precisely() {
        let got = extract_lua_bindings(LUA_SRC);
        let want: BTreeSet<(String, String)> = [
            ("lua_function", "on"),
            ("lua_table", "state"),
            ("lua_function", "state.get"),
            ("lua_function", "state.watch"),
            ("lua_function", "debounce"),
            ("lua_function", "spawn"),
            ("lua_table", "workflow"),
            ("lua_function", "workflow.define"),
        ]
        .into_iter()
        .map(|(k, n)| (k.to_string(), n.to_string()))
        .collect();
        assert_eq!(got, want, "must exclude __-prefixed internals");
    }

    #[test]
    fn extracts_ipc_methods_without_leaking_nested_match_arms() {
        let got = extract_ipc_methods(IPC_SRC);
        let want: BTreeSet<String> = ["events.subscribe", "ping", "emit"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(
            got, want,
            "must not pick up \"terminal\"/\"git\" from the nested `match source_str` inside the emit arm"
        );
    }

    #[test]
    fn clean_state_passes() {
        let report = check(LUA_SRC, IPC_SRC, CLI_SRC, &full_schema(), DOC_MD, README_MD).unwrap();
        assert!(report.is_clean(), "{report:#?}");
    }

    #[test]
    fn renamed_schema_entry_is_caught_as_drift() {
        // Simulate a contributor renaming `bread.debounce` in code without
        // updating api-schema.toml: schema still says "debounce", code no
        // longer does (only "debounce_v2" now exists).
        let renamed_lua_src = LUA_SRC.replace("bread.debounce", "bread.debounce_v2");
        let report = check(
            &renamed_lua_src,
            IPC_SRC,
            CLI_SRC,
            &full_schema(),
            DOC_MD,
            README_MD,
        )
        .unwrap();
        assert!(!report.is_clean());
        assert!(report
            .missing_from_schema
            .contains(&("lua_function".to_string(), "debounce_v2".to_string())));
        assert!(report
            .stale_in_schema
            .contains(&("lua_function".to_string(), "debounce".to_string())));
    }

    #[test]
    fn removed_ipc_method_is_caught_as_drift() {
        let without_emit = IPC_SRC.replacen("\"emit\" => {", "\"emit_removed\" => {", 1);
        let report = check(
            LUA_SRC,
            &without_emit,
            CLI_SRC,
            &full_schema(),
            DOC_MD,
            README_MD,
        )
        .unwrap();
        assert!(!report.is_clean());
        assert!(report
            .stale_in_schema
            .contains(&("ipc_method".to_string(), "emit".to_string())));
    }

    #[test]
    fn missing_doc_heading_is_caught_as_drift() {
        let doc_without_spawn_heading = DOC_MD.replace("#### `bread.spawn(fn)`\n", "");
        let report = check(
            LUA_SRC,
            IPC_SRC,
            CLI_SRC,
            &full_schema(),
            &doc_without_spawn_heading,
            README_MD,
        )
        .unwrap();
        assert!(!report.is_clean());
        assert!(report
            .undocumented
            .contains(&("lua_function".to_string(), "spawn".to_string())));
    }

    #[test]
    fn unknown_kind_is_rejected() {
        let bad_schema = schema_toml_for(&[("on", "lua_thing", "1.0")]);
        let report = check(LUA_SRC, IPC_SRC, CLI_SRC, &bad_schema, DOC_MD, README_MD).unwrap();
        assert!(!report.is_clean());
        assert!(report
            .bad_kind
            .contains(&("lua_thing".to_string(), "on".to_string())));
    }

    #[test]
    fn kebab_converts_pascal_case_correctly() {
        assert_eq!(kebab("Ping"), "ping");
        assert_eq!(kebab("ProfileList"), "profile-list");
        assert_eq!(kebab("InstallShell"), "install-shell");
    }

    #[test]
    fn extracts_cli_commands_with_subcommand_groups_dotted() {
        let got = extract_cli_commands(CLI_SRC);
        let want: BTreeSet<String> = ["ping", "modules.list", "modules.info"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(
            got, want,
            "flat leaf commands stay bare; subcommand-group members get `<group>.<sub>` names"
        );
    }

    #[test]
    fn renamed_cli_command_is_caught_as_drift() {
        // Simulate a contributor renaming the `modules info` subcommand in
        // code (e.g. to `Show`) without updating api-schema.toml.
        let renamed_cli_src = CLI_SRC.replace("Info { name: String }", "Show { name: String }");
        let report = check(
            LUA_SRC,
            IPC_SRC,
            &renamed_cli_src,
            &full_schema(),
            DOC_MD,
            README_MD,
        )
        .unwrap();
        assert!(!report.is_clean());
        assert!(report
            .missing_from_schema
            .contains(&("cli_command".to_string(), "modules.show".to_string())));
        assert!(report
            .stale_in_schema
            .contains(&("cli_command".to_string(), "modules.info".to_string())));
    }

    #[test]
    fn missing_readme_cli_line_is_caught_as_drift() {
        // The exact class of drift this coverage exists to catch: a real
        // command exists in code and api-schema.toml, but README's "CLI
        // reference" section never got a line added for it.
        let readme_without_modules_info = README_MD.replace("bread modules info <name>\n", "");
        let report = check(
            LUA_SRC,
            IPC_SRC,
            CLI_SRC,
            &full_schema(),
            DOC_MD,
            &readme_without_modules_info,
        )
        .unwrap();
        assert!(!report.is_clean());
        assert!(report
            .missing_from_readme
            .contains(&("cli_command".to_string(), "modules.info".to_string())));
    }
}
