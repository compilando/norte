//! Tabs and splitting slots.
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
    /// The three effects that touch the LAYOUT, together.
    ///
    /// Grouped here and not in `apply_effect` because that method is a
    /// dispatch and grows by families: three arms that do the same thing —
    /// changing the screen's shape — are one arm with three cases.
    pub(super) fn layout_effect(
        &mut self,
        effect: Effect,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match effect {
            Effect::Size(delta) => self.resize(delta, backend, mailbox),
            Effect::Equalize => self.equalize(backend, mailbox),
            Effect::Rotate => self.rotate(backend, mailbox),
            Effect::Split { vertical } => self.split(vertical, backend, mailbox),
            Effect::CloseSlot => self.close_slot(backend, mailbox),
            Effect::ToggleSlot { kind } => self.toggle_slot(kind, backend, mailbox),
            Effect::TabNew => self.tab_new(backend, mailbox),
            Effect::CloseTab => self.close_tab(backend, mailbox),
            Effect::CycleTab { back } => self.cycle_tab(back, backend, mailbox),
            Effect::MoverTab { right } => self.mover_tab(right, backend, mailbox),
            Effect::IrAPestana { n } => self.go_to_tab(n, backend, mailbox),
            _ => self.open_layouts(),
        }
    }

    /// Opens another TAB next to the focused slot.
    ///
    /// The new listing starts in the same directory and keeps the focus, for
    /// the same reason as splitting. `add_tab` wraps the slot in a group if it
    /// was not one yet: there is nothing to decide here.
    pub(super) fn tab_new(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        use norte_frontend::layout::KindId;
        let id = self.new_slot();
        let updated = self.tree.add_tab(
            SlotId(self.focused()),
            &Node::slot(SlotId(id), KindId::browser()),
        );
        self.apply_layout_with(updated, Some(SlotId(id)), backend, mailbox)
    }

    /// Closes the focused tab.
    ///
    /// Without a group it does nothing and SAYS so: closing the whole slot is
    /// a different command, and doing it here "because there were no tabs"
    /// would be closing what nobody asked to close.
    pub(super) fn close_tab(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(updated) = self.tree.close_tab(SlotId(self.focused())) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-no-tabs".to_owned(),
                },
                self.say("host-no-tabs"),
            );
        };
        self.apply_layout(updated, backend, mailbox)
    }

    /// Moves to the next — or previous — tab, WRAPPING around.
    pub(super) fn cycle_tab(
        &mut self,
        back: bool,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let focus = SlotId(self.focused());
        let Some((tabs, active)) = self.tree.tabs_of(focus) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-no-tabs".to_owned(),
                },
                self.say("host-no-tabs"),
            );
        };
        if tabs.is_empty() {
            return (self.applied(), Vec::new());
        }
        let n = isize::try_from(tabs.len()).unwrap_or(1);
        let i = isize::try_from(active).unwrap_or(0);
        let delta = if back { -1 } else { 1 };
        let target = usize::try_from((i + delta).rem_euclid(n)).unwrap_or(0);
        self.activate_tab(focus, target, &tabs, backend, mailbox)
    }

    /// Goes to tab `n` (1-based).
    pub(super) fn go_to_tab(
        &mut self,
        n: usize,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let focus = SlotId(self.focused());
        let Some((tabs, _)) = self.tree.tabs_of(focus) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-no-tabs".to_owned(),
                },
                self.say("host-no-tabs"),
            );
        };
        let i = n.saturating_sub(1);
        if i >= tabs.len() {
            // Asking for the seventh when there are three does not go to the
            // last one: that is not what was asked, and guessing here is
            // changing tabs on its own.
            return (
                ActionAck::Unavailable {
                    reason_key: "host-no-such-tab".to_owned(),
                },
                Vec::new(),
            );
        }
        self.activate_tab(focus, i, &tabs, backend, mailbox)
    }

    /// Brings tab `target` of `focus`'s group to the front.
    ///
    /// And gives it FOCUS: the tab in front is the one being worked on, and
    /// leaving it on the one that was just hidden leaves the keys pointing at
    /// a listing that is not visible.
    pub(super) fn activate_tab(
        &mut self,
        focus: SlotId,
        target: usize,
        tabs: &[SlotId],
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let updated = self.tree.set_active_for(focus, target);
        let active = tabs.get(target).copied();
        self.apply_layout_with(updated, active, backend, mailbox)
    }

    /// Moves the focused tab within its group.
    ///
    /// It does NOT wrap around: a tab that jumps from the end to the
    /// beginning from one press too many is exactly what nobody wanted (the
    /// rule belongs to the shared model, and here it is only used).
    pub(super) fn mover_tab(
        &mut self,
        right: bool,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let focus = SlotId(self.focused());
        if self.tree.tabs_of(focus).is_none() {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-no-tabs".to_owned(),
                },
                self.say("host-no-tabs"),
            );
        }
        let updated = self.tree.move_tab(focus, if right { 1 } else { -1 });
        self.apply_layout_with(updated, Some(focus), backend, mailbox)
    }

    /// A click on a tab: brings it to the front.
    ///
    /// The slot comes from the group itself, so a click against a tree that
    /// already changed does not hit by accident: if that id is no longer in a
    /// group, it is refused.
    pub(super) fn choose_tab(
        &mut self,
        slot_id: u32,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let clicked = SlotId(slot_id);
        let Some((tabs, _)) = self.tree.tabs_of(clicked) else {
            return (Self::stale(StaleAction::Generation), Vec::new());
        };
        let Some(i) = tabs.iter().position(|t| *t == clicked) else {
            return (Self::stale(StaleAction::Generation), Vec::new());
        };
        self.activate_tab(clicked, i, &tabs, backend, mailbox)
    }

    /// The tree's highest slot id, plus one.
    ///
    /// From the TREE and not from `slots`: the auxiliary ones — places,
    /// board, attribute sheet — are not in that map, and reusing an open
    /// one's id would put two things in the same slot.
    pub(super) fn new_slot(&self) -> u32 {
        self.tree
            .slot_ids()
            .into_iter()
            .map(|SlotId(id)| id)
            .max()
            .unwrap_or(0)
            .saturating_add(1)
    }

    /// Splits the focused slot and puts another LISTING next to it.
    ///
    /// The new one starts in the directory it was split from, which is the
    /// least surprising thing: asking for room to work is not going somewhere
    /// else. And focus goes to the newborn, for the same reason.
    pub(super) fn split(
        &mut self,
        vertical: bool,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        use norte_frontend::layout::{Dir, KindId};
        let dir = if vertical {
            Dir::Vertical
        } else {
            Dir::Horizontal
        };
        // Room for TWO, with the same math that decides the layout's
        // collapse: splitting a slot that no longer fits two creates a panel
        // the layout itself hides in the same frame — the `Split` degrades to
        // tabs — with the tree saving it just the same. The TUI refuses at
        // this same spot (ADR 0077: a decision duplicated between frontends
        // diverges silently).
        let room = self
            .split
            .placements
            .iter()
            .find(|(s, _)| s.0 == self.focused())
            .is_none_or(|(_, re)| {
                norte_frontend::layout::has_room_to_split(*re, dir, &KindId::browser(), &self.kinds)
            });
        if !room {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-layout-split-no-room".to_owned(),
                },
                self.say("msg-layout-split-no-room"),
            );
        }
        let id = self.new_slot();
        let updated = self.tree.split_slot(
            SlotId(self.focused()),
            dir,
            &Node::slot(SlotId(id), KindId::browser()),
        );
        // Focus to the newborn, and WITHIN the same call: splitting is
        // asking for room to work in it. In two steps it would be two
        // snapshots, and the first one would show focus where it no longer
        // is.
        self.apply_layout_with(updated, Some(SlotId(id)), backend, mailbox)
    }

    /// Closes the focused slot.
    ///
    /// Unless that leaves the screen with no LISTING: a screen without a
    /// usable listing is not a screen — it is a freeze with borders — and
    /// that is the same rule the shared layout already applies on its own
    /// (#229). Here it is said, instead of leaving a key that does nothing.
    pub(super) fn close_slot(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(updated) = self.tree.close_slot(SlotId(self.focused())) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-layout-last-panel".to_owned(),
                },
                self.say("msg-layout-last-panel"),
            );
        };
        let remaining = updated
            .slot_ids()
            .into_iter()
            .any(|s| es_listing(&updated, s, &self.kinds));
        if !remaining {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-layout-last-panel".to_owned(),
                },
                self.say("msg-layout-last-panel"),
            );
        }
        // Closing a panel is easy to do by accident and hard to undo if you
        // do not know how: whoever does it is left staring at half a screen.
        // It is SAID, with the preset's real shortcut in it — not a hardcoded
        // one — same as the rest of the chrome (ADR 0106).
        let notice = norte_frontend::notes::slot_closed(
            norte_frontend::palette::first_chord("layout.split-h", &self.effective).as_deref(),
            self.lang,
        );
        self.status.message = Some(clamp_display(notice));
        self.apply_layout(updated, backend, mailbox)
    }

    /// Opens — or closes — this kind's auxiliary slot.
    ///
    /// The three this window knows how to PAINT. One that would only paint
    /// gray does not open: `layout.preview` is still not built for that
    /// reason, and the catalogue says so, not an empty slot.
    ///
    /// The borders and sizes are the SAME the TUI uses, and not for symmetry:
    /// they are measured widths — sixteen cells is the places kind's minimum,
    /// eight rows are the board's six plus the frame, thirty is the sheet's
    /// longest label plus its value next to it.
    pub(super) fn toggle_slot(
        &mut self,
        kind: &'static str,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if let Some(id) = self.slot_of_kind(kind) {
            // Hidden behind another tab of its group (phase F): pressing it
            // BRINGS IT TO THE FRONT, it does not close it — for the reader, a
            // panel they cannot see is closed, and closing it would be the
            // one action that cannot be undone just by looking. It is the
            // TUI's rule (#329).
            //
            // From the TREE and not from the layout: a group that did not fit
            // was not placed, and with the layout the tab in front would
            // "reveal" itself without changing anything — the button would
            // never close it again.
            let behind = self
                .tree
                .tabs_of(id)
                .is_some_and(|(t, a)| t.get(a) != Some(&id));
            if behind {
                return self.choose_tab(id.0, backend, mailbox);
            }
            return self.close_slot_of_kind(kind, backend, mailbox);
        }
        self.open_slot_of_kind(kind, backend, mailbox)
    }

    /// The slot of that kind, if the tree has one.
    pub(super) fn slot_of_kind(&self, kind: &str) -> Option<SlotId> {
        self.tree
            .slot_ids()
            .into_iter()
            .find(|s| kind_de(&self.tree, *s).is_some_and(|k| k.as_str() == kind))
    }

    /// Closes the slot of that kind, if it is open.
    ///
    /// HALF of [`Self::toggle_slot`], split off because the process panel
    /// closes ONLY when the board empties (ADR 0115), and reusing the toggle
    /// would reopen it.
    pub(super) fn close_slot_of_kind(
        &mut self,
        kind: &str,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(id) = self.slot_of_kind(kind) else {
            return (self.applied(), Vec::new());
        };
        let Some(updated) = self.tree.close_slot(id) else {
            return (self.applied(), Vec::new());
        };
        // Closing the log LOWERS what the process captures (#326): the
        // ring's level is raised live so it can show more, and it only ever
        // goes up. Without this, a single press of "trace" left the process
        // keeping TRACE in memory for the rest of the session — including
        // `suppaftp`'s log level, which is the only thing stopping an FTP
        // password from showing up in there — with the interface saying
        // "info" and no panel left to see it in. It is what the TUI already
        // does on closing its own.
        // Closing the terminal panel KILLS its shell (#362), and it is the
        // only thing that kills it: the key that opens it does not close it,
        // precisely so that ending a process of the reader's is something
        // asked for by name. The shell's `Drop` handles it; here it is
        // released and its tick is stopped.
        if kind == super::termpanel::KIND {
            self.release_terminal();
        }
        if kind == super::logpanel::KIND {
            if let Some(ring) = &self.log_ring {
                ring.set_level(self.log_panel.level());
            }
            // And the polling turns off: the epoch bumps, so the timer in
            // flight is left to die without rearming.
            self.log_epoch += 1;
        }
        self.apply_layout(updated, backend, mailbox)
    }

    /// Opens the slot of that kind, or leaves it as is if it already exists.
    ///
    /// The other half: the process panel's automatic opening opens WITHOUT
    /// toggling, or it would close the panel when the second task starts.
    pub(super) fn open_slot_of_kind(
        &mut self,
        kind: &'static str,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        use norte_frontend::layout::{Bindings, Edge, Follow, KindId, Size};
        if self.slot_of_kind(kind).is_some() {
            return (self.applied(), Vec::new());
        }
        let id = SlotId(self.new_slot());
        let leaf = match kind {
            // The attribute sheet FOLLOWS the active role: it describes what
            // the cursor points at, and without the binding it would describe
            // the slot it was born in forever. The docked viewer (#291), for
            // the same reason.
            "metadata" | super::preview::KIND => Node::slot_bound(
                id,
                KindId::new(kind),
                Bindings {
                    follows: Some(Follow::Role(RoleId::Active)),
                },
            ),
            _ => Node::slot(id, KindId::new(kind)),
        };
        let (edge, size) = match kind {
            "places" => (Edge::Left, Size::Fixed(16)),
            "processes" => (Edge::Bottom, Size::Fixed(8)),
            // The log at the bottom, and taller than the board: its lines are
            // long, and eight rows of which two are chrome do not leave room
            // to read a trace. It is the same spot the TUI gives it.
            "log" => (Edge::Bottom, Size::Fixed(12)),
            // The tree on the left and with the places bar's width: it is the
            // same gesture — a navigation column next to the listing — and
            // two different widths for the same thing stand out.
            "tree" => (Edge::Left, Size::Fixed(24)),
            // The viewer on the right and at an EQUAL SPLIT with the listing,
            // as the TUI places it: thirty cells do not leave room to read a
            // line.
            super::preview::KIND => (Edge::Right, Size::Weight(1)),
            _ => (Edge::Right, Size::Fixed(30)),
        };
        // Grouped (phase F): a panel that reaches an edge with another panel
        // joins it as a tab, like VS Code.
        let updated = self
            .tree
            .dock_grouped(SlotId(self.focused()), edge, size, &leaf);
        let outgoing = self.apply_layout(updated, backend, mailbox);
        if kind == "tree" {
            // ANCHORING happens only here: it is the only time where the tree
            // hangs from is chosen. While the listing navigates, the panel
            // FOLLOWS it by revealing the branch (`follow_branches`), which
            // preserves what is open; re-anchoring it would close the whole
            // tree every time the reader enters a folder.
            self.seed_branches();
            self.request_branches(backend, mailbox);
        }
        if kind == super::logpanel::KIND {
            // And the log starts polling: the panel promises it FOLLOWS what
            // arrives, and this window only repaints when someone does
            // something. Without this, it said "stuck to the end" over a
            // frozen list.
            self.log_epoch += 1;
            self.log_seen = self
                .log_ring
                .as_ref()
                .map_or(0, norte_config::logring::LogRing::pushed);
            // And the remote half starts from scratch (#328): this opening
            // requests "whatever there is" and does not drag along either the
            // cursor or the previous opening's lines. Reopening the panel
            // shows the CURRENT log; the old history has already been read,
            // and putting it ahead of the new one would be starting the list
            // where nobody is looking.
            self.log_remote.restart();
            self.sondear_log(mailbox);
            self.request_log_remote(backend, mailbox);
        }
        outgoing
    }
}
