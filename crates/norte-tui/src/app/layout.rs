//! The layout as seen from `App`: changing the whole tree, opening and
//! closing the side slots (places, preview, tree, processes, metadata),
//! resizing and moving focus from slot to slot.

use super::{
    ALLOW_DISK_MAP, ALLOW_LOG, ALLOW_PANEL, ALLOW_PROCESSES, App, KeyOwner, PlacesClick, TreeClick,
    TreeSpot,
};
use norte_i18n::t;
use norte_proto::VPath;

impl App {
    /// Changes the whole layout, bringing what depends on it up to date.
    ///
    /// The new tree's slots with no listing are created empty in the
    /// focused panel's directory: a saved layout names slots, it doesn't say
    /// what was inside them, and starting with dead panels would be worse
    /// than starting with duplicated ones.
    pub fn set_layout(&mut self, tree: norte_frontend::layout::Node) {
        let dir = self.panes[self.focus].dir().clone();
        for id in tree.slot_ids() {
            // EVERY kind with its own state gets seeded, not just the
            // listing: a preset brings the sidebar, the viewer, processes
            // and the attribute sheet, and a slot with no state paints
            // empty forever — the toggle that would have created it isn't
            // going to be pressed, because the panel is already there.
            // What already exists is respected: switching layouts doesn't
            // erase your navigation.
            match tree.kind_of(id).map(norte_frontend::layout::KindId::as_str) {
                Some("browser") if self.panes.browser(id).is_none() => {
                    let new_pane = self.new_pane(dir.clone(), Vec::new());
                    self.panes.insert_browser(id, new_pane);
                }
                Some("places") if self.panes.places(id).is_none() => {
                    self.panes
                        .insert_places(id, norte_frontend::places::PlacesState::new());
                }
                Some("viewer") if self.panes.preview(id).is_none() => {
                    self.panes
                        .insert_preview(id, crate::preview::Preview::new());
                }
                Some(crate::processes::KIND) if self.panes.processes(id).is_none() => {
                    self.panes
                        .insert_processes(id, crate::processes::Processes::default());
                }
                Some(crate::metadata::KIND) if self.panes.metadata(id).is_none() => {
                    self.panes.insert_metadata(id, None);
                }
                // #136: anchored where the listing is, same as opening it by
                // hand. A saved layout with the tree inside — yesterday's
                // session, a preset that brings it — arrives through here,
                // and without this arm the slot paints blank forever: the
                // toggle that would have created its state isn't going to be
                // pressed, because the panel is already on screen.
                Some(crate::tree::KIND) if self.panes.tree(id).is_none() => {
                    let mut tree = crate::tree::Tree::default();
                    tree.anchor_near(&dir, &norte_frontend::shell::home_vpath());
                    self.panes.insert_tree(id, tree);
                }
                // Phase 7, and for the same reason as the tree above: a
                // layout that brings the timeline — yesterday's session, a
                // profile — doesn't go through the toggle that would have
                // created its state, and without this the slot stays blank
                // forever. Born EMPTY; the loop, which is what has the
                // backend, is what fills it.
                Some(crate::timeline::KIND) if self.panes.timeline(id).is_none() => {
                    self.panes
                        .insert_timeline(id, norte_frontend::timeline::Timeline::default());
                    self.timeline_stale = true;
                }
                _ => {}
            }
            // The layout's ids can't collide with the ones minted later.
            self.next_slot = self.next_slot.max(id.0.saturating_add(1));
        }
        self.layout = tree;
        self.panes.refresh_visible(&self.layout);
        self.prune_by_tree();
        self.settle_key_owner();
        // A freshly seeded sidebar is born EMPTY, and its key was what used
        // to fill it. A layout that brings it — `full`, `explorer`,
        // yesterday's session, a profile — never presses it, so the panel
        // used to stay blank forever: favorites go right here and drives
        // are requested through the flag, because they're I/O.
        self.sync_places_favorites();
        self.places_wants_drives |= self.places_drives_visible();
        self.set_focus(0);
    }

    /// Hands the keyboard back to the listings if whoever had it is no
    /// longer in the layout.
    ///
    /// Without this, switching layouts with a FOCUSED side panel —
    /// switching profiles, applying a preset, restoring a session — left
    /// `key_owner` pointing at a panel that no longer exists. And then
    /// EVERY key gets routed to its handler, `<panel>_slot()` returns
    /// `None`, each arm is a no-op, and the whole manager stops responding
    /// with nothing on screen to explain why. It isn't even reversible by
    /// eye: the panel's key RE-OPENS it, so opening it looks like it "fixes"
    /// the keyboard.
    ///
    /// Checked by the KIND in the tree and not by a separate flag: the
    /// question is literally "is it still there?", and a flag is a second
    /// place to get it wrong.
    /// Since #329 it asks whether the panel IS VISIBLE, not whether it
    /// exists. A panel left behind a tab — because the reader switched
    /// tabs, not because they closed anything — has a keyboard just as
    /// useless as a closed one: keys go to something not on screen. And the
    /// status bar paints it closed, so what the reader sees and what's in
    /// charge stopped being the same.
    pub(crate) fn settle_key_owner(&mut self) {
        let still_there = match self.key_owner {
            KeyOwner::Panes => true,
            KeyOwner::Places => self.slot_of_kind_visible("places").is_some(),
            KeyOwner::Preview => self.slot_of_kind_visible(crate::preview::KIND).is_some(),
            KeyOwner::Processes => self.slot_of_kind_visible(crate::processes::KIND).is_some(),
            KeyOwner::Tree => self.slot_of_kind_visible(crate::tree::KIND).is_some(),
            KeyOwner::DiskMap => self.slot_of_kind_visible(crate::diskmap::KIND).is_some(),
            KeyOwner::Timeline => self.slot_of_kind_visible(crate::timeline::KIND).is_some(),
            KeyOwner::Log => self.slot_of_kind_visible(crate::logview::KIND).is_some(),
            // The terminal, same as the rest: if its slot goes behind a
            // tab, the keys go back to the listings. The shell stays alive
            // behind it — what's lost is the keyboard, not the process.
            KeyOwner::Terminal => self.slot_of_kind_visible(crate::termpanel::KIND).is_some(),
            // A plugin panel keeps the keyboard as long as it's VISIBLE. If
            // the plugin gets disabled, or its slot goes behind a tab, the
            // keys go back to the listings like with any other panel.
            KeyOwner::Panel => self.panel_slot().is_some(),
        };
        if !still_there {
            self.key_owner = KeyOwner::Panes;
        }
    }

    /// Splits the focused panel in two, with the new one next to it.
    ///
    /// The new panel inherits the split one's directory and entries, same
    /// as a new tab: it's the same thing being looked at, so it shows up
    /// full instead of flickering empty while someone rereads the same
    /// thing. And it keeps the FOCUS, which is what was just requested.
    ///
    /// It's REFUSED when the focused slot no longer has room for two, and
    /// it says so on the status bar. Without that, the key created a panel
    /// the layout hid in the same frame — the `Split` doesn't fit,
    /// degrades to tabs and the screen shows one again, with the tree
    /// keeping the new one regardless — so from the outside it sometimes
    /// split, sometimes did nothing and sometimes looked like it undid the
    /// previous action. The count is done by the same spot that decides the
    /// collapse ([`norte_frontend::layout::has_room_to_split`]) over the
    /// LAST frame's rectangle: a slot's size isn't known by the tree, it's
    /// known by the screen. With no frame yet, nothing gets refused — not
    /// knowing isn't the same as knowing it doesn't fit.
    pub fn layout_split(&mut self, dir: norte_frontend::layout::Dir) {
        let focus = self.focused_slot();
        if let Some(rect) = self.mouse.slot_rect(focus)
            && !norte_frontend::layout::has_room_to_split(
                rect,
                dir,
                &norte_frontend::layout::KindId::browser(),
                &self.kinds,
            )
        {
            self.message = Some(t("msg-layout-split-no-room"));
            return;
        }
        let id = self.mint_slot();
        let new_pane = self.fork_pane(self.focus);
        self.panes.insert_browser(id, new_pane);
        self.layout = self.layout.split_slot(
            focus,
            dir,
            &norte_frontend::layout::Node::slot(id, norte_frontend::layout::KindId::browser()),
        );
        self.panes.refresh_visible(&self.layout);
        self.prune_by_tree();
        // Focus to the newborn: splitting is asking for room to work in it.
        if let Some(i) = (0..self.panes.len()).find(|i| self.panes.slot_of(*i) == id) {
            self.set_focus(i);
        }
    }

    /// Who has the body's keyboard right now (L3).
    #[must_use]
    pub const fn key_owner(&self) -> KeyOwner {
        self.key_owner
    }

    /// Hands the keyboard back to the listings.
    ///
    /// Called by `dialog.cancel` from the sidebar and `viewer.close` from
    /// the docked viewer: both release the keys WITHOUT closing the panel —
    /// closing something the reader only wanted to stop operating is the
    /// wrong answer, and its own layout command is what closes it.
    pub const fn return_keys_to_panes(&mut self) {
        self.key_owner = KeyOwner::Panes;
    }

    /// The places sidebar's slot, if it's in the tree.
    #[must_use]
    pub fn places_slot(&self) -> Option<norte_frontend::layout::SlotId> {
        self.slot_of_kind("places")
    }

    /// Copies the current favorites to the sidebar, if it's in the layout.
    ///
    /// From [`Self::hotlist`], the copy startup and every add or remove
    /// keep: the sidebar never rereads the config nor keeps an old snapshot
    /// of it.
    ///
    /// The `Ctrl+D` popup and this panel paint THE SAME data, and for a
    /// while only the popup found out about changes: you added a favorite,
    /// it showed up in the popup, and the panel next to it stayed without
    /// it. That's why everything that touches the list goes through here.
    pub fn sync_places_favorites(&mut self) {
        let Some(id) = self.places_slot() else {
            return;
        };
        let items: Vec<(String, Result<VPath, String>)> = self
            .hotlist
            .iter()
            .map(|h| (h.name.clone(), h.target.clone()))
            .collect();
        if let Some(state) = self.panes.places_mut(id) {
            state.set_favorites(&items);
        }
    }

    /// The first slot of the TREE with that kind, visible or not.
    ///
    /// From the tree and not from the layout: whoever asks if the sidebar
    /// is open wants to know if it exists, and a slot behind a tab still
    /// exists.
    fn slot_of_kind(&self, kind: &str) -> Option<norte_frontend::layout::SlotId> {
        self.layout
            .slot_ids()
            .into_iter()
            .find(|id| self.layout.kind_of(*id).is_some_and(|k| k.as_str() == kind))
    }

    /// The first slot with that kind the reader SEES right now (#329).
    ///
    /// [`Self::slot_of_kind`]'s pair, and both are needed because there are
    /// two questions: whoever is about to place a panel wants to know if it
    /// already exists — duplicating it would be the bad outcome — and
    /// whoever paints a button or counts something new wants to know if the
    /// reader has it in front of them. Asking the first and acting as if it
    /// were the second is what made a panel hidden in a tab paint open and
    /// swallow its notification mark.
    pub(crate) fn slot_of_kind_visible(
        &self,
        kind: &str,
    ) -> Option<norte_frontend::layout::SlotId> {
        self.layout
            .visible_slot_ids()
            .into_iter()
            .find(|id| self.layout.kind_of(*id).is_some_and(|k| k.as_str() == kind))
    }

    /// Does the reader have this slot in front of them?
    ///
    /// Answers by TAB, not by fit, and that asymmetry with the status bar
    /// is deliberate (#331): the status bar derives from the layout's
    /// placements — it knows what fits — and here that can't be done,
    /// because `App` doesn't keep the painted area. A toggle reasons over
    /// the tree, which is all it has.
    ///
    /// Consequence, written down so nobody rediscovers it: a panel whose tab
    /// is active but that the layout drops for lack of room paints closed
    /// and this key closes it. Fixing it would require putting the last
    /// area into the state — presentation data living where it doesn't
    /// belong — which is a separate decision and probably worse than the
    /// asymmetry.
    fn is_visible(&self, id: norte_frontend::layout::SlotId) -> bool {
        self.layout.visible_slot_ids().contains(&id)
    }

    /// Brings slot `id` into view: activates its tab in every group along
    /// the path.
    ///
    /// It isn't a gesture of its own and that's why it doesn't touch the
    /// keyboard: the toggles call it before focusing, because focusing
    /// something not visible is sending the keys nowhere.
    fn reveal(&mut self, id: norte_frontend::layout::SlotId) {
        let new_layout = self.layout.reveal(id);
        if new_layout != self.layout {
            self.layout = new_layout;
            self.panes.refresh_visible(&self.layout);
        }
    }

    /// Opens the places sidebar, focuses it, or closes it.
    ///
    /// All three on one key, in this order: if it isn't there, it docks to
    /// the LEFT of the layout the focused listing lives in and takes the
    /// keyboard; if it's there and the listings have the keyboard, it takes
    /// it; and only if it already had it, it closes. A second press can't
    /// close what the reader just glanced at.
    ///
    /// Opening it does NOT touch the listings: not how many there are, not
    /// which is focused, not where its cursor is.
    pub fn toggle_places(&mut self) {
        use norte_frontend::layout::{Edge, KindId, Node, Size};
        match self.places_slot() {
            // `se_ve` in the guard since #329: a panel hidden in a tab
            // doesn't close, it gets shown. Closing what the reader doesn't
            // have in front of them is the only one of the three actions
            // that can't be undone by looking.
            Some(id) if self.key_owner == KeyOwner::Places && self.is_visible(id) => {
                if let Some(new_layout) = self.layout.close_slot(id) {
                    self.layout = new_layout;
                    self.panes.refresh_visible(&self.layout);
                    self.prune_by_tree();
                }
                self.key_owner = KeyOwner::Panes;
            }
            Some(id) => {
                self.reveal(id);
                self.key_owner = KeyOwner::Places;
            }
            None => {
                let id = self.mint_slot();
                self.panes
                    .insert_places(id, norte_frontend::places::PlacesState::new());
                self.layout = self.layout.dock_grouped(
                    self.focused_slot(),
                    Edge::Left,
                    // 16 cells: the kind's minimum is 14 and a `Fixed` beats
                    // the minimum, so this number is the real width.
                    Size::Fixed(16),
                    &Node::slot(id, KindId::new("places")),
                );
                self.panes.refresh_visible(&self.layout);
                // The sidebar is born empty: favorites are its own from the
                // first frame, not from the first outside refresh, and
                // drives are requested through the flag the loop drains.
                self.sync_places_favorites();
                self.places_wants_drives = true;
                self.key_owner = KeyOwner::Places;
            }
        }
    }

    /// Moves the sidebar's cursor up, if it's open.
    pub fn places_up(&mut self) {
        if let Some(id) = self.places_slot()
            && let Some(s) = self.panes.places_mut(id)
        {
            s.up();
        }
    }

    /// Moves the sidebar's cursor down.
    pub fn places_down(&mut self) {
        if let Some(id) = self.places_slot()
            && let Some(s) = self.panes.places_mut(id)
        {
            s.down();
        }
    }

    /// A CLICK over the sidebar's `index` row (#226).
    ///
    /// The decision lives here and not in the mouse module so it can be
    /// tested without a terminal, and because it's the same one the
    /// keyboard makes with other keys: the mouse can't have its own idea of
    /// what activating a row does. Three outcomes:
    ///
    /// - a HEADER folds or unfolds its section with a single press — it's
    ///   what the arrow it already paints says;
    /// - a row that ISN'T selected gets selected, and the keyboard comes to
    ///   the sidebar: the click says "I'm interested in this", not "go
    ///   there";
    /// - the row that WAS ALREADY selected gets activated, same as
    ///   `Enter`. No time window: a double click works by being two clicks
    ///   over the same row, and whoever prefers two slow presses gets the
    ///   same result.
    pub fn places_click(&mut self, index: usize) -> PlacesClick {
        use norte_frontend::places::PlaceRow;
        let Some(id) = self.places_slot() else {
            return PlacesClick::Focused;
        };
        let was_already = self.key_owner == KeyOwner::Places
            && self.panes.places(id).is_some_and(|s| s.cursor() == index);
        let Some(s) = self.panes.places_mut(id) else {
            return PlacesClick::Focused;
        };
        if index >= s.rows().len() {
            return PlacesClick::Focused;
        }
        s.set_cursor(index);
        let is_header = matches!(s.rows().get(index), Some(PlaceRow::Header { .. }));
        self.key_owner = KeyOwner::Places;
        if is_header {
            self.places_toggle_fold();
            return PlacesClick::Folded;
        }
        if was_already {
            PlacesClick::Activate
        } else {
            PlacesClick::Focused
        }
    }

    /// Folds or unfolds the section the sidebar's cursor is on.
    pub fn places_toggle_fold(&mut self) {
        if let Some(id) = self.places_slot()
            && let Some(s) = self.panes.places_mut(id)
        {
            s.toggle_fold();
        }
        // Unfolding drives IS the moment to request them again: a disk
        // mounted or unmounted since the panel opened shows up here, and
        // with no clock in between.
        self.places_wants_drives |= self.places_drives_visible();
    }

    /// Are the sidebar's drives UNFOLDED?
    ///
    /// `false` also when there's no sidebar: the run loop asks in order to
    /// decide whether to request `host.volumes` again, and with no panel
    /// there's nobody to hand them to.
    #[must_use]
    pub fn places_drives_visible(&self) -> bool {
        self.places_slot()
            .and_then(|id| self.panes.places(id))
            .is_some_and(|s| !s.is_folded(norte_frontend::places::Section::Drives))
    }

    /// Confirms the sidebar's row: where the listing has to go.
    ///
    /// Returns the path instead of navigating because a `cd` is I/O and
    /// this is pure state; whoever has the `Backend` in front of them does
    /// it.
    ///
    /// Three outcomes and all three matter:
    ///
    /// - a row that leads somewhere: the path gets returned and the
    ///   keyboard goes back to the listings, because the sidebar is a
    ///   REMOTE and not a panel with its own directory;
    /// - a header: nothing happens here, and the keyboard stays where it
    ///   is — whoever decides what Enter does there asks
    ///   [`Self::places_cursor_on_header`] first and folds;
    /// - a broken favorite: the status bar says WHY. It's the other half of
    ///   painting it marked: fourteen cells fit the warning, not the
    ///   explanation.
    pub fn places_activate(&mut self) -> Option<VPath> {
        use norte_frontend::places::PlaceRow;
        let id = self.places_slot()?;
        let state = self.panes.places(id)?;
        if let Some(PlaceRow::Favorite {
            target: Err(key), ..
        }) = state.rows().get(state.cursor())
        {
            let reason = t(key);
            self.message = Some(reason);
            return None;
        }
        let dest = state.activate()?.clone();
        self.key_owner = KeyOwner::Panes;
        Some(dest)
    }

    /// Whether the sidebar's cursor is over a section HEADER.
    ///
    /// Asked by whoever decides what Enter does: over a header it folds,
    /// over a drive or a favorite it navigates. Without this question,
    /// Enter over "Drives" was inert —
    /// [`Self::places_activate`] returns `None` there — and folding was
    /// Space and only Space.
    #[must_use]
    pub fn places_cursor_on_header(&self) -> bool {
        use norte_frontend::places::PlaceRow;
        self.places_slot()
            .and_then(|id| self.panes.places(id))
            .is_some_and(|s| matches!(s.rows().get(s.cursor()), Some(PlaceRow::Header { .. })))
    }

    /// The docked viewer's slot, if it's in the tree.
    #[must_use]
    pub fn preview_slot(&self) -> Option<norte_frontend::layout::SlotId> {
        self.slot_of_kind(crate::preview::KIND)
    }

    /// Opens the docked viewer, focuses it, or closes it.
    ///
    /// Opens WITHOUT taking the keyboard, unlike [`Self::toggle_places`],
    /// and the difference isn't a whim: the sidebar opens to choose
    /// something in it, and the preview opens to keep looking at the
    /// listing. With the keyboard inside, the arrows would stop moving the
    /// cursor — the same cursor the panel follows — so opening it would
    /// turn off the only thing it does. Piloting the TUI in tmux showed it
    /// on the first press.
    ///
    /// The sequence is open → focus (for `viewer.*`: hex, encoding,
    /// scrolling) → close.
    ///
    /// Docks to the RIGHT, weighted, and with `follows: Role(Active)`: it
    /// isn't a new kind, it's the usual `viewer` with a binding set. The
    /// kind says what's inside and the binding says whose view it is
    /// (ADR 0058), so a pinned viewer and one that follows the cursor are
    /// the SAME renderer.
    pub fn toggle_preview(&mut self) {
        use norte_frontend::layout::{Bindings, Edge, Follow, KindId, Node, RoleId, Size};
        match self.preview_slot() {
            Some(id) if self.key_owner == KeyOwner::Preview && self.is_visible(id) => {
                if let Some(new_layout) = self.layout.close_slot(id) {
                    self.layout = new_layout;
                    self.panes.refresh_visible(&self.layout);
                    self.prune_by_tree();
                }
                self.key_owner = KeyOwner::Panes;
            }
            // Bringing it into view does NOT take the keyboard, and here's
            // the difference with the other five (#329): to the reader,
            // revealing a hidden viewer IS opening it, and this toggle
            // opens without grabbing the keys for what the paragraph above
            // says — with the keyboard inside, the arrows stop moving the
            // cursor the panel follows. It's still three presses from
            // hidden: show, focus, close; the same as from closed.
            Some(id) if !self.is_visible(id) => self.reveal(id),
            Some(_) => self.key_owner = KeyOwner::Preview,
            None => {
                let id = self.mint_slot();
                self.panes
                    .insert_preview(id, crate::preview::Preview::new());
                self.layout = self.layout.dock_grouped(
                    self.focused_slot(),
                    Edge::Right,
                    Size::Weight(1),
                    &Node::slot_bound(
                        id,
                        KindId::new(crate::preview::KIND),
                        Bindings {
                            follows: Some(Follow::Role(RoleId::Active)),
                        },
                    ),
                );
                self.panes.refresh_visible(&self.layout);
            }
        }
    }

    /// The tree's slot, if it's open.
    #[must_use]
    pub fn tree_slot(&self) -> Option<norte_frontend::layout::SlotId> {
        self.slot_of_kind(crate::tree::KIND)
    }

    /// Opens the directory tree, focuses it, or closes it (#136).
    ///
    /// Three states like the sidebar and the processes panel: a tree opens
    /// to MOVE around it, so taking the keyboard on open is what's
    /// expected.
    ///
    /// Anchored at the focused listing's directory. A tree that always hung
    /// off the system's root would show ten thousand branches to reach
    /// where you already are.
    pub fn toggle_tree(&mut self) {
        use norte_frontend::layout::{Edge, KindId, Node, Size};
        match self.tree_slot() {
            Some(id) if self.key_owner == KeyOwner::Tree && self.is_visible(id) => {
                if let Some(new_layout) = self.layout.close_slot(id) {
                    self.layout = new_layout;
                    self.panes.refresh_visible(&self.layout);
                    self.prune_by_tree();
                }
                self.key_owner = KeyOwner::Panes;
            }
            Some(id) => {
                // Re-anchor on opening it again: the listing may be
                // somewhere else since last time.
                let dir = self.focused().dir().clone();
                if let Some(t) = self.panes.tree_mut(id) {
                    t.anchor_near(&dir, &norte_frontend::shell::home_vpath());
                }
                self.reveal(id);
                self.key_owner = KeyOwner::Tree;
            }
            None => {
                let id = self.mint_slot();
                let mut tree = crate::tree::Tree::default();
                // Near the listing and not ON it (2026-09-21 capture).
                tree.anchor_near(self.focused().dir(), &norte_frontend::shell::home_vpath());
                self.panes.insert_tree(id, tree);
                self.layout = self.layout.dock_grouped(
                    self.focused_slot(),
                    Edge::Left,
                    // On the left and with the sidebar's width: it's the
                    // same gesture — a navigation column next to the
                    // listing — and two different widths for the same
                    // thing get noticed.
                    Size::Fixed(24),
                    &Node::slot(id, KindId::new(crate::tree::KIND)),
                );
                self.panes.refresh_visible(&self.layout);
                self.key_owner = KeyOwner::Tree;
            }
        }
    }

    /// The tree follows the FOCUSED listing: reveals its directory and
    /// keeps whatever was open ([`norte_frontend::tree::Tree::follow`]).
    ///
    /// Called from the two spots where "where the panel looks" changes —
    /// [`crate::navigate::settle_cd`], every `cd`'s funnel, and focus
    /// landing — and not from each gesture that triggers one: the list of
    /// gestures that navigate already fell short once, and that's where
    /// `settle_cd` itself came from.
    ///
    /// The branches needed are requested by the loop alone
    /// ([`norte_frontend::tree::Tree::wants`], one per turn), so there's no
    /// I/O here.
    pub fn follow_tree(&mut self) {
        let dir = self.focused().dir().clone();
        if let Some(t) = self.tree_mut() {
            t.follow(&dir);
        }
    }

    /// The open tree, to mutate it.
    pub fn tree_mut(&mut self) -> Option<&mut crate::tree::Tree> {
        let id = self.tree_slot()?;
        self.panes.tree_mut(id)
    }

    /// The open tree.
    #[must_use]
    pub fn tree(&self) -> Option<&crate::tree::Tree> {
        let id = self.tree_slot()?;
        self.panes.tree(id)
    }

    /// A CLICK over the tree's `index` row (#136).
    ///
    /// The decision lives here and not in the mouse module, for the same
    /// reason as the sidebar's: it's tested without a terminal, and it's
    /// the same one the keyboard makes with other keys — the mouse can't
    /// have its own idea of what activating a row does. Three outcomes:
    ///
    /// - over the MARK, the branch folds or unfolds with a single press:
    ///   it's what the arrow it already paints says, and it's the one
    ///   thing the mouse couldn't do any other way — `Enter` unfolds and
    ///   navigates, never folds;
    /// - a row that ISN'T selected gets selected, and the keyboard comes to
    ///   the tree: the click says "I'm interested in this", not "go
    ///   there";
    /// - the row that WAS ALREADY selected gets activated, same as
    ///   `Enter`. No time window, same as the sidebar.
    pub fn tree_click(&mut self, index: usize, spot: TreeSpot) -> TreeClick {
        let Some(id) = self.tree_slot() else {
            return TreeClick::Focused;
        };
        let was_already = self.key_owner == KeyOwner::Tree
            && self.panes.tree(id).is_some_and(|t| t.cursor() == index);
        let Some(t) = self.panes.tree_mut(id) else {
            return TreeClick::Focused;
        };
        if index >= t.rows().len() {
            return TreeClick::Focused;
        }
        t.set_cursor(index);
        self.key_owner = KeyOwner::Tree;
        if spot == TreeSpot::Mark {
            if let Some(t) = self.panes.tree_mut(id) {
                t.toggle();
            }
            return TreeClick::Focused;
        }
        if was_already {
            TreeClick::Activate
        } else {
            TreeClick::Focused
        }
    }

    /// The directory activating a tree row sends the listing to.
    ///
    /// Unfolds AND returns the destination, which is what `Enter` does
    /// inside the tree: whoever activates it wants to see what's inside,
    /// and seeing it in the listing is the complete answer.
    pub fn tree_activate(&mut self) -> Option<VPath> {
        let t = self.tree_mut()?;
        t.expand();
        t.selected()
    }

    /// The disk map's slot, if it's open.
    #[must_use]
    pub fn disk_map_slot(&self) -> Option<norte_frontend::layout::SlotId> {
        self.slot_of_kind(crate::diskmap::KIND)
    }

    /// The timeline's slot, if it's open (phase 7).
    #[must_use]
    pub fn timeline_slot(&self) -> Option<norte_frontend::layout::SlotId> {
        self.slot_of_kind(crate::timeline::KIND)
    }

    /// Opens the timeline, focuses it, or closes it.
    ///
    /// Three states, like the map: it opens to WALK through it — choosing
    /// how far back to go — so taking the keyboard on open is what's
    /// expected.
    ///
    /// **What's inside isn't requested here.** Opening is placing the slot;
    /// reading the journal belongs to the loop, which is what has the
    /// backend. Separating it keeps opening a panel from firing off a read
    /// from a spot that can't wait for it.
    pub fn toggle_timeline(&mut self) {
        match self.timeline_slot() {
            Some(id) if self.key_owner == KeyOwner::Timeline && self.is_visible(id) => {
                self.close_timeline();
            }
            Some(id) => {
                self.reveal(id);
                self.key_owner = KeyOwner::Timeline;
            }
            None => self.open_timeline(),
        }
    }

    /// Places the timeline if it wasn't there, and reveals it if it was
    /// hidden.
    pub fn open_timeline(&mut self) {
        use norte_frontend::layout::{Edge, KindId, Node, Size};
        if let Some(id) = self.timeline_slot() {
            self.reveal(id);
            self.key_owner = KeyOwner::Timeline;
            return;
        }
        let id = self.mint_slot();
        self.panes
            .insert_timeline(id, norte_frontend::timeline::Timeline::default());
        self.layout = self.layout.dock_grouped(
            self.focused_slot(),
            Edge::Bottom,
            // Twelve rows: a list you pick a point from needs to see
            // several at once to compare them, and the kind's minimum is
            // four, which only shows two rows with the frame.
            Size::Fixed(12),
            &Node::slot(id, KindId::new(crate::timeline::KIND)),
        );
        self.panes.refresh_visible(&self.layout);
        self.key_owner = KeyOwner::Timeline;
    }

    /// Closes the timeline if it's open, and hands the keyboard back to the
    /// listings if it had it.
    pub fn close_timeline(&mut self) {
        let Some(id) = self.timeline_slot() else {
            return;
        };
        if let Some(new_layout) = self.layout.close_slot(id) {
            self.layout = new_layout;
            self.panes.refresh_visible(&self.layout);
            self.prune_by_tree();
        }
        if self.key_owner == KeyOwner::Timeline {
            self.key_owner = KeyOwner::Panes;
        }
    }

    /// Opens the disk map, focuses it, or closes it.
    ///
    /// Three states like the processes panel and NOT like the docked
    /// viewer: a map opens to WALK through it — choosing a rectangle and
    /// entering it — so taking the keyboard on open is what's expected.
    ///
    /// **Which directory it describes isn't decided here.** Opening is
    /// placing the slot; pointing it at the active listing and requesting
    /// the measurement is the loop's work, which is what has the backend.
    /// Separating it keeps opening the panel from firing off a measurement
    /// from a spot that can't wait for it.
    pub fn toggle_disk_map(&mut self) {
        match self.disk_map_slot() {
            Some(id) if self.key_owner == KeyOwner::DiskMap && self.is_visible(id) => {
                self.close_disk_map();
            }
            Some(id) => {
                self.reveal(id);
                self.key_owner = KeyOwner::DiskMap;
            }
            None => self.open_disk_map(),
        }
    }

    /// Places the disk map if it wasn't there, and reveals it if it was
    /// hidden.
    pub fn open_disk_map(&mut self) {
        use norte_frontend::layout::{Edge, KindId, Node, Size};
        if let Some(id) = self.disk_map_slot() {
            self.reveal(id);
            self.key_owner = KeyOwner::DiskMap;
            return;
        }
        let id = self.mint_slot();
        self.panes
            .insert_disk_map(id, norte_frontend::diskmap::DiskMap::new());
        self.layout = self.layout.dock_grouped(
            self.focused_slot(),
            Edge::Bottom,
            // Twelve rows: a treemap needs height to lay out in strips —
            // with four it's a bar — and the kind's minimum is six. Twelve
            // lets the shape show without eating into the listing.
            Size::Fixed(12),
            &Node::slot(id, KindId::new(crate::diskmap::KIND)),
        );
        self.panes.refresh_visible(&self.layout);
        self.key_owner = KeyOwner::DiskMap;
    }

    /// Closes the disk map if it's open, and hands the keyboard back to the
    /// listings if it had it.
    pub fn close_disk_map(&mut self) {
        let Some(id) = self.disk_map_slot() else {
            return;
        };
        if let Some(new_layout) = self.layout.close_slot(id) {
            self.layout = new_layout;
            self.panes.refresh_visible(&self.layout);
            self.prune_by_tree();
        }
        if self.key_owner == KeyOwner::DiskMap {
            self.key_owner = KeyOwner::Panes;
        }
    }

    /// The processes panel's slot, if it's open.
    #[must_use]
    pub fn processes_slot(&self) -> Option<norte_frontend::layout::SlotId> {
        self.slot_of_kind(crate::processes::KIND)
    }

    /// Opens the processes panel, focuses it, or closes it.
    ///
    /// Three states like the sidebar and NOT like the docked viewer: a
    /// processes panel opens to look AND to cancel something specific, so
    /// taking the keyboard on open is what's expected. (The preview does
    /// the opposite because it opens to keep navigating; L3 learned the
    /// distinction by piloting the TUI in tmux.)
    ///
    /// The `tasks` strip isn't touched: it's still there, and it's still
    /// what `orthodox` brings. This panel is what opens to ACT on a task.
    pub fn toggle_processes(&mut self) {
        match self.processes_slot() {
            Some(id) if self.key_owner == KeyOwner::Processes && self.is_visible(id) => {
                self.close_processes();
            }
            Some(id) => {
                self.reveal(id);
                self.key_owner = KeyOwner::Processes;
            }
            None => self.open_processes(true),
        }
    }

    /// Opens the processes panel if it wasn't there, and reveals it if it
    /// was hidden.
    ///
    /// HALF of [`Self::toggle_processes`], separated because the automatic
    /// one (`[ui] processes_panel = "auto"`, spec 2026-09-15) needs to open
    /// without toggling: reusing the switch would close the panel right as
    /// the second task starts.
    ///
    /// `with_keyboard` is what tells the two doors apart: whoever opens it
    /// with the key is about to act on a task, and whoever opens it because
    /// a copy just started is looking at their listing — stealing the
    /// keyboard there would be taking the arrows away mid-sentence.
    pub fn open_processes(&mut self, with_keyboard: bool) {
        use norte_frontend::layout::{Edge, KindId, Node, Size};
        if let Some(id) = self.processes_slot() {
            self.reveal(id);
            if with_keyboard {
                self.key_owner = KeyOwner::Processes;
            }
            return;
        }
        let id = self.mint_slot();
        self.panes
            .insert_processes(id, crate::processes::Processes::default());
        self.layout = self.layout.dock_grouped(
            self.focused_slot(),
            Edge::Bottom,
            // Eight rows: six of tasks — the `TaskBoard`'s cap — plus the
            // frame. `Auto` belongs to the strip, which is zero at rest; a
            // panel opened by hand doesn't disappear.
            Size::Fixed(8),
            &Node::slot(id, KindId::new(crate::processes::KIND)),
        );
        self.panes.refresh_visible(&self.layout);
        if with_keyboard {
            self.key_owner = KeyOwner::Processes;
        }
    }

    /// Closes the processes panel if it's open, and hands the keyboard back
    /// to the listings if it had it.
    pub fn close_processes(&mut self) {
        let Some(id) = self.processes_slot() else {
            return;
        };
        if let Some(new_layout) = self.layout.close_slot(id) {
            self.layout = new_layout;
            self.panes.refresh_visible(&self.layout);
            self.prune_by_tree();
        }
        if self.key_owner == KeyOwner::Processes {
            self.key_owner = KeyOwner::Panes;
        }
    }

    /// Drops what belonged to slots the tree no longer has.
    ///
    /// The histories and — since phase 3 — what a plugin panel keeps
    /// alive: its last frame and the guest's OPAQUE STATE. Together in one
    /// function because they're the same rule, and because having it
    /// written fifteen times was how the next `BySlot` tenant forgot it.
    ///
    /// Pruning the state matters more than the frame: a preset's slot ids
    /// are small and fixed, so switching layouts can put ANOTHER plugin's
    /// panel on the same `SlotId`. Without this, the second one received
    /// the first one's opaque blob — which means nothing to norte, but does
    /// to a guest that recognizes its own format.
    pub(crate) fn prune_by_tree(&mut self) {
        self.history.retain_tree(&self.layout);
        self.panels.retain_tree(&self.layout);
        // And the terminal panel's shell goes with its slot (#362).
        //
        // Without this, closing the slot removed the node and left the
        // shell ALIVE with its reader thread, its pty and its directory
        // open — a busy mount stayed busy — with no panel to see it in and
        // no way back to it. `App::terminal`'s documentation already
        // promised the opposite.
        //
        // It checks whether the slot EXISTS, not whether it's visible:
        // behind a tab the panel is still there and its shell has to keep
        // running, which is half the point of having a shell inside the
        // manager.
        if self.terminal.is_some() && self.terminal_slot().is_none() {
            self.terminal = None;
        }
    }

    /// The visible PLUGIN panel's slot, the reader sees, if there's one.
    ///
    /// By PREFIX and not by equality, unlike its siblings: a plugin panel's
    /// kind is `plugin:<id>:<kind>` and isn't known at compile time. With
    /// `multi: false` in its declaration there's at most one visible, which
    /// is what lets [`crate::app::KeyOwner::Panel`] not have to say which
    /// one it is.
    ///
    /// VISIBLE and not "exists": a panel hidden behind a tab has a keyboard
    /// just as useless as a closed one (#329).
    #[must_use]
    pub fn panel_slot(&self) -> Option<norte_frontend::layout::SlotId> {
        let visible = self.layout.visible_slot_ids();
        let is_panel = |id: norte_frontend::layout::SlotId| {
            self.layout
                .kind_of(id)
                .is_some_and(|k| k.as_str().starts_with("plugin:"))
        };
        // The one the reader pointed at, as long as it's still visible and
        // still a panel: nobody enforces `multi: false`, so "the panel"
        // can't be "the first one" when there are two.
        self.panel_focus
            .filter(|id| visible.contains(id) && is_panel(*id))
            .or_else(|| visible.into_iter().find(|id| is_panel(*id)))
    }

    /// The visible plugin panel's kind, if there is one.
    #[must_use]
    pub fn panel_kind(&self) -> Option<&str> {
        let id = self.panel_slot()?;
        self.layout
            .kind_of(id)
            .map(norte_frontend::layout::KindId::as_str)
    }

    /// The log panel's slot, whether it's on screen or not.
    #[must_use]
    pub fn log_slot(&self) -> Option<norte_frontend::layout::SlotId> {
        self.slot_of_kind(crate::logview::KIND)
    }

    /// Hands the keyboard back to the listings.
    ///
    /// Exists because the field is private outside this module and there
    /// are two spots outside it that need it: the terminal's key arm and
    /// the pre-frame sweep, for when the shell leaves with the keyboard
    /// inside.
    pub fn release_keyboard(&mut self) {
        self.key_owner = KeyOwner::Panes;
    }

    /// The terminal panel's slot, if it exists.
    #[must_use]
    pub fn terminal_slot(&self) -> Option<norte_frontend::layout::SlotId> {
        self.slot_of_kind(crate::termpanel::KIND)
    }

    /// Opens the terminal panel, or gives it the keyboard, or takes it away.
    ///
    /// **Never closes it, and that's a deliberate divergence from its
    /// neighbors.** `toggle_log` and the rest close the panel on the second
    /// touch; here the second touch hands the keyboard back to the listings
    /// and leaves the shell ALIVE, which is what `app.toggle-panels` does
    /// with the subshell. Closing it kills a process of the reader's — with
    /// whatever it had half done inside — and that can't be what the same
    /// key you enter with does. `layout.close-slot` is there to close it,
    /// and it's named for what it does.
    ///
    /// The shell starts in the focused listing's directory, same as
    /// `app.terminal`.
    pub fn toggle_terminal(&mut self) {
        use norte_frontend::layout::{Edge, KindId, Node, Size};
        // With no loose chord to pull the keyboard out, the panel opens but
        // does NOT take it: inside, every key would belong to the shell and
        // none would come back. It's the same rule the subshell applies
        // before handing over the terminal, and the same criterion: a
        // half-baked capability beats a trap.
        let can_take_keys = self.terminal_chord.is_some();
        match self.terminal_slot() {
            // Already had it: the keyboard gets handed back and the shell
            // stays.
            Some(id) if self.key_owner == KeyOwner::Terminal && self.is_visible(id) => {
                self.key_owner = KeyOwner::Panes;
            }
            Some(id) => {
                self.reveal(id);
                if can_take_keys {
                    self.key_owner = KeyOwner::Terminal;
                }
            }
            None => {
                let id = self.mint_slot();
                self.layout = self.layout.dock_grouped(
                    self.focused_slot(),
                    Edge::Bottom,
                    // Twelve rows: ten of shell plus the frame. With fewer,
                    // every order that answers something erases the
                    // previous one and what's left isn't a terminal, it's a
                    // blinking little window — the same reason the log asks
                    // for ten.
                    Size::Fixed(12),
                    &Node::slot(id, KindId::new(crate::termpanel::KIND)),
                );
                self.panes.refresh_visible(&self.layout);
                if can_take_keys {
                    self.key_owner = KeyOwner::Terminal;
                }
            }
        }
    }

    /// The log panel's slot **only if it's really on screen**.
    ///
    /// Different from [`Self::log_slot`], which says whether it EXISTS: a
    /// slot behind a tab that isn't active still exists and isn't visible.
    /// The difference matters where something COSTS — probing the daemon's
    /// log (#328) is two RPCs per second, and paying for them for a panel
    /// nobody has in front of them, for the whole session, is spending
    /// network for nothing.
    ///
    /// Born here with #328 and now delegates to `slot_of_kind_visible` — no
    /// link: it's `pub(crate)` and this is public, and rustdoc denies
    /// linking from public to private — #329 found the same question in
    /// the panel bar and in the five toggles, so the answer stopped being
    /// the log's own business.
    #[must_use]
    pub fn log_slot_visible(&self) -> Option<norte_frontend::layout::SlotId> {
        self.slot_of_kind_visible(crate::logview::KIND)
    }

    /// Opens the log panel, focuses it, or closes it (#323).
    ///
    /// Three states and with the keyboard on open, same as the processes
    /// one: it opens to READ something specific — filtering by level or by
    /// text — not incidentally while navigating.
    ///
    /// There's no per-slot state to insert: there's one log panel and its
    /// level and its filter belong to the session, not to where you put it.
    pub fn toggle_log(&mut self) {
        use norte_frontend::layout::{Edge, KindId, Node, Size};
        match self.log_slot() {
            Some(id) if self.key_owner == KeyOwner::Log && self.is_visible(id) => {
                if let Some(new_layout) = self.layout.close_slot(id) {
                    self.layout = new_layout;
                    self.panes.refresh_visible(&self.layout);
                    self.prune_by_tree();
                }
                self.key_owner = KeyOwner::Panes;
                // Closing LOWERS the ring's level to whatever was being
                // shown. It's the only way back: raising it never lowers
                // it — so going to DEBUG and back doesn't erase what's in
                // between — and without this a single `t` press left the
                // process capturing TRACE for the rest of the session, with
                // its cost, long after nobody was watching. Closing the
                // panel says "that's enough".
                if let Some(ring) = self.log_ring.as_ref() {
                    ring.set_level(self.log_panel.level());
                }
                self.log_filter_input = None;
                // And the daemon's gets released (#328): its lines and its
                // cursor belong to THIS opening, and an answer that arrives
                // late can't land in the next one. What does NOT get
                // forgotten is whether its log serves — it's a fact about
                // the daemon, not about the panel — nor is its level: nobody
                // lowers it.
                //
                // The daemon's ring doesn't get lowered on close, unlike the
                // local one: it's global to all its clients, and lowering it
                // from here would turn off another frontend's capture while
                // it's watching.
                self.log_remote.restart();
            }
            Some(id) => {
                self.reveal(id);
                self.key_owner = KeyOwner::Log;
            }
            None => {
                let id = self.mint_slot();
                self.layout = self.layout.dock_grouped(
                    self.focused_slot(),
                    Edge::Bottom,
                    // Ten rows: eight of messages plus the frame. A
                    // four-line log forces scrolling to read a sentence
                    // that takes two, and then it doesn't get used.
                    Size::Fixed(10),
                    &Node::slot(id, KindId::new(crate::logview::KIND)),
                );
                self.panes.refresh_visible(&self.layout);
                self.key_owner = KeyOwner::Log;
            }
        }
    }

    /// Moves the processes panel's cursor up. No-op if it isn't open.
    pub fn processes_up(&mut self) {
        let ids = self.board.task_ids();
        if let Some(id) = self.processes_slot()
            && let Some(p) = self.panes.processes_mut(id)
        {
            p.up(&ids);
        }
    }

    /// Moves the processes panel's cursor down, without going past the
    /// last row.
    pub fn processes_down(&mut self) {
        let ids = self.board.task_ids();
        if let Some(id) = self.processes_slot()
            && let Some(p) = self.panes.processes_mut(id)
        {
            p.down(&ids);
        }
    }

    /// Cancels the task under the panel's cursor. `false` if there's no
    /// panel, no rows, or that one already finished.
    ///
    /// It's what the CHANGELOG and the two help topics had been promising
    /// since phase A — "cancels the one under the cursor" — with no key
    /// ever reaching the panel: `KeyOwner` was being set and nobody read
    /// it, so the arrows moved the LISTING behind it and F8 opened the
    /// delete dialog over its selection (#243).
    /// The task pointed at in the processes panel — or the most recent one
    /// if that panel isn't open — to move it in the queue (ADR 0149).
    #[must_use]
    pub fn processes_selected(&self) -> Option<norte_core::backend::TaskObserver> {
        let ids = self.board.task_ids();
        let row = self
            .processes_slot()
            .and_then(|id| self.panes.processes(id))
            .and_then(|p| p.row(&ids))
            .unwrap_or_else(|| ids.len().saturating_sub(1));
        self.board.task_at(row)
    }

    /// Cancels the task pointed at in the processes panel. `false` if
    /// there's no panel, no row pointed at, or that task already finished.
    pub fn processes_cancel(&mut self) -> bool {
        let Some(id) = self.processes_slot() else {
            return false;
        };
        let ids = self.board.task_ids();
        let Some(cursor) = self.panes.processes(id).and_then(|p| p.row(&ids)) else {
            return false;
        };
        self.board.cancel_at(cursor)
    }

    /// Dispatches ONE keymap command over the processes panel.
    ///
    /// Lives here and not in the binary so a test can feed it a real key —
    /// preset → `Effective` → `Resolver` → command — and see what the panel
    /// does. The tests that existed asserted `key_owner()`, which is
    /// exactly what left invisible that no key ever arrived (#243).
    ///
    /// Returns the message for the status bar, if the command leaves one.
    pub fn processes_command(&mut self, cmd: &str) -> Option<String> {
        if !ALLOW_PROCESSES.contains(&cmd) {
            return None; // outside this panel's allowlist: inert
        }
        // The application's chrome, before this panel's own: same funnel
        // as the sidebar and the tree.
        if self.panel_chrome_command(cmd) {
            return None;
        }
        match cmd {
            "dialog.up" => self.processes_up(),
            "dialog.down" => self.processes_down(),
            // `Esc` releases the keyboard and does NOT close the panel:
            // closing it is `layout.processes`. And `Tab` does the same,
            // for the same reason as the sidebar and the tree: opening a
            // panel with the keyboard can't cost you the key you've always
            // switched panels with.
            "dialog.cancel" | "dialog.pane" | "pane.switch" => self.return_keys_to_panes(),
            // The ring moves to the panel NEXT DOOR, and that's why it
            // isn't the same as `Tab`: you have to be able to walk the
            // screen from inside any panel, not only by going back to the
            // listings first.
            "layout.focus-next" => self.layout_focus(1),
            "layout.focus-prev" => self.layout_focus(-1),
            "layout.grow" => self.layout_resize(1),
            "layout.shrink" => self.layout_resize(-1),
            "layout.processes" => self.toggle_processes(),
            // And the other panels': the sidebar leaves the drives
            // requested and the loop serves them, so opening it from here
            // needs no backend.
            "layout.places" => self.toggle_places(),
            "layout.preview" => self.toggle_preview(),
            "layout.metadata" => self.toggle_metadata(),
            "pane.tree" => self.toggle_tree(),
            "dialog.confirm" => {
                return Some(if self.processes_cancel() {
                    t("msg-cancelling")
                } else {
                    t("msg-no-tasks")
                });
            }
            _ => {}
        }
        None
    }

    /// Dispatches a keymap command with the keyboard in the log panel
    /// (#323), filtered by [`ALLOW_LOG`].
    ///
    /// Its own funnel and not the processes one's: that one lets
    /// `dialog.confirm` through, which there CANCELS the task under the
    /// cursor. An `Enter` in a log viewer that cancels a copy is exactly
    /// what an allowlist exists to prevent.
    pub fn log_command(&mut self, cmd: &str) {
        if !ALLOW_LOG.contains(&cmd) {
            return; // outside this panel's allowlist: inert
        }
        if self.panel_chrome_command(cmd) {
            return;
        }
        match cmd {
            "dialog.pane" | "pane.switch" => self.return_keys_to_panes(),
            "layout.focus-next" => self.layout_focus(1),
            "layout.focus-prev" => self.layout_focus(-1),
            "layout.grow" => self.layout_resize(1),
            "layout.shrink" => self.layout_resize(-1),
            "layout.log" => self.toggle_log(),
            "layout.disk-map" => self.toggle_disk_map(),
            "layout.processes" => self.toggle_processes(),
            "layout.places" => self.toggle_places(),
            "layout.preview" => self.toggle_preview(),
            "layout.metadata" => self.toggle_metadata(),
            "pane.tree" => self.toggle_tree(),
            _ => {}
        }
    }

    /// Dispatches a keymap command with the keyboard in a plugin panel
    /// (phase 4), filtered by [`ALLOW_DISK_MAP`].
    ///
    /// Its own funnel and not its neighbors': here `dialog.confirm` is NOT
    /// on the list because `Enter` belongs to the panel — it enters the
    /// chosen child — and [`crate::diskmap::key`] claims it before the
    /// keymap. Letting it through would be an `Enter` with two meanings
    /// depending on who looked first.
    pub fn disk_map_command(&mut self, cmd: &str) {
        if !ALLOW_DISK_MAP.contains(&cmd) {
            return; // outside this panel's allowlist: inert
        }
        if self.panel_chrome_command(cmd) {
            return;
        }
        match cmd {
            "dialog.pane" | "pane.switch" => self.return_keys_to_panes(),
            "layout.focus-next" => self.layout_focus(1),
            "layout.focus-prev" => self.layout_focus(-1),
            "layout.grow" => self.layout_resize(1),
            "layout.shrink" => self.layout_resize(-1),
            "layout.disk-map" => self.toggle_disk_map(),
            "layout.log" => self.toggle_log(),
            "layout.processes" => self.toggle_processes(),
            "layout.places" => self.toggle_places(),
            "layout.preview" => self.toggle_preview(),
            "layout.metadata" => self.toggle_metadata(),
            "pane.tree" => self.toggle_tree(),
            _ => {}
        }
    }

    /// Dispatches a keymap command with the keyboard in a plugin panel
    /// (phase 3), filtered by [`ALLOW_PANEL`].
    ///
    /// Its own funnel and empty of content: the only thing a contributed
    /// panel understands today is the chrome. When T4 gives it rendering
    /// and the guest receives `panel-event::command`, this is where it
    /// comes IN — and it'll still be filtered, which is what keeps a
    /// plugin from grabbing `F8`.
    pub fn panel_command(&mut self, cmd: &str) {
        if !ALLOW_PANEL.contains(&cmd) {
            return; // outside this panel's allowlist: inert
        }
        if self.panel_chrome_command(cmd) {
            return;
        }
        match cmd {
            "dialog.cancel" | "dialog.pane" | "pane.switch" => self.return_keys_to_panes(),
            "layout.focus-next" => self.layout_focus(1),
            "layout.focus-prev" => self.layout_focus(-1),
            "layout.grow" => self.layout_resize(1),
            "layout.shrink" => self.layout_resize(-1),
            "layout.log" => self.toggle_log(),
            "layout.disk-map" => self.toggle_disk_map(),
            "layout.processes" => self.toggle_processes(),
            "layout.places" => self.toggle_places(),
            "layout.preview" => self.toggle_preview(),
            "layout.metadata" => self.toggle_metadata(),
            "pane.tree" => self.toggle_tree(),
            _ => {}
        }
    }

    /// The attribute sheet's slot, if it's open.
    #[must_use]
    pub fn metadata_slot(&self) -> Option<norte_frontend::layout::SlotId> {
        self.slot_of_kind(crate::metadata::KIND)
    }

    /// Opens the attribute sheet, or closes it.
    ///
    /// TWO states and not three, unlike the sidebar and the processes
    /// panel: the sheet follows the listing's cursor, so taking the
    /// keyboard would turn off the only thing it does. It used to have a
    /// `KeyOwner` of its own that got set on the second press and nobody
    /// consumed: the sheet grabbed the focus border, the arrows kept
    /// moving the listing next to it, and the third press was the only one
    /// that closed it (#243).
    /// Docks to the RIGHT with `follows: Role(Active)`.
    pub fn toggle_metadata(&mut self) {
        use norte_frontend::layout::{Bindings, Edge, Follow, KindId, Node, RoleId, Size};
        if let Some(id) = self.metadata_slot() {
            // #329: if it's hidden behind a tab, this key SHOWS it. The
            // attribute sheet doesn't take the keyboard — it's looked at,
            // not walked through — so it has two states, and what's
            // missing when it isn't visible isn't "close" but "bring it".
            if !self.is_visible(id) {
                self.reveal(id);
            } else if let Some(new_layout) = self.layout.close_slot(id) {
                self.layout = new_layout;
                self.panes.refresh_visible(&self.layout);
                self.prune_by_tree();
            }
        } else {
            let id = self.mint_slot();
            self.panes.insert_metadata(id, None);
            self.layout = self.layout.dock_grouped(
                self.focused_slot(),
                Edge::Right,
                // Thirty cells: the longest label plus its value next to
                // it. Fixed and not weighted because an attribute sheet
                // gains nothing from half the screen.
                Size::Fixed(30),
                &Node::slot_bound(
                    id,
                    KindId::new(crate::metadata::KIND),
                    Bindings {
                        follows: Some(Follow::Role(RoleId::Active)),
                    },
                ),
            );
            self.panes.refresh_visible(&self.layout);
        }
    }

    /// The preview couldn't read: the reason gets painted INSIDE the slot.
    ///
    /// And nothing gets asked. The preview follows the cursor, so a policy
    /// denial can't open a dialog: scrolling down a directory would be a
    /// burst of modals, and the reader hasn't asked to open anything.
    pub fn preview_failed(&mut self, slot: norte_frontend::layout::SlotId, key: &str) {
        let text = t(key);
        if let Some(p) = self.panes.preview_mut(slot) {
            p.say(None, text);
        }
    }

    /// Closes the focused panel.
    ///
    /// REFUSES to close the last `browser`: a screen with no listing at all
    /// isn't a layout, it's a hang with borders. It's also the invariant
    /// that keeps the two sides distinct — with a single listing, "the
    /// other pane" would be this very one and a copy would target its own
    /// source.
    ///
    /// Returns `false` if it couldn't, so the caller can warn.
    pub fn layout_close_slot(&mut self) -> bool {
        if self.browsers_in_tree() <= 2 {
            return false;
        }
        let focus = self.focused_slot();
        let Some(new_layout) = self.layout.close_slot(focus) else {
            return false;
        };
        self.layout = new_layout;
        self.panes.refresh_visible(&self.layout);
        self.prune_by_tree();
        true
    }

    /// The slot `layout.grow`/`layout.shrink` point at: the one that has
    /// the KEYBOARD, not the focused listing.
    ///
    /// `focused_slot()` is always a visible listing — `layout_focus` cycles
    /// over `panes.len()` — so with it `Node::resize`'s `Size::Fixed` arm
    /// was unreachable by any production path: the side bar kept the width
    /// it opened with and the CHANGELOG announced the opposite (#244 M1).
    /// The tests passed because they called `resize` with the sidebar's id
    /// by hand, an argument the real caller had no way to produce.
    #[must_use]
    fn resize_target(&self) -> norte_frontend::layout::SlotId {
        match self.key_owner {
            KeyOwner::Places => self.places_slot(),
            KeyOwner::Tree => self.tree_slot(),
            KeyOwner::Processes => self.processes_slot(),
            KeyOwner::Log => self.log_slot(),
            KeyOwner::DiskMap => self.disk_map_slot(),
            KeyOwner::Timeline => self.timeline_slot(),
            // The terminal grows like any other: twelve rows is the
            // start, and a `make` inside asks for more.
            KeyOwner::Terminal => self.terminal_slot(),
            // A plugin panel grows like any other side one: the key is
            // the same and the layout says which slot.
            KeyOwner::Panel => self.panel_slot(),
            KeyOwner::Panes | KeyOwner::Preview => None,
        }
        .unwrap_or_else(|| self.focused_slot())
    }

    /// Grows (`delta > 0`) or shrinks the panel that has the keyboard.
    pub fn layout_resize(&mut self, delta: i16) {
        let target = self.resize_target();
        self.layout = self.layout.resize(target, delta);
    }

    /// Gives the focused panel's siblings the same size.
    pub fn layout_equalize(&mut self) {
        let focus = self.focused_slot();
        self.layout = self.layout.equalize(focus);
    }

    /// Flips the layout of the panel that has the keyboard: side by side
    /// becomes one over the other (ADR 0138). A layout with chrome doesn't
    /// flip.
    pub fn layout_flip(&mut self) {
        let target = self.resize_target();
        let new_layout = self.layout.flip(target);
        self.change_layout(new_layout, None);
    }

    /// Moves slot `id` next to `target` (ADR 0138): what dropping a panel
    /// dragged by its title does.
    pub fn layout_move(
        &mut self,
        id: norte_frontend::layout::SlotId,
        target: norte_frontend::layout::SlotId,
        zone: norte_frontend::layout::DropZone,
    ) {
        let new_layout = self.layout.move_slot(id, target, zone);
        // In the center, the destination goes behind a tab on purpose.
        let tolerated = (zone == norte_frontend::layout::DropZone::Center).then_some(target);
        self.change_layout(new_layout, tolerated);
    }

    /// Keeps `new_layout` if it leaves visible what was visible
    /// (`keeps_on_screen`, over the LAST frame), and brings the panes,
    /// their histories and the keyboard's owner up to date, with focus on
    /// the SAME slot. If it doesn't fit, nothing is touched and it says so
    /// on the status bar, like splitting.
    fn change_layout(
        &mut self,
        new_layout: norte_frontend::layout::Node,
        tolerated: Option<norte_frontend::layout::SlotId>,
    ) {
        if new_layout == self.layout {
            return;
        }
        let old_layout = std::mem::replace(&mut self.layout, new_layout);
        if let Some(area) = self.last_frame {
            let after = crate::ui::resolved_for(self, area);
            let current = std::mem::replace(&mut self.layout, old_layout);
            let before = crate::ui::resolved_for(self, area);
            if !norte_frontend::layout::keeps_on_screen(&before, &after, &current, tolerated) {
                self.message = Some(t("msg-layout-move-no-room"));
                return;
            }
            self.layout = current;
        }
        // Focus stays on its SLOT: a slot's position changes when it's
        // moved, and an old index would name the panel next to it.
        let focused = self.focused_slot();
        self.panes.refresh_visible(&self.layout);
        self.prune_by_tree();
        self.settle_key_owner();
        if let Some(i) = (0..self.panes.len()).find(|i| self.panes.slot_of(*i) == focused) {
            self.set_focus(i);
        }
    }

    /// Designates the OTHER visible side as the operations' destination.
    ///
    /// With two panels the destination is already the other one and this
    /// changes nothing; it exists for the day there are more than two and
    /// the engine can no longer break the tie on its own (ADR 0058 D7).
    pub fn layout_set_target(&mut self) {
        let n = self.panes.len();
        if n < 2 {
            return;
        }
        let current = self
            .roles
            .get(norte_frontend::layout::RoleId::Target)
            .and_then(|t| (0..n).find(|i| self.panes.slot_of(*i) == t))
            .unwrap_or(self.focus);
        // The next one that isn't the focused one: designating yourself as
        // the destination is asking a copy to copy onto itself.
        let mut i = (current + 1) % n;
        if i == self.focus {
            i = (i + 1) % n;
        }
        let slot = self.panes.slot_of(i);
        self.roles.set(norte_frontend::layout::RoleId::Target, slot);
    }

    /// The DESTINATION panel's position, if there is one.
    ///
    /// With two panels it's the other one and nobody had to say so. With
    /// three or more it has to have been designated: guessing here is how a
    /// copy ends up heading to a panel the reader didn't have in mind,
    /// which is silent data loss (ADR 0058 D7).
    #[must_use]
    pub fn target_index(&self) -> Option<usize> {
        let n = self.panes.len();
        if let Some(t) = self.roles.get(norte_frontend::layout::RoleId::Target)
            && let Some(i) = (0..n).find(|i| self.panes.slot_of(*i) == t)
            && i != self.focus
        {
            return Some(i);
        }
        (n == 2).then_some(self.focus ^ 1)
    }

    /// How many times [`Self::swap_panes`] has run.
    ///
    /// Only useful as an equality check against a previously read value: any
    /// difference means the two sides changed places, so anything holding a
    /// pane INDEX from before now names the other side's content.
    #[must_use]
    pub const fn swap_seq(&self) -> u64 {
        self.swap_seq
    }

    /// Gives focus to pane `i`. An index outside `0|1` gets IGNORED (`focus`'s
    /// invariant belongs to `App` itself): the only emitter of indices that
    /// aren't literals is the mouse's hit test, and there an impossible
    /// index is our own bug, not something that should leave focus pointing
    /// at a pane that doesn't exist.
    pub fn set_focus(&mut self, i: usize) {
        debug_assert!(i < self.panes.len(), "pane out of range");
        if i < self.panes.len() {
            self.focus = i;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::*;

    /// A screen with plenty of room, for when what's being tested isn't the
    /// lack of room.
    const SCREEN: ratatui::layout::Rect = ratatui::layout::Rect {
        x: 0,
        y: 0,
        width: 110,
        height: 30,
    };

    /// A panel CONTRIBUTED by a plugin is a slot in full standing: it gets
    /// found, it takes the keyboard and the picker offers it (phase 3).
    ///
    /// The test walks the whole path because each stretch had its own
    /// built-in name table: `focus_stop` looked at the stock registry —
    /// where a contributed kind doesn't exist — `panel_slot` resolves by
    /// prefix because `plugin:<id>:<kind>` isn't known at compile time, and
    /// `KeyOwner::Panel` doesn't carry which one it is, which is what
    /// `multi: false` allows.
    #[test]
    fn a_plugin_panel_is_found_takes_the_keyboard_and_is_named() {
        use norte_frontend::layout::{Dir, KindId, KindRegistry, Node, Size, SlotId};

        let mut app = app_two_panes();
        app.kinds
            .insert_panels(&[plugin_with_panel("git", "status")]);
        let tree = Node::Split {
            dir: Dir::Horizontal,
            sizes: vec![Size::Weight(1), Size::Fixed(24)],
            children: vec![
                Node::slot(SlotId(70), KindId::browser()),
                Node::slot(SlotId(71), KindId::new("plugin:git:status")),
            ],
        };
        app.set_layout(tree);

        let id = app.panel_slot().expect("the contributed panel gets found");
        assert_eq!(id, SlotId(71));
        assert_eq!(app.panel_kind(), Some("plugin:git:status"));
        // And it really takes it: a slot that doesn't pass `focus_stop`
        // returns `false` here and leaves the keyboard where it was.
        assert!(app.focus_slot(id));
        assert_eq!(app.key_owner, crate::app::KeyOwner::Panel);
        assert!(
            app.kinds
                .decls()
                .iter()
                .any(|d| d.id.as_str() == "plugin:git:status"),
            "the registry declares the contributed kind"
        );
        // And it declares it with ITS OWN minimum, which is what the
        // layout consumes when placing the slot and the picker's thumbnail
        // when drawing it. With the stock registry — the one received
        // before phase 3 — that kind is unknown and evaluates to `(1, 1)`:
        // the slot got placed where it doesn't fit.
        let kid = KindId::new("plugin:git:status");
        assert_eq!(app.kinds.min_of(&kid), (20, 4));
        assert_eq!(KindRegistry::builtin().min_of(&kid), (1, 1));
    }

    /// Withdrawing a plugin's consent WITHDRAWS its panel, in the same
    /// session.
    ///
    /// Declaring additively left the kind set until the next startup: the
    /// slot kept getting placed and kept taking the keyboard for a plugin
    /// the reader had just disabled. The catalogue gets reread on
    /// approving, activating or uninstalling, so the declaration gets
    /// REBUILT whole.
    #[test]
    fn revoking_a_plugins_consent_removes_its_panel() {
        let mut app = app_two_panes();
        app.kinds
            .insert_panels(&[plugin_with_panel("git", "status")]);
        assert!(app.kinds.decls().iter().any(is_git_panel));

        let mut disabled = plugin_with_panel("git", "status");
        disabled.enabled = false;
        app.kinds.insert_panels(&[disabled]);
        assert!(
            !app.kinds.decls().iter().any(is_git_panel),
            "a panel with no consent stops existing for the layout"
        );
    }

    /// A `kind` with characters outside the alphabet doesn't get declared.
    ///
    /// The id and the kind are third-party text and end up inside a
    /// `KindId`, which validates nothing: that's where the painted name and
    /// the key saved in the session both come from.
    #[test]
    fn a_kind_with_hostile_characters_is_not_declared() {
        let mut app = app_two_panes();
        app.kinds.insert_panels(&[
            plugin_with_panel("git", "sta\ntus"),
            plugin_with_panel("git", "está"),
            plugin_with_panel("git", ""),
        ]);
        assert!(
            !app.kinds
                .decls()
                .iter()
                .any(|d| d.id.as_str().starts_with("plugin:")),
            "none of the three passes the alphabet"
        );
    }

    /// A plugin panel's key funnel lets the chrome through and NOTHING
    /// else.
    ///
    /// With no funnel, keys kept going to `browse`'s resolver and acted on
    /// the listing behind it while the focus border said the keyboard was
    /// in the panel — #243's bug.
    #[test]
    fn a_plugin_panel_only_lets_the_chrome_through() {
        use norte_frontend::layout::{Dir, KindId, Node, Size, SlotId};

        let mut app = app_two_panes();
        app.kinds
            .insert_panels(&[plugin_with_panel("git", "status")]);
        app.set_layout(Node::Split {
            dir: Dir::Horizontal,
            sizes: vec![Size::Weight(1), Size::Fixed(24)],
            children: vec![
                Node::slot(SlotId(70), KindId::browser()),
                Node::slot(SlotId(71), KindId::new("plugin:git:status")),
            ],
        });
        let id = app.panel_slot().expect("there is a panel");
        assert!(app.focus_slot(id));

        // A listing command does nothing AND doesn't hand back the
        // keyboard: the panel keeps it, which is what the reader sees.
        app.panel_command("pane.select-all");
        assert_eq!(app.key_owner, crate::app::KeyOwner::Panel);
        // And the chrome does: releasing the keyboard belongs to the panel.
        app.panel_command("dialog.cancel");
        assert_eq!(app.key_owner, crate::app::KeyOwner::Panes);
    }

    /// Is this git panel's declaration?
    fn is_git_panel(d: &norte_frontend::layout::KindDecl) -> bool {
        d.id.as_str() == "plugin:git:status"
    }

    /// An approved and active `PluginInfo` that contributes a panel.
    fn plugin_with_panel(id: &str, kind: &str) -> norte_proto::methods::PluginInfo {
        norte_proto::methods::PluginInfo {
            id: id.to_owned(),
            name: id.to_owned(),
            publisher: String::new(),
            version: "1.0.0".to_owned(),
            category: "panel".to_owned(),
            capabilities: Vec::new(),
            approved: true,
            enabled: true,
            description: None,
            commands: Vec::new(),
            columns: Vec::new(),
            panels: vec![norte_proto::methods::PluginPanelInfo {
                kind: kind.to_owned(),
                title: "Git".to_owned(),
                min_cols: None,
                min_rows: None,
            }],
            has_help: false,
            manifest_digest: None,
        }
    }

    /// A tree with the log hidden in the tab that isn't active.
    fn app_with_hidden_log() -> App {
        use norte_frontend::layout::{Dir, KindId, Node, Size, SlotId};

        let mut app = app_two_panes();
        app.set_layout(Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Fixed(10)],
            children: vec![
                Node::slot(SlotId(80), KindId::browser()),
                Node::Tabs {
                    children: vec![
                        Node::slot(SlotId(81), KindId::browser()),
                        Node::slot(SlotId(82), KindId::new(crate::logview::KIND)),
                    ],
                    active: 0,
                },
            ],
        });
        app
    }

    /// A panel hidden in a tab's button says CLOSED (#329).
    ///
    /// It used to say open, because the status bar asked if the slot
    /// EXISTS. To the reader it doesn't exist: they don't see it, and what
    /// the button promises is showing it to them.
    #[test]
    fn a_panel_hidden_in_a_tab_paints_closed() {
        use norte_frontend::panelbar::PanelState;

        let app = app_with_hidden_log();
        let button = crate::ui::panel_buttons(&app, SCREEN)
            .into_iter()
            .find(|b| b.kind == crate::logview::KIND)
            .expect("the log has a button");
        assert_eq!(button.state, PanelState::Closed);
    }

    /// A panel the layout drops for lack of room isn't open either (#331).
    ///
    /// `visible_slot_ids` answers which tab is active, not what FITS. The
    /// SAME tree, with the panel on the active tab, reads differently on
    /// two screens — and that's exactly what has to be seen, because it
    /// proves the button looks at the layout and not at the tree.
    #[test]
    fn a_panel_that_doesnt_fit_paints_closed() {
        use norte_frontend::layout::{Dir, KindId, Node, Size, SlotId};
        use norte_frontend::panelbar::PanelState;

        let mut app = app_two_panes();
        // Horizontal, because the case that DROPS a slot is the collapse:
        // two siblings competing for the same axis whose minimums don't
        // fit. A `Fixed` won't do to test this — it gets trimmed, it
        // doesn't fall.
        app.set_layout(Node::Split {
            dir: Dir::Horizontal,
            sizes: vec![Size::Weight(1), Size::Weight(1)],
            children: vec![
                Node::slot(SlotId(40), KindId::browser()),
                Node::slot(SlotId(41), KindId::new(crate::logview::KIND)),
            ],
        });

        let state = |app: &App, width: u16| {
            crate::ui::panel_buttons(
                app,
                ratatui::layout::Rect {
                    x: 0,
                    y: 0,
                    width,
                    height: 30,
                },
            )
            .into_iter()
            .find(|b| b.kind == crate::logview::KIND)
            .expect("the log has a button")
            .state
        };

        assert_eq!(state(&app, 110), PanelState::Open, "with room, open");
        assert_eq!(
            state(&app, 24),
            PanelState::Closed,
            "the layout dropped it, so the button can't say yes"
        );
    }

    /// And clicking it BRINGS IT INTO VIEW and takes the keyboard, instead
    /// of sending the keyboard to something invisible (#329).
    ///
    /// It used to fall into the "already open, focus it" branch: the keys
    /// stopped reaching what was actually visible, the status bar said
    /// "focused", and the next press closed a panel nobody had ever seen.
    #[test]
    fn pressing_a_hidden_panel_shows_it() {
        use norte_frontend::layout::SlotId;

        let mut app = app_with_hidden_log();
        app.toggle_log();
        assert!(
            app.layout.visible_slot_ids().contains(&SlotId(82)),
            "still behind the other tab"
        );
        assert_eq!(app.key_owner(), KeyOwner::Log);
        assert!(app.log_slot().is_some(), "and certainly hasn't closed it");
    }

    /// A panel that gets HIDDEN loses the keyboard, same as one that gets
    /// closed (#329).
    ///
    /// `settle_key_owner` used to ask whether the panel exists. Switching
    /// tabs closes nothing, so the log kept the keys behind another tab: it
    /// got pressed and nothing visible happened, while the status bar
    /// already painted it closed — what the reader sees and what's in
    /// charge stopped being the same. `tab_cycle` and `tab_goto` call it
    /// for that reason.
    #[test]
    fn a_panel_that_hides_releases_the_keyboard() {
        use norte_frontend::layout::SlotId;

        let mut app = app_with_hidden_log();
        app.toggle_log();
        assert_eq!(app.key_owner(), KeyOwner::Log, "visible and with the keys");

        // What any path that switches tabs does.
        app.layout = app.layout.set_active_for(SlotId(82), 0);
        assert!(!app.layout.visible_slot_ids().contains(&SlotId(82)));

        app.settle_key_owner();
        assert_eq!(app.key_owner(), KeyOwner::Panes, "it hid and kept the keys");
    }

    /// The hidden viewer gets SHOWN without taking the keyboard, unlike the
    /// other five.
    ///
    /// It isn't an arbitrary exception: this toggle opens without grabbing
    /// the keys because the viewer follows the listing's cursor, and to the
    /// reader revealing a hidden one IS opening it. With the keyboard
    /// inside, the arrows would stop moving the cursor the panel follows —
    /// meaning showing it would turn off the only thing it does.
    #[test]
    fn revealing_the_viewer_doesnt_take_the_keyboard() {
        use norte_frontend::layout::{Dir, KindId, Node, Size, SlotId};

        let mut app = app_two_panes();
        app.set_layout(Node::Split {
            dir: Dir::Horizontal,
            sizes: vec![Size::Weight(1), Size::Fixed(30)],
            children: vec![
                Node::slot(SlotId(70), KindId::browser()),
                Node::Tabs {
                    children: vec![
                        Node::slot(SlotId(71), KindId::browser()),
                        Node::slot(SlotId(72), KindId::new(crate::preview::KIND)),
                    ],
                    active: 0,
                },
            ],
        });
        app.toggle_preview();
        assert!(app.layout.visible_slot_ids().contains(&SlotId(72)));
        assert_eq!(
            app.key_owner(),
            KeyOwner::Panes,
            "showing it can't turn off the listing's arrows"
        );
        app.toggle_preview();
        assert_eq!(
            app.key_owner(),
            KeyOwner::Preview,
            "and the second focuses it"
        );
    }

    /// The attribute sheet has TWO states, so what it's missing when hidden
    /// isn't "close" but "bring it".
    #[test]
    fn the_hidden_attribute_sheet_shows_instead_of_closing() {
        use norte_frontend::layout::{Dir, KindId, Node, Size, SlotId};

        let mut app = app_two_panes();
        app.set_layout(Node::Split {
            dir: Dir::Horizontal,
            sizes: vec![Size::Weight(1), Size::Fixed(30)],
            children: vec![
                Node::slot(SlotId(60), KindId::browser()),
                Node::Tabs {
                    children: vec![
                        Node::slot(SlotId(61), KindId::browser()),
                        Node::slot(SlotId(62), KindId::new(crate::metadata::KIND)),
                    ],
                    active: 0,
                },
            ],
        });
        app.toggle_metadata();
        assert!(
            app.layout.visible_slot_ids().contains(&SlotId(62)),
            "closed it without the reader ever having seen it"
        );
        app.toggle_metadata();
        assert!(
            app.metadata_slot().is_none(),
            "and the second one does close it"
        );
    }

    /// A hidden processes panel doesn't swallow its notification mark
    /// either.
    #[test]
    fn hidden_processes_keeps_its_new_mark() {
        use norte_frontend::layout::{Dir, KindId, Node, Size, SlotId};

        let mut app = app_two_panes();
        app.set_layout(Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Fixed(8)],
            children: vec![
                Node::slot(SlotId(50), KindId::browser()),
                Node::Tabs {
                    children: vec![
                        Node::slot(SlotId(51), KindId::browser()),
                        Node::slot(SlotId(52), KindId::new(crate::processes::KIND)),
                    ],
                    active: 0,
                },
            ],
        });
        let button = crate::ui::panel_buttons(&app, SCREEN)
            .into_iter()
            .find(|b| b.kind == crate::processes::KIND)
            .expect("processes has a button");
        assert_eq!(
            button.attention,
            norte_frontend::panelbar::figure(app.board.rows().len()),
            "the mark depends on whether there are tasks, not on the slot existing"
        );
    }

    /// The second press does close it: showing and focusing is ONE step,
    /// not two.
    ///
    /// If bringing it into view cost one press and focusing it another, the
    /// panel the reader just requested would end up with no keyboard, and
    /// the three-state promise — open and take the keyboard, take the
    /// keyboard, close — would have four.
    #[test]
    fn the_second_press_closes_what_the_first_showed() {
        let mut app = app_with_hidden_log();
        app.toggle_log();
        app.toggle_log();
        assert!(app.log_slot().is_none());
        assert_eq!(app.key_owner(), KeyOwner::Panes);
    }

    /// A HIDDEN panel doesn't swallow its notification mark (#329).
    ///
    /// The mark stays quiet when the panel is open because then you're
    /// already looking at it. Hidden, you aren't, so keeping it quiet
    /// turned off the warning right when it's needed.
    #[test]
    fn a_hidden_log_keeps_its_new_mark() {
        use norte_config::logring::LogRing;
        use tracing_subscriber::layer::SubscriberExt as _;

        let mut app = app_with_hidden_log();
        let ring = LogRing::new(16);
        let s = tracing_subscriber::registry().with(norte_config::logring::ring_layer(&ring));
        tracing::subscriber::with_default(s, || {
            tracing::warn!(target: "norte_core::prueba", "something degraded");
        });
        app.log_ring = Some(ring);

        let button = crate::ui::panel_buttons(&app, SCREEN)
            .into_iter()
            .find(|b| b.kind == crate::logview::KIND)
            .expect("the log has a button");
        assert_eq!(
            button.attention, 1,
            "there's a warning and the reader doesn't have it in front of them"
        );
    }

    /// The everyday case doesn't change: a docked, visible panel focuses and
    /// closes as always.
    #[test]
    fn a_visible_panel_still_does_the_three_steps() {
        let mut app = app_two_panes();
        app.toggle_log();
        assert_eq!(
            app.key_owner(),
            KeyOwner::Log,
            "opens and takes the keyboard"
        );
        app.key_owner = KeyOwner::Panes;
        app.toggle_log();
        assert_eq!(app.key_owner(), KeyOwner::Log, "takes it back");
        app.toggle_log();
        assert!(app.log_slot().is_none(), "and only then closes");
    }

    /// #136: the tree opens anchored WHERE the listing is, not at the
    /// system's root: a tree that always hung off `/` would show ten
    /// thousand branches to reach where you already are.
    #[test]
    fn the_tree_anchors_where_the_listing_is() {
        let mut app = app_two_panes();
        let dir = app.focused().dir().clone();
        app.toggle_tree();
        assert_eq!(app.tree().and_then(|t| t.root().cloned()), Some(dir));
        assert_eq!(app.key_owner(), KeyOwner::Tree, "takes the keyboard");
    }

    /// And once open it FOLLOWS the listing: navigating inside its root
    /// moves the cursor to that branch and keeps whatever was open.
    /// Anchored and still, the panel said where you were when you opened it
    /// and nothing more.
    #[test]
    fn the_tree_follows_the_navigating_listing() {
        let mut app = app_en("mem:///r", "mem:///other");
        app.toggle_tree();
        let t = app.tree_mut().expect("tree");
        // What's looked at here is FOLLOWING, not anchoring: it starts from
        // a tree hung off the listing so the row count is the usual one
        // (opening it hangs it further up since 2026-09-21).
        t.anchor(vp("mem:///r"));
        t.insert_children(vp("mem:///r"), vec![vp("mem:///r/a"), vp("mem:///r/b")]);
        // `b` opened by hand: it's what a re-anchor would have closed.
        t.set_cursor(2);
        t.expand();
        t.insert_children(vp("mem:///r/b"), vec![vp("mem:///r/b/x")]);
        t.insert_children(vp("mem:///r/a"), vec![vp("mem:///r/a/y")]);

        *app.focused_mut() = crate::app::pane::Pane::new(vp("mem:///r/a/y"), Vec::new());
        app.follow_tree();

        let t = app.tree().expect("tree");
        assert_eq!(t.selected(), Some(vp("mem:///r/a/y")));
        assert_eq!(t.rows().len(), 5, "`b` is still unfolded");
    }

    /// Switching panels also changes what the tree looks at: the two sides
    /// are in different places, and a tree left on the previous panel's
    /// would describe the one that no longer has focus.
    ///
    /// And the root climbs to the two sides' COMMON ancestor, without
    /// dropping anything: `Tab` is the program's most-used key, and
    /// re-anchoring at the destination closed the whole tree on every
    /// press. Here the two sides only share the provider's root, so that's
    /// where it ends up climbing to; with two sibling directories — the
    /// normal case — it climbs one level and stops.
    #[test]
    fn the_tree_follows_the_panel_change() {
        let mut app = app_en("mem:///r", "mem:///other");
        app.toggle_tree();
        app.return_keys_to_panes();
        // Opening it hangs it NEAR the listing (2026-09-21): `mem:///r`
        // doesn't hang off home, so off its provider's root, revealing `r`.
        assert_eq!(
            app.tree().and_then(|t| t.root().cloned()),
            Some(vp("mem:///"))
        );
        assert_eq!(
            app.tree().and_then(norte_frontend::tree::Tree::revealing),
            Some(&vp("mem:///r"))
        );

        app.switch_focus();

        assert_eq!(
            app.tree().and_then(|t| t.root().cloned()),
            Some(vp("mem:///")),
            "climbs to the two sides' common ancestor"
        );
        assert_eq!(
            app.tree().and_then(norte_frontend::tree::Tree::revealing),
            Some(&vp("mem:///other")),
            "and the cursor will go to where the now-focused panel is"
        );

        // And switching back no longer moves the root: both hang off it.
        app.switch_focus();
        assert_eq!(
            app.tree().and_then(|t| t.root().cloned()),
            Some(vp("mem:///"))
        );
    }

    /// **A RESTORED layout with the tree inside brings its state.**
    ///
    /// It's the bug piloting the TUI found: yesterday's session saves the
    /// tree, on startup the slot comes back… and paints blank, because the
    /// toggle that would have created its state isn't going to be pressed —
    /// the panel is already there. `set_layout` seeds EVERY kind's state
    /// for this reason, and the tree had to enter that list.
    #[test]
    fn a_layout_with_a_tree_brings_its_state() {
        use norte_frontend::layout::{Dir, KindId, Node, Size, SlotId};

        let mut app = app_two_panes();
        let tree = Node::Split {
            dir: Dir::Horizontal,
            sizes: vec![Size::Fixed(24), Size::Weight(1)],
            children: vec![
                Node::slot(SlotId(90), KindId::new(crate::tree::KIND)),
                Node::slot(SlotId(91), KindId::browser()),
            ],
        };
        app.set_layout(tree);
        assert!(
            app.panes.tree(SlotId(90)).is_some(),
            "the tree's slot arrived with no state and would paint empty"
        );
        assert!(
            app.panes.tree(SlotId(90)).and_then(|t| t.root()).is_some(),
            "and anchored somewhere, or it requests nothing"
        );
    }

    /// And the tree's slot GETS PLACED in the layout: without this the
    /// layout reserves room for it and nobody paints it, which is a blank
    /// column.
    #[test]
    fn the_trees_slot_is_placed() {
        use norte_frontend::layout::{KindRegistry, Rect, resolve};

        let mut app = app_two_panes();
        app.toggle_tree();
        let id = app.tree_slot().expect("open");
        let res = resolve(
            Rect::new(0, 0, 110, 30),
            &app.layout,
            &KindRegistry::builtin(),
        );
        assert!(
            res.placements.iter().any(|(p, _)| *p == id),
            "the tree's slot didn't get placed: {:?}",
            res.placements
        );
        assert!(app.panes.tree(id).is_some(), "and its panel is there");
    }

    /// Three presses, like the sidebar: opens and focuses, focuses again,
    /// closes. The middle one is what makes releasing the keyboard not
    /// close the panel.
    #[test]
    fn the_tree_opens_focuses_and_closes() {
        let mut app = app_two_panes();
        app.toggle_tree();
        assert!(app.tree_slot().is_some());
        app.return_keys_to_panes();
        app.toggle_tree();
        assert!(
            app.tree_slot().is_some(),
            "the second one only regains the keyboard"
        );
        assert_eq!(app.key_owner(), KeyOwner::Tree);
        app.toggle_tree();
        assert!(app.tree_slot().is_none(), "and the third one closes");
        assert_eq!(app.key_owner(), KeyOwner::Panes);
    }
}
