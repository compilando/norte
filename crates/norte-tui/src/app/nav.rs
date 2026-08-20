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
    /// design §D) con `items` YA construidos por [`volume_items`] — `main.rs`
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

    /// Abre el input de nombre del popup de hotlist (`a`), prellenado
    /// vacío. En el popup de historial es no-op (no hay nada que nombrar).
    pub fn nav_popup_open_name_input(&mut self) {
        if let Some(p) = &mut self.nav_popup
            && p.kind == NavPopupKind::Hotlist
        {
            p.name_input = Some(String::new());
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

    /// Refleja en la copia local un favorito YA persistido con éxito
    /// (reemplaza por `name` conservando posición, o añade al final — la
    /// MISMA semántica que `config::persist_hotlist_add`/`load`) y refresca
    /// el popup si está abierto.
    pub fn hotlist_apply_saved(&mut self, name: &str, target: VPath) {
        if let Some(item) = self.hotlist.iter_mut().find(|h| h.name == name) {
            item.target = Ok(target);
        } else {
            self.hotlist.push(crate::config::HotlistItem {
                name: name.to_owned(),
                target: Ok(target),
            });
        }
        self.rebuild_hotlist_popup();
    }

    /// Refleja en la copia local un favorito YA borrado del disco y
    /// refresca el popup si está abierto.
    pub fn hotlist_apply_removed(&mut self, name: &str) {
        self.hotlist.retain(|h| h.name != name);
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
