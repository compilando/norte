//! The panes as seen from `App`: which one has focus, how it's switched and
//! swapped, which window of entries needs a `stat`, and a slot's tabs (open,
//! close, cycle, go to one and move it).

use super::pane::Pane;
use super::{App, KeyOwner};
use norte_proto::{EntryKind, VPath};

impl App {
    /// Index of the focused pane (0 = left, 1 = right).
    #[must_use]
    pub fn focus(&self) -> usize {
        self.focus
    }

    /// The focused pane.
    #[must_use]
    pub fn focused(&self) -> &Pane {
        &self.panes[self.focus]
    }

    /// (index, path) of the focused File entry with no `size`: a candidate
    /// for the stat-on-focus probe (#52, lazy listing).
    #[must_use]
    pub fn focused_needs_stat(&self) -> Option<(usize, VPath)> {
        let e = self.focused().selected()?;
        (e.kind == EntryKind::File && e.size.is_none()).then(|| (self.focus(), e.path.clone()))
    }

    /// Candidates to hydrate from the visible WINDOW (#52, lazy listing) in
    /// BOTH panes — both get painted at once, so probing only the focused
    /// entry left the Size/Date columns blank in everything else. The
    /// focused pane goes first; the selection WITHIN each pane belongs to
    /// the shared model ([`norte_frontend::PaneState::needs_stat_window`],
    /// rule 7).
    #[must_use]
    pub fn needs_stat_window(&self, radius: usize) -> Vec<(usize, VPath)> {
        let mut out = Vec::new();
        for pane_idx in [self.focus(), self.focus() ^ 1] {
            out.extend(
                self.panes[pane_idx]
                    .needs_stat_window(radius)
                    .into_iter()
                    .map(|p| (pane_idx, p)),
            );
        }
        out
    }

    /// The focused pane, mutable.
    pub fn focused_mut(&mut self) -> &mut Pane {
        &mut self.panes[self.focus]
    }

    /// Moves focus to the next LISTING, cycling (`pane.switch`, the orthodox
    /// `Tab`).
    ///
    /// Used to be `focus ^= 1`, and that's a count of TWO. As soon as
    /// `layout.split-v` puts a third listing on screen —
    /// `[left, new, right]` — the tab key only alternated between the first
    /// two: from index 2, `2 ^ 1` is 3, which doesn't exist, and
    /// `PaneSlots` CLAMPS out of range instead of panicking, so the key did
    /// nothing and didn't say so. The panel you hadn't split was
    /// unreachable.
    ///
    /// Listings only, and that's why it isn't [`Self::layout_focus`]: `Tab`
    /// is "the other panel" of any orthodox manager, and with the places
    /// bar, the tree and the viewer open, a ring over the whole screen
    /// would force five keypresses just to get back to the listing next
    /// door. The side panels are reached with `layout.focus-next` and each
    /// one's own key.
    ///
    /// And with the SAME landing as the big ring, which is also what hands
    /// back the keyboard: `tab` is in `[global]`, so it can be pressed with
    /// the docked viewer focused, and without this the focus border jumped
    /// to the listing next door while the arrows kept moving the viewer.
    /// `Tab` gets you out of a side panel — that's what guarantees no
    /// combination leaves the reader stuck inside one.
    pub fn switch_focus(&mut self) {
        let n = self.panes.len();
        if n < 2 {
            // With a single listing there's no "the other one", and the
            // keyboard gets handed back anyway: pressing `Tab` inside a side
            // panel has to get you out of it even if there's nowhere to go
            // afterward.
            self.return_keys_to_panes();
            return;
        }
        self.land(FocusStop::Pane((self.focus + 1) % n));
    }

    /// Exchanges the two panes and everything `App` keeps beside them
    /// (`pane.swap`).
    ///
    /// Touches no disk: no listing is refetched, nothing can fail, and the
    /// marks, the filter, the sort and the cursor all survive because the
    /// WHOLE pane moves rather than being rebuilt.
    ///
    /// The focus stays on the same physical SIDE on purpose. Moving it along
    /// with the content would make the command a no-op from where the reader
    /// sits: they would still be looking at the same listing, just on the
    /// other half of the screen.
    ///
    /// The history moves WITH the pane, because it belongs to the content and
    /// not to the side of the screen. Left behind, each pane would offer to
    /// take the reader "back" to places that content has never been.
    ///
    /// NOT the whole story: the run loop keeps its own state indexed by pane
    /// (the paginated fills in flight, the decoration fetches, the stat-probe
    /// dedup, the live search run) which `App` cannot see.
    /// `main::reconcile_swap` is the other half, and the two are driven
    /// together by `Cd::Swapped`.
    pub fn swap_panes(&mut self) {
        self.panes.swap(0, 1);
        self.history.swap(0, 1);
        // The ONLY thing left as a trace that a swap happened: everything
        // else travels with its pane, so whoever compares by side sees
        // nothing move (see [`Self::swap_seq`]).
        self.swap_seq = self.swap_seq.wrapping_add(1);
    }

    /// Mints a `SlotId` never used before in this session.
    pub(super) fn mint_slot(&mut self) -> norte_frontend::layout::SlotId {
        let id = norte_frontend::layout::SlotId(self.next_slot);
        self.next_slot = self.next_slot.saturating_add(1);
        id
    }

    /// The slot the focused side is showing right now.
    #[must_use]
    pub fn focused_slot(&self) -> norte_frontend::layout::SlotId {
        self.panes.slot_of(self.focus)
    }

    /// Opens a new tab next to the focused pane, in the same directory, with
    /// its listing already inherited ([`App::fork_pane`]).
    pub fn tab_new(&mut self) {
        let focus = self.focused_slot();
        let id = self.mint_slot();
        let new_pane = self.fork_pane(self.focus);
        self.panes.insert_browser(id, new_pane);
        self.layout = self.layout.add_tab(
            focus,
            &norte_frontend::layout::Node::slot(id, norte_frontend::layout::KindId::browser()),
        );
        self.panes.refresh_visible(&self.layout);
        self.prune_by_tree();
    }

    /// Closes the focused tab. No effect if the pane isn't in a group.
    pub fn tab_close(&mut self) {
        let focus = self.focused_slot();
        if let Some(new_layout) = self.layout.close_tab(focus) {
            self.layout = new_layout;
            self.panes.refresh_visible(&self.layout);
            self.prune_by_tree();
        }
    }

    /// Switches tabs within the focused group, cycling.
    pub fn tab_cycle(&mut self, delta: isize) {
        let focus = self.focused_slot();
        let Some((tabs, active)) = self.layout.tabs_of(focus) else {
            return;
        };
        if tabs.is_empty() {
            return;
        }
        let n = isize::try_from(tabs.len()).unwrap_or(1);
        let i = isize::try_from(active).unwrap_or(0);
        let dest = usize::try_from((i + delta).rem_euclid(n)).unwrap_or(0);
        self.layout = self.layout.set_active_for(focus, dest);
        self.panes.refresh_visible(&self.layout);
        self.prune_by_tree();
        // #329: switching tabs can hide the panel that held the keyboard,
        // and then keys went to something no longer on screen. Nothing
        // closes it, so without this there was nothing to hand it back to
        // the listings.
        self.settle_key_owner();
    }

    /// Goes to tab `n` (1-based) of the focused group.
    pub fn tab_goto(&mut self, n: usize) {
        let focus = self.focused_slot();
        if self.layout.tabs_of(focus).is_some() {
            self.layout = self.layout.set_active_for(focus, n.saturating_sub(1));
            self.panes.refresh_visible(&self.layout);
            self.prune_by_tree();
            // Same reason as in `tab_cycle` (#329).
            self.settle_key_owner();
        }
    }

    /// Moves the focused tab within its group. Doesn't wrap around: a tab
    /// jumping from the end to the start on one extra keypress is exactly
    /// what nobody wanted.
    pub fn tab_move(&mut self, delta: isize) {
        let focus = self.focused_slot();
        if self.layout.tabs_of(focus).is_some() {
            self.layout = self.layout.move_tab(focus, delta);
            self.panes.refresh_visible(&self.layout);
            self.prune_by_tree();
        }
    }

    /// How many `browser`s are in the tree, visible or hidden.
    pub(super) fn browsers_in_tree(&self) -> usize {
        self.layout
            .slot_ids()
            .into_iter()
            .filter(|id| {
                self.layout
                    .kind_of(*id)
                    .is_some_and(|k| *k == norte_frontend::layout::KindId::browser())
            })
            .count()
    }

    /// The keyboard ring's stops, in the order they appear on screen.
    ///
    /// Who gets in is decided by the kind REGISTRY, not a list written here:
    /// `takes_keys` is exactly the question — does this panel consume its
    /// own keys? — and it's already answered in a spot both frontends
    /// share. That's why metadata is left out: it gets FOCUSED (the layout
    /// counts it) but doesn't take keys, so stopping there would be a spot
    /// no key gets you out of.
    ///
    /// The order is the TREE's, which is the screen's: cycling has to
    /// follow the view, not the order the panels were opened in.
    ///
    /// A kind that takes keys and that this screen doesn't know how to
    /// focus — `compare`, `sync`, which in the TUI are overlays and not
    /// slots — gets skipped: it has no [`KeyOwner`] to hand anything to.
    #[must_use]
    fn focus_ring(&self) -> Vec<FocusStop> {
        self.layout
            .visible_slot_ids()
            .into_iter()
            .filter_map(|id| self.focus_stop(id))
            .collect()
    }

    /// The ring stop the `id` slot occupies, or `None` if that slot doesn't
    /// take keys.
    ///
    /// It's the ONLY translation from "slot" to "who keeps the keyboard",
    /// and it's shared by the `Tab` ring and the mouse: two tables from
    /// kinds to [`KeyOwner`] are two ways to reach a panel that one day
    /// drift apart and leave a panel reachable by mouse but not by
    /// keyboard.
    #[must_use]
    fn focus_stop(&self, id: norte_frontend::layout::SlotId) -> Option<FocusStop> {
        // The app's LIVE registry, not a freshly built stock one: since
        // phase 3 it carries the panels plugins contribute inside it, and
        // with `builtin()` a contributed panel didn't even get past this
        // gate — it was left out of the `Tab` ring and out of the mouse's
        // reach.
        let kind = self.layout.kind_of(id)?;
        if !self.kinds.get(kind).is_some_and(|d| d.takes_keys) {
            return None;
        }
        // A plugin panel is resolved by PREFIX, before the built-in name
        // table: its kind isn't known at compile time.
        if kind.as_str().starts_with("plugin:") {
            return Some(FocusStop::Side(KeyOwner::Panel));
        }
        match kind.as_str() {
            "browser" => (0..self.panes.len())
                .find(|i| self.panes.slot_of(*i) == id)
                .map(FocusStop::Pane),
            "places" => Some(FocusStop::Side(KeyOwner::Places)),
            crate::preview::KIND => Some(FocusStop::Side(KeyOwner::Preview)),
            crate::processes::KIND => Some(FocusStop::Side(KeyOwner::Processes)),
            crate::tree::KIND => Some(FocusStop::Side(KeyOwner::Tree)),
            crate::logview::KIND => Some(FocusStop::Side(KeyOwner::Log)),
            crate::diskmap::KIND => Some(FocusStop::Side(KeyOwner::DiskMap)),
            crate::timeline::KIND => Some(FocusStop::Side(KeyOwner::Timeline)),
            // The terminal is only a ring stop if there IS a way out. With
            // no loose chord to exit it, entering would mean staying stuck
            // inside: the panel is seen and looked at, and the keyboard
            // stays with the listings.
            crate::termpanel::KIND => self
                .terminal_chord
                .map(|_| FocusStop::Side(KeyOwner::Terminal)),
            _ => None,
        }
    }

    /// Hands the keyboard to slot `id` — and focus, if it's a listing — and
    /// says whether it accepted.
    ///
    /// Called by the MOUSE: pointing at a panel means "I work here now",
    /// and that includes the keys. Before, a click moved the listing's
    /// cursor and left the arrows wherever they were, so the focus border
    /// said one thing and the keyboard went to another.
    ///
    /// A slot that doesn't take keys — metadata, the task strip, the
    /// status bar — returns `false` and changes nothing: clicking something
    /// that isn't listening can't leave the keyboard without an owner.
    pub fn focus_slot(&mut self, id: norte_frontend::layout::SlotId) -> bool {
        let Some(stop) = self.focus_stop(id) else {
            return false;
        };
        self.land(stop);
        // Pointing at a plugin panel says WHICH one, and that doesn't fit in
        // `KeyOwner::Panel`: without this, with two contributed panels
        // visible the keyboard went to one and `layout.grow` to the other.
        if stop == FocusStop::Side(KeyOwner::Panel) {
            self.panel_focus = Some(id);
        }
        true
    }

    /// Puts the keyboard (and focus) on a ring stop.
    fn land(&mut self, stop: FocusStop) {
        match stop {
            FocusStop::Pane(i) => {
                self.return_keys_to_panes();
                self.set_focus(i);
                // Changes which listing is focused, so it changes what the
                // tree points at.
                self.follow_tree();
            }
            FocusStop::Side(owner) => self.key_owner = owner,
        }
    }

    /// Hands the keyboard to the ring's next panel, cycling.
    ///
    /// To ALL panels, not just the listings. `Tab` alternates between the
    /// two listings and every side panel opens and gets focused with its own
    /// key, so with the sidebar and the viewer up front there was no way to
    /// walk the screen: getting from the sidebar to the viewer meant
    /// remembering each one's key. This is the one that requires memorizing
    /// nothing.
    ///
    /// A single-stop ring does nothing, and that case matters: `delta` over
    /// a screen with a single listing and no panels must be a no-op, not a
    /// `set_focus` that clamps against itself.
    pub fn layout_focus(&mut self, delta: isize) {
        let ring = self.focus_ring();
        if ring.len() < 2 {
            return;
        }
        let current = match self.key_owner() {
            KeyOwner::Panes => FocusStop::Pane(self.focus),
            other => FocusStop::Side(other),
        };
        // If the current spot isn't in the ring — a frame where the layout
        // hasn't placed anything yet — it starts from the beginning instead
        // of going nowhere.
        let i = ring.iter().position(|s| *s == current).unwrap_or(0);
        let n = isize::try_from(ring.len()).unwrap_or(1);
        let dest =
            usize::try_from((isize::try_from(i).unwrap_or(0) + delta).rem_euclid(n)).unwrap_or(0);
        self.land(ring[dest]);
    }
}

/// A ring stop [`App::layout_focus`] walks through.
///
/// A listing is named by its POSITION and a side panel by who keeps the
/// keyboard, because those are the two ways `App` has of saying "here":
/// [`App::focus`] is an index over the visible listings and can't point at
/// a sidebar (see [`KeyOwner`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusStop {
    /// The listing at that position.
    Pane(usize),
    /// The side panel that keeps the keyboard.
    Side(KeyOwner),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::pane::Pane;
    use crate::app::testutil::*;
    use norte_proto::EntryKind;

    /// #52: `needs_stat_window` hydrates what's VISIBLE, not just what's
    /// focused — the Size/Date columns came out blank in every row except
    /// the cursor's. Both panes get painted at once, so both contribute
    /// candidates (the focused one first); out of radius, no; a Dir, never;
    /// already hydrated, not either.
    #[test]
    fn needs_stat_window_covers_both_panes_within_the_radius() {
        let lazy = |n: &str| {
            let mut e = file(n);
            e.size = None;
            e
        };
        let mut dir_lazy = lazy("z-dir");
        dir_lazy.kind = EntryKind::Dir;
        let left = vec![lazy("a.txt"), lazy("b.txt"), lazy("c.txt"), dir_lazy];
        let right = vec![lazy("d.txt"), file("e.txt")];
        let mut app = App::new(Pane::new(root(), left), Pane::new(root(), right));
        // `Pane::new` sorts (dirs first): [z-dir, a, b, c].
        app.panes[0].set_cursor(1);

        let window = app.needs_stat_window(1);
        let names: Vec<String> = window
            .iter()
            .map(|(p, path)| format!("{p}:{}", path.display_lossy()))
            .collect();
        assert!(
            names
                .iter()
                .any(|n| n.starts_with("0:") && n.ends_with("/a.txt"))
                && names
                    .iter()
                    .any(|n| n.starts_with("0:") && n.ends_with("/b.txt")),
            "cursor ± radius of the focused pane: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n.contains("c.txt")),
            "out of radius doesn't get probed: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n.contains("z-dir")),
            "a Dir is never probed: {names:?}"
        );
        assert!(
            names
                .iter()
                .any(|n| n.starts_with("1:") && n.ends_with("/d.txt")),
            "the UNFOCUSED pane also gets painted: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n.contains("e.txt")),
            "already hydrated, not a candidate: {names:?}"
        );
        assert_eq!(window[0].0, 0, "the focused pane goes first");

        // A generous radius reaches both panes' whole listing.
        assert_eq!(app.needs_stat_window(64).len(), 4);
    }

    /// #52: `focused_needs_stat` flags the focused File entry with NO
    /// `size` (a candidate for the lazy probe). Already hydrated or being a
    /// Dir, it doesn't apply.
    #[test]
    fn focused_needs_stat_only_file_lazy() {
        let mut lazy = file("a.txt");
        lazy.size = None;
        let mut app = App::new(
            Pane::new(root(), vec![lazy.clone()]),
            Pane::new(root(), vec![]),
        );
        assert_eq!(
            app.focused_needs_stat(),
            Some((0, lazy.path.clone())),
            "a File with no size is a candidate"
        );

        // Already hydrated: stops being a candidate.
        app.panes[0].hydrate(&lazy.path, Some(5), None);
        assert!(app.focused_needs_stat().is_none(), "already has a size");

        // A Dir is never probed, even without a size.
        let mut dir_lazy = file("b");
        dir_lazy.kind = EntryKind::Dir;
        dir_lazy.size = None;
        app.panes[0] = Pane::new(root(), vec![dir_lazy]);
        assert!(app.focused_needs_stat().is_none(), "a Dir isn't probed");
    }

    /// The swap crosses the pane AND its history, and leaves focus on the
    /// same SIDE: whoever was looking left keeps looking left, and now
    /// what's there is what used to be on the right.
    #[test]
    fn the_swap_crosses_pane_and_history_and_doesnt_move_focus() {
        let mut app = app_en("mem:///left", "mem:///right");
        app.history[0].record(vp("mem:///trail-left"));
        app.history[1].record(vp("mem:///trail-right"));
        app.set_focus(0);

        app.swap_panes();

        assert_eq!(app.panes[0].dir(), &vp("mem:///right"));
        assert_eq!(app.panes[1].dir(), &vp("mem:///left"));
        assert_eq!(app.focus(), 0, "focus stays on its side");
        // The trail travels with the CONTENT, not with the side: otherwise
        // the popup would offer to take you "back" to places that content
        // never was.
        assert_eq!(
            app.history[0].entries().front(),
            Some(&vp("mem:///trail-right"))
        );
        assert_eq!(
            app.history[1].entries().front(),
            Some(&vp("mem:///trail-left"))
        );
        // And the back/forward TRAIL travels too, not just the MRU the
        // popup paints: they're two structures inside the same `History`.
        assert_eq!(app.history[0].back_len(), 1);
        assert_eq!(
            app.history[0].step_back(vp("mem:///right")),
            Some(vp("mem:///trail-right")),
            "pane 0's back points at the trail that arrived with its content"
        );
    }

    /// Two swaps are the identity.
    #[test]
    fn two_swaps_leave_everything_as_it_was() {
        let mut app = app_en("mem:///left", "mem:///right");
        app.swap_panes();
        app.swap_panes();
        assert_eq!(app.panes[0].dir(), &vp("mem:///left"));
        assert_eq!(app.panes[1].dir(), &vp("mem:///right"));
    }

    /// Focus also stays on the SIDE when it was on the right: the swap
    /// doesn't touch `focus` at all. (Control mutation: adding
    /// `self.focus ^= 1` to `swap_panes` breaks here and in the test above
    /// at the same time.)
    #[test]
    fn the_swap_with_focus_on_the_right_doesnt_move_it_either() {
        let mut app = app_en("mem:///left", "mem:///right");
        app.set_focus(1);
        app.swap_panes();
        assert_eq!(app.focus(), 1);
        assert_eq!(
            app.focused().dir(),
            &vp("mem:///left"),
            "on the right side now is what used to be on the left"
        );
    }

    /// The ring walks ALL panels, not just the listings.
    ///
    /// This was the missing piece: `Tab` alternates the two listings and
    /// every side panel opens with its own key, so with the sidebar and the
    /// viewer up front there was no way to walk the screen without
    /// remembering three different keys.
    #[test]
    fn the_ring_passes_through_the_side_panels() {
        let mut app = app_dos_panes();
        app.toggle_places();
        app.toggle_preview();
        // Opening a panel takes the keyboard; the ring is tested from the
        // listings.
        app.return_keys_to_panes();
        app.set_focus(0);

        // The sidebar is docked on the LEFT, so it's first in the ring and
        // from listing 0 it's reached going BACKWARD.
        app.layout_focus(-1);
        assert_eq!(app.key_owner(), KeyOwner::Places);

        // And going forward it walks both listings and the viewer.
        let mut seen = vec![(app.key_owner(), app.focus())];
        for _ in 0..3 {
            app.layout_focus(1);
            seen.push((app.key_owner(), app.focus()));
        }
        assert_eq!(
            seen,
            vec![
                (KeyOwner::Places, 0),
                (KeyOwner::Panes, 0),
                (KeyOwner::Panes, 1),
                (KeyOwner::Preview, 1),
            ],
            "the order is the screen's: sidebar, listings, viewer"
        );

        // And it wraps around.
        app.layout_focus(1);
        assert_eq!(app.key_owner(), KeyOwner::Places);
    }

    /// With nothing but a listing, cycling does nothing. It matters because
    /// the previous calculation was a modulo over the number of listings,
    /// and a ring of one left it spinning on itself.
    #[test]
    fn a_ring_of_one_goes_nowhere() {
        use norte_frontend::layout::{KindId, Node, SlotId};
        let mut app = app_dos_panes();
        // A single-listing, no-panel layout: it comes from a saved layout,
        // not `layout.close` — which refuses to leave the screen without
        // two listings.
        app.set_layout(Node::slot(SlotId(0), KindId::browser()));
        app.layout_focus(1);
        assert_eq!(app.key_owner(), KeyOwner::Panes);
        assert_eq!(app.focus(), 0);
    }

    /// **`Tab` reaches the third listing.**
    ///
    /// Used to be `focus ^= 1`, a count of two: with `[left, new, right]`
    /// on screen, from index 2 that gave 3 — which doesn't exist — and
    /// `PaneSlots` CLAMPS out of range instead of panicking, so the key did
    /// nothing and didn't say so. The panel you hadn't split was
    /// unreachable.
    #[test]
    fn tab_reaches_the_third_listing() {
        use norte_frontend::layout::{Dir, KindId, Node, Size, SlotId};
        let mut app = app_dos_panes();
        app.set_layout(Node::Split {
            dir: Dir::Horizontal,
            sizes: vec![Size::Weight(1), Size::Weight(1), Size::Weight(1)],
            children: vec![
                Node::slot(SlotId(0), KindId::browser()),
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        });
        assert_eq!(app.panes.len(), 3, "three listings on screen");

        app.set_focus(0);
        let mut seen = Vec::new();
        for _ in 0..3 {
            app.switch_focus();
            seen.push(app.focus());
        }
        assert_eq!(
            seen,
            vec![1, 2, 0],
            "all three, and the whole loop in three jumps"
        );
    }

    /// With a single listing, `Tab` is a no-op: there's no other panel, and
    /// spinning on yourself would be pretending something happened.
    #[test]
    fn tab_over_a_single_listing_does_nothing() {
        use norte_frontend::layout::{KindId, Node, SlotId};
        let mut app = app_dos_panes();
        app.set_layout(Node::slot(SlotId(0), KindId::browser()));
        app.switch_focus();
        assert_eq!(app.focus(), 0);
    }

    /// The METADATA panel isn't a ring stop: it has no `KeyOwner`, so
    /// stopping there would be a spot no key gets you out of.
    #[test]
    fn metadata_is_not_a_stop() {
        let mut app = app_dos_panes();
        app.toggle_metadata();
        app.return_keys_to_panes();
        app.set_focus(0);
        for _ in 0..2 {
            app.layout_focus(1);
        }
        assert_eq!(app.key_owner(), KeyOwner::Panes);
        assert_eq!(app.focus(), 0, "two jumps between two listings: full loop");
    }
}
