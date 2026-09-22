//! La línea de tiempo del journal, en la ventana (fase 7, #359).
//!
//! El MODELO —qué es una fila, que un lote es una, qué `seq` manda el corte y
//! cuánto se va a llevar— es el compartido (`norte_frontend::timeline`), el
//! mismo que usa la TUI. Lo de aquí es el cableado: pedir las páginas, andar
//! por las filas y preguntar antes de deshacer.
//!
//! El molde es el del mapa de disco: un estado por hueco, una petición viva
//! con su testigo, y una respuesta que llega con otro testigo se tira.

#[allow(clippy::wildcard_imports)]
use super::*;

/// El kind que ocupa un hueco de línea de tiempo.
pub(super) const KIND: &str = "timeline";

/// Cuántas filas se piden por página: las mismas que la TUI. Muy por debajo
/// del tope del protocolo, porque esto es una pantalla que se lee y lo que
/// no quepa se pide al llegar abajo.
const POR_PAGINA: u32 = 50;

/// Lo que un hueco de línea de tiempo tiene y lo que está pidiendo.
#[derive(Default)]
pub(super) struct EstadoLinea {
    /// Las filas y el cursor. El estado COMPARTIDO.
    modelo: norte_frontend::timeline::Timeline,
    /// La petición en vuelo: su testigo y desde dónde se pidió.
    en_vuelo: Option<(RequestToken, Option<i64>)>,
    /// Ya se pidió la primera página (contestara lo que contestara). Sin esto
    /// un journal que no se deja leer se repide tras cada mensaje del actor.
    pedida: bool,
    /// Por qué no hay historial que enseñar, ya traducido. Solo lo pone la
    /// PRIMERA página: sin ella no hay nada que pintar y el motivo va en su
    /// lugar.
    motivo: Option<String>,
    /// El fallo de una página POSTERIOR, ya traducido. Va al pie —las filas
    /// que sí llegaron siguen ahí, así que el hueco de «vacío» no se pinta— y
    /// no para la paginación para siempre: se reintenta al volver a bajar.
    error_pagina: Option<String>,
    /// El cursor se ha movido desde ese fallo: se puede volver a pedir.
    reintentar: bool,
    /// El daemon dejó de avanzar —una página vacía, o un cursor que no
    /// retrocede—. Se trata como el final: uno honesto nunca lo hace, y uno
    /// que lo hiciera provocaría una petición tras cada mensaje del actor.
    agotada: bool,
    /// Tras una recarga, a qué fila volver: su `seq`. Sin esto, cada Task que
    /// terminara devolvería el cursor arriba mientras alguien lo mira.
    volver_a: Option<i64>,
}

impl Estado {
    /// Pide lo que les falte a las líneas de tiempo colocadas: la primera
    /// página cuando aparece su hueco, y la siguiente cuando el cursor llega a
    /// la última fila cargada.
    ///
    /// Se llama tras CADA mensaje del actor, así que lo primero es salir
    /// barato: sin ningún hueco de línea de tiempo no hay nada que recorrer.
    ///
    /// Un hueco nuevo empieza de cero, y por eso cerrar y volver a abrir el
    /// panel RELEE el historial: entre medias ha podido pasar cualquier cosa
    /// —lo normal es hacer cosas con el panel cerrado—, y uno que enseña el de
    /// hace un rato es peor que uno vacío.
    pub(super) fn sondear_lineas(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        let huecos: Vec<u32> = self
            .reparto
            .placements
            .iter()
            .filter(|(slot, _)| kind_de(&self.arbol, *slot).is_some_and(|k| k.as_str() == KIND))
            .map(|(SlotId(id), _)| *id)
            .collect();
        if huecos.is_empty() && self.lineas.is_empty() {
            return;
        }
        // Un `SlotId` se reutiliza: sin podar, un hueco nuevo heredaría el
        // historial de otro.
        self.lineas.retain(|id, _| huecos.contains(id));
        for id in huecos {
            let est = self.lineas.entry(id).or_default();
            if est.en_vuelo.is_some() || est.motivo.is_some() {
                continue;
            }
            let desde = if est.pedida {
                // Llegar abajo pide la siguiente página, y es el único momento
                // en que se pide más: cargar el journal entero al abrir traería
                // meses de historial para enseñar diez filas.
                let abajo = !est.modelo.is_empty() && est.modelo.cursor() + 1 >= est.modelo.len();
                // Y no más allá de lo que el puente deja cruzar: una fila
                // cargada que no se manda es un cursor sobre algo invisible.
                let cabe = est.modelo.len() < crate::bridge::MAX_ROWS_PER_BATCH;
                let puede = est.error_pagina.is_none() || est.reintentar;
                match est.modelo.next_before_seq() {
                    Some(s) if abajo && cabe && puede && !est.agotada => Some(s),
                    _ => continue,
                }
            } else {
                None
            };
            self.token += 1;
            let token = RequestToken(self.token);
            if let Some(e) = self.lineas.get_mut(&id) {
                e.en_vuelo = Some((token, desde));
            }
            let backend = Arc::clone(backend);
            let buzon = buzon.clone();
            tokio::spawn(async move {
                let res = match tokio::time::timeout(
                    PLAZO_PLUGINS,
                    backend.journal_list(desde, POR_PAGINA),
                )
                .await
                {
                    Ok(r) => r,
                    Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
                };
                let _ = buzon
                    .send(Mensaje::Fondo(Box::new(Fondo::PaginaDeLinea(
                        id, token, desde, res,
                    ))))
                    .await;
            });
        }
    }

    /// Aterriza una página: se usa si el testigo es el de la última petición
    /// de ESE hueco, y se tira si no.
    pub(super) fn aterrizar_pagina(
        &mut self,
        slot: u32,
        token: RequestToken,
        desde: Option<i64>,
        res: Result<norte_proto::methods::JournalListResult, Error>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let lang = self.lang;
        let est = self.lineas.get_mut(&slot)?;
        if est.en_vuelo.map(|(t, _)| t) != Some(token) {
            return None;
        }
        est.en_vuelo = None;
        est.pedida = true;
        match (res, desde) {
            (Ok(pagina), None) => {
                est.modelo =
                    norte_frontend::timeline::Timeline::new(&pagina.rows, pagina.next_before_seq);
                // Una recarga vuelve a la fila que tenía el cursor, si sigue.
                if let Some(seq) = est.volver_a.take()
                    && let Some(i) = est.modelo.rows().iter().position(|r| r.seq == seq)
                {
                    est.modelo.set_cursor(i);
                }
            }
            (Ok(pagina), Some(d)) => {
                if pagina.rows.is_empty() || pagina.next_before_seq.is_some_and(|n| n >= d) {
                    est.agotada = true;
                }
                est.modelo.extend(&pagina.rows, pagina.next_before_seq);
                est.error_pagina = None;
            }
            // Un daemon sin journal, o que no conoce el método: aquí no hay
            // historial que enseñar, y se DICE.
            (Err(Error::Unsupported), None) => {
                est.motivo = Some(norte_i18n::t_in(lang, "timeline-unavailable"));
            }
            (Err(e), None) => {
                est.motivo = Some(clamp_display(norte_frontend::error::error_category_in(
                    lang, &e,
                )));
            }
            (Err(e), Some(_)) => {
                est.error_pagina = Some(clamp_display(norte_frontend::error::error_category_in(
                    lang, &e,
                )));
                est.reintentar = false;
            }
        }
        let snap = self.snapshot();
        Some(self.sobre(UiUpdate::Snapshot(Box::new(snap))))
    }

    /// Vuelve a pedir la primera página de las líneas abiertas, conservando la
    /// fila del cursor.
    ///
    /// Se llama cuando termina una Task: con el panel ABIERTO —que es lo
    /// normal en un hueco lateral— lo que se acaba de hacer, o de deshacer,
    /// tiene que aparecer. El techo (`upto_seq`) ya impide que un undo pase de
    /// lo contado; esto es para que lo contado sea lo de ahora.
    pub(super) fn recargar_lineas(&mut self) {
        for est in self.lineas.values_mut() {
            if !est.pedida || est.en_vuelo.is_some() {
                continue;
            }
            est.volver_a = est.modelo.selected().map(|r| r.seq);
            est.pedida = false;
            est.agotada = false;
            est.error_pagina = None;
            est.motivo = None;
        }
    }

    /// El hueco de línea de tiempo con el foco, si lo tiene uno.
    fn linea_enfocada(&self) -> Option<u32> {
        let SlotId(id) = self.roles.get(RoleId::Active)?;
        kind_de(&self.arbol, SlotId(id))
            .is_some_and(|k| k.as_str() == KIND)
            .then_some(id)
    }

    /// Si la línea de tiempo tiene el foco (y por tanto `Enter` es suyo).
    pub(super) fn linea_tiene_el_foco(&self) -> bool {
        self.linea_enfocada().is_some()
    }

    /// El movimiento, con la línea de tiempo enfocada. Solo los TRES efectos
    /// de movimiento son suyos; lo demás —el tabulador con el que se sale,
    /// sobre todo— sigue su camino.
    pub(super) fn efecto_en_linea(
        &mut self,
        efecto: Efecto,
    ) -> Option<(ActionAck, Vec<BridgeEnvelope<UiUpdate>>)> {
        if !matches!(
            efecto,
            Efecto::Cursor(_) | Efecto::Pagina(_) | Efecto::Extremo { .. }
        ) {
            return None;
        }
        let id = self.linea_enfocada()?;
        let est = self.lineas.get_mut(&id)?;
        // Hasta la última fila que CRUZA el puente: más allá, el cursor
        // señalaría una fila que el renderer no tiene.
        let filas = est.modelo.len().min(crate::bridge::MAX_ROWS_PER_BATCH);
        // Moverse es lo que autoriza a volver a pedir una página que falló.
        est.reintentar = true;
        if filas == 0 {
            return Some((self.aplicada(), Vec::new()));
        }
        let total = i64::try_from(filas).unwrap_or(i64::MAX);
        let actual = i64::try_from(est.modelo.cursor()).unwrap_or(0);
        let destino = match efecto {
            Efecto::Cursor(n) => actual.saturating_add(n.clamp(-total, total)),
            // Una página son diez filas, como la lista de la TUI cuando no
            // sabe cuánto mide: saltar más de lo que hay no significa nada.
            Efecto::Pagina(n) => actual.saturating_add(n.clamp(-total, total).saturating_mul(10)),
            Efecto::Extremo { al_final: false } => 0,
            Efecto::Extremo { al_final: true } => total - 1,
            _ => return None,
        };
        est.modelo
            .set_cursor(usize::try_from(destino.max(0)).unwrap_or(0));
        let snap = self.snapshot();
        Some((
            self.aplicada(),
            vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))],
        ))
    }

    /// `Enter` en la línea de tiempo: pregunta antes de deshacer hasta la fila
    /// del cursor, con el RECUENTO.
    ///
    /// Un corte que no se lleva nada NO abre un diálogo: preguntar «¿seguro?»
    /// por algo que no va a pasar enseña a decir que sí sin leer.
    pub(super) fn preguntar_deshacer_hasta(
        &mut self,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(id) = self.linea_enfocada() else {
            return (self.aplicada(), Vec::new());
        };
        let Some(est) = self.lineas.get(&id) else {
            return (self.aplicada(), Vec::new());
        };
        let (Some(seq), resumen) = (est.modelo.corte(), est.modelo.resumen()) else {
            return (self.aplicada(), Vec::new());
        };
        // El techo se congela AHORA, con el recuento que se va a enseñar: el
        // undo no pasa de lo que esta pregunta contó.
        let techo = est.modelo.techo();
        if resumen.no_hace_nada() {
            return (self.aplicada(), self.decir("timeline-undo-nothing"));
        }
        if self.efectos == crate::commands::Efectos::SoloLectura {
            return Self::no_muta();
        }
        let linea = |texto: String| crate::dto::DialogLine {
            text: clamp_display(texto),
            hostile: false,
        };
        // Los tres números en líneas distintas porque significan cosas
        // distintas y no se suman. Lo que se salta y lo ajeno sólo si lo hay.
        let mut body = vec![
            linea(norte_i18n::t_in(self.lang, "timeline-undo-body")),
            linea(norte_i18n::ta_in(
                self.lang,
                "timeline-undo-count",
                &[("n", &resumen.a_deshacer.to_string())],
            )),
        ];
        if resumen.irreversibles > 0 {
            body.push(linea(norte_i18n::ta_in(
                self.lang,
                "timeline-undo-skipped",
                &[("n", &resumen.irreversibles.to_string())],
            )));
        }
        if resumen.ajenas > 0 {
            body.push(linea(norte_i18n::ta_in(
                self.lang,
                "timeline-undo-foreign",
                &[("n", &resumen.ajenas.to_string())],
            )));
        }
        let modal = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id: modal,
            title_key: "timeline-undo-title".to_owned(),
            destination: None,
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body,
            overflow_note: String::new(),
            overflow_hostile: false,
            choices: vec![
                DialogChoice {
                    id: "confirm".to_owned(),
                    label_key: "dialog-confirm".to_owned(),
                    // Deshacer ESCRIBE: mueve ficheros de vuelta y borra lo
                    // que se creó.
                    destructive: true,
                },
                DialogChoice {
                    id: "cancel".to_owned(),
                    label_key: "dialog-cancel".to_owned(),
                    destructive: false,
                },
            ],
            input: None,
            input_hostile: false,
            input_secret: false,
            fields: Vec::new(),
            dest_check: crate::dto::DestCheckView::NotAsked,
        };
        self.dialogos.push(Dialogo {
            id: modal,
            vista,
            tecleado: Tecleado::Texto(String::new()),
            reconocido: true,
            al_confirmar: Some(Pendiente::DeshacerHasta { seq, techo }),
        });
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// El humano leyó el recuento y dijo que sí: corre como Task de undo, con
    /// el progreso y la cancelación de siempre. Lo que pasó lo cuenta su
    /// informe, que esta ventana ya enseña (`informe_de_undo`).
    pub(super) fn deshacer_hasta(
        &mut self,
        seq: i64,
        techo: Option<i64>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // Sin alcance conocido, como el de una sesión: se relista lo que está
        // en pantalla.
        let visibles = self.dirs_visibles();
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            match backend.undo_after(seq, techo).await {
                Ok(task) => {
                    let _ = buzon
                        .send(Mensaje::TaskNueva(Box::new((task, visibles, None))))
                        .await;
                }
                Err(e) => {
                    let _ = buzon.send(Mensaje::TaskFallida(Box::new(e))).await;
                }
            }
        });
        self.decir("msg-timeline-undo-running")
    }

    /// La proyección de una línea de tiempo.
    pub(super) fn vista_de_linea(&self, id: u32) -> crate::dto::TimelineSlotView {
        let est = self.lineas.get(&id);
        let rows: Vec<crate::dto::TimelineRowView> = est
            .map(|e| {
                e.modelo
                    .rows()
                    .iter()
                    .take(crate::bridge::MAX_ROWS_PER_BATCH)
                    .map(|f| {
                        let mut cola = Vec::new();
                        if f.members > 1 {
                            cola.push(norte_i18n::ta_in(
                                self.lang,
                                "timeline-batch",
                                &[("n", &f.members.to_string())],
                            ));
                        }
                        if !f.reversible {
                            cola.push(norte_i18n::t_in(self.lang, "timeline-irreversible"));
                        }
                        crate::dto::TimelineRowView {
                            time: norte_frontend::format::hora_utc(f.ts_ms),
                            actor: clamp_display(f.actor_kind.clone()),
                            op: clamp_display(f.op.clone()),
                            path: clamp_display(norte_frontend::timeline::path_label(&f.path)),
                            hostile: f.hostile,
                            tail: cola.join(" · "),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let empty = match est {
            Some(e) if e.motivo.is_some() => e.motivo.clone().unwrap_or_default(),
            // «Todavía no se ha hecho nada» sólo cuando SE HA MIRADO.
            Some(e) if e.modelo.cargada() => norte_i18n::t_in(self.lang, "timeline-empty"),
            _ => norte_i18n::t_in(self.lang, "timeline-loading"),
        };
        let solo_lectura = self.efectos == crate::commands::Efectos::SoloLectura;
        let footer = est
            .filter(|e| !e.modelo.is_empty())
            .map(|e| {
                let c = e.modelo.resumen();
                // Una página que no llegó se dice aquí: las filas que sí
                // llegaron ocupan el hueco, así que el motivo no tiene otro
                // sitio donde verse.
                if let Some(error) = &e.error_pagina {
                    error.clone()
                } else if solo_lectura {
                    // Un pie que promete deshacer en una ventana que no lo va
                    // a hacer enseña a no fiarse del pie.
                    norte_i18n::t_in(self.lang, "host-read-only")
                } else if c.no_hace_nada() {
                    norte_i18n::t_in(self.lang, "timeline-undo-nothing")
                } else {
                    norte_i18n::ta_in(
                        self.lang,
                        "timeline-undo-count",
                        &[("n", &c.a_deshacer.to_string())],
                    )
                }
            })
            .unwrap_or_default();
        crate::dto::TimelineSlotView {
            slot_id: id,
            title: norte_i18n::t_in(self.lang, "timeline-title"),
            cursor: est
                .filter(|e| !e.modelo.is_empty())
                .map(|e| e.modelo.cursor() as u64),
            rows,
            empty,
            footer,
        }
    }
}
