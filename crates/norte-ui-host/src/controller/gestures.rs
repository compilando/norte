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
        h.estado = SlotState::Loading;
        self.pedir_listado(slot, &dir, token, backend, buzon);
    }

    /// Abre el rastro de navegación del hueco activo.
    ///
    /// Las filas son el MRU compartido (`History::entries`), más reciente
    /// primero: qué recuerda un panel no puede depender de quién lo pinta.
    pub(super) fn abrir_historial(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let slot = self.activo();
        let rastro = self.hueco().historial.entries().clone();
        self.selector = Some(crate::pickers::Selector::historial(slot, &rastro));
        self.gen_selector += 1;
        let cambio = ViewChange::Picker {
            picker: self.vista_selector(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
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
