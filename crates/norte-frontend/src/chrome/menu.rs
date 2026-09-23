//! La barra de menús: las mismas órdenes que el teclado, ordenadas por tema.
//!
//! No añade capacidades. Añade una forma de ENCONTRARLAS: la paleta pide que
//! sepas el nombre de lo que buscas y `F1` pide que leas, mientras que un menú
//! se recorre. Es la vía para quien llega de un gestor con menús y para quien
//! usa el ratón, y su contenido son ids del catálogo compartido — un comando
//! que no exista aquí no puede aparecer en un menú.
//!
//! Dentro de un menú, las órdenes van en SECCIONES (ADR 0125): diecisiete
//! entradas seguidas se leen como una lista que hay que recorrer entera, y
//! cinco grupos de tres se leen de un vistazo. El cursor no ve las secciones —
//! recorre las órdenes como una sola lista—; las ven los que pintan.

/// Un grupo de órdenes dentro de un menú.
#[derive(Debug, Clone, Copy)]
pub struct Section {
    /// Clave Fluent del rótulo (`menu-section-*`), o `None` para una
    /// separación sin nombre: cuando el grupo se entiende solo, un rótulo es
    /// ruido.
    pub title: Option<&'static str>,
    /// Ids de comando, en el orden en que se pintan.
    pub items: &'static [&'static str],
}

/// Un menú: su título y sus secciones.
#[derive(Debug, Clone, Copy)]
pub struct Menu {
    /// Clave Fluent del título (`menu-*`).
    pub title: &'static str,
    /// Las secciones, de arriba abajo.
    pub sections: &'static [Section],
}

impl Menu {
    /// Cuántas órdenes tiene, todas las secciones juntas.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sections.iter().map(|s| s.items.len()).sum()
    }

    /// ¿No tiene ninguna orden?
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Las órdenes en el orden en que se pintan, sin secciones: es lo que
    /// recorre el cursor.
    pub fn items(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.sections.iter().flat_map(|s| s.items.iter().copied())
    }

    /// La orden `i` de la lista plana.
    #[must_use]
    pub fn item(&self, i: usize) -> Option<&'static str> {
        self.items().nth(i)
    }

    /// Si una sección EMPIEZA en la orden `i` —y no es la primera—, su
    /// rótulo: `Some(None)` es una separación sin nombre, `Some(Some(k))` una
    /// con rótulo. `None`: `i` sigue en la sección de la anterior.
    ///
    /// La primera sección no lleva separación: la raya de arriba del menú ya
    /// la hace. Un rótulo en la primera sí se pinta, y por eso se devuelve.
    #[must_use]
    pub fn section_at(&self, i: usize) -> Option<Option<&'static str>> {
        let mut inicio = 0;
        for (k, s) in self.sections.iter().enumerate() {
            if inicio == i && !s.items.is_empty() && (k > 0 || s.title.is_some()) {
                return Some(s.title);
            }
            inicio += s.items.len();
        }
        None
    }
}

/// Qué clase de orden es, para pintarla como lo que es.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemRole {
    /// Una orden cualquiera.
    Normal,
    /// Borra o no se puede deshacer: se pinta en el color de peligro, para
    /// que la mano que baja por el menú la vea ANTES de pulsarla.
    Destructive,
    /// La hace un modelo de IA: lleva la marca `✦`, porque lo que propone no
    /// lo ha decidido norte y conviene leerlo antes de aceptarlo.
    Ai,
}

impl ItemRole {
    /// El nombre estable que cruza el puente de la ventana.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Destructive => "destructive",
            Self::Ai => "ai",
        }
    }
}

/// El papel de una orden del menú, DERIVADO del efecto que el catálogo le
/// declara (ADR 0126, que reemplaza aquí la lista propia de ADR 0125): borrar
/// se pinta como peligro y mandar datos a un modelo lleva `✦`. Era el mismo
/// hecho que la ventana de solo lectura consultaba por su lado, en otra lista.
#[must_use]
pub fn role(id: &str) -> ItemRole {
    match crate::keymap::catalogue::effect(id) {
        Some(crate::keymap::Effect::Destroys) => ItemRole::Destructive,
        Some(crate::keymap::Effect::SendsOut) => ItemRole::Ai,
        _ => ItemRole::Normal,
    }
}

/// Atajo para declarar una sección.
const fn sec(title: Option<&'static str>, items: &'static [&'static str]) -> Section {
    Section { title, items }
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
        sections: &[
            sec(
                None,
                &["pane.view", "pane.edit", "pane.edit-new", "pane.open"],
            ),
            // #139: las propiedades son del FICHERO, así que van con lo que se
            // hace a un fichero, no con lo que se cambia de la pantalla.
            sec(
                None,
                &["pane.properties", "pane.dir-size", "pane.copy-path"],
            ),
            sec(None, &["app.quit"]),
        ],
    },
    // Lo que ESCRIBE: aparte de lo que solo lee, porque es lo que pasa por el
    // journal y lo que un lector quiere encontrar junto. Borrar va en su
    // propia sección: es lo único de aquí que no se deshace con un gesto.
    Menu {
        title: "menu-operate",
        sections: &[
            sec(
                None,
                &[
                    "pane.copy",
                    "pane.move",
                    "pane.rename",
                    "pane.rename-batch",
                    "pane.ai-rename",
                    "pane.organize",
                ],
            ),
            sec(None, &["pane.mkdir", "pane.chmod"]),
            sec(None, &["pane.delete", "pane.delete-permanent"]),
            sec(
                Some("menu-section-archives"),
                &["pane.pack", "pane.unpack", "pane.test-archive"],
            ),
            sec(
                Some("menu-section-pieces"),
                &["pane.split-file", "pane.combine-files"],
            ),
            sec(
                Some("menu-section-integrity"),
                &["pane.checksum", "pane.checksum-verify"],
            ),
        ],
    },
    Menu {
        title: "menu-mark",
        sections: &[
            sec(
                None,
                &[
                    "mark.toggle",
                    "mark.all",
                    "mark.invert",
                    "mark.clear",
                    "mark.restore",
                ],
            ),
            sec(
                Some("menu-section-by-pattern"),
                &[
                    "mark.pattern-add",
                    "mark.pattern-remove",
                    "mark.extension-add",
                    "mark.extension-remove",
                ],
            ),
            sec(Some("menu-section-by-kind"), &["mark.files", "mark.dirs"]),
        ],
    },
    // A DÓNDE mira un panel: subir, volver, favoritos, volúmenes, conectar.
    // #140 los ponía en Paneles por esa misma razón; con un menú propio de
    // navegación, es aquí donde se buscan.
    Menu {
        title: "menu-go",
        sections: &[
            // La primera del menú porque es la que sirve cuando no sabes
            // cuál de las otras quieres, y porque los cuatro presets
            // importados no la atan a ninguna tecla: aquí es donde la
            // encuentran.
            sec(None, &["app.goto"]),
            sec(
                None,
                &["nav.parent", "nav.back", "nav.forward", "pane.refresh"],
            ),
            sec(
                Some("menu-section-history"),
                &[
                    "nav.jump-back",
                    "nav.set-jump-point",
                    "pane.history",
                    "pane.history-left",
                    "pane.history-right",
                    "pane.popular",
                ],
            ),
            sec(
                Some("menu-section-places"),
                &[
                    "pane.hotlist",
                    "pane.select-drive",
                    "pane.connect",
                    "pane.disconnect",
                ],
            ),
            sec(
                Some("menu-section-shell"),
                &["pane.command-line", "app.terminal", "app.handoff"],
            ),
        ],
    },
    Menu {
        title: "menu-panels",
        sections: &[
            sec(
                None,
                &["pane.switch", "layout.focus-next", "layout.focus-prev"],
            ),
            sec(
                Some("menu-section-contents"),
                &[
                    "pane.mirror",
                    "pane.mirror-target",
                    "pane.pull",
                    "pane.swap",
                ],
            ),
            sec(
                Some("menu-section-split"),
                &[
                    "layout.split-h",
                    "layout.split-v",
                    "layout.close-slot",
                    "layout.grow",
                    "layout.shrink",
                    "layout.equalize",
                    "layout.flip",
                ],
            ),
            sec(None, &["layout.set-target", "app.toggle-panels"]),
        ],
    },
    Menu {
        title: "menu-tabs",
        sections: &[
            sec(None, &["pane.tab-new", "pane.tab-close"]),
            sec(None, &["pane.tab-next", "pane.tab-prev"]),
            sec(None, &["pane.tab-move-left", "pane.tab-move-right"]),
        ],
    },
    Menu {
        title: "menu-find",
        sections: &[
            sec(
                None,
                &["pane.quick-search", "pane.search", "pane.semantic-search"],
            ),
            sec(
                Some("menu-section-compare"),
                &["pane.compare-files", "pane.compare-dirs", "pane.sync-dirs"],
            ),
        ],
    },
    Menu {
        title: "menu-view",
        sections: &[
            sec(
                None,
                &[
                    "pane.toggle-hidden",
                    "pane.columns",
                    // #138: el orden es de la VISTA, y aquí es donde se
                    // cambia lo que la vista enseña.
                    "pane.sort-menu",
                    "pane.names-encoding",
                ],
            ),
            // Lo que se abre AL LADO del listado. #136: el árbol es otra
            // columna de navegación, como el sidebar. #323: el registro va
            // junto a procesos —los dos contestan «¿qué está haciendo
            // esto?»—, y el mapa de disco con ellos (fase 4). La línea de
            // tiempo (fase 7) no tiene atajo en ningún preset: aquí es su
            // única forma de teclado.
            sec(
                Some("menu-section-side-panels"),
                &[
                    "pane.tree",
                    "layout.places",
                    "layout.preview",
                    "layout.processes",
                    "layout.metadata",
                    "layout.log",
                    "layout.disk-map",
                    "layout.timeline",
                    // #362: el terminal empotrado. Va con los paneles del lado
                    // y no con `app.terminal` en el menú de órdenes, porque lo
                    // que abre es un PANEL: lo que se administra en este menú
                    // es qué se ve al lado del listado, y esto es una cosa más
                    // que se ve al lado.
                    "layout.terminal",
                ],
            ),
            sec(None, &["layout.pick", "app.theme"]),
        ],
    },
    // Lo que se administra: extensiones, agentes, ajustes, perfiles. La
    // paleta va aquí y no en Ayuda, porque desde ella se HACE.
    Menu {
        title: "menu-tools",
        sections: &[
            sec(None, &["app.extensions", "app.agents", "app.settings"]),
            sec(
                Some("menu-section-profiles"),
                &["profile.pick", "profile.save-as"],
            ),
            sec(None, &["app.palette"]),
        ],
    },
    Menu {
        title: "menu-help",
        sections: &[sec(None, &["app.help"])],
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
        MENUS.get(self.menu)?.item(self.item)
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
        let Some(n) = MENUS.get(self.menu).map(Menu::len) else {
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
        if MENUS.get(self.menu).is_some_and(|m| item < m.len()) {
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
    /// que el gate vive aquí. Los rótulos de sección, igual.
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
                for id in menu.items() {
                    let clave = format!("menu-item-{}", id.replace('.', "-"));
                    let etiqueta = norte_i18n::t(&clave);
                    assert!(
                        !etiqueta.is_empty() && etiqueta != clave,
                        "{lang:?}: {id} sale en el menú sin etiqueta ({clave})"
                    );
                }
                for clave in menu.sections.iter().filter_map(|s| s.title) {
                    let rotulo = norte_i18n::t(clave);
                    assert!(
                        !rotulo.is_empty() && rotulo != clave,
                        "{lang:?}: la sección {clave} no tiene rótulo"
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
            for id in m.items() {
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
            for id in m.items() {
                assert!(vistos.insert(id), "{id} aparece en dos menús");
            }
        }
    }

    /// Una sección vacía pintaría una raya sin nada debajo.
    #[test]
    fn ninguna_seccion_esta_vacia() {
        for m in MENUS {
            for s in m.sections {
                assert!(!s.items.is_empty(), "{}: sección vacía", m.title);
            }
        }
    }

    /// Las secciones se anuncian donde empiezan, la primera sin raya.
    #[test]
    fn section_at_marca_el_principio_de_cada_seccion() {
        let operar = MENUS
            .iter()
            .find(|m| m.title == "menu-operate")
            .expect("Operar");
        assert_eq!(operar.section_at(0), None, "la primera no lleva raya");
        assert_eq!(operar.section_at(1), None, "Mover sigue en la de Copiar");
        let borrar = operar
            .items()
            .position(|id| id == "pane.delete")
            .expect("Borrar");
        assert_eq!(operar.section_at(borrar), Some(None), "raya sin rótulo");
        let empaquetar = operar
            .items()
            .position(|id| id == "pane.pack")
            .expect("Empaquetar");
        assert_eq!(
            operar.section_at(empaquetar),
            Some(Some("menu-section-archives"))
        );
        assert_eq!(operar.item(borrar), Some("pane.delete"));
    }

    #[test]
    fn borrar_es_destructivo_y_la_ia_se_marca() {
        assert_eq!(role("pane.delete-permanent"), ItemRole::Destructive);
        assert_eq!(role("pane.ai-rename"), ItemRole::Ai);
        assert_eq!(role("pane.copy"), ItemRole::Normal);
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
        assert_eq!(s.item(), MENUS[MENUS.len() - 1].len() - 1);
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
