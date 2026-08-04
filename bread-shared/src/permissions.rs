//! Structured module permission types for the capability-scoped module API.
//!
//! This is the `[[permissions]]` schema for `bread.module.toml`. It is shared
//! between `bread-cli` (which parses/writes the manifest on `bread modules
//! install`/`audit`) and `breadd` (which reads the same manifest to build a
//! capability-scoped Lua environment for third-party modules) so the two
//! never drift on what a permission "type" string means — see
//! `Documentation.md`'s "Capability-scoped modules" section for the full
//! baseline-vs-gated taxonomy this enum encodes.

use serde::{Deserialize, Serialize};

/// One `[[permissions]]` entry in a module's `bread.module.toml`, e.g.:
///
/// ```toml
/// [[permissions]]
/// type = "fs.read"
/// path = "~/Wallpapers"
/// ```
///
/// `path`/`bin` are optional scoping metadata (a filesystem path prefix, a
/// state-tree path, or a binary name). **They are not enforced by the
/// in-process Lua environment scoping `breadd` builds today** — that
/// mechanism only gates *presence* of a `bread.*` binding (a module without
/// `fs.read` sees `bread.fs == nil`, full stop). Recording the scoping
/// metadata now means manifests won't need a second migration when the
/// planned out-of-process module sandboxing workstream lands and actually
/// enforces path/bin matching per call — that enforcement is explicitly out
/// of scope here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModulePermission {
    #[serde(rename = "type")]
    pub kind: PermissionKind,
    /// Scoping hint for `fs.read`/`fs.write` (a path prefix) or
    /// `state.read`/`state.watch` (a dotted state-tree path, e.g.
    /// `"monitors"`). Advisory only — see the struct-level doc.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Scoping hint for `exec` (the binary name the module intends to run,
    /// e.g. `"hyprpaper"`). Advisory only — see the struct-level doc.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bin: Option<String>,
}

/// The permission taxonomy covering every capability-gated `bread.*`
/// binding.
///
/// Not covered here because they're **baseline** (always available to every
/// module, gated or not — no real side effect, or a side effect a module
/// can't function at all without): `bread.on`/`once`/`filter`/`off`/`emit`
/// (event subscription is how a module does anything), `bread.after`/
/// `every`/`cancel` (timers), `bread.json` (pure decode), `bread.module`
/// (required just to register), `bread.log`/`warn`/`error` (diagnostics),
/// `bread.debounce`/`spawn`/`wait`/`wait_any`/`wait_all`/`workflow` (pure
/// Lua sugar built entirely on top of the baseline primitives above).
///
/// Gated because they touch the filesystem, spawn processes, control
/// hardware, or otherwise have a real side effect:
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum PermissionKind {
    /// `bread.state.get`/`.monitors`/`.active_workspace`/`.active_window`/
    /// `.devices`/`.power`/`.network`/`.profile` — read-only snapshots of
    /// daemon-maintained runtime state.
    #[serde(rename = "state.read")]
    StateRead,
    /// `bread.state.watch` — a standing subscription to state changes,
    /// gated separately from `state.read` since a long-lived watch is a
    /// more persistent capability than a one-off read.
    #[serde(rename = "state.watch")]
    StateWatch,
    /// `bread.profile.activate` — switches the daemon's system-wide active
    /// profile, a real cross-module side effect.
    #[serde(rename = "profile.activate")]
    ProfileActivate,
    /// `bread.exec` and `bread.exec_capture` — spawns an arbitrary shell
    /// command.
    #[serde(rename = "exec")]
    Exec,
    /// `bread.notify` — sends a desktop notification.
    #[serde(rename = "notify")]
    Notify,
    /// `bread.machine.name`/`.tags`/`.has_tag` — reads hostname/tags,
    /// including an optional on-disk `sync.toml`.
    #[serde(rename = "machine")]
    Machine,
    /// `bread.hyprland.*` — compositor IPC (dispatch/keyword/eval read and
    /// control the running Hyprland session).
    #[serde(rename = "hyprland")]
    Hyprland,
    /// `bread.widget.*` — registers/updates/removes a rendered widget in a
    /// sibling `bread*` app (breadbar).
    #[serde(rename = "widget")]
    Widget,
    /// `bread.fs.read`/`.exists`/`.readlink`/`.expand` — read-only
    /// filesystem access.
    #[serde(rename = "fs.read")]
    FsRead,
    /// `bread.fs.write` — filesystem writes.
    #[serde(rename = "fs.write")]
    FsWrite,
    /// `bread.bluetooth.*` — BlueZ control (power/connect/disconnect/scan).
    #[serde(rename = "bluetooth")]
    Bluetooth,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_round_trips_through_toml_with_dotted_type_names() {
        let toml_src = r#"
            type = "fs.read"
            path = "~/Wallpapers"
        "#;
        let perm: ModulePermission = toml::from_str(toml_src).unwrap();
        assert_eq!(perm.kind, PermissionKind::FsRead);
        assert_eq!(perm.path.as_deref(), Some("~/Wallpapers"));
        assert_eq!(perm.bin, None);
    }

    #[test]
    fn exec_permission_with_bin_round_trips() {
        let toml_src = r#"
            type = "exec"
            bin = "hyprpaper"
        "#;
        let perm: ModulePermission = toml::from_str(toml_src).unwrap();
        assert_eq!(perm.kind, PermissionKind::Exec);
        assert_eq!(perm.bin.as_deref(), Some("hyprpaper"));
    }

    #[test]
    fn permission_list_round_trips_as_array_of_tables() {
        #[derive(Serialize, Deserialize)]
        struct Wrapper {
            #[serde(default)]
            permissions: Option<Vec<ModulePermission>>,
        }

        let w = Wrapper {
            permissions: Some(vec![
                ModulePermission {
                    kind: PermissionKind::StateRead,
                    path: Some("monitors".to_string()),
                    bin: None,
                },
                ModulePermission {
                    kind: PermissionKind::Exec,
                    path: None,
                    bin: Some("hyprpaper".to_string()),
                },
            ]),
        };
        let out = toml::to_string_pretty(&w).unwrap();
        assert!(out.contains("[[permissions]]"));
        assert!(out.contains("type = \"state.read\""));
        assert!(out.contains("type = \"exec\""));

        let back: Wrapper = toml::from_str(&out).unwrap();
        assert_eq!(back.permissions.unwrap().len(), 2);
    }

    #[test]
    fn missing_permissions_field_deserializes_to_none() {
        #[derive(Serialize, Deserialize)]
        struct Wrapper {
            #[serde(default)]
            permissions: Option<Vec<ModulePermission>>,
            name: String,
        }
        let w: Wrapper = toml::from_str("name = \"x\"\n").unwrap();
        assert!(w.permissions.is_none());
    }

    #[test]
    fn explicit_empty_permissions_deserializes_to_some_empty() {
        #[derive(Serialize, Deserialize)]
        struct Wrapper {
            #[serde(default)]
            permissions: Option<Vec<ModulePermission>>,
            name: String,
        }
        let w: Wrapper = toml::from_str("name = \"x\"\npermissions = []\n").unwrap();
        assert_eq!(w.permissions, Some(vec![]));
    }
}
