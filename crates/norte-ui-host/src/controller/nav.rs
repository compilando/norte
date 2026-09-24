//! Navigating: entering, going up, and walking the trail.
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
    /// The three actions that CHANGE directory.
    ///
    /// Apart from the ones above because they are the only ones that leave
    /// work in flight: the rest end inside this function.
    pub(super) fn navegacion(
        &mut self,
        action: &UiAction,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match action {
            UiAction::Activate {
                slot_id,
                key,
                generation,
            } => {
                let (slot_id, key, generation) = (*slot_id, *key, *generation);
                // Activating a row of ANOTHER panel focuses it first. The
                // renderer sends `focus_slot` on click and then this action,
                // so it is almost always already the active one; but if that
                // focus was not applied — the layout changed, the slot was
                // not in the walk yet — `fila_de` used to silently refuse
                // this and a double click on the panel next door did
                // NOTHING. An action that names its slot cannot depend on
                // another one having arrived first.
                self.enfocar_para_actuar(slot_id);
                let Some(i) = self.fila_de(slot_id, key, generation) else {
                    return (Self::obsoleta(StaleAction::Generation), Vec::new());
                };
                let Some(entry) = self.hueco().pane.entries().get(i) else {
                    return (Self::obsoleta(StaleAction::Generation), Vec::new());
                };
                // What can be navigated is said by the SHARED crate: a
                // directory, a link, and a CONTAINER, which opens from the
                // inside. Here it used to check `kind != Dir`, so a `.zip`
                // and a symlink were handed to the desktop while the
                // terminal entered them — with a comment, three lines below,
                // claiming the decision was the same.
                let navigable = norte_frontend::nav::enter_target(entry);
                if navigable.is_none() {
                    // A FILE opens, which is what an orthodox file manager
                    // does: with whatever program the desktop associates if
                    // it is on this disk, and with the INTERNAL viewer if
                    // not — you cannot hand `xdg-open` an `sftp://`, and
                    // there the viewer is the only thing that can be done.
                    // The same decision the TUI makes, and now for real: it
                    // comes from the same function (ADR 0077).
                    return if norte_frontend::shell::is_local(&entry.path) {
                        self.abrir_externo()
                    } else {
                        self.pedir_visor(backend, mailbox)
                    };
                }
                let target = navigable.unwrap_or_else(|| entry.path.clone());
                // Activating the `..` row is GOING UP, and going up lands the
                // cursor on the directory you left — the same thing
                // `UiAction::Parent` does a few lines below. Without this,
                // the same navigation left the cursor on the first row
                // depending on whether it was requested with the row or with
                // the key, and going up and down stopped being reversible
                // through one of the two doors.
                if self.hueco().pane.is_parent_row(i) {
                    let current = self.hueco().pane.dir().clone();
                    self.hueco_mut().pane.set_pending_focus(current);
                }
                (
                    self.aplicada(),
                    self.navegar(&target, Trail::Record, backend, mailbox),
                )
            }
            UiAction::BreadcrumbActivate {
                slot_id,
                depth,
                generation,
            } => self.ir_a_miga(*slot_id, *depth, *generation, backend, mailbox),
            UiAction::Parent { slot_id } => {
                if *slot_id != self.activo() {
                    return (Self::obsoleta(StaleAction::Generation), Vec::new());
                }
                let current = self.hueco().pane.dir().clone();
                let Some(parent) = current.parent() else {
                    return (
                        ActionAck::Unavailable {
                            reason_key: "msg-nav-at-root".to_owned(),
                        },
                        Vec::new(),
                    );
                };
                // The cursor lands on the directory you left, not on the
                // first row: that is what makes going up and down reversible.
                // `PaneState` resolves it on receiving the listing.
                self.hueco_mut().pane.set_pending_focus(current);
                (
                    self.aplicada(),
                    self.navegar(&parent, Trail::Record, backend, mailbox),
                )
            }
            UiAction::History { slot_id, back } => {
                let (slot_id, back) = (*slot_id, *back);
                if slot_id != self.activo() {
                    return (Self::obsoleta(StaleAction::Generation), Vec::new());
                }
                let current = self.hueco().pane.dir().clone();
                let step = if back {
                    TrailStep::Back
                } else {
                    TrailStep::Forward
                };
                let target = if back {
                    self.hueco_mut().historial.step_back(current)
                } else {
                    self.hueco_mut().historial.step_forward(current)
                };
                let Some(target) = target else {
                    // A key that goes mute is not distinguishable from a
                    // broken one: an exhausted trail SAYS so.
                    return (
                        ActionAck::Unavailable {
                            reason_key: step.empty_message().to_owned(),
                        },
                        Vec::new(),
                    );
                };
                (
                    self.aplicada(),
                    self.navegar(&target, Trail::Replay(step), backend, mailbox),
                )
            }
            _ => (Self::obsoleta(StaleAction::Generation), Vec::new()),
        }
    }

    /// Starts a navigation: records the step in the trail, marks the slot as
    /// loading, and leaves the request IN FLIGHT with its token.
    pub(super) fn navegar(
        &mut self,
        target: &VPath,
        trail: Trail,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        self.navegar_hueco(self.activo(), target, trail, backend, mailbox)
    }

    /// The same, on a slot that does NOT have to be the active one.
    ///
    /// It exists because there are gestures that move ANOTHER panel: the
    /// mirror sends the active one's location to the target, and a volume
    /// selector opened for one side of the screen mounts there. This used to
    /// be done by reading `activo()` three times inside, so there was no way
    /// to say it explicitly.
    pub(super) fn navegar_hueco(
        &mut self,
        slot: u32,
        target: &VPath,
        trail: Trail,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        self.token += 1;
        let token = RequestToken(self.token);
        let target = target.clone();
        let cap = self.config.common.ui_chrome.history_size();
        let Some(slot_state) = self.huecos.get_mut(&slot) else {
            return Vec::new();
        };
        let previous = slot_state.pane.dir().clone();
        slot_state.historial.set_capacity(cap);
        // The SAME decision as the terminal's (`counts_as_step`): a `Replay`
        // is the trail replaying itself — recording it would make it
        // oscillate between two directories —, a `Seed` places without
        // walking, and a refresh is not a step. What counts enters the trail
        // RIGHT AWAY; the visit to the popular list waits for the listing to
        // arrive (`aterrizar_listado`), because one that fails is not a
        // place you went to — the terminal only counts on arrival.
        let counts = norte_frontend::history::counts_as_step(&previous, &target, trail);
        if counts {
            slot_state.historial.record(previous);
        }
        slot_state.visita_pendiente = counts.then(|| target.clone());
        // The cursor's memory is taken with the dir being LEFT still set
        // (`remember_cursor`'s contract).
        slot_state.pane.remember_cursor();
        // WITH the target: the body is going to keep showing the previous
        // listing until the new one arrives — on purpose, so a failure
        // leaves the reader where they were —, and without saying where that
        // mix is going, it cannot be read.
        let encoding = slot_state.pane.name_encoding();
        slot_state.estado = Self::cargando_hacia(Some(&target), encoding);
        slot_state.en_vuelo = Some(token);
        // The drain lives LONGER than the first page: it is marked here and
        // only another navigation of the same slot supersedes it.
        slot_state.drenando = Some(token);

        self.pedir_listado(slot, &target, token, backend, mailbox);

        let change = ViewChange::SlotState {
            slot_id: slot,
            state: Self::cargando_hacia(Some(&target), encoding),
        };
        let mut outgoing = vec![self.parche(vec![change])];

        // SYNCHRONIZED navigation (`pane.sync-nav`): the target slot repeats
        // THIS navigation. It goes here, at the single point every one of
        // them passes through — keys, breadcrumbs, trail, volumes — and not
        // in the dispatcher: hung off there, moving through history would
        // not mirror and the mode would half-lie.
        //
        // The echo travels as `Trail::Seed` and only fires if THIS navigation
        // was not one: it is not a reader step — it does not enter their
        // trail — and that is what cuts the recursion without a separate
        // flag. And it only mirrors what comes out of the ACTIVE slot: a
        // listing that places itself does not drag the other one along.
        if self.espejo_permanente
            && !matches!(trail, Trail::Seed)
            && slot == self.activo()
            && let Ok(other) = self.hueco_destino()
            && let Some(other_dir) = self.dir_en_curso(other)
            // `false`: in this window a search's hits do not live in a
            // slot — they have their own view — so no listing can be
            // showing anything other than a location.
            && let Some(echo) = norte_frontend::nav::destino_en_espejo(&target, &other_dir, false)
        {
            outgoing.extend(self.navegar_hueco(other, &echo, Trail::Seed, backend, mailbox));
        }
        outgoing
    }

    /// Focuses `slot_id` if it can, so that an action that NAMES its slot
    /// does not depend on focus having arrived first through another
    /// message.
    ///
    /// The criterion is the same as `UiAction::FocusSlot`'s: the shared focus
    /// walk and the slot being visible. A slot that does not qualify is left
    /// as is, and the caller will refuse it on its own.
    pub(super) fn enfocar_para_actuar(&mut self, slot_id: u32) {
        if slot_id == self.activo()
            || !self.reparto.focus_order.contains(&SlotId(slot_id))
            || self.oculto(slot_id)
        {
            return;
        }
        self.roles.set(RoleId::Active, SlotId(slot_id));
        self.reconcilia_roles();
    }

    /// A clicked breadcrumb (bridge 65): navigates to the ancestor with the
    /// first `depth` segments of the slot's path. By depth and not by name:
    /// the segments travelled masked. The breadcrumb of the CURRENT directory
    /// does not navigate — it is already there — and it answers applied
    /// without moving anything.
    pub(super) fn ir_a_miga(
        &mut self,
        slot_id: u32,
        depth: u32,
        generation: u64,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if !self.huecos.contains_key(&slot_id) || self.oculto(slot_id) {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        }
        let Some(slot_state) = self.huecos.get(&slot_id) else {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        };
        // A breadcrumb from a listing that is no longer there: the depth was
        // talking about a different path. Stale, like a row from another
        // generation.
        if slot_state.pane.listing_epoch() != generation {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        }
        let current = slot_state.pane.dir().clone();
        let depth_count = usize::try_from(depth).unwrap_or(usize::MAX);
        if depth_count >= current.segments().count() {
            return (self.aplicada(), Vec::new());
        }
        // Trim from the back down to the requested depth, with the same
        // operation as `..`: an ancestor is chained parents.
        let mut target = current;
        while target.segments().count() > depth_count {
            let Some(parent) = target.parent() else {
                break;
            };
            target = parent;
        }
        (
            self.aplicada(),
            self.navegar_hueco(slot_id, &target, Trail::Record, backend, mailbox),
        )
    }

    /// A row's index, if the key is of THIS generation and exists.
    pub(super) fn fila_valida(&self, key: RowKey) -> Option<usize> {
        let i = usize::try_from(key.0).ok()?;
        (i < self.hueco().pane.entries().len()).then_some(i)
    }
}
