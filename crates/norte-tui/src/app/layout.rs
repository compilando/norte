//! La disposición vista desde `App`: cambiar el árbol entero, abrir y cerrar
//! los huecos laterales (places, preview, tree, procesos, metadatos),
//! redimensionar y mover el foco de hueco en hueco.

use super::{ALLOW_PROCESSES, App, KeyOwner, PlacesClick};
use crate::app::pane::Pane;
use norte_i18n::t;
use norte_proto::VPath;

impl App {
    /// Cambia la disposición entera, poniendo al día lo que depende de ella.
    ///
    /// Los huecos del árbol nuevo que no tengan listado se crean vacíos en el
    /// directorio del panel enfocado: un layout guardado nombra huecos, no
    /// dice qué había dentro, y arrancar con paneles muertos sería peor que
    /// arrancar con paneles repetidos.
    pub fn set_layout(&mut self, tree: norte_frontend::layout::Node) {
        let dir = self.panes[self.focus].dir().clone();
        for id in tree.slot_ids() {
            // Se siembra TODO kind con estado propio, no solo el listado: un
            // preset trae sidebar, visor, procesos y hoja de atributos, y un
            // hueco sin su estado se pinta vacío para siempre —el toggle que
            // lo habría creado no se va a pulsar, porque el panel ya está ahí.
            // Lo que ya existe se respeta: cambiar de layout no borra tu
            // navegación.
            match tree.kind_of(id).map(norte_frontend::layout::KindId::as_str) {
                Some("browser") if self.panes.browser(id).is_none() => {
                    self.panes
                        .insert_browser(id, Pane::new(dir.clone(), Vec::new()));
                }
                Some("places") if self.panes.places(id).is_none() => {
                    self.panes
                        .insert_places(id, norte_frontend::places::PlacesState::new());
                }
                Some("viewer") if self.panes.preview(id).is_none() => {
                    self.panes
                        .insert_preview(id, crate::preview::Preview::new());
                }
                Some(crate::processes::KIND) if self.panes.processes(id).is_none() => {
                    self.panes
                        .insert_processes(id, crate::processes::Processes::default());
                }
                Some(crate::metadata::KIND) if self.panes.metadata(id).is_none() => {
                    self.panes.insert_metadata(id, None);
                }
                // #136: anclado donde está el listado, igual que al abrirlo a
                // mano. Un layout guardado con el árbol dentro —una sesión de
                // ayer, un preset que lo traiga— llega por aquí, y sin este
                // brazo el hueco se pinta en blanco para siempre: el toggle
                // que habría creado su estado no se va a pulsar, porque el
                // panel ya está en pantalla.
                Some(crate::tree::KIND) if self.panes.tree(id).is_none() => {
                    let mut tree = crate::tree::Tree::default();
                    tree.anchor(dir.clone());
                    self.panes.insert_tree(id, tree);
                }
                _ => {}
            }
            // Los ids del layout no pueden chocar con los que se acuñen luego.
            self.next_slot = self.next_slot.max(id.0.saturating_add(1));
        }
        self.layout = tree;
        self.panes.refresh_visible(&self.layout);
        self.history.retain_tree(&self.layout);
        self.set_focus(0);
    }

    /// Parte el panel enfocado en dos, con el nuevo al lado.
    ///
    /// El panel nuevo hereda directorio y entradas del que se partió, igual
    /// que una pestaña nueva: es lo mismo que se está mirando, así que aparece
    /// lleno en vez de parpadear vacío mientras alguien relee lo mismo. Y se
    /// queda con el FOCO, que es lo que uno acaba de pedir.
    pub fn layout_split(&mut self, dir: norte_frontend::layout::Dir) {
        let focus = self.focused_slot();
        let (d, entradas) = {
            let p = &self.panes[self.focus];
            (p.dir().clone(), p.entries().to_vec())
        };
        let id = self.mint_slot();
        self.panes.insert_browser(id, Pane::new(d, entradas));
        self.layout = self.layout.split_slot(
            focus,
            dir,
            &norte_frontend::layout::Node::slot(id, norte_frontend::layout::KindId::browser()),
        );
        self.panes.refresh_visible(&self.layout);
        self.history.retain_tree(&self.layout);
        // El foco al recién nacido: partir es pedir sitio para trabajar en él.
        if let Some(i) = (0..self.panes.len()).find(|i| self.panes.slot_of(*i) == id) {
            self.set_focus(i);
        }
    }

    /// Quién tiene el teclado del cuerpo ahora mismo (L3).
    #[must_use]
    pub const fn key_owner(&self) -> KeyOwner {
        self.key_owner
    }

    /// Devuelve el teclado a los listados.
    ///
    /// Lo llaman `dialog.cancel` desde el sidebar y `viewer.close` desde el
    /// visor acoplado: los dos sueltan las teclas SIN cerrar el panel — cerrar
    /// algo que el lector solo quería dejar de manejar es la respuesta
    /// equivocada, y cerrarlo es lo que hace su propio comando de layout.
    pub const fn return_keys_to_panes(&mut self) {
        self.key_owner = KeyOwner::Panes;
    }

    /// El hueco del sidebar de sitios, si está en el árbol.
    #[must_use]
    pub fn places_slot(&self) -> Option<norte_frontend::layout::SlotId> {
        self.slot_of_kind("places")
    }

    /// El primer hueco del ÁRBOL con ese kind, visible o no.
    ///
    /// Del árbol y no del reparto: quien pregunta si el sidebar está abierto
    /// quiere saber si existe, y un hueco detrás de una pestaña sigue
    /// existiendo.
    fn slot_of_kind(&self, kind: &str) -> Option<norte_frontend::layout::SlotId> {
        self.layout
            .slot_ids()
            .into_iter()
            .find(|id| self.layout.kind_of(*id).is_some_and(|k| k.as_str() == kind))
    }

    /// Abre el sidebar de sitios, lo enfoca, o lo cierra.
    ///
    /// Las tres en una tecla, y en este orden: si no está, se acopla a la
    /// IZQUIERDA del reparto donde vive el listado enfocado y se queda el
    /// teclado; si está y el teclado lo tienen los listados, se lo lleva; y
    /// solo si ya lo tenía, se cierra. Una segunda pulsación no puede cerrar
    /// lo que el lector acaba de mirar de reojo.
    ///
    /// Abrirlo NO toca los listados: ni cuántos hay, ni cuál está enfocado, ni
    /// dónde está su cursor.
    pub fn toggle_places(&mut self) {
        use norte_frontend::layout::{Edge, KindId, Node, Size};
        match self.places_slot() {
            Some(id) if self.key_owner == KeyOwner::Places => {
                if let Some(nuevo) = self.layout.close_slot(id) {
                    self.layout = nuevo;
                    self.panes.refresh_visible(&self.layout);
                    self.history.retain_tree(&self.layout);
                }
                self.key_owner = KeyOwner::Panes;
            }
            Some(_) => self.key_owner = KeyOwner::Places,
            None => {
                let id = self.mint_slot();
                self.panes
                    .insert_places(id, norte_frontend::places::PlacesState::new());
                self.layout = self.layout.dock(
                    self.focused_slot(),
                    Edge::Left,
                    // 16 celdas: el mínimo del kind son 14 y un `Fixed` gana
                    // al mínimo, así que este número es el ancho de verdad.
                    Size::Fixed(16),
                    &Node::slot(id, KindId::new("places")),
                );
                self.panes.refresh_visible(&self.layout);
                self.key_owner = KeyOwner::Places;
            }
        }
    }

    /// Mueve el cursor del sidebar, si está abierto.
    pub fn places_up(&mut self) {
        if let Some(id) = self.places_slot()
            && let Some(s) = self.panes.places_mut(id)
        {
            s.up();
        }
    }

    /// Baja el cursor del sidebar.
    pub fn places_down(&mut self) {
        if let Some(id) = self.places_slot()
            && let Some(s) = self.panes.places_mut(id)
        {
            s.down();
        }
    }

    /// Un CLICK sobre la fila `index` del sidebar (#226).
    ///
    /// La decisión vive aquí y no en el módulo del ratón para que se pueda
    /// probar sin terminal, y porque es la misma que toma el teclado con otras
    /// teclas: el ratón no puede tener su propia idea de qué hace activar una
    /// fila. Tres desenlaces:
    ///
    /// - una CABECERA pliega o despliega su sección de una sola pulsación —
    ///   es lo que dice la flecha que ya pinta;
    /// - una fila que NO está seleccionada se selecciona, y el teclado se
    ///   viene al sidebar: el click dice «me interesa esto», no «vete ahí»;
    /// - la fila que YA estaba seleccionada se activa, que es lo mismo que
    ///   `Enter`. Sin ventana de tiempo: un doble click funciona por ser dos
    ///   clicks sobre la misma fila, y quien prefiera dos pulsaciones lentas
    ///   obtiene lo mismo.
    pub fn places_click(&mut self, index: usize) -> PlacesClick {
        use norte_frontend::places::PlaceRow;
        let Some(id) = self.places_slot() else {
            return PlacesClick::Focused;
        };
        let was_already = self.key_owner == KeyOwner::Places
            && self.panes.places(id).is_some_and(|s| s.cursor() == index);
        let Some(s) = self.panes.places_mut(id) else {
            return PlacesClick::Focused;
        };
        if index >= s.rows().len() {
            return PlacesClick::Focused;
        }
        s.set_cursor(index);
        let is_header = matches!(s.rows().get(index), Some(PlaceRow::Header { .. }));
        self.key_owner = KeyOwner::Places;
        if is_header {
            self.places_toggle_fold();
            return PlacesClick::Folded;
        }
        if was_already {
            PlacesClick::Activate
        } else {
            PlacesClick::Focused
        }
    }

    /// Pliega o despliega la sección donde está el cursor del sidebar.
    pub fn places_toggle_fold(&mut self) {
        if let Some(id) = self.places_slot()
            && let Some(s) = self.panes.places_mut(id)
        {
            s.toggle_fold();
        }
    }

    /// ¿Están DESPLEGADAS las unidades del sidebar?
    ///
    /// `false` también cuando no hay sidebar: quien pregunta es el run loop
    /// para decidir si vuelve a pedir `host.volumes`, y sin panel no hay a
    /// quién dárselos.
    #[must_use]
    pub fn places_drives_visible(&self) -> bool {
        self.places_slot()
            .and_then(|id| self.panes.places(id))
            .is_some_and(|s| !s.is_folded(norte_frontend::places::Section::Drives))
    }

    /// Confirma la fila del sidebar: a dónde hay que llevar el listado.
    ///
    /// Devuelve la ruta en vez de navegar porque un `cd` es I/O y esto es
    /// estado puro; quien tiene el `Backend` delante lo hace.
    ///
    /// Tres desenlaces y los tres importan:
    ///
    /// - una fila que lleva a un sitio: se devuelve la ruta y el teclado vuelve
    ///   a los listados, porque el sidebar es un MANDO y no un panel con
    ///   directorio propio;
    /// - una cabecera: aquí no pasa nada, y el teclado se queda donde está —
    ///   quien decide qué hace Enter ahí pregunta antes por
    ///   [`Self::places_cursor_on_header`] y pliega;
    /// - un favorito roto: la barra dice POR QUÉ. Es la otra mitad de pintarlo
    ///   marcado: en catorce celdas cabe el aviso, no la explicación.
    pub fn places_activate(&mut self) -> Option<VPath> {
        use norte_frontend::places::PlaceRow;
        let id = self.places_slot()?;
        let state = self.panes.places(id)?;
        if let Some(PlaceRow::Favorite {
            target: Err(clave), ..
        }) = state.rows().get(state.cursor())
        {
            let reason = t(clave);
            self.message = Some(reason);
            return None;
        }
        let dest = state.activate()?.clone();
        self.key_owner = KeyOwner::Panes;
        Some(dest)
    }

    /// Si el cursor del sidebar está sobre una CABECERA de sección.
    ///
    /// Lo pregunta quien decide qué hace Enter: sobre una cabecera pliega,
    /// sobre una unidad o un favorito navega. Sin esta pregunta, Enter sobre
    /// «Unidades» era inerte —[`Self::places_activate`] devuelve `None` ahí— y
    /// plegar era Espacio y solo Espacio.
    #[must_use]
    pub fn places_cursor_on_header(&self) -> bool {
        use norte_frontend::places::PlaceRow;
        self.places_slot()
            .and_then(|id| self.panes.places(id))
            .is_some_and(|s| matches!(s.rows().get(s.cursor()), Some(PlaceRow::Header { .. })))
    }

    /// El hueco del visor acoplado, si está en el árbol.
    #[must_use]
    pub fn preview_slot(&self) -> Option<norte_frontend::layout::SlotId> {
        self.slot_of_kind(crate::preview::KIND)
    }

    /// Abre el visor acoplado, lo enfoca, o lo cierra.
    ///
    /// Abre SIN llevarse el teclado, al revés que [`Self::toggle_places`], y
    /// la diferencia no es un capricho: el sidebar se abre para elegir algo en
    /// él, y el preview se abre para seguir mirando el listado. Con el teclado
    /// dentro, las flechas dejarían de mover el cursor —el mismo cursor al que
    /// el panel sigue—, o sea que abrirlo apagaría lo único que hace. Pilotar
    /// la TUI en tmux lo enseñó en la primera pulsación.
    ///
    /// La secuencia es abrir → enfocar (para `viewer.*`: hex, encoding,
    /// desplazar) → cerrar.
    ///
    /// Se acopla a la DERECHA, ponderado, y con `follows: Role(Active)`: no es
    /// un kind nuevo, es el `viewer` de siempre con un vínculo puesto. El kind
    /// dice qué hay dentro y el vínculo de quién es vista (ADR 0058), así que
    /// un visor fijado y uno que sigue al cursor son el MISMO renderer.
    pub fn toggle_preview(&mut self) {
        use norte_frontend::layout::{Bindings, Edge, Follow, KindId, Node, RoleId, Size};
        match self.preview_slot() {
            Some(id) if self.key_owner == KeyOwner::Preview => {
                if let Some(nuevo) = self.layout.close_slot(id) {
                    self.layout = nuevo;
                    self.panes.refresh_visible(&self.layout);
                    self.history.retain_tree(&self.layout);
                }
                self.key_owner = KeyOwner::Panes;
            }
            Some(_) => self.key_owner = KeyOwner::Preview,
            None => {
                let id = self.mint_slot();
                self.panes
                    .insert_preview(id, crate::preview::Preview::new());
                self.layout = self.layout.dock(
                    self.focused_slot(),
                    Edge::Right,
                    Size::Weight(1),
                    &Node::slot_bound(
                        id,
                        KindId::new(crate::preview::KIND),
                        Bindings {
                            follows: Some(Follow::Role(RoleId::Active)),
                        },
                    ),
                );
                self.panes.refresh_visible(&self.layout);
            }
        }
    }

    /// El hueco del árbol, si está abierto.
    #[must_use]
    pub fn tree_slot(&self) -> Option<norte_frontend::layout::SlotId> {
        self.slot_of_kind(crate::tree::KIND)
    }

    /// Abre el árbol de directorios, lo enfoca, o lo cierra (#136).
    ///
    /// Tres estados como el sidebar y el panel de procesos: un árbol se abre
    /// para MOVERSE por él, así que llevarse el teclado al abrir es lo que se
    /// espera.
    ///
    /// Se ancla en el directorio del listado con foco. Un árbol que colgara
    /// siempre de la raíz del sistema enseñaría diez mil ramas para llegar a
    /// donde ya estás.
    pub fn toggle_tree(&mut self) {
        use norte_frontend::layout::{Edge, KindId, Node, Size};
        match self.tree_slot() {
            Some(id) if self.key_owner == KeyOwner::Tree => {
                if let Some(nuevo) = self.layout.close_slot(id) {
                    self.layout = nuevo;
                    self.panes.refresh_visible(&self.layout);
                    self.history.retain_tree(&self.layout);
                }
                self.key_owner = KeyOwner::Panes;
            }
            Some(id) => {
                // Re-anclar al abrirlo de nuevo: el listado puede estar en otro
                // sitio desde la última vez.
                let dir = self.focused().dir().clone();
                if let Some(t) = self.panes.tree_mut(id) {
                    t.anchor(dir);
                }
                self.key_owner = KeyOwner::Tree;
            }
            None => {
                let id = self.mint_slot();
                let mut tree = crate::tree::Tree::default();
                tree.anchor(self.focused().dir().clone());
                self.panes.insert_tree(id, tree);
                self.layout = self.layout.dock(
                    self.focused_slot(),
                    Edge::Left,
                    // A la izquierda y con el ancho del sidebar: es el mismo
                    // gesto —una columna de navegación al lado del listado— y
                    // dos anchos distintos para lo mismo se notan.
                    Size::Fixed(24),
                    &Node::slot(id, KindId::new(crate::tree::KIND)),
                );
                self.panes.refresh_visible(&self.layout);
                self.key_owner = KeyOwner::Tree;
            }
        }
    }

    /// El árbol abierto, para mutarlo.
    pub fn tree_mut(&mut self) -> Option<&mut crate::tree::Tree> {
        let id = self.tree_slot()?;
        self.panes.tree_mut(id)
    }

    /// El árbol abierto.
    #[must_use]
    pub fn tree(&self) -> Option<&crate::tree::Tree> {
        let id = self.tree_slot()?;
        self.panes.tree(id)
    }

    /// El hueco del panel de procesos, si está abierto.
    #[must_use]
    pub fn processes_slot(&self) -> Option<norte_frontend::layout::SlotId> {
        self.slot_of_kind(crate::processes::KIND)
    }

    /// Abre el panel de procesos, lo enfoca, o lo cierra.
    ///
    /// Tres estados como el sidebar y NO como el visor acoplado: un panel de
    /// procesos se abre para mirar Y para cancelar algo concreto, así que
    /// llevarse el teclado al abrir es lo que se espera. (El preview hace lo
    /// contrario porque se abre para seguir navegando; L3 aprendió la
    /// distinción pilotando la TUI en tmux.)
    ///
    /// La franja `tasks` no se toca: sigue ahí, y sigue siendo lo que trae
    /// `orthodox`. Este panel es lo que se abre para ACTUAR sobre una tarea.
    pub fn toggle_processes(&mut self) {
        use norte_frontend::layout::{Edge, KindId, Node, Size};
        match self.processes_slot() {
            Some(id) if self.key_owner == KeyOwner::Processes => {
                if let Some(nuevo) = self.layout.close_slot(id) {
                    self.layout = nuevo;
                    self.panes.refresh_visible(&self.layout);
                    self.history.retain_tree(&self.layout);
                }
                self.key_owner = KeyOwner::Panes;
            }
            Some(_) => self.key_owner = KeyOwner::Processes,
            None => {
                let id = self.mint_slot();
                self.panes
                    .insert_processes(id, crate::processes::Processes::default());
                self.layout = self.layout.dock(
                    self.focused_slot(),
                    Edge::Bottom,
                    // Ocho filas: seis de tareas —el tope del `TaskBoard`— más
                    // el marco. `Auto` es de la franja, que vale cero en
                    // reposo; un panel que se abre a mano no desaparece.
                    Size::Fixed(8),
                    &Node::slot(id, KindId::new(crate::processes::KIND)),
                );
                self.panes.refresh_visible(&self.layout);
                self.key_owner = KeyOwner::Processes;
            }
        }
    }

    /// Sube el cursor del panel de procesos. No-op si no está abierto.
    pub fn processes_up(&mut self) {
        if let Some(id) = self.processes_slot()
            && let Some(p) = self.panes.processes_mut(id)
        {
            p.up();
        }
    }

    /// Baja el cursor del panel de procesos, sin pasarse de la última fila.
    pub fn processes_down(&mut self) {
        let rows = self.board.rows().len();
        if let Some(id) = self.processes_slot()
            && let Some(p) = self.panes.processes_mut(id)
        {
            p.down(rows);
        }
    }

    /// Cancela la tarea bajo el cursor del panel. `false` si no hay panel,
    /// ni filas, o si esa ya había terminado.
    ///
    /// Es lo que el CHANGELOG y los dos temas de ayuda llevaban prometiendo
    /// desde la fase A —«cancela la que está bajo el cursor»— sin que ninguna
    /// tecla llegara al panel: el `KeyOwner` se ponía y no lo leía nadie, así
    /// que las flechas movían el LISTADO de detrás y F8 abría el diálogo de
    /// borrar sobre su selección (#243).
    pub fn processes_cancel(&mut self) -> bool {
        let Some(id) = self.processes_slot() else {
            return false;
        };
        let rows = self.board.rows().len();
        let Some(cursor) = self.panes.processes(id).map(|p| p.cursor(rows)) else {
            return false;
        };
        self.board.cancel_at(cursor)
    }

    /// Despacha UN comando del keymap sobre el panel de procesos.
    ///
    /// Vive aquí y no en el binario para que un test pueda meter una tecla de
    /// verdad —preset → `Effective` → `Resolver` → comando— y ver qué hace el
    /// panel. Los tests que había afirmaban `key_owner()`, que es exactamente
    /// lo que dejó invisible que ninguna tecla llegara (#243).
    ///
    /// Devuelve el mensaje para la barra, si el comando deja uno.
    pub fn processes_command(&mut self, cmd: &str) -> Option<String> {
        if !ALLOW_PROCESSES.contains(&cmd) {
            return None; // fuera del allowlist de este panel: inerte
        }
        match cmd {
            "dialog.up" => self.processes_up(),
            "dialog.down" => self.processes_down(),
            // `Esc` suelta el teclado y NO cierra el panel: cerrarlo es
            // `layout.processes`. Y `Tab` hace lo mismo, por la misma razón
            // que el sidebar y el árbol: abrir un panel con teclado no puede
            // costarte la tecla con la que se cambia de panel toda la vida.
            "dialog.cancel" | "dialog.pane" | "pane.switch" => self.return_keys_to_panes(),
            "layout.grow" => self.layout_resize(1),
            "layout.shrink" => self.layout_resize(-1),
            "layout.processes" => self.toggle_processes(),
            "dialog.confirm" => {
                return Some(if self.processes_cancel() {
                    t("msg-cancelling")
                } else {
                    t("msg-no-tasks")
                });
            }
            _ => {}
        }
        None
    }

    /// El hueco de la hoja de atributos, si está abierta.
    #[must_use]
    pub fn metadata_slot(&self) -> Option<norte_frontend::layout::SlotId> {
        self.slot_of_kind(crate::metadata::KIND)
    }

    /// Abre la hoja de atributos, o la cierra.
    ///
    /// DOS estados y no tres, al contrario que el sidebar y el panel de
    /// procesos: la hoja sigue al cursor del listado, así que llevarse el
    /// teclado apagaría lo único que hace. Antes tenía un `KeyOwner` propio
    /// que se ponía en la segunda pulsación y no consumía nadie: la hoja
    /// cogía el borde de foco, las flechas seguían moviendo el listado de al
    /// lado, y la tercera pulsación era la única que cerraba (#243).
    /// Se acopla a la DERECHA con `follows: Role(Active)`.
    pub fn toggle_metadata(&mut self) {
        use norte_frontend::layout::{Bindings, Edge, Follow, KindId, Node, RoleId, Size};
        if let Some(id) = self.metadata_slot() {
            if let Some(nuevo) = self.layout.close_slot(id) {
                self.layout = nuevo;
                self.panes.refresh_visible(&self.layout);
                self.history.retain_tree(&self.layout);
            }
        } else {
            let id = self.mint_slot();
            self.panes.insert_metadata(id, None);
            self.layout = self.layout.dock(
                self.focused_slot(),
                Edge::Right,
                // Treinta celdas: la etiqueta más larga con su valor al lado.
                // Fijo y no ponderado porque una hoja de atributos no gana
                // nada con la mitad de la pantalla.
                Size::Fixed(30),
                &Node::slot_bound(
                    id,
                    KindId::new(crate::metadata::KIND),
                    Bindings {
                        follows: Some(Follow::Role(RoleId::Active)),
                    },
                ),
            );
            self.panes.refresh_visible(&self.layout);
        }
    }

    /// El preview no pudo leer: se pinta el motivo DENTRO del hueco.
    ///
    /// Y no se pregunta nada. El preview sigue al cursor, así que una
    /// denegación de policy no puede abrir un diálogo: bajar por un directorio
    /// sería una ráfaga de modales, y el lector no ha pedido abrir nada.
    pub fn preview_failed(&mut self, slot: norte_frontend::layout::SlotId, clave: &str) {
        let text = t(clave);
        if let Some(p) = self.panes.preview_mut(slot) {
            p.say(None, text);
        }
    }

    /// Cierra el panel enfocado.
    ///
    /// Se NIEGA a cerrar el último `browser`: una pantalla sin ningún listado
    /// no es un layout, es un cuelgue con bordes. Además es el invariante que
    /// mantiene distintos los dos lados — con un solo listado, «el otro pane»
    /// sería este mismo y una copia tendría por destino su propio origen.
    ///
    /// Devuelve `false` si no se pudo, para que el llamante avise.
    pub fn layout_close_slot(&mut self) -> bool {
        if self.browsers_in_tree() <= 2 {
            return false;
        }
        let focus = self.focused_slot();
        let Some(nuevo) = self.layout.close_slot(focus) else {
            return false;
        };
        self.layout = nuevo;
        self.panes.refresh_visible(&self.layout);
        self.history.retain_tree(&self.layout);
        true
    }

    /// El hueco al que apuntan `layout.grow`/`layout.shrink`: el que tiene el
    /// TECLADO, no el listado enfocado.
    ///
    /// `focused_slot()` es siempre un listado visible —`layout_focus` cicla
    /// sobre `panes.len()`—, así que con él la rama de `Size::Fixed` de
    /// `Node::resize` no la alcanzaba ningún camino de producción: la barra
    /// lateral se quedaba con el ancho con el que abría y el CHANGELOG
    /// anunciaba lo contrario (#244 M1). Los tests pasaban porque llamaban a
    /// `resize` con el id del sidebar a mano, un argumento que el llamante de
    /// verdad no sabía producir.
    #[must_use]
    fn resize_target(&self) -> norte_frontend::layout::SlotId {
        match self.key_owner {
            KeyOwner::Places => self.places_slot(),
            KeyOwner::Tree => self.tree_slot(),
            KeyOwner::Processes => self.processes_slot(),
            KeyOwner::Panes | KeyOwner::Preview => None,
        }
        .unwrap_or_else(|| self.focused_slot())
    }

    /// Agranda (`delta > 0`) o encoge el panel que tiene el teclado.
    pub fn layout_resize(&mut self, delta: i16) {
        let target = self.resize_target();
        self.layout = self.layout.resize(target, delta);
    }

    /// Devuelve a los hermanos del panel enfocado el mismo tamaño.
    pub fn layout_equalize(&mut self) {
        let focus = self.focused_slot();
        self.layout = self.layout.equalize(focus);
    }

    /// Designa el OTRO lado visible como destino de las operaciones.
    ///
    /// Con dos paneles el destino ya es el otro y esto no cambia nada; existe
    /// para el día en que haya más de dos y el motor deje de poder desempatar
    /// solo (ADR 0058 D7).
    pub fn layout_set_target(&mut self) {
        let n = self.panes.len();
        if n < 2 {
            return;
        }
        let current = self
            .roles
            .get(norte_frontend::layout::RoleId::Target)
            .and_then(|t| (0..n).find(|i| self.panes.slot_of(*i) == t))
            .unwrap_or(self.focus);
        // El siguiente que no sea el enfocado: designarse a uno mismo como
        // destino es pedirle a una copia que se copie encima.
        let mut i = (current + 1) % n;
        if i == self.focus {
            i = (i + 1) % n;
        }
        let slot = self.panes.slot_of(i);
        self.roles.set(norte_frontend::layout::RoleId::Target, slot);
    }

    /// La posición del panel DESTINO, si la hay.
    ///
    /// Con dos paneles es el otro y nadie tuvo que decirlo. Con tres o más
    /// hace falta haberlo designado: adivinar aquí es cómo una copia sale
    /// hacia un panel que el lector no tenía en la cabeza, que es pérdida de
    /// datos silenciosa (ADR 0058 D7).
    #[must_use]
    pub fn target_index(&self) -> Option<usize> {
        let n = self.panes.len();
        if let Some(t) = self.roles.get(norte_frontend::layout::RoleId::Target)
            && let Some(i) = (0..n).find(|i| self.panes.slot_of(*i) == t)
            && i != self.focus
        {
            return Some(i);
        }
        (n == 2).then_some(self.focus ^ 1)
    }

    /// How many times [`Self::swap_panes`] has run.
    ///
    /// Only useful as an equality check against a previously read value: any
    /// difference means the two sides changed places, so anything holding a
    /// pane INDEX from before now names the other side's content.
    #[must_use]
    pub const fn swap_seq(&self) -> u64 {
        self.swap_seq
    }

    /// Da el foco al pane `i`. Un índice fuera de `0|1` se IGNORA (el
    /// invariante de `focus` es de la propia `App`): el único emisor de
    /// índices que no son literales es el hit test del ratón, y ahí un
    /// índice imposible es un bug nuestro, no algo que deba dejar el foco
    /// apuntando a un pane que no existe.
    pub fn set_focus(&mut self, i: usize) {
        debug_assert!(i < self.panes.len(), "pane fuera de rango");
        if i < self.panes.len() {
            self.focus = i;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::*;

    /// #136: el árbol se abre anclado DONDE está el listado, no en la raíz del
    /// sistema: un árbol que colgara siempre de `/` enseñaría diez mil ramas
    /// para llegar a donde ya estás.
    #[test]
    fn el_arbol_se_ancla_donde_esta_el_listado() {
        let mut app = app_dos_panes();
        let dir = app.focused().dir().clone();
        app.toggle_tree();
        assert_eq!(app.tree().and_then(|t| t.root().cloned()), Some(dir));
        assert_eq!(app.key_owner(), KeyOwner::Tree, "se lleva el teclado");
    }

    /// **Un layout RESTAURADO con el árbol dentro trae su estado.**
    ///
    /// Es el fallo que encontró pilotar la TUI: la sesión de ayer guarda el
    /// árbol, al arrancar el hueco vuelve… y se pinta en blanco, porque el
    /// toggle que habría creado su estado no se va a pulsar — el panel ya está
    /// ahí. `set_layout` siembra el estado de CADA kind por esta razón, y el
    /// árbol tenía que entrar en esa lista.
    #[test]
    fn un_layout_con_arbol_trae_su_estado() {
        use norte_frontend::layout::{Dir, KindId, Node, Size, SlotId};

        let mut app = app_dos_panes();
        let tree = Node::Split {
            dir: Dir::Horizontal,
            sizes: vec![Size::Fixed(24), Size::Weight(1)],
            children: vec![
                Node::slot(SlotId(90), KindId::new(crate::tree::KIND)),
                Node::slot(SlotId(91), KindId::browser()),
            ],
        };
        app.set_layout(tree);
        assert!(
            app.panes.tree(SlotId(90)).is_some(),
            "el hueco del árbol llegó sin estado y se pintaría vacío"
        );
        assert!(
            app.panes.tree(SlotId(90)).and_then(|t| t.root()).is_some(),
            "y anclado en algún sitio, o no pide nada"
        );
    }

    /// Y el hueco del árbol SE COLOCA en el reparto: sin esto el layout le
    /// reserva sitio y nadie lo pinta, que es una columna en blanco.
    #[test]
    fn el_hueco_del_arbol_se_coloca() {
        use norte_frontend::layout::{KindRegistry, Rect, resolve};

        let mut app = app_dos_panes();
        app.toggle_tree();
        let id = app.tree_slot().expect("abierto");
        let res = resolve(
            Rect::new(0, 0, 110, 30),
            &app.layout,
            &KindRegistry::builtin(),
        );
        assert!(
            res.placements.iter().any(|(p, _)| *p == id),
            "el hueco del árbol no se colocó: {:?}",
            res.placements
        );
        assert!(app.panes.tree(id).is_some(), "y su panel está");
    }

    /// Tres pulsaciones, como el sidebar: abre y enfoca, vuelve a enfocar,
    /// cierra. La del medio es la que hace que soltar el teclado no cierre el
    /// panel.
    #[test]
    fn el_arbol_abre_enfoca_y_cierra() {
        let mut app = app_dos_panes();
        app.toggle_tree();
        assert!(app.tree_slot().is_some());
        app.return_keys_to_panes();
        app.toggle_tree();
        assert!(
            app.tree_slot().is_some(),
            "la segunda solo recupera el teclado"
        );
        assert_eq!(app.key_owner(), KeyOwner::Tree);
        app.toggle_tree();
        assert!(app.tree_slot().is_none(), "y la tercera cierra");
        assert_eq!(app.key_owner(), KeyOwner::Panes);
    }
}
