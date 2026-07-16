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
    pub fn add_with_id(
        &mut self,
        id: SubscriptionId,
        pattern: String,
        once: bool,
    ) -> SubscriptionId {
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
            .filter(|sub| bread_shared::glob::matches_pattern(&sub.pattern, event_name))
            .cloned()
            .collect()
    }
}

// Glob-matching semantics (`*`, `**`, `?`) are implemented and tested once,
// in `bread_shared::glob`. Both this module (real event dispatch) and
// `breadd::ipc` (the CLI `--filter` path) delegate to that single
// implementation so they cannot drift apart. See `bread-shared/src/glob.rs`
// for the pattern-matching test suite.

#[cfg(test)]
mod tests {
    use super::*;

    // ─── SubscriptionTable ────────────────────────────────────────────────

    #[test]
    fn table_add_assigns_provided_id_and_finds_match() {
        let mut t = SubscriptionTable::default();
        let id = t.add_with_id(SubscriptionId(7), "bread.window.*".into(), false);
        assert_eq!(id, SubscriptionId(7));

        let matches = t.match_event("bread.window.opened");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].id, SubscriptionId(7));
        assert_eq!(matches[0].pattern, "bread.window.*");
        assert!(!matches[0].once);
    }

    #[test]
    fn table_match_returns_all_matching_subscriptions() {
        let mut t = SubscriptionTable::default();
        t.add_with_id(SubscriptionId(1), "bread.window.opened".into(), false);
        t.add_with_id(SubscriptionId(2), "bread.window.*".into(), false);
        t.add_with_id(SubscriptionId(3), "bread.**".into(), true);
        t.add_with_id(SubscriptionId(4), "bread.device.*".into(), false);

        let matches = t.match_event("bread.window.opened");
        let ids: Vec<u64> = matches.iter().map(|s| s.id.0).collect();
        assert!(ids.contains(&1));
        assert!(ids.contains(&2));
        assert!(ids.contains(&3));
        assert!(!ids.contains(&4));
    }

    #[test]
    fn table_remove_returns_true_only_for_known_ids() {
        let mut t = SubscriptionTable::default();
        t.add_with_id(SubscriptionId(1), "a".into(), false);
        assert!(t.remove(SubscriptionId(1)));
        // Second remove of the same id is false.
        assert!(!t.remove(SubscriptionId(1)));
        // Removing a never-known id is false.
        assert!(!t.remove(SubscriptionId(999)));
    }

    #[test]
    fn table_remove_preserves_other_entries_after_swap_remove() {
        let mut t = SubscriptionTable::default();
        t.add_with_id(SubscriptionId(1), "a".into(), false);
        t.add_with_id(SubscriptionId(2), "b".into(), false);
        t.add_with_id(SubscriptionId(3), "c".into(), false);

        // Remove the middle entry — swap_remove will move entry 3 into the slot.
        assert!(t.remove(SubscriptionId(2)));

        // Subsequent removes still work, proving the by_id index was kept consistent.
        assert!(t.remove(SubscriptionId(3)));
        assert!(t.remove(SubscriptionId(1)));
    }

    #[test]
    fn table_clear_removes_all() {
        let mut t = SubscriptionTable::default();
        t.add_with_id(SubscriptionId(1), "a".into(), false);
        t.add_with_id(SubscriptionId(2), "b".into(), false);
        t.clear();
        assert!(t.match_event("a").is_empty());
        assert!(t.match_event("b").is_empty());
        // After clear, the ids are reusable.
        assert!(!t.remove(SubscriptionId(1)));
    }

    #[test]
    fn table_match_returns_empty_for_unmatched_event() {
        let mut t = SubscriptionTable::default();
        t.add_with_id(SubscriptionId(1), "bread.device.*".into(), false);
        assert!(t.match_event("bread.window.opened").is_empty());
    }

    #[test]
    fn table_once_flag_is_preserved_in_match_result() {
        let mut t = SubscriptionTable::default();
        t.add_with_id(SubscriptionId(1), "bread.test".into(), true);
        let matches = t.match_event("bread.test");
        assert_eq!(matches.len(), 1);
        assert!(matches[0].once);
    }
}
