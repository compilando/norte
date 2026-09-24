//! Request a listing, land it, and refresh what the operation touched.
//!
//! Part of `controller`: these are `Estado` methods, moved here without
//! touching them (ADR 0086). The only writer is still the actor.

// These modules are the same `impl Estado` split into pieces, so they use
// the same imports as the parent. Enumerating them here would be a
// forty-line list per file, in 32 files, that goes stale the moment the
// parent imports something — `super::*` tracks it on its own.
#[allow(clippy::wildcard_imports)]
use super::*;

impl Estado {
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
    pub(super) fn pedir_capacidades(
        &mut self,
        slot: u32,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        let Some(hueco) = self.huecos.get(&slot) else {
            return;
        };
        let dir = hueco.pane.dir().clone();
        // The old ones are NOT deleted: they are tied to their path, so
        // another place's are simply no longer read, and this place's are
        // still good. Deleting them here left the help reading "unknown" on
        // every re-listing, because landing re-freezes it right after this
        // call.
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let Ok(caps) = backend.capabilities(dir.clone()).await else {
                return;
            };
            let _ = buzon.send(Mensaje::Capacidades(slot, dir, caps)).await;
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
    pub(super) fn aplicar_capacidades(
        &mut self,
        slot: u32,
        dir: &VPath,
        caps: norte_proto::Capabilities,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let h = self.huecos.get_mut(&slot)?;
        if h.pane.dir() != dir {
            return None;
        }
        h.caps = Some((dir.clone(), caps));
        self.recongelar_ayuda()
    }

    /// What a slot's location accepts, if it is known and is of THAT place.
    ///
    /// The path check is what makes an answer that arrives late, or that
    /// survives a `cd`, harmless: another directory's capabilities are not
    /// stale data, they are data about something else.
    fn caps_of(&self, slot: u32) -> Option<norte_proto::Capabilities> {
        let h = self.huecos.get(&slot)?;
        let (dir, caps) = h.caps.as_ref()?;
        (dir == h.pane.dir()).then_some(*caps)
    }

    /// "Loading", saying WHERE TO.
    ///
    /// `None` = a refresh: the place already open is being reloaded, so
    /// there is no destination to announce. With a destination, the renderer
    /// can say "going here" next to the spinner, which is what keeps the
    /// body's still-showing PREVIOUS listing legible in the meantime.
    pub(super) fn cargando_hacia(
        destino: Option<&VPath>,
        enc: Option<norte_encoding::NameEncoding>,
    ) -> SlotState {
        // The VERB comes from the shared closed vocabulary, which exists
        // since #323 for this exact thing: the window used to say
        // "loading…" even for a remote connection, which is the case that
        // exposed this and which the terminal names "connecting…". A
        // destination with an authority is a remote to reach; everything
        // else, a listing.
        let kind = destino.map_or(norte_frontend::busy::BusyKind::Listing, |d| {
            if d.authority().is_some() {
                norte_frontend::busy::BusyKind::Connecting
            } else {
                norte_frontend::busy::BusyKind::Listing
            }
        });
        let (target_display, target_hostile) = destino.map_or_else(
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
    pub(super) fn caps_de_ruta(&self, dir: &VPath) -> Option<norte_proto::Capabilities> {
        self.huecos
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
    pub(super) fn solo_lectura(&self, slot: u32) -> bool {
        let Some(h) = self.huecos.get(&slot) else {
            return false;
        };
        norte_frontend::availability::read_only(self.caps_of(slot), h.pane.dir().scheme())
    }

    /// How a slot's location folds names, if known (#268).
    pub(super) fn pliegue_de(&self, slot: u32) -> Option<norte_encoding::FoldMode> {
        self.caps_of(slot).map(norte_vfs::fold_mode_of)
    }

    /// A listing requested earlier has just come back.
    ///
    /// `None` = it arrived LATE and another navigation superseded it.
    /// Discarded here, not hidden in the renderer.
    pub(super) fn aterrizar_listado(
        &mut self,
        datos: RespuestaListado,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let (token, slot, dir, res) = datos;
        if self.huecos.get(&slot).and_then(|h| h.en_vuelo) != Some(token) {
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
            self.aterriza_en(slot, dir.clone(), res);
            // The snapshot BEFORE the dialog: the slot has just changed state
            // and the dialog stacks on top. The other way around, the
            // renderer would see the question over the previous screen.
            let snap = self.snapshot();
            let mut fuera = vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))];
            fuera.extend(self.pedir_secreto(conn, &endpoint, slot, dir));
            return fuera;
        }
        // A visit to the frequent list counts when the listing ARRIVES (spec
        // 2026-09-15 D6), same as in the terminal; and a directory that no
        // longer exists also drops out of it, the same way it drops out of
        // history.
        let pendiente = self
            .huecos
            .get_mut(&slot)
            .and_then(|h| h.visita_pendiente.take());
        match &res {
            Ok(_) => {
                if let Some(visitado) = pendiente {
                    self.popular.visit(&visitado);
                }
            }
            Err(Error::NotFound) => self.popular.remove(&dir),
            Err(_) => {}
        }
        self.aterriza_en(slot, dir, res);
        self.pedir_capacidades(slot, backend, buzon);
        self.sondear(slot, backend, buzon);
        self.adornar(slot, backend, buzon);
        // And the footer's free space: this listing can be on another
        // volume (spec 2026-09-10).
        self.pedir_volumenes_de_pie(backend, buzon);
        // And the tree, if there is one: this listing is where the pane is
        // now looking, and the neighboring pane has to say the same thing.
        self.seguir_ramas(slot, backend, buzon);
        // The help facts describe the entry under the CURSOR, and this
        // listing is a different thing (#262). The snapshot below already
        // carries it re-frozen, so no patch is built here: it would spend a
        // sequence number nobody would receive.
        self.recongelar_hechos_de_ayuda();
        // A `cd` changes the whole screen — directory, rows, cursor,
        // marks — so a snapshot is sent instead of enumerating patches the
        // renderer would have to reconcile.
        let snap = self.snapshot();
        vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))]
    }

    /// What a probe found out, applied; and the next batch is requested.
    ///
    /// `MAX_SONDEOS` bounds each ROUND, not the window: without asking
    /// again, a window taller than one batch would stay half-silent.
    pub(super) fn aterrizar_sondas(
        &mut self,
        datos: Sondas,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let (dir, slot, sondas) = datos;
        let u = self.aplicar_sondas(slot, &dir, &sondas)?;
        self.sondear(slot, backend, buzon);
        self.adornar(slot, backend, buzon);
        Some(u)
    }

    /// What the plugins said, attached to the slot that asked for it.
    ///
    /// `None` if the slot disappeared or if the listing is a DIFFERENT one:
    /// pasting one directory's badges onto another's rows is exactly the
    /// failure the keying-by-PATH avoids, and the directory is still
    /// checked — two different directories' paths never match, but spending
    /// a whole patch to paint nothing can still be avoided.
    pub(super) fn aplicar_adornos(
        &mut self,
        datos: Adornos,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let (generacion, slot, dir, adornos, celdas, rotulos) = datos;
        // The labels belong to the PLUGIN, not to the slot: they hold for
        // everyone's headers, and they survive even when this batch is
        // discarded as stale — a column's name does not expire with a
        // listing. They live in the SHARED model, the one that resolves a
        // column's style for both frontends.
        let cambian_cabeceras = self.columnas.apply_plugin_headers(rotulos);
        let hueco = self.huecos.get_mut(&slot)?;
        hueco.adornando = false;
        if generacion != hueco.gen_adornos {
            // Requested BEFORE the decorations were forgotten — a plugin
            // turned off, a setting changed — it describes what there was,
            // not what there is. It is dropped, and what was left unasked is
            // requested again.
            self.adornar(slot, backend, buzon);
            return None;
        }
        if *hueco.pane.dir() != dir {
            return None;
        }
        if adornos.is_empty() && celdas.is_empty() {
            // No decorator consented and no plugin column. Not a failure and
            // repaints nothing — unless new labels arrived, which only move
            // the headers.
            return cambian_cabeceras.then(|| {
                let cambios = self.cabeceras_de_todos();
                self.parche(cambios)
            });
        }
        hueco.adornos.extend(adornos);
        for (columna, valores) in celdas {
            hueco
                .celdas_plugin
                .entry(columna)
                .or_default()
                .extend(valores);
        }
        // And to the pane, which is the one that serves them: its setters
        // REPLACE, so the whole accumulated set is passed, not the batch.
        hueco.pane.set_decorations(hueco.adornos.clone());
        hueco.pane.set_plugin_columns(hueco.celdas_plugin.clone());
        // The ROWS, which are the only thing that changes: a badge moves
        // neither the cursor nor the directory. With new labels, the headers
        // of ALL slots also travel: a column's name does not belong to one
        // listing.
        let mut cambios = vec![self.cambio_de_filas()];
        if cambian_cabeceras {
            cambios.extend(self.cabeceras_de_todos());
        }
        Some(self.parche(cambios))
    }

    /// Every listing slot's header, for a patch.
    pub(super) fn cabeceras_de_todos(&self) -> Vec<ViewChange> {
        self.huecos
            .iter()
            .map(|(id, h)| ViewChange::Columns {
                slot_id: *id,
                columns: self.cabeceras(*id, h),
            })
            .collect()
    }

    /// One more batch of the listing that is draining in the background.
    pub(super) fn aterrizar_lote(
        &mut self,
        datos: (RequestToken, u32, Vec<Entry>, bool),
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let (token, slot, batch, ultimo) = datos;
        let Some(u) = self.aplicar_lote(slot, token, batch, ultimo) else {
            return Vec::new();
        };
        self.sondear(slot, backend, buzon);
        self.adornar(slot, backend, buzon);
        // The listing grew from below: if the help is in front, its facts
        // talk about a different entry (#262). There is no snapshot here to
        // drag it along, so it gets its own patch.
        let mut salida = vec![u];
        salida.extend(self.recongelar_ayuda());
        salida
    }

    /// The movement, when focus is on a pane that is not a listing.
    ///
    /// `None` = focus is on a listing, or the effect is not a movement and
    /// follows its normal path. Who takes keys is said by the SHARED kind
    /// registry (`takes_keys`), not a list here: the attribute sheet gets
    /// focus and does NOT take keys on purpose — it follows the listing's
    /// cursor, so with the keyboard inside it would stop following anything —
    /// and that decision is already made in one place.
    pub(super) fn efecto_en_panel_enfocado(
        &mut self,
        efecto: Efecto,
    ) -> Option<(ActionAck, Vec<BridgeEnvelope<UiUpdate>>)> {
        let SlotId(id) = self.roles.get(RoleId::Active)?;
        if self.huecos.contains_key(&id) {
            return None;
        }
        let kind = kind_de(&self.arbol, SlotId(id))?;
        if !self.kinds.get(&kind).is_some_and(|d| d.takes_keys) {
            return None;
        }
        if kind.as_str() == "places" {
            return self.efecto_en_sitios(efecto);
        }
        if kind.as_str() == super::timeline::KIND {
            return self.efecto_en_linea(efecto);
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
            efecto,
            Efecto::Cursor(_) | Efecto::Pagina(_) | Efecto::Extremo { .. }
        ) {
            return None;
        }
        let ids = self.ids_del_tablero();
        if ids.is_empty() {
            return Some((self.aplicada(), Vec::new()));
        }
        let total = i64::try_from(ids.len()).unwrap_or(i64::MAX);
        let paso = |n: i64| -> i64 { n.clamp(-total, total) };
        let actual = i64::try_from(self.cursor_procesos.fila_o_cero(&ids)).unwrap_or(i64::MAX);
        let delta = match efecto {
            Efecto::Cursor(n) => paso(n),
            // A page of the processes pane is its rows: no window is
            // declared for it, and jumping more than there is means nothing.
            Efecto::Pagina(n) => paso(n).saturating_mul(total),
            Efecto::Extremo { al_final: false } => -actual,
            Efecto::Extremo { al_final: true } => total - 1 - actual,
            // The three above are the only ones that reach here: the filter
            // is in the entry guard.
            _ => return None,
        };
        self.cursor_procesos.mover(delta, &ids);
        // SNAPSHOT, not a patch. Since bridge 57 the cursor has somewhere to
        // travel (`ViewChange::Tasks`), so this is no longer "there is no
        // contract": it is that a key that only moves the selection does not
        // need to resend the whole board. Changing it is an optimization.
        let snap = self.snapshot();
        Some((
            self.aplicada(),
            vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))],
        ))
    }

    /// Requests the listing for `dir` for `slot`, with the token already
    /// reserved.
    ///
    /// Extracted from navigation so that something else that opens new
    /// slots — a layout change — asks through the SAME path: two ways of
    /// requesting a listing are two places to forget the attribute catalog
    /// or the token.
    pub(super) fn pedir_listado(
        &mut self,
        slot: u32,
        dir: &VPath,
        token: RequestToken,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        self.pedir_catalogo(dir, backend, buzon);
        // WHERE it is going stays noted down: what the slot shows does not
        // change until this lands, and until then `pane.dir()` answers for
        // the directory being left behind.
        if let Some(h) = self.huecos.get_mut(&slot) {
            h.dir_pedido = Some(dir.clone());
        }
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        let dir = dir.clone();
        let attrs = self.attrs_de(&dir);
        tokio::spawn(async move {
            let stream = backend.list(dir.clone(), attrs).await;
            let res = Estado::primera_pagina(stream, slot, token, buzon.clone()).await;
            // If the actor is no longer there, the answer matters to nobody.
            let _ = buzon
                .send(Mensaje::Listado(Box::new((token, slot, dir, res))))
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
    pub(super) fn refrescar_afectados(
        &mut self,
        task_id: u64,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<ViewChange> {
        let afectados = match self.tasks.get(&task_id) {
            Some(t) if !t.afectados.is_empty() => t.afectados.clone(),
            _ => return Vec::new(),
        };
        // While ANOTHER task is still alive over the same directory, there
        // is no re-listing: a batch of two hundred copies would produce two
        // hundred listings of the same pane, each invalidating the previous
        // one and paying the probe and the plugin decorations all over
        // again (measured at 167 ms per page of twenty). It refreshes when
        // the LAST one finishes, which is when the directory stops moving.
        let queda_trabajo = self.tasks.iter().any(|(id, t)| {
            *id != task_id
                && !Self::terminal(t.vista.state)
                && t.afectados.iter().any(|d| afectados.contains(d))
        });
        if queda_trabajo {
            return Vec::new();
        }
        // Consumed: neither this one nor its already-finished siblings ask
        // for it again.
        for t in self.tasks.values_mut() {
            if t.afectados.iter().any(|d| afectados.contains(d)) {
                t.afectados.clear();
            }
        }
        let huecos: Vec<(u32, bool)> = self
            .huecos
            .iter()
            .filter(|(_, h)| {
                afectados.contains(h.dir_pedido.as_ref().unwrap_or_else(|| h.pane.dir()))
            })
            .map(|(id, _)| (*id, self.oculto(*id)))
            .collect();
        let mut cambios = Vec::new();
        for (slot, oculto) in huecos {
            if oculto {
                // A slot that is not visible does not request listings —
                // what is not seen is not fetched — but it also cannot keep
                // believing its listing is still true: it is marked LOADING,
                // which is what `despertar_visibles` picks up as soon as it
                // comes back to the screen. Without this, a background tab
                // over the destination directory kept showing a listing from
                // before the copy until someone navigated by hand.
                if let Some(h) = self.huecos.get_mut(&slot) {
                    h.estado = Self::cargando_hacia(None, None);
                }
                cambios.push(ViewChange::SlotState {
                    slot_id: slot,
                    state: Self::cargando_hacia(None, None),
                });
                continue;
            }
            cambios.extend(self.refrescar(slot, backend, buzon));
        }
        cambios
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
    pub(super) fn refrescar(
        &mut self,
        slot: u32,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<ViewChange> {
        self.token += 1;
        let token = RequestToken(self.token);
        let Some(hueco) = self.huecos.get_mut(&slot) else {
            return Vec::new();
        };
        if hueco.en_vuelo.is_some() {
            return Vec::new();
        }
        if let Some(sel) = hueco.pane.selected().map(|e| e.path.clone()) {
            hueco.pane.set_pending_focus(sel);
        }
        hueco.pane.remember_cursor();
        // `marked_paths` falls back to the cursor when there are no marks,
        // and restoring THAT would turn a refresh into a mark the reader
        // never made.
        hueco.marcas_a_restaurar = if hueco.pane.marks_len() > 0 {
            hueco.pane.marked_paths()
        } else {
            Vec::new()
        };
        let dir = hueco.pane.dir().clone();
        hueco.estado = Self::cargando_hacia(None, None);
        hueco.en_vuelo = Some(token);
        hueco.drenando = Some(token);
        self.pedir_listado(slot, &dir, token, backend, buzon);
        vec![ViewChange::SlotState {
            slot_id: slot,
            state: Self::cargando_hacia(None, None),
        }]
    }

    /// Requests the listing of ALL visible slots again.
    ///
    /// Of all, not just the focused one, which is what the TUI does and for
    /// the same reason: what changes a listing underneath is a change ON
    /// DISK, and a change on disk does not respect focus. Hidden ones stay
    /// out — what is not seen is not fetched — `despertar_visibles` already
    /// wakes them when the layout brings them into view.
    pub(super) fn refrescar_visibles(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let slots: Vec<u32> = self
            .huecos
            .keys()
            .copied()
            .filter(|id| !self.oculto(*id))
            .collect();
        let mut cambios = Vec::new();
        for slot in slots {
            cambios.extend(self.refrescar(slot, backend, buzon));
        }
        if cambios.is_empty() {
            // Everyone had something in flight: what is about to land is
            // newer than this key, so there is nothing to say or to paint.
            return (self.aplicada(), Vec::new());
        }
        (self.aplicada(), vec![self.parche(cambios)])
    }

    /// Sets aside — or brings back — the active pane's hidden entries (#107).
    ///
    /// Presentation-only: the provider does not re-list, the set-aside
    /// entries stay in the model. And it is ANNOUNCED, because a listing
    /// that shrinks without saying why reads as a pane failure.
    pub(super) fn alternar_ocultos(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let (visibles, podadas) = {
            let hueco = self.hueco_mut();
            let visibles = hueco.pane.toggle_hidden();
            (visibles, hueco.pane.pruned_marks())
        };
        let clave = if visibles {
            "msg-hidden-shown"
        } else {
            "msg-hidden-hidden"
        };
        let mut frase = norte_i18n::t_in(self.lang, clave);
        if podadas > 0 {
            // Setting the hidden ones aside PRUNES the marks of the ones
            // that leave. The contract of `PaneState::pruned_marks` is that
            // this is never silent: keeping quiet about it would send the
            // next bulk op over fewer files than the reader marked, while
            // they believe all of them are going.
            frase.push_str(", ");
            frase.push_str(&norte_i18n::ta_in(
                self.lang,
                "status-marks-pruned",
                &[("n", &podadas.to_string())],
            ));
        }
        self.status.message = Some(clamp_display(frase));
        let filas = self.parche_filas();
        let cambio = ViewChange::Status(self.status.clone());
        (self.aplicada(), vec![filas, self.parche(vec![cambio])])
    }

    /// Cycles the reinterpretation of names that are not UTF-8 (#57).
    ///
    /// Display-only (rule 1): what changes is how the bytes are PAINTED, not
    /// the bytes. That is why row keys stay valid and only the visible rows
    /// travel.
    pub(super) fn ciclar_encoding(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let etiqueta = self.hueco_mut().pane.cycle_name_encoding();
        let frase = match etiqueta {
            Some(enc) => norte_i18n::ta_in(self.lang, "msg-names-encoding", &[("enc", enc)]),
            None => norte_i18n::t_in(self.lang, "msg-names-encoding-off"),
        };
        self.status.message = Some(clamp_display(frase));
        let filas = self.parche_filas();
        let cambio = ViewChange::Status(self.status.clone());
        (self.aplicada(), vec![filas, self.parche(vec![cambio])])
    }
}
