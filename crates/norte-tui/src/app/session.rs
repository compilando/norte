//! La sesión de UI vista desde `App` (L2, ADR 0059): componer el cuerpo de
//! AHORA, aplicar el que se leyó del disco, recolocar el cursor, adoptar los
//! huecos huérfanos y sellar su edad.

use super::App;
use super::pane::Pane;
use norte_i18n::{t, ta};

impl App {
    /// Bajo qué clave de `layouts` va la pantalla de este proceso.
    ///
    /// El nombre del perfil activo, o `default` si no hay ninguno — que es la
    /// clave que usaba todo el mundo antes de que hubiera perfiles, así que un
    /// lector que nunca elija uno lee y escribe exactamente donde ya escribía.
    ///
    /// La conversión a texto es la única de D4: `layouts` es un objeto JSON.
    /// Un perfil cuyo directorio no sea UTF-8 cae a `default`, que es la
    /// consecuencia que el selector avisa por adelantado con su
    /// `carries_state`.
    fn session_key(&self) -> String {
        let activo = self.session_key_active();
        if activo.is_empty() {
            "default".to_owned()
        } else {
            activo
        }
    }

    /// El nombre del perfil activo para el campo `active` del cuerpo: vacío
    /// cuando no hay ninguno, o cuando el que hay no puede ser una clave.
    fn session_key_active(&self) -> String {
        self.active_profile
            .as_ref()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_owned()
    }

    /// La pantalla de AHORA como cuerpo de sesión (L2).
    ///
    /// Lleva la disposición y, por hueco de listado, dónde está, cómo mira y
    /// por dónde ha pasado. NO lleva las marcas: son el estado de una
    /// operación a medias, no de una sesión, y devolverlas al arrancar sería
    /// devolver un `F8` apuntando a lo que uno marcó ayer.
    ///
    /// Los huecos que la sesión traía y este layout no tiene viajan de vuelta
    /// intactos, en el rincón de huérfanos de [`super::SessionUi`].
    #[must_use]
    pub fn session_body(&self) -> norte_frontend::session::SessionBody {
        use norte_frontend::session::{SessionBody, SlotState};

        // La disposición de ESTE perfil va bajo su nombre; las de los demás
        // vuelven tal cual. Escribir solo la del activo borraría del documento
        // el sitio donde los otros perfiles dejaron sus paneles.
        let mut layouts = self.session.other_layouts.clone();
        layouts.insert(self.session_key(), self.layout.clone());
        let mut body = SessionBody {
            active: self.session_key_active(),
            layouts,
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

    /// `ntc <DIR>` sobre una sesión aplicada: el panel ACTIVO pasa a `dir`
    /// y nada más cambia. La sesión guardada es más específica que la
    /// configuración, pero un directorio escrito en la línea de órdenes es
    /// más específico que las dos: quien teclea `ntc ~/proyecto` quiere ver
    /// `~/proyecto`, no donde cerró ayer.
    ///
    /// Conserva el orden y los ocultos del panel (son preferencias, no
    /// sitio) y olvida el cursor guardado, que era una fila de OTRO
    /// directorio. El hueco sigue en la lista de los que hay que listar.
    pub fn pin_start_dir(&mut self, dir: norte_proto::VPath) {
        let idx = self.focus();
        let slot = self.panes.slot_of(idx);
        let pane = &self.panes[idx];
        let (sort, hidden) = (pane.sort(), pane.show_hidden());
        // Por la puerta de adopción, como los otros dos listados que nacen
        // fuera del constructor. Puesto a mano, este se quedaba sin la fila
        // `..` — y `set_listing` no lo cura después, porque `poner_padre` no
        // hace nada desde `Apagada`: el panel se quedaba sin ella hasta el
        // siguiente hot-reload de la config.
        self.adoptar_pane(slot, Pane::new(dir, Vec::new()), Some(sort), Some(hidden));
        self.session.cursors.remove(&slot.0);
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
        let clave = self.session_key();
        if let Some(tree) = body.layouts.get(&clave) {
            self.set_layout(tree.clone());
        }
        // Lo de los OTROS perfiles se guarda entero para volver a escribirlo:
        // este proceso mira un perfil y el documento es de todos.
        self.session.other_layouts = body
            .layouts
            .iter()
            .filter(|(k, _)| **k != clave)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let mut ask = Vec::new();
        self.session.orphans.clear();
        for (raw, estado) in &body.slots {
            let id = norte_frontend::layout::SlotId(*raw);
            self.session.touched.insert(*raw, estado.touched_ms);
            // «¿Tiene ESTE layout el hueco?» se le pregunta al LAYOUT, no al
            // almacén de panes: los huérfanos siguen en el almacén, así que
            // `browser(id).is_some()` contestaba que sí para un hueco que la
            // disposición no coloca — y ese hueco entraba por la puerta de
            // adopción en vez de conservarse tal cual, que es lo que promete
            // el párrafo de abajo.
            if !self.layout.slot_ids().contains(&id) {
                // Un hueco que este layout no tiene NO se borra: se guarda tal
                // cual y se vuelve a escribir. Volver a la disposición de ayer
                // devuelve el panel donde estaba.
                self.session.orphans.insert(*raw, estado.clone());
                continue;
            }
            // El pane se levanta sobre la ruta guardada y ADOPTA la
            // configuración de esta sesión: el orden y los ocultos son de la
            // sesión, la fila `..` es de la config. Poniéndola a mano aquí,
            // se perdía en cada restauración.
            self.adoptar_pane(
                id,
                Pane::new(estado.path.clone(), Vec::new()),
                Some(estado.sort),
                Some(estado.show_hidden),
            );
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
                // El perfil PEGAJOSO llega AQUÍ y no antes: vive en la sesión,
                // y la sesión la tiene el daemon, al que se llega con la
                // configuración que ya está cargada. Así que se pide el cambio
                // y lo hace el bucle por el mismo camino que cualquier otro
                // (ADR 0079, D8) — con la única baja que ese camino tiene:
                // `[ui] lang` no se puede reaplicar, y se anuncia.
                //
                // Un `--profile` explícito ya dejó `active_profile` puesto
                // antes de llegar aquí, y entonces el pegajoso NO manda: el
                // lector nombró uno para esta vez.
                if self.active_profile.is_none() && !body.active.is_empty() {
                    self.pending_profile = Some(std::ffi::OsString::from(&body.active));
                }
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
    /// La REGLA —fichero del usuario, y si no el preset— vive en
    /// [`norte_frontend::layout::config::or_preset`], compartida con la
    /// ventana: aquí estaba escrita a mano y la ventana no la tenía, así que
    /// `norte-gui --layout mio` no podía abrir un layout del usuario. Lo que
    /// queda aquí es lo que sí es del TUI: poner el árbol y pintar el aviso.
    ///
    /// Lee un fichero pequeño de config en el hilo que llama, como el
    /// `[ui] layout` del arranque.
    pub fn apply_loaded_layout(
        &mut self,
        name: &std::ffi::OsStr,
        loaded: Result<norte_frontend::layout::Node, norte_frontend::layout::LayoutError>,
    ) -> bool {
        // El nombre se PINTA, y viene de un fichero o de la línea de
        // comandos: lossy marcado y hazards enmascarados, como cualquier otro
        // nombre (#246 m3). Los bytes no se tocan: los usó el cargador.
        // Y con su marca si hubo bytes que no se podían pintar: sin ella
        // `$'\xff'` y `$'\xfe'` dan el MISMO mensaje y el lector no puede
        // saber cuál de los dos nombró.
        let (showable, lossy) = norte_frontend::display_os_name(name);
        let showable = norte_encoding::mask_terminal_hazards(&showable);
        let showable = if lossy {
            format!("{} {showable}", crate::ui::HOSTILE_BADGE)
        } else {
            showable
        };
        match norte_frontend::layout::config::or_preset(name, loaded) {
            Ok((tree, roto)) => {
                self.set_layout(tree);
                if let Some(e) = roto {
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
                    &[("name", &showable), ("err", &e.to_string())],
                ));
                false
            }
        }
    }
}
