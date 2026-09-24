//! The panel a PLUGIN paints, in the window (phase 3).
//!
//! The same mold as the docked preview, on purpose: one state per slot, one
//! live request with its token, and a response that arrives with a different
//! token is discarded. What changes is what is requested — a frame, not a
//! file — and that here there is something to KEEP between repaints: the
//! guest's opaque state, which is the only thing that survives (the
//! permission to read is minted per call, in the core).
//!
//! What the guest describes is converted with
//! [`norte_frontend::frame::StyledFrame::de_wire`], the same gate the
//! terminal uses: it masks each span's text and narrows its role. A
//! conversion written here by hand would reopen the hole that function
//! closed.

#[allow(clippy::wildcard_imports)]
use super::*;

/// What makes one repaint different from another.
///
/// It carries the KIND and not just the geometry: a `SlotId` gets reused —
/// presets bring small, fixed ids — so without it, another plugin's panel
/// would inherit the first one's frame and state.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Firma {
    /// Which panel it is: `plugin:<id>:<kind>`.
    kind: String,
    /// The directory it looks at.
    dir: VPath,
    /// Usable width, without the frame.
    cols: u32,
    /// Usable height, without the frame.
    rows: u32,
    /// The name under the cursor of the listing it follows.
    cursor: Option<String>,
}

/// What a panel slot has NOW and what it is requesting.
#[derive(Default)]
pub(super) struct EstadoPanel {
    /// WHICH panel what is stored here belongs to.
    ///
    /// A `SlotId` gets reused — `poner_arbol` changes the whole tree and
    /// keeps the ids, and presets and templates bring small, fixed ids — so
    /// slot 3 can go from one plugin to another on a layout change or a
    /// restored session. Without this, A's stuff stayed here for B: its
    /// frame — with its clickable zones — painted under B's title until its
    /// first one arrived, and its OPAQUE STATE handed to B on the first
    /// request. Nobody in this house looks at the blob, but it is A's, and
    /// the reader's consent was plugin by plugin.
    kind: Option<String>,
    /// The last frame that arrived. It is kept while the next one is
    /// requested: a slow plugin leaves the previous picture, not a flickering
    /// slot.
    frame: Option<norte_frontend::frame::StyledFrame>,
    /// The guest's opaque state, as is. This process does not look at it.
    state: Option<Vec<u8>>,
    /// The signature of what is being shown, or of what was attempted and
    /// came back with no frame. Both things in one field because both answer
    /// the same question: does this need requesting? Without noting the empty
    /// attempt, a panel whose plugin is no longer there would be retried on
    /// every actor message.
    firma: Option<Firma>,
    /// The request in flight, with its token.
    en_vuelo: Option<(RequestToken, Firma)>,
}

impl Estado {
    /// The PLACED plugin panel slots, with their kind and their size.
    ///
    /// From the layout, not the tree, like the preview: a slot behind a tab
    /// exists, but is not being seen, and what is not seen does not request.
    fn huecos_de_panel(&self) -> Vec<(u32, String, u16, u16)> {
        self.reparto
            .placements
            .iter()
            .filter_map(|(slot, r)| {
                let kind = kind_de(&self.arbol, *slot)?;
                if !kind.as_str().starts_with("plugin:") {
                    return None;
                }
                // And that the kind be DECLARED by a consented plugin. The
                // prefix is written by whoever edits a layout, and without
                // this gate a `plugin:whatever:whatever` in a file was enough
                // for the host to hand the resolver the directory the reader
                // is looking at.
                self.kinds.get(&kind)?;
                let SlotId(id) = *slot;
                Some((id, kind.as_str().to_owned(), r.width, r.height))
            })
            .collect()
    }

    /// What slot `slot`'s panel should be showing.
    ///
    /// The link is resolved with the shared engine, same as the preview and
    /// the sheet: a followed slot that dies degrades to the `active` role.
    fn firma_de_panel(&self, slot: SlotId, kind: &str, width: u16, height: u16) -> Firma {
        let mut diags = Vec::new();
        let followed =
            norte_frontend::layout::resolve_follow(&self.arbol, slot, &self.roles, &mut diags)
                .or_else(|| self.roles.get(norte_frontend::layout::RoleId::Active));
        let slot_state = followed
            .and_then(|SlotId(s)| self.huecos.get(&s))
            .or_else(|| self.huecos.get(&self.activo()));
        Firma {
            kind: kind.to_owned(),
            dir: slot_state
                .map_or_else(|| self.hueco().pane.dir().clone(), |h| h.pane.dir().clone()),
            // Without the frame: the guest describes what is INSIDE.
            cols: u32::from(width.saturating_sub(2)),
            rows: u32::from(height.saturating_sub(2)),
            // `cursor_entry` and not `selected`, for the same reason as the
            // preview: the panel talks about what is UNDER the cursor. And
            // via `display_name`, which is what this house would paint.
            cursor: slot_state
                .and_then(|h| h.pane.cursor_entry())
                .and_then(|e| {
                    e.path
                        .file_name()
                        .map(|n| norte_frontend::display_name(n.as_bytes()).0)
                }),
        }
    }

    /// Requests the frame of every placed plugin panel that needs one.
    ///
    /// Called after every actor message, like the preview: anything moves the
    /// cursor, and the guest gets the row under it. One live request per
    /// slot; while there is one, another is not started — dropping the
    /// response does not cancel the work, which is already instantiating
    /// wasm.
    pub(super) fn sondear_paneles(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // The NORMAL case — no plugin panel placed and nothing stored —
        // returns without touching the tree: this runs after every actor
        // message, and walking the slots to discover there is none is paid
        // on every keystroke of every session that does not use plugins.
        let slots = self.huecos_de_panel();
        if slots.is_empty() && self.paneles.is_empty() {
            return Vec::new();
        }
        // A slot that no longer exists keeps nothing. It matters more than in
        // the preview: what a panel keeps is its guest's OPAQUE state, and a
        // reused `SlotId` would hand it to the next plugin.
        let alive: Vec<u32> = self
            .arbol
            .slot_ids()
            .into_iter()
            .map(|SlotId(id)| id)
            .collect();
        self.paneles.retain(|id, _| alive.contains(id));

        for (id, kind, width, height) in slots {
            let firma = self.firma_de_panel(SlotId(id), &kind, width, height);
            let state = self.paneles.entry(id).or_default();
            // The slot changed panel: what was there belonged to ANOTHER
            // plugin and is not inherited — neither the frame, nor the opaque
            // state.
            if state.kind.as_deref() != Some(kind.as_str()) {
                *state = EstadoPanel {
                    kind: Some(kind.clone()),
                    ..EstadoPanel::default()
                };
            }
            if state.firma.as_ref() == Some(&firma) || state.en_vuelo.is_some() {
                continue;
            }
            // The kind is split BEFORE marking anything in flight. The other
            // way around, a kind with the prefix and no second half —
            // `plugin:git`, which a layout file can name — left the slot with
            // an in-flight request that did not exist: since a live one
            // blocks starting another, that panel never requested again.
            let Some((plugin_id, panel_kind)) = partes(&kind) else {
                continue;
            };
            self.token += 1;
            let token = RequestToken(self.token);
            let opaque_state = state.state.clone();
            self.paneles.entry(id).or_default().en_vuelo = Some((token, firma.clone()));
            let params = norte_proto::methods::PluginPanelRenderParams {
                plugin_id: plugin_id.to_owned(),
                kind: panel_kind.to_owned(),
                dir: firma.dir.clone(),
                cols: firma.cols,
                rows: firma.rows,
                lang: norte_frontend::frame::lang_code().to_owned(),
                cursor_name: firma.cursor.clone(),
                state: opaque_state,
                // A repaint from a context change is the NEUTRAL event. What
                // is missing is the guest receiving the click and the
                // command.
                event: norte_proto::methods::PanelEvent::Refresh,
            };
            let backend = Arc::clone(backend);
            let mailbox = mailbox.clone();
            tokio::spawn(async move {
                let res =
                    match tokio::time::timeout(PLAZO_PLUGINS, backend.plugin_panel_render(params))
                        .await
                    {
                        Ok(r) => r,
                        Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
                    };
                let _ = mailbox
                    .send(Mensaje::PanelContenido(Box::new((id, token, res))))
                    .await;
            });
        }
        Vec::new()
    }

    /// A panel's frame lands: it is shown if the token is that of THAT slot's
    /// last request, and discarded otherwise.
    pub(super) fn aterrizar_panel(
        &mut self,
        slot: u32,
        token: RequestToken,
        res: Result<Option<norte_proto::methods::PanelFrame>, Error>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let state = self.paneles.get_mut(&slot)?;
        // The token is compared by REFERENCE and the signature is MOVED:
        // cloning it meant cloning a path and two strings on every landing,
        // and the old one is not needed for anything else.
        if state.en_vuelo.as_ref().map(|(t, _)| *t) != Some(token) {
            return None;
        }
        let (_, firma) = state.en_vuelo.take()?;
        // The attempt is recorded no matter what: without this, a panel with
        // no plugin to paint it would be retried after every actor message.
        state.firma = Some(firma.clone());
        let Ok(Some(frame)) = res else {
            return None;
        };
        // And that whoever was requested is the one signing it: the frame
        // says which plugin it is from.
        if partes(&firma.kind).map(|(id, _)| id) != Some(frame.plugin_id.as_str()) {
            return None;
        }
        // An IDENTICAL frame does not move the screen, and a whole snapshot
        // for every cursor movement does weigh: a panel that describes the
        // directory — not the row — returns the same thing over and over.
        let new_frame = norte_frontend::frame::StyledFrame::de_wire(&frame);
        let changed = state.frame.as_ref() != Some(&new_frame);
        state.frame = Some(new_frame);
        state.state = frame.state;
        if !changed {
            return None;
        }
        let snap = self.snapshot();
        Some(self.sobre(UiUpdate::Snapshot(Box::new(snap))))
    }

    /// A slot's panel, in the bridge's shape.
    ///
    /// Without a frame yet — the first request in flight, or the plugin
    /// failed — the slot travels with its lines empty: the renderer paints
    /// the border and the title, which is what says the panel is there and
    /// whose it is.
    pub(super) fn vista_de_panel(&self, id: u32) -> crate::dto::PanelSlotView {
        // The kind comes from the TREE — a layout file, `--layout`, or the
        // session —, not from the registry: `validate` checks the shape and
        // keeps kinds this host does not know (ADR 0059), so nobody has
        // required an alphabet of it. It is text that can carry control
        // characters and ends up in the DOM and in an `aria-label`, same as
        // an unknown kind's name, and it is treated the same.
        let kind = kind_de(&self.arbol, SlotId(id)).map_or_else(String::new, |k| {
            partes(k.as_str()).map_or_else(String::new, |(_, panel)| {
                clamp_display(norte_frontend::display_name(panel.as_bytes()).0)
            })
        });
        let state = self.paneles.get(&id);
        let lines = state
            .and_then(|e| e.frame.as_ref())
            .map(|f| {
                f.lines
                    .iter()
                    .map(|line| line.iter().map(super::views::span_view).collect())
                    .collect()
            })
            .unwrap_or_default();
        let hits = state
            .and_then(|e| e.frame.as_ref())
            .map(|f| {
                f.hits
                    .iter()
                    .map(|h| crate::dto::HitView {
                        row: h.row,
                        col: h.col,
                        width: h.width,
                    })
                    .collect()
            })
            .unwrap_or_default();
        crate::dto::PanelSlotView {
            slot_id: id,
            title: kind,
            lines,
            hits,
        }
    }

    /// A cell of a plugin panel was clicked: it is resolved WHICH zone it was
    /// and its command is run.
    ///
    /// The command does not travel over the wire: the frame has it, and it is
    /// here. And it is filtered with [`norte_frontend::frame::zona_puede`],
    /// the same list the terminal applies — the plugin chooses the label AND
    /// the command, and nothing binds them together, so without a filter a
    /// zone that says "Refresh" could name something that copies files. The
    /// consent was to paint.
    ///
    /// What passes the filter goes through the SAME path as the menu and the
    /// panel bar (`efecto_de` + `aplicar_efecto`): a second door into the
    /// catalogue would be a second dispatcher.
    pub(super) fn clic_en_panel(
        &mut self,
        slot: u32,
        row: u16,
        col: u16,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // A HIDDEN slot — behind a tab — keeps its frame, so its zones would
        // keep resolving even though nobody sees them. The renderer does not
        // paint what is hidden, so a click there does not come from a
        // person: the answer is the same the preview gives, "request a
        // snapshot". In the terminal this is not needed because the count
        // starts from the painted rectangle; here the cell arrives over the
        // wire.
        if self.oculto(slot) {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        }
        let command = self
            .paneles
            .get(&slot)
            .and_then(|e| e.frame.as_ref())
            .and_then(|f| f.hit_at(row, col))
            .map(|h| h.command.clone())
            .filter(|c| norte_frontend::frame::zona_puede(c));
        let Some(command) = command else {
            // A cell with no zone, or a zone that names something outside its
            // scope: nothing happens, and it is not a reader error.
            return (self.aplicada(), Vec::new());
        };
        match crate::commands::efecto_de(&command, 1) {
            Some(effect) => self.aplicar_efecto(effect, backend, mailbox),
            None => self.no_implementado(&command),
        }
    }
}

/// `plugin:<id>:<kind>` split into the two halves the RPC needs.
///
/// The separator is the FIRST `:` after the prefix, and it is unambiguous
/// because the alphabet `KindRegistry::insert_panels` validates does not let
/// a colon through in either the id or the kind.
fn partes(kind: &str) -> Option<(&str, &str)> {
    kind.strip_prefix("plugin:")?.split_once(':')
}
