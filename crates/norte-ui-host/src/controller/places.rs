//! The places bar: drives and favorites.
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
    /// Focus is on the places side bar.
    pub(super) fn sitios_tienen_el_foco(&self) -> bool {
        self.sitios.is_some()
            && self
                .roles
                .get(RoleId::Active)
                .is_some_and(|s| self.hueco_de_sitios() == Some(s))
    }

    /// Movement and activation, with focus on the side bar.
    ///
    /// The vocabulary is the LISTING's because it is the only map this window
    /// has — there is no `dialog` screen here — and each command means in the
    /// bar what it means on its own surface: down moves down through it,
    /// enter goes to the place, and the mark key COLLAPSES, because a side
    /// bar has nothing to mark and does have two sections to open and close.
    ///
    /// Activation needs the backend, so `None` is returned for
    /// `aplicar_efecto` to handle through its normal path; here only the
    /// cursor moves.
    pub(super) fn efecto_en_sitios(
        &mut self,
        efecto: Efecto,
    ) -> Option<(ActionAck, Vec<BridgeEnvelope<UiUpdate>>)> {
        // ITS OWN, before counting rows: for the same reason as the process
        // panel, an empty panel that answers "applied" to everything
        // swallows the key that gets you out of it.
        if !matches!(
            efecto,
            Efecto::Cursor(_) | Efecto::Pagina(_) | Efecto::Extremo { .. }
        ) {
            return None;
        }
        let state = self.sitios.as_mut()?;
        let rows = state.rows().len();
        if rows == 0 {
            return Some((self.aplicada(), Vec::new()));
        }
        let total = i64::try_from(rows).unwrap_or(i64::MAX);
        let current = i64::try_from(state.cursor().min(rows - 1)).unwrap_or(0);
        let target = match efecto {
            Efecto::Cursor(n) => current.saturating_add(n.clamp(-total, total)),
            Efecto::Pagina(n) => {
                current.saturating_add(n.clamp(-total, total).saturating_mul(total))
            }
            Efecto::Extremo { al_final: false } => 0,
            Efecto::Extremo { al_final: true } => total - 1,
            // Everything else goes its own way. Entering and collapsing, in
            // particular, need the backend — a navigation, or asking for the
            // volumes again — so whoever does have it handles them.
            _ => return None,
        };
        state.set_cursor(usize::try_from(target.max(0)).unwrap_or(0).min(rows - 1));
        let snap = self.snapshot();
        Some((
            self.aplicada(),
            vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))],
        ))
    }

    /// Feeds the side bar with the configuration's favorites.
    ///
    /// From the config the window STARTED with, which is the one in use. A
    /// favorite whose path does not parse is kept with its error key: the
    /// hotlist is user data, not structural configuration, and one that
    /// silently disappears is a failure nobody can see.
    pub(super) fn sembrar_sitios(&mut self) {
        let items: Vec<(String, Result<VPath, String>)> = self
            .config
            .common
            .hotlist
            .iter()
            .map(|h| (h.name.clone(), h.target.clone()))
            .collect();
        let state = self
            .sitios
            .get_or_insert_with(norte_frontend::places::PlacesState::new);
        state.set_favorites(&items);
        self.gen_sitios += 1;
    }

    /// Requests the volumes for the side bar.
    ///
    /// Called by startup and by expanding the drives section. And nobody
    /// else: a side bar with a clock would break ADR 0058's suspension rule
    /// from the first frame, and `host.volumes` is not free — it mounts and
    /// queries space on every filesystem.
    pub(super) fn pedir_sitios(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) {
        if self.hueco_de_sitios().is_none() {
            return;
        }
        let backend = Arc::clone(backend);
        let mailbox = mailbox.clone();
        tokio::spawn(async move {
            let res = match tokio::time::timeout(PLAZO_PLUGINS, backend.volumes()).await {
                Ok(r) => r,
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            let _ = mailbox
                .send(Mensaje::Fondo(Box::new(Fondo::SitiosVolumenes(res))))
                .await;
        });
    }

    /// The volumes arrived at the side bar.
    ///
    /// A failure does NOT empty whatever was there: what was seen is still
    /// the last thing the host said.
    pub(super) fn aplicar_sitios(
        &mut self,
        res: Result<Vec<norte_proto::methods::Volume>, Error>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let Ok(vols) = res else {
            return None;
        };
        // Only if the bar EXISTS in this layout. `get_or_insert_with` used to
        // create a state — without favorites, because `sembrar_sitios` does
        // not run — for a late response from a layout that no longer has a
        // `places` slot, and then it sent a whole snapshot for nothing.
        // `pedir_sitios` already guards the same way.
        self.hueco_de_sitios()?;
        self.sitios
            .get_or_insert_with(norte_frontend::places::PlacesState::new)
            .set_drives(&vols);
        // Drives are inserted BEFORE favorites: every index painted until now
        // names a different row.
        self.gen_sitios += 1;
        let snap = self.snapshot();
        Some(self.sobre(UiUpdate::Snapshot(Box::new(snap))))
    }

    /// A click on a side bar row: selects it AND activates it.
    pub(super) fn activar_sitio(
        &mut self,
        row: u32,
        generation: u64,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if generation != self.gen_sitios {
            // What was clicked and what is there now are not the same list:
            // the volumes land IN THE MIDDLE. Rejecting is the only correct
            // thing — `set_cursor` clamps to the last one, so going ahead
            // would have navigated to the bar's last place.
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        }
        let Some(state) = self.sitios.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        if row as usize >= state.rows().len() {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        }
        state.set_cursor(row as usize);
        self.activar_sitio_del_cursor(backend, mailbox)
    }

    /// Activates the side bar's cursor row: navigates to it, or collapses its
    /// section if it is a header.
    ///
    /// The `cd` goes to the FOCUSED listing through the same path as any
    /// other: that is what makes having the bar open not change where
    /// operations go.
    pub(super) fn activar_sitio_del_cursor(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(state) = self.sitios.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        if let Some(target) = state.activate().cloned() {
            return (
                self.aplicada(),
                self.navegar(&target, Trail::Record, backend, mailbox),
            );
        }
        // A header: it collapses. And expanding the drives IS the moment to
        // request them again — a disk mounted or unmounted since the window
        // opened shows up here, with no clock involved.
        state.toggle_fold();
        self.gen_sitios += 1;
        let expanded = state
            .rows()
            .iter()
            .any(|r| matches!(r, norte_frontend::places::PlaceRow::Drive { .. }));
        if expanded {
            self.pedir_sitios(backend, mailbox);
        }
        let snap = self.snapshot();
        (
            self.aplicada(),
            vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))],
        )
    }

    /// The places side bar, projected.
    ///
    /// If there is no state yet — the layout places it but nobody has fed it
    /// — it is projected EMPTY with its two headers, which is what the shared
    /// model does: the list does not jump when the volumes arrive.
    pub(super) fn barra_de_sitios(&self, id: u32) -> crate::dto::PlacesSlotView {
        use norte_frontend::places::{PlaceRow, PlacesState};
        let generation = self.gen_sitios;

        let empty = PlacesState::new();
        let state = self.sitios.as_ref().unwrap_or(&empty);
        let rows = state
            .rows()
            .iter()
            .map(|r| match r {
                PlaceRow::Header { section, folded } => crate::dto::PlaceRowView::Header {
                    label: clamp_display(norte_i18n::t_in(self.lang, section.label_key())),
                    folded: *folded,
                },
                PlaceRow::Drive {
                    label,
                    mount,
                    free,
                    total,
                    read_only,
                    kind,
                } => {
                    // The label is BYTES and the mount point a `VPath`: both
                    // through the shared gate, never through
                    // `to_string_lossy`. The SHORT name is decided by
                    // `drive_name`, the same one the TUI uses.
                    let (displayable, hostile) = norte_frontend::places::drive_name(label, mount);
                    // The whole mount goes into the title, MASKED; the flag
                    // is that of the name that gets PAINTED, the same one the
                    // TUI sees (one flag for what is not on display was a
                    // mark that only ever showed up on one of the two).
                    let (mount_text, _) = norte_frontend::display::path_display(mount);
                    crate::dto::PlaceRowView::Drive {
                        label: clamp_display(displayable),
                        hostile,
                        detail: clamp_display(self.espacio_de(*free, *total, *read_only)),
                        // A `?` when it did not answer, never a zero: that
                        // would read as "full".
                        free: free
                            .map_or_else(|| "?".to_owned(), norte_frontend::human_bytes_short),
                        mount: clamp_display(mount_text),
                        kind: match kind {
                            norte_proto::methods::VolumeKind::Fixed => "fixed",
                            norte_proto::methods::VolumeKind::Removable => "removable",
                            norte_proto::methods::VolumeKind::Network => "network",
                            _ => "unknown",
                        }
                        .to_owned(),
                    }
                }
                PlaceRow::Favorite { name, target } => {
                    let (target_text, target_hostile) = match target {
                        Ok(v) => norte_frontend::display::path_display(v),
                        Err(_) => (String::new(), false),
                    };
                    let (display_name, name_hostile) =
                        norte_frontend::display_name(name.as_bytes());
                    crate::dto::PlaceRowView::Favorite {
                        // The name is typed by the user, but it can come from
                        // the PROJECT layer: it is masked the same way.
                        name: clamp_display(display_name),
                        target: clamp_display(target_text),
                        // The name OR the target. The flag used to document
                        // the target and the name was masked by dropping its
                        // own, so a favorite named with a bidi override
                        // arrived with no flag at all.
                        hostile: target_hostile || name_hostile,
                        broken: target.as_ref().err().map_or_else(String::new, |key| {
                            clamp_display(norte_i18n::t_in(self.lang, key))
                        }),
                    }
                }
            })
            .collect();
        crate::dto::PlacesSlotView {
            slot_id: id,
            rows,
            cursor: state.cursor() as u64,
            generation,
        }
    }

    /// A volume's space, said out loud.
    ///
    /// A size the system did not answer is SAID: a `0` reads as "full", which
    /// is the opposite of "I don't know".
    pub(super) fn espacio_de(
        &self,
        free: Option<u64>,
        total: Option<u64>,
        read_only: bool,
    ) -> String {
        // The SHARED crate drafts the phrase. There used to be three versions
        // — two in this same crate — and they already differed in how they
        // write the numbers, and all three said "unknown" when the only thing
        // missing was the total, throwing away the data that was there.
        norte_frontend::places::PlacesState::volume_detail(free, total, read_only, true, self.lang)
    }

    /// The slot the side bar occupies, if the layout places one.
    pub(super) fn hueco_de_sitios(&self) -> Option<SlotId> {
        self.reparto
            .placements
            .iter()
            .map(|(s, _)| *s)
            .find(|s| kind_de(&self.arbol, *s).is_some_and(|k| k.as_str() == "places"))
    }

    /// The PLACED `metadata` slots, and what each would show NOW.
    ///
    /// From the layout, not the tree, like the viewer: a slot behind a tab
    /// exists but is not visible.
    fn huecos_de_hoja(&self) -> Vec<SlotId> {
        self.reparto
            .placements
            .iter()
            .map(|(s, _)| *s)
            .filter(|s| kind_de(&self.arbol, *s).is_some_and(|k| k.as_str() == "metadata"))
            .collect()
    }

    /// Brings up to date what each placed sheet shows, and returns a snapshot
    /// if any changed.
    ///
    /// The sheet FOLLOWS the cursor, and any message moves the cursor: a key,
    /// a click, a listing that lands. The TUI resolves this for free because
    /// it recomputes every frame; here it has to be asked after every
    /// message, exactly like the docked preview (`sondear_previews`).
    ///
    /// Without this, the sheet had NO path of its own to the renderer at all:
    /// it rode piggyback on the whole snapshot another panel triggered, so a
    /// layout with a sheet and no viewer left it frozen at whatever was there
    /// on startup. `SelectRow` — the click — answers with a row patch, and
    /// the sheet does not go in that.
    ///
    /// It requests nothing and launches nothing: comparing costs whatever it
    /// costs to build the sheet, which comes from the listing already in
    /// memory.
    pub(super) fn sondear_hojas(&mut self) -> Vec<BridgeEnvelope<UiUpdate>> {
        let alive: Vec<u32> = self
            .arbol
            .slot_ids()
            .into_iter()
            .map(|SlotId(id)| id)
            .collect();
        self.hojas.retain(|id, _| alive.contains(id));
        let mut changed = false;
        for slot in self.huecos_de_hoja() {
            let SlotId(id) = slot;
            let current = self.hoja_de_atributos(slot);
            if self.hojas.get(&id) != Some(&current) {
                self.hojas.insert(id, current);
                changed = true;
            }
        }
        if changed {
            let snap = self.snapshot();
            vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))]
        } else {
            Vec::new()
        }
    }

    /// The attribute sheet of a `metadata` slot.
    ///
    /// What it shows comes from the panel this slot FOLLOWS, resolved with
    /// the shared engine: a slot following a role that has been left without
    /// a panel degrades to the active one instead of silently looking at
    /// nothing.
    ///
    /// It requests nothing: the listing already brought the `Entry`.
    pub(super) fn hoja_de_atributos(&self, slot: SlotId) -> crate::dto::MetadataSlotView {
        let SlotId(id) = slot;
        let mut diags = Vec::new();
        let followed =
            norte_frontend::layout::resolve_follow(&self.arbol, slot, &self.roles, &mut diags)
                .or_else(|| self.roles.get(norte_frontend::layout::RoleId::Active));
        // With FOCUS on the sheet itself, the active role is the sheet, and
        // following yourself is following nobody: so the active listing takes
        // over, which always exists (same fix as the docked preview, #291).
        let pane = followed
            .and_then(|SlotId(s)| self.huecos.get(&s))
            .or_else(|| self.huecos.get(&self.activo()))
            .map(|h| &h.pane);
        // `cursor_entry` and not `selected`: the sheet DESCRIBES what is under
        // the cursor, and on the `..` row — where the cursor is born —
        // "pointed at" is `None` on purpose. Asking for the selection
        // instead, the panel came out empty on every startup and after every
        // `cd`.
        // The flag comes from the SAME index as the entry (`cursor_entry` /
        // `cursor_is_parent_row`): asking `cursor()` by hand, a quick-search
        // filter — which chooses on its own and does not move the real cursor
        // — left the sheet describing `..` while the listing highlighted a
        // different row.
        let entry = pane.and_then(|p| p.cursor_entry());
        let parent_row = pane.is_some_and(PaneState::cursor_is_parent_row);
        // WHAT it follows, for the title. With the SAME name reinterpretation
        // as that listing's header, which is the path the reader has in
        // front of them to compare against.
        let (follows, follows_hostile) = pane.map_or_else(
            || (String::new(), false),
            |p| norte_frontend::path_display_with(p.dir(), p.name_encoding()),
        );
        let follows = clamp_display(follows);
        let Some(e) = entry else {
            return crate::dto::MetadataSlotView {
                slot_id: id,
                fields: Vec::new(),
                note: clamp_display(norte_i18n::t_in(self.lang, "metadata-empty")),
                follows_display: follows,
                follows_hostile,
            };
        };
        // Which fields go inside is decided by the SHARED crate, not this
        // host: the TUI paints the same sheet, and back when each had its own
        // copy they already diverged (the TUI did not flag a hostile
        // attribute value). Here only what crosses the bridge is clamped.
        let catalog = self.catalogos.get(e.path.scheme());
        let fields = norte_frontend::metadata::sheet(e, parent_row, catalog, self.lang)
            .into_iter()
            .map(|f| crate::dto::MetadataFieldView {
                label: clamp_display(f.label),
                value: clamp_display(f.value),
                hostile: f.hostile,
            })
            .collect();
        crate::dto::MetadataSlotView {
            slot_id: id,
            fields,
            note: String::new(),
            follows_display: follows,
            follows_hostile,
        }
    }
}
