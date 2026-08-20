//! La sesión de UI vista desde `App` (L2, ADR 0059): componer el cuerpo de
//! AHORA, aplicar el que se leyó del disco, recolocar el cursor, adoptar los
//! huecos huérfanos y sellar su edad.

use super::App;
use super::pane::Pane;
use norte_i18n::{t, ta};

impl App {
    /// La pantalla de AHORA como cuerpo de sesión (L2).
    ///
    /// Lleva la disposición y, por hueco de listado, dónde está, cómo mira y
    /// por dónde ha pasado. NO lleva las marcas: son el estado de una
    /// operación a medias, no de una sesión, y devolverlas al arrancar sería
    /// devolver un `F8` apuntando a lo que uno marcó ayer.
    ///
    /// Los huecos que la sesión traía y este layout no tiene viajan de vuelta
    /// intactos, en el rincón de huérfanos de [`SessionUi`].
    #[must_use]
    pub fn session_body(&self) -> norte_frontend::session::SessionBody {
        use norte_frontend::session::{SessionBody, SlotState};

        let mut body = SessionBody {
            layouts: std::iter::once(("default".to_owned(), self.layout.clone())).collect(),
            slots: self.session.orphans.clone(),
        };
        for id in self.layout.slot_ids() {
            let Some(pane) = self.panes.browser(id) else {
                continue;
            };
            let history = self.history.for_slot(id);
            body.slots.insert(
                id.0,
                SlotState {
                    path: pane.dir().clone(),
                    cursor: pane.cursor() as u64,
                    back: history.map(|h| h.trail().to_vec()).unwrap_or_default(),
                    forward: history
                        .map(|h| h.forward_trail().to_vec())
                        .unwrap_or_default(),
                    sort: pane.sort(),
                    // Las columnas son de la CONFIGURACIÓN por scheme, no
                    // estado por hueco: capturarlas aquí inventaría un estado
                    // que este frontend no tiene. El campo existe para quien
                    // sí lo tenga.
                    columns: Vec::new(),
                    show_hidden: pane.show_hidden(),
                    touched_ms: self.session.touched.get(&id.0).copied().unwrap_or_default(),
                },
            );
        }
        body
    }

    /// Aplica una sesión guardada y dice qué huecos necesitan listado.
    ///
    /// Pone la disposición, siembra cada listado con su directorio, su orden,
    /// sus ocultos y sus dos rastros, y GUARDA el cursor para cuando llegue el
    /// listado ([`Self::restore_cursor`]): sobre un pane vacío no hay fila 12
    /// donde ponerlo.
    ///
    /// Lo que el layout no tiene se conserva aparte en vez de tirarse.
    pub fn apply_session(
        &mut self,
        body: &norte_frontend::session::SessionBody,
    ) -> Vec<norte_frontend::layout::SlotId> {
        if let Some(tree) = body.layouts.get("default") {
            self.set_layout(tree.clone());
        }
        let mut ask = Vec::new();
        self.session.orphans.clear();
        for (raw, estado) in &body.slots {
            let id = norte_frontend::layout::SlotId(*raw);
            self.session.touched.insert(*raw, estado.touched_ms);
            let Some(pane) = self.panes.browser_mut(id) else {
                // Un hueco que este layout no tiene NO se borra: se guarda tal
                // cual y se vuelve a escribir. Volver a la disposición de ayer
                // devuelve el panel donde estaba.
                self.session.orphans.insert(*raw, estado.clone());
                continue;
            };
            *pane = Pane::new(estado.path.clone(), Vec::new());
            pane.set_sort(estado.sort);
            pane.set_show_hidden(estado.show_hidden);
            self.session.cursors.insert(*raw, estado.cursor);
            self.history
                .for_slot_mut(id)
                .seed(estado.back.clone(), estado.forward.clone());
            ask.push(id);
        }
        ask
    }

    /// Coloca el cursor que traía la sesión, ahora que el listado ya está.
    ///
    /// Se consume: es de UNA vez, la del arranque. Fuera del listado se clampa
    /// —un directorio con menos entradas que ayer no deja el cursor fuera— y
    /// eso lo hace [`Pane::set_cursor`].
    pub fn restore_cursor(&mut self, id: norte_frontend::layout::SlotId) {
        let Some(row) = self.session.cursors.remove(&id.0) else {
            return;
        };
        if let Some(pane) = self.panes.browser_mut(id) {
            pane.set_cursor(usize::try_from(row).unwrap_or(usize::MAX));
        }
    }

    /// Adopta huecos que otra ventana guardaba y esta no tenía (#231).
    ///
    /// Los que el layout VIVO tiene ganan los nuestros: esta pantalla es la que
    /// acaba de moverse. Los demás se guardan en el rincón de huérfanos y se
    /// vuelven a escribir tal cual — el único camino que trae este mapa es un
    /// relevo de propiedad, o sea justo cuando lo guardado no es nuestro, y
    /// reescribir encima sin más le tiraría a alguien el historial de un panel
    /// al que iba a volver.
    pub fn adopt_session_orphans(
        &mut self,
        ajenos: std::collections::BTreeMap<u32, norte_frontend::session::SlotState>,
    ) {
        let alive: std::collections::BTreeSet<u32> =
            self.layout.slot_ids().into_iter().map(|s| s.0).collect();
        for (id, estado) in ajenos {
            if alive.contains(&id) {
                continue;
            }
            self.session.touched.insert(id, estado.touched_ms);
            self.session.orphans.insert(id, estado);
        }
    }

    /// Marca un hueco como tocado AHORA, para la barrida por edad.
    pub fn touch_session_slot(&mut self, id: norte_frontend::layout::SlotId, now_ms: u64) {
        self.session.touched.insert(id.0, now_ms);
    }

    /// Aplica el cuerpo OPACO que vino del core, o dice por qué no.
    ///
    /// Un cuerpo que no se puede leer NO deja pantalla en blanco: se queda la
    /// disposición de la configuración y se avisa. Es la misma decisión que
    /// toma el core con un fichero corrupto, un proceso más allá.
    pub fn apply_session_value(&mut self, version: u32, v: &serde_json::Value) {
        match norte_frontend::session::SessionBody::from_value(version, v) {
            Ok(body) => {
                self.apply_session(&body);
            }
            // Un cuerpo de una versión MÁS NUEVA no se lee y tampoco se pisa:
            // esta ventana se declara suelta y deja de escribir. Sin esto, el
            // aviso salía y un segundo después el volcado publicaba encima la
            // pantalla de la configuración — «no se lee» acabando en «se
            // pierde», que es lo que ADR 0059 promete que no pasa.
            Err(e @ norte_frontend::session::SessionError::FromTheFuture { .. }) => {
                tracing::warn!(error = %e, "sesión de UI de una versión más nueva: no se escribe");
                self.session.detached = true;
                self.message = Some(t("msg-session-unreadable"));
            }
            Err(e) => {
                tracing::warn!(error = %e, "sesión de UI ilegible");
                self.message = Some(t("msg-session-unreadable"));
            }
        }
    }

    /// Pone la disposición `name`, y dice si lo consiguió.
    ///
    /// Primero `<dir>/layouts/<name>.toml` y después el preset de fábrica del
    /// mismo nombre: gana el fichero del usuario, como en todas las demás
    /// capas de configuración, y un preset se recupera borrando el fichero.
    /// Si el fichero está roto se avisa Y se cae al preset — un layout que no
    /// parsea no puede dejar a norte sin pantalla.
    ///
    /// Lee un fichero pequeño de config en el hilo que llama, como el
    /// `[ui] layout` del arranque.
    pub fn apply_loaded_layout(
        &mut self,
        name: &std::ffi::OsStr,
        loaded: Result<norte_frontend::layout::Node, norte_frontend::layout::LayoutError>,
    ) -> bool {
        use norte_frontend::layout::{LayoutError, presets};
        // El nombre se PINTA, y viene de un fichero o de la línea de
        // comandos: lossy marcado y hazards enmascarados, como cualquier otro
        // nombre (#246 m3). Los bytes no se tocan: los usó el cargador.
        let (showable, _) = norte_frontend::display_os_name(name);
        let showable = norte_encoding::mask_terminal_hazards(&showable);
        let broken = match loaded {
            Ok(tree) => {
                self.set_layout(tree);
                return true;
            }
            // Que no haya fichero es lo NORMAL para uno de fábrica: no se
            // avisa de nada.
            Err(LayoutError::NotFound(_)) => None,
            Err(e) => Some(e),
        };
        // Un preset de fábrica se llama por su nombre ASCII: un nombre que no
        // es texto no puede ser uno de ellos.
        let factory = name
            .to_str()
            .map_or(Err(LayoutError::NotFound(showable.clone())), presets::tree);
        match factory {
            Ok(tree) => {
                self.set_layout(tree);
                if let Some(e) = broken {
                    self.message = Some(ta(
                        "msg-layout-load-failed",
                        &[("name", &showable), ("err", &e.to_string())],
                    ));
                }
                true
            }
            Err(e) => {
                self.message = Some(ta(
                    "msg-layout-load-failed",
                    &[
                        ("name", &showable),
                        ("err", &broken.unwrap_or(e).to_string()),
                    ],
                ));
                false
            }
        }
    }
}
