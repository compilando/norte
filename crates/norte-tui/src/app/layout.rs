//! La disposición vista desde `App`: cambiar el árbol entero, abrir y cerrar
//! los huecos laterales (places, preview, tree, procesos, metadatos),
//! redimensionar y mover el foco de hueco en hueco.

use super::{ALLOW_LOG, ALLOW_PROCESSES, App, KeyOwner, PlacesClick, TreeClick, TreeSpot};
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
                    let nuevo = self.nuevo_pane(dir.clone(), Vec::new());
                    self.panes.insert_browser(id, nuevo);
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
        self.settle_key_owner();
        // Un sidebar recién sembrado nace VACÍO, y quien lo llenaba era su
        // tecla. Una disposición que lo trae —`full`, `explorer`, la sesión de
        // ayer, un perfil— no la pulsa nunca, así que el panel se quedaba en
        // blanco para siempre: los favoritos van aquí mismo y las unidades se
        // piden por la bandera, porque son I/O.
        self.sync_places_favorites();
        self.places_wants_drives |= self.places_drives_visible();
        self.set_focus(0);
    }

    /// Devuelve el teclado a los listados si quien lo tenía ya no está en la
    /// disposición.
    ///
    /// Sin esto, cambiar de disposición con un panel lateral ENFOCADO —cambiar
    /// de perfil, aplicar un preset, restaurar una sesión— dejaba `key_owner`
    /// apuntando a un panel que ya no existe. Y entonces TODA tecla se enruta
    /// a su manejador, `<panel>_slot()` devuelve `None`, cada brazo es un
    /// no-op, y el gestor entero deja de responder sin nada en pantalla que
    /// explique por qué. Ni siquiera es reversible a ojo: la tecla del panel
    /// lo REABRE, así que parece que abrirlo «arregla» el teclado.
    ///
    /// Se comprueba por el KIND en el árbol y no por una bandera aparte: la
    /// pregunta es literalmente «¿sigue ahí?», y una bandera es un segundo
    /// sitio donde equivocarse.
    /// Desde #329 pregunta si el panel SE VE, no si existe. Un panel que se
    /// queda detrás de una pestaña —porque el lector cambió de pestaña, no
    /// porque cerrara nada— tiene el teclado igual de inútil que uno cerrado:
    /// las teclas van a algo que no está en pantalla. Y la barra lo pinta
    /// cerrado, así que el estado que el lector ve y el que manda dejaban de
    /// ser el mismo.
    pub(crate) fn settle_key_owner(&mut self) {
        let sigue = match self.key_owner {
            KeyOwner::Panes => true,
            KeyOwner::Places => self.slot_of_kind_visible("places").is_some(),
            KeyOwner::Preview => self.slot_of_kind_visible(crate::preview::KIND).is_some(),
            KeyOwner::Processes => self.slot_of_kind_visible(crate::processes::KIND).is_some(),
            KeyOwner::Tree => self.slot_of_kind_visible(crate::tree::KIND).is_some(),
            KeyOwner::Log => self.slot_of_kind_visible(crate::logview::KIND).is_some(),
        };
        if !sigue {
            self.key_owner = KeyOwner::Panes;
        }
    }

    /// Parte el panel enfocado en dos, con el nuevo al lado.
    ///
    /// El panel nuevo hereda directorio y entradas del que se partió, igual
    /// que una pestaña nueva: es lo mismo que se está mirando, así que aparece
    /// lleno en vez de parpadear vacío mientras alguien relee lo mismo. Y se
    /// queda con el FOCO, que es lo que uno acaba de pedir.
    ///
    /// Se NIEGA cuando el hueco enfocado ya no da para dos, y lo dice en la
    /// barra. Sin eso, la tecla creaba un panel que el reparto escondía en el
    /// mismo frame —el `Split` no cabe, se degrada a pestañas y la pantalla
    /// vuelve a enseñar uno, con el árbol guardando el nuevo igualmente—, así
    /// que desde fuera unas veces partía, otras no hacía nada y otras parecía
    /// deshacer lo anterior. La cuenta la hace el mismo sitio que decide el
    /// colapso ([`norte_frontend::layout::has_room_to_split`]) sobre el
    /// rectángulo del ÚLTIMO frame: el tamaño de un hueco no lo sabe el árbol,
    /// lo sabe la pantalla. Sin frame todavía no se niega nada — no saber no
    /// es lo mismo que saber que no.
    pub fn layout_split(&mut self, dir: norte_frontend::layout::Dir) {
        let focus = self.focused_slot();
        if let Some(rect) = self.mouse.slot_rect(focus)
            && !norte_frontend::layout::has_room_to_split(
                rect,
                dir,
                &norte_frontend::layout::KindId::browser(),
                &self.kinds,
            )
        {
            self.message = Some(t("msg-layout-split-no-room"));
            return;
        }
        let id = self.mint_slot();
        let nuevo = self.fork_pane(self.focus);
        self.panes.insert_browser(id, nuevo);
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

    /// Copia los favoritos vigentes al sidebar, si está en la disposición.
    ///
    /// De [`Self::hotlist`], que es la copia que mantienen el arranque y cada
    /// alta o baja: el sidebar no vuelve a leer la config ni se queda con una
    /// foto vieja de ella.
    ///
    /// El popup de `Ctrl+D` y este panel pintan EL MISMO dato, y durante un
    /// tiempo solo el popup se enteraba de los cambios: añadías un favorito,
    /// salía en el popup, y el panel de al lado seguía sin él. Por eso todo
    /// lo que toca la lista pasa por aquí.
    pub fn sync_places_favorites(&mut self) {
        let Some(id) = self.places_slot() else {
            return;
        };
        let items: Vec<(String, Result<VPath, String>)> = self
            .hotlist
            .iter()
            .map(|h| (h.name.clone(), h.target.clone()))
            .collect();
        if let Some(state) = self.panes.places_mut(id) {
            state.set_favorites(&items);
        }
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

    /// El primer hueco con ese kind que el lector VE ahora mismo (#329).
    ///
    /// La pareja de [`Self::slot_of_kind`], y las dos hacen falta porque hay
    /// dos preguntas: quien va a colocar un panel quiere saber si ya existe
    /// —duplicarlo sería lo malo—, y quien pinta un botón o cuenta una novedad
    /// quiere saber si el lector lo tiene delante. Preguntar la primera y
    /// actuar como si fuera la segunda es lo que hacía que un panel escondido
    /// en una pestaña se pintara abierto y se comiera su marca de aviso.
    pub(crate) fn slot_of_kind_visible(
        &self,
        kind: &str,
    ) -> Option<norte_frontend::layout::SlotId> {
        self.layout
            .visible_slot_ids()
            .into_iter()
            .find(|id| self.layout.kind_of(*id).is_some_and(|k| k.as_str() == kind))
    }

    /// ¿Tiene el lector este hueco delante?
    ///
    /// Responde por PESTAÑAS, no por sitio, y esa asimetría con la barra es
    /// deliberada (#331): la barra deriva de las colocaciones del reparto —sabe
    /// qué cabe—, y aquí no se puede, porque `App` no guarda el área pintada.
    /// Un toggle razona sobre el árbol, que es lo único que tiene.
    ///
    /// Consecuencia, escrita para que nadie la descubra de nuevo: un panel cuya
    /// pestaña está activa pero que el reparto descarta por falta de sitio se
    /// pinta cerrado y esta tecla lo cierra. Arreglarlo pediría meter el último
    /// área en el estado —un dato de presentación viviendo donde no vive—, que
    /// es una decisión aparte y probablemente peor que la asimetría.
    fn se_ve(&self, id: norte_frontend::layout::SlotId) -> bool {
        self.layout.visible_slot_ids().contains(&id)
    }

    /// Saca a la luz el hueco `id`: activa su pestaña en cada grupo del camino.
    ///
    /// No es un gesto propio y por eso no toca el teclado: lo llaman los
    /// toggles antes de enfocar, porque enfocar algo que no se ve es mandar las
    /// teclas a ninguna parte.
    fn revelar(&mut self, id: norte_frontend::layout::SlotId) {
        let nuevo = self.layout.reveal(id);
        if nuevo != self.layout {
            self.layout = nuevo;
            self.panes.refresh_visible(&self.layout);
        }
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
            // `se_ve` en la guarda desde #329: un panel escondido en una
            // pestaña no se cierra, se enseña. Cerrar lo que el lector no
            // tiene delante es la única de las tres acciones que no puede
            // deshacer mirando.
            Some(id) if self.key_owner == KeyOwner::Places && self.se_ve(id) => {
                if let Some(nuevo) = self.layout.close_slot(id) {
                    self.layout = nuevo;
                    self.panes.refresh_visible(&self.layout);
                    self.history.retain_tree(&self.layout);
                }
                self.key_owner = KeyOwner::Panes;
            }
            Some(id) => {
                self.revelar(id);
                self.key_owner = KeyOwner::Places;
            }
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
                // El sidebar nace vacío: los favoritos son suyos desde el
                // primer frame, no desde el primer refresco de fuera, y las
                // unidades se piden por la bandera que drena el bucle.
                self.sync_places_favorites();
                self.places_wants_drives = true;
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
        // Desplegar las unidades ES el momento de volver a pedirlas: un disco
        // montado o desmontado desde que se abrió el panel se ve aquí, y sin
        // un reloj de por medio.
        self.places_wants_drives |= self.places_drives_visible();
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
            Some(id) if self.key_owner == KeyOwner::Preview && self.se_ve(id) => {
                if let Some(nuevo) = self.layout.close_slot(id) {
                    self.layout = nuevo;
                    self.panes.refresh_visible(&self.layout);
                    self.history.retain_tree(&self.layout);
                }
                self.key_owner = KeyOwner::Panes;
            }
            // Sacarlo a la luz NO se lleva el teclado, y aquí está la
            // diferencia con los otros cinco (#329): para el lector, revelar
            // un visor escondido es ABRIRLO, y este toggle abre sin coger las
            // teclas por lo que dice el párrafo de arriba — con el teclado
            // dentro, las flechas dejan de mover el cursor al que el panel
            // sigue. Siguen siendo tres pulsaciones desde escondido: enseñar,
            // enfocar, cerrar; las mismas que desde cerrado.
            Some(id) if !self.se_ve(id) => self.revelar(id),
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
            Some(id) if self.key_owner == KeyOwner::Tree && self.se_ve(id) => {
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
                self.revelar(id);
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

    /// El árbol sigue al listado ENFOCADO: revela su directorio y conserva lo
    /// que estuviera abierto ([`norte_frontend::tree::Tree::follow`]).
    ///
    /// Se llama desde los dos sitios en los que «dónde mira el panel» cambia
    /// —[`crate::navigate::settle_cd`], el embudo de todo `cd`, y el aterrizaje
    /// del foco— y no desde cada gesto que provoca uno: la lista de gestos que
    /// navegan ya se quedó corta una vez, y de ahí salió el propio `settle_cd`.
    ///
    /// Las ramas que hagan falta las pide el bucle solo
    /// ([`norte_frontend::tree::Tree::wants`], una por vuelta), así que aquí no
    /// hay I/O.
    pub fn follow_tree(&mut self) {
        let dir = self.focused().dir().clone();
        if let Some(t) = self.tree_mut() {
            t.follow(&dir);
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

    /// Un CLICK sobre la fila `index` del árbol (#136).
    ///
    /// La decisión vive aquí y no en el módulo del ratón, por lo mismo que la
    /// del sidebar: se prueba sin terminal, y es la misma que toma el teclado
    /// con otras teclas — el ratón no puede tener su propia idea de qué hace
    /// activar una fila. Tres desenlaces:
    ///
    /// - sobre la MARCA, la rama se pliega o se despliega de una sola
    ///   pulsación: es lo que dice la flecha que ya se pinta, y es lo único
    ///   que el ratón no podría hacer de otra forma —`Enter` despliega y
    ///   navega, nunca pliega;
    /// - una fila que NO está seleccionada se selecciona, y el teclado se
    ///   viene al árbol: el click dice «me interesa esto», no «vete ahí»;
    /// - la fila que YA estaba seleccionada se activa, que es lo mismo que
    ///   `Enter`. Sin ventana de tiempo, igual que el sidebar.
    pub fn tree_click(&mut self, index: usize, spot: TreeSpot) -> TreeClick {
        let Some(id) = self.tree_slot() else {
            return TreeClick::Focused;
        };
        let ya_estaba = self.key_owner == KeyOwner::Tree
            && self.panes.tree(id).is_some_and(|t| t.cursor() == index);
        let Some(t) = self.panes.tree_mut(id) else {
            return TreeClick::Focused;
        };
        if index >= t.rows().len() {
            return TreeClick::Focused;
        }
        t.set_cursor(index);
        self.key_owner = KeyOwner::Tree;
        if spot == TreeSpot::Mark {
            if let Some(t) = self.panes.tree_mut(id) {
                t.toggle();
            }
            return TreeClick::Focused;
        }
        if ya_estaba {
            TreeClick::Activate
        } else {
            TreeClick::Focused
        }
    }

    /// El directorio al que activar una fila del árbol lleva el listado.
    ///
    /// Despliega Y devuelve el destino, que es lo que hace `Enter` dentro del
    /// árbol: quien lo activa quiere ver qué hay dentro, y verlo en el listado
    /// es la respuesta completa.
    pub fn tree_activate(&mut self) -> Option<VPath> {
        let t = self.tree_mut()?;
        t.expand();
        t.selected()
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
            Some(id) if self.key_owner == KeyOwner::Processes && self.se_ve(id) => {
                if let Some(nuevo) = self.layout.close_slot(id) {
                    self.layout = nuevo;
                    self.panes.refresh_visible(&self.layout);
                    self.history.retain_tree(&self.layout);
                }
                self.key_owner = KeyOwner::Panes;
            }
            Some(id) => {
                self.revelar(id);
                self.key_owner = KeyOwner::Processes;
            }
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

    /// El hueco del panel de registro, si está abierto.
    #[must_use]
    pub fn log_slot(&self) -> Option<norte_frontend::layout::SlotId> {
        self.slot_of_kind(crate::logview::KIND)
    }

    /// El hueco del panel de registro **si de verdad está en pantalla**.
    ///
    /// Distinto de [`Self::log_slot`], que dice si EXISTE: un hueco detrás de
    /// una pestaña que no es la activa sigue existiendo y no se ve. La
    /// diferencia importa donde algo CUESTA — el sondeo del registro del daemon
    /// (#328) son dos RPC por segundo, y pagarlas por un panel que nadie tiene
    /// delante, durante toda la sesión, es gastar red por nada.
    ///
    /// Nació aquí con #328 y ahora delega en `slot_of_kind_visible` —sin
    /// enlace: es `pub(crate)` y esto es público, y rustdoc deniega el enlace
    /// de lo público a lo privado—: #329 encontró la misma pregunta en la barra
    /// de paneles y en los cinco toggles, así que la respuesta dejó de ser cosa
    /// del registro.
    #[must_use]
    pub fn log_slot_visible(&self) -> Option<norte_frontend::layout::SlotId> {
        self.slot_of_kind_visible(crate::logview::KIND)
    }

    /// Abre el panel de registro, lo enfoca, o lo cierra (#323).
    ///
    /// Tres estados y con el teclado al abrir, igual que el de procesos: se
    /// abre para LEER algo concreto —filtrando por nivel o por texto—, no de
    /// paso mientras navegas.
    ///
    /// No hay estado por hueco que insertar: hay un panel de registro y su
    /// nivel y su filtro son de la sesión, no del sitio donde lo pongas.
    pub fn toggle_log(&mut self) {
        use norte_frontend::layout::{Edge, KindId, Node, Size};
        match self.log_slot() {
            Some(id) if self.key_owner == KeyOwner::Log && self.se_ve(id) => {
                if let Some(nuevo) = self.layout.close_slot(id) {
                    self.layout = nuevo;
                    self.panes.refresh_visible(&self.layout);
                    self.history.retain_tree(&self.layout);
                }
                self.key_owner = KeyOwner::Panes;
                // Cerrar BAJA el nivel del anillo al que se estaba enseñando.
                // Es la única forma de volver atrás: subirlo nunca baja
                // —para que ir a DEBUG y volver no borre lo de en medio—, y sin
                // esto una sola pulsación de `t` dejaba el proceso capturando
                // TRACE el resto de la sesión, con su coste, mucho después de
                // que nadie mirara. Cerrar el panel es decir «ya está».
                if let Some(ring) = self.log_ring.as_ref() {
                    ring.set_level(self.log_panel.level());
                }
                self.log_filter_input = None;
                // Y lo del daemon se suelta (#328): sus líneas y su cursor son
                // de ESTA apertura, y una respuesta que llegue tarde no puede
                // aterrizar en la siguiente. Lo que NO se olvida es si sirve su
                // registro — es un hecho sobre el daemon, no sobre el panel—,
                // y su nivel tampoco: no lo baja nadie.
                //
                // El anillo del daemon no se baja al cerrar, al revés que el
                // local: es global a todos sus clientes, y bajárselo desde aquí
                // apagaría la captura de otro frontend que esté mirando.
                self.log_remote.reiniciar();
            }
            Some(id) => {
                self.revelar(id);
                self.key_owner = KeyOwner::Log;
            }
            None => {
                let id = self.mint_slot();
                self.layout = self.layout.dock(
                    self.focused_slot(),
                    Edge::Bottom,
                    // Diez filas: ocho de mensajes más el marco. Un log de
                    // cuatro líneas obliga a desplazarse para leer una frase
                    // que ocupa dos, y entonces no se usa.
                    Size::Fixed(10),
                    &Node::slot(id, KindId::new(crate::logview::KIND)),
                );
                self.panes.refresh_visible(&self.layout);
                self.key_owner = KeyOwner::Log;
            }
        }
    }

    /// Sube el cursor del panel de procesos. No-op si no está abierto.
    pub fn processes_up(&mut self) {
        let ids = self.board.task_ids();
        if let Some(id) = self.processes_slot()
            && let Some(p) = self.panes.processes_mut(id)
        {
            p.up(&ids);
        }
    }

    /// Baja el cursor del panel de procesos, sin pasarse de la última fila.
    pub fn processes_down(&mut self) {
        let ids = self.board.task_ids();
        if let Some(id) = self.processes_slot()
            && let Some(p) = self.panes.processes_mut(id)
        {
            p.down(&ids);
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
        let ids = self.board.task_ids();
        let Some(cursor) = self.panes.processes(id).and_then(|p| p.fila(&ids)) else {
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
        // El cromo de la aplicación, antes que lo de este panel: mismo embudo
        // que el sidebar y el árbol.
        if self.panel_chrome_command(cmd) {
            return None;
        }
        match cmd {
            "dialog.up" => self.processes_up(),
            "dialog.down" => self.processes_down(),
            // `Esc` suelta el teclado y NO cierra el panel: cerrarlo es
            // `layout.processes`. Y `Tab` hace lo mismo, por la misma razón
            // que el sidebar y el árbol: abrir un panel con teclado no puede
            // costarte la tecla con la que se cambia de panel toda la vida.
            "dialog.cancel" | "dialog.pane" | "pane.switch" => self.return_keys_to_panes(),
            // El anillo pasa al panel de AL LADO, y por eso no es lo mismo que
            // `Tab`: hay que poder recorrer la pantalla desde dentro de
            // cualquier panel, no solo volviendo antes a los listados.
            "layout.focus-next" => self.layout_focus(1),
            "layout.focus-prev" => self.layout_focus(-1),
            "layout.grow" => self.layout_resize(1),
            "layout.shrink" => self.layout_resize(-1),
            "layout.processes" => self.toggle_processes(),
            // Y las de los otros paneles, igual que en el sidebar: el sidebar
            // deja las unidades pedidas y las sirve el bucle, así que abrirlo
            // desde aquí no necesita backend.
            "layout.places" => self.toggle_places(),
            "layout.preview" => self.toggle_preview(),
            "layout.metadata" => self.toggle_metadata(),
            "pane.tree" => self.toggle_tree(),
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

    /// Despacha un comando del keymap con el teclado en el panel de registro
    /// (#323), filtrado por [`ALLOW_LOG`].
    ///
    /// Su propio embudo y no el de procesos: aquel deja pasar `dialog.confirm`,
    /// que allí CANCELA la tarea bajo el cursor. Un `Enter` en un visor de log
    /// que cancela una copia es justo lo que una allowlist existe para impedir.
    pub fn log_command(&mut self, cmd: &str) {
        if !ALLOW_LOG.contains(&cmd) {
            return; // fuera del allowlist de este panel: inerte
        }
        if self.panel_chrome_command(cmd) {
            return;
        }
        match cmd {
            "dialog.pane" | "pane.switch" => self.return_keys_to_panes(),
            "layout.focus-next" => self.layout_focus(1),
            "layout.focus-prev" => self.layout_focus(-1),
            "layout.grow" => self.layout_resize(1),
            "layout.shrink" => self.layout_resize(-1),
            "layout.log" => self.toggle_log(),
            "layout.processes" => self.toggle_processes(),
            "layout.places" => self.toggle_places(),
            "layout.preview" => self.toggle_preview(),
            "layout.metadata" => self.toggle_metadata(),
            "pane.tree" => self.toggle_tree(),
            _ => {}
        }
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
            // #329: si está escondido detrás de una pestaña, esta tecla lo
            // ENSEÑA. La hoja de atributos no toma el teclado —se mira, no se
            // recorre—, así que sus estados son dos, y el que falta cuando no
            // se ve no es «cerrar» sino «tráelo».
            if !self.se_ve(id) {
                self.revelar(id);
            } else if let Some(nuevo) = self.layout.close_slot(id) {
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
            KeyOwner::Log => self.log_slot(),
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

    /// Una pantalla con sitio de sobra, para cuando lo que se prueba no es la
    /// falta de sitio.
    const PANTALLA: ratatui::layout::Rect = ratatui::layout::Rect {
        x: 0,
        y: 0,
        width: 110,
        height: 30,
    };

    /// Un árbol con el registro escondido en la pestaña que no está activa.
    fn app_con_registro_escondido() -> App {
        use norte_frontend::layout::{Dir, KindId, Node, Size, SlotId};

        let mut app = app_dos_panes();
        app.set_layout(Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Fixed(10)],
            children: vec![
                Node::slot(SlotId(80), KindId::browser()),
                Node::Tabs {
                    children: vec![
                        Node::slot(SlotId(81), KindId::browser()),
                        Node::slot(SlotId(82), KindId::new(crate::logview::KIND)),
                    ],
                    active: 0,
                },
            ],
        });
        app
    }

    /// El botón de un panel escondido en una pestaña dice CERRADO (#329).
    ///
    /// Decía abierto, porque la barra preguntaba si el hueco EXISTE. Para el
    /// lector no existe: no lo ve, y lo que el botón le promete es enseñárselo.
    #[test]
    fn un_panel_escondido_en_una_pestana_se_pinta_cerrado() {
        use norte_frontend::panelbar::PanelState;

        let app = app_con_registro_escondido();
        let boton = crate::ui::panel_buttons(&app, PANTALLA)
            .into_iter()
            .find(|b| b.kind == crate::logview::KIND)
            .expect("el registro tiene botón");
        assert_eq!(boton.state, PanelState::Closed);
    }

    /// Un panel que el reparto descarta por falta de sitio tampoco está
    /// abierto (#331).
    ///
    /// `visible_slot_ids` contesta qué pestaña está activa, no qué CABE. El
    /// MISMO árbol, con el panel en la pestaña activa, se lee distinto en dos
    /// pantallas — y eso es exactamente lo que hay que ver, porque prueba que
    /// el botón mira el reparto y no el árbol.
    #[test]
    fn un_panel_que_no_cabe_se_pinta_cerrado() {
        use norte_frontend::layout::{Dir, KindId, Node, Size, SlotId};
        use norte_frontend::panelbar::PanelState;

        let mut app = app_dos_panes();
        // En horizontal, porque el caso que DESCARTA un hueco es el colapso:
        // dos hermanos que se disputan el mismo eje y cuyos mínimos no caben.
        // Un `Fixed` no vale para probar esto — se recorta, no se cae.
        app.set_layout(Node::Split {
            dir: Dir::Horizontal,
            sizes: vec![Size::Weight(1), Size::Weight(1)],
            children: vec![
                Node::slot(SlotId(40), KindId::browser()),
                Node::slot(SlotId(41), KindId::new(crate::logview::KIND)),
            ],
        });

        let estado = |app: &App, ancho: u16| {
            crate::ui::panel_buttons(
                app,
                ratatui::layout::Rect {
                    x: 0,
                    y: 0,
                    width: ancho,
                    height: 30,
                },
            )
            .into_iter()
            .find(|b| b.kind == crate::logview::KIND)
            .expect("el registro tiene botón")
            .state
        };

        assert_eq!(estado(&app, 110), PanelState::Open, "con sitio, abierto");
        assert_eq!(
            estado(&app, 24),
            PanelState::Closed,
            "el reparto lo descartó, así que el botón no puede decir que sí"
        );
    }

    /// Y pulsarlo lo SACA A LA LUZ y se lleva el teclado, en vez de mandar el
    /// teclado a algo invisible (#329).
    ///
    /// Antes caía en la rama «ya está abierto, enfócalo»: las teclas dejaban de
    /// llegar a lo que sí se veía, la barra decía «enfocado», y la siguiente
    /// pulsación cerraba un panel que nadie había visto nunca.
    #[test]
    fn pulsar_un_panel_escondido_lo_ensena() {
        use norte_frontend::layout::SlotId;

        let mut app = app_con_registro_escondido();
        app.toggle_log();
        assert!(
            app.layout.visible_slot_ids().contains(&SlotId(82)),
            "sigue detrás de la otra pestaña"
        );
        assert_eq!(app.key_owner(), KeyOwner::Log);
        assert!(app.log_slot().is_some(), "y desde luego no lo ha cerrado");
    }

    /// Un panel que se ESCONDE pierde el teclado, igual que uno que se cierra
    /// (#329).
    ///
    /// `settle_key_owner` preguntaba si el panel existe. Cambiar de pestaña no
    /// cierra nada, así que el registro se quedaba con las teclas detrás de
    /// otra pestaña: se pulsaba y no pasaba nada visible, mientras la barra ya
    /// lo pintaba cerrado — el estado que el lector ve y el que manda dejaban
    /// de ser el mismo. `tab_cycle` y `tab_goto` lo llaman por eso.
    #[test]
    fn un_panel_que_se_esconde_suelta_el_teclado() {
        use norte_frontend::layout::SlotId;

        let mut app = app_con_registro_escondido();
        app.toggle_log();
        assert_eq!(app.key_owner(), KeyOwner::Log, "visible y con las teclas");

        // Lo que hace cualquier camino que cambia de pestaña.
        app.layout = app.layout.set_active_for(SlotId(82), 0);
        assert!(!app.layout.visible_slot_ids().contains(&SlotId(82)));

        app.settle_key_owner();
        assert_eq!(
            app.key_owner(),
            KeyOwner::Panes,
            "se escondió y se quedó con las teclas"
        );
    }

    /// El visor escondido se ENSEÑA sin llevarse el teclado, al revés que los
    /// otros cinco.
    ///
    /// No es una excepción caprichosa: este toggle abre sin coger las teclas
    /// porque el visor sigue al cursor del listado, y para el lector revelar
    /// uno escondido ES abrirlo. Con el teclado dentro, las flechas dejarían de
    /// mover el cursor al que el panel sigue — o sea que enseñarlo apagaría lo
    /// único que hace.
    #[test]
    fn revelar_el_visor_no_se_lleva_el_teclado() {
        use norte_frontend::layout::{Dir, KindId, Node, Size, SlotId};

        let mut app = app_dos_panes();
        app.set_layout(Node::Split {
            dir: Dir::Horizontal,
            sizes: vec![Size::Weight(1), Size::Fixed(30)],
            children: vec![
                Node::slot(SlotId(70), KindId::browser()),
                Node::Tabs {
                    children: vec![
                        Node::slot(SlotId(71), KindId::browser()),
                        Node::slot(SlotId(72), KindId::new(crate::preview::KIND)),
                    ],
                    active: 0,
                },
            ],
        });
        app.toggle_preview();
        assert!(app.layout.visible_slot_ids().contains(&SlotId(72)));
        assert_eq!(
            app.key_owner(),
            KeyOwner::Panes,
            "enseñarlo no puede apagar las flechas del listado"
        );
        app.toggle_preview();
        assert_eq!(app.key_owner(), KeyOwner::Preview, "y la segunda lo enfoca");
    }

    /// La hoja de atributos es de DOS estados, así que lo que le falta cuando
    /// está escondida no es «cerrar» sino «tráela».
    #[test]
    fn la_hoja_de_atributos_escondida_se_ensena_en_vez_de_cerrarse() {
        use norte_frontend::layout::{Dir, KindId, Node, Size, SlotId};

        let mut app = app_dos_panes();
        app.set_layout(Node::Split {
            dir: Dir::Horizontal,
            sizes: vec![Size::Weight(1), Size::Fixed(30)],
            children: vec![
                Node::slot(SlotId(60), KindId::browser()),
                Node::Tabs {
                    children: vec![
                        Node::slot(SlotId(61), KindId::browser()),
                        Node::slot(SlotId(62), KindId::new(crate::metadata::KIND)),
                    ],
                    active: 0,
                },
            ],
        });
        app.toggle_metadata();
        assert!(
            app.layout.visible_slot_ids().contains(&SlotId(62)),
            "la cerró sin que el lector la hubiera visto"
        );
        app.toggle_metadata();
        assert!(app.metadata_slot().is_none(), "y la segunda sí cierra");
    }

    /// Un panel de procesos escondido tampoco se come su marca de novedad.
    #[test]
    fn procesos_escondido_conserva_su_marca_de_novedad() {
        use norte_frontend::layout::{Dir, KindId, Node, Size, SlotId};

        let mut app = app_dos_panes();
        app.set_layout(Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Fixed(8)],
            children: vec![
                Node::slot(SlotId(50), KindId::browser()),
                Node::Tabs {
                    children: vec![
                        Node::slot(SlotId(51), KindId::browser()),
                        Node::slot(SlotId(52), KindId::new(crate::processes::KIND)),
                    ],
                    active: 0,
                },
            ],
        });
        let boton = crate::ui::panel_buttons(&app, PANTALLA)
            .into_iter()
            .find(|b| b.kind == crate::processes::KIND)
            .expect("procesos tiene botón");
        assert_eq!(
            boton.attention,
            !app.board.rows().is_empty(),
            "la marca depende de si hay tareas, no de que el hueco exista"
        );
    }

    /// La segunda pulsación ya sí cierra: enseñar y enfocar es UN paso, no dos.
    ///
    /// Si sacarlo a la luz costara una pulsación y enfocarlo otra, el panel que
    /// el lector acaba de pedir se quedaría sin teclado, y la promesa de los
    /// tres estados —abre y coge el teclado, coge el teclado, cierra— tendría
    /// cuatro.
    #[test]
    fn la_segunda_pulsacion_cierra_lo_que_la_primera_ensena() {
        let mut app = app_con_registro_escondido();
        app.toggle_log();
        app.toggle_log();
        assert!(app.log_slot().is_none());
        assert_eq!(app.key_owner(), KeyOwner::Panes);
    }

    /// Un panel ESCONDIDO no se come la marca de novedad (#329).
    ///
    /// La marca se calla cuando el panel está abierto porque entonces ya lo
    /// estás viendo. Escondido no lo estás viendo, así que callarla apagaba el
    /// aviso justo en el caso en que hace falta.
    #[test]
    fn un_registro_escondido_conserva_su_marca_de_novedad() {
        use norte_config::logring::LogRing;
        use tracing_subscriber::layer::SubscriberExt as _;

        let mut app = app_con_registro_escondido();
        let anillo = LogRing::new(16);
        let s = tracing_subscriber::registry().with(norte_config::logring::ring_layer(&anillo));
        tracing::subscriber::with_default(s, || {
            tracing::warn!(target: "norte_core::prueba", "algo se degradó");
        });
        app.log_ring = Some(anillo);

        let boton = crate::ui::panel_buttons(&app, PANTALLA)
            .into_iter()
            .find(|b| b.kind == crate::logview::KIND)
            .expect("el registro tiene botón");
        assert!(
            boton.attention,
            "hay un aviso y el lector no lo tiene delante"
        );
    }

    /// El caso corriente no cambia: un panel acoplado, visible, se enfoca y se
    /// cierra como siempre.
    #[test]
    fn un_panel_a_la_vista_sigue_haciendo_los_tres_pasos() {
        let mut app = app_dos_panes();
        app.toggle_log();
        assert_eq!(app.key_owner(), KeyOwner::Log, "abre y coge el teclado");
        app.key_owner = KeyOwner::Panes;
        app.toggle_log();
        assert_eq!(app.key_owner(), KeyOwner::Log, "se lo lleva de vuelta");
        app.toggle_log();
        assert!(app.log_slot().is_none(), "y solo entonces cierra");
    }

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

    /// Y una vez abierto SIGUE al listado: navegar dentro de su raíz mueve el
    /// cursor a esa rama y conserva lo que hubiera abierto. Anclado y quieto,
    /// el panel decía dónde estabas cuando lo abriste y nada más.
    #[test]
    fn el_arbol_sigue_al_listado_que_navega() {
        let mut app = app_en("mem:///r", "mem:///otro");
        app.toggle_tree();
        let t = app.tree_mut().expect("árbol");
        t.insert_children(vp("mem:///r"), vec![vp("mem:///r/a"), vp("mem:///r/b")]);
        // `b` abierta a mano: es lo que un re-anclado habría cerrado.
        t.set_cursor(2);
        t.expand();
        t.insert_children(vp("mem:///r/b"), vec![vp("mem:///r/b/x")]);
        t.insert_children(vp("mem:///r/a"), vec![vp("mem:///r/a/y")]);

        *app.focused_mut() = crate::app::pane::Pane::new(vp("mem:///r/a/y"), Vec::new());
        app.follow_tree();

        let t = app.tree().expect("árbol");
        assert_eq!(t.selected(), Some(vp("mem:///r/a/y")));
        assert_eq!(t.rows().len(), 5, "`b` sigue desplegada");
    }

    /// Cambiar de panel también cambia a dónde mira el árbol: los dos lados
    /// están en sitios distintos, y un árbol que se quedara en el del panel
    /// anterior describiría el que ya no tiene el foco.
    ///
    /// Y la raíz sube al ancestro COMÚN de los dos, sin tirar nada: `Tab` es la
    /// tecla más usada del programa, y re-anclar en el destino cerraba el árbol
    /// entero en cada pulsación. Aquí los dos lados solo comparten la raíz del
    /// provider, así que ahí acaba subiendo; con dos directorios hermanos —el
    /// caso normal— sube un nivel y se queda.
    #[test]
    fn el_arbol_sigue_al_cambio_de_panel() {
        let mut app = app_en("mem:///r", "mem:///otro");
        app.toggle_tree();
        app.return_keys_to_panes();
        assert_eq!(
            app.tree().and_then(|t| t.root().cloned()),
            Some(vp("mem:///r"))
        );

        app.switch_focus();

        assert_eq!(
            app.tree().and_then(|t| t.root().cloned()),
            Some(vp("mem:///")),
            "sube al ancestro común de los dos lados"
        );
        assert_eq!(
            app.tree().and_then(norte_frontend::tree::Tree::revealing),
            Some(&vp("mem:///otro")),
            "y el cursor irá a donde está el panel que ahora tiene el foco"
        );

        // Y la vuelta ya no mueve la raíz: los dos cuelgan de ella.
        app.switch_focus();
        assert_eq!(
            app.tree().and_then(|t| t.root().cloned()),
            Some(vp("mem:///"))
        );
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
