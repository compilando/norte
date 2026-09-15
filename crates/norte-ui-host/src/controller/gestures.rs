//! Los gestos de panel: espejar, traer, intercambiar, ir y volver.
//!
//! Parte de `controller`: son métodos de `Estado`, movidos aquí sin
//! tocarlos (ADR 0086). El único escritor sigue siendo el actor.

// Estos módulos son el mismo `impl Estado` partido en trozos, así que usan
// los mismos imports que el padre. Enumerarlos aquí sería una lista de
// cuarenta líneas por fichero, en 32 ficheros, que se desincroniza en cuanto
// el padre importa algo — `super::*` la sigue sola.
#[allow(clippy::wildcard_imports)]
use super::*;

impl Estado {
    /// Los tres gestos de panel de la ADR 0058: espejo, traer e intercambiar.
    ///
    /// Los tres necesitan el OTRO hueco, y el otro hueco lo dice el rol
    /// compartido —el mismo del que sale el destino de una copia—, nunca «el
    /// de al lado»: con tres listados, adivinar es mandar el panel de alguien
    /// a un sitio que no eligió.
    pub(super) fn gesto_de_panel(
        &mut self,
        efecto: Efecto,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let otro = match self.hueco_destino() {
            Ok(id) => id,
            Err(clave) => {
                return (
                    ActionAck::Unavailable {
                        reason_key: clave.to_owned(),
                    },
                    Vec::new(),
                );
            }
        };
        let activo = self.activo();
        match efecto {
            // Lo que viaja es A DÓNDE VA el panel de origen, no lo que
            // enseña: durante una navegación `pane.dir()` responde todavía
            // por el directorio que se abandona, y espejar eso mandaría al
            // otro panel al sitio del que el lector acaba de salir.
            Efecto::Espejo | Efecto::EspejoObjetivo | Efecto::Traer => {
                let (origen, llega) = if matches!(efecto, Efecto::Traer) {
                    (otro, activo)
                } else {
                    (activo, otro)
                };
                let Some(destino) = self.destino_del_gesto(efecto, origen) else {
                    return (Self::obsoleta(StaleAction::Generation), Vec::new());
                };
                if self.dir_en_curso(llega).as_ref() == Some(&destino) {
                    // Los dos ya están ahí. Un cd redundante RE-LISTA el
                    // panel que llega: `set_listing` le borra las marcas y
                    // una navegación —a diferencia de un refresco— no las
                    // restaura, además de deslizarle el listado bajo el
                    // cursor. Todo eso a cambio de nada, porque ya enseña lo
                    // que se le pide. El TUI lo rehúsa por lo mismo
                    // (`gestures::mirror_plan`).
                    return (self.aplicada(), Vec::new());
                }
                (
                    self.aplicada(),
                    self.navegar_hueco(llega, &destino, Trail::Record, backend, buzon),
                )
            }
            Efecto::Intercambiar => self.intercambiar_huecos(activo, otro, backend, buzon),
            // El `match` de arriba no manda aquí nada más.
            _ => Self::no_muta(),
        }
    }

    /// La ubicación que VIAJA en un gesto de panel, leída del hueco `origen`.
    ///
    /// Para espejo y traer es [`Self::dir_en_curso`]. Para
    /// [`Efecto::EspejoObjetivo`] es la carpeta bajo el cursor si lo es
    /// (`PaneState::target_dir`, la misma respuesta que da el TUI) — salvo con
    /// una navegación EN VUELO, donde el cursor sigue siendo el del listado
    /// que se abandona y lo que vale es a dónde va el hueco.
    pub(super) fn destino_del_gesto(&self, efecto: Efecto, origen: u32) -> Option<VPath> {
        let hueco = self.huecos.get(&origen)?;
        if matches!(efecto, Efecto::EspejoObjetivo) && hueco.dir_pedido.is_none() {
            return Some(hueco.pane.target_dir().clone());
        }
        self.dir_en_curso(origen)
    }

    /// A dónde va un hueco: el directorio pedido si hay una navegación en
    /// vuelo, y si no el que enseña.
    ///
    /// `None` solo si el hueco no existe, que para quien llama es una
    /// pantalla que cambió por debajo.
    pub(super) fn dir_en_curso(&self, slot: u32) -> Option<VPath> {
        let h = self.huecos.get(&slot)?;
        Some(h.dir_pedido.clone().unwrap_or_else(|| h.pane.dir().clone()))
    }

    /// Los dos listados cambian de sitio. NO toca disco.
    ///
    /// Lo que se intercambia es el CONTENIDO del hueco —listado, cursor,
    /// marcas, rastro y orden—, porque partirlo más sería inventar reglas
    /// sobre qué se queda dónde. El foco no se mueve: quien lo tenía sigue
    /// teniéndolo, y ahora enseña lo otro, que es lo que el gesto significa.
    ///
    /// Lo único que NO viaja es la ventana de pintado (`primera_visible` y
    /// `visibles`): esa es geometría del SLOT, no del listado.
    ///
    /// Lo que estaba EN VUELO es la parte que no se ve. Una respuesta viaja
    /// etiquetada con su hueco, así que tras el intercambio llegaría al hueco
    /// equivocado y se descartaría por testigo: el panel se quedaría cargando
    /// para siempre. Se vuelve a pedir, apuntando a donde iba. Lo mismo con
    /// el sondeo y la decoración, que casan por RUTA y por eso no pintarían
    /// nada raro, pero dejarían la memoria de «ya se pidió» sobre un listado
    /// que ya no está ahí — o sea columnas de tamaño en blanco para siempre.
    pub(super) fn intercambiar_huecos(
        &mut self,
        a: u32,
        b: u32,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if !self.huecos.contains_key(&a) || !self.huecos.contains_key(&b) {
            // Uno de los dos desapareció entre el rol y aquí. Se comprueba
            // ANTES de sacar ninguno: los dos `remove` de una tupla se
            // evalúan los dos antes de casar el patrón, así que salir por el
            // camino de error con uno ya extraído lo DROPEA — «se deshace lo
            // hecho» no deshacía nada.
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        }
        let (Some(mut ha), Some(mut hb)) = (self.huecos.remove(&a), self.huecos.remove(&b)) else {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        };
        // La ventana de pintado se queda en SU slot. `primera_visible` y
        // `visibles` no describen el listado: los pone el renderer con
        // `set_visible_range`, y su `scrollTop` es suyo — un intercambio no
        // lo mueve ni dispara un evento de scroll que lo recalcule. Si
        // viajaran con el hueco, cada panel pintaría filas de una banda que
        // el lector no tiene delante y los DOS se verían vacíos, sin nada
        // que lo corrigiera salvo arrastrar la barra a mano.
        std::mem::swap(&mut ha.primera_visible, &mut hb.primera_visible);
        std::mem::swap(&mut ha.visibles, &mut hb.visibles);
        self.huecos.insert(a, hb);
        self.huecos.insert(b, ha);
        for slot in [a, b] {
            if let Some(h) = self.huecos.get_mut(&slot) {
                h.cancelar_sondeo
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                h.cancelar_sondeo = std::sync::Arc::default();
                h.sondeando = false;
                h.sondeados.clear();
                h.olvidar_adornos();
                h.adornando = false;
            }
            self.reanudar_peticion(slot, backend, buzon);
        }
        // Cambian las dos mitades de la pantalla a la vez —filas, cabeceras,
        // ruta, cursor y estado—, así que viaja una FOTO y no seis parches.
        let snap = self.snapshot();
        (
            self.aplicada(),
            vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))],
        )
    }

    /// Vuelve a pedir lo que este hueco tenía en vuelo, con testigo nuevo.
    ///
    /// «En vuelo» son DOS cosas, y mirar solo la primera dejaba pasar el caso
    /// común. `en_vuelo` se limpia en cuanto aterriza la primera página,
    /// mientras `drenando` sigue trayendo el resto del stream: en un
    /// directorio de más de `FIRST_PAGE` entradas —o sea casi cualquiera— hay
    /// una ventana en la que solo vive el drenaje. Los lotes que siguieran
    /// llegando se descartarían por testigo (no se cruzan de hueco, eso está
    /// bien), y el listado se quedaría congelado en las cien primeras
    /// entradas, en `Ready`, sin decir nada: marcar todo actuaría sobre ese
    /// trozo.
    ///
    /// Los dos casos se repiden distinto:
    ///
    /// - **Navegación**: conserva el DESTINO de la petición vieja, no el
    ///   directorio del que salía.
    /// - **Solo drenaje**: la primera página ya está en pantalla, así que
    ///   esto es un REFRESCO de lo que el lector mira — cursor y marcas
    ///   vuelven, con la misma disciplina que [`Self::refrescar`].
    ///
    /// Sin ninguna de las dos no hace nada, y no gasta testigo.
    pub(super) fn reanudar_peticion(
        &mut self,
        slot: u32,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        let Some(h) = self.huecos.get(&slot) else {
            return;
        };
        let navegando = h.en_vuelo.is_some();
        if !navegando && h.drenando.is_none() {
            return;
        }
        self.token += 1;
        let token = RequestToken(self.token);
        let Some(h) = self.huecos.get_mut(&slot) else {
            return;
        };
        let dir = if navegando {
            h.dir_pedido.clone().unwrap_or_else(|| h.pane.dir().clone())
        } else {
            // El hueco YA está en su directorio: lo que faltaba era el resto.
            h.pane.dir().clone()
        };
        if !navegando {
            if let Some(sel) = h.pane.selected().map(|e| e.path.clone()) {
                h.pane.set_pending_focus(sel);
            }
            h.pane.remember_cursor();
            // `marked_paths` cae al cursor sin marcas, y restaurar ESO sería
            // una marca que nadie hizo.
            h.marcas_a_restaurar = if h.pane.marks_len() > 0 {
                h.pane.marked_paths()
            } else {
                Vec::new()
            };
        }
        h.en_vuelo = Some(token);
        h.drenando = Some(token);
        // CON el destino cuando lo hay. Esta función documenta tres líneas
        // más arriba que conserva el destino de la petición vieja, y luego lo
        // tiraba: intercambiar dos paneles mientras uno navega degradaba
        // «yendo a X» a «cargando…» sobre un cuerpo que sigue enseñando el
        // directorio ANTERIOR — la mezcla ilegible que esto existe para
        // evitar.
        let enc = h.pane.name_encoding();
        h.estado = Self::cargando_hacia(navegando.then_some(&dir), enc);
        self.pedir_listado(slot, &dir, token, backend, buzon);
    }

    /// Abre la historia del hueco activo.
    pub(super) fn abrir_historial(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let slot = self.activo();
        self.abrir_lista_de_historia(slot, "picker-history-title", false, None)
    }

    /// Abre la historia de un LADO de la pantalla (spec 2026-09-15 D7): lo
    /// elegido navega ESE hueco aunque el foco esté en el otro. Qué es un lado
    /// lo dice la geometría del reparto, como en los volúmenes.
    pub(super) fn abrir_historial_de_lado(
        &mut self,
        derecha: bool,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(slot) = self.listado_del_lado(derecha) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-no-other-slot".to_owned(),
                },
                Vec::new(),
            );
        };
        let titulo = if derecha {
            "picker-history-title-right"
        } else {
            "picker-history-title-left"
        };
        self.abrir_lista_de_historia(slot, titulo, false, None)
    }

    /// Abre los populares de la sesión (D6), navegando el hueco activo.
    pub(super) fn abrir_populares(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let slot = self.activo();
        self.abrir_lista_de_historia(slot, "picker-popular-title", true, None)
    }

    /// Abre —o rehace, con `cursor`— una lista de historia sobre `slot`.
    ///
    /// Las filas son las COMPARTIDAS (`norte_frontend::history`): qué recuerda
    /// un panel, en qué orden y con qué marca no puede depender de quién lo
    /// pinta. El tope de `[ui] history_size` se aplica aquí y al navegar, que
    /// son los dos sitios donde la historia se lee o crece.
    fn abrir_lista_de_historia(
        &mut self,
        slot: u32,
        titulo: &'static str,
        populares: bool,
        cursor: Option<usize>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let tope = self.config.common.ui_chrome.history_size();
        let Some(hueco) = self.huecos.get_mut(&slot) else {
            // El hueco se fue con la lista puesta: se cierra, como cuando pasa
            // lo mismo al elegir. Dejarla abierta ofrecía filas de un panel que
            // ya no existe.
            let cerrar = self.selector.take().is_some();
            let envios = if cerrar {
                vec![self.parche(vec![ViewChange::Picker { picker: None }])]
            } else {
                Vec::new()
            };
            return (
                ActionAck::Unavailable {
                    reason_key: "host-no-other-slot".to_owned(),
                },
                envios,
            );
        };
        hueco.historial.set_capacity(tope);
        let actual = hueco.pane.dir().clone();
        // La historia se pinta con la reinterpretación de SU panel, como el
        // terminal y como la barra de rutas (#98/F4: una lista es superficie de
        // decisión). Los populares con ninguna: son de toda la sesión, y aplicar
        // el encoding de un panel a rutas de otro sería inventar mojibake.
        let enc = if populares {
            None
        } else {
            hueco.pane.name_encoding()
        };
        let filas = if populares {
            norte_frontend::history::popular_rows(&self.popular, &actual, "")
        } else {
            norte_frontend::history::history_rows(&hueco.historial, &actual, "")
        };
        let mut selector = crate::pickers::Selector::historia(
            slot,
            &filas,
            |p| norte_frontend::path_display_with(p, enc),
            self.lang,
            titulo,
            populares,
        );
        if let Some(c) = cursor {
            selector.senalar(c.min(filas.len().saturating_sub(1)));
        }
        self.selector = Some(selector);
        self.gen_selector += 1;
        let cambio = ViewChange::Picker {
            picker: self.vista_selector(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// `dialog.remove` sobre una lista de historia (D2): la fila del cursor
    /// sale del rastro del hueco —o de los populares— y la lista se rehace sin
    /// perder el sitio.
    pub(super) fn quitar_de_historia(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(s) = self.selector.as_ref() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        let (slot, titulo, populares, cursor) =
            (s.slot(), s.titulo(), s.es_populares(), s.cursor());
        let Some(destino) = s.elegir() else {
            return (self.aplicada(), Vec::new());
        };
        if populares {
            self.popular.remove(&destino);
        } else if let Some(h) = self.huecos.get_mut(&slot) {
            // La fila «aquí» no se quita: la lista la pone siempre, así que
            // quitarla no la quitaría de la pantalla, y `History::remove` sí
            // podaría del rastro el directorio actual y su punto de salto sin
            // que se viera (rust-reviewer, fase 1).
            if *h.pane.dir() == destino {
                return (self.aplicada(), Vec::new());
            }
            h.historial.remove(&destino);
        }
        self.abrir_lista_de_historia(slot, titulo, populares, Some(cursor))
    }

    /// `dialog.clear` sobre una lista de historia (D2). Sin confirmación, como
    /// en el terminal: es memoria de navegación, no ficheros, y el aviso lo
    /// dice.
    pub(super) fn vaciar_historia(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(s) = self.selector.as_ref() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        let (slot, titulo, populares) = (s.slot(), s.titulo(), s.es_populares());
        let clave = if populares {
            self.popular.clear();
            "msg-popular-cleared"
        } else {
            if let Some(h) = self.huecos.get_mut(&slot) {
                h.historial.clear();
            }
            "msg-history-cleared"
        };
        let (ack, mut envios) = self.abrir_lista_de_historia(slot, titulo, populares, Some(0));
        envios.extend(self.decir(clave));
        (ack, envios)
    }

    /// `dialog.confirm-other` (D2): lo elegido va al OTRO hueco y el foco se
    /// queda donde está. El otro de una lista del foco es el destino; el de una
    /// lista de un lado que no tiene el foco, el foco.
    pub(super) fn elegir_del_selector_en_otro(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(s) = self.selector.as_ref() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        let (desde, destino, hay_fila) = (s.slot(), s.elegir(), s.hay_fila());
        let otro = if desde == self.activo() {
            self.hueco_destino()
        } else {
            Ok(self.activo())
        };
        let otro = match otro {
            Ok(otro) => otro,
            Err(clave) => {
                return (
                    ActionAck::Unavailable {
                        reason_key: clave.to_owned(),
                    },
                    Vec::new(),
                );
            }
        };
        let Some(destino) = destino else {
            // Hay fila y no lleva a ninguna parte: un favorito cuya ruta no
            // parsea. Se DICE, igual que al elegirlo en su sitio.
            if hay_fila {
                return (
                    ActionAck::Unavailable {
                        reason_key: "hotlist-invalid".to_owned(),
                    },
                    Vec::new(),
                );
            }
            return (self.aplicada(), Vec::new());
        };
        self.selector = None;
        let cierre = self.parche(vec![ViewChange::Picker { picker: None }]);
        let mut envios = vec![cierre];
        envios.extend(self.navegar_hueco(otro, &destino, Trail::Record, backend, buzon));
        (self.aplicada(), envios)
    }

    /// `nav.jump-back` (D5): una navegación NORMAL al punto de salto del hueco
    /// activo. Entra en el rastro, así que `nav.back` deshace el salto.
    pub(super) fn saltar_al_punto(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match norte_frontend::history::jump_target(&self.hueco().historial) {
            Ok(destino) => {
                let envios = self.navegar(&destino, Trail::Record, backend, buzon);
                (self.aplicada(), envios)
            }
            Err(clave) => (
                ActionAck::Unavailable {
                    reason_key: clave.to_owned(),
                },
                Vec::new(),
            ),
        }
    }

    /// `nav.set-jump-point` (D5): marca el directorio del hueco activo.
    pub(super) fn fijar_punto_de_salto(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let dir = self.hueco().pane.dir().clone();
        self.hueco_mut().historial.set_jump(dir);
        let envios = self.decir("msg-nav-jump-point-set");
        (self.aplicada(), envios)
    }

    /// Abre los favoritos de la configuración con la que arrancó la ventana.
    ///
    /// Los mismos que alimentan la barra lateral, y de la misma fuente: dos
    /// listas de favoritos que se leen distinto serían dos configuraciones.
    pub(super) fn abrir_hotlist(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let slot = self.activo();
        let favoritos: Vec<(String, Result<VPath, String>)> = self
            .config
            .common
            .hotlist
            .iter()
            .map(|h| (h.name.clone(), h.target.clone()))
            .collect();
        self.selector = Some(crate::pickers::Selector::hotlist(
            slot, &favoritos, self.lang,
        ));
        self.gen_selector += 1;
        let cambio = ViewChange::Picker {
            picker: self.vista_selector(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }
}
