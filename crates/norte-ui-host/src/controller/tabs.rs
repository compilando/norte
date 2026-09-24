//! Tabs and splitting slots.
//!
//! Part of `controller`: these are methods of `Estado`, moved here without
//! touching them (ADR 0086). The only writer is still the actor.

// These modules are the same `impl Estado` split into pieces, so they use
// the same imports as the parent. Listing them here would be a forty-line
// list per file, across 32 files, that goes out of sync the moment the
// parent imports something — `super::*` keeps it in sync on its own.
#[allow(clippy::wildcard_imports)]
use super::*;

impl Estado {
    /// The three effects that touch the LAYOUT, together.
    ///
    /// Grouped here and not in `aplicar_efecto` because that method is a
    /// dispatch and grows by families: three arms that do the same thing —
    /// changing the screen's shape — are one arm with three cases.
    pub(super) fn efecto_de_disposicion(
        &mut self,
        efecto: Efecto,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match efecto {
            Efecto::Tamano(delta) => self.redimensionar(delta, backend, mailbox),
            Efecto::Igualar => self.igualar(backend, mailbox),
            Efecto::Girar => self.girar(backend, mailbox),
            Efecto::Partir { vertical } => self.partir(vertical, backend, mailbox),
            Efecto::CerrarHueco => self.cerrar_hueco(backend, mailbox),
            Efecto::AlternarHueco { kind } => self.alternar_hueco(kind, backend, mailbox),
            Efecto::PestanaNueva => self.pestana_nueva(backend, mailbox),
            Efecto::CerrarPestana => self.cerrar_pestana(backend, mailbox),
            Efecto::CiclarPestana { atras } => self.ciclar_pestana(atras, backend, mailbox),
            Efecto::MoverPestana { derecha } => self.mover_pestana(derecha, backend, mailbox),
            Efecto::IrAPestana { n } => self.ir_a_pestana(n, backend, mailbox),
            _ => self.abrir_disposiciones(),
        }
    }

    /// Opens another TAB next to the focused slot.
    ///
    /// The new listing starts in the same directory and keeps the focus, for
    /// the same reason as splitting. `add_tab` wraps the slot in a group if it
    /// was not one yet: there is nothing to decide here.
    pub(super) fn pestana_nueva(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        use norte_frontend::layout::KindId;
        let id = self.nuevo_slot();
        let updated = self.arbol.add_tab(
            SlotId(self.enfocado()),
            &Node::slot(SlotId(id), KindId::browser()),
        );
        self.aplicar_disposicion_con(updated, Some(SlotId(id)), backend, mailbox)
    }

    /// Closes the focused tab.
    ///
    /// Without a group it does nothing and SAYS so: closing the whole slot is
    /// a different command, and doing it here "because there were no tabs"
    /// would be closing what nobody asked to close.
    pub(super) fn cerrar_pestana(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(updated) = self.arbol.close_tab(SlotId(self.enfocado())) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-no-tabs".to_owned(),
                },
                self.decir("host-no-tabs"),
            );
        };
        self.aplicar_disposicion(updated, backend, mailbox)
    }

    /// Moves to the next — or previous — tab, WRAPPING around.
    pub(super) fn ciclar_pestana(
        &mut self,
        atras: bool,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let focus = SlotId(self.enfocado());
        let Some((tabs, active)) = self.arbol.tabs_of(focus) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-no-tabs".to_owned(),
                },
                self.decir("host-no-tabs"),
            );
        };
        if tabs.is_empty() {
            return (self.aplicada(), Vec::new());
        }
        let n = isize::try_from(tabs.len()).unwrap_or(1);
        let i = isize::try_from(active).unwrap_or(0);
        let delta = if atras { -1 } else { 1 };
        let target = usize::try_from((i + delta).rem_euclid(n)).unwrap_or(0);
        self.activar_pestana(focus, target, &tabs, backend, mailbox)
    }

    /// Goes to tab `n` (1-based).
    pub(super) fn ir_a_pestana(
        &mut self,
        n: usize,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let focus = SlotId(self.enfocado());
        let Some((tabs, _)) = self.arbol.tabs_of(focus) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-no-tabs".to_owned(),
                },
                self.decir("host-no-tabs"),
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
        self.activar_pestana(focus, i, &tabs, backend, mailbox)
    }

    /// Brings tab `target` of `focus`'s group to the front.
    ///
    /// And gives it FOCUS: the tab in front is the one being worked on, and
    /// leaving it on the one that was just hidden leaves the keys pointing at
    /// a listing that is not visible.
    pub(super) fn activar_pestana(
        &mut self,
        focus: SlotId,
        target: usize,
        tabs: &[SlotId],
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let updated = self.arbol.set_active_for(focus, target);
        let active = tabs.get(target).copied();
        self.aplicar_disposicion_con(updated, active, backend, mailbox)
    }

    /// Moves the focused tab within its group.
    ///
    /// It does NOT wrap around: a tab that jumps from the end to the
    /// beginning from one press too many is exactly what nobody wanted (the
    /// rule belongs to the shared model, and here it is only used).
    pub(super) fn mover_pestana(
        &mut self,
        derecha: bool,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let focus = SlotId(self.enfocado());
        if self.arbol.tabs_of(focus).is_none() {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-no-tabs".to_owned(),
                },
                self.decir("host-no-tabs"),
            );
        }
        let updated = self.arbol.move_tab(focus, if derecha { 1 } else { -1 });
        self.aplicar_disposicion_con(updated, Some(focus), backend, mailbox)
    }

    /// A click on a tab: brings it to the front.
    ///
    /// The slot comes from the group itself, so a click against a tree that
    /// already changed does not hit by accident: if that id is no longer in a
    /// group, it is refused.
    pub(super) fn elegir_pestana(
        &mut self,
        slot_id: u32,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let clicked = SlotId(slot_id);
        let Some((tabs, _)) = self.arbol.tabs_of(clicked) else {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        };
        let Some(i) = tabs.iter().position(|t| *t == clicked) else {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        };
        self.activar_pestana(clicked, i, &tabs, backend, mailbox)
    }

    /// The tree's highest slot id, plus one.
    ///
    /// From the TREE and not from `huecos`: the auxiliary ones — places,
    /// board, attribute sheet — are not in that map, and reusing an open
    /// one's id would put two things in the same slot.
    pub(super) fn nuevo_slot(&self) -> u32 {
        self.arbol
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
    pub(super) fn partir(
        &mut self,
        vertical: bool,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
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
            .reparto
            .placements
            .iter()
            .find(|(s, _)| s.0 == self.enfocado())
            .is_none_or(|(_, re)| {
                norte_frontend::layout::has_room_to_split(*re, dir, &KindId::browser(), &self.kinds)
            });
        if !room {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-layout-split-no-room".to_owned(),
                },
                self.decir("msg-layout-split-no-room"),
            );
        }
        let id = self.nuevo_slot();
        let updated = self.arbol.split_slot(
            SlotId(self.enfocado()),
            dir,
            &Node::slot(SlotId(id), KindId::browser()),
        );
        // Focus to the newborn, and WITHIN the same call: splitting is
        // asking for room to work in it. In two steps it would be two
        // snapshots, and the first one would show focus where it no longer
        // is.
        self.aplicar_disposicion_con(updated, Some(SlotId(id)), backend, mailbox)
    }

    /// Closes the focused slot.
    ///
    /// Unless that leaves the screen with no LISTING: a screen without a
    /// usable listing is not a screen — it is a freeze with borders — and
    /// that is the same rule the shared layout already applies on its own
    /// (#229). Here it is said, instead of leaving a key that does nothing.
    pub(super) fn cerrar_hueco(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(updated) = self.arbol.close_slot(SlotId(self.enfocado())) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-layout-last-panel".to_owned(),
                },
                self.decir("msg-layout-last-panel"),
            );
        };
        let remaining = updated
            .slot_ids()
            .into_iter()
            .any(|s| es_listado(&updated, s, &self.kinds));
        if !remaining {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-layout-last-panel".to_owned(),
                },
                self.decir("msg-layout-last-panel"),
            );
        }
        // Closing a panel is easy to do by accident and hard to undo if you
        // do not know how: whoever does it is left staring at half a screen.
        // It is SAID, with the preset's real shortcut in it — not a hardcoded
        // one — same as the rest of the chrome (ADR 0106).
        let notice = norte_frontend::notes::slot_closed(
            norte_frontend::palette::first_chord("layout.split-h", &self.efectivo).as_deref(),
            self.lang,
        );
        self.status.message = Some(clamp_display(notice));
        self.aplicar_disposicion(updated, backend, mailbox)
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
    pub(super) fn alternar_hueco(
        &mut self,
        kind: &'static str,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if let Some(id) = self.hueco_de_kind(kind) {
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
                .arbol
                .tabs_of(id)
                .is_some_and(|(t, a)| t.get(a) != Some(&id));
            if behind {
                return self.elegir_pestana(id.0, backend, mailbox);
            }
            return self.cerrar_hueco_de_kind(kind, backend, mailbox);
        }
        self.abrir_hueco_de_kind(kind, backend, mailbox)
    }

    /// The slot of that kind, if the tree has one.
    pub(super) fn hueco_de_kind(&self, kind: &str) -> Option<SlotId> {
        self.arbol
            .slot_ids()
            .into_iter()
            .find(|s| kind_de(&self.arbol, *s).is_some_and(|k| k.as_str() == kind))
    }

    /// Closes the slot of that kind, if it is open.
    ///
    /// HALF of [`Self::alternar_hueco`], split off because the process panel
    /// closes ONLY when the board empties (ADR 0115), and reusing the toggle
    /// would reopen it.
    pub(super) fn cerrar_hueco_de_kind(
        &mut self,
        kind: &str,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(id) = self.hueco_de_kind(kind) else {
            return (self.aplicada(), Vec::new());
        };
        let Some(updated) = self.arbol.close_slot(id) else {
            return (self.aplicada(), Vec::new());
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
            self.soltar_terminal();
        }
        if kind == super::logpanel::KIND {
            if let Some(ring) = &self.log_ring {
                ring.set_level(self.log_panel.level());
            }
            // And the polling turns off: the epoch bumps, so the timer in
            // flight is left to die without rearming.
            self.log_epoca += 1;
        }
        self.aplicar_disposicion(updated, backend, mailbox)
    }

    /// Opens the slot of that kind, or leaves it as is if it already exists.
    ///
    /// The other half: the process panel's automatic opening opens WITHOUT
    /// toggling, or it would close the panel when the second task starts.
    pub(super) fn abrir_hueco_de_kind(
        &mut self,
        kind: &'static str,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        use norte_frontend::layout::{Bindings, Edge, Follow, KindId, Size};
        if self.hueco_de_kind(kind).is_some() {
            return (self.aplicada(), Vec::new());
        }
        let id = SlotId(self.nuevo_slot());
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
            .arbol
            .dock_grouped(SlotId(self.enfocado()), edge, size, &leaf);
        let outgoing = self.aplicar_disposicion(updated, backend, mailbox);
        if kind == "tree" {
            // ANCHORING happens only here: it is the only time where the tree
            // hangs from is chosen. While the listing navigates, the panel
            // FOLLOWS it by revealing the branch (`seguir_ramas`), which
            // preserves what is open; re-anchoring it would close the whole
            // tree every time the reader enters a folder.
            self.sembrar_ramas();
            self.pedir_ramas(backend, mailbox);
        }
        if kind == super::logpanel::KIND {
            // And the log starts polling: the panel promises it FOLLOWS what
            // arrives, and this window only repaints when someone does
            // something. Without this, it said "stuck to the end" over a
            // frozen list.
            self.log_epoca += 1;
            self.log_visto = self
                .log_ring
                .as_ref()
                .map_or(0, norte_config::logring::LogRing::pushed);
            // And the remote half starts from scratch (#328): this opening
            // requests "whatever there is" and does not drag along either the
            // cursor or the previous opening's lines. Reopening the panel
            // shows the CURRENT log; the old history has already been read,
            // and putting it ahead of the new one would be starting the list
            // where nobody is looking.
            self.log_remoto.reiniciar();
            self.sondear_registro(mailbox);
            self.pedir_registro_remoto(backend, mailbox);
        }
        outgoing
    }
}
