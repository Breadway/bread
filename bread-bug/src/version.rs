//! Comparing bread version strings.
//!
//! `0.9.1-dev.20260929181236+abc` is older than `0.9.1`, which is older than
//! `0.9.2`. Only the numeric parts matter.

use std::cmp::Ordering;

fn key(v: &str) -> (Vec<u64>, bool, Vec<u64>) {
    let (core, pre) = match v.split_once(['-', '+']) {
        Some((c, p)) => (c, Some(p)),
        None => (v, None),
    };
    let nums = |s: &str| -> Vec<u64> {
        s.split(|c: char| !c.is_ascii_digit())
            .filter(|p| !p.is_empty())
            .filter_map(|p| p.parse().ok())
            .collect()
    };
    let mut triple = nums(core);
    while triple.len() < 3 {
        triple.push(0);
    }
    let is_pre = pre.is_some_and(|p| p.starts_with(|c: char| c.is_ascii_alphabetic()));
    (triple, !is_pre, pre.map(nums).unwrap_or_default())
}

pub fn cmp(a: &str, b: &str) -> Ordering {
    key(a).cmp(&key(b))
}

/// Whether `installed` is older than `fixed_in`.
pub fn older_than(installed: &str, fixed_in: &str) -> bool {
    cmp(installed, fixed_in) == Ordering::Less
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering() {
        assert!(older_than("0.3.1", "0.3.22"));
        assert!(older_than("0.9.1-dev.20260929181236+4774581", "0.9.1"));
        assert!(!older_than("0.9.1", "0.9.1"));
        assert!(!older_than("1.0.0", "0.99.99"));
        assert!(older_than("0.4.0-rc.1", "0.4.0"));
        assert_eq!(cmp("0.3", "0.3.0"), Ordering::Equal);
    }
}
