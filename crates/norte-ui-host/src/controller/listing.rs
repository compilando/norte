//! Pedir un listado, aterrizarlo y refrescar lo que la operación tocó.
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
    /// Pregunta cómo pliega nombres el directorio de un hueco (#268).
    ///
    /// Se pide al ATERRIZAR y no delante de cada diálogo: hacerlo al copiar
    /// metería un viaje al daemon en el camino de F5, que es la tecla que más
    /// se pulsa de un gestor ortodoxo. Aquí va detrás de un listado que ya
    /// costó una ronda, y la respuesta sirve para todas las copias que salgan
    /// de ese directorio.
    ///
    /// Un fallo no dice nada y no rompe nada: sin respuesta no se pliega, que
    /// es exactamente lo que se hacía antes de #268.
    pub(super) fn pedir_pliegue(
        &mut self,
        slot: u32,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        let Some(hueco) = self.huecos.get_mut(&slot) else {
            return;
        };
        let dir = hueco.pane.dir().clone();
        // El de antes ya no vale: es de otro sitio.
        hueco.pliegue = None;
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let Ok(caps) = backend.capabilities(dir.clone()).await else {
                return;
            };
            let modo = norte_vfs::fold_mode_of(caps);
            let _ = buzon.send(Mensaje::Pliegue(slot, dir, modo)).await;
        });
    }

    /// Guarda el modo de plegado, si el hueco sigue donde estaba.
    ///
    /// La comprobación del directorio no es paranoia: entre pedir y contestar
    /// cabe una navegación entera, y guardar el pliegue de otro sitio haría
    /// que la comprobación del lote mintiera en la dirección permisiva.
    pub(super) fn aplicar_pliegue(
        &mut self,
        slot: u32,
        dir: &VPath,
        modo: norte_encoding::FoldMode,
    ) {
        if let Some(h) = self.huecos.get_mut(&slot)
            && h.pane.dir() == dir
        {
            h.pliegue = Some(modo);
        }
    }

    /// Un listado que se pidió antes acaba de volver.
    ///
    /// `None` = llegó TARDE y otra navegación lo relevó. Se descarta aquí y
    /// no se esconde en el renderer.
    pub(super) fn aterrizar_listado(
        &mut self,
        datos: RespuestaListado,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let (token, slot, dir, res) = datos;
        if self.huecos.get(&slot).and_then(|h| h.en_vuelo) != Some(token) {
            return Vec::new();
        }
        // #327: la entrada declara `secret = "prompt"` y ninguna de las tres
        // fuentes lo tiene. Se PREGUNTA en vez de pintar el error, que es lo
        // único que esta ventana sabía hacer: el texto de `err-secret-needed`
        // nombra una variable de entorno y ahí se acababa el camino.
        //
        // El estado del hueco se deja como lo dejaría cualquier otro error
        // —a propósito—: si el diálogo se cierra sin contestar, lo que queda
        // detrás es la pantalla que ya sabía explicarse.
        if let Err(Error::SecretNeeded { conn, endpoint }) = &res {
            let (conn, endpoint) = (conn.clone(), endpoint.clone());
            self.aterriza_en(slot, dir.clone(), res);
            // La foto ANTES del diálogo: el hueco acaba de cambiar de estado y
            // el diálogo se apila encima. Al revés, el renderer vería la
            // pregunta sobre la pantalla anterior.
            let snap = self.snapshot();
            let mut fuera = vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))];
            fuera.extend(self.pedir_secreto(conn, &endpoint, slot, dir));
            return fuera;
        }
        self.aterriza_en(slot, dir, res);
        self.pedir_pliegue(slot, backend, buzon);
        self.sondear(slot, backend, buzon);
        self.adornar(slot, backend, buzon);
        // Los hechos de la ayuda describen la entrada bajo el CURSOR, y este
        // listado es otro (#262). La foto de abajo la lleva ya re-congelada,
        // así que aquí no se fabrica parche: gastaría un número de secuencia
        // que nadie recibiría.
        self.recongelar_hechos_de_ayuda();
        // Un `cd` cambia la pantalla entera —directorio, filas, cursor,
        // marcas—, así que se manda una foto en vez de enumerar parches que
        // el renderer tendría que casar.
        let snap = self.snapshot();
        vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))]
    }

    /// Lo que un sondeo averiguó, aplicado; y se pide la siguiente tanda.
    ///
    /// `MAX_SONDEOS` acota cada VUELTA, no la ventana: sin volver a pedir,
    /// una ventana más alta que una tanda se quedaba a medias en silencio.
    pub(super) fn aterrizar_sondas(
        &mut self,
        datos: Sondas,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let (dir, slot, sondas) = datos;
        let u = self.aplicar_sondas(slot, &dir, &sondas)?;
        self.sondear(slot, backend, buzon);
        self.adornar(slot, backend, buzon);
        Some(u)
    }

    /// Lo que los plugins dijeron, pegado al hueco que lo pidió.
    ///
    /// `None` si el hueco desapareció o si el listado es OTRO: pegar unas
    /// insignias de un directorio a las filas de otro es exactamente el
    /// fallo que la clave por RUTA evita, y aun así se comprueba el
    /// directorio — las rutas de dos directorios distintos no casan, pero
    /// gastar un parche entero para no pintar nada sí se puede evitar.
    pub(super) fn aplicar_adornos(&mut self, datos: Adornos) -> Option<BridgeEnvelope<UiUpdate>> {
        let (slot, dir, adornos, celdas) = datos;
        let hueco = self.huecos.get_mut(&slot)?;
        hueco.adornando = false;
        if *hueco.pane.dir() != dir {
            return None;
        }
        if adornos.is_empty() && celdas.is_empty() {
            // Ningún decorador consentido y ninguna columna de plugin. No es
            // un fallo y no repinta nada.
            return None;
        }
        hueco.adornos.extend(adornos);
        for (columna, valores) in celdas {
            hueco
                .celdas_plugin
                .entry(columna)
                .or_default()
                .extend(valores);
        }
        // Y al pane, que es quien las sirve: sus setters REEMPLAZAN, así que
        // se le pasa el acumulado entero y no el lote.
        hueco.pane.set_decorations(hueco.adornos.clone());
        hueco.pane.set_plugin_columns(hueco.celdas_plugin.clone());
        // Las FILAS, que es lo único que cambia: una insignia no mueve el
        // cursor ni el directorio.
        Some(self.parche_filas())
    }

    /// Un lote más del listado que se está drenando por detrás.
    pub(super) fn aterrizar_lote(
        &mut self,
        datos: (RequestToken, u32, Vec<Entry>, bool),
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let (token, slot, batch, ultimo) = datos;
        let Some(u) = self.aplicar_lote(slot, token, batch, ultimo) else {
            return Vec::new();
        };
        self.sondear(slot, backend, buzon);
        self.adornar(slot, backend, buzon);
        // El listado creció por debajo: si la ayuda está delante, sus hechos
        // hablan de otra entrada (#262). Aquí no hay foto que lo arrastre,
        // así que va su propio parche.
        let mut salida = vec![u];
        salida.extend(self.recongelar_ayuda());
        salida
    }

    /// El movimiento, cuando el foco está en un panel que no es un listado.
    ///
    /// `None` = el foco está en un listado, o el efecto no es un movimiento y
    /// sigue su camino normal. Quién toma teclas lo dice el registro
    /// COMPARTIDO de kinds (`takes_keys`), no una lista aquí: la hoja de
    /// atributos se enfoca y NO toma teclas a propósito —sigue al cursor del
    /// listado, así que con el teclado dentro dejaría de seguir a nada—, y
    /// esa decisión ya está tomada en un sitio.
    pub(super) fn efecto_en_panel_enfocado(
        &mut self,
        efecto: Efecto,
    ) -> Option<(ActionAck, Vec<BridgeEnvelope<UiUpdate>>)> {
        let SlotId(id) = self.roles.get(RoleId::Active)?;
        if self.huecos.contains_key(&id) {
            return None;
        }
        let kind = kind_de(&self.arbol, SlotId(id))?;
        if !self.kinds.get(&kind).is_some_and(|d| d.takes_keys) {
            return None;
        }
        if kind.as_str() == "places" {
            return self.efecto_en_sitios(efecto);
        }
        if kind.as_str() != "processes" {
            // Otro panel que toma teclas y que este host todavía no proyecta:
            // se deja pasar, y el listado sigue respondiendo. Cuando se
            // proyecte, su brazo entra aquí.
            return None;
        }
        // Qué es SUYO se decide antes de mirar cuántas filas hay, y ese orden
        // es el arreglo: con el tablero vacío esto contestaba «aplicado» a
        // CUALQUIER efecto, así que el tabulador que sirve para salir del
        // panel se lo tragaba él. Se entraba en procesos y no se salía —un
        // anillo que entra y no sale es una trampa, y sin ratón no había
        // vuelta.
        if !matches!(
            efecto,
            Efecto::Cursor(_) | Efecto::Pagina(_) | Efecto::Extremo { .. }
        ) {
            return None;
        }
        let filas = self.filas_de_tablero();
        if filas == 0 {
            return Some((self.aplicada(), Vec::new()));
        }
        let total = i64::try_from(filas).unwrap_or(i64::MAX);
        let paso = |n: i64| -> i64 { n.clamp(-total, total) };
        let actual = i64::try_from(self.cursor_procesos.min(filas - 1)).unwrap_or(0);
        let destino = match efecto {
            Efecto::Cursor(n) => actual.saturating_add(paso(n)),
            // Una página del panel de procesos son sus filas: no hay ventana
            // declarada para él, y saltar más de lo que hay no significa nada.
            Efecto::Pagina(n) => actual.saturating_add(paso(n).saturating_mul(total)),
            Efecto::Extremo { al_final: false } => 0,
            Efecto::Extremo { al_final: true } => total - 1,
            // Los tres de arriba son los únicos que llegan aquí: el filtro
            // está en la guarda de la entrada.
            _ => return None,
        };
        self.cursor_procesos = usize::try_from(destino.max(0)).unwrap_or(0).min(filas - 1);
        // Va como FOTO y no como parche: no hay un `ViewChange` para un hueco
        // que no es un listado, y añadir uno por un cursor de tres dígitos es
        // contrato nuevo para nada. Es una tecla, no un scroll continuo.
        let snap = self.snapshot();
        Some((
            self.aplicada(),
            vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))],
        ))
    }

    /// Pide el listado de `dir` para `slot`, con el testigo ya reservado.
    ///
    /// Extraído de la navegación para que otra cosa que estrena huecos —un
    /// cambio de disposición— pida por el MISMO camino: dos formas de pedir
    /// un listado son dos sitios donde olvidarse del catálogo de atributos o
    /// del testigo.
    pub(super) fn pedir_listado(
        &mut self,
        slot: u32,
        dir: &VPath,
        token: RequestToken,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        self.pedir_catalogo(dir, backend, buzon);
        // Queda apuntado A DÓNDE va: lo que el hueco enseña no cambia hasta
        // que esto aterrice, y hasta entonces `pane.dir()` responde por el
        // directorio que se abandona.
        if let Some(h) = self.huecos.get_mut(&slot) {
            h.dir_pedido = Some(dir.clone());
        }
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        let dir = dir.clone();
        let attrs = self.attrs_de(&dir);
        tokio::spawn(async move {
            let stream = backend.list(dir.clone(), attrs).await;
            let res = Estado::primera_pagina(stream, slot, token, buzon.clone()).await;
            // Si el actor ya no está, la respuesta no le importa a nadie.
            let _ = buzon
                .send(Mensaje::Listado(Box::new((token, slot, dir, res))))
                .await;
        });
    }

    /// Vuelve a listar los huecos que esta task dejó desactualizados, y
    /// OLVIDA lo que afectaba: un desenlace se aplica una vez.
    ///
    /// Por directorio y no por hueco: quien encoló la task sabía qué
    /// directorios tocaba, no qué paneles estarán mirándolos cuando termine
    /// —el lector puede haber navegado, o haber cambiado de disposición—.
    ///
    /// Un hueco cuenta como afectado por a dónde VA si tiene algo en vuelo, y
    /// por lo que enseña si no: los dos son «el directorio de este panel», y
    /// mirar solo el segundo dejaba sin refrescar al panel que estaba
    /// entrando justo en el sitio que la mutación cambió.
    pub(super) fn refrescar_afectados(
        &mut self,
        task_id: u64,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<ViewChange> {
        let afectados = match self.tasks.get(&task_id) {
            Some(t) if !t.afectados.is_empty() => t.afectados.clone(),
            _ => return Vec::new(),
        };
        // Mientras quede OTRA task viva sobre el mismo directorio, no se
        // relista: un lote de doscientas copias produciría doscientos
        // listados del mismo panel, cada uno invalidando el anterior y
        // volviendo a pagar el sondeo y las decoraciones de plugin (medidas
        // en 167 ms por página de veinte). Se refresca cuando termina la
        // ÚLTIMA, que es cuando el directorio deja de moverse.
        let queda_trabajo = self.tasks.iter().any(|(id, t)| {
            *id != task_id
                && !Self::terminal(t.vista.state)
                && t.afectados.iter().any(|d| afectados.contains(d))
        });
        if queda_trabajo {
            return Vec::new();
        }
        // Consumido: ni esta ni las hermanas ya terminadas vuelven a pedirlo.
        for t in self.tasks.values_mut() {
            if t.afectados.iter().any(|d| afectados.contains(d)) {
                t.afectados.clear();
            }
        }
        let huecos: Vec<(u32, bool)> = self
            .huecos
            .iter()
            .filter(|(_, h)| {
                afectados.contains(h.dir_pedido.as_ref().unwrap_or_else(|| h.pane.dir()))
            })
            .map(|(id, _)| (*id, self.oculto(*id)))
            .collect();
        let mut cambios = Vec::new();
        for (slot, oculto) in huecos {
            if oculto {
                // Un hueco que no se ve no pide listados —lo que no se ve no
                // se trae—, pero tampoco puede quedarse creyendo que su
                // listado sigue siendo verdad: se marca CARGANDO, que es lo
                // que `despertar_visibles` recoge en cuanto vuelva a la
                // pantalla. Sin esto, una pestaña de atrás sobre el
                // directorio de destino enseñaba un listado anterior a la
                // copia hasta que alguien navegara a mano.
                if let Some(h) = self.huecos.get_mut(&slot) {
                    h.estado = SlotState::Loading;
                }
                cambios.push(ViewChange::SlotState {
                    slot_id: slot,
                    state: SlotState::Loading,
                });
                continue;
            }
            cambios.extend(self.refrescar(slot, backend, buzon));
        }
        cambios
    }

    /// Vuelve a pedir el listado de UN hueco, en su MISMO directorio.
    ///
    /// No es una navegación: no toca el rastro ni el foco. Lo que sí hace es
    /// conservar lo que el lector tenía puesto, y las dos cosas son por
    /// IDENTIDAD y no por índice:
    ///
    /// - el CURSOR se ancla con `set_pending_focus`, o sea por ruta. La
    ///   memoria por directorio guarda un índice, y un índice no sobrevive a
    ///   que la operación quite o añada una entrada: quien miraba `e` se
    ///   encontraba el cursor en otro fichero, sin haber tocado una tecla, y
    ///   la siguiente tecla podía ser F8.
    /// - las MARCAS se vuelven a poner por ruta con `restore_marks`
    ///   (`set_listing` las limpia, que es lo correcto para un `cd`). Lo que
    ///   la operación se llevó no se vuelve a marcar y no se inventa nada.
    ///
    /// Con algo EN VUELO no hace nada. Reservaría un testigo nuevo, así que
    /// la respuesta de esa navegación llegaría con uno viejo y se tiraría: el
    /// panel se quedaría en el directorio del que el lector acababa de salir,
    /// sin decir nada. Perder un refresco es una pantalla un poco vieja;
    /// perder una navegación es la aplicación moviéndose sola. Y no hay nada
    /// que perder: el listado que va a aterrizar es más nuevo que la
    /// mutación, o va a otro sitio.
    pub(super) fn refrescar(
        &mut self,
        slot: u32,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<ViewChange> {
        self.token += 1;
        let token = RequestToken(self.token);
        let Some(hueco) = self.huecos.get_mut(&slot) else {
            return Vec::new();
        };
        if hueco.en_vuelo.is_some() {
            return Vec::new();
        }
        if let Some(sel) = hueco.pane.selected().map(|e| e.path.clone()) {
            hueco.pane.set_pending_focus(sel);
        }
        hueco.pane.remember_cursor();
        // `marked_paths` cae al cursor cuando no hay marcas, y restaurar ESO
        // convertiría un refresco en una marca que el lector no hizo.
        hueco.marcas_a_restaurar = if hueco.pane.marks_len() > 0 {
            hueco.pane.marked_paths()
        } else {
            Vec::new()
        };
        let dir = hueco.pane.dir().clone();
        hueco.estado = SlotState::Loading;
        hueco.en_vuelo = Some(token);
        hueco.drenando = Some(token);
        self.pedir_listado(slot, &dir, token, backend, buzon);
        vec![ViewChange::SlotState {
            slot_id: slot,
            state: SlotState::Loading,
        }]
    }

    /// Vuelve a pedir el listado de TODOS los huecos que se ven.
    ///
    /// De todos y no solo del enfocado, que es lo que hace el TUI y por el
    /// mismo motivo: lo que cambia un listado por debajo es un cambio EN EL
    /// DISCO, y un cambio en el disco no respeta el foco. Los ocultos se
    /// quedan fuera —lo que no se ve no se trae—; ya los despierta
    /// `despertar_visibles` cuando el reparto los saca a la luz.
    pub(super) fn refrescar_visibles(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let slots: Vec<u32> = self
            .huecos
            .keys()
            .copied()
            .filter(|id| !self.oculto(*id))
            .collect();
        let mut cambios = Vec::new();
        for slot in slots {
            cambios.extend(self.refrescar(slot, backend, buzon));
        }
        if cambios.is_empty() {
            // Todos tenían algo en vuelo: lo que va a aterrizar es más nuevo
            // que esta tecla, así que no hay nada que decir ni que pintar.
            return (self.aplicada(), Vec::new());
        }
        (self.aplicada(), vec![self.parche(cambios)])
    }

    /// Aparta —o devuelve— las entradas ocultas del panel activo (#107).
    ///
    /// Presentación-solo: el provider no vuelve a listar, las entradas
    /// apartadas siguen en el modelo. Y se ANUNCIA, porque un listado que
    /// encoge sin decir por qué se lee como un fallo del panel.
    pub(super) fn alternar_ocultos(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let (visibles, podadas) = {
            let hueco = self.hueco_mut();
            let visibles = hueco.pane.toggle_hidden();
            (visibles, hueco.pane.pruned_marks())
        };
        let clave = if visibles {
            "msg-hidden-shown"
        } else {
            "msg-hidden-hidden"
        };
        let mut frase = norte_i18n::t_in(self.lang, clave);
        if podadas > 0 {
            // Apartar las ocultas PODA las marcas de las que se van. El
            // contrato de `PaneState::pruned_marks` es que eso jamás es
            // silencioso: callarlo mandaría la siguiente op en masa sobre
            // menos ficheros de los que el lector marcó, creyendo él que van
            // todos.
            frase.push_str(", ");
            frase.push_str(&norte_i18n::ta_in(
                self.lang,
                "status-marks-pruned",
                &[("n", &podadas.to_string())],
            ));
        }
        self.status.message = Some(clamp_display(frase));
        let filas = self.parche_filas();
        let cambio = ViewChange::Status(self.status.clone());
        (self.aplicada(), vec![filas, self.parche(vec![cambio])])
    }

    /// Cicla la reinterpretación de los nombres que no son UTF-8 (#57).
    ///
    /// Display-only (regla 1): lo que cambia es cómo se PINTAN los bytes, no
    /// los bytes. Por eso las claves de fila siguen valiendo y solo viajan
    /// las filas visibles.
    pub(super) fn ciclar_encoding(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let etiqueta = self.hueco_mut().pane.cycle_name_encoding();
        let frase = match etiqueta {
            Some(enc) => norte_i18n::ta_in(self.lang, "msg-names-encoding", &[("enc", enc)]),
            None => norte_i18n::t_in(self.lang, "msg-names-encoding-off"),
        };
        self.status.message = Some(clamp_display(frase));
        let filas = self.parche_filas();
        let cambio = ViewChange::Status(self.status.clone());
        (self.aplicada(), vec![filas, self.parche(vec![cambio])])
    }
}
