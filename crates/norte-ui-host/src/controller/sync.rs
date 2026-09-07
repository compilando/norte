//! Comparar dos directorios y sincronizarlos.
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
    /// El desenlace de la Task de una comparación entra en el modelo.
    ///
    /// Lo traduce `finish_from_task`, que es donde vive la diferencia que
    /// importa: «terminó» no es lo mismo que «terminó y llegó todo». Una
    /// comparación a la que se le perdieron lotes se lee INCOMPLETA, y una
    /// cuyo canal se cerró sin desenlace observado se lee DESCONOCIDA — dos
    /// estados que el CLI y el MCP ya perdieron cada uno por su cuenta.
    /// La Task del APPLY terminó: se pide su informe.
    ///
    /// El desenlace de la Task dice si corrió; lo que se hizo y lo que NO lo
    /// cuenta el informe, y sin él «terminó» se lee como «salió bien» sobre
    /// un destino que puede haber quedado a medias.
    pub(super) fn pedir_informe_de_sync(
        &mut self,
        p: &norte_proto::TaskProgress,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        let Some(sinc) = self.sincronizacion.as_ref() else {
            return;
        };
        // Los MISMOS tres guards que el informe de un lote, y por los mismos
        // motivos: la CLASE (un `fs.copy` cualquiera puede llevar el mismo id
        // tras un relevo), la ÉPOCA de conexión (los ids del daemon nuevo
        // empiezan otra vez en 1) y la idempotencia (una reconexión reanuncia
        // el terminal, y esto es una RPC).
        if sinc.task != p.task_id
            || sinc.epoca_conexion != self.epoca_conexion
            || !matches!(p.kind, norte_proto::TaskKind::Sync)
            || sinc.informe_pedido
            || !matches!(
                sinc.vista.state,
                norte_frontend::sync::SyncState::Applying(_)
            )
        {
            return;
        }
        let epoca = sinc.epoca;
        let estado = p.state.clone();
        let id = p.task_id;
        if let Some(s) = self.sincronizacion.as_mut() {
            s.informe_pedido = true;
        }
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let informe = backend.sync_report(id).await;
            let _ = buzon
                .send(Mensaje::Fondo(Box::new(Fondo::InformeDeSync(
                    epoca,
                    estado,
                    Box::new(informe),
                ))))
                .await;
        });
    }

    /// El informe llegó: entra en el modelo, que decide qué frase sale.
    pub(super) fn informe_de_sync(
        &mut self,
        epoca: u64,
        estado: &norte_proto::TaskState,
        informe: Result<norte_proto::methods::SyncReportResult, Error>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let lang = self.lang;
        let Some(sinc) = self.sincronizacion.as_mut().filter(|s| s.epoca == epoca) else {
            return Vec::new();
        };
        // `on_apply_ended` es quien sabe leer el par (desenlace, informe): un
        // apply cancelado CON informe dice las dos mitades —«cancelado tras
        // aplicar N»— y uno sin informe deja que mande el error, porque no
        // hay recuento que lo pueda sustituir.
        // La categoría del error vuelve YA localizada en el idioma de esta
        // ventana, porque el modelo lo recibe como parámetro: leerlo del
        // global habría puesto el desenlace de una escritura en el idioma de
        // otra ventana.
        if let Some(categoria) = sinc.vista.on_apply_ended(estado, informe, lang) {
            sinc.vista.error = Some(clamp_display(categoria));
        }
        let cambio = ViewChange::Sync {
            sync: self.vista_sincronizacion(),
        };
        vec![self.parche(vec![cambio])]
    }

    /// El desenlace de la Task de un PLAN entra en el modelo.
    ///
    /// Sin esto, `run` se quedaba en `Running` para siempre y con él moría la
    /// cláusula que el modelo compartido documenta como su motivo de existir:
    /// un plan CANCELADO o FALLIDO no se aprueba aunque haya cerrado. El
    /// `sync.plan_done` puede ir ya en el canal cuando el lector pulsa
    /// `Escape`, así que sin el desenlace la pantalla ofrecía aprobar un plan
    /// que acababan de mandar parar — y la fase B cuelga de ese campo el
    /// botón que escribe.
    ///
    /// Y por el progreso, no por el cierre del canal: una Task que muere sin
    /// cerrar su stream dejaba el panel en «planificando…» para siempre.
    pub(super) fn cerrar_sincronizacion(
        &mut self,
        p: &norte_proto::TaskProgress,
    ) -> Vec<ViewChange> {
        let lang = self.lang;
        let Some(sinc) = self.sincronizacion.as_mut() else {
            return Vec::new();
        };
        if sinc.task != p.task_id {
            return Vec::new();
        }
        sinc.vista.run = norte_frontend::sync::SyncRunState::from_task_state(&p.state);
        if let norte_proto::TaskState::Failed { error } = &p.state {
            // La CATEGORÍA localizada, jamás el `Display` inglés: esto se
            // pinta de forma persistente y varias variantes interpolan datos
            // del otro extremo.
            sinc.vista.error = Some(clamp_display(norte_frontend::error::error_category_in(
                lang, error,
            )));
        }
        vec![ViewChange::Sync {
            sync: self.vista_sincronizacion(),
        }]
    }

    pub(super) fn cerrar_comparacion(&mut self, p: &norte_proto::TaskProgress) -> Vec<ViewChange> {
        let lang = self.lang;
        let Some(c) = self.comparacion.as_mut() else {
            return Vec::new();
        };
        if c.task != p.task_id {
            return Vec::new();
        }
        let recibidas = c.vista.pane.len() as u64;
        c.vista
            .finish_from_task(&p.state, p.entries_done, recibidas, lang);
        vec![ViewChange::Compare {
            compare: self.vista_comparacion(),
        }]
    }

    /// Las teclas mientras el panel de diferencias está abierto.
    /// Las teclas mientras el panel de diferencias está abierto.
    ///
    /// `Escape` DOS veces y no una: la primera pide cancelar la Task, la
    /// segunda cierra pase lo que pase. Sin la segunda, cerrar dependía de
    /// que el canal de filas se cerrara de verdad, y hay formas de que no lo
    /// haga —un daemon muerto, un provider colgado de un NFS— que dejaban al
    /// lector atrapado en la única pantalla de norte sin salida.
    /// Las teclas mientras el panel de sincronización está abierto.
    ///
    /// `Escape` DOS veces, por lo mismo que en el panel de diferencias: la
    /// primera pide cancelar la Task viva —la del plan, o la del apply si ya
    /// está escribiendo—, la segunda cierra pase lo que pase.
    /// `dialog.approve` aprueba, y cuando el plan borra o deja algo sin vuelta
    /// atrás contesta también la SEGUNDA pregunta: es la última pantalla donde
    /// todavía se puede decir que no.
    #[expect(
        clippy::too_many_lines,
        reason = "despachador de una pantalla con dos regímenes de tecla"
    )]
    pub(super) fn tecla_en_sincronizacion(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(sinc) = self.sincronizacion.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        // Con la SEGUNDA pregunta delante, las teclas son suyas: solo `y`
        // contesta que sí, y cualquier otra cosa la retira. Una pregunta que
        // se puede contestar con cualquier tecla no es una pregunta.
        if sinc.vista.confirming.is_some() {
            let si = self
                .verbo_de_dialogo(k)
                .is_some_and(|v| v == "dialog.approve");
            let Some(sinc) = self.sincronizacion.as_mut() else {
                return (Self::obsoleta(StaleAction::Modal), Vec::new());
            };
            sinc.vista.confirming = None;
            if si {
                return self.aplicar_plan(backend, buzon);
            }
            let cambio = ViewChange::Sync {
                sync: self.vista_sincronizacion(),
            };
            return (self.aplicada(), vec![self.parche(vec![cambio])]);
        }
        // `Home`/`End` se quedan fijas: el catálogo compartido no tiene verbo
        // para «al principio» dentro de un diálogo.
        let verbo = match k.key.as_str() {
            "Home" | "home" | "End" | "end" => None,
            _ => self.verbo_de_dialogo(k),
        };
        let Some(sinc) = self.sincronizacion.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        match (verbo.as_deref(), k.key.as_str()) {
            // Aprobar el plan es `dialog.approve`, no `dialog.confirm`: lo
            // que se contesta aquí es «sí, escribe» sobre un plan que ya está
            // delante, que es exactamente lo que ese verbo nombra — y es el
            // mismo con el que se contesta la SEGUNDA pregunta.
            (Some("dialog.approve"), _) => self.pedir_aprobacion(backend, buzon),
            (Some("dialog.cancel"), _) => {
                // Mientras el daemon ESCRIBE, `Escape` pide cancelar y no
                // cierra: cerrar pierde el informe —y con él el recuento, los
                // fallos y el asa del deshacer— sobre un destino que se
                // reescribió a medias.
                let escribiendo = sinc.vista.is_submitted()
                    || matches!(
                        sinc.vista.state,
                        norte_frontend::sync::SyncState::Applying(_)
                    );
                if escribiendo {
                    // La PRIMERA vez pide parar y no cierra: cerrar pierde el
                    // informe sobre un destino a medio reescribir.
                    //
                    // La segunda SÍ cierra, y eso no contradice lo anterior:
                    // «espera al informe» vale mientras el informe pueda
                    // llegar, y hay formas de que no llegue nunca —un daemon
                    // muerto, un `sync.report` que falla, una Task cuyo canal
                    // se cae sin desenlace—. Sin esta salida, esta pantalla
                    // —la que ESCRIBE— era la única de norte sin salida.
                    if !sinc.vista.cancel_requested {
                        sinc.vista.cancel_requested = true;
                        let task = sinc.task;
                        if task.get() != 0 {
                            self.cancelar(task.get());
                        }
                        let cambio = ViewChange::Sync {
                            sync: self.vista_sincronizacion(),
                        };
                        return (self.aplicada(), vec![self.parche(vec![cambio])]);
                    }
                    let task = sinc.task;
                    self.sincronizacion = None;
                    if task.get() != 0 {
                        self.cancelar(task.get());
                    }
                    let mut fuera = vec![self.parche(vec![ViewChange::Sync { sync: None }])];
                    // Y se DICE lo que se pierde al cerrar: el destino puede
                    // haber quedado a medias y su informe ya no se va a ver.
                    fuera.extend(self.decir("msg-sync-closed-midway"));
                    return (self.aplicada(), fuera);
                }
                if sinc.vista.cancel_requested {
                    let task = sinc.task;
                    sinc.abandonada
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                    self.sincronizacion = None;
                    if task.get() != 0 {
                        self.cancelar(task.get());
                    }
                    return (
                        self.aplicada(),
                        vec![self.parche(vec![ViewChange::Sync { sync: None }])],
                    );
                }
                sinc.vista.cancel_requested = true;
                // Y el modelo se entera YA: si el `plan_done` viene de camino,
                // sin esto el panel pasaría a «listo para aprobar» un plan que
                // el lector acaba de mandar parar.
                //
                // Solo mientras algo CORRE. Sobre un plan ya aplicado, marcar
                // «cancelado» reescribía el desenlace a «cancelado tras
                // aplicar N; el resto no se aplicó» sobre una sincronización
                // que terminó entera: dos frases falsas sobre lo que hay en
                // disco, en la única pantalla que lo describe.
                if matches!(sinc.vista.run, norte_frontend::sync::SyncRunState::Running) {
                    sinc.vista.run = norte_frontend::sync::SyncRunState::Cancelled;
                }
                let task = sinc.task;
                if task.get() != 0 {
                    self.cancelar(task.get());
                }
                let cambio = ViewChange::Sync {
                    sync: self.vista_sincronizacion(),
                };
                (self.aplicada(), vec![self.parche(vec![cambio])])
            }
            (Some("dialog.down" | "dialog.up" | "dialog.page-down" | "dialog.page-up"), _)
            | (_, "Home" | "home" | "End" | "end") => {
                let total = sinc.vista.steps().len();
                if total == 0 {
                    return (self.aplicada(), Vec::new());
                }
                // El TOPE del desplazamiento es «lo que hay menos lo que
                // cabe», no «lo que hay menos uno»: con lo segundo, una sola
                // flecha sobre un plan de dos pasos y una ventana de
                // doscientos dejaba de mandar el primer paso.
                let tope = total.saturating_sub(sinc.ventana.max(1));
                let pagina = sinc.ventana.max(1);
                sinc.primera_visible = match (verbo.as_deref(), k.key.as_str()) {
                    (Some("dialog.down"), _) => sinc.primera_visible.saturating_add(1),
                    (Some("dialog.up"), _) => sinc.primera_visible.saturating_sub(1),
                    (Some("dialog.page-down"), _) => sinc.primera_visible.saturating_add(pagina),
                    (Some("dialog.page-up"), _) => sinc.primera_visible.saturating_sub(pagina),
                    (_, "Home" | "home") => 0,
                    _ => tope,
                }
                .min(tope);
                let cambio = ViewChange::Sync {
                    sync: self.vista_sincronizacion(),
                };
                (self.aplicada(), vec![self.parche(vec![cambio])])
            }
            // Lo que no entiende se COME: un panel que deja pasar teclas no
            // es una pantalla.
            _ => (self.aplicada(), Vec::new()),
        }
    }

    /// `a`: pide aprobar el plan. Puede que haya una SEGUNDA pregunta.
    ///
    /// La segunda no es ceremonia: la compone el modelo compartido con una
    /// rama por perspectiva de deshacer, y solo aparece cuando el plan borra
    /// árboles o deja algo sin vuelta atrás. Un plan que se deshace entero y
    /// no borra nada no la tiene — preguntar siempre es lo que enseña a
    /// contestar sin leer.
    pub(super) fn pedir_aprobacion(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let lang = self.lang;
        let Some(sinc) = self.sincronizacion.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        if !sinc.vista.can_approve() {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-sync-cannot-approve".to_owned(),
                },
                Vec::new(),
            );
        }
        let pregunta = sinc.vista.state.plan().and_then(|p| p.confirmation(lang));
        match pregunta {
            Some(c) => {
                sinc.vista.confirming = Some(c);
                let cambio = ViewChange::Sync {
                    sync: self.vista_sincronizacion(),
                };
                (self.aplicada(), vec![self.parche(vec![cambio])])
            }
            None => self.aplicar_plan(backend, buzon),
        }
    }

    /// Manda `sync.apply` con el hash que el CORE devolvió.
    ///
    /// Por `SyncView::submit`, que es la ÚNICA puerta: mira `can_approve` y
    /// echa el pestillo del apply en vuelo en el mismo gesto. Separarlos deja
    /// la ventana en la que un segundo `a` —o un `Escape`— entra entre que la
    /// petición sale y el daemon contesta, y esta ventana lee eventos entre
    /// teclas, así que es alcanzable de verdad.
    pub(super) fn aplicar_plan(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let epoca = self.sincronizacion.as_ref().map_or(0, |s| s.epoca);
        let Some(hash) = self.sincronizacion.as_mut().and_then(|s| s.vista.submit()) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-sync-cannot-approve".to_owned(),
                },
                Vec::new(),
            );
        };
        let backend2 = Arc::clone(backend);
        let buzon2 = buzon.clone();
        tokio::spawn(async move {
            let resultado = backend2.sync_apply(hash).await;
            match resultado {
                Ok(task) => {
                    let id = task.id;
                    let _ = buzon2
                        .send(Mensaje::TaskNueva(Box::new((task, Vec::new(), None))))
                        .await;
                    let _ = buzon2
                        .send(Mensaje::Fondo(Box::new(Fondo::SyncAplicando(epoca, id))))
                        .await;
                }
                Err(e) => {
                    // ¿Se SABE que no escribió? Solo si el daemon contestó que
                    // no. Un transporte muerto deja la petición en el aire.
                    let seguro = matches!(
                        e,
                        Error::PolicyDenied { .. }
                            | Error::Conflict { .. }
                            | Error::NotFound
                            | Error::PermissionDenied
                            | Error::InvalidPath
                            | Error::Unsupported
                            | Error::EncodingLoss
                    );
                    let _ = buzon2.send(Mensaje::TaskFallida(Box::new(e))).await;
                    // Y se suelta el pestillo —cuando toca—: sin esto la `a`
                    // queda muerta para siempre sobre un plan que nadie aplicó.
                    let _ = buzon2
                        .send(Mensaje::Fondo(Box::new(Fondo::SyncNoAplicado(
                            epoca, seguro,
                        ))))
                        .await;
                }
            }
        });
        let cambio = ViewChange::Sync {
            sync: self.vista_sincronizacion(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// El daemon aceptó el apply: el modelo pasa a APLICANDO.
    pub(super) fn sync_aplicando(
        &mut self,
        epoca: u64,
        task: norte_proto::TaskId,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Some(sinc) = self.sincronizacion.as_mut().filter(|s| s.epoca == epoca) else {
            return Vec::new();
        };
        // La Task que se sigue pasa a ser la del APPLY: es a la que apunta
        // ahora el `Escape`, y de la que hay que pedir el informe.
        sinc.task = task;
        sinc.epoca_conexion = self.epoca_conexion;
        if !sinc.vista.on_apply_started(task) {
            // El modelo la NIEGA —el lector pidió parar en la ventana en la
            // que el apply todavía no tenía id— y entonces cancelarla es
            // NUESTRO trabajo: nadie más conoce ese id, y el contrato del
            // modelo lo dice con todas las letras. Sin esto, el daemon seguía
            // reescribiendo el destino de un plan que el humano canceló.
            sinc.vista.on_apply_abandoned();
            let (_, mut fuera) = self.cancelar(task.get());
            fuera.extend(self.decir("msg-sync-cancelled-late"));
            fuera.push(self.parche(vec![ViewChange::Sync {
                sync: self.vista_sincronizacion(),
            }]));
            return fuera;
        }
        // Pudo nacer TERMINAL: el daemon la completó antes de contestar y su
        // progreso no dispara nunca. Es la misma carrera que el tablero ya
        // documenta, y aquí se traduce en un panel aplicando para siempre.
        let nacio = self
            .tasks
            .get(&task.get())
            .map(|t| t.progreso.borrow().clone());
        let mut fuera = vec![self.parche(vec![ViewChange::Sync {
            sync: self.vista_sincronizacion(),
        }])];
        if let Some(p) = nacio.filter(|p| p.state.is_terminal()) {
            self.pedir_informe_de_sync(&p, backend, buzon);
        }
        fuera.extend(Vec::new());
        fuera
    }

    pub(super) fn tecla_en_comparacion(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.comparacion.is_none() {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        }
        // Los DÍGITOS de los filtros no pasan por el resolutor: son
        // posicionales —el n-ésimo de `CATEGORIES`— y no hay cinco verbos que
        // los nombren. Es la misma decisión que en el TUI.
        let digito = k.key.len() == 1 && k.key.chars().all(|c| ('1'..='5').contains(&c));
        let verbo = if digito {
            None
        } else {
            self.verbo_de_dialogo(k)
        };
        let Some(c) = self.comparacion.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        match (verbo.as_deref(), k.key.as_str()) {
            (Some("dialog.cancel"), _) => {
                if c.vista.cancel_requested {
                    let task = c.task;
                    c.abandonada
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                    self.comparacion = None;
                    if task.get() != 0 {
                        self.cancelar(task.get());
                    }
                    return (
                        self.aplicada(),
                        vec![self.parche(vec![ViewChange::Compare { compare: None }])],
                    );
                }
                c.vista.cancel_requested = true;
                let task = c.task;
                if task.get() != 0 {
                    self.cancelar(task.get());
                }
                let cambio = ViewChange::Compare {
                    compare: self.vista_comparacion(),
                };
                (self.aplicada(), vec![self.parche(vec![cambio])])
            }
            (Some("dialog.pane"), _) => {
                // Cambiar de lado cambia a qué panel navega `Enter` y sobre
                // qué lado operan las teclas de fichero.
                c.vista.pane.swap_active_side();
                let cambio = ViewChange::Compare {
                    compare: self.vista_comparacion(),
                };
                (self.aplicada(), vec![self.parche(vec![cambio])])
            }
            (Some("dialog.confirm"), _) => {
                let Some(id) = c.vista.pane.selected_id() else {
                    return (self.aplicada(), Vec::new());
                };
                self.comparacion_activa(id, backend, buzon)
            }
            (Some(v @ ("dialog.up" | "dialog.down")), _) => {
                let abajo = v == "dialog.down";
                let visibles = c.vista.pane.visible_ids();
                if visibles.is_empty() {
                    return (self.aplicada(), Vec::new());
                }
                let actual = c
                    .vista
                    .pane
                    .selected_id()
                    .and_then(|id| visibles.iter().position(|v| *v == id))
                    .unwrap_or(0);
                let destino = if abajo {
                    (actual + 1).min(visibles.len() - 1)
                } else {
                    actual.saturating_sub(1)
                };
                let id = visibles[destino];
                c.vista.pane.select(id);
                let cambio = ViewChange::Compare {
                    compare: self.vista_comparacion(),
                };
                (self.aplicada(), vec![self.parche(vec![cambio])])
            }
            // 1..5: los filtros, en el orden fijo de las categorías, igual
            // que en el TUI.
            (_, d) if digito => {
                let i = d.chars().next().and_then(|c| c.to_digit(10)).unwrap_or(1) as usize - 1;
                let Some(cat) = norte_frontend::compare::CATEGORIES.get(i).copied() else {
                    return (self.aplicada(), Vec::new());
                };
                c.vista.pane.toggle_filter(cat);
                let cambio = ViewChange::Compare {
                    compare: self.vista_comparacion(),
                };
                (self.aplicada(), vec![self.parche(vec![cambio])])
            }
            // Una tecla que no entiende se COME igual: un panel que deja
            // pasar lo que no entiende no es una pantalla, es un adorno.
            _ => (self.aplicada(), Vec::new()),
        }
    }

    /// Elige una fila del panel de diferencias.
    /// Las cuatro acciones del panel de diferencias, en un brazo.
    ///
    /// Juntas y no cuatro brazos del reparto general: son la misma superficie
    /// y ninguna significa nada sin ella.
    pub(super) fn accion_de_comparacion(
        &mut self,
        accion: &UiAction,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match accion {
            UiAction::CompareSelectRow { id } => self.comparacion_selecciona(*id),
            UiAction::CompareActivateRow { id } => self.comparacion_activa(*id, backend, buzon),
            UiAction::CompareToggleFilter { category } => self.comparacion_filtra(category),
            UiAction::CompareSetVisibleRange { first, count } => {
                self.comparacion_ventana(*first, *count)
            }
            // El reparto general solo manda aquí esas cuatro.
            _ => (Self::obsoleta(StaleAction::Modal), Vec::new()),
        }
    }

    /// Elige una fila del panel de diferencias.
    pub(super) fn comparacion_selecciona(
        &mut self,
        id: u64,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(c) = self.comparacion.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        // `select` IGNORA un id que no llegó, que es lo correcto: la
        // alternativa es una selección que nombra una fila inexistente.
        c.vista.pane.select(id);
        if c.vista.pane.selected_id() != Some(id) {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        }
        let cambio = ViewChange::Compare {
            compare: self.vista_comparacion(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Enseña o esconde una categoría entera.
    pub(super) fn comparacion_filtra(
        &mut self,
        categoria: &str,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(c) = self.comparacion.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        let Some(cat) = norte_frontend::compare::CATEGORIES
            .iter()
            .find(|c| c.id() == categoria)
        else {
            // Una categoría que no existe es un renderer de otra versión, no
            // una orden.
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        };
        c.vista.pane.toggle_filter(*cat);
        let cambio = ViewChange::Compare {
            compare: self.vista_comparacion(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// El renderer dice qué ventana pinta.
    pub(super) fn comparacion_ventana(
        &mut self,
        primera: u64,
        cuantas: u32,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(c) = self.comparacion.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        c.primera_visible = usize::try_from(primera).unwrap_or(0);
        // Acotada: lo que el renderer diga que le cabe no puede hacer que un
        // parche lleve medio millón de filas.
        c.ventana = usize::try_from(cuantas)
            .unwrap_or(Self::VENTANA_COMPARACION)
            .clamp(1, MAX_ROWS_PER_BATCH);
        let cambio = ViewChange::Compare {
            compare: self.vista_comparacion(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Abre la fila elegida: navega al directorio del lado ACTIVO.
    ///
    /// A dónde ir lo decide el modelo COMPARTIDO (`navigation_target`): la
    /// fila si es un directorio, su padre si es un fichero, y `None` cuando
    /// ese lado está vacío —un huérfano mirado desde el lado que no lo
    /// tiene—, que NO cae al otro lado.
    pub(super) fn comparacion_activa(
        &mut self,
        id: u64,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(c) = self.comparacion.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        c.vista.pane.select(id);
        let Some(destino) = c.vista.pane.navigation_target() else {
            let lado = norte_frontend::compare::side_label(c.vista.pane.active_side(), self.lang);
            return (
                ActionAck::Unavailable {
                    reason_key: "compare-no-target".to_owned(),
                },
                self.decir_con("compare-no-target", &[("side", &lado)]),
            );
        };
        // El panel que navega es el del lado ACTIVO, no el que tenga el foco:
        // quien mira la derecha no puede perder su directorio de la izquierda
        // por pulsar `Enter`. Se ENFOCA ese hueco y se navega por el camino
        // de siempre, que es el que registra el rastro y pide el listado.
        if let Some(slot) = self.hueco_del_lado() {
            self.roles.set(RoleId::Active, SlotId(slot));
            self.reconcilia_roles();
        }
        let mut salidas = self.navegar(&destino, Trail::Record, backend, buzon);
        let cambio = ViewChange::Compare {
            compare: self.vista_comparacion(),
        };
        salidas.push(self.parche(vec![cambio]));
        (self.aplicada(), salidas)
    }

    /// El hueco que corresponde al lado ACTIVO de la comparación.
    pub(super) fn hueco_del_lado(&self) -> Option<u32> {
        let c = self.comparacion.as_ref()?;
        let izquierdo = u32::try_from(c.vista.left_pane).ok()?;
        match c.vista.pane.active_side() {
            norte_proto::methods::Side::Right => self.hueco_destino().ok(),
            _ => Some(izquierdo),
        }
    }

    /// Pide el PLAN de sincronizar el panel activo sobre el destino.
    ///
    /// El plan no escribe un byte: dice qué haría. Lo que escribe es
    /// `sync.apply`, y solo contra el hash que este plan cierre.
    pub(super) fn pedir_sincronizacion(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // UNA a la vez. Relanzar dejaba el panel anterior sin abandonar y su
        // Task sin cancelar —el daemon seguía caminando un árbol para un plan
        // que ya no se puede ver— y, con una petición en vuelo, la segunda
        // pulsación mataba el panel de las dos.
        if self.sincronizacion.is_some() || self.sync_pedida.is_some() {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-sync-already".to_owned(),
                },
                Vec::new(),
            );
        }
        let destino_slot = match self.hueco_destino() {
            Ok(d) => d,
            Err(reason_key) => {
                return (
                    ActionAck::Unavailable {
                        reason_key: reason_key.to_owned(),
                    },
                    Vec::new(),
                );
            }
        };
        // Qué árbol se sobrescribe lo decide la regla COMPARTIDA, no una
        // copia local: dos respuestas a «cuál de los dos se reescribe» es el
        // bug más barato de escribir y el más caro de encontrar, porque las
        // dos producen un plan perfectamente plausible.
        let enfocado = self.hueco().pane.dir().clone();
        let otro = self.huecos[&destino_slot].pane.dir().clone();
        // Con el panel de diferencias abierto manda el LADO ACTIVO; sin él,
        // el pane con foco es el origen. Las dos ramas viven en la regla
        // compartida, y aquí solo se le pasan los datos.
        let raices = norte_frontend::sync::sync_roots(
            self.comparacion.as_ref().map(|c| &c.vista),
            &norte_frontend::sync::Panes {
                focused_root: &enfocado,
                focused_encoding: None,
                other_root: &otro,
                other_encoding: None,
            },
        );
        let (origen, destino) = (raices.source.clone(), raices.dest.clone());
        if origen == destino {
            // Raíces solapadas: el daemon lo rechaza con `OverlappingRoots` y
            // no crea Task. Este atajo local es cortesía —la autoridad es el
            // core, que también caza el solape ANIDADO— pero abrir un panel
            // que va a morir es peor que decirlo antes.
            return (
                ActionAck::Unavailable {
                    reason_key: "host-same-directory".to_owned(),
                },
                Vec::new(),
            );
        }
        (
            self.aplicada(),
            self.lanzar_plan_de_sync(raices, backend, buzon),
        )
    }

    /// Encola `sync.plan` y engancha su canal de eventos al actor.
    pub(super) fn lanzar_plan_de_sync(
        &mut self,
        raices: norte_frontend::sync::SyncRoots,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let norte_frontend::sync::SyncRoots {
            source: origen,
            dest: destino,
            source_encoding: origen_encoding,
            dest_encoding: destino_encoding,
        } = raices;
        self.epoca_busqueda += 1;
        let epoca = self.epoca_busqueda;
        let abandonada = Arc::new(std::sync::atomic::AtomicBool::new(false));
        // `Update` y no `Mirror`: el modo que NO borra es el que puede ser el
        // de por defecto. Elegir espejo es una decisión que se toma a
        // propósito, y hasta que haya dónde tomarla no se ofrece.
        let modo = norte_proto::methods::SyncMode::Update;
        let params = norte_proto::methods::SyncPlanParams {
            source: origen.clone(),
            dest: destino.clone(),
            mode: modo,
            compare: norte_proto::methods::SyncCompareOptions::default(),
            on_unknown: norte_proto::methods::OnUnknown::default(),
            // Sin `include`: el árbol entero. Acotar el plan a una selección
            // es lo que hace el panel de diferencias con sus marcas, y eso
            // llega cuando esta ventana tenga esa vía.
            include: None,
        };
        let backend2 = Arc::clone(backend);
        let buzon2 = buzon.clone();
        let abandonada2 = Arc::clone(&abandonada);
        tokio::spawn(async move {
            let (task, mut rx) = match backend2.sync_plan(params).await {
                Ok(par) => par,
                Err(e) => {
                    // El fallo se DICE y además SUELTA la petición: sin lo
                    // segundo, un daemon que no sabe planificar —o unas
                    // raíces solapadas— dejaban `sync_pedida` puesta para
                    // siempre y el siguiente intento se rehusaba solo.
                    let _ = buzon2.send(Mensaje::TaskFallida(Box::new(e))).await;
                    let _ = buzon2
                        .send(Mensaje::Fondo(Box::new(Fondo::PlanDeSyncFallido(epoca))))
                        .await;
                    return;
                }
            };
            let id = task.id;
            let cancel = Arc::clone(&task.cancel);
            let _ = buzon2
                .send(Mensaje::TaskNueva(Box::new((task, Vec::new(), None))))
                .await;
            let _ = buzon2
                .send(Mensaje::Fondo(Box::new(Fondo::PlanDeSyncVivo(epoca, id))))
                .await;
            if abandonada2.load(std::sync::atomic::Ordering::SeqCst) {
                cancel();
                return;
            }
            while let Some(ev) = rx.recv().await {
                if abandonada2.load(std::sync::atomic::Ordering::SeqCst) {
                    cancel();
                    return;
                }
                if buzon2
                    .send(Mensaje::Fondo(Box::new(Fondo::EventoDeSync(
                        epoca,
                        Box::new(ev),
                    ))))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        });
        // El panel se abre cuando se SABE el id de la Task, y no antes: el
        // modelo compartido lo usa para descartar lo que venga de otro plan,
        // y con un id de relleno descartaba también los suyos —el panel se
        // quedaba en cero pasos y el plan cerraba «no se puede aprobar»—.
        self.sincronizacion = None;
        self.sync_pedida = Some(SyncPedida {
            epoca,
            abandonada,
            modo,
            origen,
            destino,
            origen_encoding,
            destino_encoding,
        });
        Vec::new()
    }

    /// Un evento del plan: un lote de pasos, o su cierre.
    /// El daemon aceptó el plan y dijo su Task: ahora se abre el panel.
    ///
    /// El modelo compartido nace CON el id porque es lo que usa para
    /// descartar lo que venga de otro plan; construirlo antes, con un id de
    /// relleno, hacía que descartara también sus propios lotes y el panel se
    /// quedaba en cero pasos y cerraba «no se puede aprobar».
    pub(super) fn abrir_panel_de_sync(
        &mut self,
        epoca: u64,
        task: norte_proto::TaskId,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // FILTRAR antes de TOMAR: `take()` incondicional se llevaba por
        // delante una petición nueva cuando contestaba la Task de una vieja,
        // y entonces no se abría panel ninguno mientras dos recorridos
        // seguían caminando dos árboles en el daemon.
        if self.sync_pedida.as_ref().is_none_or(|p| p.epoca != epoca) {
            return Vec::new();
        }
        let Some(pedida) = self.sync_pedida.take() else {
            return Vec::new();
        };
        self.sincronizacion = Some(Sincronizacion {
            epoca,
            task,
            abandonada: pedida.abandonada,
            vista: norte_frontend::sync::SyncView::new(
                task,
                pedida.modo,
                pedida.origen,
                pedida.destino,
                // Las reinterpretaciones de cada lado, tal como las decidió
                // la regla compartida: son DOS porque los dos panes son dos
                // ubicaciones, y cruzarlas nombraría con otros bytes el
                // fichero sobre el que cae la escritura.
                pedida.origen_encoding,
                pedida.destino_encoding,
            ),
            primera_visible: 0,
            ventana: Self::VENTANA_COMPARACION,
            epoca_conexion: self.epoca_conexion,
            informe_pedido: false,
        });
        let cambio = ViewChange::Sync {
            sync: self.vista_sincronizacion(),
        };
        vec![self.parche(vec![cambio])]
    }

    pub(super) fn aplicar_evento_de_sync(
        &mut self,
        epoca: u64,
        ev: norte_client::SyncPlanEvent,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Some(sinc) = self.sincronizacion.as_mut() else {
            return Vec::new();
        };
        if sinc.epoca != epoca {
            return Vec::new();
        }
        // El modelo COMPARTIDO decide qué entra: descarta lo que venga de
        // otro plan por su `task_id`, y es quien sabe cuándo el plan cierra.
        let cambio = match ev {
            norte_client::SyncPlanEvent::Steps(lote) => sinc.vista.state.on_steps(lote),
            norte_client::SyncPlanEvent::Done(done) => sinc.vista.state.on_plan_done(done),
        };
        if !cambio {
            // Que se descartó, DICHO: un lote rechazado después del cierre es
            // una violación del contrato del daemon, y callarla la esconde.
            tracing::warn!(epoca, "un evento del plan se descartó");
            return Vec::new();
        }
        let cambio = ViewChange::Sync {
            sync: self.vista_sincronizacion(),
        };
        vec![self.parche(vec![cambio])]
    }

    /// Los pasos de la ventana, proyectados por el modelo COMPARTIDO.
    pub(super) fn pasos_proyectados(
        pasos: &[norte_proto::methods::SyncStep],
        papelera: norte_proto::methods::DestTrash,
        enc: norte_frontend::sync::SyncEncodings,
        lang: norte_i18n::Lang,
    ) -> Vec<crate::dto::SyncStepView> {
        pasos
            .iter()
            .map(|paso| {
                // Las celdas las compone el modelo COMPARTIDO: qué hace el
                // paso, por qué, si el deshacer lo devuelve —que NUNCA sale
                // de `reversal` a secas, porque esa es media respuesta— y las
                // dos ortografías cuando las hay.
                let c = norte_frontend::sync::render_step(paso, papelera, enc);
                crate::dto::SyncStepView {
                    id: c.id,
                    kind: clamp_display(norte_frontend::sync::step_label(paso.kind, lang)),
                    // El porqué solo lo tienen los pasos que lo tienen: un
                    // `Skip`, o uno que no se puede deshacer. Vacío es
                    // AUSENCIA, no una frase inventada.
                    reason: c.reason.map_or_else(String::new, |r| {
                        clamp_display(norte_frontend::sync::reason_label(r, lang))
                    }),
                    undo: clamp_display(norte_frontend::sync::undo_label(c.undo, lang)),
                    anchor: Self::nombre_de_ancla(c.anchor),
                    anchor_label: Self::etiqueta_de_ancla(c.anchor, lang),
                    path: clamp_display(c.rel.text.clone()),
                    path_hostile: c.rel.hostile,
                    dest_path: c.dest_rel.as_ref().map(|d| clamp_display(d.text.clone())),
                    dest_path_hostile: c.dest_rel.as_ref().is_some_and(|d| d.hostile),
                    twins: c.dest_rel_twin,
                }
            })
            .collect()
    }

    /// Los fallos del informe, cuando ya hay informe.
    pub(super) fn fallos_proyectados(
        estado: &norte_frontend::sync::SyncState,
        enc: norte_frontend::sync::SyncEncodings,
        lang: norte_i18n::Lang,
    ) -> Vec<crate::dto::SyncFailureView> {
        let norte_frontend::sync::SyncState::Applied(a) = estado else {
            return Vec::new();
        };
        a.report()
            .failures
            .iter()
            .map(|f| {
                let c = norte_frontend::sync::render_failure(f, enc);
                crate::dto::SyncFailureView {
                    cause: clamp_display(norte_frontend::sync::failure_cause_label(f.cause, lang)),
                    path: clamp_display(c.rel.text.clone()),
                    path_hostile: c.rel.hostile,
                    anchor: Self::nombre_de_ancla(c.anchor),
                    anchor_label: Self::etiqueta_de_ancla(c.anchor, lang),
                }
            })
            .collect()
    }

    /// De qué raíz cuelga una ruta, por su id estable.
    ///
    /// `either` se dice: en un panel donde una ruta sin calificar significa
    /// «del origen», callarlo es afirmar el origen.
    pub(super) fn nombre_de_ancla(anchor: norte_frontend::sync::RelAnchor) -> String {
        match anchor {
            norte_frontend::sync::RelAnchor::Dest => "dest".to_owned(),
            norte_frontend::sync::RelAnchor::Source => "source".to_owned(),
            norte_frontend::sync::RelAnchor::Either => "either".to_owned(),
        }
    }

    /// La etiqueta del ancla, ya traducida, o vacía cuando no hay nada que
    /// decir.
    ///
    /// La etiqueta y no solo el id: el DTO promete que esto se pinta, y un
    /// `data-` que ningún estilo lee no lo pinta — el `either` seguía
    /// callado, que en un panel donde una ruta sin calificar significa «del
    /// origen» es afirmar el origen.
    pub(super) fn etiqueta_de_ancla(
        anchor: norte_frontend::sync::RelAnchor,
        lang: norte_i18n::Lang,
    ) -> String {
        norte_frontend::sync::anchor_label(anchor, lang).map_or_else(String::new, clamp_display)
    }

    /// La proyección del panel de sincronización, acotada a su ventana.
    pub(super) fn vista_sincronizacion(&self) -> Option<crate::dto::SyncView> {
        let sinc = self.sincronizacion.as_ref()?;
        let v = &sinc.vista;
        let (origen, origen_hostil) = norte_frontend::path_display(&v.source_root);
        let (destino, destino_hostil) = norte_frontend::path_display(&v.dest_root);
        let pasos = v.steps();
        let primera = sinc.primera_visible.min(pasos.len());
        let hasta = primera.saturating_add(sinc.ventana).min(pasos.len());
        let papelera = v.dest_trash();
        let enc = v.encodings();
        let filas = Self::pasos_proyectados(
            pasos.get(primera..hasta).unwrap_or_default(),
            papelera,
            enc,
            self.lang,
        );
        let fallos = Self::fallos_proyectados(&v.state, enc, self.lang);
        Some(crate::dto::SyncView {
            source: crate::dto::DialogLine {
                text: clamp_display(origen),
                hostile: origen_hostil,
            },
            dest: crate::dto::DialogLine {
                text: clamp_display(destino),
                hostile: destino_hostil,
            },
            // El modo, por la etiqueta COMPARTIDA. Caer en «actualizar» ante
            // un modo que esta build no sabe nombrar afirmaría la mitad
            // SEGURA de lo que se está aprobando —«esto no borra»— sobre algo
            // desconocido, y el propio catálogo lo prohíbe por escrito.
            mode: clamp_display(norte_frontend::sync::mode_label(v.mode, self.lang)),
            steps: filas,
            first_visible: primera as u64,
            // Los RETENIDOS más los que el modelo tiró: sin sumarlos, este
            // número y el de la línea de estado se contradicen en un plan
            // grande, y los dos cruzan en el mismo mensaje.
            total: (pasos.len() as u64).saturating_add(
                v.state
                    .plan()
                    .map_or(0, norte_frontend::sync::SyncPlan::dropped),
            ),
            // El RESUMEN, que es lo que un humano lee antes de aprobar:
            // irreversibles, bytes (con los que no se pudieron medir aparte),
            // lo que no se pudo leer, y si la lista esconde pasos. No cabe en
            // la línea de estado y no puede quedarse dentro del modelo.
            summary: v
                .state
                .plan()
                .map(|p| {
                    p.summary_lines(self.lang)
                        .into_iter()
                        .map(clamp_display)
                        .collect()
                })
                .unwrap_or_default(),
            // Lo que IMPIDE aplicar, con SU RUTA: «el destino es de solo
            // lectura» sin decir cuál manda a buscar el problema a ciegas, y
            // un bloqueo de la raíz se dice «todo el árbol», no vacío.
            blockers: v
                .state
                .plan()
                .map(|p| {
                    p.done()
                        .blockers
                        .iter()
                        .map(|b| {
                            // Sin reinterpretación: de qué raíz cuelga el
                            // `rel` de un BLOQUEO no lo decide ninguna regla
                            // compartida todavía —la que existe es para
                            // pasos—, y elegirla aquí sería inventar una
                            // segunda respuesta. Hoy no cambia nada porque
                            // esta ventana no tiene override de codificación
                            // de nombres; el día que lo tenga, la regla va
                            // arriba y no aquí.
                            let ruta =
                                norte_frontend::sync::rel_display_or_root(&b.rel, None, self.lang);
                            crate::dto::SyncBlockerView {
                                label: clamp_display(norte_frontend::sync::blocker_label(
                                    b.kind, self.lang,
                                )),
                                path: clamp_display(ruta.text),
                                path_hostile: ruta.hostile,
                            }
                        })
                        .collect()
                })
                .unwrap_or_default(),
            // Cuántos hay DE VERDAD: el wire recorta la lista a 256 y el
            // total viaja aparte justo para que 40 000 no se lean como 256.
            blockers_total: v.state.plan().map_or(0, |p| p.done().blockers_total),
            status: clamp_display(norte_frontend::sync::status_line(v, self.lang)),
            // La línea de teclas del modelo ofrece `a aprobar` en cuanto el
            // plan se puede aprobar, y esta fase NO tiene esa tecla: decir lo
            // que no se puede hacer entrena a pulsarla justo en la pantalla
            // donde la fase siguiente pone la escritura. Mientras aprobar no
            // exista, esta pantalla dice que solo lee.
            hint: clamp_display(if v.can_approve() {
                norte_i18n::t_in(self.lang, "host-sync-read-only")
            } else {
                norte_i18n::t_in(self.lang, norte_frontend::sync::hint_id(v))
            }),
            confirming: v.confirming.as_ref().map(|c| clamp_display(c.text.clone())),
            // Los fallos del informe, uno a uno. El recuento va en la línea
            // de estado, que lo compone el modelo compartido; esto es el
            // detalle, y sin él «3 fallaron» no dice cuáles.
            failures: fallos,
            cancel_requested: v.cancel_requested,
            can_approve: v.can_approve(),
            running: matches!(v.run, norte_frontend::sync::SyncRunState::Running),
        })
    }

    /// Cuántas filas de la comparación —o pasos de un plan— cruzan si el
    /// renderer no ha dicho su ventana todavía.
    pub(super) const VENTANA_COMPARACION: usize = 200;

    /// Lanza la comparación de los dos paneles y abre el panel de
    /// diferencias.
    ///
    /// La raíz derecha sale del hueco con el rol `Target`, por el MISMO
    /// camino que una transferencia: dos formas de decidir «el otro panel»
    /// son dos sitios donde pueden divergir, y con varios candidatos y
    /// ninguno designado se pide elegir en vez de romper el empate.
    pub(super) fn pedir_comparacion(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let derecha = match self.directorio_destino() {
            Ok(d) => d,
            Err(reason_key) => {
                return (
                    ActionAck::Unavailable {
                        reason_key: reason_key.to_owned(),
                    },
                    Vec::new(),
                );
            }
        };
        let izquierda = self.hueco().pane.dir().clone();
        if izquierda == derecha {
            // El daemon lo rechazaría igual (`-32602`), y abrir un panel que
            // promete una respuesta imposible es peor que decirlo antes.
            return (
                ActionAck::Unavailable {
                    reason_key: "host-same-directory".to_owned(),
                },
                Vec::new(),
            );
        }
        (
            self.aplicada(),
            self.lanzar_comparacion(izquierda, derecha, backend, buzon),
        )
    }

    /// `pane.dir-size` (#139, #290): cuenta lo que ocupa lo MARCADO —o lo que
    /// hay bajo el cursor— y lo deja en el tablero.
    ///
    /// UNA Task para el lote entero, al revés que copiar o borrar: el método
    /// del wire toma una lista, y contar por separado obligaría a quien
    /// pregunta a sumar los bytes **y** los ilegibles, que no se suman igual
    /// —un total redondo compuesto de dos cuentas parciales es una respuesta
    /// equivocada, no una incompleta—.
    ///
    /// No hay directorios afectados que refrescar: esto no escribe nada. Su
    /// resultado ES su progreso terminal, que el tablero ya sabe leer.
    pub(super) fn contar_tamano(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // `marked_paths` cae al cursor cuando no hay marcas: la misma fuente
        // de «sobre qué opera esto» que usa una transferencia.
        let paths: Vec<VPath> = self.hueco().pane.marked_paths();
        if paths.is_empty() {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-nothing-selected".to_owned(),
                },
                self.decir("msg-nothing-selected"),
            );
        }
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let mensaje = match backend.dir_size(paths).await {
                Ok(task) => Mensaje::TaskNueva(Box::new((task, Vec::new(), None))),
                Err(e) => Mensaje::TaskFallida(Box::new(e)),
            };
            let _ = buzon.send(mensaje).await;
        });
        (self.aplicada(), Vec::new())
    }

    /// Encola `fs.compare` y engancha su canal de filas al actor.
    pub(super) fn lanzar_comparacion(
        &mut self,
        izquierda: VPath,
        derecha: VPath,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        self.epoca_busqueda += 1;
        let epoca = self.epoca_busqueda;
        let abandonada = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let params = norte_proto::methods::FsCompareParams {
            left: izquierda.clone(),
            right: derecha.clone(),
            criteria: norte_proto::methods::CompareCriteria::default(),
            // Sin tope de profundidad, como el TUI: una comparación que se
            // para a mitad no ha contestado a lo que se le preguntó.
            max_depth: None,
            // La regla FAT, que es el default del wire.
            mtime_tolerance_ms: 2000,
            // Sin seguir enlaces, como el default del core: los destinos se
            // comparan como BYTES, y seguirlos podría salirse del árbol que
            // se preguntó.
            follow_symlinks: false,
            // Un huérfano se emite como UNA fila y no se recorre, que es lo
            // que sabe hacer el default. Descender un lado es una decisión
            // del plan de sincronización, no de una comparación que solo
            // mira.
            descend_orphans: None,
        };
        self.comparacion = Some(Comparacion {
            epoca,
            task: norte_proto::TaskId::new(0),
            abandonada: Arc::clone(&abandonada),
            vista: norte_frontend::compare::CompareView::new(
                izquierda,
                derecha,
                // El hueco que lanzó la comparación ES el lado izquierdo, y
                // eso decide a qué panel navega un `Enter`. Sin ello, quien
                // mira el lado derecho perdía su directorio de la izquierda
                // para ir a ver el de la derecha.
                self.activo() as usize,
                None,
                None,
            ),
            primera_visible: 0,
            ventana: Self::VENTANA_COMPARACION,
        });
        let backend2 = Arc::clone(backend);
        let buzon2 = buzon.clone();
        tokio::spawn(async move {
            let (task, mut rx) = match backend2.compare(params).await {
                Ok(par) => par,
                Err(e) => {
                    let _ = buzon2.send(Mensaje::TaskFallida(Box::new(e))).await;
                    return;
                }
            };
            let id = task.id;
            let cancel = Arc::clone(&task.cancel);
            let _ = buzon2
                .send(Mensaje::TaskNueva(Box::new((task, Vec::new(), None))))
                .await;
            let _ = buzon2
                .send(Mensaje::Fondo(Box::new(Fondo::ComparacionViva(epoca, id))))
                .await;
            // La vista pudo cerrarse mientras el daemon aceptaba la Task: en
            // esa ventana el actor no tiene a quién cancelar, así que cancela
            // quien sí lo tiene.
            if abandonada.load(std::sync::atomic::Ordering::SeqCst) {
                cancel();
                return;
            }
            while let Some(lote) = rx.recv().await {
                if abandonada.load(std::sync::atomic::Ordering::SeqCst) {
                    cancel();
                    return;
                }
                if buzon2
                    .send(Mensaje::Fondo(Box::new(Fondo::FilasComparadas(
                        epoca,
                        Box::new(lote),
                    ))))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        });
        let cambio = ViewChange::Compare {
            compare: self.vista_comparacion(),
        };
        vec![self.parche(vec![cambio])]
    }

    /// Un lote de filas comparadas. Casa por ÉPOCA, como los hallazgos.
    pub(super) fn aplicar_filas_comparadas(
        &mut self,
        epoca: u64,
        lote: norte_proto::methods::CompareRowsBatch,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Some(c) = self.comparacion.as_mut() else {
            return Vec::new();
        };
        if c.epoca != epoca {
            return Vec::new();
        }
        // El panel COMPARTIDO es quien cuenta, filtra y selecciona: aquí solo
        // se le dan las filas.
        c.vista.pane.extend(lote.rows);
        let cambio = ViewChange::Compare {
            compare: self.vista_comparacion(),
        };
        vec![self.parche(vec![cambio])]
    }

    /// La proyección del panel de diferencias, acotada a su ventana.
    pub(super) fn vista_comparacion(&self) -> Option<crate::dto::CompareView> {
        use norte_frontend::compare::{Category, cells_for};

        let c = self.comparacion.as_ref()?;
        let ahora = ahora_ms();
        let (izq, izq_hostil) = norte_frontend::path_display(&c.vista.left_root);
        let (der, der_hostil) = norte_frontend::path_display(&c.vista.right_root);
        let visibles: Vec<&norte_proto::methods::CompareRow> = c.vista.pane.visible().collect();
        let primera = c.primera_visible.min(visibles.len());
        let hasta = primera.saturating_add(c.ventana).min(visibles.len());
        let filas = visibles
            .get(primera..hasta)
            .unwrap_or_default()
            .iter()
            .map(|r| {
                // Las celdas las compone el modelo COMPARTIDO: los nombres
                // enmascarados con su bandera, y los dos glifos del medio.
                // Ni el emparejado ni el veredicto se recalculan aquí.
                let celdas = cells_for(r, None, None);
                let cara = |f: Option<&norte_frontend::compare::RowFace>| {
                    f.map(|f| crate::dto::CompareFaceView {
                        name: clamp_display(f.name.clone()),
                        hostile: f.hostile,
                        // Formateados con las MISMAS funciones que una
                        // columna del listado: un tamaño o una fecha no
                        // pueden leerse distinto según qué panel los pinte.
                        size: f.size.map(norte_frontend::human_bytes).unwrap_or_default(),
                        mtime: f
                            .mtime_ms
                            .map(|ms| {
                                norte_frontend::columns::format_mtime_in(
                                    ms,
                                    norte_frontend::columns::TimeFormat::Iso,
                                    ahora,
                                    self.lang,
                                )
                            })
                            .unwrap_or_default(),
                        is_dir: f.kind == EntryKind::Dir,
                    })
                };
                crate::dto::CompareRowView {
                    id: r.id,
                    verdict: clamp_display(norte_frontend::compare::verdict_label(
                        r.verdict, self.lang,
                    )),
                    category: Category::of(r.verdict).id().to_owned(),
                    confidence: clamp_display(norte_frontend::compare::confidence_label(
                        r.confidence,
                        self.lang,
                    )),
                    criterion: clamp_display(norte_frontend::compare::criterion_label(
                        r.criterion,
                        self.lang,
                    )),
                    reason: r.reason.map(|x| {
                        clamp_display(norte_frontend::compare::reason_label(x, self.lang))
                    }),
                    left: cara(celdas.left.as_ref()),
                    right: cara(celdas.right.as_ref()),
                    paired_under: norte_frontend::compare::paired_under_label(
                        r.paired_under,
                        self.lang,
                    )
                    .map(clamp_display),
                }
            })
            .collect();
        let filtros = norte_frontend::compare::CATEGORIES
            .iter()
            .map(|cat| crate::dto::CompareFilterView {
                id: cat.id().to_owned(),
                label: clamp_display(cat.label(self.lang)),
                count: c.vista.pane.count_of(*cat) as u64,
                hidden: c.vista.pane.is_hidden(*cat),
            })
            .collect();
        Some(crate::dto::CompareView {
            left: clamp_display(izq),
            left_hostile: izq_hostil,
            right: clamp_display(der),
            right_hostile: der_hostil,
            rows: filas,
            first_visible: primera as u64,
            total: visibles.len() as u64,
            selected: c.vista.pane.selected_id(),
            filters: filtros,
            // La frase la compone el modelo COMPARTIDO, y no es un detalle:
            // sus cinco estados son cómo se dice si la respuesta está
            // completa, y una comparación que perdió lotes tiene que leerse
            // distinto de una que terminó. El TUI y el CLI ya se equivocaron
            // aquí cada uno por su cuenta.
            status: clamp_display(norte_frontend::compare::status_line(
                &c.vista,
                c.vista.pane.marked_len(),
                self.lang,
            )),
            running: c.vista.state == norte_frontend::compare::CompareState::Running,
        })
    }
}
