//! Canonical dotted-segment glob matcher.
//!
//! This is the single implementation of bread's event-name glob semantics,
//! shared by real event dispatch (`breadd::core::subscriptions`) and the
//! IPC `--filter` path used by `bread events` (`breadd::ipc`). Previously
//! these lived as two independently hand-written copies that could drift
//! out of sync despite the docs claiming identical behavior; this module is
//! now the one place that logic is implemented and tested.
//!
//! Wildcard semantics (see `Documentation.md` / the API reference):
//! - `*` matches within a single dot-delimited segment (never crosses `.`).
//! - `**` matches zero or more segments, at any depth.
//! - `?` matches exactly one character, but never a `.`.

/// Returns true if `event_name` matches the dotted glob `pattern`.
pub fn matches_pattern(pattern: &str, event_name: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix(".**") {
        if event_name == prefix || event_name.starts_with(&format!("{prefix}.")) {
            return true;
        }
    }

    matches_glob(pattern.as_bytes(), event_name.as_bytes())
}

fn matches_glob(pattern: &[u8], text: &[u8]) -> bool {
    if pattern.is_empty() {
        return text.is_empty();
    }

    if pattern.len() >= 2 && pattern[0] == b'*' && pattern[1] == b'*' {
        let mut idx = 2;
        while pattern.len() >= idx + 2 && pattern[idx] == b'*' && pattern[idx + 1] == b'*' {
            idx += 2;
        }
        let rest = &pattern[idx..];
        if rest.is_empty() {
            return true;
        }
        for offset in 0..=text.len() {
            if matches_glob(rest, &text[offset..]) {
                return true;
            }
        }
        return false;
    }

    match pattern[0] {
        b'*' => {
            let mut offset = 0;
            loop {
                if matches_glob(&pattern[1..], &text[offset..]) {
                    return true;
                }
                if offset == text.len() || text[offset] == b'.' {
                    break;
                }
                offset += 1;
            }
            false
        }
        b'?' => {
            if text.is_empty() || text[0] == b'.' {
                return false;
            }
            matches_glob(&pattern[1..], &text[1..])
        }
        ch => {
            if text.first().copied() != Some(ch) {
                return false;
            }
            matches_glob(&pattern[1..], &text[1..])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match() {
        assert!(matches_pattern(
            "bread.device.dock.connected",
            "bread.device.dock.connected"
        ));
        assert!(!matches_pattern(
            "bread.device.dock.connected",
            "bread.device.dock.disconnected"
        ));
    }

    #[test]
    fn single_segment_wildcard() {
        assert!(matches_pattern("bread.device.*", "bread.device.foo"));
        assert!(!matches_pattern(
            "bread.device.*",
            "bread.device.dock.connected"
        ));
        assert!(!matches_pattern("bread.device.*", "bread.device"));
    }

    #[test]
    fn recursive_wildcard() {
        assert!(matches_pattern(
            "bread.device.**",
            "bread.device.dock.connected"
        ));
        assert!(matches_pattern("bread.**", "bread.device.dock.connected"));
        assert!(matches_pattern("bread.**", "bread"));
    }

    #[test]
    fn single_char_wildcard() {
        assert!(matches_pattern("bread.monitor.?", "bread.monitor.1"));
        assert!(!matches_pattern("bread.monitor.?", "bread.monitor.10"));
        assert!(!matches_pattern("bread.monitor.?", "bread.monitor."));
    }

    #[test]
    fn star_does_not_cross_dot_segments() {
        assert!(matches_pattern(
            "bread.*.connected",
            "bread.device.connected"
        ));
        assert!(!matches_pattern(
            "bread.*.connected",
            "bread.device.dock.connected"
        ));
    }

    #[test]
    fn double_star_matches_zero_or_more_segments() {
        assert!(matches_pattern("bread.**", "bread.a"));
        assert!(matches_pattern("bread.**", "bread.a.b.c.d"));
    }

    #[test]
    fn empty_pattern_matches_only_empty_text() {
        assert!(matches_pattern("", ""));
        assert!(!matches_pattern("", "bread"));
    }

    #[test]
    fn empty_text_only_matches_wildcards() {
        assert!(matches_pattern("**", ""));
        assert!(!matches_pattern("bread.*", ""));
    }

    #[test]
    fn dot_double_star_matches_exact_prefix_with_zero_segments() {
        assert!(matches_pattern("bread.device.**", "bread.device"));
    }

    #[test]
    fn dot_double_star_does_not_match_sibling_prefix() {
        assert!(!matches_pattern("bread.device.**", "bread.devicex"));
        assert!(!matches_pattern(
            "bread.device.**",
            "bread.network.connected"
        ));
    }

    #[test]
    fn mid_pattern_star_does_not_cross_dots() {
        assert!(matches_pattern(
            "bread.*.connected",
            "bread.alpha.connected"
        ));
        assert!(!matches_pattern(
            "bread.*.connected",
            "bread.alpha.beta.connected"
        ));
    }
}
