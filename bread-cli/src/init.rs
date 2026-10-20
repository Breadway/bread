//! `bread init` installs bread's Hyprland compositor integration: the
//! `layerrule`s (blur, ignore_alpha, blur_popups, animation) that make
//! breadbar/breadbox actually look like the translucent shell they're
//! designed as, generated from the active shell theme's `[compositor]`
//! table via `bread-theme layerrules` → `~/.config/hypr/layerrules.json`.
//!
//! # Why this command auto-edits a file, unlike `hooks_shell`/`hooks_git`
//!
//! Read `hooks_shell.rs`'s module header first: it lays out the general
//! rule this repo follows: bread does not silently edit a user's own
//! startup/config files, because that kind of edit is hard to notice and
//! hard to undo. This module is the one deliberate exception, and it is
//! narrow and consent-gated on purpose:
//!
//! - A missing shell hook is invisible enrichment: the shell just doesn't
//!   report telemetry, nothing looks broken, and the fix (source a line)
//!   is something the user can take at their own pace.
//! - A missing compositor layer rule is not invisible. breadbar renders
//!   without blur, without `ignore_alpha`, motion is wrong; it looks like
//!   a broken app, not a missing optional feature, and a user hitting that
//!   has no way to know the fix is "hand-write five `hl.layer_rule` calls
//!   into your Hyprland config." The gap between "install the bread stack"
//!   and "the shell looks right" needs to be closable in one command.
//!
//! So: this module *does* propose an edit to the user's own config, but
//! only ever one specific, clearly marked, single block; only after
//! showing the exact diff; only with the user's consent (an interactive
//! prompt, `--yes`, or nothing at all in a non-interactive session); always
//! behind a timestamped backup; and always reversible with `bread init
//! --undo`. Every actual line of layer-rule logic lives in a separate,
//! entirely bread-owned file (`~/.config/hypr/bread.lua`) that is
//! regenerated freely and never hand-edited; the one line touched in the
//! user's own `hyprland.lua` just sources it.
//!
//! # Scope: appearance only
//!
//! `bread.lua` (see [`BREAD_LUA`]) emits `hl.layer_rule` calls built from
//! exactly five fields: `blur`, `ignore_alpha`, `blur_popups`, `animation`,
//! `no_anim`. It never reads or emits window placement, workspace
//! assignment, or focus rules; those are `hl.window_rule` policy, are
//! never part of `layerrules.json`, and are explicitly out of scope for
//! this file even if a future `layerrules.json` somehow grew such a key.
//! An installer that rearranged a user's workspaces on upgrade would be a
//! serious bug; this module is structured so it physically cannot do that.
//!
//! # CLI shape: top-level `Init`, not `HooksCommand::Compositor`
//!
//! `hooks install-shell`/`hooks install-git` share one shape: "generate
//! some bread-owned files, print instructions, never touch the user's own
//! config." This command's shape is different in a way that matters enough
//! to warrant its own top-level verb rather than a third `hooks install`
//! variant: it *proposes an edit to a file it doesn't own*, which needs a
//! diff, a consent gate, a backup, and a symmetrical `--undo`; none of
//! which the `hooks` family has or needs. Folding it into `HooksCommand`
//! would either bolt consent/undo semantics onto a subcommand group that
//! otherwise has neither, or quietly suggest to a reader that hooks and
//! compositor integration are the same kind of operation, when the whole
//! point of this module's design is that they aren't.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, IsTerminal, Write as IoWrite};
use std::path::{Path, PathBuf};

/// Start-of-block marker written into the user's `hyprland.lua`. Both this
/// and [`MARKER_END`] must appear verbatim (as a whole trimmed line) for a
/// block to be recognized as bread-managed and therefore safe to update or
/// remove; text near, but not matching exactly, is left alone.
pub const MARKER_BEGIN: &str = "-- >>> bread managed >>>";
/// End-of-block marker. See [`MARKER_BEGIN`].
pub const MARKER_END: &str = "-- <<< bread managed <<<";

/// The single line inserted between the markers. Deliberately just a
/// `pcall(dofile(...))`: every actual rule lives in `bread.lua`, guarded
/// by its own internal `pcall` too (belt and suspenders: this call site
/// can never abort the rest of `hyprland.lua` even if `bread.lua` itself
/// somehow threw outside its own guard, e.g. a syntax error from a
/// half-written file).
fn managed_block_body() -> &'static str {
    "pcall(dofile, os.getenv(\"HOME\") .. \"/.config/hypr/bread.lua\") -- bread compositor integration; installed by `bread init`, removed by `bread init --undo`"
}

fn managed_block_text() -> String {
    format!("{MARKER_BEGIN}\n{}\n{MARKER_END}", managed_block_body())
}

// ---------------------------------------------------------------------------
// bread.lua: the entirely bread-owned generated file
// ---------------------------------------------------------------------------

/// Full contents written to `~/.config/hypr/bread.lua`.
///
/// Self-contained on purpose: it bundles its own minimal JSON object
/// decoder rather than `dofile`-ing `scripts/lib/json.lua`, because that
/// file is a BOS/this-machine convention, not something every Hyprland+Lua
/// user has. The decoder only needs to handle what `layerrules.json` can
/// contain (a flat object of objects with string/number/bool/null leaves).
/// See `bread-theme layerrules`'s writer for the schema this reads.
///
/// The whole body runs inside one `pcall`. A missing `layerrules.json`
/// (bread-theme layerrules never run), an unreadable one, or a malformed
/// one all degrade to "no bread layer rules applied this session", never
/// to a Hyprland config that fails to load.
const BREAD_LUA: &str = r#"-- ~/.config/hypr/bread.lua
--
-- Generated by `bread init`. DO NOT HAND-EDIT. Rerun `bread init` any
-- time to regenerate this file from the current layerrules.json, or
-- `bread init --undo` to remove the one line in hyprland.lua that sources
-- it (this file itself is left in place, inert, until you delete it).
--
-- Reads ~/.config/hypr/layerrules.json (written by `bread-theme
-- layerrules` from the active shell theme's [compositor] table) and emits
-- one hl.layer_rule() per namespace it describes.
--
-- Appearance only: blur, ignore_alpha, blur_popups, animation, no_anim.
-- This file must never grow window placement, workspace assignment, or
-- focus rules; those are hl.window_rule policy and belong in your own
-- hyprland.lua, not here. Only these five fields are ever read off each
-- entry below; any other key in layerrules.json (there should not be any)
-- is silently ignored.

local ok, err = pcall(function()
    local path = os.getenv("HOME") .. "/.config/hypr/layerrules.json"

    local fh = io.open(path, "r")
    if not fh then
        return -- no layerrules.json yet (bread-theme layerrules not run); nothing to do
    end
    local raw = fh:read("*a")
    fh:close()

    -- Minimal JSON decoder, scoped to layerrules.json's shape (an object of
    -- objects; string/number/bool/null leaves; arrays supported for
    -- completeness though the schema never uses one). Not a general-purpose
    -- parser: see BREAD_LUA's doc comment in bread-cli/src/init.rs for why
    -- this doesn't just dofile scripts/lib/json.lua.
    local pos, len = 1, #raw

    local function skip_ws()
        while pos <= len and raw:sub(pos, pos):match("%s") do
            pos = pos + 1
        end
    end

    local parse_value

    local function parse_string()
        pos = pos + 1
        local out = {}
        while pos <= len do
            local c = raw:sub(pos, pos)
            if c == '"' then
                pos = pos + 1
                return table.concat(out)
            elseif c == "\\" then
                local n = raw:sub(pos + 1, pos + 1)
                local escapes = { ['"'] = '"', ["\\"] = "\\", ["/"] = "/", b = "\b", f = "\f", n = "\n", r = "\r", t = "\t" }
                out[#out + 1] = escapes[n] or n
                pos = pos + 2
            else
                out[#out + 1] = c
                pos = pos + 1
            end
        end
        error("unterminated string in layerrules.json")
    end

    local function parse_array()
        pos = pos + 1
        skip_ws()
        local arr = {}
        if raw:sub(pos, pos) == "]" then
            pos = pos + 1
            return arr
        end
        while true do
            skip_ws()
            arr[#arr + 1] = parse_value()
            skip_ws()
            local c = raw:sub(pos, pos)
            pos = pos + 1
            if c == "]" then
                return arr
            elseif c ~= "," then
                error("expected ',' or ']' in layerrules.json array")
            end
        end
    end

    local function parse_object()
        pos = pos + 1
        skip_ws()
        local obj = {}
        if raw:sub(pos, pos) == "}" then
            pos = pos + 1
            return obj
        end
        while true do
            skip_ws()
            local key = parse_string()
            skip_ws()
            if raw:sub(pos, pos) ~= ":" then
                error("expected ':' in layerrules.json object")
            end
            pos = pos + 1
            skip_ws()
            obj[key] = parse_value()
            skip_ws()
            local c = raw:sub(pos, pos)
            pos = pos + 1
            if c == "}" then
                return obj
            elseif c ~= "," then
                error("expected ',' or '}' in layerrules.json object")
            end
        end
    end

    parse_value = function()
        skip_ws()
        local c = raw:sub(pos, pos)
        if c == '"' then
            return parse_string()
        elseif c == "{" then
            return parse_object()
        elseif c == "[" then
            return parse_array()
        elseif raw:sub(pos, pos + 3) == "true" then
            pos = pos + 4
            return true
        elseif raw:sub(pos, pos + 4) == "false" then
            pos = pos + 5
            return false
        elseif raw:sub(pos, pos + 3) == "null" then
            pos = pos + 4
            return nil
        else
            local start = pos
            while pos <= len and raw:sub(pos, pos):match("[%d%+%-.eE]") do
                pos = pos + 1
            end
            if pos == start then
                error("unexpected character in layerrules.json at byte " .. pos)
            end
            return tonumber(raw:sub(start, pos - 1))
        end
    end

    local parsed = parse_value()
    if type(parsed) ~= "table" then
        return
    end

    local namespaces = {}
    for ns, rule in pairs(parsed) do
        if type(ns) == "string" and type(rule) == "table" then
            namespaces[#namespaces + 1] = ns
        end
    end
    table.sort(namespaces) -- deterministic emission order

    for _, ns in ipairs(namespaces) do
        local r = parsed[ns]
        hl.layer_rule({
            name         = ns,
            match        = { namespace = "^" .. ns .. "$" },
            blur         = r.blur == true,
            ignore_alpha = r.ignore_alpha,
            blur_popups  = r.blur_popups == true,
            animation    = r.animation,
            no_anim      = r.no_anim == true,
        })
    end
end)

if not ok then
    -- Never let a bad layerrules.json break the rest of the compositor
    -- config; Hyprland just runs without bread's layer rules until the
    -- underlying file is fixed (or regenerated with `bread-theme layerrules`).
    io.stderr:write("bread.lua: layer rules not applied: " .. tostring(err) .. "\n")
end
"#;

// ---------------------------------------------------------------------------
// Config layout detection
// ---------------------------------------------------------------------------

/// What we found at `~/.config/hypr` and whether it's something `bread
/// init` can safely auto-edit.
#[derive(Debug, PartialEq, Eq)]
enum ConfigLayout {
    /// `hyprland.lua` exists and is readable as UTF-8 text: the only
    /// layout this command will ever write to.
    Lua(PathBuf),
    /// No `hyprland.lua`, but `hyprland.conf` exists: the legacy hyprlang
    /// format. Never auto-edited (a `dofile`-based integration can't work
    /// there anyway; conf and Lua are different, mutually exclusive config
    /// entry points).
    ConfOnly(PathBuf),
    /// Neither recognized, or `hyprland.lua` exists but couldn't be read as
    /// text (permissions, binary garbage, etc). Conservative default:
    /// print instructions, write nothing.
    Unrecognized,
}

fn detect_layout(hypr_dir: &Path) -> ConfigLayout {
    let lua_path = hypr_dir.join("hyprland.lua");
    let conf_path = hypr_dir.join("hyprland.conf");

    if lua_path.is_file() {
        return if fs::read_to_string(&lua_path).is_ok() {
            ConfigLayout::Lua(lua_path)
        } else {
            ConfigLayout::Unrecognized
        };
    }
    if conf_path.is_file() {
        return ConfigLayout::ConfOnly(conf_path);
    }
    ConfigLayout::Unrecognized
}

// ---------------------------------------------------------------------------
// BOS / existing-owner conflict detection
// ---------------------------------------------------------------------------

/// How deep to recurse under `~/.config/hypr` while scanning for an
/// existing `breadbar` layer rule. Real trees here are a handful of levels
/// (`scripts/ui/rules.lua`, etc); this is just a hang-safety bound.
const CONFLICT_SCAN_MAX_DEPTH: usize = 8;

/// Look for a `breadbar*` layer rule bread doesn't own: either BOS's own
/// dotfiles or a user's hand-authored one (this machine's
/// `scripts/ui/rules.lua` is exactly this case: `hl.layer_rule({ name =
/// "breadbar-island", match = { namespace = "^breadbar$" }, ... })`).
///
/// Detection is deliberately heuristic and file-scoped rather than a real
/// Lua/hyprlang parse: any `.lua` or `.conf` file under `hypr_dir` (other
/// than bread's own generated `bread.lua`) that mentions both a layer-rule
/// keyword (`layer_rule` for Lua, `layerrule` for hyprlang conf) and
/// `breadbar` is treated as an existing owner. False positives (a file
/// that mentions both words without actually defining a breadbar layer
/// rule) are the safe failure mode here: worst case bread declines to
/// install and points at a red herring, which is recoverable by hand;
/// installing a second, conflicting rule set is not.
fn find_breadbar_conflict(hypr_dir: &Path) -> Result<Option<PathBuf>> {
    if !hypr_dir.is_dir() {
        return Ok(None);
    }
    scan_dir_for_conflict(hypr_dir, 0)
}

fn scan_dir_for_conflict(dir: &Path, depth: usize) -> Result<Option<PathBuf>> {
    if depth > CONFLICT_SCAN_MAX_DEPTH {
        return Ok(None);
    }
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(None), // an unreadable subdir shouldn't abort the whole command
    };
    for entry in entries {
        let entry =
            entry.context("reading directory entry while scanning for compositor conflicts")?;
        let path = entry.path();
        let file_type = entry.file_type()?;

        if file_type.is_dir() {
            if let Some(found) = scan_dir_for_conflict(&path, depth + 1)? {
                return Ok(Some(found));
            }
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        if path.file_name().and_then(|n| n.to_str()) == Some("bread.lua") {
            continue; // our own generated file, never a conflict with itself
        }
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext != "lua" && ext != "conf" {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        if mentions_breadbar_layer_rule(&content) {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

fn mentions_breadbar_layer_rule(content: &str) -> bool {
    let lower = content.to_ascii_lowercase();
    let has_layer_rule_keyword = lower.contains("layer_rule") || lower.contains("layerrule");
    has_layer_rule_keyword && lower.contains("breadbar")
}

// ---------------------------------------------------------------------------
// Managed-block find / plan (pure: operate on strings only)
// ---------------------------------------------------------------------------

/// Locate the bread-managed block's (start, end) 0-based *line* indices in
/// `content`, if present. A line must match a marker exactly after
/// trimming; a passing mention of the marker text mid-line is not enough,
/// mirroring `hooks_git.rs`'s `is_bread_managed`.
fn find_managed_block(content: &str) -> Option<(usize, usize)> {
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.iter().position(|l| l.trim() == MARKER_BEGIN)?;
    let end = lines[start..].iter().position(|l| l.trim() == MARKER_END)? + start;
    Some((start, end))
}

/// Outcome of planning an install edit against a file's current content.
#[derive(Debug, PartialEq, Eq)]
enum ChangeKind {
    /// A bread block is already present and byte-identical to what we'd
    /// write: nothing to change in `hyprland.lua` (`bread.lua` itself is
    /// still freely refreshed by the caller).
    Unchanged,
    /// No existing block found; the new one will be appended at the end.
    Appended,
    /// An existing block was found at these 0-based line indices and will
    /// be replaced in place (its content differs from what we'd write now,
    /// e.g. an older bread version's block).
    Replaced { start: usize, end: usize },
}

/// Compute the new file content and what kind of change that represents,
/// without touching the filesystem.
fn plan_install(content: &str) -> (String, ChangeKind) {
    let block = managed_block_text();

    if let Some((start, end)) = find_managed_block(content) {
        let lines: Vec<&str> = content.lines().collect();
        let existing_block = lines[start..=end].join("\n");
        if existing_block == block {
            return (content.to_string(), ChangeKind::Unchanged);
        }

        let mut new_lines: Vec<&str> = Vec::with_capacity(lines.len());
        new_lines.extend_from_slice(&lines[..start]);
        let block_lines: Vec<&str> = block.lines().collect();
        new_lines.extend_from_slice(&block_lines);
        new_lines.extend_from_slice(&lines[end + 1..]);

        let mut new_content = new_lines.join("\n");
        if content.ends_with('\n') {
            new_content.push('\n');
        }
        (new_content, ChangeKind::Replaced { start, end })
    } else {
        let mut new_content = content.to_string();
        if !new_content.is_empty() && !new_content.ends_with('\n') {
            new_content.push('\n');
        }
        if !new_content.is_empty() {
            new_content.push('\n'); // blank separator before the appended block
        }
        new_content.push_str(&block);
        new_content.push('\n');
        (new_content, ChangeKind::Appended)
    }
}

/// Compute the content with the bread-managed block removed, if one is
/// present. Also drops a single blank separator line immediately before
/// the block, if there is one, to avoid leaving a stray blank line behind
/// on every round trip.
fn plan_undo(content: &str) -> Option<String> {
    let (start, end) = find_managed_block(content)?;
    let lines: Vec<&str> = content.lines().collect();

    let mut new_lines: Vec<&str> = Vec::with_capacity(lines.len());
    new_lines.extend_from_slice(&lines[..start]);
    new_lines.extend_from_slice(&lines[end + 1..]);

    if start > 0 && lines[start - 1].trim().is_empty() {
        // Remove the one separator blank line `plan_install` would have
        // added, if it's still the line immediately preceding the block.
        let idx_in_new = start - 1;
        if new_lines
            .get(idx_in_new)
            .is_some_and(|l| l.trim().is_empty())
        {
            new_lines.remove(idx_in_new);
        }
    }

    let mut new_content = new_lines.join("\n");
    if content.ends_with('\n') && !new_content.is_empty() {
        new_content.push('\n');
    }
    Some(new_content)
}

// ---------------------------------------------------------------------------
// layerrules.json → hyprlang `.conf` snippet (for the print-only fallback)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Default)]
struct LayerRuleEntry {
    #[serde(default)]
    blur: bool,
    ignore_alpha: Option<f64>,
    #[serde(default)]
    blur_popups: bool,
    animation: Option<String>,
    #[serde(default)]
    no_anim: bool,
}

/// `BTreeMap` so iteration order (used when rendering the snippet) is
/// deterministic without a separate sort step.
type LayerRulesFile = BTreeMap<String, LayerRuleEntry>;

/// Read and parse `~/.config/hypr/layerrules.json`.
///
/// `None` means the file doesn't exist (not an error: `bread-theme
/// layerrules` just hasn't been run yet). `Some(Err(..))` means it exists
/// but couldn't be read or parsed; the message is meant to be shown to the
/// user as-is.
fn read_layerrules_json(hypr_dir: &Path) -> Option<Result<LayerRulesFile, String>> {
    let path = hypr_dir.join("layerrules.json");
    if !path.exists() {
        return None;
    }
    match fs::read_to_string(&path) {
        Err(e) => Some(Err(format!("could not read {}: {e}", path.display()))),
        Ok(raw) => match serde_json::from_str::<LayerRulesFile>(&raw) {
            Ok(v) => Some(Ok(v)),
            Err(e) => Some(Err(format!(
                "could not parse {} as JSON: {e}",
                path.display()
            ))),
        },
    }
}

/// Render `rules` as hyprlang `.conf` `layerrule` lines: the manual
/// snippet printed for a `hyprland.conf`-only or unrecognized layout. Same
/// five fields as `bread.lua`, nothing else.
fn render_conf_snippet(rules: &LayerRulesFile) -> String {
    let mut out = String::new();
    for (ns, r) in rules {
        let pattern = format!("^{ns}$");
        if r.no_anim {
            out.push_str(&format!("layerrule = noanim,{pattern}\n"));
        }
        if r.blur {
            out.push_str(&format!("layerrule = blur,{pattern}\n"));
        }
        if let Some(alpha) = r.ignore_alpha {
            out.push_str(&format!("layerrule = ignorealpha {alpha},{pattern}\n"));
        }
        if r.blur_popups {
            out.push_str(&format!("layerrule = blurpopups,{pattern}\n"));
        }
        if let Some(animation) = &r.animation {
            out.push_str(&format!("layerrule = animation {animation},{pattern}\n"));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Consent
// ---------------------------------------------------------------------------

enum Consent {
    Yes,
    No,
    /// No TTY on stdin and `--yes` wasn't given; per spec, this must never
    /// write.
    NonInteractive,
}

fn ask_consent(yes: bool, prompt: &str) -> Result<Consent> {
    if yes {
        return Ok(Consent::Yes);
    }
    if !io::stdin().is_terminal() {
        return Ok(Consent::NonInteractive);
    }
    print!("{prompt} [y/N] ");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    if line.trim().eq_ignore_ascii_case("y") {
        Ok(Consent::Yes)
    } else {
        Ok(Consent::No)
    }
}

// ---------------------------------------------------------------------------
// Backups
// ---------------------------------------------------------------------------

/// A timestamped backup path alongside `original`, disambiguated with a
/// numeric suffix in the unlikely case two runs land in the same second.
fn unique_backup_path(original: &Path) -> PathBuf {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let base = format!("{}.bak.{stamp}", original.display());
    let mut candidate = PathBuf::from(&base);
    let mut n = 1u32;
    while candidate.exists() {
        candidate = PathBuf::from(format!("{base}.{n}"));
        n += 1;
    }
    candidate
}

// ---------------------------------------------------------------------------
// Diff / message rendering
// ---------------------------------------------------------------------------

fn print_proposed_change(path: &Path, original: &str, new_content: &str, change: &ChangeKind) {
    println!("Proposed change to {}:", path.display());
    println!();
    match change {
        ChangeKind::Unchanged => {
            println!("  (already installed: no change needed)");
        }
        ChangeKind::Appended => {
            let (start, end) = find_managed_block(new_content).expect("block was just appended");
            let new_lines: Vec<&str> = new_content.lines().collect();
            println!(
                "  appended at end of file (new lines {}-{}):",
                start + 1,
                end + 1
            );
            println!();
            for (i, line) in new_lines[start..=end].iter().enumerate() {
                println!("  + {:>4}  {}", start + 1 + i, line);
            }
        }
        ChangeKind::Replaced { start, end } => {
            let old_lines: Vec<&str> = original.lines().collect();
            println!(
                "  existing bread block at lines {}-{} will be updated:",
                start + 1,
                end + 1
            );
            println!();
            for (i, line) in old_lines[*start..=*end].iter().enumerate() {
                println!("  - {:>4}  {}", start + 1 + i, line);
            }
            let (nstart, nend) = find_managed_block(new_content).expect("block was just replaced");
            let new_lines: Vec<&str> = new_content.lines().collect();
            for (i, line) in new_lines[nstart..=nend].iter().enumerate() {
                println!("  + {:>4}  {}", nstart + 1 + i, line);
            }
        }
    }
    println!();
}

fn print_bos_conflict(path: &Path) {
    println!(
        "bread init: found an existing breadbar layer rule in {}, refusing to install.",
        path.display()
    );
    println!(
        "Two owners writing the same Hyprland layer rules is how they end up fighting each \
         other on reload. That file already provides this behavior (BOS's own dotfiles, or \
         something hand-authored). Edit it directly if you want to change the rules, or \
         remove it first if you'd rather have `bread init` manage this from now on."
    );
}

enum ManualReason<'a> {
    ConfOnly(&'a Path),
    Unrecognized,
    LuaUnreadable(&'a Path),
}

fn print_manual_snippet(hypr_dir: &Path, reason: ManualReason) {
    match reason {
        ManualReason::ConfOnly(path) => {
            println!(
                "bread init: found {} but no hyprland.lua; bread only auto-edits the Lua-based \
                 Hyprland config, never .conf (they're different, mutually exclusive config \
                 entry points), so nothing was written.",
                path.display()
            );
        }
        ManualReason::Unrecognized => {
            println!(
                "bread init: no recognized Hyprland config found at {} (looked for \
                 hyprland.lua, hyprland.conf). Nothing was written.",
                hypr_dir.display()
            );
        }
        ManualReason::LuaUnreadable(path) => {
            println!(
                "bread init: {} exists but could not be read as text; bread only auto-edits a \
                 config it can safely parse, so nothing was written.",
                path.display()
            );
        }
    }
    println!();

    match reason {
        ManualReason::ConfOnly(_) | ManualReason::Unrecognized => {
            match read_layerrules_json(hypr_dir) {
                Some(Ok(rules)) if !rules.is_empty() => {
                    println!(
                        "Paste this into hyprland.conf to get the same blur/opacity behavior:"
                    );
                    println!();
                    print!("{}", render_conf_snippet(&rules));
                }
                Some(Ok(_)) => {
                    println!(
                        "~/.config/hypr/layerrules.json exists but is empty: nothing to paste \
                         yet. Set a theme's [compositor] table and run `bread-theme layerrules`."
                    );
                }
                Some(Err(e)) => {
                    println!(
                        "~/.config/hypr/layerrules.json exists but {e}: fix it and re-run \
                         `bread init`, or run `bread-theme layerrules` to regenerate it."
                    );
                }
                None => {
                    println!(
                        "Run `bread-theme layerrules` first to generate \
                         ~/.config/hypr/layerrules.json from your active theme, then re-run \
                         `bread init` for the exact snippet."
                    );
                }
            }
        }
        ManualReason::LuaUnreadable(_) => {
            println!("If this really is a Lua config, add this near the end of the file:");
            println!();
            println!("{}", managed_block_text());
        }
    }
}

// ---------------------------------------------------------------------------
// Install / undo
// ---------------------------------------------------------------------------

fn write_bread_lua(hypr_dir: &Path) -> Result<PathBuf> {
    fs::create_dir_all(hypr_dir)
        .with_context(|| format!("failed to create {}", hypr_dir.display()))?;
    let path = hypr_dir.join("bread.lua");
    fs::write(&path, BREAD_LUA).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

fn run_install(hypr_dir: &Path, dry_run: bool, yes: bool) -> Result<()> {
    if let Some(existing) = find_breadbar_conflict(hypr_dir)? {
        print_bos_conflict(&existing);
        return Ok(());
    }

    let lua_path = match detect_layout(hypr_dir) {
        ConfigLayout::Lua(path) => path,
        ConfigLayout::ConfOnly(path) => {
            print_manual_snippet(hypr_dir, ManualReason::ConfOnly(&path));
            return Ok(());
        }
        ConfigLayout::Unrecognized => {
            let candidate = hypr_dir.join("hyprland.lua");
            if candidate.exists() {
                print_manual_snippet(hypr_dir, ManualReason::LuaUnreadable(&candidate));
            } else {
                print_manual_snippet(hypr_dir, ManualReason::Unrecognized);
            }
            return Ok(());
        }
    };

    let original = fs::read_to_string(&lua_path)
        .with_context(|| format!("failed to read {}", lua_path.display()))?;
    let (new_content, change) = plan_install(&original);

    if change == ChangeKind::Unchanged {
        println!(
            "bread init: compositor integration already installed in {}.",
            lua_path.display()
        );
        if dry_run {
            println!("(dry run: bread.lua left untouched)");
            return Ok(());
        }
        let bread_lua_path = write_bread_lua(hypr_dir)?;
        println!("bread.lua refreshed at {}", bread_lua_path.display());
        return Ok(());
    }

    print_proposed_change(&lua_path, &original, &new_content, &change);

    if dry_run {
        println!("(dry run: no changes made)");
        return Ok(());
    }

    match ask_consent(yes, "Apply this change?")? {
        Consent::No => {
            println!("aborted: no changes made.");
            return Ok(());
        }
        Consent::NonInteractive => {
            println!(
                "non-interactive session: re-run with --yes to apply this change, or paste \
                 the snippet above yourself."
            );
            return Ok(());
        }
        Consent::Yes => {}
    }

    let bread_lua_path = write_bread_lua(hypr_dir)?;
    let backup = unique_backup_path(&lua_path);
    fs::copy(&lua_path, &backup).with_context(|| {
        format!(
            "failed to back up {} to {}",
            lua_path.display(),
            backup.display()
        )
    })?;
    fs::write(&lua_path, &new_content)
        .with_context(|| format!("failed to write {}", lua_path.display()))?;

    println!("bread init: installed.");
    println!("  wrote    {}", bread_lua_path.display());
    println!("  edited   {}", lua_path.display());
    println!("  backup   {}", backup.display());
    Ok(())
}

fn run_undo(hypr_dir: &Path, dry_run: bool, yes: bool) -> Result<()> {
    let lua_path = match detect_layout(hypr_dir) {
        ConfigLayout::Lua(path) => path,
        _ => {
            println!(
                "bread init --undo: no bread-managed hyprland.lua found at {}, nothing to undo.",
                hypr_dir.display()
            );
            return Ok(());
        }
    };

    let original = fs::read_to_string(&lua_path)
        .with_context(|| format!("failed to read {}", lua_path.display()))?;

    let Some((start, end)) = find_managed_block(&original) else {
        println!(
            "bread init --undo: no bread-managed block found in {}, nothing to undo.",
            lua_path.display()
        );
        return Ok(());
    };

    let new_content = plan_undo(&original).expect("block located just above");

    println!("Proposed change to {}:", lua_path.display());
    println!();
    println!(
        "  bread block at lines {}-{} will be removed:",
        start + 1,
        end + 1
    );
    println!();
    let old_lines: Vec<&str> = original.lines().collect();
    for (i, line) in old_lines[start..=end].iter().enumerate() {
        println!("  - {:>4}  {}", start + 1 + i, line);
    }
    println!();

    if dry_run {
        println!("(dry run: no changes made)");
        return Ok(());
    }

    match ask_consent(yes, "Remove this block?")? {
        Consent::No => {
            println!("aborted: no changes made.");
            return Ok(());
        }
        Consent::NonInteractive => {
            println!("non-interactive session: re-run with --yes to apply this change.");
            return Ok(());
        }
        Consent::Yes => {}
    }

    let backup = unique_backup_path(&lua_path);
    fs::copy(&lua_path, &backup).with_context(|| {
        format!(
            "failed to back up {} to {}",
            lua_path.display(),
            backup.display()
        )
    })?;
    fs::write(&lua_path, &new_content)
        .with_context(|| format!("failed to write {}", lua_path.display()))?;

    println!("bread init --undo: removed.");
    println!("  edited   {}", lua_path.display());
    println!("  backup   {}", backup.display());

    let bread_lua = hypr_dir.join("bread.lua");
    if bread_lua.exists() {
        println!(
            "  note     {} was left in place (harmless: nothing sources it anymore); delete \
             it yourself if you want it gone",
            bread_lua.display()
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// `~/.config/hypr`: Hyprland always reads its config from here; unlike
/// most XDG apps this is not `$XDG_CONFIG_HOME`-relative in practice, so
/// this resolves `$HOME` directly rather than going through
/// `dirs::config_dir()`.
pub fn hypr_dir() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        return home.join(".config").join("hypr");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".config").join("hypr");
    }
    PathBuf::from(".config/hypr")
}

/// `bread init` entry point.
pub fn run(dry_run: bool, yes: bool, undo: bool) -> Result<()> {
    let dir = hypr_dir();
    if undo {
        run_undo(&dir, dry_run, yes)
    } else {
        run_install(&dir, dry_run, yes)
    }
}

/// Status lines for `bread doctor`'s compositor section. Pure
/// filesystem/string inspection: no daemon round trip, so this works even
/// when breadd isn't running, and it never installs anything itself.
pub fn doctor_report(hypr_dir: &Path) -> Vec<String> {
    let mut lines = Vec::new();

    if let Ok(Some(existing)) = find_breadbar_conflict(hypr_dir) {
        lines.push(format!(
            "  compositor  ✓ layer rules provided by {} (not bread-managed)",
            existing.display()
        ));
        return lines;
    }

    match detect_layout(hypr_dir) {
        ConfigLayout::Lua(path) => {
            let content = fs::read_to_string(&path).unwrap_or_default();
            let installed = find_managed_block(&content).is_some();
            let bread_lua_exists = hypr_dir.join("bread.lua").is_file();
            match (installed, bread_lua_exists) {
                (true, true) => lines.push(
                    "  compositor  ✓ installed (bread.lua sourced from hyprland.lua)".to_string(),
                ),
                (true, false) => lines.push(
                    "  compositor  ⚠ hyprland.lua sources bread.lua, but bread.lua is missing: \
                     run `bread init` to regenerate it"
                        .to_string(),
                ),
                (false, _) => lines.push(
                    "  compositor  ✗ not installed: run `bread init` to enable layer-rule \
                     blur/opacity for breadbar"
                        .to_string(),
                ),
            }
        }
        ConfigLayout::ConfOnly(_) => {
            lines.push(
                "  compositor  ✗ not installed (hyprland.conf detected, not the Lua config): \
                 run `bread init` for the layerrule snippet to paste in by hand"
                    .to_string(),
            );
        }
        ConfigLayout::Unrecognized => {
            lines.push(
                "  compositor  ✗ not installed (no recognized Hyprland config found): run \
                 `bread init`"
                    .to_string(),
            );
        }
    }

    let layerrules_json = hypr_dir.join("layerrules.json");
    lines.push(format!(
        "  layerrules  {}",
        if layerrules_json.is_file() {
            format!("present ({})", layerrules_json.display())
        } else {
            "missing: run `bread-theme layerrules`".to_string()
        }
    ));

    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    // -- managed block: find / plan_install / plan_undo -------------------

    #[test]
    fn find_managed_block_locates_exact_markers() {
        let content = "before\n-- >>> bread managed >>>\nfoo\n-- <<< bread managed <<<\nafter\n";
        assert_eq!(find_managed_block(content), Some((1, 3)));
    }

    #[test]
    fn find_managed_block_none_when_absent() {
        assert_eq!(find_managed_block("nothing here\n"), None);
    }

    #[test]
    fn find_managed_block_requires_exact_trimmed_match() {
        let content = "-- this mentions >>> bread managed >>> in passing\n";
        assert_eq!(find_managed_block(content), None);
    }

    #[test]
    fn plan_install_appends_when_absent() {
        let content = "local x = 1\n";
        let (new_content, change) = plan_install(content);
        assert_eq!(change, ChangeKind::Appended);
        assert!(new_content.starts_with("local x = 1\n\n-- >>> bread managed >>>"));
        assert!(new_content.trim_end().ends_with(MARKER_END));
    }

    #[test]
    fn plan_install_is_idempotent() {
        let content = "local x = 1\n";
        let (once, _) = plan_install(content);
        let (twice, change) = plan_install(&once);
        assert_eq!(change, ChangeKind::Unchanged);
        assert_eq!(once, twice);
        // Running install a second (or third) time must never duplicate
        // the block.
        assert_eq!(twice.matches(MARKER_BEGIN).count(), 1);
        assert_eq!(twice.matches(MARKER_END).count(), 1);
    }

    #[test]
    fn plan_install_replaces_stale_block_in_place() {
        let content = format!(
            "local x = 1\n\n{MARKER_BEGIN}\n-- an old/different bread line\n{MARKER_END}\n"
        );
        let (new_content, change) = plan_install(&content);
        assert!(matches!(change, ChangeKind::Replaced { .. }));
        assert_eq!(new_content.matches(MARKER_BEGIN).count(), 1);
        assert!(new_content.contains(managed_block_body()));
        assert!(!new_content.contains("an old/different bread line"));
    }

    #[test]
    fn plan_undo_removes_block_and_separator() {
        let content = "local x = 1\n\n-- >>> bread managed >>>\nfoo\n-- <<< bread managed <<<\n";
        let undone = plan_undo(content).unwrap();
        assert_eq!(undone, "local x = 1\n");
        assert!(!undone.contains(MARKER_BEGIN));
    }

    #[test]
    fn plan_undo_none_when_no_block() {
        assert_eq!(plan_undo("local x = 1\n"), None);
    }

    #[test]
    fn install_then_undo_round_trips_to_appendable_state() {
        let original = "local x = 1\n";
        let (installed, _) = plan_install(original);
        let undone = plan_undo(&installed).unwrap();
        // Not required to be byte-identical to the original (only one
        // blank-line convention is guaranteed to be cleaned up), but the
        // result must contain no trace of the managed block and must be
        // fresh ground for another install.
        assert!(!undone.contains(MARKER_BEGIN));
        assert!(!undone.contains(MARKER_END));
        let (_, change) = plan_install(&undone);
        assert_eq!(change, ChangeKind::Appended);
    }

    // -- config layout detection -------------------------------------------

    #[test]
    fn detect_layout_prefers_lua_when_present() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("hyprland.lua"), "-- lua config\n").unwrap();
        fs::write(
            dir.path().join("hyprland.conf"),
            "monitor=,preferred,auto,1\n",
        )
        .unwrap();
        match detect_layout(dir.path()) {
            ConfigLayout::Lua(p) => assert_eq!(p, dir.path().join("hyprland.lua")),
            other => panic!("expected Lua layout, got {other:?}"),
        }
    }

    #[test]
    fn detect_layout_falls_back_to_conf_only() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("hyprland.conf"),
            "monitor=,preferred,auto,1\n",
        )
        .unwrap();
        match detect_layout(dir.path()) {
            ConfigLayout::ConfOnly(p) => assert_eq!(p, dir.path().join("hyprland.conf")),
            other => panic!("expected ConfOnly layout, got {other:?}"),
        }
    }

    #[test]
    fn detect_layout_unrecognized_when_neither_present() {
        let dir = TempDir::new().unwrap();
        assert_eq!(detect_layout(dir.path()), ConfigLayout::Unrecognized);
    }

    #[test]
    fn detect_layout_unrecognized_when_lua_unreadable_binary() {
        let dir = TempDir::new().unwrap();
        // Invalid UTF-8 bytes: fs::read_to_string will fail on this.
        fs::write(dir.path().join("hyprland.lua"), [0xff, 0xfe, 0x00, 0xff]).unwrap();
        assert_eq!(detect_layout(dir.path()), ConfigLayout::Unrecognized);
    }

    // -- BOS / existing-owner conflict detection ---------------------------

    #[test]
    fn conflict_detected_in_nested_rules_file() {
        let dir = TempDir::new().unwrap();
        let scripts_ui = dir.path().join("scripts").join("ui");
        fs::create_dir_all(&scripts_ui).unwrap();
        fs::write(
            scripts_ui.join("rules.lua"),
            "hl.layer_rule({ name = \"breadbar-island\", match = { namespace = \"^breadbar$\" }, blur = true })\n",
        )
        .unwrap();
        let found = find_breadbar_conflict(dir.path()).unwrap();
        assert_eq!(found, Some(scripts_ui.join("rules.lua")));
    }

    #[test]
    fn conflict_detected_in_conf_style_layerrule() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("hyprland.conf"),
            "layerrule = blur,^(breadbar)$\n",
        )
        .unwrap();
        let found = find_breadbar_conflict(dir.path()).unwrap();
        assert_eq!(found, Some(dir.path().join("hyprland.conf")));
    }

    #[test]
    fn no_conflict_on_unrelated_content() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("hyprland.lua"),
            "hl.window_rule({ class = \"kitty\" })\n",
        )
        .unwrap();
        assert_eq!(find_breadbar_conflict(dir.path()).unwrap(), None);
    }

    #[test]
    fn own_generated_bread_lua_is_never_a_conflict() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("bread.lua"), BREAD_LUA).unwrap();
        // bread.lua emits hl.layer_rule({ name = ns, ... }) for a namespace
        // that could well be literally "breadbar" once layerrules.json has
        // that key: the scan must still skip bread.lua by filename.
        assert_eq!(find_breadbar_conflict(dir.path()).unwrap(), None);
    }

    #[test]
    fn conflict_scan_ignores_non_lua_conf_files() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("notes.txt"),
            "hl.layer_rule breadbar reminder to self\n",
        )
        .unwrap();
        assert_eq!(find_breadbar_conflict(dir.path()).unwrap(), None);
    }

    // -- layerrules.json reading / conf snippet rendering -------------------

    #[test]
    fn read_layerrules_json_missing_file_is_none() {
        let dir = TempDir::new().unwrap();
        assert!(read_layerrules_json(dir.path()).is_none());
    }

    #[test]
    fn read_layerrules_json_malformed_is_reported_not_panicking() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("layerrules.json"), "{ this is not json ").unwrap();
        let result = read_layerrules_json(dir.path());
        assert!(matches!(result, Some(Err(_))));
    }

    #[test]
    fn read_layerrules_json_parses_valid_file() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("layerrules.json"),
            r#"{"breadbar":{"blur":true,"ignore_alpha":0.2,"blur_popups":true,"animation":"slide top","no_anim":false}}"#,
        )
        .unwrap();
        let result = read_layerrules_json(dir.path()).unwrap().unwrap();
        assert_eq!(result.len(), 1);
        assert!(result["breadbar"].blur);
        assert_eq!(result["breadbar"].ignore_alpha, Some(0.2));
    }

    #[test]
    fn render_conf_snippet_emits_expected_lines() {
        let mut rules = LayerRulesFile::new();
        rules.insert(
            "breadbar".to_string(),
            LayerRuleEntry {
                blur: true,
                ignore_alpha: Some(0.2),
                blur_popups: true,
                animation: Some("slide top".to_string()),
                no_anim: false,
            },
        );
        let snippet = render_conf_snippet(&rules);
        assert!(snippet.contains("layerrule = blur,^breadbar$"));
        assert!(snippet.contains("layerrule = ignorealpha 0.2,^breadbar$"));
        assert!(snippet.contains("layerrule = blurpopups,^breadbar$"));
        assert!(snippet.contains("layerrule = animation slide top,^breadbar$"));
        assert!(!snippet.contains("noanim"));
    }

    #[test]
    fn render_conf_snippet_no_anim_only() {
        let mut rules = LayerRulesFile::new();
        rules.insert(
            "breadbar-dismiss".to_string(),
            LayerRuleEntry {
                no_anim: true,
                ..Default::default()
            },
        );
        let snippet = render_conf_snippet(&rules);
        assert_eq!(snippet, "layerrule = noanim,^breadbar-dismiss$\n");
    }

    // -- doctor report --------------------------------------------------

    #[test]
    fn doctor_report_reports_not_installed() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("hyprland.lua"), "-- empty\n").unwrap();
        let lines = doctor_report(dir.path());
        assert!(lines.iter().any(|l| l.contains("not installed")));
    }

    #[test]
    fn doctor_report_reports_installed() {
        let dir = TempDir::new().unwrap();
        let (content, _) = plan_install("-- empty\n");
        fs::write(dir.path().join("hyprland.lua"), content).unwrap();
        fs::write(dir.path().join("bread.lua"), BREAD_LUA).unwrap();
        let lines = doctor_report(dir.path());
        assert!(lines.iter().any(|l| l.contains("✓ installed")));
    }

    #[test]
    fn doctor_report_reports_bos_conflict_distinctly() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("hyprland.lua"),
            "hl.layer_rule({ match = { namespace = \"^breadbar$\" } })\n",
        )
        .unwrap();
        let lines = doctor_report(dir.path());
        assert!(lines.iter().any(|l| l.contains("not bread-managed")));
    }

    // -- backup path uniqueness -----------------------------------------

    #[test]
    fn unique_backup_path_disambiguates_collisions() {
        let dir = TempDir::new().unwrap();
        let original = dir.path().join("hyprland.lua");
        fs::write(&original, "x").unwrap();
        let first = unique_backup_path(&original);
        fs::write(&first, "backup 1").unwrap();
        let second = unique_backup_path(&original);
        assert_ne!(first, second);
    }
}
