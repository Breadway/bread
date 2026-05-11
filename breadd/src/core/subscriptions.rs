use std::collections::HashMap;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub struct SubscriptionId(pub u64);

#[derive(Debug, Clone)]
pub struct Subscription {
    pub id: SubscriptionId,
    pub pattern: String,
    pub once: bool,
}

#[derive(Default, Debug)]
pub struct SubscriptionTable {
    entries: Vec<Subscription>,
    by_id: HashMap<SubscriptionId, usize>,
    next_id: u64,
}

impl SubscriptionTable {
    pub fn add_with_id(&mut self, id: SubscriptionId, pattern: String, once: bool) -> SubscriptionId {
        self.next_id = self.next_id.max(id.0.saturating_add(1));

        let sub = Subscription { id, pattern, once };
        self.entries.push(sub);
        self.by_id.insert(id, self.entries.len() - 1);
        id
    }

    pub fn remove(&mut self, id: SubscriptionId) -> bool {
        let Some(idx) = self.by_id.remove(&id) else {
            return false;
        };

        // swap_remove moves the last element into `idx`. We need to update by_id
        // for that element. But first, remove its stale entry (it was at the last
        // position before the swap); then re-insert it at the new position.
        let _last_idx = self.entries.len() - 1;
        self.entries.swap_remove(idx);

        if idx < self.entries.len() {
            // The element that was at `last_idx` is now at `idx`.
            let swapped_id = self.entries[idx].id;
            self.by_id.remove(&swapped_id); // remove stale last_idx entry
            self.by_id.insert(swapped_id, idx);
        }

        true
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.by_id.clear();
    }

    pub fn match_event(&self, event_name: &str) -> Vec<Subscription> {
        self.entries
            .iter()
            .filter(|sub| matches_pattern(&sub.pattern, event_name))
            .cloned()
            .collect()
    }
}

fn matches_pattern(pattern: &str, event_name: &str) -> bool {
    if pattern.ends_with(".*") {
        let prefix = &pattern[..pattern.len() - 1];
        return event_name.starts_with(prefix);
    }

    if let Some(prefix) = pattern.strip_suffix(".**") {
        if event_name == prefix {
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
    use super::matches_pattern;

    #[test]
    fn exact_match() {
        assert!(matches_pattern("bread.device.dock.connected", "bread.device.dock.connected"));
        assert!(!matches_pattern("bread.device.dock.connected", "bread.device.dock.disconnected"));
    }

    #[test]
    fn single_segment_wildcard() {
        assert!(matches_pattern("bread.device.*", "bread.device.dock.connected"));
        assert!(matches_pattern("bread.device.*", "bread.device.foo"));
        assert!(!matches_pattern("bread.device.*", "bread.device"));
    }

    #[test]
    fn recursive_wildcard() {
        assert!(matches_pattern("bread.device.**", "bread.device.dock.connected"));
        assert!(matches_pattern("bread.**", "bread.device.dock.connected"));
        assert!(matches_pattern("bread.**", "bread"));
    }

    #[test]
    fn single_char_wildcard() {
        assert!(matches_pattern("bread.monitor.?", "bread.monitor.1"));
        assert!(!matches_pattern("bread.monitor.?", "bread.monitor.10"));
        assert!(!matches_pattern("bread.monitor.?", "bread.monitor."));
    }
}
