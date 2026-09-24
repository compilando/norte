//! The DOCKED viewer (#291): what each preview slot should be showing, and
//! what it shows.
//!
//! The same decision as `norte-tui/src/preview.rs`, with the same split of
//! responsibilities (ADR 0077): a preview slot the layout did not place —
//! closed, behind a tab, collapsed for lack of room — produces no target, so
//! there is no read to suspend; a directory under the cursor is not read; and
//! the response travels with its SLOT and its token, never with a position,
//! so a late one does not land on whoever occupies that spot when it arrives.
//!
//! What changes compared to the TUI is the "when": there it is asked every
//! frame; here, after every actor message (`sondear_previews`), which is the
//! closest thing to a frame a host that only speaks when something changes
//! has.

// The same `impl Estado` split into pieces, with the parent's imports: see
// `viewer.rs`.
#[allow(clippy::wildcard_imports)]
use super::*;

/// The kind that occupies a viewer slot. The same as the full-screen viewer:
/// what changes is the link, not what is inside.
pub(super) const KIND: &str = "viewer";

/// Cap on rows that cross for a preview slot. Normally what travels is the
/// WINDOW that fits the slot (`alto_de_preview`); this bounds a slot with no
/// known placement.
const PREVIEW_MAX_ROWS: usize = crate::bridge::MAX_ROWS_PER_BATCH;

/// What a preview slot has NOW, and what it is requesting.
#[derive(Default)]
pub(super) struct EstadoPreview {
    /// Which path it shows (or tried to show), if any.
    shown: Option<VPath>,
    /// The viewer with what was read.
    viewer: Option<norte_frontend::viewer::Viewer>,
    /// The Fluent key that replaces the file: a directory, nothing under the
    /// cursor, a read error.
    note: Option<&'static str>,
    /// The read in flight, with its token: a response with a different token
    /// is from a cursor that already moved.
    in_flight: Option<(RequestToken, VPath)>,
}

/// What a preview slot should be showing.
enum Wants {
    /// This file, which needs reading.
    File(VPath),
    /// Nothing to read, and this key says why.
    Note(&'static str),
}

impl Estado {
    /// The PLACED preview slots, with their width in cells.
    ///
    /// From the layout, not the tree: a slot behind a tab exists, but is not
    /// being seen, and what is not seen does not read.
    fn huecos_de_preview(&self) -> Vec<(u32, u16)> {
        self.reparto
            .placements
            .iter()
            .filter(|(slot, _)| kind_de(&self.arbol, *slot).is_some_and(|k| k.as_str() == KIND))
            .map(|(SlotId(id), r)| (*id, r.width))
            .collect()
    }

    /// What slot `slot` should be showing.
    ///
    /// The link is resolved with the shared engine, like the attribute sheet:
    /// a followed slot that dies degrades to the `active` role.
    fn quiere_preview(&self, slot: SlotId) -> Wants {
        let mut diags = Vec::new();
        let followed =
            norte_frontend::layout::resolve_follow(&self.arbol, slot, &self.roles, &mut diags)
                .or_else(|| self.roles.get(norte_frontend::layout::RoleId::Active));
        // With FOCUS on the preview slot itself, the active role is it, and
        // following yourself is following nobody: so the active listing takes
        // over, which always exists. In the TUI, the keyboard and the role
        // are two different things and this does not happen; here the focus
        // IS the role.
        // `cursor_entry` and not `selected`, for the same reason as the
        // attribute sheet: the viewer DESCRIBES what is under the cursor. On
        // the `..` row it used to say "nothing selected" — which is false:
        // there is a row, and it leads to a folder — on every startup.
        let entry = followed
            .and_then(|SlotId(s)| self.huecos.get(&s))
            .or_else(|| self.huecos.get(&self.activo()))
            .and_then(|h| h.pane.cursor_entry());
        let Some(e) = entry else {
            return Wants::Note("preview-empty");
        };
        match e.kind {
            EntryKind::File => Wants::File(e.path.clone()),
            EntryKind::Dir => Wants::Note("preview-directory"),
            // A link or something the provider does not classify: it is not
            // read blindly, because reading "whatever it is" is exactly how
            // an automatic preview turns into opening a block device.
            _ => Wants::Note("preview-not-a-file"),
        }
    }

    /// Sets every placed preview slot to show whatever it should: a note,
    /// right away; a file, requesting it if it is not the one already shown
    /// nor the one already in flight. Returns a snapshot if any note changed.
    pub(super) fn sondear_previews(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // A slot that no longer exists keeps nothing: neither a viewer for a
        // file nobody sees, nor a response in flight that would land on it.
        let alive: Vec<u32> = self
            .arbol
            .slot_ids()
            .into_iter()
            .map(|SlotId(id)| id)
            .collect();
        self.previews.retain(|id, _| alive.contains(id));

        let mut changed = false;
        for (id, width) in self.huecos_de_preview() {
            match self.quiere_preview(SlotId(id)) {
                Wants::Note(key) => {
                    let state = self.previews.entry(id).or_default();
                    if state.note != Some(key) || state.viewer.is_some() {
                        *state = EstadoPreview {
                            note: Some(key),
                            ..EstadoPreview::default()
                        };
                        changed = true;
                    }
                }
                Wants::File(path) => {
                    let already = self.previews.get(&id).is_some_and(|state| {
                        state.shown.as_ref() == Some(&path)
                            || state.in_flight.as_ref().is_some_and(|(_, p)| *p == path)
                    });
                    if already {
                        continue;
                    }
                    self.token += 1;
                    let token = RequestToken(self.token);
                    self.previews.entry(id).or_default().in_flight = Some((token, path.clone()));
                    // The SLOT's width minus its frame, for the previewer
                    // (proto 0.66.0): an image shrinks to whatever it is
                    // told.
                    let columns = Some(u32::from(width.saturating_sub(2).max(1)));
                    let backend = Arc::clone(backend);
                    let mailbox = mailbox.clone();
                    tokio::spawn(async move {
                        let reading = backend.read(
                            path.clone(),
                            Some(norte_proto::ByteRange {
                                offset: 0,
                                len: Some(VISOR_CAP + 1),
                            }),
                        );
                        let read_bytes = match tokio::time::timeout(PLAZO_VISOR, reading).await {
                            Ok(r) => r,
                            Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
                        };
                        // A previewer that fails, that takes too long, or
                        // that does not apply is NOT an error: it falls back
                        // to the raw view.
                        let preview = match tokio::time::timeout(
                            PLAZO_PLUGINS,
                            backend.plugin_preview_styled(path.clone(), columns),
                        )
                        .await
                        {
                            Ok(Ok(p)) => p,
                            _ => None,
                        };
                        let _ = mailbox
                            .send(Mensaje::PreviewContenido(Box::new((
                                id,
                                (token, path, read_bytes, preview),
                            ))))
                            .await;
                    });
                }
            }
        }
        if changed {
            let snap = self.snapshot();
            vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))]
        } else {
            Vec::new()
        }
    }

    /// What was read for a preview slot lands: it is shown if the token is
    /// that of THAT slot's last request, and discarded otherwise.
    pub(super) fn aterrizar_preview(
        &mut self,
        slot: u32,
        token: RequestToken,
        path: VPath,
        read_bytes: Result<Vec<u8>, Error>,
        preview: Option<norte_proto::methods::PluginPreviewStyled>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let state = self.previews.get_mut(&slot)?;
        if state.in_flight.as_ref().map(|(t, _)| *t) != Some(token) {
            return None;
        }
        state.in_flight = None;
        if let Ok(mut bytes) = read_bytes {
            let cap = usize::try_from(VISOR_CAP).unwrap_or(usize::MAX);
            let truncated = bytes.len() > cap;
            if truncated {
                bytes.truncate(cap);
            }
            // By BYTES, and before a previewer can hide the format: the
            // reel's class depends on this (`viewer.next`), and what the file
            // IS does not change because a plugin claimed it.
            let by_bytes = norte_frontend::viewer::image_format(&bytes).is_some();
            let mut v = match preview {
                Some(p) => norte_frontend::viewer::Viewer::with_plugin_preview_styled(
                    path.clone(),
                    p.plugin_name,
                    &p.lines,
                    p.lossy,
                ),
                None => norte_frontend::viewer::Viewer::new(path.clone(), bytes, truncated),
            };
            v.set_image_by_bytes(by_bytes);
            state.viewer = Some(v);
            state.note = None;
        } else {
            // It could not be read: it is SAID, in the slot, instead of
            // leaving the previous file in place as if it were this one.
            state.viewer = None;
            state.note = Some("preview-unreadable");
        }
        state.shown = Some(path);
        let snap = self.snapshot();
        Some(self.sobre(UiUpdate::Snapshot(Box::new(snap))))
    }

    /// How many rows fit in slot `slot`: its height minus the chrome (title
    /// and border). It is the window that travels and the page keys jump. A
    /// slot that is not placed has no height: it falls back to the cap.
    fn alto_de_preview(&self, slot: u32) -> usize {
        self.reparto
            .placements
            .iter()
            .find(|(s, _)| *s == SlotId(slot))
            .map_or(PREVIEW_MAX_ROWS, |(_, r)| {
                usize::from(r.height.saturating_sub(2)).clamp(1, PREVIEW_MAX_ROWS)
            })
    }

    /// The preview slot with FOCUS, if the focus is on one and it has a
    /// viewer. Without a viewer — a note — there is nothing to move, and the
    /// keys go their own way.
    fn preview_enfocado(&self) -> Option<u32> {
        let SlotId(id) = self.roles.get(norte_frontend::layout::RoleId::Active)?;
        let is_viewer = kind_de(&self.arbol, SlotId(id)).is_some_and(|k| k.as_str() == KIND);
        (is_viewer && self.previews.get(&id).is_some_and(|e| e.viewer.is_some())).then_some(id)
    }

    /// The viewer keys over the docked slot with focus (#291).
    ///
    /// They are resolved with the VIEWER's keymap, like the full one:
    /// `viewer.*` moves this viewer. `viewer.close` does not close the slot —
    /// it returns focus to the listing, like the TUI: closing a panel the
    /// reader only wanted to stop looking at is the wrong answer; closing it
    /// is `layout.preview`. `None` = focus is not on a docked viewer, or the
    /// key is not one of its own: let it go through the normal path.
    pub(super) fn tecla_en_preview(
        &mut self,
        k: &crate::keys::KeyInput,
    ) -> Option<(ActionAck, Vec<BridgeEnvelope<UiUpdate>>)> {
        let slot = self.preview_enfocado()?;
        let chord = k.to_chord().ok()?;
        let (command, count) = match self.resolver_visor.push(chord) {
            Resolution::Run { command, count } => (command, count),
            // A half-finished prefix is its own; whatever is not bound, is
            // not.
            Resolution::Pending(_) | Resolution::Counting(_) => {
                return Some((self.aplicada(), Vec::new()));
            }
            Resolution::Unavailable { .. } | Resolution::Reset => return None,
        };
        let effect = crate::commands::efecto_visor_de(&command, count.times())?;
        let height = self.alto_de_preview(slot);
        if matches!(effect, crate::commands::EfectoVisor::Cerrar) {
            let listing = SlotId(self.activo());
            self.roles
                .set(norte_frontend::layout::RoleId::Active, listing);
            self.reconcilia_roles();
            let change = ViewChange::Layout(self.disposicion());
            return Some((self.aplicada(), vec![self.parche(vec![change])]));
        }
        if let crate::commands::EfectoVisor::Hermana { adelante } = effect {
            return Some(self.hermana_del_preview(slot, adelante));
        }
        let v = self.previews.get_mut(&slot)?.viewer.as_mut()?;
        Self::mover_visor(v, effect, height);
        let snap = self.snapshot();
        Some((
            self.aplicada(),
            vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))],
        ))
    }

    /// The next (or previous) sibling in the DOCKED viewer.
    ///
    /// Nothing is opened here, and that is the whole difference from the full
    /// viewer: the docked one follows the active listing's CURSOR
    /// ([`Self::quiere_preview`]), so moving the cursor IS requesting the
    /// next file, and the read is done by the next round's polling, like with
    /// any other movement.
    fn hermana_del_preview(
        &mut self,
        slot: u32,
        adelante: bool,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let state = self.previews.get(&slot);
        let is_image = state
            .and_then(|e| e.viewer.as_ref())
            .is_some_and(norte_frontend::viewer::Viewer::is_image_by_bytes);
        let Some(open_path) = state.and_then(|e| e.shown.clone()) else {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        };
        let wanted = if is_image {
            norte_frontend::viewer::Clase::Imagen
        } else {
            norte_frontend::viewer::Clase::Otro
        };
        // The listing the ladder comes from is the one THIS preview follows
        // ([`Self::quiere_preview`]), which with a slot tied to a role is not
        // the active listing: reading the active one would move another
        // panel's cursor and leave this preview exactly as it was.
        let mut diags = Vec::new();
        let followed = norte_frontend::layout::resolve_follow(
            &self.arbol,
            SlotId(slot),
            &self.roles,
            &mut diags,
        )
        .or_else(|| self.roles.get(norte_frontend::layout::RoleId::Active))
        .map_or_else(|| self.activo(), |SlotId(s)| s);
        let Some(pane) = self.huecos.get(&followed).map(|h| &h.pane) else {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        };
        let entries = pane.entries();
        // Only by what the reader SEES, like the full viewer.
        let visible = pane.quick_visible();
        let target = entries
            .iter()
            .position(|e| e.path == open_path)
            .and_then(|from| {
                norte_frontend::viewer::hermana(entries, visible, from, adelante, wanted)
            });
        let Some(row) = target else {
            self.status.message = Some(clamp_display(norte_i18n::t_in(
                self.lang,
                "host-no-sibling",
            )));
            let change = ViewChange::Status(self.status.clone());
            return (
                ActionAck::Unavailable {
                    reason_key: "host-no-sibling".to_owned(),
                },
                vec![self.parche(vec![change])],
            );
        };
        // An earlier "no more" cannot survive a jump that DID happen.
        self.status.message = None;
        // `senalar` and not `set_cursor`: with a live filter, what the
        // preview follows is the quick-search selection, and moving the real
        // cursor does not move it — the panel would stay the same while the
        // key claims it worked.
        if let Some(h) = self.huecos.get_mut(&followed) {
            h.pane.senalar(row);
        }
        let snap = self.snapshot();
        (
            self.aplicada(),
            vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))],
        )
    }

    /// Applies a viewer effect that is NOT closing.
    fn mover_visor(
        v: &mut norte_frontend::viewer::Viewer,
        effect: crate::commands::EfectoVisor,
        height: usize,
    ) {
        use crate::commands::EfectoVisor;
        // `unsigned_abs`, not `abs`: `PreviewScroll`'s delta arrives RAW from
        // the renderer, and `i64::MIN.abs()` overflows.
        let steps = |n: i64| usize::try_from(n.unsigned_abs()).unwrap_or(usize::MAX);
        match effect {
            // Both are handled by the caller, and before getting here:
            // closing returns focus to the listing, and the siblings move
            // their CURSOR — the docked one follows it, so moving it IS
            // requesting the next file. In neither case is there anything to
            // move in THIS viewer.
            EfectoVisor::Cerrar | EfectoVisor::Hermana { .. } => {}
            EfectoVisor::Linea(n) if n < 0 => v.scroll_up(steps(n)),
            EfectoVisor::Linea(n) => v.scroll_down(steps(n)),
            EfectoVisor::Pagina(n) if n < 0 => v.scroll_up(steps(n).saturating_mul(height)),
            EfectoVisor::Pagina(n) => v.scroll_down(steps(n).saturating_mul(height)),
            EfectoVisor::Columna(n) if n < 0 => v.scroll_left(steps(n)),
            EfectoVisor::Columna(n) => v.scroll_right(steps(n)),
            EfectoVisor::Extremo { al_final: false } => v.scroll_top(),
            EfectoVisor::Extremo { al_final: true } => v.scroll_bottom(),
            EfectoVisor::Hex => v.toggle_hex(),
            EfectoVisor::Encoding => v.cycle_encoding(),
            EfectoVisor::EncodingAuto => v.reset_encoding(),
            EfectoVisor::Zoom { acercar: true } => v.zoom_in(),
            EfectoVisor::Zoom { acercar: false } => v.zoom_out(),
            EfectoVisor::ZoomAjustar => v.zoom_fit(),
        }
    }

    /// The wheel over a preview slot: `delta` lines through the HOST, which
    /// is the one who decides the visible window (like the log panel).
    pub(super) fn desplazar_preview(
        &mut self,
        slot: u32,
        delta: i64,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.oculto(slot) {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        }
        let Some(v) = self.previews.get_mut(&slot).and_then(|e| e.viewer.as_mut()) else {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        };
        Self::mover_visor(v, crate::commands::EfectoVisor::Linea(delta), 1);
        let snap = self.snapshot();
        (
            self.aplicada(),
            vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))],
        )
    }

    /// A preview slot's projection: the WINDOW of rows that fits the slot,
    /// from wherever the viewer is scrolled to.
    pub(super) fn vista_de_preview(&self, slot: u32) -> crate::dto::PreviewSlotView {
        let state = self.previews.get(&slot);
        let height = self.alto_de_preview(slot);
        let viewer = state
            .and_then(|e| e.viewer.as_ref())
            .map(|v| self.vista_de_visor(v, height, false));
        let note = if viewer.is_some() {
            String::new()
        } else {
            let key = state.and_then(|e| e.note).unwrap_or("preview-empty");
            clamp_display(norte_i18n::t_in(self.lang, key))
        };
        crate::dto::PreviewSlotView {
            slot_id: slot,
            viewer,
            note,
        }
    }
}
