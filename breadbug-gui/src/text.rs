//! Wording helpers. Pure functions so they can be tested without a display.

use bread_bug::report::{AppEntry, ReportType};
use bread_bug::store::MarkerKind;

/// "just now", "5 minutes ago", "3 hours ago", "2 days ago".
pub fn ago(now_ms: u64, then_ms: u64) -> String {
    let secs = now_ms.saturating_sub(then_ms) / 1000;
    let plural = |n: u64, unit: &str| format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" });
    match secs {
        0..=44 => "just now".to_string(),
        45..=3599 => plural((secs / 60).max(1), "minute"),
        3600..=86_399 => plural(secs / 3600, "hour"),
        _ => plural(secs / 86_400, "day"),
    }
}

/// One line for the app list: `clip 0.3.1  crashed 5 minutes ago`.
pub fn app_line(e: &AppEntry, now_ms: u64) -> String {
    let version = e
        .version
        .as_deref()
        .map_or(String::new(), |v| format!(" {v}"));
    let problem = match e.last_problem {
        Some((MarkerKind::Freeze, t)) => format!("  froze {}", ago(now_ms, t)),
        Some((_, t)) => format!("  crashed {}", ago(now_ms, t)),
        None => String::new(),
    };
    format!("{}{version}{problem}", e.app)
}

pub const TYPE_LABELS: [&str; 3] = ["It crashed", "It froze", "It is behaving wrongly"];

pub fn app_name(id: &str) -> String {
    match id {
        "bakery" => "bakery".into(),
        "daemon" => "breadd".into(),
        "crumbs" => "breadcrumbs".into(),
        id if id.starts_with("bread") => id.into(),
        id => format!("bread{id}"),
    }
}

pub fn app_icon(id: &str) -> &'static str {
    match id {
        "clip" => "edit-paste-symbolic",
        "pad" => "document-edit-symbolic",
        "search" => "system-search-symbolic",
        "lock" => "system-lock-screen-symbolic",
        "shot" | "cast" => "camera-photo-symbolic",
        "crumbs" => "network-wireless-symbolic",
        "paper" => "preferences-desktop-wallpaper-symbolic",
        _ => "application-x-executable-symbolic",
    }
}

pub fn app_problem(e: &AppEntry, now_ms: u64) -> String {
    match e.last_problem {
        Some((MarkerKind::Freeze, t)) => format!("Froze {}", ago(now_ms, t)),
        Some((_, t)) => format!("Crashed {}", ago(now_ms, t)),
        None => "No recent crash or freeze recorded".into(),
    }
}

pub fn type_from_index(i: u32) -> ReportType {
    match i {
        0 => ReportType::Crash,
        1 => ReportType::Freeze,
        _ => ReportType::Behavior,
    }
}

pub fn index_of_type(t: ReportType) -> u32 {
    match t {
        ReportType::Crash => 0,
        ReportType::Freeze => 1,
        ReportType::Behavior => 2,
    }
}

/// The scrubber's counts as a sentence.
pub fn redaction_summary(r: &std::collections::BTreeMap<String, usize>) -> String {
    if r.is_empty() {
        return "The scrubber found nothing personal to replace.".to_string();
    }
    let parts: Vec<String> = r.iter().map(|(k, v)| format!("{v} {k}")).collect();
    format!(
        "Personal values replaced before this was built: {}.",
        parts.join(", ")
    )
}

pub const SENDING_OFF_HINT: &str = "Sending is turned off. To allow it, open Settings, Privacy & security, Bug reports. You can still save the report to a file.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ago_wording() {
        let now = 10_000_000_000;
        assert_eq!(ago(now, now - 5_000), "just now");
        assert_eq!(ago(now, now - 60_000), "1 minute ago");
        assert_eq!(ago(now, now - 5 * 60_000), "5 minutes ago");
        assert_eq!(ago(now, now - 3 * 3_600_000), "3 hours ago");
        assert_eq!(ago(now, now - 86_400_000), "1 day ago");
        assert_eq!(ago(now, now - 5 * 86_400_000), "5 days ago");
        // clocks that disagree never go negative
        assert_eq!(ago(now, now + 9_999), "just now");
    }

    #[test]
    fn app_lines() {
        let e = AppEntry {
            app: "clip".into(),
            version: Some("0.3.1".into()),
            last_problem: Some((MarkerKind::Panic, 1_000_000)),
        };
        assert_eq!(
            app_line(&e, 1_000_000 + 120_000),
            "clip 0.3.1  crashed 2 minutes ago"
        );
        let f = AppEntry {
            last_problem: Some((MarkerKind::Freeze, 1_000_000)),
            ..e.clone()
        };
        assert!(app_line(&f, 1_000_000).contains("froze just now"));
        let n = AppEntry {
            last_problem: None,
            version: None,
            ..e
        };
        assert_eq!(app_line(&n, 5), "clip");
    }

    #[test]
    fn type_index_roundtrip() {
        for t in [ReportType::Crash, ReportType::Freeze, ReportType::Behavior] {
            assert_eq!(type_from_index(index_of_type(t)), t);
        }
        assert_eq!(type_from_index(99), ReportType::Behavior);
    }

    #[test]
    fn redaction_sentence() {
        assert!(redaction_summary(&Default::default()).contains("nothing personal"));
        let mut m = std::collections::BTreeMap::new();
        m.insert("IP".to_string(), 2);
        m.insert("EMAIL".to_string(), 1);
        assert_eq!(
            redaction_summary(&m),
            "Personal values replaced before this was built: 1 EMAIL, 2 IP."
        );
    }
}
