//! The layout: slots, columns, and the screen's split.
//!
//! Part of `controller`: these are methods of `State`, moved here without
//! touching them (ADR 0086). The only writer is still the actor.

// These modules are the same `impl State` split into pieces, so they use
// the same imports as the parent. Listing them here would be a forty-line
// list per file, across 32 files, that goes out of sync the moment the
// parent imports something — `super::*` keeps it in sync on its own.
#[allow(clippy::wildcard_imports)]
use super::*;

impl State {
    /// Opens the layout selector.
    ///
    /// The user's are already read at startup: the selector paints each
    /// one's SHAPE, and reading them on moving the cursor would be I/O in the
    /// event loop.
    pub(super) fn open_layouts(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.selector_layout = Some(norte_frontend::layout_picker::LayoutPicker::open(
            self.layouts.clone(),
        ));
        let change = ViewChange::Layouts {
            layouts: self.vista_layouts(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// Opens the COLUMNS selector over the focused slot's scheme.
    ///
    /// Over ITS scheme and not the default set: columns are configured per
    /// scheme (`sftp` does not show the same as `file`), and opening it over
    /// a different one would be editing a screen other than the one being
    /// looked at.
    pub(super) fn open_columns(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let slot = self.slot();
        let scheme = slot.pane.dir().scheme().to_owned();
        let sort = slot.pane.sort();
        let catalog = self.catalogos.get(scheme.as_str()).cloned();
        self.selector_columns = Some(
            norte_frontend::columns_picker::ColumnsPicker::open_with_catalog(
                &self.columns,
                &scheme,
                sort,
                catalog.as_ref(),
                // No plugin catalogue yet: the selector offers what is
                // CONFIGURED plus the attrs the provider announces, and a
                // plugin column nobody has configured does not show up yet.
                // Offering them requires caching `plugin.list` in the host,
                // which today is requested per decoration batch and thrown
                // away.
                &[],
            ),
        );
        let change = ViewChange::ColumnsPicker {
            columns: self.vista_columns(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// The columns selector's projection.
    pub(super) fn vista_columns(&self) -> Option<crate::dto::ColumnsPickerView> {
        let p = self.selector_columns.as_ref()?;
        let scheme = p.scheme().to_owned();
        Some(crate::dto::ColumnsPickerView {
            // The title already carries the SCOPE, with the key the TUI uses
            // and its `$target`: inventing a second one collided — Fluent
            // keeps the FIRST definition — and mine would have ended up dead
            // with the catalogue saying it was there.
            title: clamp_display(norte_i18n::ta_in(
                self.lang,
                "columns-picker-title",
                &[(
                    "target",
                    &if p.scheme_override() {
                        scheme.clone()
                    } else {
                        norte_i18n::t_in(self.lang, "columns-picker-target-default")
                    },
                )],
            )),
            rows: p
                .rows()
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    // An `attr:` or `plugin:` column's label is given by its
                    // catalogue, i.e. THIRD-PARTY text. A builtin's comes
                    // from Fluent and is ours.
                    let (label, hostile) = column_label(r, &scheme, &self.columns, self.lang);
                    crate::dto::ColumnsPickerRowView {
                        // Identity: whole or empty, never truncated — it is
                        // what comes back to enable, disable and move.
                        id: text_identity(&r.id),
                        label: clamp_display(label),
                        hostile,
                        enabled: r.enabled,
                        format: r.format.clone().unwrap_or_default(),
                        format_locked: r.format_locked,
                        // The first row is the NAME, which by the render's
                        // contract goes first and cannot be disabled.
                        fixed: i == 0,
                    }
                })
                .collect(),
            cursor: p.cursor() as u64,
            // This window does NOT write configuration yet: what is chosen
            // holds for it and is lost on closing it. Staying quiet about it
            // would leave the user believing they just configured norte.
            note: clamp_display(norte_i18n::t_in(self.lang, "columns-picker-session-only")),
            hint: clamp_display(self.column_footer()),
        })
    }

    /// The columns selector's footer, with the chords the keymap BINDS.
    ///
    /// It is composed here and not in the renderer because the verbs can be
    /// rebound, and a translated string that names specific keys stops being
    /// true the moment someone does. An unbound verb drops out of the whole
    /// footer: announcing it with no key helps nobody.
    pub(super) fn column_footer(&self) -> String {
        let parts = [
            ("dialog.toggle-enabled", "columns-picker-hint-toggle"),
            ("dialog.move-up", "columns-picker-hint-move"),
            ("dialog.sort", "columns-picker-hint-sort"),
            ("dialog.cycle-format", "columns-picker-hint-format"),
            ("dialog.confirm", "columns-picker-hint-apply"),
            ("dialog.cancel", "columns-picker-hint-close"),
        ];
        parts
            .iter()
            .filter_map(|(cmd, key)| {
                let chord = self.dialog_chord(cmd);
                if chord.is_empty() {
                    return None;
                }
                Some(format!("{chord} {}", norte_i18n::t_in(self.lang, key)))
            })
            .collect::<Vec<_>>()
            .join(" · ")
    }

    /// The columns selector's keys.
    ///
    /// The same as the TUI's overlay's, by the same model: enabling and
    /// disabling, moving the row up and down, choosing what to sort by, and
    /// cycling the format of the one that allows it.
    pub(super) fn key_in_columns(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.selector_columns.is_none() {
            return (Self::stale(StaleAction::Modal), Vec::new());
        }
        // Through the SHARED resolver (#287), not fixed keys: a preset that
        // rebinds `dialog.move-up` has to change this window the same way it
        // changes the TUI, which is what the common catalogue exists for. And
        // the footer is painted with the chords that come out of here, not a
        // literal: a footer that says `Shift+↑/↓` over code that listens for
        // something else is a lie only discovered by trying it.
        let Some(verb) = self.dialog_verb(k) else {
            return (self.applied(), Vec::new());
        };
        let Some(p) = self.selector_columns.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        match verb.as_str() {
            "dialog.cancel" => self.selector_columns = None,
            "dialog.down" => p.down(),
            "dialog.up" => p.up(),
            "dialog.move-down" => p.move_down(),
            "dialog.move-up" => p.move_up(),
            "dialog.toggle-enabled" => p.toggle(),
            "dialog.sort" => p.sort_current(),
            "dialog.cycle-format" => p.cycle_format(),
            "dialog.confirm" => return self.apply_columns(backend, mailbox),
            _ => return (self.applied(), Vec::new()),
        }
        let change = ViewChange::ColumnsPicker {
            columns: self.vista_columns(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// Applies what was chosen TO THIS WINDOW.
    ///
    /// It does not write `norte.toml`: this phase mutates nothing on disk,
    /// and the selector SAYS so in its own note. Same deal as the layout
    /// selector's.
    ///
    /// If the set of `attr:`/`plugin:` columns changes, a RE-LIST is needed:
    /// an attr's values only arrive by requesting them in `fs.list`, so a new
    /// column over the old listing would stay blank — indistinguishable from
    /// "this file has no such attribute" — until the next `cd`. What decides
    /// that is the SHARED fingerprint (`pane_fingerprint`), not a count of
    /// its own here.
    pub(super) fn apply_columns(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(p) = self.selector_columns.as_ref() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        let picked = p.finish();
        self.selector_columns = None;
        let before = self.column_footprints();
        self.columns
            .apply_picked(picked.scheme_target.as_deref(), &picked.ids, picked.sort);
        for (id, fmt) in &picked.formats {
            self.columns.apply_format(id, fmt);
        }
        for id in self.slots.keys().copied().collect::<Vec<_>>() {
            if before.get(&id) != self.column_footprints().get(&id) {
                self.re_list(id, backend, mailbox);
            }
        }
        let snap = self.snapshot();
        (
            self.applied(),
            vec![self.over(UiUpdate::Snapshot(Box::new(snap)))],
        )
    }

    /// Requests a slot's listing again, without moving from its place.
    ///
    /// Requested by a column change: an `attr:`'s values only arrive if
    /// requested in `fs.list`, so a new column over the old listing would
    /// stay blank — indistinguishable from "this file has no such
    /// attribute".
    pub(super) fn re_list(
        &mut self,
        slot: u32,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) {
        // A single reload path. This used to have its own copy and was
        // missing the two things that make a reload go unnoticed: it did not
        // anchor the cursor nor keep the marks, so changing columns sent the
        // cursor to the first row and erased the selection.
        let _ = self.refresh(slot, backend, mailbox);
    }

    /// Each slot's column fingerprint: which `attr:`/`plugin:` it paints.
    pub(super) fn column_footprints(&self) -> std::collections::BTreeMap<u32, Vec<String>> {
        self.slots
            .iter()
            .map(|(id, h)| (*id, self.columns.pane_fingerprint(h.pane.dir().scheme())))
            .collect()
    }

    /// The layout selector's projection, with its preview.
    ///
    /// The thumbnail is painted by the SAME engine that splits the real
    /// screen, so it cannot lie about what will come out.
    pub(super) fn vista_layouts(&self) -> Option<crate::dto::LayoutPickerView> {
        /// The thumbnail's size, in characters.
        const THUMBNAIL: (u16, u16) = (32, 12);

        let p = self.selector_layout.as_ref()?;
        let current = p.current();
        // The parser's diagnostic can QUOTE the user's file: it goes through
        // the same gate as everything else, with its flag (#266) — what is
        // masked is said.
        let diagnostic = current.and_then(|r| r.problem.clone()).map_or_else(
            || (String::new(), false),
            |p| norte_frontend::display_name(p.as_bytes()),
        );
        Some(crate::dto::LayoutPickerView {
            title: clamp_display(norte_i18n::t_in(self.lang, "layout-picker-title")),
            rows: p
                .rows()
                .iter()
                .map(|r| {
                    // The name is a FILE name: bytes (rule 1). It is painted
                    // through the shared gate and does NOT travel as a key —
                    // choosing a row sends its index.
                    let (displayable, hostile) =
                        norte_frontend::display::display_os_name(r.name.as_os_str());
                    crate::dto::LayoutRowView {
                        name: clamp_display(displayable),
                        hostile,
                        factory: r.factory,
                        shares_keymap_name: r.shares_keymap_name,
                        broken: r.tree.is_none(),
                    }
                })
                .collect(),
            cursor: p.cursor() as u64,
            preview: current
                .and_then(|r| r.tree.as_ref())
                .map_or_else(Vec::new, |t| {
                    norte_frontend::layout_picker::preview(t, THUMBNAIL.0, THUMBNAIL.1, &self.kinds)
                }),
            problem: clamp_display(diagnostic.0),
            problem_hostile: diagnostic.1,
        })
    }

    /// The keys while the layout selector is open.
    pub(super) fn key_in_layouts(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.selector_layout.is_none() {
            return (Self::stale(StaleAction::Modal), Vec::new());
        }
        let verb = self.dialog_verb(k);
        let Some(p) = self.selector_layout.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        match verb.as_deref() {
            Some("dialog.cancel") => self.selector_layout = None,
            Some("dialog.down") => p.down(),
            Some("dialog.up") => p.up(),
            Some("dialog.confirm") => return self.apply_layout_chosen(backend, mailbox),
            _ => return (self.applied(), Vec::new()),
        }
        let change = ViewChange::Layouts {
            layouts: self.vista_layouts(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// A click on a selector row: selects it AND applies it.
    pub(super) fn choose_layout(
        &mut self,
        row: u32,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(p) = self.selector_layout.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        // Out of range is NOT clamped: the `while` below stops at the last
        // row, so an old index used to apply THE LAST layout in the list —
        // the host's most invasive operation — instead of doing nothing. The
        // three sibling actions that only point already do it this way.
        if row as usize >= p.rows().len() {
            return (Self::stale(StaleAction::Generation), Vec::new());
        }
        // The shared selector has no `set_cursor`: it walks to the row,
        // which for a list of five to ten is the same and does not add
        // surface to a model that is already tested.
        while p.cursor() > row as usize {
            p.up();
        }
        while p.cursor() < row as usize && p.cursor() + 1 < p.rows().len() {
            p.down();
        }
        self.apply_layout_chosen(backend, mailbox)
    }

    /// Applies the cursor's layout.
    ///
    /// One that does not parse is NOT applied, and it is said: the row
    /// already carries its reason, and changing the screen for a broken file
    /// would be worse than doing nothing. It is applied for THIS window and
    /// not written to the configuration: writing is mutating, and it arrives
    /// with phase 5.
    pub(super) fn apply_layout_chosen(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(p) = self.selector_layout.as_ref() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        let Some(tree) = p.current().and_then(|r| r.tree.clone()) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-layout-broken".to_owned(),
                },
                Vec::new(),
            );
        };
        self.selector_layout = None;
        self.apply_layout(tree, backend, mailbox)
    }

    /// Changes the WHOLE TREE: a different layout.
    ///
    /// Sends a SNAPSHOT and not a patch: it changes the split, which slots
    /// there are and what is inside each one. Listings the new layout places
    /// that did not exist start in the directory the one already there was
    /// in, which is the least surprising thing: changing the screen's shape
    /// is not going somewhere else.
    pub(super) fn apply_layout(
        &mut self,
        tree: Node,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.apply_layout_with(tree, None, backend, mailbox)
    }

    /// Like [`Self::apply_layout`], saying which slot STAYS active.
    ///
    /// `None` = the reconciliation decides, which is what is needed when the
    /// tree comes from outside. `Some` is for whoever just created a slot and
    /// wants focus there: in two steps it would be two snapshots, and the
    /// first would show focus where it no longer is.
    pub(super) fn apply_layout_with(
        &mut self,
        tree: Node,
        active: Option<SlotId>,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.set_tree(tree, active);
        self.wake_visible(backend, mailbox);
        if self.places_slot().is_some() {
            self.seed_places();
            self.request_places(backend, mailbox);
        }
        // The tree changed: to the session NOW, without waiting for the tick.
        // An open panel or a chosen template is exactly what the reader
        // expects to find on returning, and a close that does not make it in
        // time must not lose it.
        self.push_session(backend, mailbox);
        let snap = self.snapshot();
        (
            self.applied(),
            vec![self.over(UiUpdate::Snapshot(Box::new(snap)))],
        )
    }

    /// Sets `tree` as the screen's tree and seeds the slots it debuts, WITHOUT
    /// requesting any listing nor touching the session.
    ///
    /// It is the I/O-free half of [`Self::apply_layout_with`], and what
    /// startup uses for the layout the session saved: there the listings are
    /// requested later, once the session has said where each one was, and
    /// waking them here would request the startup directory just to
    // TODO(translation): review — this doc comment is interrupted midway
    /// (it breaks off at "...just to") by `redo_split`'s own doc and
    /// function landing in between, and the sentence's tail
    /// ("throw it away an instant later.") is stranded below as its own
    /// one-line comment right before this function's signature; it looks
    /// like a stale split left by an earlier edit. The translation keeps the
    /// same broken shape as the original.
    /// Recomputes the split with the tree and the CURRENT registry.
    ///
    /// Declaring a kind changes minimums and focusability, and until phase 3
    /// the split was only rebuilt on setting a tree or changing the
    /// viewport: the panels plugins contribute arrive AFTER the first
    /// snapshot, so the slot already placed was stuck with the split that
    /// did not know it.
    pub(super) fn redo_split(&mut self) {
        self.split = resolve(rect(self.viewport), &self.tree, &self.kinds);
    }

    /// throw it away an instant later.
    pub(super) fn set_tree(&mut self, tree: Node, active: Option<SlotId>) {
        let dir = self.slot().pane.dir().clone();
        // A slot that debuts is born like the startup ones: with the
        // configuration's hidden-files setting applied.
        let hidden_default = self.config.common.ui_show_hidden.unwrap_or(true);
        self.tree = tree;
        self.split = resolve(rect(self.viewport), &self.tree, &self.kinds);
        // From the TREE, not from the layout. `placements` and `hidden`
        // PARTITION the tree, so seeding from `placements` erases the slot of
        // a listing the layout does not place — a `Tabs` whose active one is
        // a different kind, or an all-weighted split that does not fit — and
        // with it the LAST one can go: `slots` ends up empty and the next
        // key dies on `slot()`'s `expect`, inside the actor's task.
        // `validate` guarantees the tree HAS a listing, not that the layout
        // places it, so the guarantee has to be taken from the tree.
        let new_ids: Vec<u32> = self
            .tree
            .slot_ids()
            .into_iter()
            .map(|SlotId(id)| id)
            .filter(|id| es_listing(&self.tree, SlotId(*id), &self.kinds))
            .collect();
        self.slots.retain(|id, _| new_ids.contains(id));
        for id in new_ids {
            if let std::collections::btree_map::Entry::Vacant(slot) = self.slots.entry(id) {
                slot.insert(Slot::empty(
                    dir.clone(),
                    hidden_default,
                    self.columns.sort_for(dir.scheme()),
                    self.config.common.ui_parent_entry.unwrap_or(true),
                ));
            }
        }
        match active {
            Some(id) => self.roles.set(RoleId::Active, id),
            None => self.roles.clear(RoleId::Active),
        }
        self.reconciles_roles();
    }

    /// Changes the size of the slot with FOCUS, not the active listing.
    ///
    /// From focus on purpose: the only way to widen the side bar is to have
    /// it focused and grow, and `active()` — which skips whatever is not a
    /// listing — would have resized the panel next to it.
    ///
    /// Resizing is a decision about THE TREE, so it is saved in it: the split
    /// is recomputed from the new tree, not the other way around.
    pub(super) fn resize(
        &mut self,
        delta: i64,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let step = i16::try_from(delta.clamp(i64::from(i16::MIN), i64::from(i16::MAX)))
            .unwrap_or(if delta < 0 { -1 } else { 1 });
        let updated = self.tree.resize(SlotId(self.focused()), step);
        self.apply_tree(updated, backend, mailbox)
    }

    /// Equalizes the weight of the focused slot's siblings.
    pub(super) fn equalize(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let updated = self.tree.equalize(SlotId(self.focused()));
        self.apply_tree(updated, backend, mailbox)
    }

    /// Flips the focused slot's split (ADR 0138): side by side becomes one on
    /// top of the other. A split with chrome does not flip, and then
    /// `apply_tree` sends nothing.
    pub(super) fn rotate(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let updated = self.tree.flip(SlotId(self.focused()));
        self.apply_si_fits(updated, None, backend, mailbox)
    }

    /// Moves slot `slot` next to `target` (ADR 0138): dropping a panel dragged
    /// by its title. An id that is not there — the layout changed between the
    /// drag and the drop — leaves the tree as it was, and `apply_tree`
    /// sends nothing.
    pub(super) fn mover_slot(
        &mut self,
        slot: u32,
        target: u32,
        zone: norte_frontend::layout::DropZone,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let updated = self.tree.move_slot(SlotId(slot), SlotId(target), zone);
        // In the center, the target goes behind a tab on purpose.
        let tolerated =
            (zone == norte_frontend::layout::DropZone::Center).then_some(SlotId(target));
        self.apply_si_fits(updated, tolerated, backend, mailbox)
    }

    /// Like [`Self::apply_tree`], but only if `new` leaves visible what
    /// was visible (`keeps_on_screen`, the SAME rule as the TUI's). If not, it
    /// touches nothing and says so on the bar, like splitting with no room.
    fn apply_si_fits(
        &mut self,
        new: Node,
        tolerated: Option<SlotId>,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let after = resolve(rect(self.viewport), &new, &self.kinds);
        if new != self.tree
            && !norte_frontend::layout::keeps_on_screen(&self.split, &after, &new, tolerated)
        {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-layout-move-no-room".to_owned(),
                },
                self.say("msg-layout-move-no-room"),
            );
        }
        self.apply_tree(new, backend, mailbox)
    }

    /// Replaces the tree and re-splits.
    ///
    /// If the split does not change — the slot was at its limit, or has no
    /// siblings to split with — NOTHING is sent: a patch that changes nothing
    /// forces a repaint for nothing, and the key already said its piece
    /// without moving anything.
    pub(super) fn apply_tree(
        &mut self,
        new: Node,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let before = self.split.clone();
        let tree_before = std::mem::replace(&mut self.tree, new);
        self.split = resolve(rect(self.viewport), &self.tree, &self.kinds);
        // The same on screen AND in the tree: nothing to say. The split alone
        // is not enough — reordering a group's tabs (ADR 0138) does not move
        // a rectangle, and with neither patch nor session the new order
        // reached neither the window nor got saved.
        if self.split.placements == before.placements && self.tree == tree_before {
            return (self.applied(), Vec::new());
        }
        self.reconciles_roles();
        // The split changed: whatever just came out of `hidden` has no
        // listing and nobody else is going to request it.
        self.wake_visible(backend, mailbox);
        // And to the session now: a size is a decision about the tree.
        self.push_session(backend, mailbox);
        let change = ViewChange::Layout(self.layout());
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// This size's split, with the roles set.
    ///
    /// Comes from the same `resolve` the TUI uses: the renderer receives
    /// rectangles in cells and not a list of slots it has to place itself,
    /// which would be a second layout rule written in another language
    /// (decision D14).
    pub(super) fn layout(&self) -> LayoutView {
        let active = self.roles.get(RoleId::Active);
        let target = self.roles.get(RoleId::Target);
        let placements = self
            .split
            .placements
            .iter()
            .map(|(slot, r)| {
                let SlotId(id) = *slot;
                let role = if Some(*slot) == active {
                    Some(SlotRole::Active)
                } else if Some(*slot) == target {
                    Some(SlotRole::Target)
                } else {
                    None
                };
                let focus_index = self
                    .split
                    .focus_order
                    .iter()
                    .position(|s| s == slot)
                    .unwrap_or(usize::MAX);
                SlotPlacement {
                    slot_id: id,
                    x: r.x,
                    y: r.y,
                    width: r.width,
                    height: r.height,
                    role,
                    focus_index: u32::try_from(focus_index).unwrap_or(u32::MAX),
                }
            })
            .collect();
        LayoutView {
            cells: self.viewport,
            tabs: self.tab_groups(),
            placements,
            // Slots that CAN be a target are counted, not the placed ones: a
            // split also carries the status bar and the task strip, and
            // counting them would make "three or more" always hold — which
            // is the same as not having the rule.
            mark_target: norte_frontend::layout::target_worth_marking(
                self.split
                    .placements
                    .iter()
                    .filter(|(slot, _)| {
                        self.tree
                            .kind_of(*slot)
                            .is_some_and(|k| self.kinds.holds_role(k, RoleId::Target))
                    })
                    .count(),
            ),
        }
    }

    /// The TAB groups there are on screen.
    ///
    /// One per placed slot living inside a `Tabs`: inactive ones are not
    /// placed — the shared splitter does not paint them — and without this
    /// the window would show the front one without saying there are two
    /// others open.
    pub(super) fn tab_groups(&self) -> Vec<crate::dto::TabGroupView> {
        let mut groups = Vec::new();
        for (slot, _) in &self.split.placements {
            let Some((tabs, active)) = self.tree.tabs_of(*slot) else {
                continue;
            };
            if tabs.len() < 2 {
                // A group of ONE is not a group: painting it a tab bar is
                // chrome that says nothing and steals a row.
                continue;
            }
            let SlotId(id) = *slot;
            let panels = tabs
                .iter()
                .all(|t| kind_de(&self.tree, *t).is_some_and(|k| k.as_str() != "browser"));
            groups.push(crate::dto::TabGroupView {
                slot_id: id,
                tabs: tabs.iter().map(|t| self.tab(*t)).collect(),
                active: active as u64,
                panels,
            });
        }
        groups
    }

    /// A tab: which slot it carries inside and what it is called.
    ///
    /// The label is its listing's DIRECTORY name — not the whole path, which
    /// does not fit — masked like any other name: a hostile one inside a tab
    /// is as hostile as inside a listing. A root directory has no name: it
    /// falls back to the scheme, which is the only thing that sets it apart
    /// from another.
    pub(super) fn tab(&self, slot: SlotId) -> crate::dto::TabView {
        let SlotId(id) = slot;
        let (title, hostile) = if let Some(h) = self.slots.get(&id) {
            let dir = h.pane.dir();
            match dir.file_name() {
                Some(seg) => norte_frontend::display_name(seg.as_bytes()),
                // A root has no name: it falls back to the scheme, which is
                // the only thing that sets it apart from another.
                None => (dir.scheme().to_owned(), false),
            }
        } else {
            // Whatever is not a listing is named like its panel bar button
            // ("Viewer", "Details"): since a border's panels are grouped into
            // tabs (phase F) this label is READ, and the kind's id is not a
            // name. The kind comes from a layout file, so the result goes
            // through the same gate.
            let kind = kind_de(&self.tree, slot)
                .map_or_else(|| "unknown".to_owned(), |k| k.as_str().to_owned());
            let name =
                norte_frontend::panelbar::label_in(self.lang, &kind, &format!("layout.{kind}"));
            norte_frontend::display_name(name.as_bytes())
        };
        crate::dto::TabView {
            slot_id: id,
            title: clamp_display(title),
            title_hostile: hostile,
        }
    }
}
