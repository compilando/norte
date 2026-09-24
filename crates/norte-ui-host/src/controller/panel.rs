//! The panel: cursor, marks, focus, sort order and columns.
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
    /// Moves the slot's cursor, clamping at the ends.
    pub(super) fn mover_cursor(
        &mut self,
        slot_id: u32,
        delta: i64,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if slot_id != self.active() {
            return (Self::stale(StaleAction::Generation), Vec::new());
        }
        if self.slot().pane.entries().is_empty() {
            return (self.applied(), Vec::new());
        }
        let current = i128::try_from(self.slot().pane.cursor()).unwrap_or(0);
        let last = i128::try_from(self.slot().pane.entries().len() - 1).unwrap_or(0);
        let target = (current + i128::from(delta)).clamp(0, last);
        let i = usize::try_from(target).unwrap_or(0);
        self.slot_mut().pane.set_cursor(i);
        (self.applied(), vec![self.parche_cursor()])
    }

    /// Puts the cursor on a specific row (a click).
    pub(super) fn set_cursor(
        &mut self,
        slot_id: u32,
        key: RowKey,
        generation: u64,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(i) = self.row_of(slot_id, key, generation) else {
            // A row that no longer exists: the listing changed under the
            // click. Neither interpreted nor an error.
            return (Self::stale(StaleAction::Generation), Vec::new());
        };
        self.slot_mut().pane.set_cursor(i);
        (self.applied(), vec![self.parche_rows()])
    }

    /// Marks or unmarks a row.
    pub(super) fn mark(
        &mut self,
        slot_id: u32,
        key: RowKey,
        generation: u64,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(i) = self.row_of(slot_id, key, generation) else {
            return (Self::stale(StaleAction::Generation), Vec::new());
        };
        let marked = self
            .slot()
            .pane
            .entries()
            .get(i)
            .is_some_and(|e| self.slot().pane.is_marked(e));
        self.slot_mut().pane.set_mark(i, !marked);
        (self.applied(), vec![self.parche_rows()])
    }

    /// The row an action names, if the slot is the active one and the key
    /// still holds IN THE GENERATION the renderer said.
    ///
    /// The `(key, generation)` pair is what makes the key mean something:
    /// alone it is an index, and an index from the previous screen names a
    /// different file. A filler batch that lands between painting and the
    /// click reorders the listing and bumps the epoch; without this
    /// comparison, the click marks whatever fell on that row.
    pub(super) fn row_of(&self, slot_id: u32, key: RowKey, generation: u64) -> Option<usize> {
        if slot_id != self.active() || self.slot().pane.listing_epoch() != generation {
            return None;
        }
        self.row_valid(key)
    }

    /// This frontend does not mutate yet, and it SAYS so.
    ///
    /// A mute key is worse than a "not here": a user who presses F8 and sees
    /// nothing does not know whether they deleted something.
    pub(super) fn no_mutates() -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        (
            ActionAck::Unavailable {
                reason_key: "host-read-only".to_owned(),
            },
            Vec::new(),
        )
    }

    /// Moves focus to the next focusable slot, or to the previous one.
    ///
    /// With `solo_listings`, the side panels are skipped: it is `pane.switch`,
    /// the orthodox `Tab`, and what it answers is "the other panel". Without
    /// it, it is `layout.focus-next`, the whole screen's walk.
    pub(super) fn mover_focus(
        &mut self,
        back: bool,
        solo_listings: bool,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // The walk is the SHARED one: `focus_order` already skips what is not
        // visible and what does not take focus (a status bar does not
        // receive focus), so there is no second rule here that could
        // diverge from the TUI's.
        //
        // With one exception, and it is the one this ring has to apply: what
        // is FOCUSABLE is not what TAKES KEYS. The attribute sheet is the
        // first and not the second — it follows the listing's cursor, and
        // with the keyboard inside it would stop following anything — so
        // stopping there is a stop no key gets you out of. It is skipped,
        // and it comes back around anyway because the walk cycles.
        let current = SlotId(self.focused());
        let next = self.next_in_ring(current, back, solo_listings);
        let Some(SlotId(id)) = next else {
            // A single slot: there is nowhere to go, and saying so is more
            // honest than pretending something happened.
            return (
                ActionAck::Unavailable {
                    reason_key: "host-no-other-slot".to_owned(),
                },
                Vec::new(),
            );
        };
        self.roles.set(RoleId::Active, SlotId(id));
        self.reconcilia_roles();
        // The tree follows the active panel, and which one that is just
        // changed. It goes through a SNAPSHOT because the tree has no
        // `ViewChange` of its own: it travels whole or not at all, and a tree
        // cursor that does not cross leaves the panel pointing at the
        // previous panel's branch.
        //
        // Only if the tree REALLY moved. Landing on the places bar does not
        // move it — `follow_branches` only follows a listing — and sending the
        // whole screen for that is paying for a snapshot for a layout patch.
        let active = self.active();
        if self.follow_branches(active, backend, mailbox) {
            let snap = self.snapshot();
            return (
                self.applied(),
                vec![self.over(UiUpdate::Snapshot(Box::new(snap)))],
            );
        }
        let change = ViewChange::Layout(self.layout());
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// Drags the border between `slot` and the slot next to it to `cells`.
    ///
    /// `cells` is where the POINTER is, and everything else is resolved here:
    /// which pair that border forms, how much they occupy together, and in
    /// which direction they split. The renderer only converts pixels to
    /// cells, which is what it already does to declare its viewport.
    ///
    /// The neighbor is looked up in the LAYOUT and not the tree: what the
    /// reader has grabbed is a screen border, and two slots are neighbors
    /// when one starts where the other ends. With no neighbor there is no
    /// border, and then this is not a drag but a race with an earlier
    /// layout.
    pub(super) fn drag_edge(
        &mut self,
        slot: u32,
        cells: u16,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some((_, ra)) = self
            .split
            .placements
            .iter()
            .find(|(SlotId(id), _)| *id == slot)
            .copied()
        else {
            return (Self::stale(StaleAction::Generation), Vec::new());
        };
        // The neighbor: the one that starts exactly where this one ends, on
        // one of the two axes.
        let right = self.split.placements.iter().find(|(_, r)| {
            r.x == ra.x + ra.width && r.y < ra.y + ra.height && ra.y < r.y + r.height
        });
        let below = self.split.placements.iter().find(|(_, r)| {
            r.y == ra.y + ra.height && r.x < ra.x + ra.width && ra.x < r.x + r.width
        });
        let (neighbor, axis) = match (right, below) {
            (Some((b, _)), _) => (*b, norte_frontend::layout::Dir::Horizontal),
            (None, Some((b, _))) => (*b, norte_frontend::layout::Dir::Vertical),
            (None, None) => return (Self::stale(StaleAction::Generation), Vec::new()),
        };
        // The real pair is the layout's, where both are neighbors, and it is
        // measured WHOLE: the border between the second listing and the
        // details separates the body from the details, not that listing from
        // them.
        let Some((left_slot, right_slot)) = self.tree.border_pair(SlotId(slot), neighbor) else {
            return (Self::stale(StaleAction::Generation), Vec::new());
        };
        let Some((start, length)) =
            norte_frontend::layout::border_span(&self.split, &left_slot, &right_slot, axis)
        else {
            return (Self::stale(StaleAction::Generation), Vec::new());
        };
        if length == 0 {
            return (Self::stale(StaleAction::Generation), Vec::new());
        }
        let frac = f32::from(cells.saturating_sub(start)) / f32::from(length);
        let tree = self
            .tree
            .drag_border_between(SlotId(slot), neighbor, frac, length);
        if tree == self.tree {
            // The border did not move: neither snapshot nor patch. A drag
            // fires one event per pixel, and repainting the whole screen for
            // each would be paying for a snapshot for a hand tremor.
            return (self.applied(), Vec::new());
        }
        self.apply_layout(tree, backend, mailbox)
    }

    /// The shared walk's next slot that qualifies as a stop.
    ///
    /// With `solo_listings`, only `browser`s; without it, any that TAKES
    /// KEYS. What is focusable is not what takes keys: the attribute sheet is
    /// the first and not the second — it follows the listing's cursor, and
    /// with the keyboard inside it would stop following anything — so
    /// stopping there would be a stop no key gets you out of.
    ///
    /// Goes around at most once: if none qualifies — a screen that only has
    /// an attribute sheet, which the layout allows — it returns `None`
    /// instead of spinning forever.
    pub(super) fn next_in_ring(
        &self,
        from: SlotId,
        back: bool,
        solo_listings: bool,
    ) -> Option<SlotId> {
        let mut current = from;
        for _ in 0..self.split.focus_order.len() {
            let next = if back {
                norte_frontend::layout::focus_prev(&self.split, current)?
            } else {
                norte_frontend::layout::focus_next(&self.split, current)?
            };
            if next == from {
                return None; // went all the way around without finding one
            }
            let qualifies = kind_de(&self.tree, next).is_some_and(|k| {
                if solo_listings {
                    k == norte_frontend::layout::KindId::browser()
                } else {
                    self.kinds.get(&k).is_some_and(|d| d.takes_keys)
                }
            });
            if qualifies {
                return Some(next);
            }
            current = next;
        }
        None
    }

    /// Designates ANOTHER visible slot as the target of the next operation.
    pub(super) fn designar_dest(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // The next one that is NOT the focused one: designating yourself as
        // the target is asking a copy to copy onto itself. Same rule as the
        // TUI's.
        let active = self.active();
        let candidates: Vec<u32> = self
            .slots
            .keys()
            .copied()
            .filter(|id| *id != active && !self.oculto(*id))
            .collect();
        let current = self.roles.get(RoleId::Target).map(|SlotId(id)| id);
        let next = match current.and_then(|a| candidates.iter().position(|c| *c == a)) {
            Some(i) => candidates.get((i + 1) % candidates.len()).copied(),
            None => candidates.first().copied(),
        };
        let Some(id) = next else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-no-other-slot".to_owned(),
                },
                Vec::new(),
            );
        };
        self.roles.set(RoleId::Target, SlotId(id));
        let change = ViewChange::Layout(self.layout());
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// Sorts a listing by a column, with the shared rule.
    ///
    /// The id travels as text because that is how its header travelled;
    /// what it means — and whether it reverses or starts over — is resolved
    /// by `norte-frontend`, not a table here (ADR 0066, decision D14).
    pub(super) fn sort_by(
        &mut self,
        slot_id: u32,
        column: &str,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        use norte_frontend::columns::{ColumnId, sort_column_id};
        if !self.slots.contains_key(&slot_id) || self.oculto(slot_id) {
            return (Self::stale(StaleAction::Generation), Vec::new());
        }
        // The ones for this slot's SCHEME, not only the painted ones: the fit
        // (ADR 0124) may have dropped one, and sorting by it still makes
        // sense — it is the same thing the sort menu offers.
        let configured = self
            .slots
            .get(&slot_id)
            .map(|h| self.columns_of(h.pane.dir()))
            .unwrap_or_default();
        let col = configured
            .iter()
            .find(|c| column_identity(c) == *column)
            .and_then(sort_column_id)
            .or_else(|| {
                // An id that is not configured but IS a known column can
                // still sort: a sort menu offers more columns than are
                // painted. Only the FIXED ones: an `attr:` has to be
                // configured (ADR 0144), or any renderer text would end up in
                // the sort order and in the session.
                column
                    .parse::<ColumnId>()
                    .ok()
                    .filter(|id| matches!(id, ColumnId::Builtin(_)))
                    .as_ref()
                    .and_then(sort_column_id)
            });
        let Some(col) = col else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-column-not-sortable".to_owned(),
                },
                Vec::new(),
            );
        };
        self.sort_by_column(slot_id, col)
    }

    /// Sorts a listing by an already resolved column.
    ///
    /// The two doors — clicking the header and the catalogue's
    /// `pane.sort-*` — end up HERE, and that is why they sort the same way:
    /// the active column reverses and a new one starts ascending, because
    /// what decides that is `SortSpec::after_click` and not a table per
    /// surface.
    pub(super) fn sort_by_column(
        &mut self,
        slot_id: u32,
        col: norte_frontend::SortColumn,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(h) = self.slots.get_mut(&slot_id) else {
            return (Self::stale(StaleAction::Generation), Vec::new());
        };
        let spec = h.pane.sort().after_click(col);
        h.pane.set_sort(spec);
        // Re-sorting moves EVERY row — so it bumps the generation and the
        // whole window travels — AND the header's sort mark. Without the
        // second, the listing repainted in the new order and the `▲` kept
        // describing the previous one.
        let rows_change = self.patch_rows_of(slot_id);
        let headers = self.slots.get(&slot_id).map(|h| self.headers(slot_id, h));
        let mut outgoing = vec![rows_change];
        if let Some(columns) = headers {
            let change = ViewChange::Columns { slot_id, columns };
            outgoing.push(self.parche(vec![change]));
        }
        (self.applied(), outgoing)
    }

    /// The columns configured for a slot's scheme.
    ///
    /// By SCHEME and not once on startup: `[ui.columns.schemes.sftp]` is real
    /// configuration, and resolving it on startup left it dead the moment the
    /// panel navigated somewhere else.
    pub(super) fn columns_of(&self, dir: &VPath) -> Vec<norte_frontend::columns::ColumnId> {
        self.columns
            .layout_items_for(dir.scheme())
            .into_iter()
            .map(|(id, _)| id)
            .collect()
    }

    /// The attribute ids a directory's scheme requests.
    ///
    /// They travel with EVERY listing: a provider only delivers what it is
    /// asked for, and an `attr:` column that is not requested stays blank
    /// forever.
    pub(super) fn attrs_de(&self, dir: &VPath) -> Vec<String> {
        self.columns.attr_ids_for(dir.scheme())
    }

    /// The listing's headers, with the label already translated and the sort
    /// mark set.
    ///
    /// `header_label` and `sort_column_id` are the SAME functions the TUI
    /// uses: what a column is called and whether it sorts cannot depend on
    /// who paints it.
    // TODO(translation): review — this paragraph describes `headers`
    /// (right after it), but the item before it is a leftover one-line doc
    /// ("A single listing's projection") that seems to belong to `browser`,
    /// further down; it looks like a stale fragment left by an earlier edit.
    /// The columns painted in `slot`, fitted so their names can be read: the
    /// SAME rule as the terminal's (`norte_frontend::columns::fitted_columns`),
    /// over the width in cells the layout gives the slot.
    ///
    /// The interior discounts four cells — borders, padding and the mark
    /// checkbox — and what goes in front of the name in the row is the badge
    /// and, if there are any, the icons. A slot the layout does not place
    /// paints all its columns: there is no width to decide with, and dropping
    /// one without knowing is dropping for the sake of dropping.
    pub(super) fn setting_of(
        &self,
        slot: u32,
        target_slot: &Slot,
    ) -> Vec<norte_frontend::columns::Fitted> {
        use norte_frontend::columns::fitted_columns;
        let scheme = target_slot.pane.dir().scheme();
        // This pane's catalogue: decides whether the permissions column the
        // listing sets is painted (spec 2026-09-20). The SAME one that feeds
        // the headers, so width and header do not disagree.
        let catalog = self.catalog_of(target_slot.pane.dir());
        let width = self
            .split
            .placements
            .iter()
            .find(|(s, _)| s.0 == slot)
            .map(|(_, r)| r.width);
        match width {
            Some(width) => {
                let lead: u16 = 2 + if target_slot.pane.any_icon() { 3 } else { 0 };
                let wants = target_slot.pane.name_width_p80().saturating_add(lead);
                fitted_columns(
                    &self.columns,
                    scheme,
                    width.saturating_sub(4),
                    wants,
                    catalog,
                )
            }
            None => fitted_columns(&self.columns, scheme, u16::MAX / 2, 0, catalog),
        }
    }

    pub(super) fn headers(&self, slot: u32, target_slot: &Slot) -> Vec<ColumnHeader> {
        use norte_frontend::columns::{header_label_in, sort_column_id};
        let spec = target_slot.pane.sort();
        let catalog = self.catalog_of(target_slot.pane.dir());
        let scheme = target_slot.pane.dir().scheme().to_owned();
        // Each column's width policy, ONCE per header: only the fixed one
        // travels (bridge 64); `auto` and `flex` paint at whatever they
        // measure, which is what this window used to do with all of them.
        let policies = self.columns.layout_items_for(&scheme);
        self.setting_of(slot, target_slot)
            .iter()
            .map(|f| {
                let id = &f.id;
                let width = policies
                    .iter()
                    .find(|(c, _)| c == id)
                    .and_then(|(_, item)| match item.policy {
                        // The NAME carries no fixed width: it carries its
                        // FLOOR, the shared layout's, so the renderer does
                        // not have to repeat the number.
                        _ if item.is_name => Some(norte_frontend::columns::NAME_MIN),
                        // Compact, its width is the short one even if the
                        // policy says otherwise.
                        _ if f.compact => Some(norte_frontend::columns::COMPACT_WIDTH),
                        norte_frontend::columns::WidthPolicy::Fixed(n) => Some(n),
                        norte_frontend::columns::WidthPolicy::Auto
                        | norte_frontend::columns::WidthPolicy::Flex { .. } => None,
                    });
                // The CONFIGURED style, not the factory one: `[ui.columns]`
                // lets you set your own label, format, alignment and width
                // per column, and asking for `default_for_id` left all of
                // that dead in this window while the terminal honored it.
                // A plugin column's manifest label comes in through
                // `apply_plugin_headers`: without it the header showed the id
                // (`ORG.NORTE.SIZE-BAR/BAR` instead of "Size").
                let style = self
                    .columns
                    .style_for_id(&scheme, id, catalog)
                    .compacted(f.compact);
                let sortable_id = sort_column_id(id);
                // Borrowed and not consumed: the same answer also says
                // whether the header is clickable. With an attribute (ADR
                // 0144) both things come out of here with no code of its
                // own.
                let sort = sortable_id
                    .as_ref()
                    .filter(|c| **c == spec.column)
                    .map(|_| {
                        match spec.dir {
                            norte_frontend::SortDir::Asc => "asc",
                            norte_frontend::SortDir::Desc => "desc",
                        }
                        .to_owned()
                    });
                ColumnHeader {
                    id: column_identity(id),
                    // With THIS host's language, not the process's: the two
                    // do not have to match, and half a screen in each
                    // language is worse than no translation at all.
                    label: clamp_display(header_label_in(id, &style, catalog, self.lang)),
                    sort,
                    sortable: sortable_id.is_some(),
                    width,
                    align: match style.align {
                        norte_frontend::columns::Align::Left => "left",
                        norte_frontend::columns::Align::Right => "right",
                    }
                    .to_owned(),
                }
            })
            .collect()
    }

    /// Sets a column's width: its header's border dragged in the window
    /// (bridge 64, spec 2026-09-11 V2).
    ///
    /// Only a column this slot PAINTS: the renderer does not name columns it
    /// did not see. The width is applied in memory and written to
    /// `[ui.columns] spec.width` outside the actor; and since it belongs to
    /// the column and not the slot, EVERY slot's header comes back, which is
    /// what the terminal will also see on its next load.
    pub(super) fn resize_column(
        &mut self,
        slot_id: u32,
        column: &str,
        cells: u16,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if !self.slots.contains_key(&slot_id) || self.oculto(slot_id) {
            return (Self::stale(StaleAction::Generation), Vec::new());
        }
        // Against the FIT, not against what is configured (ADR 0124): a drag
        // arriving after the column was dropped would fix its width, and a
        // fixed width takes it off the ladder forever — the name would go
        // back to being cut off by an old event.
        let painted = self
            .slots
            .get(&slot_id)
            .map(|h| self.setting_of(slot_id, h))
            .unwrap_or_default()
            .iter()
            .any(|f| column_identity(&f.id) == column);
        if !painted {
            return (Self::stale(StaleAction::Generation), Vec::new());
        }
        let cells = self.columns.apply_width(column, cells);
        self.persistir_width(column, cells, mailbox);
        let changes: Vec<ViewChange> = self
            .slots
            .iter()
            .map(|(id, h)| ViewChange::Columns {
                slot_id: *id,
                columns: self.headers(*id, h),
            })
            .collect();
        (self.applied(), vec![self.parche(changes)])
    }

    /// Writes the width outside the actor, like the theme (`persistir_theme`):
    /// `persist_column_width` takes a cross-process lock and doing it here
    /// would freeze the window. Only the failure comes back through the
    /// mailbox.
    fn persistir_width(&mut self, column: &str, cells: u16, mailbox: &mpsc::Sender<Message>) {
        let Some(dir) = self.write_dir() else {
            self.status.message = Some(clamp_display(norte_i18n::t_in(
                self.lang,
                "host-no-config-dir",
            )));
            return;
        };
        let column = column.to_owned();
        let mailbox = mailbox.clone();
        tokio::task::spawn_blocking(move || {
            let key = match norte_config::persist_column_width(&dir, &column, cells) {
                Ok(_) => None,
                Err(e) => Some(io_key(&e)),
            };
            let _ = mailbox.blocking_send(Message::WidthPersistido(key));
        });
    }

    /// A single listing's projection.
    /// A listing's HEADER fields, derived ONCE.
    ///
    /// Read by the snapshot ([`Self::browser`]) and the patch
    /// ([`Self::header_of`]). Two derivations of the same fact is where half
    /// a parity audit came from, so there is a single one here.
    pub(super) fn header_of(&self, id: u32, target_slot: &Slot) -> crate::dto::ViewChange {
        // With the SAME reinterpretation as the rows: painting the header
        // with the raw bytes while the rows go transcoded leaves
        // `pane.names-encoding` half-done — the mojibake stays up top and the
        // reader cannot tell whether the command did anything (#57, #293).
        let (path, hostile) = norte_frontend::path_display_with(
            target_slot.pane.dir(),
            target_slot.pane.name_encoding(),
        );
        // One pass over the marks for the three things that count them.
        let marks = target_slot.pane.marks_summary(crate::dto::MARK_RULER_SPANS);
        crate::dto::ViewChange::BrowserHeader {
            slot_id: id,
            path_display: clamp_display(path),
            path_hostile: hostile,
            // The six of them are DRAFTED by the shared crate, which is also
            // where the terminal gets them. Here they used to be written by
            // hand and had already diverged: the omitted-count one used a
            // different key, without the ⚠ that makes it read as a warning,
            // and it also came out with zero — i.e. it announced an
            // incomplete listing that was complete, spending the only signal
            // there is for when something really is missing.
            hidden_note: clamp_display(norte_frontend::notes::hidden(
                target_slot.pane.hidden_count(),
                self.lang,
            )),
            skipped_note: clamp_display(norte_frontend::notes::skipped(
                target_slot.pane.skipped(),
                self.lang,
            )),
            names_note: clamp_display(norte_frontend::notes::names_encoding(
                target_slot.pane.name_encoding(),
                self.lang,
            )),
            filling_note: clamp_display(norte_frontend::notes::filling(
                target_slot.pane.loading(),
                target_slot.pane.entries().len(),
                self.lang,
            )),
            pruned_note: clamp_display(norte_frontend::notes::pruned_marks(
                target_slot.pane.pruned_marks(),
                self.lang,
            )),
            marked_note: clamp_display(norte_frontend::notes::marked(
                target_slot.pane.marks_len(),
                marks.bytes,
                marks.dirs,
                self.lang,
            )),
            footer: clamp_display(self.pie_con(target_slot, &marks)),
            path_segments: Self::crumbs_of(target_slot),
            used_ratio: norte_frontend::space::used_ratio_for(
                target_slot.pane.dir(),
                &self.volumes_pie,
            ),
            marks: target_slot.pane.marks_len() as u64,
            mark_ruler: marks.ruler,
        }
    }

    /// The path's breadcrumbs (bridge 65): the root and one segment per
    /// directory, each masked on its own — a segment is a file name and is
    /// treated as such. The root carries the scheme and, if there is one, the
    /// authority, in the same shape as `path_display` (`⟨file⟩`,
    /// `⟨sftp⟩host`).
    fn crumbs_of(target_slot: &Slot) -> Vec<String> {
        let dir = target_slot.pane.dir();
        let root = match dir.authority() {
            Some(a) => format!("⟨{}⟩{}", dir.scheme(), a),
            None => format!("⟨{}⟩", dir.scheme()),
        };
        std::iter::once(clamp_display(root))
            .chain(dir.segments().map(|s| {
                let (text, _hostile) = norte_frontend::display_name(s);
                clamp_display(text)
            }))
            .collect()
    }

    /// A listing's footer (spec 2026-09-10), drafted by the shared crate;
    /// empty with `[ui] pane_footer` off.
    pub(super) fn pie_de(&self, target_slot: &Slot) -> String {
        if !self.config.common.ui_chrome.pane_footer() {
            return String::new();
        }
        self.pie_con(target_slot, &target_slot.pane.marks_summary(0))
    }

    /// The footer with the marks already summarized: the header requests it
    /// together with its own summary and does not need to walk the listing
    /// again.
    fn pie_con(&self, target_slot: &Slot, marks: &norte_frontend::MarksSummary) -> String {
        if !self.config.common.ui_chrome.pane_footer() {
            return String::new();
        }
        let counts = norte_frontend::footer::counts(
            target_slot.pane.entries(),
            target_slot.pane.is_parent_row(0),
        );
        let marked = norte_frontend::footer::Marked {
            n: target_slot.pane.marks_len(),
            bytes: marks.bytes,
            dirs: marks.dirs,
        };
        let free = norte_frontend::space::free_for(target_slot.pane.dir(), &self.volumes_pie);
        norte_frontend::footer::pane_footer(counts, marked, free, self.lang)
    }

    /// Requests the volumes for the footer, if the footer is on and there is
    /// no request already in flight. Called on a listing landing: that is
    /// when the panel may have changed volume.
    pub(super) fn request_footer_volumes(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) {
        if !self.config.common.ui_chrome.pane_footer() || self.footer_in_flight {
            return;
        }
        self.footer_in_flight = true;
        let backend = Arc::clone(backend);
        let mailbox = mailbox.clone();
        tokio::spawn(async move {
            let res = match tokio::time::timeout(DEADLINE_PLUGINS, backend.volumes()).await {
                Ok(r) => r,
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            let _ = mailbox
                .send(Message::Background(Box::new(Background::FooterVolumes(
                    res,
                ))))
                .await;
        });
    }

    /// The footer's volumes arrived: they are cached and, if some listing's
    /// footer changes with them, its header travels again. A failure leaves
    /// the cache as it was: the footer stays quiet about the space rather
    /// than making it up.
    pub(super) fn apply_footer_volumes(
        &mut self,
        res: Result<Vec<norte_proto::methods::Volume>, Error>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        self.footer_in_flight = false;
        let vols = res.ok()?;
        let before: Vec<(u32, String)> = self
            .slots
            .iter()
            .map(|(id, h)| (*id, self.pie_de(h)))
            .collect();
        self.volumes_pie = vols;
        let changes: Vec<ViewChange> = before
            .into_iter()
            .filter_map(|(id, old)| {
                let h = self.slots.get(&id)?;
                (self.pie_de(h) != old).then(|| self.header_of(id, h))
            })
            .collect();
        (!changes.is_empty()).then(|| self.parche(changes))
    }

    pub(super) fn browser(&self, id: u32, target_slot: &Slot) -> BrowserSlotView {
        // NO `..`: the snapshot has to carry the same as the patch, and a
        // wildcard here is exactly how a new header field is left out of the
        // first paint with nothing complaining about it.
        let crate::dto::ViewChange::BrowserHeader {
            slot_id: _,
            path_display,
            path_hostile,
            hidden_note,
            skipped_note,
            names_note,
            filling_note,
            pruned_note,
            marked_note,
            footer,
            path_segments,
            used_ratio,
            marks,
            mark_ruler,
        } = self.header_of(id, target_slot)
        else {
            unreachable!("`cabecera_de` builds that variant")
        };
        BrowserSlotView {
            slot_id: id,
            generation: target_slot.pane.listing_epoch(),
            progress: self.slot_progress(target_slot),
            path_display,
            path_hostile,
            total_rows: Some(target_slot.pane.entries().len() as u64),
            first_visible: target_slot.first_visible,
            rows: self.rows_of(id, target_slot),
            icon_column: target_slot.pane.any_icon(),
            cursor: (!target_slot.pane.entries().is_empty())
                .then_some(RowKey(target_slot.pane.cursor() as u64)),
            marks,
            mark_ruler,
            hidden_note,
            skipped_note,
            names_note,
            filling_note,
            pruned_note,
            marked_note,
            footer,
            path_segments,
            used_ratio,
            columns: self.headers(id, target_slot),
            state: target_slot.state.clone(),
            quick: target_slot.pane.quick().map(|q| crate::dto::QuickView {
                query: clamp_display(q.query_display()),
                mode: match q.mode() {
                    norte_frontend::nav::Mode::Filter => "filter",
                    norte_frontend::nav::Mode::Jump => "jump",
                }
                .to_owned(),
                matches: q.visible().len() as u64,
            }),
        }
    }
}
