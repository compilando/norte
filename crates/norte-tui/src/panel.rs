//! The TUI's panel state and the `orthodox` preset's well-known slots.
//!
//! # Why `PaneSlots` and not 213 sites rewritten
//!
//! L1a's goal is for panel state to stop living in a named field
//! (`App.panes: [Pane; 2]`) and become indexed by `SlotId`. Rewriting the
//! 213 sites that say `app.panes[i]` would achieve that, and along the way
//! would plant 213 opportunities to accidentally change behavior in the
//! same commit that says it is not changing it.
//!
//! [`PaneSlots`] achieves the same thing with an adapter: on the inside it
//! is a [`SlotStore`] indexed by [`SlotId`]; on the outside it is indexed
//! with `0`/`1`, iterated and swapped just like the old array. The storage
//! DOES change; the call sites' ergonomics do not. When L1b makes them slot-
//! aware, it will do it one at a time and with its own review.

use std::ops::{Index, IndexMut};

use norte_frontend::layout::{BySlot, KindId, Node, Params, Rect as LayoutRect, SlotId, SlotStore};

use crate::app::Pane;

/// The left pane's slot in the `orthodox` preset.
///
/// Fixed and not minted: they are the same slots the TUI has always had, so
/// today's state and the preset are the same thing and there is no
/// migration to do.
pub const SLOT_LEFT: SlotId = SlotId(1);
/// The right pane's slot in the `orthodox` preset.
pub const SLOT_RIGHT: SlotId = SlotId(2);
/// The task strip's slot in the `orthodox` preset.
pub const SLOT_TASKS: SlotId = SlotId(3);
/// The status bar's slot in the `orthodox` preset.
pub const SLOT_STATUS: SlotId = SlotId(4);

/// What can be inside a slot in the TUI.
///
/// In L1a there are only two live variants: the listing, and the box for a
/// kind this binary does not know how to paint. The viewer, the comparison
/// and the sync stay as `App` fields until L1b — moving them is not needed
/// for the engine to come in, and mixing it in here would be risk with no
/// payoff.
#[derive(Debug)]
pub enum TuiPanel {
    /// A file listing.
    Browser(Box<Pane>),
    /// The DOCKED viewer (L3): the `viewer` kind in a slot, following the
    /// active listing. The full-screen viewer is still `App::viewer` and
    /// does not go through here.
    Preview(Box<crate::preview::Preview>),
    /// The places sidebar (L3): drives and favorites.
    ///
    /// The first panel that is not a listing. `as_browser` returning `None`
    /// for it is what keeps `app.panes[i]` meaning "the i-th LISTING": a
    /// sidebar is not a side.
    Places(Box<norte_frontend::places::PlacesState>),
    /// The processes panel (phase A): its cursor. The rows belong to the
    /// `TaskBoard`, which is `App`'s: there is no second copy here.
    ///
    /// NO box, unlike its neighbors: it is two words — a position and the
    /// chosen one's id — and they are not what fixes this enum's size, so
    /// the indirection would only add an allocation.
    Processes(crate::processes::Processes),
    /// The directory tree (#136): its open branches and its cursor.
    Tree(Box<crate::tree::Tree>),
    /// The attributes sheet (phase A): the entry being shown and whether it
    /// is the `..` row.
    ///
    /// Stores the `Entry` and not its path: the sheet is drawn entirely from
    /// it and there is no second read that could arrive late. The flag sits
    /// next to it because over `..` the sheet is called `..` and says where
    /// it leads, and that cannot be derived from the `Entry` alone: its own
    /// is the parent's.
    Metadata(Box<Option<(norte_proto::Entry, bool)>>),
    /// The disk map (phase 4): which directory it describes, what was
    /// measured and which child is chosen.
    ///
    /// Boxed like the tree: the report carries up to 4096 children, and that
    /// must not fix this enum's size for every other panel.
    DiskMap(Box<norte_frontend::diskmap::DiskMap>),
    /// The journal's timeline (phase 7): what has been done, with its
    /// cursor and its backward pagination cursor.
    ///
    /// Boxed like the map and the tree: one page carries up to 200 rows and
    /// several pages accumulate, and that must not fix this enum's size for
    /// every other panel.
    Timeline(Box<norte_frontend::timeline::Timeline>),
    /// A kind this binary does not know: it is painted as a box with its
    /// name, and its `params` are kept intact, so opening the GUI's layout
    /// in the TUI does not erase anything from it.
    Unknown {
        /// The kind it did not know how to paint.
        kind: KindId,
        /// Its parameters, exactly as they arrived.
        raw: Params,
    },
}

impl TuiPanel {
    /// The listing, if this panel is one.
    #[must_use]
    pub fn as_browser(&self) -> Option<&Pane> {
        match self {
            Self::Browser(p) => Some(p),
            Self::Places(_)
            | Self::Preview(_)
            | Self::Processes(_)
            | Self::Tree(_)
            | Self::Metadata(_)
            | Self::DiskMap(_)
            | Self::Timeline(_)
            | Self::Unknown { .. } => None,
        }
    }

    /// The listing, to mutate it.
    pub fn as_browser_mut(&mut self) -> Option<&mut Pane> {
        match self {
            Self::Browser(p) => Some(p),
            Self::Places(_)
            | Self::Preview(_)
            | Self::Processes(_)
            | Self::Tree(_)
            | Self::Metadata(_)
            | Self::DiskMap(_)
            | Self::Timeline(_)
            | Self::Unknown { .. } => None,
        }
    }

    /// The sidebar, if this panel is one.
    #[must_use]
    pub fn as_places(&self) -> Option<&norte_frontend::places::PlacesState> {
        match self {
            Self::Places(s) => Some(s),
            Self::Browser(_)
            | Self::Preview(_)
            | Self::Processes(_)
            | Self::Tree(_)
            | Self::Metadata(_)
            | Self::DiskMap(_)
            | Self::Timeline(_)
            | Self::Unknown { .. } => None,
        }
    }

    /// The sidebar, to mutate it.
    pub fn as_places_mut(&mut self) -> Option<&mut norte_frontend::places::PlacesState> {
        match self {
            Self::Places(s) => Some(s),
            Self::Browser(_)
            | Self::Preview(_)
            | Self::Processes(_)
            | Self::Tree(_)
            | Self::Metadata(_)
            | Self::DiskMap(_)
            | Self::Timeline(_)
            | Self::Unknown { .. } => None,
        }
    }
}

/// The `orthodox` preset's two listings, stored by slot.
///
/// Indexed by SIDE (`0` left, `1` right) and translates to
/// [`SLOT_LEFT`]/[`SLOT_RIGHT`] on the inside. See the module for why.
#[derive(Debug)]
pub struct PaneSlots {
    store: SlotStore<TuiPanel>,
    /// Which slot shows each visible POSITION, left to right.
    ///
    /// With tabs there are more live `browser`s than visible, and with
    /// splits more than two are visible. "The left pane" still means what it
    /// always did — the listing painted furthest left — so the ~212 sites
    /// that say `app.panes[0]` are still valid. [`Self::set_visible`] keeps
    /// it up to date after every layout pass.
    visible: Vec<SlotId>,
}

impl PaneSlots {
    /// The two startup listings.
    #[must_use]
    pub fn new(left: Pane, right: Pane) -> Self {
        let mut store = SlotStore::default();
        store.insert(SLOT_LEFT, TuiPanel::Browser(Box::new(left)));
        store.insert(SLOT_RIGHT, TuiPanel::Browser(Box::new(right)));
        Self {
            store,
            visible: vec![SLOT_LEFT, SLOT_RIGHT],
        }
    }

    /// The slot showing a position. Out of range, the last one.
    #[must_use]
    pub fn slot_of(&self, side: usize) -> SlotId {
        let i = side.min(self.visible.len().saturating_sub(1));
        self.visible.get(i).copied().unwrap_or(SLOT_LEFT)
    }

    /// Says which slot shows each position. Called by the frontend after a
    /// layout pass, with the placed `browser`s ordered left to right.
    ///
    /// An EMPTY list erases nothing: it happens when the layout pass places
    /// no pane (viewer open, or an impossible window), and in that frame
    /// what was there is still correct.
    pub fn set_visible(&mut self, order: &[SlotId]) {
        if !order.is_empty() {
            self.visible = order.to_vec();
        }
        self.rescue_visible();
    }

    /// Drops from `visible` the slots that NO LONGER carry a listing, and if
    /// that leaves none it grabs any listing from the store.
    ///
    /// This is the net for the invariant [`Index`] and [`IndexMut`]
    /// document: without it, a slot whose content became another kind
    /// stayed in the list — "empty erases nothing" keeps the PREVIOUS state,
    /// not the valid one — and the first access by side panicked (#242).
    /// That the tree carries a listing is guaranteed by
    /// [`norte_frontend::layout::validate`]; that the list points at one, by
    /// this.
    fn rescue_visible(&mut self) {
        self.visible
            .retain(|id| matches!(self.store.get(*id), Some(TuiPanel::Browser(_))));
        if self.visible.is_empty()
            && let Some(id) = self
                .store
                .iter()
                .find(|(_, p)| matches!(p, TuiPanel::Browser(_)))
                .map(|(id, _)| id)
        {
            self.visible = vec![id];
        }
    }

    /// Refreshes the sides from the TREE, without laying anything out.
    ///
    /// Called right after touching the layout, when there is not yet a
    /// frame: without this, a side would point at the slot that was just
    /// closed and the next `app.panes[i]` would blow up.
    ///
    /// With only one live `browser` the list has ONE entry, and `len()` says
    /// so: whoever asks for "the other pane" gets that same one, which is
    /// the truth — there is no other.
    pub fn refresh_visible(&mut self, tree: &Node) {
        let alive: Vec<SlotId> = tree
            .visible_slot_ids()
            .into_iter()
            .filter(|id| {
                tree.kind_of(*id).is_some_and(|k| *k == KindId::browser())
                    && matches!(self.store.get(*id), Some(TuiPanel::Browser(_)))
            })
            .collect();
        if !alive.is_empty() {
            self.visible = alive;
        }
        self.store.sync_with(tree);
        self.rescue_visible();
    }

    /// Any slot's listing, visible or not. Requested by the tab bar, which
    /// has to title the ones not seen too.
    #[must_use]
    pub fn browser(&self, id: SlotId) -> Option<&Pane> {
        self.store.get(id).and_then(TuiPanel::as_browser)
    }

    /// Any slot's listing, to mutate it.
    ///
    /// The gate the loop needs: a response in flight carries the SLOT it was
    /// going to, and applying it by position would apply it to whoever
    /// occupies that position by the time it arrives.
    pub fn browser_mut(&mut self, id: SlotId) -> Option<&mut Pane> {
        self.store.get_mut(id).and_then(TuiPanel::as_browser_mut)
    }

    /// Puts a new listing into the store, for a freshly minted slot.
    pub fn insert_browser(&mut self, id: SlotId, pane: Pane) {
        self.store.insert(id, TuiPanel::Browser(Box::new(pane)));
    }

    /// How many listings are VISIBLE.
    #[must_use]
    pub fn len(&self) -> usize {
        self.visible.len()
    }

    /// Never: there is always at least one listing on screen.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.visible.is_empty()
    }

    /// A side's listing, or `None` if the index is not `0|1`.
    ///
    /// Exists because there is a caller — the read-only verdict — whose
    /// index can come from outside and for which an impossible index must
    /// NOT be a panic, but an "I don't know".
    #[must_use]
    pub fn get(&self, side: usize) -> Option<&Pane> {
        if side > 1 {
            return None;
        }
        self.store
            .get(self.slot_of(side))
            .and_then(TuiPanel::as_browser)
    }

    /// The visible slots, with no repeats.
    fn visible(&self) -> Vec<SlotId> {
        let mut v = self.visible.clone();
        v.dedup();
        v
    }

    /// The VISIBLE listings, left to right.
    ///
    /// Hidden tabs' do NOT come out: whoever iterates panes is doing
    /// something with what is on screen — watching its directory, requesting
    /// a page, refreshing — and a tab nobody is looking at should cost
    /// nothing. A hidden slot's suspension is not separate code: it is this.
    pub fn iter(&self) -> impl Iterator<Item = &Pane> {
        self.visible()
            .into_iter()
            .filter_map(|id| self.store.get(id).and_then(TuiPanel::as_browser))
    }

    /// Like [`Self::iter`], to mutate them.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Pane> {
        let ids = self.visible();
        self.store
            .iter_mut()
            .filter(move |(id, _)| ids.contains(id))
            .filter_map(|(_, p)| p.as_browser_mut())
    }

    /// ALL listings, visible or not, to mutate them.
    ///
    /// Unlike [`Self::iter_mut`], and on purpose: this is for
    /// CONFIGURATION, not for work. A tab nobody is looking at should not
    /// cost a request, but it does have to repaint like the rest once it is
    /// looked at — one half of the screen with the `..` row and the other
    /// without it would be the same configuration saying two things.
    pub fn browsers_mut(&mut self) -> impl Iterator<Item = &mut Pane> {
        self.store
            .iter_mut()
            .filter_map(|(_, p)| p.as_browser_mut())
    }

    /// Swaps the two sides' content, leaving the ids in place.
    pub fn swap(&mut self, a: usize, b: usize) {
        let (sa, sb) = (self.slot_of(a), self.slot_of(b));
        self.store.swap(sa, sb);
    }

    /// A slot's sidebar, if there is one.
    #[must_use]
    pub fn places(&self, id: SlotId) -> Option<&norte_frontend::places::PlacesState> {
        self.store.get(id).and_then(TuiPanel::as_places)
    }

    /// A slot's sidebar, to mutate it.
    pub fn places_mut(&mut self, id: SlotId) -> Option<&mut norte_frontend::places::PlacesState> {
        self.store.get_mut(id).and_then(TuiPanel::as_places_mut)
    }

    /// A slot's preview, if there is one.
    #[must_use]
    pub fn preview(&self, id: SlotId) -> Option<&crate::preview::Preview> {
        match self.store.get(id) {
            Some(TuiPanel::Preview(p)) => Some(p),
            _ => None,
        }
    }

    /// A slot's preview, to mutate it.
    pub fn preview_mut(&mut self, id: SlotId) -> Option<&mut crate::preview::Preview> {
        match self.store.get_mut(id) {
            Some(TuiPanel::Preview(p)) => Some(p),
            _ => None,
        }
    }

    /// Puts a new preview into the store, for a freshly minted slot.
    pub fn insert_preview(&mut self, id: SlotId, p: crate::preview::Preview) {
        self.store.insert(id, TuiPanel::Preview(Box::new(p)));
    }

    /// A slot's processes panel, if there is one.
    #[must_use]
    pub fn processes(&self, id: SlotId) -> Option<&crate::processes::Processes> {
        match self.store.get(id) {
            Some(TuiPanel::Processes(p)) => Some(p),
            _ => None,
        }
    }

    /// A slot's processes panel, to move its cursor.
    pub fn processes_mut(&mut self, id: SlotId) -> Option<&mut crate::processes::Processes> {
        match self.store.get_mut(id) {
            Some(TuiPanel::Processes(p)) => Some(p),
            _ => None,
        }
    }

    /// Puts a new processes panel, for a freshly minted slot.
    /// That slot's tree, if it is one.
    #[must_use]
    pub fn tree(&self, id: SlotId) -> Option<&crate::tree::Tree> {
        match self.store.get(id) {
            Some(TuiPanel::Tree(t)) => Some(t),
            _ => None,
        }
    }

    /// That slot's tree, to mutate it.
    pub fn tree_mut(&mut self, id: SlotId) -> Option<&mut crate::tree::Tree> {
        match self.store.get_mut(id) {
            Some(TuiPanel::Tree(t)) => Some(t),
            _ => None,
        }
    }

    /// Puts a tree into a slot.
    pub fn insert_tree(&mut self, id: SlotId, t: crate::tree::Tree) {
        self.store.insert(id, TuiPanel::Tree(Box::new(t)));
    }

    /// Puts the processes panel into a slot.
    pub fn insert_processes(&mut self, id: SlotId, p: crate::processes::Processes) {
        self.store.insert(id, TuiPanel::Processes(p));
    }

    /// That slot's disk map, if there is one.
    #[must_use]
    pub fn disk_map(&self, id: SlotId) -> Option<&norte_frontend::diskmap::DiskMap> {
        match self.store.get(id) {
            Some(TuiPanel::DiskMap(m)) => Some(m),
            _ => None,
        }
    }

    /// That slot's disk map, to mutate it.
    pub fn disk_map_mut(&mut self, id: SlotId) -> Option<&mut norte_frontend::diskmap::DiskMap> {
        match self.store.get_mut(id) {
            Some(TuiPanel::DiskMap(m)) => Some(m),
            _ => None,
        }
    }

    /// Puts the disk map into a slot.
    pub fn insert_disk_map(&mut self, id: SlotId, m: norte_frontend::diskmap::DiskMap) {
        self.store.insert(id, TuiPanel::DiskMap(Box::new(m)));
    }

    /// A slot's timeline, if it is one (phase 7).
    #[must_use]
    pub fn timeline(&self, id: SlotId) -> Option<&norte_frontend::timeline::Timeline> {
        match self.store.get(id) {
            Some(TuiPanel::Timeline(t)) => Some(t),
            _ => None,
        }
    }

    /// A slot's timeline, to mutate it.
    pub fn timeline_mut(&mut self, id: SlotId) -> Option<&mut norte_frontend::timeline::Timeline> {
        match self.store.get_mut(id) {
            Some(TuiPanel::Timeline(t)) => Some(t),
            _ => None,
        }
    }

    /// Places a slot's timeline.
    pub fn insert_timeline(&mut self, id: SlotId, t: norte_frontend::timeline::Timeline) {
        self.store.insert(id, TuiPanel::Timeline(Box::new(t)));
    }

    /// What a slot's attributes sheet shows, if there is one.
    #[must_use]
    pub fn metadata(&self, id: SlotId) -> Option<&Option<(norte_proto::Entry, bool)>> {
        match self.store.get(id) {
            Some(TuiPanel::Metadata(e)) => Some(e),
            _ => None,
        }
    }

    /// A slot's attributes sheet, to refresh it.
    pub fn metadata_mut(&mut self, id: SlotId) -> Option<&mut Option<(norte_proto::Entry, bool)>> {
        match self.store.get_mut(id) {
            Some(TuiPanel::Metadata(e)) => Some(e),
            _ => None,
        }
    }

    /// Puts a new attributes sheet, for a freshly minted slot.
    pub fn insert_metadata(&mut self, id: SlotId, e: Option<(norte_proto::Entry, bool)>) {
        self.store.insert(id, TuiPanel::Metadata(Box::new(e)));
    }

    /// Puts a new sidebar into the store, for a freshly minted slot.
    pub fn insert_places(&mut self, id: SlotId, state: norte_frontend::places::PlacesState) {
        self.store.insert(id, TuiPanel::Places(Box::new(state)));
    }

    /// The real store, for whoever already thinks in slots.
    #[must_use]
    pub const fn store(&self) -> &SlotStore<TuiPanel> {
        &self.store
    }

    /// The real store, to mutate it.
    pub const fn store_mut(&mut self) -> &mut SlotStore<TuiPanel> {
        &mut self.store
    }
}

impl Index<usize> for PaneSlots {
    type Output = Pane;

    /// # Panics
    ///
    /// If the side does not carry a listing. In L1a it cannot happen:
    /// `PaneSlots::new` sets both and nothing removes them — the only path
    /// that changes the content is [`PaneSlots::swap`], which exchanges them
    /// between themselves.
    fn index(&self, side: usize) -> &Self::Output {
        self.store
            .get(self.slot_of(side))
            .and_then(TuiPanel::as_browser)
            .expect("the orthodox preset always carries its two listings")
    }
}

impl IndexMut<usize> for PaneSlots {
    /// # Panics
    ///
    /// Same as [`Index::index`].
    fn index_mut(&mut self, side: usize) -> &mut Self::Output {
        self.store
            .get_mut(self.slot_of(side))
            .and_then(TuiPanel::as_browser_mut)
            .expect("the orthodox preset always carries its two listings")
    }
}

impl<'a> IntoIterator for &'a PaneSlots {
    type Item = &'a Pane;
    type IntoIter = Box<dyn Iterator<Item = &'a Pane> + 'a>;

    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter())
    }
}

impl<'a> IntoIterator for &'a mut PaneSlots {
    type Item = &'a mut Pane;
    type IntoIter = Box<dyn Iterator<Item = &'a mut Pane> + 'a>;

    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter_mut())
    }
}

/// The navigation histories, indexed by SLOT and accessed by position.
///
/// They used to be in a `[History; 2]` because there were exactly two panes.
/// With tabs and splits, a history belongs to its listing, not to the
/// screen spot where it is painted today: switching tabs and finding the
/// other one's history would be the same bug as seeing its cursor.
///
/// The order is set by [`Self::set_order`], in the SAME function as
/// [`PaneSlots::set_visible`] and with the same value — they are kept
/// together on purpose, because two order lists that can drift out of sync
/// are a bug that only shows up when switching tabs.
#[derive(Debug, Default)]
pub struct Histories {
    by_slot: BySlot<crate::nav::History>,
    order: Vec<SlotId>,
    /// The `[ui] history_size` cap. `None` = the factory one, which is what
    /// a freshly born `History` already has.
    cap: Option<usize>,
}

impl Histories {
    /// The two at startup.
    #[must_use]
    pub fn new() -> Self {
        Self {
            by_slot: BySlot::new(),
            order: vec![SLOT_LEFT, SLOT_RIGHT],
            cap: None,
        }
    }

    /// Applies `[ui] history_size` to the histories that exist and the ones
    /// that get born afterward (spec 2026-09-15 D4).
    pub fn set_capacity(&mut self, cap: usize) {
        self.cap = Some(cap);
        for (_, h) in self.by_slot.iter_mut() {
            h.set_capacity(cap);
        }
    }

    /// A history just pulled from storage, with the current cap: the one
    /// born via `entry` carries the factory one.
    fn capped(cap: Option<usize>, h: &mut crate::nav::History) -> &mut crate::nav::History {
        if let Some(cap) = cap
            && h.capacity() != cap
        {
            h.set_capacity(cap);
        }
        h
    }

    /// Says which slot occupies each visible position.
    pub fn set_order(&mut self, order: &[SlotId]) {
        if !order.is_empty() {
            self.order = order.to_vec();
        }
    }

    /// A position's slot.
    fn slot_of(&self, side: usize) -> SlotId {
        let i = side.min(self.order.len().saturating_sub(1));
        self.order.get(i).copied().unwrap_or(SLOT_LEFT)
    }

    /// Swaps two positions' history, for the swap-panels gesture.
    pub fn swap(&mut self, a: usize, b: usize) {
        let (sa, sb) = (self.slot_of(a), self.slot_of(b));
        if sa == sb {
            return;
        }
        let (va, vb) = (self.by_slot.remove(sa), self.by_slot.remove(sb));
        if let Some(v) = vb {
            self.by_slot.insert(sa, v);
        }
        if let Some(v) = va {
            self.by_slot.insert(sb, v);
        }
    }

    /// ONE slot's history, by its id.
    #[must_use]
    pub fn for_slot(&self, id: SlotId) -> Option<&crate::nav::History> {
        self.by_slot.get(id)
    }

    /// ONE slot's history, creating it empty if it did not have one.
    pub fn for_slot_mut(&mut self, id: SlotId) -> &mut crate::nav::History {
        let cap = self.cap;
        Self::capped(cap, self.by_slot.entry(id))
    }

    /// Drops the histories of slots the tree no longer has.
    pub fn retain_tree(&mut self, tree: &Node) {
        self.by_slot.retain_tree(tree);
    }
}

impl std::ops::Index<usize> for Histories {
    type Output = crate::nav::History;

    fn index(&self, side: usize) -> &Self::Output {
        // A slot with no history yet is a freshly opened slot: it gets an
        // empty one back, which is exactly its history.
        static EMPTY: std::sync::OnceLock<crate::nav::History> = std::sync::OnceLock::new();
        self.by_slot
            .get(self.slot_of(side))
            .unwrap_or_else(|| EMPTY.get_or_init(crate::nav::History::default))
    }
}

impl std::ops::IndexMut<usize> for Histories {
    fn index_mut(&mut self, side: usize) -> &mut Self::Output {
        let id = self.slot_of(side);
        let cap = self.cap;
        Self::capped(cap, self.by_slot.entry(id))
    }
}

/// The default preset: the WHOLE frame, exactly as seen today.
///
/// ```text
/// Split V   [Weight(1), Auto, Fixed(1)]
///   ├── Split H  [Weight(1), Weight(1)]  →  browser, browser
///   ├── tasks     ← Auto: measures whatever the tasks ask for, ZERO at rest
///   └── status    ← Fixed(1)
/// ```
///
/// The task strip's `Auto` is the reason
/// [`Size::Auto`](norte_frontend::layout::Size::Auto) exists: today it is
/// zero with the system at rest, so a `Fixed(6)` would paint six empty rows
/// where there is now nothing. The frontend substitutes it with
/// [`Node::substitute_auto`] before laying out, because the only one who
/// knows how many tasks there are is whoever has the `TaskBoard` in front of
/// them.
///
/// Comes from the factory PRESET, not from a tree written here: two
/// definitions of the same screen drift apart, and whichever one loads from
/// a file would win without anyone noticing. This module's test pins them as
/// equal.
#[must_use]
pub fn orthodox() -> Node {
    // The `unwrap` is justified by `layout::presets`'s tests, which parse
    // and validate the five presets on every CI run: if this one failed,
    // the binary shipped with an embedded file that does not compile as a
    // tree.
    norte_frontend::layout::presets::tree("orthodox")
        .unwrap_or_else(|e| unreachable!("the factory preset does not parse: {e}"))
}

/// Cells to `ratatui::layout::Rect`, field by field. The names match on
/// purpose: there is no interpretation to do here.
#[must_use]
pub const fn to_ratatui(r: LayoutRect) -> ratatui::layout::Rect {
    ratatui::layout::Rect {
        x: r.x,
        y: r.y,
        width: r.width,
        height: r.height,
    }
}

/// And back.
#[must_use]
pub const fn from_ratatui(r: ratatui::layout::Rect) -> LayoutRect {
    LayoutRect {
        x: r.x,
        y: r.y,
        width: r.width,
        height: r.height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_frontend::layout::Dir;
    use norte_proto::VPath;

    fn pane(wire: &str) -> Pane {
        Pane::new(VPath::parse(wire).expect("wire"), Vec::new())
    }

    /// The four slots the TUI names are the four the file carries.
    ///
    /// `orthodox()` no longer builds the tree, it reads it from the factory
    /// preset, and an equality against itself would prove nothing. What
    /// needs pinning is the other thing: that `SLOT_LEFT` and its three
    /// companions still mean in the file what they mean in the code.
    /// Renumbering the file would leave the ~212 sites that say
    /// `app.panes[0]` pointing at a slot that is not a listing.
    #[test]
    fn the_named_slots_are_the_files() {
        let tree = orthodox();
        assert_eq!(
            tree.slot_ids(),
            vec![SLOT_LEFT, SLOT_RIGHT, SLOT_TASKS, SLOT_STATUS]
        );
        let kind = |id| tree.kind_of(id).expect("kind").as_str().to_owned();
        assert_eq!(kind(SLOT_LEFT), "browser");
        assert_eq!(kind(SLOT_RIGHT), "browser");
        assert_eq!(kind(SLOT_TASKS), "tasks");
        assert_eq!(kind(SLOT_STATUS), "status");
    }

    /// A tree that calls the slot where the listing was `places` left
    /// `visible` pointing at a slot that NO LONGER carries one — the empty
    /// list "keeps the previous one" — and the first `panes[0]` panicked
    /// (#242). With validation in place that tree no longer arrives, but the
    /// list has to be coherent on its own: that is what `Index`'s `expect`
    /// documents.
    #[test]
    fn a_side_never_points_at_a_slot_with_no_listing() {
        let mut slots = PaneSlots::new(pane("mem:///left"), pane("mem:///right"));
        let tree = Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SLOT_LEFT, KindId::new("places")),
                Node::slot(SlotId(5), KindId::browser()),
            ],
        );
        slots.insert_places(SLOT_LEFT, norte_frontend::places::PlacesState::new());
        slots.insert_browser(SlotId(5), pane("mem:///five"));
        slots.refresh_visible(&tree);
        assert_eq!(
            slots.slot_of(0),
            SlotId(5),
            "the side goes to the listing there is"
        );
        assert_eq!(slots[0].dir(), &VPath::parse("mem:///five").expect("wire"));
    }

    /// Indexing by side gives the same listing the array used to give.
    #[test]
    fn the_two_sides_index_like_the_old_array_did() {
        let slots = PaneSlots::new(pane("mem:///left"), pane("mem:///right"));
        assert_eq!(slots[0].dir(), &VPath::parse("mem:///left").expect("wire"));
        assert_eq!(slots[1].dir(), &VPath::parse("mem:///right").expect("wire"));
        assert_eq!(slots.len(), 2);
    }

    /// Iterating walks them left to right. Order matters: the render paints
    /// by index and a reversed order would change the whole screen.
    #[test]
    fn iterating_goes_left_to_right() {
        let slots = PaneSlots::new(pane("mem:///left"), pane("mem:///right"));
        let dirs: Vec<String> = slots.iter().map(|p| p.dir().to_wire()).collect();
        assert_eq!(
            dirs,
            vec!["mem:///left".to_owned(), "mem:///right".to_owned()]
        );
    }

    /// The preset carries the two well-known slots, and only those: if it
    /// carried another id, the startup state and the layout would stop
    /// being the same thing and something that never needed migrating would
    /// have to be migrated.
    #[test]
    fn the_orthodox_preset_carries_the_usual_two_slots() {
        assert_eq!(
            orthodox().slot_ids(),
            vec![SLOT_LEFT, SLOT_RIGHT, SLOT_TASKS, SLOT_STATUS]
        );
    }

    /// The rectangle conversion is field by field in both directions.
    #[test]
    fn rectangles_go_and_come_back_equal() {
        let r = LayoutRect::new(3, 4, 50, 20);
        assert_eq!(from_ratatui(to_ratatui(r)), r);
    }

    /// Swapping moves the content and NOT the slots: a `SlotId` kept from
    /// before still names the same screen spot.
    #[test]
    fn swapping_moves_the_content_and_leaves_the_slots() {
        let mut slots = PaneSlots::new(pane("mem:///left"), pane("mem:///right"));
        slots.swap(0, 1);
        assert_eq!(slots[0].dir(), &VPath::parse("mem:///right").expect("wire"));
        assert_eq!(slots[1].dir(), &VPath::parse("mem:///left").expect("wire"));
        assert!(
            slots.store().get(SLOT_LEFT).is_some(),
            "the left slot still exists"
        );
    }
}
