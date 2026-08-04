//! Wire types shared between `breadd`'s IPC server and the `bread-module-host`
//! client for the out-of-process module bridge (Workstream G).
//!
//! Living here (rather than duplicated as private structs in each crate)
//! means the two processes can't drift on what a `module_host.hello`
//! response or an async event/timer push looks like on the wire — the same
//! failure mode `ModulePermission`/`PermissionKind` already guard against
//! for the manifest schema (see `permissions.rs`).
//!
//! The request side (`{"id", "method", "params"}`) and the plain response
//! side (`{"id", "result"/"error"}`) are *not* duplicated here: they're
//! generic enough (a bare method+params envelope) that both ends already
//! define their own minimal local copy, and sharing a type for something
//! that's just "an id, a string, and a `Value`" buys little. What's shared
//! is the part that's easy to get subtly wrong across two independently
//! maintained crates: the exact shape of the one-time `hello` handshake
//! result and the tagged push-message envelope used for unsolicited
//! event/timer delivery on an otherwise request/response connection.

use serde::{Deserialize, Serialize};

use crate::permissions::ModulePermission;
use crate::BreadEvent;

/// The successful result of a `module_host.hello` call — what `breadd`
/// looked up for the presented one-time token, told back to the
/// `bread-module-host` process that presented it. Deliberately does not
/// trust anything the child process asserts about its own identity (see
/// `breadd/src/module_host.rs`'s token/registry doc comments) — this is
/// `breadd` telling the child who *it* has decided the child is, based on
/// which token was issued for which pending spawn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleHostHello {
    pub module: String,
    pub permissions: Vec<ModulePermission>,
    pub api_version: String,
}

/// An unsolicited message `breadd` pushes down an already-established
/// module-host connection, interleaved with ordinary request/response
/// lines. Distinguished on the wire by the `"push"` tag (internally-tagged
/// enum), which never collides with a plain `{"id", "result"/"error"}`
/// response envelope or an `{"id", "method", "params"}` request envelope —
/// neither of those ever carries a `"push"` key.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "push")]
pub enum ModuleHostPush {
    /// A `bread.on`/`bread.once` subscription (registered via
    /// `module_host.on`/`module_host.once`) matched an event.
    #[serde(rename = "event")]
    Event {
        subscription_id: String,
        event: BreadEvent,
    },
    /// A `bread.after`/`bread.every` timer (registered via
    /// `module_host.after`/`module_host.every`) fired.
    #[serde(rename = "timer")]
    Timer { timer_id: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AdapterSource, PermissionKind};

    #[test]
    fn hello_round_trips() {
        let hello = ModuleHostHello {
            module: "wallpaper".to_string(),
            permissions: vec![ModulePermission {
                kind: PermissionKind::FsRead,
                path: Some("~/Wallpapers".to_string()),
                bin: None,
            }],
            api_version: "1.6.0".to_string(),
        };
        let json = serde_json::to_string(&hello).unwrap();
        let back: ModuleHostHello = serde_json::from_str(&json).unwrap();
        assert_eq!(back.module, "wallpaper");
        assert_eq!(back.permissions.len(), 1);
    }

    #[test]
    fn push_event_tag_is_distinguishable_from_a_response_envelope() {
        let push = ModuleHostPush::Event {
            subscription_id: "sub-1".to_string(),
            event: BreadEvent::new("bread.test.tick", AdapterSource::Manual, serde_json::json!({})),
        };
        let value = serde_json::to_value(&push).unwrap();
        assert_eq!(value.get("push").and_then(|v| v.as_str()), Some("event"));
        // A plain response envelope never has a "push" key — this is the
        // disambiguator bread-module-host's read loop relies on.
        assert!(value.get("id").is_none());
    }

    #[test]
    fn push_timer_round_trips() {
        let push = ModuleHostPush::Timer {
            timer_id: "timer-1".to_string(),
        };
        let json = serde_json::to_string(&push).unwrap();
        let back: ModuleHostPush = serde_json::from_str(&json).unwrap();
        match back {
            ModuleHostPush::Timer { timer_id } => assert_eq!(timer_id, "timer-1"),
            _ => panic!("wrong variant"),
        }
    }
}
