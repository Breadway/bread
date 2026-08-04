//! The known-apps registry for sibling `bread*` application integration.
//!
//! Adding a new sibling app is a one-line edit to [`KNOWN_APPS`] — no new
//! `AdapterSource` variant, no new normalizer arm, no recompile-driven
//! exhaustiveness churn. This is what makes the sibling-app integration
//! model extensible: see `Documentation.md`'s "Namespaces" section and
//! "Integrating a bread* app" recipe.

/// Registered sibling `bread*` app ids. Each id is also the app's reserved
/// segment in the `bread.<id>.*` (events) and `bread.command.<id>.*`
/// (commands) namespaces.
pub const KNOWN_APPS: &[&str] = &[
    "clip", "pad", "bar", "box", "lock", "mon", "paper", "search", "shot", "arr", "crumbs", "help",
    "bakery", "cast",
];

/// Daemon-internal domains that are reserved and can never be claimed as an
/// app id, even if a future `bread*` app would otherwise want that name —
/// these are the top-level segments the normalizer and built-in event
/// families already use.
///
/// This is also the single source of truth the IPC boundary checks before
/// allowing a manual (no-`source`) `emit` request to use an event name — a
/// socket client may freely emit a custom/test event, but not one whose
/// top-level segment is one of these, since that would let it impersonate
/// a real adapter (or another daemon-internal event family) rather than
/// producing an obviously-manual one. See [`is_reserved_domain`] and
/// `breadd/src/ipc/mod.rs`'s `emit` handler. *Since: v1.5 — `bluetooth`,
/// `workspace`, `window`, and `monitor` added (event families the Hyprland
/// and Bluetooth adapters already published under, but that were missing
/// from this list) when this became a spoofing-prevention boundary and not
/// just an app-id-conflict one.*
const RESERVED_DOMAINS: &[&str] = &[
    "terminal",
    "git",
    "hyprland",
    "device",
    "power",
    "network",
    "service",
    "container",
    "project",
    "remote",
    "system",
    "profile",
    "notify",
    "command",
    "workflow",
    "bluetooth",
    "workspace",
    "window",
    "monitor",
];

/// Whether `id` is a registered sibling-app id.
pub fn is_known_app(id: &str) -> bool {
    KNOWN_APPS.contains(&id)
}

/// Whether `id` is reserved for daemon-internal use and can never be
/// registered as a sibling-app id.
pub fn is_reserved_domain(id: &str) -> bool {
    RESERVED_DOMAINS.contains(&id)
}

/// Whether `event` is a well-formed event name for `app` — i.e. it starts
/// with `bread.<app>.`. An app may only publish within its own namespace
/// segment; this is what the IPC boundary checks before constructing a
/// `RawEvent` tagged `AdapterSource::App(app)`.
pub fn validate_app_namespace(app: &str, event: &str) -> bool {
    event.starts_with(&format!("bread.{app}."))
}

/// The top-level dotted segment after `bread.` in an event name — e.g.
/// `Some("power")` for `"bread.power.ac.connected"`. Returns `None` for
/// event names that don't start with `bread.` at all, which are always
/// outside any reserved namespace (freely-named custom/test events, the
/// `bread emit <name>` debug use case, never take this prefix).
pub fn event_domain(event: &str) -> Option<&str> {
    event.strip_prefix("bread.")?.split('.').next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_apps_are_recognized() {
        assert!(is_known_app("clip"));
        assert!(is_known_app("bakery"));
    }

    #[test]
    fn unknown_app_is_not_recognized() {
        assert!(!is_known_app("notanapp"));
        assert!(!is_known_app(""));
    }

    #[test]
    fn reserved_domains_are_never_known_apps() {
        for domain in RESERVED_DOMAINS {
            assert!(
                !is_known_app(domain),
                "reserved domain '{domain}' must not double as a known app id"
            );
        }
    }

    #[test]
    fn reserved_domains_are_recognized() {
        assert!(is_reserved_domain("power"));
        assert!(is_reserved_domain("hyprland"));
        assert!(!is_reserved_domain("clip"));
    }

    #[test]
    fn reserved_domains_cover_every_adapter_owned_event_family() {
        // Every top-level segment a real adapter (via the normalizer) or the
        // daemon itself publishes under must be reserved, or a manual/no-source
        // `emit` over the IPC socket could impersonate it undetected.
        for domain in [
            "power", "network", "device", "bluetooth", "hyprland", "workspace", "monitor",
            "window", "system",
        ] {
            assert!(
                is_reserved_domain(domain),
                "'{domain}' is an adapter-owned event family and must be reserved"
            );
        }
    }

    #[test]
    fn event_domain_extracts_top_level_segment() {
        assert_eq!(event_domain("bread.power.ac.connected"), Some("power"));
        assert_eq!(event_domain("bread.custom.event"), Some("custom"));
        assert_eq!(event_domain("bread.test"), Some("test"));
    }

    #[test]
    fn event_domain_is_none_without_bread_prefix() {
        assert_eq!(event_domain("power.ac.connected"), None);
        assert_eq!(event_domain(""), None);
    }

    #[test]
    fn validate_app_namespace_accepts_own_namespace() {
        assert!(validate_app_namespace("clip", "bread.clip.copied"));
        assert!(validate_app_namespace(
            "clip",
            "bread.clip.stack_trace.captured"
        ));
    }

    #[test]
    fn validate_app_namespace_rejects_other_namespaces() {
        assert!(!validate_app_namespace("clip", "bread.pad.reminder.due"));
        assert!(!validate_app_namespace("clip", "bread.power.ac.connected"));
        assert!(!validate_app_namespace("clip", "bread.clipboard.copied"));
    }

    #[test]
    fn validate_app_namespace_rejects_bare_prefix_without_trailing_dot() {
        // "bread.clipx..." must not satisfy the "clip" namespace just
        // because it shares a string prefix.
        assert!(!validate_app_namespace("clip", "bread.clipx.copied"));
    }
}
