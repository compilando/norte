//! Request a listing, land it, and refresh what the operation touched.
//!
//! Part of `controller`: these are `State` methods, moved here without
//! touching them (ADR 0086). The only writer is still the actor.

// These modules are the same `impl State` split into pieces, so they use
// the same imports as the parent. Enumerating them here would be a
// forty-line list per file, in 32 files, that goes stale the moment the
// parent imports something — `super::*` tracks it on its own.
#[allow(clippy::wildcard_imports)]
use super::*;

impl State {
    /// Asks what a slot's directory accepts: how it folds names
    /// (#268) and whether it refuses writes.
    ///
    /// Requested on LANDING and not in front of every dialog: doing it on
    /// copy would put a daemon round trip on the path of F5, the most-pressed
    /// key of an orthodox manager. Here it rides behind a listing that
    /// already cost a round trip, and the answer serves every copy that
    /// leaves that directory.
    ///
    /// And it travels WHOLE. Distilling it to a `FoldMode` here is what left
    /// the window's help declaring `source_read_only: false` everywhere: the
    /// answer to that question had already been requested and was being
    /// thrown a field away.
    ///
    /// A failure says nothing and breaks nothing: with no answer, nothing
    /// folds and nothing dims — exactly what used to happen before.
    pub(super) fn request_capabilities(
        &mut self,
        slot: u32,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) {
        let Some(target_slot) = self.slots.get(&slot) else {
            return;
        };
        let dir = target_slot.pane.dir().clone();
        // The old ones are NOT deleted: they are tied to their path, so
        // another place's are simply no longer read, and this place's are
        // still good. Deleting them here left the help reading "unknown" on
        // every re-listing, because landing re-freezes it right after this
        // call.
        let backend = Arc::clone(backend);
        let mailbox = mailbox.clone();
        tokio::spawn(async move {
            let Ok(caps) = backend.capabilities(dir.clone()).await else {
                return;
            };
            let _ = mailbox.send(Message::Capabilities(slot, dir, caps)).await;
        });
    }

    /// Saves what a location accepts, if the slot is still where it was.
    ///
    /// The directory check is not paranoia: a whole navigation fits between
    /// asking and answering, and saving another place's capabilities would
    /// make the batch check lie in the permissive direction — and make the
    /// help dim, or stop dimming, for a place the reader is no longer at.
    /// And it RE-FREEZES the help facts, which is what makes the answer
    /// visible: the help freezes them on open (#262), so one opened before
    /// they arrived would keep offering, for its whole life, writes this
    /// place refuses. `None` = there was nothing to say.
    pub(super) fn apply_capabilities(
        &mut self,
        slot: u32,
        dir: &VPath,
        caps: norte_proto::Capabilities,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let h = self.slots.get_mut(&slot)?;
        if h.pane.dir() != dir {
            return None;
        }
        h.caps = Some((dir.clone(), caps));
        self.refreeze_help()
    }

    /// What a slot's location accepts, if it is known and is of THAT place.
    ///
    /// The path check is what makes an answer that arrives late, or that
    /// survives a `cd`, harmless: another directory's capabilities are not
    /// stale data, they are data about something else.
    fn caps_of(&self, slot: u32) -> Option<norte_proto::Capabilities> {
        let h = self.slots.get(&slot)?;
        let (dir, caps) = h.caps.as_ref()?;
        (dir == h.pane.dir()).then_some(*caps)
    }

    /// "Loading", saying WHERE TO.
    ///
    /// `None` = a refresh: the place already open is being reloaded, so
    /// there is no destination to announce. With a destination, the renderer
    /// can say "going here" next to the spinner, which is what keeps the
    /// body's still-showing PREVIOUS listing legible in the meantime.
    pub(super) fn loading_toward(
        dest: Option<&VPath>,
        enc: Option<norte_encoding::NameEncoding>,
    ) -> SlotState {
        // The VERB comes from the shared closed vocabulary, which exists
        // since #323 for this exact thing: the window used to say
        // "loading…" even for a remote connection, which is the case that
        // exposed this and which the terminal names "connecting…". A
        // destination with an authority is a remote to reach; everything
        // else, a listing.
        let kind = dest.map_or(norte_frontend::busy::BusyKind::Listing, |d| {
            if d.authority().is_some() {
                norte_frontend::busy::BusyKind::Connecting
            } else {
                norte_frontend::busy::BusyKind::Listing
            }
        });
        let (target_display, target_hostile) = dest.map_or_else(
            || (String::new(), false),
            |d| {
                let (t, h) = norte_frontend::path_display_with(d, enc);
                (clamp_display(t), h)
            },
        );
        SlotState::Loading {
            verb_key: kind.key().to_owned(),
            target_display,
            target_hostile,
        }
    }

    /// What is known about a PATH, whichever slot holds it.
    ///
    /// By location and not by slot because whoever asks is not always
    /// talking about a slot: a transfer's destination can be a directory the
    /// reader picked on the desktop (#284). What makes the answer valid is
    /// that it is FOR that path, and the stored value already carries that.
    pub(super) fn path_caps(&self, dir: &VPath) -> Option<norte_proto::Capabilities> {
        self.slots
            .values()
            .filter_map(|h| h.caps.as_ref())
            .find(|(p, _)| p == dir)
            .map(|(_, c)| *c)
    }

    /// Whether a slot's location REFUSES to be written to.
    ///
    /// The pair of answers — the flag if known, the scheme if not — is
    /// decided by the SHARED spot, the same one that answers
    /// `norte_tui::app::App::pane_read_only`: writing it here again is how a
    /// decision drifts apart without anyone noticing (ADR 0077).
    ///
    /// A slot that does not exist blocks nothing: that is the permissive
    /// answer, and whoever asks about a destination that is not there will
    /// find it rejected by its name (`host-no-other-slot`).
    pub(super) fn solo_read(&self, slot: u32) -> bool {
        let Some(h) = self.slots.get(&slot) else {
            return false;
        };
        norte_frontend::availability::read_only(self.caps_of(slot), h.pane.dir().scheme())
    }

    /// How a slot's location folds names, if known (#268).
    pub(super) fn fold_of(&self, slot: u32) -> Option<norte_encoding::FoldMode> {
        self.caps_of(slot).map(norte_vfs::fold_mode_of)
    }

    /// A listing requested earlier has just come back.
    ///
    /// `None` = it arrived LATE and another navigation superseded it.
    /// Discarded here, not hidden in the renderer.
    pub(super) fn land_listing(
        &mut self,
        data: ResponseListing,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let (token, slot, dir, res) = data;
        if self.slots.get(&slot).and_then(|h| h.in_flight) != Some(token) {
            return Vec::new();
        }
        // #327: the entry declares `secret = "prompt"` and none of the three
        // sources has it. It is ASKED instead of painting the error, which
        // is all this window used to know how to do: the text of
        // `err-secret-needed` names an environment variable and that is
        // where the road ended.
        //
        // The slot's state is left as any other error would leave it — on
        // purpose: if the dialog is closed without answering, what remains
        // behind is the screen that already knew how to explain itself.
        if let Err(Error::SecretNeeded { conn, endpoint }) = &res {
            let (conn, endpoint) = (conn.clone(), endpoint.clone());
            self.lands_on(slot, dir.clone(), res);
            // The snapshot BEFORE the dialog: the slot has just changed state
            // and the dialog stacks on top. The other way around, the
            // renderer would see the question over the previous screen.
            let snap = self.snapshot();
            let mut outside = vec![self.over(UiUpdate::Snapshot(Box::new(snap)))];
            outside.extend(self.request_secret(conn, &endpoint, slot, dir));
            return outside;
        }
        // A visit to the frequent list counts when the listing ARRIVES (spec
        // 2026-09-15 D6), same as in the terminal; and a directory that no
        // longer exists also drops out of it, the same way it drops out of
        // history.
        let pending = self
            .slots
            .get_mut(&slot)
            .and_then(|h| h.visita_pending.take());
        match &res {
            Ok(_) => {
                if let Some(visited) = pending {
                    self.popular.visit(&visited);
                }
            }
            Err(Error::NotFound) => self.popular.remove(&dir),
            Err(_) => {}
        }
        self.lands_on(slot, dir, res);
        self.request_capabilities(slot, backend, mailbox);
        self.probe(slot, backend, mailbox);
        self.adornar(slot, backend, mailbox);
        // And the footer's free space: this listing can be on another
        // volume (spec 2026-09-10).
        self.request_footer_volumes(backend, mailbox);
        // And the tree, if there is one: this listing is where the pane is
        // now looking, and the neighboring pane has to say the same thing.
        self.follow_branches(slot, backend, mailbox);
        // The help facts describe the entry under the CURSOR, and this
        // listing is a different thing (#262). The snapshot below already
        // carries it re-frozen, so no patch is built here: it would spend a
        // sequence number nobody would receive.
        self.refreeze_help_facts();
        // A `cd` changes the whole screen — directory, rows, cursor,
        // marks — so a snapshot is sent instead of enumerating patches the
        // renderer would have to reconcile.
        let snap = self.snapshot();
        vec![self.over(UiUpdate::Snapshot(Box::new(snap)))]
    }

    /// What a probe found out, applied; and the next batch is requested.
    ///
    /// `MAX_PROBES` bounds each ROUND, not the window: without asking
    /// again, a window taller than one batch would stay half-silent.
    pub(super) fn land_probes(
        &mut self,
        data: Probes,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let (dir, slot, probes) = data;
        let u = self.apply_probes(slot, &dir, &probes)?;
        self.probe(slot, backend, mailbox);
        self.adornar(slot, backend, mailbox);
        Some(u)
    }

    /// What the plugins said, attached to the slot that asked for it.
    ///
    /// `None` if the slot disappeared or if the listing is a DIFFERENT one:
    /// pasting one directory's badges onto another's rows is exactly the
    /// failure the keying-by-PATH avoids, and the directory is still
    /// checked — two different directories' paths never match, but spending
    /// a whole patch to paint nothing can still be avoided.
    pub(super) fn apply_adornos(
        &mut self,
        data: Adornos,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let (generation, slot, dir, adornos, cells, labels) = data;
        // The labels belong to the PLUGIN, not to the slot: they hold for
        // everyone's headers, and they survive even when this batch is
        // discarded as stale — a column's name does not expire with a
        // listing. They live in the SHARED model, the one that resolves a
        // column's style for both frontends.
        let change_headers = self.columns.apply_plugin_headers(labels);
        let target_slot = self.slots.get_mut(&slot)?;
        target_slot.decorating = false;
        if generation != target_slot.gen_adornos {
            // Requested BEFORE the decorations were forgotten — a plugin
            // turned off, a setting changed — it describes what there was,
            // not what there is. It is dropped, and what was left unasked is
            // requested again.
            self.adornar(slot, backend, mailbox);
            return None;
        }
        if *target_slot.pane.dir() != dir {
            return None;
        }
        if adornos.is_empty() && cells.is_empty() {
            // No decorator consented and no plugin column. Not a failure and
            // repaints nothing — unless new labels arrived, which only move
            // the headers.
            return change_headers.then(|| {
                let changes = self.headers_of_all();
                self.parche(changes)
            });
        }
        target_slot.adornos.extend(adornos);
        for (column, values) in cells {
            target_slot
                .cells_plugin
                .entry(column)
                .or_default()
                .extend(values);
        }
        // And to the pane, which is the one that serves them: its setters
        // REPLACE, so the whole accumulated set is passed, not the batch.
        target_slot
            .pane
            .set_decorations(target_slot.adornos.clone());
        target_slot
            .pane
            .set_plugin_columns(target_slot.cells_plugin.clone());
        // The ROWS, which are the only thing that changes: a badge moves
        // neither the cursor nor the directory. With new labels, the headers
        // of ALL slots also travel: a column's name does not belong to one
        // listing.
        let mut changes = vec![self.row_change()];
        if change_headers {
            changes.extend(self.headers_of_all());
        }
        Some(self.parche(changes))
    }

    /// Every listing slot's header, for a patch.
    pub(super) fn headers_of_all(&self) -> Vec<ViewChange> {
        self.slots
            .iter()
            .map(|(id, h)| ViewChange::Columns {
                slot_id: *id,
                columns: self.headers(*id, h),
            })
            .collect()
    }

    /// One more batch of the listing that is draining in the background.
    pub(super) fn land_batch(
        &mut self,
        data: (RequestToken, u32, Vec<Entry>, bool),
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let (token, slot, batch, last) = data;
        let Some(u) = self.apply_batch(slot, token, batch, last) else {
            return Vec::new();
        };
        self.probe(slot, backend, mailbox);
        self.adornar(slot, backend, mailbox);
        // The listing grew from below: if the help is in front, its facts
        // talk about a different entry (#262). There is no snapshot here to
        // drag it along, so it gets its own patch.
        let mut output = vec![u];
        output.extend(self.refreeze_help());
        output
    }

    /// The movement, when focus is on a pane that is not a listing.
    ///
    /// `None` = focus is on a listing, or the effect is not a movement and
    /// follows its normal path. Who takes keys is said by the SHARED kind
    /// registry (`takes_keys`), not a list here: the attribute sheet gets
    /// focus and does NOT take keys on purpose — it follows the listing's
    /// cursor, so with the keyboard inside it would stop following anything —
    /// and that decision is already made in one place.
    pub(super) fn effect_on_focused_pane(
        &mut self,
        effect: Effect,
    ) -> Option<(ActionAck, Vec<BridgeEnvelope<UiUpdate>>)> {
        let SlotId(id) = self.roles.get(RoleId::Active)?;
        if self.slots.contains_key(&id) {
            return None;
        }
        let kind = kind_de(&self.tree, SlotId(id))?;
        if !self.kinds.get(&kind).is_some_and(|d| d.takes_keys) {
            return None;
        }
        if kind.as_str() == "places" {
            return self.places_effect(effect);
        }
        if kind.as_str() == super::timeline::KIND {
            return self.timeline_effect(effect);
        }
        if kind.as_str() != "processes" {
            // Another pane that takes keys and that this host does not yet
            // project: it is let through, and the listing keeps responding.
            // When it is projected, its arm goes in here.
            return None;
        }
        // What is ITS OWN is decided before looking at how many rows there
        // are, and that order is the fix: with an empty board this used to
        // answer "applied" to ANY effect, so the tab key that serves to leave
        // the pane got swallowed by it. Processes was entered and never
        // left — a ring that goes in and does not come out is a trap, and
        // with no mouse there was no way back.
        if !matches!(
            effect,
            Effect::Cursor(_) | Effect::Page(_) | Effect::End { .. }
        ) {
            return None;
        }
        let ids = self.board_ids();
        if ids.is_empty() {
            return Some((self.applied(), Vec::new()));
        }
        let total = i64::try_from(ids.len()).unwrap_or(i64::MAX);
        let paso = |n: i64| -> i64 { n.clamp(-total, total) };
        let actual = i64::try_from(self.cursor_processes.row_or_zero(&ids)).unwrap_or(i64::MAX);
        let delta = match effect {
            Effect::Cursor(n) => paso(n),
            // A page of the processes pane is its rows: no window is
            // declared for it, and jumping more than there is means nothing.
            Effect::Page(n) => paso(n).saturating_mul(total),
            Effect::End { al_final: false } => -actual,
            Effect::End { al_final: true } => total - 1 - actual,
            // The three above are the only ones that reach here: the filter
            // is in the entry guard.
            _ => return None,
        };
        self.cursor_processes.mover(delta, &ids);
        // SNAPSHOT, not a patch. Since bridge 57 the cursor has somewhere to
        // travel (`ViewChange::Tasks`), so this is no longer "there is no
        // contract": it is that a key that only moves the selection does not
        // need to resend the whole board. Changing it is an optimization.
        let snap = self.snapshot();
        Some((
            self.applied(),
            vec![self.over(UiUpdate::Snapshot(Box::new(snap)))],
        ))
    }

    /// Requests the listing for `dir` for `slot`, with the token already
    /// reserved.
    ///
    /// Extracted from navigation so that something else that opens new
    /// slots — a layout change — asks through the SAME path: two ways of
    /// requesting a listing are two places to forget the attribute catalog
    /// or the token.
    pub(super) fn request_listing(
        &mut self,
        slot: u32,
        dir: &VPath,
        token: RequestToken,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) {
        self.request_catalog(dir, backend, mailbox);
        // WHERE it is going stays noted down: what the slot shows does not
        // change until this lands, and until then `pane.dir()` answers for
        // the directory being left behind.
        if let Some(h) = self.slots.get_mut(&slot) {
            h.dir_requested = Some(dir.clone());
        }
        let backend = Arc::clone(backend);
        let mailbox = mailbox.clone();
        let dir = dir.clone();
        let attrs = self.attrs_de(&dir);
        tokio::spawn(async move {
            let stream = backend.list(dir.clone(), attrs).await;
            let res = State::first_page(stream, slot, token, mailbox.clone()).await;
            // If the actor is no longer there, the answer matters to nobody.
            let _ = mailbox
                .send(Message::Listing(Box::new((token, slot, dir, res))))
                .await;
        });
    }

    /// Re-lists the slots this task left out of date, and FORGETS what it
    /// affected: an outcome applies once.
    ///
    /// By directory and not by slot: whoever enqueued the task knew which
    /// directories it touched, not which panes would be looking at them when
    /// it finished — the reader may have navigated, or changed the layout.
    ///
    /// A slot counts as affected by where it is GOING if it has something in
    /// flight, and by what it shows if not: both are "this pane's
    /// directory", and looking only at the second left unrefreshed the pane
    /// that was entering the very place the mutation changed.
    pub(super) fn refresh_affected(
        &mut self,
        task_id: u64,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> Vec<ViewChange> {
        let affected = match self.tasks.get(&task_id) {
            Some(t) if !t.affected.is_empty() => t.affected.clone(),
            _ => return Vec::new(),
        };
        // While ANOTHER task is still alive over the same directory, there
        // is no re-listing: a batch of two hundred copies would produce two
        // hundred listings of the same pane, each invalidating the previous
        // one and paying the probe and the plugin decorations all over
        // again (measured at 167 ms per page of twenty). It refreshes when
        // the LAST one finishes, which is when the directory stops moving.
        let remains_work = self.tasks.iter().any(|(id, t)| {
            *id != task_id
                && !Self::terminal(t.vista.state)
                && t.affected.iter().any(|d| affected.contains(d))
        });
        if remains_work {
            return Vec::new();
        }
        // Consumed: neither this one nor its already-finished siblings ask
        // for it again.
        for t in self.tasks.values_mut() {
            if t.affected.iter().any(|d| affected.contains(d)) {
                t.affected.clear();
            }
        }
        let slots: Vec<(u32, bool)> = self
            .slots
            .iter()
            .filter(|(_, h)| {
                affected.contains(h.dir_requested.as_ref().unwrap_or_else(|| h.pane.dir()))
            })
            .map(|(id, _)| (*id, self.hidden(*id)))
            .collect();
        let mut changes = Vec::new();
        for (slot, hidden) in slots {
            if hidden {
                // A slot that is not visible does not request listings —
                // what is not seen is not fetched — but it also cannot keep
                // believing its listing is still true: it is marked LOADING,
                // which is what `wake_visible` picks up as soon as it
                // comes back to the screen. Without this, a background tab
                // over the destination directory kept showing a listing from
                // before the copy until someone navigated by hand.
                if let Some(h) = self.slots.get_mut(&slot) {
                    h.state = Self::loading_toward(None, None);
                }
                changes.push(ViewChange::SlotState {
                    slot_id: slot,
                    state: Self::loading_toward(None, None),
                });
                continue;
            }
            changes.extend(self.refresh(slot, backend, mailbox));
        }
        changes
    }

    /// Requests one SLOT's listing again, in its SAME directory.
    ///
    /// Not a navigation: it touches neither the trail nor the focus. What it
    /// does do is keep what the reader had set, and both things are by
    /// IDENTITY and not by index:
    ///
    /// - the CURSOR is anchored with `set_pending_focus`, i.e. by path. Per-
    ///   directory memory stores an index, and an index does not survive the
    ///   operation removing or adding an entry: whoever was looking at `e`
    ///   would find the cursor on a different file, without having pressed a
    ///   key, and the next key could be F8.
    /// - the MARKS are set again by path with `restore_marks` (`set_listing`
    ///   clears them, which is correct for a `cd`). What the operation took
    ///   away is not marked again and nothing is invented.
    ///
    /// With something IN FLIGHT it does nothing. It would reserve a new
    /// token, so that navigation's answer would arrive with an old one and
    /// get dropped: the pane would stay in the directory the reader had just
    /// left, saying nothing. Losing a refresh is a slightly stale screen;
    /// losing a navigation is the application moving on its own. And there
    /// is nothing to lose: the listing about to land is newer than the
    /// mutation, or it is going somewhere else.
    pub(super) fn refresh(
        &mut self,
        slot: u32,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> Vec<ViewChange> {
        self.token += 1;
        let token = RequestToken(self.token);
        let Some(target_slot) = self.slots.get_mut(&slot) else {
            return Vec::new();
        };
        if target_slot.in_flight.is_some() {
            return Vec::new();
        }
        if let Some(sel) = target_slot.pane.selected().map(|e| e.path.clone()) {
            target_slot.pane.set_pending_focus(sel);
        }
        target_slot.pane.remember_cursor();
        // `marked_paths` falls back to the cursor when there are no marks,
        // and restoring THAT would turn a refresh into a mark the reader
        // never made.
        target_slot.marks_to_restore = if target_slot.pane.marks_len() > 0 {
            target_slot.pane.marked_paths()
        } else {
            Vec::new()
        };
        let dir = target_slot.pane.dir().clone();
        target_slot.state = Self::loading_toward(None, None);
        target_slot.in_flight = Some(token);
        target_slot.draining = Some(token);
        self.request_listing(slot, &dir, token, backend, mailbox);
        vec![ViewChange::SlotState {
            slot_id: slot,
            state: Self::loading_toward(None, None),
        }]
    }

    /// Requests the listing of ALL visible slots again.
    ///
    /// Of all, not just the focused one, which is what the TUI does and for
    /// the same reason: what changes a listing underneath is a change ON
    /// DISK, and a change on disk does not respect focus. Hidden ones stay
    /// out — what is not seen is not fetched — `wake_visible` already
    /// wakes them when the layout brings them into view.
    pub(super) fn refresh_visible(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let slots: Vec<u32> = self
            .slots
            .keys()
            .copied()
            .filter(|id| !self.hidden(*id))
            .collect();
        let mut changes = Vec::new();
        for slot in slots {
            changes.extend(self.refresh(slot, backend, mailbox));
        }
        if changes.is_empty() {
            // Everyone had something in flight: what is about to land is
            // newer than this key, so there is nothing to say or to paint.
            return (self.applied(), Vec::new());
        }
        (self.applied(), vec![self.parche(changes)])
    }

    /// Sets aside — or brings back — the active pane's hidden entries (#107).
    ///
    /// Presentation-only: the provider does not re-list, the set-aside
    /// entries stay in the model. And it is ANNOUNCED, because a listing
    /// that shrinks without saying why reads as a pane failure.
    pub(super) fn toggle_hidden(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let (visible, pruned) = {
            let target_slot = self.slot_mut();
            let visible = target_slot.pane.toggle_hidden();
            (visible, target_slot.pane.pruned_marks())
        };
        let key = if visible {
            "msg-hidden-shown"
        } else {
            "msg-hidden-hidden"
        };
        let mut phrase = norte_i18n::t_in(self.lang, key);
        if pruned > 0 {
            // Setting the hidden ones aside PRUNES the marks of the ones
            // that leave. The contract of `PaneState::pruned_marks` is that
            // this is never silent: keeping quiet about it would send the
            // next bulk op over fewer files than the reader marked, while
            // they believe all of them are going.
            phrase.push_str(", ");
            phrase.push_str(&norte_i18n::ta_in(
                self.lang,
                "status-marks-pruned",
                &[("n", &pruned.to_string())],
            ));
        }
        self.status.message = Some(clamp_display(phrase));
        let rows = self.parche_rows();
        let change = ViewChange::Status(self.status.clone());
        (self.applied(), vec![rows, self.parche(vec![change])])
    }

    /// Cycles the reinterpretation of names that are not UTF-8 (#57).
    ///
    /// Display-only (rule 1): what changes is how the bytes are PAINTED, not
    /// the bytes. That is why row keys stay valid and only the visible rows
    /// travel.
    pub(super) fn cycle_encoding(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let label = self.slot_mut().pane.cycle_name_encoding();
        let phrase = match label {
            Some(enc) => norte_i18n::ta_in(self.lang, "msg-names-encoding", &[("enc", enc)]),
            None => norte_i18n::t_in(self.lang, "msg-names-encoding-off"),
        };
        self.status.message = Some(clamp_display(phrase));
        let rows = self.parche_rows();
        let change = ViewChange::Status(self.status.clone());
        (self.applied(), vec![rows, self.parche(vec![change])])
    }
}
