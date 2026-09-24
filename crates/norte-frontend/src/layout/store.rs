//! Pane state, indexed by slot.

use std::collections::BTreeMap;

use super::{Node, SlotId};

/// How many orphaned states are kept before purging the oldest.
///
/// Generous for a work session and bounded so that opening and closing
/// panes all afternoon does not grow without end.
const ORPHAN_CAP: usize = 32;

/// Pane state, indexed by slot.
///
/// GENERIC over the state, and each frontend puts in its own: this crate
/// has the pure halves (`pane`, `viewer`, `compare`, `sync`), but the TUI
/// wraps them in its own views and the GUI in different ones. A concrete
/// enum here would drag one frontend's view types into the other's graph.
///
/// Eligibility for a role does NOT need a trait over `P`: it is decided by
/// the slot's `kind` in the tree plus the registry.
///
/// # Orphans, and why they are not deleted
///
/// Closing a slot does not delete its state: it becomes an orphan.
/// Reopening the same layout recovers the history, the marks and the
/// cursor instead of starting blank — which is what a user expects from
/// closing a tab by mistake. The price is bounded: past the ceiling (32 by
/// default) the oldest is purged, and age is the order they became orphans
/// in, not the clock (there is no clock here, and none is wanted: it would
/// make the tests time-dependent).
#[derive(Debug, Clone)]
pub struct SlotStore<P> {
    slots: BTreeMap<SlotId, P>,
    /// Orphans from the OLDEST to the most recent.
    orphans: Vec<SlotId>,
    cap: usize,
}

impl<P> Default for SlotStore<P> {
    fn default() -> Self {
        Self::with_orphan_cap(ORPHAN_CAP)
    }
}

impl<P> SlotStore<P> {
    /// A store with a custom orphan ceiling.
    #[must_use]
    pub fn with_orphan_cap(cap: usize) -> Self {
        Self {
            slots: BTreeMap::new(),
            orphans: Vec::new(),
            cap,
        }
    }

    /// Sets a slot's state. If that id was orphaned, it revives.
    pub fn insert(&mut self, id: SlotId, state: P) {
        self.orphans.retain(|o| *o != id);
        self.slots.insert(id, state);
    }

    /// Slot `id`'s state, alive or orphaned.
    #[must_use]
    pub fn get(&self, id: SlotId) -> Option<&P> {
        self.slots.get(&id)
    }

    /// Slot `id`'s state, to mutate it.
    pub fn get_mut(&mut self, id: SlotId) -> Option<&mut P> {
        self.slots.get_mut(&id)
    }

    /// Removes a slot and its state entirely. Used by whoever really wants
    /// to forget, not by closing a pane.
    pub fn remove(&mut self, id: SlotId) -> Option<P> {
        self.orphans.retain(|o| *o != id);
        self.slots.remove(&id)
    }

    /// Reclassifies against `tree`: what the tree mentions is ALIVE,
    /// everything else becomes orphaned; past the ceiling, the oldest is
    /// purged.
    ///
    /// Called after every layout change.
    pub fn sync_with(&mut self, tree: &Node) {
        let alive = tree.slot_ids();
        // Those that return to the tree stop being orphans.
        self.orphans.retain(|o| !alive.contains(o));
        // Those that left the tree and were not yet in the list, enter it.
        for id in self.slots.keys() {
            if !alive.contains(id) && !self.orphans.contains(id) {
                self.orphans.push(*id);
            }
        }
        while self.orphans.len() > self.cap {
            let oldest = self.orphans.remove(0);
            self.slots.remove(&oldest);
        }
    }

    /// The states, in [`SlotId`] order.
    pub fn values(&self) -> impl Iterator<Item = &P> {
        self.slots.values()
    }

    /// The states, in [`SlotId`] order, to mutate them.
    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut P> {
        self.slots.values_mut()
    }

    /// The `(id, state)` pairs, in [`SlotId`] order.
    pub fn iter(&self) -> impl Iterator<Item = (SlotId, &P)> {
        self.slots.iter().map(|(id, p)| (*id, p))
    }

    /// The `(id, state)` pairs, in [`SlotId`] order, to mutate them.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (SlotId, &mut P)> {
        self.slots.iter_mut().map(|(id, p)| (*id, p))
    }

    /// Swaps two slots' state, leaving the ids where they were.
    ///
    /// Requested by the swap-panes gesture: what changes place is the
    /// CONTENT, not the slot's identity — if the ids moved, anything
    /// holding an earlier `SlotId` would end up naming the other one.
    pub fn swap(&mut self, a: SlotId, b: SlotId) {
        if a == b {
            return;
        }
        let (va, vb) = (self.slots.remove(&a), self.slots.remove(&b));
        if let Some(v) = vb {
            self.slots.insert(a, v);
        }
        if let Some(v) = va {
            self.slots.insert(b, v);
        }
    }

    /// The orphaned ids, from the oldest to the most recent.
    #[must_use]
    pub fn orphans(&self) -> Vec<SlotId> {
        self.orphans.clone()
    }

    /// How many states are stored, alive and orphaned.
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// None at all?
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Dir, KindId, Size};

    fn browser(id: u32) -> Node {
        Node::slot(SlotId(id), KindId::browser())
    }
    fn two(a: u32, b: u32) -> Node {
        Node::Split {
            dir: Dir::Horizontal,
            sizes: vec![Size::Weight(1), Size::Weight(1)],
            children: vec![browser(a), browser(b)],
        }
    }

    /// Closing a slot does NOT delete its state: it becomes an orphan, so
    /// reopening the same layout recovers the history instead of starting
    /// blank.
    #[test]
    fn closing_a_slot_leaves_its_state_orphaned() {
        let mut s: SlotStore<u32> = SlotStore::default();
        s.insert(SlotId(1), 10);
        s.insert(SlotId(2), 20);
        s.sync_with(&browser(1));
        assert_eq!(
            s.get(SlotId(2)),
            Some(&20),
            "the closed one's state remains"
        );
        assert_eq!(s.orphans(), vec![SlotId(2)]);
    }

    /// Orphans have a ceiling: without it, opening and closing panes for a
    /// whole session grows without end. The OLDEST is purged.
    #[test]
    fn orphans_have_a_cap_and_the_oldest_is_purged() {
        let mut s: SlotStore<u32> = SlotStore::with_orphan_cap(2);
        for i in 1..=4 {
            s.insert(SlotId(i), i);
        }
        s.sync_with(&browser(4));
        assert_eq!(s.orphans().len(), 2);
        assert!(s.get(SlotId(1)).is_none(), "the oldest is gone");
        assert_eq!(s.get(SlotId(4)), Some(&4), "the live one is untouched");
    }

    /// Reopening an orphaned id revives it with its state.
    #[test]
    fn reopening_an_orphan_id_recovers_its_state() {
        let mut s: SlotStore<u32> = SlotStore::default();
        s.insert(SlotId(1), 10);
        s.insert(SlotId(2), 20);
        s.sync_with(&browser(1));
        s.sync_with(&two(1, 2));
        assert_eq!(s.get(SlotId(2)), Some(&20));
        assert!(s.orphans().is_empty());
    }

    /// Swapping moves the CONTENT and leaves the ids in place: if the ids
    /// moved, any `SlotId` stored earlier would name the other slot.
    #[test]
    fn swapping_moves_the_content_not_the_ids() {
        let mut s: SlotStore<u32> = SlotStore::default();
        s.insert(SlotId(1), 10);
        s.insert(SlotId(2), 20);
        s.swap(SlotId(1), SlotId(2));
        assert_eq!(s.get(SlotId(1)), Some(&20));
        assert_eq!(s.get(SlotId(2)), Some(&10));
        assert_eq!(s.values().copied().collect::<Vec<_>>(), vec![20, 10]);
    }

    /// Two `sync_with` calls in a row with no changes do not duplicate the
    /// orphan: if they did, the ceiling would run out with a single closed
    /// slot, and one repeated frame would be enough to drop live state.
    #[test]
    fn syncing_twice_does_not_duplicate_the_orphan() {
        let mut s: SlotStore<u32> = SlotStore::default();
        s.insert(SlotId(1), 10);
        s.insert(SlotId(2), 20);
        s.sync_with(&browser(1));
        s.sync_with(&browser(1));
        assert_eq!(s.orphans(), vec![SlotId(2)]);
    }
}
