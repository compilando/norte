//! El popup de navegación visto desde `App`: historial, hotlist y volúmenes,
//! su entrada de teclado y el guardado y borrado de una entrada de hotlist.

use super::nav_popup::{NavItem, NavPopup, NavPopupKind, nav_item_display};
use super::{App, PickerAction, display_name};
use norte_i18n::t;
use norte_proto::VPath;

impl App {
    /// Abre el popup de navegación (spec 2026-07-18): historial del pane
    /// con foco (más reciente primero) o la copia de hotlist. Los items se
    /// construyen YA saneados aquí (`nav_item_display`); una entrada de
    /// hotlist inválida se muestra con su aviso y destino `None`.
    ///
    /// # Panics
    /// Con `NavPopupKind::Volumes`: esos items necesitan un fetch ASYNC
    /// contra `Backend::volumes` que este método (síncrono, sin `Backend`)
    /// no puede hacer — `main.rs` abre ese kind vía
    /// [`Self::open_volumes_popup`], nunca aquí.
    pub fn open_nav_popup(&mut self, kind: NavPopupKind) {
        let enc = self.focused().name_encoding();
        let items: Vec<NavItem> = match kind {
            NavPopupKind::Volumes => unreachable!(
                "Volumes se abre vía `open_volumes_popup` (design §D), nunca `open_nav_popup`"
            ),
            NavPopupKind::History => self.history[self.focus]
                .entries()
                .iter()
                .map(|p| NavItem {
                    display: nav_item_display(None, p, enc),
                    target: Some(p.clone()),
                    hotlist_name: None,
                })
                .collect(),
            NavPopupKind::Hotlist => self
                .hotlist
                .iter()
                .map(|h| {
                    let (display, target) = if let Ok(p) = &h.target {
                        (nav_item_display(Some(&h.name), p, enc), Some(p.clone()))
                    } else {
                        // review MINOR T5: el flag hostil del name NO se
                        // descarta — una inválida con name bidi también
                        // lleva el badge (mismo criterio que el resto).
                        let (name, hostile) = display_name(h.name.as_bytes());
                        let notice = t("hotlist-invalid");
                        let display = if hostile {
                            format!("{} {name} {notice}", crate::ui::HOSTILE_BADGE)
                        } else {
                            format!("{name} {notice}")
                        };
                        (display, None)
                    };
                    NavItem {
                        display,
                        target,
                        hotlist_name: Some(h.name.clone()),
                    }
                })
                .collect(),
        };
        self.nav_popup = Some(NavPopup {
            kind,
            items,
            cursor: 0,
            name_input: None,
            target_pane: self.focus,
            include_pseudo: false,
        });
    }

    /// Abre el popup de volúmenes (`pane.select-drive`/`-left`/`-right`,
    /// design §D) con `items` YA construidos por [`super::nav_popup::volume_items`] — `main.rs`
    /// hace el fetch async contra `Backend::volumes` y llama aquí, mismo
    /// reparto que el resto de este popup: main.rs es I/O, app.rs es estado y
    /// presentación.
    ///
    /// `pane` es el LADO que `Confirm` va a navegar: el foco para
    /// `pane.select-drive`, un lado fijo para `-left`/`-right`
    /// independientemente del foco actual. `include_pseudo` es el modo con el
    /// que se pidió ESTA lista — el toggle de dentro del popup vuelve a
    /// llamar aquí con el valor invertido, así que esto es literalmente una
    /// re-apertura, no un caso especial.
    pub fn open_volumes_popup(&mut self, pane: usize, include_pseudo: bool, items: Vec<NavItem>) {
        self.nav_popup = Some(NavPopup {
            kind: NavPopupKind::Volumes,
            items,
            cursor: 0,
            name_input: None,
            target_pane: pane,
            include_pseudo,
        });
    }

    /// Procesa una acción sobre el popup de navegación. `Confirm` con un
    /// item VÁLIDO cierra el popup y devuelve su destino (el caller navega
    /// por el flujo de cd normal); sobre un item inválido (o sin items) es
    /// no-op — el popup sigue abierto. `Cancel` cierra el `name_input` si
    /// está activo, y si no, el popup. El caller no debe llamar a `Confirm`
    /// con `name_input` activo (Enter ahí confirma el ADD, main.rs).
    pub fn nav_popup_input(&mut self, action: PickerAction) -> Option<VPath> {
        match action {
            PickerAction::Up => {
                if let Some(p) = &mut self.nav_popup {
                    p.up();
                }
                None
            }
            PickerAction::Down => {
                if let Some(p) = &mut self.nav_popup {
                    p.down();
                }
                None
            }
            PickerAction::Confirm => {
                let target = self
                    .nav_popup
                    .as_ref()
                    .and_then(NavPopup::selected)
                    .and_then(|it| it.target.clone());
                if target.is_some() {
                    self.nav_popup = None;
                }
                target
            }
            PickerAction::Cancel => {
                if let Some(p) = &mut self.nav_popup {
                    if p.name_input.is_some() {
                        p.name_input = None;
                    } else {
                        self.nav_popup = None;
                    }
                }
                None
            }
        }
    }

    /// Abre el input de nombre del popup de hotlist (`a`), prellenado con lo
    /// que [`norte_frontend::places::suggested_hotlist_name`] propone para el
    /// dir del pane con foco —el mismo que se va a guardar— ya libre de los
    /// nombres que la hotlist tiene puestos. En el popup de historial es no-op
    /// (no hay nada que nombrar).
    ///
    /// Prellenado y EDITABLE, el mismo molde que el nombre del destino de una
    /// copia: el campo en blanco pedía teclear a mano lo que el path ya
    /// intuía. Vacío sigue queriendo decir cancelar (`main.rs`), así que
    /// borrarlo entero sigue siendo la salida.
    pub fn nav_popup_open_name_input(&mut self) {
        if self
            .nav_popup
            .as_ref()
            .is_none_or(|p| p.kind != NavPopupKind::Hotlist)
        {
            return;
        }
        let ocupados: Vec<&str> = self.hotlist.iter().map(|h| h.name.as_str()).collect();
        let sugerido =
            norte_frontend::places::suggested_hotlist_name(self.focused().dir(), &ocupados);
        if let Some(p) = &mut self.nav_popup {
            p.name_input = Some(sugerido);
        }
    }

    /// El `name` CRUDO del favorito seleccionado (la clave que necesita
    /// `persist_hotlist_remove` — el display del item va saneado y NO sirve
    /// como clave). Sale de la clave CONGELADA en el propio item
    /// ([`NavItem::hotlist_name`]): jamás se indexa `App::hotlist`, que un
    /// hot-reload pudo mutar bajo el popup (review MAJOR T5 — borraría
    /// otro favorito). `None` en historial o sin items.
    #[must_use]
    pub fn nav_popup_selected_hotlist_name(&self) -> Option<String> {
        self.nav_popup.as_ref()?.selected()?.hotlist_name.clone()
    }

    /// Reemplaza la lista de favoritos vigente y la lleva a las dos
    /// superficies que la enseñan.
    ///
    /// La usan el arranque, el hot-reload del `norte.toml` y el cambio de
    /// perfil. Antes cada uno escribía `App::hotlist` a pelo, y el sidebar se
    /// quedaba con la lista de antes sin que nada volviera a tocarlo.
    ///
    /// Un popup ABIERTO no se reconstruye, y eso es lo contrario de lo que
    /// hacen el alta y la baja: sus items son una foto congelada al abrirlo
    /// (ver [`NavPopup`]) porque `dialog.remove` borra por el nombre de la
    /// fila, y una lista que se mueve bajo el cursor por un fichero editado
    /// fuera borraría otra cosa.
    pub fn set_hotlist(&mut self, items: Vec<crate::config::HotlistItem>) {
        self.hotlist = items;
        self.sync_places_favorites();
    }

    /// Refleja en la copia local un favorito YA persistido con éxito
    /// (reemplaza por `name` conservando posición, o añade al final — la
    /// MISMA semántica que `config::persist_hotlist_add`/`load`) y refresca
    /// las dos superficies que la enseñan: el popup abierto y el sidebar.
    pub fn hotlist_apply_saved(&mut self, name: &str, target: VPath) {
        if let Some(item) = self.hotlist.iter_mut().find(|h| h.name == name) {
            item.target = Ok(target);
        } else {
            self.hotlist.push(crate::config::HotlistItem {
                name: name.to_owned(),
                target: Ok(target),
            });
        }
        self.sync_places_favorites();
        self.rebuild_hotlist_popup();
    }

    /// Refleja en la copia local un favorito YA borrado del disco y
    /// refresca las dos superficies que lo enseñaban.
    pub fn hotlist_apply_removed(&mut self, name: &str) {
        self.hotlist.retain(|h| h.name != name);
        self.sync_places_favorites();
        self.rebuild_hotlist_popup();
    }

    /// Reconstruye los items del popup de hotlist tras un add/remove,
    /// conservando el cursor (con clamp): la lista pintada nunca queda
    /// desincronizada de la copia en `App` (el invariante 1:1 de índices).
    fn rebuild_hotlist_popup(&mut self) {
        if let Some(p) = &self.nav_popup
            && p.kind == NavPopupKind::Hotlist
        {
            let cursor = p.cursor;
            self.open_nav_popup(NavPopupKind::Hotlist);
            if let Some(p) = &mut self.nav_popup {
                p.cursor = cursor.min(p.items.len().saturating_sub(1));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::nav_popup::volume_items;
    use crate::app::testutil::*;

    /// Popup de historial (spec 2026-07-18): navegación con `PickerAction`,
    /// Confirm devuelve el destino y cierra, Cancel cierra.
    #[test]
    fn nav_popup_historial_navega_confirma_y_cancela() {
        let mut app = app_dos_panes();
        app.history[0].push(vp("mem:///uno"));
        app.history[0].push(vp("mem:///dos"));
        app.open_nav_popup(NavPopupKind::History);
        assert_eq!(app.nav_popup.as_ref().unwrap().items().len(), 2);
        assert_eq!(
            app.nav_popup.as_ref().unwrap().selected().unwrap().target,
            Some(vp("mem:///dos")),
            "más reciente primero"
        );
        assert_eq!(app.nav_popup_input(PickerAction::Down), None);
        assert_eq!(
            app.nav_popup_input(PickerAction::Confirm),
            Some(vp("mem:///uno")),
            "Confirm devuelve el destino del item resaltado"
        );
        assert!(app.nav_popup.is_none(), "Confirm cierra el popup");

        app.open_nav_popup(NavPopupKind::History);
        assert_eq!(app.nav_popup_input(PickerAction::Cancel), None);
        assert!(app.nav_popup.is_none(), "Cancel cierra el popup");
    }

    /// Un favorito INVÁLIDO (path que no parsea) se muestra con su aviso y
    /// destino `None`: Confirm sobre él es no-op (el popup sigue abierto).
    #[test]
    fn nav_popup_hotlist_item_invalido_no_confirma() {
        let _ = norte_i18n::force(norte_i18n::Lang::Es);
        let mut app = app_dos_panes();
        app.hotlist = vec![crate::config::HotlistItem {
            name: "rota".into(),
            target: Err("err-invalid-path".into()),
        }];
        app.open_nav_popup(NavPopupKind::Hotlist);
        let item = app.nav_popup.as_ref().unwrap().selected().unwrap().clone();
        assert!(item.target.is_none(), "inválida no navega");
        assert!(
            item.display.contains(&norte_i18n::t("hotlist-invalid")),
            "el aviso de inválida se pinta: {}",
            item.display
        );
        assert_eq!(app.nav_popup_input(PickerAction::Confirm), None);
        assert!(app.nav_popup.is_some(), "el popup NO se cierra");
    }

    /// `a` abre el input de nombre SOLO en hotlist; Cancel con input activo
    /// cierra el input (no el popup). `d`: el name CRUDO seleccionado sirve
    /// de clave y el borrado local refresca los items.
    #[test]
    fn nav_popup_hotlist_input_y_borrado() {
        let mut app = app_dos_panes();
        app.hotlist = vec![
            crate::config::HotlistItem {
                name: "uno".into(),
                target: Ok(vp("mem:///uno")),
            },
            crate::config::HotlistItem {
                name: "dos".into(),
                target: Ok(vp("mem:///dos")),
            },
        ];
        app.open_nav_popup(NavPopupKind::Hotlist);
        app.nav_popup_open_name_input();
        assert_eq!(
            app.nav_popup.as_ref().unwrap().name_input.as_deref(),
            Some("/"),
            "`a` abre el input prellenado con el nombre sugerido"
        );
        app.nav_popup_input(PickerAction::Cancel);
        let p = app.nav_popup.as_ref().unwrap();
        assert!(p.name_input.is_none(), "Cancel cierra el input");
        assert!(app.nav_popup.is_some(), "…no el popup");

        assert_eq!(
            app.nav_popup_selected_hotlist_name().as_deref(),
            Some("uno"),
            "el name CRUDO del seleccionado (clave del persist)"
        );
        app.hotlist_apply_removed("uno");
        assert_eq!(app.hotlist.len(), 1);
        let p = app.nav_popup.as_ref().unwrap();
        assert_eq!(p.items().len(), 1, "el popup se refresca tras borrar");
        assert_eq!(p.selected().unwrap().target, Some(vp("mem:///dos")));
    }

    /// review MAJOR T5: un hot-reload con el popup abierto muta
    /// `App.hotlist` mientras el usuario ve la snapshot VIEJA (items
    /// congelados a propósito) — `d` debe borrar lo MOSTRADO (clave
    /// congelada en el item), jamás lo que ahora ocupa ese índice en la
    /// lista nueva (borraría OTRO favorito: pérdida de config).
    #[test]
    fn d_con_popup_desincronizado_borra_el_mostrado() {
        let mut app = app_dos_panes();
        app.hotlist = vec![
            crate::config::HotlistItem {
                name: "uno".into(),
                target: Ok(vp("mem:///uno")),
            },
            crate::config::HotlistItem {
                name: "dos".into(),
                target: Ok(vp("mem:///dos")),
            },
        ];
        app.open_nav_popup(NavPopupKind::Hotlist);
        // Cursor en 0: el usuario VE "uno". Simula el hot-reload que quitó
        // "uno" de la config (la copia en App cambia, el popup no).
        app.hotlist.remove(0);
        assert_eq!(
            app.nav_popup_selected_hotlist_name().as_deref(),
            Some("uno"),
            "la clave es la CONGELADA del popup, no App.hotlist[cursor]"
        );
    }

    /// `a` no abre un campo en blanco: el path ya lo intuye del panel, así que
    /// el nombre también. Y la sugerencia esquiva los nombres que la hotlist ya
    /// tiene puestos —`persist_hotlist_add` REEMPLAZA por nombre, y aceptar sin
    /// leer pisaría un favorito que apuntaba a otro sitio.
    #[test]
    fn el_input_de_nombre_se_prellena_con_el_dir_del_panel() {
        let mut app = crate::app::testutil::app_en("mem:///home/o/norte/src", "mem:///otro");
        app.open_nav_popup(NavPopupKind::Hotlist);
        app.nav_popup_open_name_input();
        assert_eq!(
            app.nav_popup.as_ref().unwrap().name_input.as_deref(),
            Some("src")
        );

        app.nav_popup_input(PickerAction::Cancel);
        app.hotlist = vec![crate::config::HotlistItem {
            name: "src".into(),
            target: Ok(vp("mem:///otro/src")),
        }];
        app.nav_popup_open_name_input();
        assert_eq!(
            app.nav_popup.as_ref().unwrap().name_input.as_deref(),
            Some("norte/src"),
            "ocupado: se cualifica con el padre en vez de pisar"
        );
    }

    /// En el popup de HISTORIAL no hay input de nombre ni name de hotlist.
    #[test]
    fn nav_popup_historial_sin_input_ni_name() {
        let mut app = app_dos_panes();
        app.history[0].push(vp("mem:///uno"));
        app.open_nav_popup(NavPopupKind::History);
        app.nav_popup_open_name_input();
        assert!(app.nav_popup.as_ref().unwrap().name_input.is_none());
        assert_eq!(app.nav_popup_selected_hotlist_name(), None);
    }

    /// `hotlist_apply_saved` reemplaza por name conservando posición o
    /// añade al final (misma semántica que persist/load) y refresca popup.
    #[test]
    fn hotlist_apply_saved_reemplaza_o_anade() {
        let mut app = app_dos_panes();
        app.hotlist = vec![crate::config::HotlistItem {
            name: "uno".into(),
            target: Ok(vp("mem:///viejo")),
        }];
        app.open_nav_popup(NavPopupKind::Hotlist);
        app.hotlist_apply_saved("uno", vp("mem:///nuevo"));
        assert_eq!(app.hotlist.len(), 1, "reemplaza, no duplica");
        assert_eq!(app.hotlist[0].target.as_ref().unwrap(), &vp("mem:///nuevo"));
        app.hotlist_apply_saved("dos", vp("mem:///dos"));
        assert_eq!(app.hotlist.len(), 2, "name nuevo se añade al final");
        assert_eq!(
            app.nav_popup.as_ref().unwrap().items().len(),
            2,
            "el popup abierto refleja el alta"
        );
    }

    /// Un path HOSTIL en el historial sale enmascarado y con el badge como
    /// prefijo — jamás bidi/controles crudos en el popup (spec §6).
    #[test]
    fn nav_popup_sanea_paths_hostiles() {
        let mut app = app_dos_panes();
        app.history[0].push(vp("mem:///evil%E2%80%AEdir"));
        app.open_nav_popup(NavPopupKind::History);
        let display = app
            .nav_popup
            .as_ref()
            .unwrap()
            .selected()
            .unwrap()
            .display
            .clone();
        assert!(!display.contains('\u{202E}'), "sin bidi crudo: {display:?}");
        assert!(display.starts_with('!'), "badge prefijo: {display}");
    }

    /// encoding-auditor MAJOR: `fs_type` looked like a closed, ASCII-only
    /// vocabulary (`ext4`, `nfs4`…) but a FUSE mount's `fuse.<subtype>` is
    /// the `-o subtype=` value an UNPRIVILEGED user picks (`sshfs`, `rclone
    /// mount`…) — exactly as untrusted as a filename. An earlier draft of
    /// `volume_item_display` spliced it in with `{}` and skipped
    /// `display_name` entirely, so a hostile `fs_type` reached the row raw.
    #[test]
    fn volume_row_sanea_fs_type_hostil() {
        let vol = norte_proto::methods::Volume {
            mount: vp("mem:///media/usb"),
            label: None,
            fs_type: "fuse.evil\u{202E}type".to_owned(),
            kind: norte_proto::methods::VolumeKind::Fixed,
            total_bytes: None,
            free_bytes: None,
            read_only: false,
        };
        let items = volume_items(std::slice::from_ref(&vol), None);
        let display = items[0].display.clone();
        assert!(!display.contains('\u{202E}'), "sin bidi crudo: {display:?}");
        assert!(display.starts_with('!'), "badge prefijo: {display}");
    }

    /// V3.5 (encoding-auditor MAJOR deferred from V3): `label` is
    /// `Option<Vec<u8>>` end to end now, so a non-UTF-8 label reaches this
    /// row as the ORIGINAL bytes — not a lossy `String` some earlier layer
    /// already mangled — and goes through the exact same masking `fs_type`
    /// gets above. Bytes `\xFF\xFE` are not valid UTF-8 in any position, so
    /// `display_name` must fall back to lossy rendering AND mark it hostile.
    #[test]
    fn volume_row_sanea_label_no_utf8() {
        let vol = norte_proto::methods::Volume {
            mount: vp("mem:///media/usb"),
            label: Some(vec![0xFF, 0xFE, b'X']),
            fs_type: "vfat".to_owned(),
            kind: norte_proto::methods::VolumeKind::Removable,
            total_bytes: None,
            free_bytes: None,
            read_only: false,
        };
        let items = volume_items(std::slice::from_ref(&vol), None);
        let display = items[0].display.clone();
        assert!(display.starts_with('!'), "badge prefijo: {display}");
        assert!(
            display.contains('\u{FFFD}'),
            "el label no-UTF8 se pinta lossy: {display}"
        );
        assert_eq!(
            items[0].target,
            Some(vp("mem:///media/usb")),
            "el target sigue siendo el mount real, ajeno al label"
        );
    }

    /// V3.5 (encoding-auditor MINOR: the hand-picked byte string above is
    /// not the canonical corpus): every hostile name in
    /// `norte_testkit::corpus::hostile_names()`, used as a LABEL, must reach
    /// the row without panicking, badged EXACTLY when `display_name` alone
    /// says that name comes out altered — the same function
    /// `volume_item_display` calls, so this pins agreement rather than
    /// reimplementing the masking rule a second time. `target` stays the
    /// clean mount throughout: a hostile label must never leak into
    /// Enter-to-navigate.
    ///
    /// #169's `archive_marker_literal` (a label whose own CLEAN text is
    /// `"!"`, the same glyph as [`crate::ui::HOSTILE_BADGE`]) caught this
    /// assertion checking `display.starts_with('!')` — true for that
    /// fixture even with `label_hostil == false`, because the UN-badged
    /// label prefix (`"{label} — "`) itself starts with `!`. A leading `!`
    /// is not proof of a badge, and even `"! "` is not enough: that fixture's
    /// clean prefix is `"! — "`, which also starts with `"! "`. Nothing
    /// short of the FULL string settles it, so the expected display is
    /// rebuilt here from the same primitives `volume_item_display` calls
    /// (`display_name`, `path_display_with`, `t`) — not the masking rule
    /// itself, only the template it is spliced into — and compared for
    /// EXACT equality.
    #[test]
    fn volume_label_hostile_corpus_sweep() {
        let mount = vp("mem:///media/usb");
        let (path_text, path_hostile) = norte_frontend::path_display_with(&mount, None);
        assert!(
            !path_hostile,
            "control: el mount fijo del test no es hostil"
        );
        let (fs_text, fs_hostile) = display_name(b"vfat");
        assert!(!fs_hostile, "control: \"vfat\" no es hostil");
        let sizes = format!("{u} / {u}", u = t("volumes-size-unknown"));
        for fixture in norte_testkit::corpus::hostile_names() {
            let vol = norte_proto::methods::Volume {
                mount: mount.clone(),
                label: Some(fixture.bytes.clone()),
                fs_type: "vfat".to_owned(),
                kind: norte_proto::methods::VolumeKind::Removable,
                total_bytes: None,
                free_bytes: None,
                read_only: false,
            };
            let items = volume_items(std::slice::from_ref(&vol), None);
            let display = &items[0].display;
            let (label_text, label_hostile) = display_name(&fixture.bytes);
            let body = format!("{label_text} — {path_text}  {fs_text}  {sizes}");
            let expected = if label_hostile {
                format!("{} {body}", crate::ui::HOSTILE_BADGE)
            } else {
                body
            };
            assert_eq!(
                display, &expected,
                "{}: badge debe coincidir con display_name({:?})",
                fixture.id, fixture.bytes
            );
            assert_eq!(
                items[0].target,
                Some(mount.clone()),
                "{}: el target sigue siendo el mount, ajeno al label",
                fixture.id
            );
        }
    }

    /// The everyday case: an ordinary `fs_type` and absent sizes (the
    /// filesystem never answered `statvfs` in time, design §A) render with
    /// NO badge and say `volumes-size-unknown` rather than a bare zero — a
    /// zero here would read as "full", the opposite of "unknown".
    #[test]
    fn volume_row_talla_ausente_no_es_cero() {
        let vol = norte_proto::methods::Volume {
            mount: vp("mem:///media/usb"),
            label: Some(b"USB".to_vec()),
            fs_type: "vfat".to_owned(),
            kind: norte_proto::methods::VolumeKind::Removable,
            total_bytes: None,
            free_bytes: None,
            read_only: false,
        };
        let items = volume_items(std::slice::from_ref(&vol), None);
        let display = items[0].display.clone();
        assert!(!display.starts_with('!'), "nada hostil aquí: {display}");
        assert!(!display.contains('0'), "ausente no es cero: {display}");
        assert_eq!(items[0].target, Some(vp("mem:///media/usb")));
    }

    /// review MINOR T5: una entrada INVÁLIDA con name hostil también lleva
    /// el badge (antes el flag de `display_name` se descartaba en ese brazo).
    #[test]
    fn hotlist_invalida_con_name_hostil_lleva_badge() {
        let mut app = app_dos_panes();
        app.hotlist = vec![crate::config::HotlistItem {
            name: "evil\u{202E}name".into(),
            target: Err("err-invalid-path".into()),
        }];
        app.open_nav_popup(NavPopupKind::Hotlist);
        let display = app
            .nav_popup
            .as_ref()
            .unwrap()
            .selected()
            .unwrap()
            .display
            .clone();
        assert!(!display.contains('\u{202E}'), "sin bidi crudo: {display:?}");
        assert!(display.starts_with('!'), "badge prefijo: {display}");
    }
}
