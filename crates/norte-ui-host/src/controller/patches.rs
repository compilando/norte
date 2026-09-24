//! Patches: what crosses the bridge and with which generation.
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
    pub(super) fn aplicada(&self) -> ActionAck {
        // The sequence number that will reflect it: the next one emitted.
        ActionAck::Applied {
            sequence: self.sequence + 1,
        }
    }

    /// A normal race between what the renderer believed and what is there.
    pub(super) fn obsoleta(reason: StaleAction) -> ActionAck {
        ActionAck::Stale { reason }
    }

    pub(super) fn sobre(&mut self, u: UiUpdate) -> BridgeEnvelope<UiUpdate> {
        // A whole snapshot carries the bar inside: it is the last thing the
        // renderer saw of it, and what the next patch compares against.
        if let UiUpdate::Snapshot(s) = &u {
            self.ultima_barra = Some(s.panel_bar.clone());
            self.ultimos_elementos = Some(s.status_items.clone());
            // The snapshot was assembled with the CURRENT layout
            // (`ajuste_de` is pure).
            let _ = self.ajustes_movidos();
        }
        self.sequence += 1;
        BridgeEnvelope::new(self.instance.clone(), self.sequence, u)
    }

    pub(super) fn parche(&mut self, mut changes: Vec<ViewChange>) -> BridgeEnvelope<UiUpdate> {
        // The panel bar goes in ANY patch that changes it, without whoever
        // assembles the patch knowing (#324). It is the translation of the
        // TUI's "derived per frame": there `panel_buttons` runs on every
        // paint; here the bridge only speaks when something changes, so it is
        // compared against the last one that crossed. A panel opened by key,
        // by menu, by palette, or by the bar itself updates the bar the same
        // way.
        let bar = self.vista_barra_de_paneles();
        if self.ultima_barra.as_ref() != Some(&bar) {
            changes.push(ViewChange::PanelBar {
                panel_bar: bar.clone(),
            });
            self.ultima_barra = Some(bar);
        }
        // And the status bar items (ADR 0132), by the same mechanism: the
        // cursor, the marks, the sort order, and the board move them, and
        // none of those paths knows there is a bar counting them.
        let items = self.vista_elementos_de_estado();
        if self.ultimos_elementos.as_ref() != Some(&items) {
            changes.push(ViewChange::StatusItems {
                status_items: items.clone(),
            });
            self.ultimos_elementos = Some(items);
        }
        // And every slot's column layout: the slot's width and the listing's
        // names move them, and none of the paths that change those sends a
        // header. Header and rows go TOGETHER, behind whatever the patch
        // already carried, so they take precedence over it.
        for slot in self.ajustes_movidos() {
            if let Some(h) = self.huecos.get(&slot) {
                changes.push(ViewChange::Columns {
                    slot_id: slot,
                    columns: self.cabeceras(slot, h),
                });
            }
            changes.push(self.cambio_de_filas_de(slot));
        }
        let base = self.sequence;
        self.sobre(UiUpdate::Patch(ViewPatch {
            base_sequence: base,
            changes,
        }))
    }

    /// Only the visible window travels: a directory with a hundred thousand
    /// entries does not cross the bridge to paint forty rows.
    pub(super) fn filas_visibles(&self) -> Vec<RowView> {
        self.filas_de(self.activo(), self.hueco())
    }

    /// Any slot's visible rows.
    pub(super) fn filas_de(&self, slot: u32, hueco: &Hueco) -> Vec<RowView> {
        let first = usize::try_from(hueco.primera_visible).unwrap_or(0);
        let count = usize::try_from(hueco.visibles).unwrap_or(0);
        // ONCE per batch, not once per row: it walks the whole task map and
        // clones a `VPath` per live task. Inside `fila` that was one walk and
        // one clone per VISIBLE entry, on every repaint, and the repaints are
        // triggered by exactly what fills that list: progress.
        let operands = self.operandos_vivos();
        // And the column layout, for the same reason: the header and every
        // row of the batch have to come from the SAME one.
        let fitted_columns = self.ajuste_de(slot, hueco);
        hueco
            .pane
            .entries()
            .iter()
            .enumerate()
            .skip(first)
            .take(count.min(MAX_ROWS_PER_BATCH))
            .map(|(i, e)| self.fila(hueco, i, e, &operands, &fitted_columns))
            .collect()
    }

    /// A listing's generation: `PaneState`'s EPOCH, which rises on anything
    /// that moves the indices — a re-listing, a re-sort, a hidden-files
    /// filter — not only on changing directory.
    pub(super) fn generacion(&self) -> u64 {
        self.hueco().pane.listing_epoch()
    }

    /// Moving the cursor sends the cursor, not the listing.
    pub(super) fn parche_cursor(&mut self) -> BridgeEnvelope<UiUpdate> {
        let change = ViewChange::Cursor {
            slot_id: self.activo(),
            generation: self.generacion(),
            cursor: Some(RowKey(self.hueco().pane.cursor() as u64)),
        };
        self.parche(vec![change])
    }

    /// Polls what a slot's visible window does not know yet.
    ///
    /// The local listing is LAZY on purpose (#52): `readdir` gives the type
    /// but not the size, and stat-ing half a million entries to paint forty
    /// rows is exactly what that decision avoids. Whoever shows size and date
    /// columns has to request them for what is visible — the TUI does it from
    /// its loop, and this is the same thing with the window the renderer
    /// declared. The rule for WHAT is needed is the shared one
    /// (`needs_stat_at`), not one of its own here.
    // TODO(translation): review — this paragraph describes a lazy-listing
    /// stat poll, but the function right after it (`adornar`) is about plugin
    /// decorations; it looks like a stale doc fragment left by an earlier
    /// edit.
    /// Asks plugins whatever they want to say about the VISIBLE WINDOW.
    ///
    /// Two things in one trip — badges and `plugin:` column values — because
    /// they are the same question over the same paths, and the TUI already
    /// does it this way.
    ///
    /// Of the window and NOT of the listing, which is where this parts ways
    /// with the TUI: the terminal decorates "every loaded entry" because its
    /// pane does not declare a window, and here the renderer does declare
    /// one. Every call spins up one wasm instance per plugin, and #224
    /// measured **167 ms per page of 20 over 2000 entries**: requesting it
    /// for what is not visible is paying that price for nothing, multiplied
    /// by the directory's size.
    ///
    /// A HIDDEN slot does not ask. What is not seen is not fetched, same as
    /// its listing.
    ///
    /// Everything fail-soft: with no consented decorators, with the
    /// catalogue down, or with a broken RPC, the listing paints the same and
    /// with no badges. A decoration is cosmetic by contract (ADR 0037).
    pub(super) fn adornar(
        &mut self,
        slot: u32,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) {
        if self.oculto(slot) {
            return;
        }
        let columns = self
            .huecos
            .get(&slot)
            // The painted ones and the status bar's (ADR 0137): the SAME
            // list the TUI requests.
            .map(|h| {
                norte_frontend::columns::plugin_requests(
                    &self.columnas,
                    &self.config.common.ui_status_plugins,
                    h.pane.dir().scheme(),
                )
            })
            .unwrap_or_default();
        let Some(hueco) = self.huecos.get_mut(&slot) else {
            return;
        };
        // One batch per slot, checked BEFORE choosing candidates: the other
        // way around, the chosen ones would end up marked as requested
        // without having been, and would never be requested again. It is the
        // same trap `sondear` documents, and just as easy to fall into.
        if hueco.adornando {
            return;
        }
        let first = usize::try_from(hueco.primera_visible).unwrap_or(0);
        let count = usize::try_from(hueco.visibles).unwrap_or(0);
        let (candidates, kinds): (Vec<VPath>, Vec<norte_proto::EntryKind>) = hueco
            .pane
            .entries()
            .iter()
            .skip(first)
            .take(count)
            .filter(|e| !hueco.adornadas.contains(&e.path))
            .map(|e| (e.path.clone(), e.kind))
            .unzip();
        if candidates.is_empty() {
            return;
        }
        for p in &candidates {
            hueco.adornadas.insert(p.clone());
        }
        let dir = hueco.pane.dir().clone();
        hueco.adornando = true;
        let generation = hueco.gen_adornos;
        let cancel_flag = hueco.cancelar_sondeo.clone();
        let backend = Arc::clone(backend);
        let mailbox = mailbox.clone();
        tokio::spawn(async move {
            let raw = backend
                .plugin_decorate(candidates.clone(), kinds)
                .await
                .unwrap_or_default();
            let decorations = norte_frontend::merge_decorations(&candidates, &raw);
            let (cells, labels) = celdas_de_plugin(&backend, &columns, &candidates, || {
                cancel_flag.load(std::sync::atomic::Ordering::SeqCst)
            })
            .await;
            let _ = mailbox
                .send(Mensaje::Fondo(Box::new(Fondo::Adornos(Box::new((
                    generation,
                    slot,
                    dir,
                    decorations,
                    cells,
                    labels,
                ))))))
                .await;
        });
    }

    pub(super) fn sondear(
        &mut self,
        slot: u32,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) {
        let attrs = self
            .huecos
            .get(&slot)
            .map(|h| self.attrs_de(h.pane.dir()))
            .unwrap_or_default();
        let Some(hueco) = self.huecos.get_mut(&slot) else {
            return;
        };
        // One probe per slot, and it is checked BEFORE choosing candidates:
        // if they are chosen and then the batch is abandoned, those paths end
        // up marked as probed without having been, and are never requested
        // again. Without this order, a debounced scroll stacked batches of
        // two hundred trips against the same connection and also ate rows
        // along the way.
        if hueco.sondeando {
            return;
        }
        let first = usize::try_from(hueco.primera_visible).unwrap_or(0);
        let count = usize::try_from(hueco.visibles).unwrap_or(0);
        let candidates: Vec<VPath> = hueco
            .pane
            .needs_stat_at(first..first.saturating_add(count))
            .into_iter()
            .filter(|p| !hueco.sondeados.contains(p))
            .take(MAX_SONDEOS)
            .collect();
        if candidates.is_empty() {
            return;
        }
        for p in &candidates {
            hueco.sondeados.insert(p.clone());
        }
        let dir = hueco.pane.dir().clone();
        hueco.sondeando = true;
        let cancel_flag = hueco.cancelar_sondeo.clone();
        let backend = Arc::clone(backend);
        let mailbox = mailbox.clone();
        tokio::spawn(async move {
            use futures::StreamExt as _;
            // BOUNDED in parallel: a remote session cannot pay for N trips in
            // series (200 probes at 80 ms round trip is sixteen seconds), and
            // with a deadline, because a stuck provider cannot be allowed to
            // take the other 199 down with it.
            let probes: Vec<(VPath, Entry)> = futures::stream::iter(candidates)
                .map(|p| {
                    let backend = Arc::clone(&backend);
                    let attrs = attrs.clone();
                    async move {
                        let stat = backend.stat(p.clone(), attrs);
                        match tokio::time::timeout(PLAZO_SONDEO, stat).await {
                            // A probe that fails or takes too long is not a
                            // screen error: that cell stays blank and is not
                            // requested again.
                            Ok(Ok(e)) => Some((p, e)),
                            _ => None,
                        }
                    }
                })
                .buffer_unordered(SONDEOS_A_LA_VEZ)
                .filter_map(|x| async move { x })
                .collect()
                .await;
            if cancel_flag.load(std::sync::atomic::Ordering::SeqCst) || probes.is_empty() {
                // The listing changed while probing: what comes back does not
                // describe the screen that is there.
                let _ = mailbox
                    .send(Mensaje::Hidratado(Box::new((dir, slot, Vec::new()))))
                    .await;
                return;
            }
            let _ = mailbox
                .send(Mensaje::Hidratado(Box::new((dir, slot, probes))))
                .await;
        });
    }

    /// Pastes a batch of filler onto the listing that requested it.
    ///
    /// `None` if the slot disappeared or the batch is from a navigation
    /// already superseded: pasting it would mix two trees on one screen.
    pub(super) fn aplicar_lote(
        &mut self,
        slot: u32,
        token: RequestToken,
        batch: Vec<Entry>,
        last: bool,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let hueco = self.huecos.get_mut(&slot)?;
        if hueco.drenando != Some(token) {
            // A batch from a navigation already superseded: pasting it would
            // mix two trees on one screen.
            return None;
        }
        if last {
            // The stream is done: this slot is no longer growing.
            hueco.drenando = None;
        }
        if batch.is_empty() && !hueco.filas_por_publicar {
            // Nothing to paste and nothing pending: the stream closed with no
            // leftover.
            return None;
        }
        // What is visible NOW, to compare against what will be visible. A
        // directory with a hundred thousand entries drains in batches of 500
        // and each batch used to publish its own patch: two hundred patches
        // in a burst against a channel of 64, meaning any subscriber that
        // does not drain at that speed gets `Lagged` and has to request a
        // whole snapshot. And almost all those patches carried the SAME
        // rows: what was being mixed in fell well below the visible window
        // (#252).
        let before = self
            .huecos
            .get(&slot)
            .map(|h| self.filas_de(slot, h))
            .unwrap_or_default();
        if !batch.is_empty()
            && let Some(hueco) = self.huecos.get_mut(&slot)
        {
            hueco.pane.extend(batch);
        }
        let after = self
            .huecos
            .get(&slot)
            .map(|h| self.filas_de(slot, h))
            .unwrap_or_default();
        // Staying quiet about a patch is not free: `extend` bumps the
        // listing's EPOCH and the renderer names every row by the epoch it
        // saw it in, so a renderer that has every patch withheld from it is
        // left with an old epoch and every click of theirs is rejected as
        // stale. That is why what is withheld is NOTED, and the last batch —
        // even if it comes in empty, which happens when the rest is an exact
        // multiple of the batch — settles the debt.
        let silenced = !last && before == after;
        if let Some(h) = self.huecos.get_mut(&slot) {
            // Withheld: debt remains. Published: the debt is settled, because
            // the patch carries the CURRENT epoch.
            h.filas_por_publicar = silenced;
        }
        if silenced {
            return None;
        }
        Some(self.parche_filas_de(slot))
    }

    /// Pastes what a probe found out onto the listing that requested it.
    ///
    /// `None` if there is nothing to repaint: the slot disappeared, or the
    /// listing that was probed has already been superseded — pasting sizes
    /// onto it would be lying about what is visible.
    pub(super) fn aplicar_sondas(
        &mut self,
        slot: u32,
        dir: &VPath,
        probes: &[(VPath, Entry)],
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let hueco = self.huecos.get_mut(&slot)?;
        // The flag is lowered ONLY if what arrives describes this listing. A
        // cancelled batch landing late used to lower the NEW batch's flag,
        // and then `sondear` would let a second one launch on the same slot.
        if hueco.pane.dir() != dir {
            // The slot is in ANOTHER directory: pasting these sizes onto it
            // would be lying about what is visible. (A filler batch, by
            // contrast, invalidates nothing: it bumps the epoch and shifts
            // indices, and here it is matched by path.)
            return None;
        }
        hueco.sondeando = false;
        for (requested, e) in probes {
            // By the path that was REQUESTED: the one the provider returns
            // can be a different spelling of the same name (NFD on HFS+, a
            // different case on SMB, a link's target) and then it matches
            // nothing — and since it is already in `sondeados`, it is never
            // retried.
            hueco.pane.hydrate(requested, e.size, e.mtime_ms);
        }
        Some(self.parche_filas_de(slot))
    }

    /// A specific slot's visible rows, not the one with focus.
    pub(super) fn parche_filas_de(&mut self, slot: u32) -> BridgeEnvelope<UiUpdate> {
        let change = self.cambio_de_filas_de(slot);
        // The header goes WITH the rows: `pane.names-encoding` retranscribes
        // the path the same way as the names, and hiding moves entries in and
        // out of the listing. Sending only the rows left the title with the
        // old reading.
        let header = self
            .huecos
            .get(&slot)
            .map(|h| self.cabecera_de(slot, h))
            .into_iter();
        self.parche(std::iter::once(change).chain(header).collect())
    }

    /// Any slot's ROW change, unwrapped.
    fn cambio_de_filas_de(&self, slot: u32) -> ViewChange {
        let (generation, first, rows, total, icons) = match self.huecos.get(&slot) {
            Some(h) => (
                h.pane.listing_epoch(),
                h.primera_visible,
                self.filas_de(slot, h),
                Some(h.pane.entries().len() as u64),
                h.pane.any_icon(),
            ),
            None => (0, 0, Vec::new(), None, false),
        };
        ViewChange::Rows {
            slot_id: slot,
            generation,
            first_visible: first,
            rows,
            icon_column: icons,
            // The total goes WITH the rows: it is the renderer's scroll
            // height, and paginated draining sends nothing else — not on the
            // last batch either.
            total_rows: total,
        }
    }

    /// The slots whose column layout is no longer the last one that crossed,
    /// with the new one already noted as crossed.
    fn ajustes_movidos(&mut self) -> Vec<u32> {
        let now: Vec<(u32, Vec<norte_frontend::columns::Fitted>)> = self
            .huecos
            .iter()
            .map(|(id, h)| (*id, self.ajuste_de(*id, h)))
            .collect();
        let mut moved = Vec::new();
        for (id, fit) in now {
            if self.ultimo_ajuste.get(&id) != Some(&fit) {
                self.ultimo_ajuste.insert(id, fit);
                moved.push(id);
            }
        }
        self.ultimo_ajuste
            .retain(|id, _| self.huecos.contains_key(id));
        moved
    }

    /// What a mark or a scroll changes: the visible rows.
    pub(super) fn parche_filas(&mut self) -> BridgeEnvelope<UiUpdate> {
        let change = self.cambio_de_filas();
        let header = self.cabecera_de(self.activo(), self.hueco());
        self.parche(vec![change, header])
    }

    /// The active slot's ROW change, unwrapped: for whoever has to send it
    /// alongside others in the same patch.
    pub(super) fn cambio_de_filas(&self) -> ViewChange {
        ViewChange::Rows {
            slot_id: self.activo(),
            generation: self.generacion(),
            first_visible: self.hueco().primera_visible,
            rows: self.filas_visibles(),
            icon_column: self.hueco().pane.any_icon(),
            // Marking or hiding does not only change which rows are visible:
            // `toggle-hidden` moves entries in and out of the listing, so the
            // total and the scroll height move with them.
            total_rows: Some(self.hueco().pane.entries().len() as u64),
        }
    }

    /// A listing row, with the live operands ALREADY computed.
    ///
    /// They are received instead of requested: `operandos_vivos` walks the
    /// task map and clones a path per live task, and doing that per row
    /// turned a fifty-row repaint with twenty tasks into a thousand walks and
    /// a thousand clones.
    pub(super) fn fila(
        &self,
        hueco: &Hueco,
        i: usize,
        e: &Entry,
        operands: &[(VPath, Option<u8>)],
        columns: &[norte_frontend::columns::Fitted],
    ) -> RowView {
        let bytes = e
            .path
            .file_name()
            .map_or(&[][..], norte_proto::Segment::as_bytes);
        // WITH whatever reinterpretation the panel has set (#57): without it
        // `pane.names-encoding` cycled internally and the screen did not
        // change, which is a command that can only read as broken. What gets
        // reinterpreted is the PAINTED text; the bytes are not touched, and
        // the row is still flagged hostile (rule 1).
        // The PARENT row is painted as `..` and not the parent directory's
        // name, which is what its path says: the parent's name on the first
        // row reads as "there is a directory here named that". Neither a
        // hostile badge nor reinterpretation — two ASCII characters are
        // nobody's name.
        let (text, hostile) = if hueco.pane.is_parent_row(i) {
            ("..".to_owned(), false)
        } else {
            norte_frontend::display_name_with(bytes, hueco.pane.name_encoding())
        };
        // Served by the PANE, which re-masks on serving: the host accumulates
        // but is not the one deciding what gets painted.
        let decoration = hueco.pane.decoration_for(&e.path);
        // The theme's color for THIS entry (`[files.ext]` / `[files.kind]`).
        // Against the RAW bytes, not against `text`: that one is masked and
        // reinterpreted for painting, and the masking is not injective — an
        // extension matched against it would be another name's extension.
        // The mapping belongs to `norte-frontend` and not here: both
        // frontends need it and it is the SAME decision, which written twice
        // diverges silently (ADR 0077).
        let style = self.tema.estilo_de_entrada(
            bytes,
            norte_frontend::theme::file_kind_of(e.kind),
            self.esquema_oscuro,
        );
        RowView {
            key: RowKey(i as u64),
            display_name: clamp_display(text),
            hostile,
            kind: match e.kind {
                EntryKind::Dir => RowKind::Dir,
                EntryKind::File => RowKind::File,
                EntryKind::Symlink => RowKind::Symlink,
                EntryKind::Other => RowKind::Other,
            },
            // Which task is working on THIS row (spec 2026-09-15): decided by
            // the shared crate, which matches by exact path and keeps the
            // least advanced one.
            progress: norte_frontend::processes::progress_for(
                operands.iter().map(|(r, p)| (r, *p)),
                &e.path,
            ),
            selected: i == hueco.pane.cursor(),
            marked: hueco.pane.is_marked(e),
            cells: self.celdas(hueco, e, columns),
            badge: decoration
                .and_then(|d| d.badge.clone())
                .map(clamp_display)
                .unwrap_or_default(),
            badge_hostile: decoration.is_some_and(|d| d.badge_hostile),
            badge_role: decoration
                .and_then(|d| d.role)
                .map_or_else(String::new, |r| r.as_kebab().to_owned()),
            icon: decoration
                .and_then(|d| d.icon.clone())
                .map(clamp_display)
                .unwrap_or_default(),
            icon_hostile: decoration.is_some_and(|d| d.icon_hostile),
            name_color: style.color,
            name_bold: style.bold,
            name_dim: style.dim,
            name_italic: style.italic,
            name_underline: style.underline,
        }
    }

    /// What each live task is working on, with its percentage.
    ///
    /// The path is the LAST progress's (`current`), which is what the wire
    /// says is being touched right now; a finished task does not count,
    /// because its row is no longer waiting on anyone.
    pub(super) fn operandos_vivos(&self) -> Vec<(VPath, Option<u8>)> {
        self.tasks
            .values()
            .filter(|t| !Self::terminal(t.vista.state))
            .filter_map(|t| {
                // The LIVE progress and not the view: the view is a snapshot
                // projected when the mailbox message goes out, and a
                // listing's row is painted far more often than that.
                let p = t.progreso.borrow();
                p.current
                    .as_ref()
                    .map(|path| (path.clone(), norte_frontend::tasks::progress_pct(&p)))
            })
            .collect()
    }

    /// A path's location's catalogue, if it has arrived yet.
    pub(super) fn catalogo_de(&self, path: &VPath) -> Option<&norte_proto::AttrCatalog> {
        self.catalogos.get(path.scheme())
    }

    /// A row's cells, one per configured column.
    ///
    /// Built by `norte_frontend::columns::styled_cell`, the same function the
    /// TUI uses: a size's or a date's format cannot depend on who paints it.
    /// `None` is ABSENCE — a directory with no size, an attribute the
    /// provider did not send — and it travels as such: never a manufactured
    /// `0`.
    ///
    /// Only the ones in `columnas`, the slot's layout (`ajuste_de`): a cell
    /// for a column the header dropped would paint with no width.
    pub(super) fn celdas(
        &self,
        hueco: &Hueco,
        e: &Entry,
        columns: &[norte_frontend::columns::Fitted],
    ) -> Vec<crate::dto::CellView> {
        use norte_frontend::columns::{ColumnId, styled_cell_in};
        let now = ahora_ms();
        let scheme = hueco.pane.dir().scheme().to_owned();
        columns
            .iter()
            .filter(|f| {
                !matches!(
                    f.id,
                    ColumnId::Builtin(norte_frontend::columns::Builtin::Name)
                )
            })
            .map(|f| {
                let col = &f.id;
                let text = match col {
                    // Plugin ones do not live in the `Entry` but in the
                    // pane's side-map: they are resolved that way.
                    ColumnId::Plugin { plugin, column } => hueco.pane.plugin_cell(
                        &norte_frontend::columns::plugin_display_id(plugin, column),
                        &e.path,
                    ),
                    // The CONFIGURED style, same as the header: with
                    // `default_for_id`, a `format = "iso"` did nothing here
                    // while the terminal did honor it, and the date always
                    // came out relative.
                    //
                    // And with the host's LANGUAGE: the kind, a boolean and
                    // the relative date translate, and they came out in the
                    // process's — every date cell in the listing under a
                    // header in a different language.
                    other => styled_cell_in(
                        e,
                        other,
                        now,
                        &self
                            .columnas
                            .style_for_id(&scheme, other, self.catalogo_de(&e.path))
                            .compacted(f.compact),
                        self.lang,
                    ),
                };
                crate::dto::CellView {
                    // Identity: whole or empty, never truncated.
                    column: identidad_de_columna(col),
                    text: text.map(clamp_display),
                }
            })
            .collect()
    }
}
