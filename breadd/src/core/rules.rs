//! `rules.toml` — a declarative shortcut for the common "when event X
//! happens, do Y" case that otherwise requires hand-written Lua
//! (`bread.on(...)`, `bread.exec(...)`, etc).
//!
//! This module owns TOML parsing and validation only; it has no `mlua`
//! dependency so it can be unit-tested in isolation. The `bread.rules`
//! built-in Lua module (`breadd/src/lua/mod.rs`, `BUILTIN_RULES`) is the
//! other half — it takes the [`ParsedRule`]s this module produces and turns
//! each one into a real `bread.on()` subscription.
//!
//! Schema:
//!
//! ```toml
//! [[rule]]
//! on = "device.dock.connected"       # matched against "bread." .. on, wildcards allowed
//! run = "~/.config/bread/scripts/dock-connected.sh"
//!
//! [[rule]]
//! on = "power.ac.disconnected"
//! notify = "Unplugged"
//!
//! [[rule]]
//! on = "device.keyboard.connected"
//! exec = "xset r rate 200 40"
//! ```
//!
//! Exactly one of `run` / `notify` / `exec` must be set per rule. `run` and
//! `exec` both ultimately shell out via `bread.exec()`, but with distinct
//! semantics documented on [`RuleAction`].

use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
struct RulesFile {
    #[serde(default, rename = "rule")]
    rule: Vec<RawRule>,
}

#[derive(Debug, Default, Deserialize)]
struct RawRule {
    on: Option<String>,
    run: Option<String>,
    notify: Option<String>,
    exec: Option<String>,
}

/// The action a validated rule fires when its event matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleAction {
    /// Path to a single script/executable. Treated as exactly one program —
    /// tilde-expanded and shell-quoted as a whole before being handed to
    /// `bread.exec()`, so a path containing spaces still runs as one file
    /// rather than being word-split into a command plus arguments.
    Run(String),
    /// A raw shell command line, passed to `bread.exec()` verbatim (same as
    /// calling `bread.exec()` from hand-written Lua) — you're responsible
    /// for quoting/escaping exactly as if you'd typed it in a shell.
    Exec(String),
    /// A desktop notification message, passed to `bread.notify()`.
    Notify(String),
}

/// A `[[rule]]` entry that passed validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedRule {
    /// Event-name suffix, e.g. `"device.dock.connected"`. The real
    /// subscription is `"bread." .. on` — wildcards (`*`, `**`, `?`) are
    /// whatever `bread.on()` itself supports, since this is handed straight
    /// through.
    pub on: String,
    pub action: RuleAction,
}

/// A `[[rule]]` entry that failed validation — reported, not silently
/// dropped, so it shows up via `bread doctor` the same way a broken Lua
/// module would.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleIssue {
    /// Zero-based position of the `[[rule]]` table in the file.
    pub index: usize,
    /// The rule's `on` value, if it had one (helps identify *which* rule in
    /// a large file when `on` itself isn't the problem).
    pub on: Option<String>,
    pub message: String,
}

impl std::fmt::Display for RuleIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.on {
            Some(on) => write!(f, "rule #{} (on = \"{}\"): {}", self.index, on, self.message),
            None => write!(f, "rule #{}: {}", self.index, self.message),
        }
    }
}

/// Result of attempting to load `rules.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RulesLoadOutcome {
    /// The file doesn't exist. Not an error — `rules.toml` is entirely
    /// optional and purely additive alongside `init.lua`.
    Absent,
    /// The file exists but couldn't be read, or doesn't parse as TOML at
    /// all — no rules could be recovered from it.
    Fatal(String),
    /// The file parsed as TOML. `rules` are the entries that passed
    /// validation (register these); `issues` describes any `[[rule]]`
    /// entries that didn't (report these, but they don't block the rest of
    /// the file from working).
    Loaded {
        rules: Vec<ParsedRule>,
        issues: Vec<RuleIssue>,
    },
}

/// Location of `rules.toml` — re-exported from `core::config` (single
/// source of truth, colocated with `config_path()`'s identical
/// `XDG_CONFIG_HOME`-vs-`HOME` resolution for `breadd.toml`) so callers that
/// only care about rules loading can reach it as `rules::rules_path()`.
pub use crate::core::config::rules_path;

/// Reads and validates `rules.toml` at `path`. Never panics — every failure
/// mode (missing file, unreadable file, invalid TOML, invalid individual
/// rules) is represented in the returned [`RulesLoadOutcome`].
pub fn load_rules(path: &Path) -> RulesLoadOutcome {
    if !path.exists() {
        return RulesLoadOutcome::Absent;
    }

    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => return RulesLoadOutcome::Fatal(format!("failed to read rules.toml: {e}")),
    };

    let parsed: RulesFile = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(e) => return RulesLoadOutcome::Fatal(format!("failed to parse rules.toml: {e}")),
    };

    let mut rules = Vec::new();
    let mut issues = Vec::new();
    for (index, raw_rule) in parsed.rule.into_iter().enumerate() {
        match validate_rule(index, raw_rule) {
            Ok(rule) => rules.push(rule),
            Err(issue) => issues.push(issue),
        }
    }

    RulesLoadOutcome::Loaded { rules, issues }
}

fn validate_rule(index: usize, raw: RawRule) -> Result<ParsedRule, RuleIssue> {
    let on = raw.on.filter(|s| !s.trim().is_empty());

    let mut present = Vec::new();
    if raw.run.is_some() {
        present.push("run");
    }
    if raw.notify.is_some() {
        present.push("notify");
    }
    if raw.exec.is_some() {
        present.push("exec");
    }

    let Some(on) = on else {
        return Err(RuleIssue {
            index,
            on: None,
            message: "missing or empty `on`".to_string(),
        });
    };

    if present.is_empty() {
        return Err(RuleIssue {
            index,
            on: Some(on),
            message: "must set exactly one of `run`, `notify`, `exec` (none set)".to_string(),
        });
    }
    if present.len() > 1 {
        return Err(RuleIssue {
            index,
            on: Some(on),
            message: format!(
                "must set exactly one of `run`, `notify`, `exec` (found: {})",
                present.join(", ")
            ),
        });
    }

    let action = if let Some(v) = raw.run {
        RuleAction::Run(v)
    } else if let Some(v) = raw.notify {
        RuleAction::Notify(v)
    } else {
        RuleAction::Exec(raw.exec.expect("exactly one action present"))
    };

    Ok(ParsedRule { on, action })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    fn write_temp(contents: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("rules.toml");
        let mut f = std::fs::File::create(&path).expect("create rules.toml");
        f.write_all(contents.as_bytes()).expect("write rules.toml");
        (dir, path)
    }

    #[test]
    fn absent_file_is_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("does-not-exist.toml");
        assert_eq!(load_rules(&path), RulesLoadOutcome::Absent);
    }

    #[test]
    fn invalid_toml_is_fatal() {
        let (_dir, path) = write_temp("[[rule\nbroken");
        match load_rules(&path) {
            RulesLoadOutcome::Fatal(msg) => assert!(msg.contains("failed to parse")),
            other => panic!("expected Fatal, got {other:?}"),
        }
    }

    #[test]
    fn empty_file_loads_with_no_rules() {
        let (_dir, path) = write_temp("");
        assert_eq!(
            load_rules(&path),
            RulesLoadOutcome::Loaded {
                rules: vec![],
                issues: vec![],
            }
        );
    }

    #[test]
    fn well_formed_rules_all_three_action_kinds_parse() {
        let (_dir, path) = write_temp(
            r#"
[[rule]]
on = "device.dock.connected"
run = "~/.config/bread/scripts/dock-connected.sh"

[[rule]]
on = "power.ac.disconnected"
notify = "Unplugged"

[[rule]]
on = "device.keyboard.connected"
exec = "xset r rate 200 40"
"#,
        );
        let RulesLoadOutcome::Loaded { rules, issues } = load_rules(&path) else {
            panic!("expected Loaded");
        };
        assert!(issues.is_empty());
        assert_eq!(
            rules,
            vec![
                ParsedRule {
                    on: "device.dock.connected".to_string(),
                    action: RuleAction::Run(
                        "~/.config/bread/scripts/dock-connected.sh".to_string()
                    ),
                },
                ParsedRule {
                    on: "power.ac.disconnected".to_string(),
                    action: RuleAction::Notify("Unplugged".to_string()),
                },
                ParsedRule {
                    on: "device.keyboard.connected".to_string(),
                    action: RuleAction::Exec("xset r rate 200 40".to_string()),
                },
            ]
        );
    }

    #[test]
    fn wildcard_on_is_passed_through_unvalidated() {
        let (_dir, path) = write_temp(
            r#"
[[rule]]
on = "device.*.connected"
exec = "true"
"#,
        );
        let RulesLoadOutcome::Loaded { rules, issues } = load_rules(&path) else {
            panic!("expected Loaded");
        };
        assert!(issues.is_empty());
        assert_eq!(rules[0].on, "device.*.connected");
    }

    #[test]
    fn missing_on_is_reported_with_index_and_no_on() {
        let (_dir, path) = write_temp(
            r#"
[[rule]]
exec = "true"
"#,
        );
        let RulesLoadOutcome::Loaded { rules, issues } = load_rules(&path) else {
            panic!("expected Loaded");
        };
        assert!(rules.is_empty());
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].index, 0);
        assert_eq!(issues[0].on, None);
        assert!(issues[0].message.contains("missing or empty"));
    }

    #[test]
    fn empty_on_is_treated_as_missing() {
        let (_dir, path) = write_temp(
            r#"
[[rule]]
on = "   "
exec = "true"
"#,
        );
        let RulesLoadOutcome::Loaded { issues, .. } = load_rules(&path) else {
            panic!("expected Loaded");
        };
        assert_eq!(issues.len(), 1);
        assert!(issues[0].message.contains("missing or empty"));
    }

    #[test]
    fn zero_action_keys_is_reported() {
        let (_dir, path) = write_temp(
            r#"
[[rule]]
on = "power.ac.disconnected"
"#,
        );
        let RulesLoadOutcome::Loaded { rules, issues } = load_rules(&path) else {
            panic!("expected Loaded");
        };
        assert!(rules.is_empty());
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].on.as_deref(), Some("power.ac.disconnected"));
        assert!(issues[0].message.contains("none set"));
    }

    #[test]
    fn multiple_action_keys_is_reported() {
        let (_dir, path) = write_temp(
            r#"
[[rule]]
on = "power.ac.disconnected"
notify = "Unplugged"
exec = "true"
"#,
        );
        let RulesLoadOutcome::Loaded { rules, issues } = load_rules(&path) else {
            panic!("expected Loaded");
        };
        assert!(rules.is_empty());
        assert_eq!(issues.len(), 1);
        assert!(issues[0].message.contains("notify"));
        assert!(issues[0].message.contains("exec"));
    }

    #[test]
    fn one_bad_rule_does_not_block_other_valid_rules() {
        let (_dir, path) = write_temp(
            r#"
[[rule]]
on = "device.dock.connected"
exec = "true"

[[rule]]
notify = "no on here"

[[rule]]
on = "power.ac.disconnected"
notify = "Unplugged"
"#,
        );
        let RulesLoadOutcome::Loaded { rules, issues } = load_rules(&path) else {
            panic!("expected Loaded");
        };
        assert_eq!(rules.len(), 2);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].index, 1);
    }

    #[test]
    fn rule_issue_display_includes_index_and_on() {
        let issue = RuleIssue {
            index: 2,
            on: Some("power.ac.disconnected".to_string()),
            message: "must set exactly one of `run`, `notify`, `exec` (none set)".to_string(),
        };
        assert_eq!(
            issue.to_string(),
            "rule #2 (on = \"power.ac.disconnected\"): must set exactly one of `run`, `notify`, `exec` (none set)"
        );
    }
}
