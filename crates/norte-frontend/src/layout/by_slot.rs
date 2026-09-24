//! State indexed by slot, with the ergonomics of the array it replaces.

use std::collections::BTreeMap;

use super::{Node, SlotId};

/// State per slot.
///
/// Exists because a frontend's loop keeps things PER PANE — in-flight
/// paginated fill, the requested decoration, the stat probe's dedup, the
/// live search — and keeping them by POSITION is two problems: a ceiling of
/// two panes, and a silent failure. An in-flight response for position 1
/// applies to whoever is at position 1 when it arrives, which after closing
/// a pane is someone else. That is why the key is the slot, which does not
/// move.
#[derive(Debug, Clone)]
pub struct BySlot<T> {
    inner: BTreeMap<SlotId, T>,
}

impl<T> Default for BySlot<T> {
    fn default() -> Self {
        Self {
            inner: BTreeMap::new(),
        }
    }
}

impl<T> BySlot<T> {
    /// Empty.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The slot's value, if there is one.
    #[must_use]
    pub fn get(&self, id: SlotId) -> Option<&T> {
        self.inner.get(&id)
    }

    /// The slot's value, to mutate it.
    pub fn get_mut(&mut self, id: SlotId) -> Option<&mut T> {
        self.inner.get_mut(&id)
    }

    /// Sets a slot's value and returns the previous one.
    pub fn insert(&mut self, id: SlotId, v: T) -> Option<T> {
        self.inner.insert(id, v)
    }

    /// Removes a slot's value.
    pub fn remove(&mut self, id: SlotId) -> Option<T> {
        self.inner.remove(&id)
    }

    /// Sets or removes a slot's value, depending on whether `Some` or `None`
    /// comes in.
    ///
    /// It is the shape the array had (`x[i] = ...`) without the part that
    /// hurt: the key is the slot.
    pub fn set(&mut self, id: SlotId, v: Option<T>) {
        match v {
            Some(v) => {
                self.inner.insert(id, v);
            }
            None => {
                self.inner.remove(&id);
            }
        }
    }

    /// Is there anything for that slot?
    #[must_use]
    pub fn contains(&self, id: SlotId) -> bool {
        self.inner.contains_key(&id)
    }

    /// How many slots have a value.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// None at all?
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// The pairs, in [`SlotId`] order.
    pub fn iter(&self) -> impl Iterator<Item = (SlotId, &T)> {
        self.inner.iter().map(|(id, v)| (*id, v))
    }

    /// The pairs, to mutate them.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (SlotId, &mut T)> {
        self.inner.iter_mut().map(|(id, v)| (*id, v))
    }

    /// Swaps what is in two slots.
    ///
    /// Needed because swapping panes moves the CONTENT between slots (the
    /// ids stay where they were), so in-flight work has to travel with its
    /// listing. Swapping the ids IN THE TREE instead of the content would
    /// make this unnecessary, and it is the improvement the plan notes.
    pub fn swap(&mut self, a: SlotId, b: SlotId) {
        if a == b {
            return;
        }
        let (va, vb) = (self.inner.remove(&a), self.inner.remove(&b));
        if let Some(v) = vb {
            self.inner.insert(a, v);
        }
        if let Some(v) = va {
            self.inner.insert(b, v);
        }
    }

    /// Drops what `tree` no longer mentions.
    ///
    /// Called after every layout change. Unlike [`super::SlotStore`],
    /// nothing orphaned is kept HERE: the store's state is the reader's —
    /// their cursor, their marks — and deserves to survive an accidental
    /// close; this here is work IN FLIGHT, and applying a request's result
    /// to a pane that no longer exists is not recovering anything, it is
    /// acting on a ghost.
    pub fn retain_tree(&mut self, tree: &Node) {
        let alive = tree.slot_ids();
        self.inner.retain(|id, _| alive.contains(id));
    }
}

impl<T: Default> BySlot<T> {
    /// The slot's value, creating it by default if it was not there.
    pub fn entry(&mut self, id: SlotId) -> &mut T {
        self.inner.entry(id).or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Dir, KindId};

    fn b(id: u32) -> Node {
        Node::slot(SlotId(id), KindId::browser())
    }

    /// What the tree no longer mentions is DROPPED. Applying a request's
    /// result to a pane that closed is not recovering anything: it is
    /// acting on a ghost, and in the neighboring position.
    #[test]
    fn lo_que_el_arbol_no_menciona_se_tira() {
        let mut m: BySlot<u32> = BySlot::new();
        m.insert(SlotId(1), 10);
        m.insert(SlotId(2), 20);
        m.retain_tree(&b(1));
        assert_eq!(m.get(SlotId(1)), Some(&10));
        assert_eq!(m.get(SlotId(2)), None);
    }

    /// A HIDDEN tab is still in the tree, so its in-flight work is not
    /// dropped: it is still theirs and still has somewhere to land.
    #[test]
    fn una_pestana_oculta_conserva_lo_suyo() {
        let mut m: BySlot<u32> = BySlot::new();
        m.insert(SlotId(2), 20);
        let tree = Node::Tabs {
            children: vec![b(1), b(2)],
            active: 0,
        };
        m.retain_tree(&tree);
        assert_eq!(m.get(SlotId(2)), Some(&20));
    }

    #[test]
    fn entry_crea_por_defecto_y_iter_va_en_orden() {
        let mut m: BySlot<u32> = BySlot::new();
        *m.entry(SlotId(3)) += 1;
        *m.entry(SlotId(1)) += 5;
        assert_eq!(
            m.iter().map(|(id, v)| (id.0, *v)).collect::<Vec<_>>(),
            vec![(1, 5), (3, 1)]
        );
    }

    /// A `Split` changes nothing: what decides is which slots there are, not
    /// how they are laid out.
    #[test]
    fn la_forma_del_arbol_no_decide_nada_aqui() {
        let mut m: BySlot<u32> = BySlot::new();
        m.insert(SlotId(1), 1);
        m.insert(SlotId(2), 2);
        m.retain_tree(&Node::split(Dir::Horizontal, vec![b(1), b(2)]));
        assert_eq!(m.len(), 2);
    }
}
