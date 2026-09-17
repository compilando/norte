//! La barra de menús: las mismas órdenes que el teclado, ordenadas por tema.
//!
//! No añade capacidades. Añade una forma de ENCONTRARLAS: la paleta pide que
//! sepas el nombre de lo que buscas y `F1` pide que leas, mientras que un menú
//! se recorre. Es la vía para quien llega de un gestor con menús y para quien
//! usa el ratón, y su contenido son ids del catálogo compartido — un comando
//! que no exista aquí no puede aparecer en un menú.

/// Un menú: su título y las órdenes que lista, por id.
#[derive(Debug, Clone, Copy)]
pub struct Menu {
    /// Clave Fluent del título (`menu-*`).
    pub title: &'static str,
    /// Ids de comando, en el orden en que se pintan.
    pub items: &'static [&'static str],
}

/// Los menús, de izquierda a derecha.
///
/// Ni un id inventado: un test comprueba que todos existen en el catálogo y
/// que ninguno está declarado `Planned`, porque un menú que ofrece algo que no
/// está construido es peor que no tener menú.
pub const MENUS: &[Menu] = &[
    // Diez grupos por lo que el lector QUIERE HACER, no por dónde vive el
    // comando: leer un fichero, cambiarlo, elegir sobre qué, ir a otro sitio,
    // mover los paneles, las pestañas, buscar, qué se ve, las herramientas y
    // la ayuda. Todo lo construido está en alguno; nada en dos.
    Menu {
        title: "menu-file",
        items: &[
            "pane.view",
            "pane.edit",
            "pane.edit-new",
            "pane.open",
            // #139: las propiedades son del FICHERO, así que van con lo que se
            // hace a un fichero, no con lo que se cambia de la pantalla.
            "pane.properties",
            "pane.dir-size",
            "pane.copy-path",
            "app.quit",
        ],
    },
    // Lo que ESCRIBE: aparte de lo que solo lee, porque es lo que pasa por el
    // journal y lo que un lector quiere encontrar junto.
    Menu {
        title: "menu-operate",
        items: &[
            "pane.copy",
            "pane.move",
            "pane.rename",
            "pane.rename-batch",
            "pane.ai-rename",
            "pane.mkdir",
            "pane.delete",
            "pane.delete-permanent",
            "pane.chmod",
            "pane.pack",
            "pane.unpack",
            "pane.test-archive",
            "pane.split-file",
            "pane.combine-files",
            "pane.checksum",
            "pane.checksum-verify",
        ],
    },
    Menu {
        title: "menu-mark",
        items: &[
            "mark.toggle",
            "mark.all",
            "mark.invert",
            "mark.clear",
            "mark.restore",
            "mark.pattern-add",
            "mark.pattern-remove",
            "mark.extension-add",
            "mark.extension-remove",
            "mark.files",
            "mark.dirs",
        ],
    },
    // A DÓNDE mira un panel: subir, volver, favoritos, volúmenes, conectar.
    // #140 los ponía en Paneles por esa misma razón; con un menú propio de
    // navegación, es aquí donde se buscan.
    Menu {
        title: "menu-go",
        items: &[
            "nav.parent",
            "nav.back",
            "nav.forward",
            "nav.jump-back",
            "nav.set-jump-point",
            "pane.history",
            "pane.history-left",
            "pane.history-right",
            "pane.popular",
            "pane.hotlist",
            "pane.select-drive",
            "pane.connect",
            "pane.disconnect",
            "pane.refresh",
            "pane.command-line",
            "app.terminal",
        ],
    },
    Menu {
        title: "menu-panels",
        items: &[
            "pane.switch",
            "layout.focus-next",
            "layout.focus-prev",
            "pane.mirror",
            "pane.mirror-target",
            "pane.pull",
            "pane.swap",
            "layout.split-h",
            "layout.split-v",
            "layout.close-slot",
            "layout.grow",
            "layout.shrink",
            "layout.equalize",
            "layout.set-target",
            "app.toggle-panels",
        ],
    },
    Menu {
        title: "menu-tabs",
        items: &[
            "pane.tab-new",
            "pane.tab-close",
            "pane.tab-next",
            "pane.tab-prev",
            "pane.tab-move-left",
            "pane.tab-move-right",
        ],
    },
    Menu {
        title: "menu-find",
        items: &[
            "pane.quick-search",
            "pane.search",
            "pane.semantic-search",
            "pane.compare-files",
            "pane.compare-dirs",
            "pane.sync-dirs",
        ],
    },
    Menu {
        title: "menu-view",
        items: &[
            "pane.toggle-hidden",
            "pane.columns",
            // #138: el orden es de la VISTA, y aquí es donde se cambia lo que
            // la vista enseña.
            "pane.sort-menu",
            "pane.names-encoding",
            // #136: el árbol es otra columna de navegación al lado del
            // listado, como el sidebar.
            "pane.tree",
            "layout.places",
            "layout.preview",
            "layout.processes",
            "layout.metadata",
            // #323: el registro va junto a procesos, que es su vecino de
            // sentido — los dos contestan «¿qué está haciendo esto?».
            "layout.log",
            // Fase 4: el mapa de disco va con sus vecinos de sentido — los
            // tres contestan «¿qué está pasando aquí?», y este además «¿en qué
            // se ha ido el sitio?».
            "layout.disk-map",
            "layout.pick",
            "app.theme",
        ],
    },
    // Lo que se administra: extensiones, agentes, ajustes, perfiles. La
    // paleta va aquí y no en Ayuda, porque desde ella se HACE.
    Menu {
        title: "menu-tools",
        items: &[
            "app.extensions",
            "app.agents",
            "app.settings",
            "profile.pick",
            "profile.save-as",
            "app.palette",
        ],
    },
    Menu {
        title: "menu-help",
        items: &["app.help"],
    },
];

/// Qué menú está abierto y en qué orden va el cursor.
///
/// Puro y sin render: el TUI lo pinta y la GUI lo pintará distinto, pero
/// recorrer un menú no se decide dos veces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuState {
    menu: usize,
    item: usize,
}

impl Default for MenuState {
    fn default() -> Self {
        Self::new()
    }
}

impl MenuState {
    /// El primer menú, primer elemento.
    #[must_use]
    pub const fn new() -> Self {
        Self { menu: 0, item: 0 }
    }

    /// Reabre por el menú que estaba abierto la última vez.
    ///
    /// Un menú que siempre se abre por el primero obliga a recorrer la barra
    /// entera cada vez, y quien usa dos entradas del mismo menú lo paga en
    /// cada gesto. Un índice que ya no existe —la barra cambió entre una
    /// apertura y la siguiente— cae al primero en vez de no abrir nada.
    ///
    /// El CURSOR sí vuelve al principio: dentro de un menú la lista es corta y
    /// se lee entera, y recordar también la fila haría que la misma tecla
    /// ejecutara cosas distintas según lo último que se rozó.
    ///
    /// Vive aquí porque es una decisión de presentación y los dos frontends
    /// tienen que tomarla igual: un menú que en la ventana recuerda y en el
    /// terminal no son dos programas.
    #[must_use]
    pub fn reopen_at(menu: usize) -> Self {
        let mut estado = Self::new();
        estado.open(menu);
        estado
    }

    /// Qué menú está abierto.
    #[must_use]
    pub const fn menu(&self) -> usize {
        self.menu
    }

    /// Qué elemento va resaltado.
    #[must_use]
    pub const fn item(&self) -> usize {
        self.item
    }

    /// El id del comando resaltado.
    #[must_use]
    pub fn selected(&self) -> Option<&'static str> {
        MENUS.get(self.menu)?.items.get(self.item).copied()
    }

    /// Cambia de menú, ciclando. El cursor vuelve al primero: mantenerlo
    /// donde estaba lo dejaría en un elemento que el menú nuevo no tiene.
    pub fn cycle_menu(&mut self, delta: isize) {
        let n = MENUS.len();
        if n == 0 {
            return;
        }
        let i = isize::try_from(self.menu).unwrap_or(0);
        self.menu =
            usize::try_from((i + delta).rem_euclid(isize::try_from(n).unwrap_or(1))).unwrap_or(0);
        self.item = 0;
    }

    /// Mueve el cursor dentro del menú abierto, ciclando.
    pub fn cycle_item(&mut self, delta: isize) {
        let Some(n) = MENUS.get(self.menu).map(|m| m.items.len()) else {
            return;
        };
        if n == 0 {
            return;
        }
        let i = isize::try_from(self.item).unwrap_or(0);
        self.item =
            usize::try_from((i + delta).rem_euclid(isize::try_from(n).unwrap_or(1))).unwrap_or(0);
    }

    /// Abre un menú por índice y pone el cursor al principio.
    pub fn open(&mut self, menu: usize) {
        if menu < MENUS.len() {
            self.menu = menu;
            self.item = 0;
        }
    }

    /// Pone el cursor en un elemento del menú abierto.
    pub fn point_at(&mut self, item: usize) {
        if MENUS.get(self.menu).is_some_and(|m| item < m.items.len()) {
            self.item = item;
        }
    }
}

#[cfg(test)]
mod tests {
    /// Cada ítem del menú tiene ETIQUETA en los dos idiomas.
    ///
    /// Sin esto, un comando nuevo sale en el menú con su clave cruda
    /// —`menu-item-pane-properties` en mitad de la lista—, que es exactamente
    /// lo que pasó al añadir los de #138 y #139: la suite entera en verde y la
    /// pantalla enseñando el identificador. El menú lo pinta el frontend, así
    /// que el gate vive aquí.
    #[test]
    fn cada_item_del_menu_tiene_etiqueta_en_los_dos_idiomas() {
        for lang in [norte_i18n::Lang::Es, norte_i18n::Lang::En] {
            let _ = norte_i18n::force(lang);
            for menu in MENUS {
                let titulo = norte_i18n::t(menu.title);
                assert!(
                    !titulo.is_empty() && titulo != menu.title,
                    "{lang:?}: el menú {} no tiene título",
                    menu.title
                );
                for id in menu.items {
                    let clave = format!("menu-item-{}", id.replace('.', "-"));
                    let etiqueta = norte_i18n::t(&clave);
                    assert!(
                        !etiqueta.is_empty() && etiqueta != clave,
                        "{lang:?}: {id} sale en el menú sin etiqueta ({clave})"
                    );
                }
            }
        }
    }

    use super::*;
    use crate::keymap::catalogue::{Status, lookup};

    /// Ni un id inventado, y ninguno `Planned`: un menú que ofrece algo que no
    /// está construido es peor que no tener menú — el lector lo pulsa y no
    /// pasa nada, sin explicación.
    #[test]
    fn todo_lo_que_ofrece_un_menu_existe_y_esta_construido() {
        for m in MENUS {
            for id in m.items {
                let def = lookup(id).unwrap_or_else(|| panic!("{id} no está en el catálogo"));
                assert_eq!(def.status, Status::Live, "{id} está declarado Planned");
            }
        }
    }

    /// Ningún comando en dos menús: dos sitios para lo mismo es un menú que
    /// no enseña dónde están las cosas.
    #[test]
    fn ningun_comando_esta_en_dos_menus() {
        let mut vistos = std::collections::BTreeSet::new();
        for m in MENUS {
            for id in m.items {
                assert!(vistos.insert(*id), "{id} aparece en dos menús");
            }
        }
    }

    #[test]
    fn cambiar_de_menu_devuelve_el_cursor_al_principio() {
        let mut s = MenuState::new();
        s.cycle_item(2);
        assert_eq!(s.item(), 2);
        s.cycle_menu(1);
        assert_eq!(s.menu(), 1);
        assert_eq!(s.item(), 0, "el elemento 2 puede no existir aquí");
    }

    #[test]
    fn los_dos_recorridos_ciclan() {
        let mut s = MenuState::new();
        s.cycle_menu(-1);
        assert_eq!(s.menu(), MENUS.len() - 1);
        s.cycle_item(-1);
        assert_eq!(s.item(), MENUS[MENUS.len() - 1].items.len() - 1);
    }

    /// Apuntar fuera de rango NO mueve el cursor: el emisor de índices es el
    /// ratón, y un índice imposible es un bug nuestro, no algo que deba dejar
    /// el cursor sobre un elemento que no existe.
    #[test]
    fn apuntar_fuera_de_rango_no_mueve_nada() {
        let mut s = MenuState::new();
        s.point_at(999);
        assert_eq!(s.item(), 0);
    }
}
