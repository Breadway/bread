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
        let last_idx = self.entries.len() - 1;
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

    pattern == event_name
}
