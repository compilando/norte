//! El tablero de tasks: progreso, desenlace, informe y cancelación.
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
    /// Hace sitio en el tablero tirando lo más viejo TERMINADO.
    ///
    /// Se prefiere desalojar una TERMINADA BIEN: una fallida o una cancelada
    /// es la única superficie que dice qué no llegó —un fallo no deja entrada
    /// de journal—, y en un lote grande con colisiones son justo las que se
    /// acumulan. Una VIVA no se toca: tiene progreso que bombear y, quizá, un
    /// directorio que relistar.
    /// Le pone reloj a una task recién terminada: a los [`TTL_TASK_TERMINAL`]
    /// se va del tablero.
    ///
    /// Mismo patrón que el TTL de una aprobación: un `spawn` que duerme y
    /// manda un mensaje al actor, porque el estado lo toca un solo escritor.
    pub(super) fn programar_caducidad(id: u64, epoca: u64, buzon: &mpsc::Sender<Mensaje>) {
        let buzon = buzon.clone();
        tokio::spawn(async move {
            tokio::time::sleep(TTL_TASK_TERMINAL).await;
            let _ = buzon.send(Mensaje::TaskCaducada(id, epoca)).await;
        });
    }

    /// Se acabó el rato de una task terminada: fuera del tablero.
    ///
    /// Tres cosas se comprueban antes, y ninguna es paranoia:
    ///
    /// - la ÉPOCA, porque tras un relevo del daemon los ids empiezan de nuevo
    ///   y este reloj lleva diez segundos volando;
    /// - que siga TERMINAL, porque un id reanunciado puede volver a estar en
    ///   marcha;
    /// - que no deba un refresco (`afectados`), que es el invariante que el
    ///   desalojo por tope ya afirma: tirar la fila se llevaría por delante la
    ///   relectura del directorio que esa mutación cambió.
    pub(super) fn caducar_task(&mut self, id: u64, epoca: u64) -> Vec<BridgeEnvelope<UiUpdate>> {
        let quitar = self.tasks.get(&id).is_some_and(|t| {
            t.epoca == epoca && Self::terminal(t.vista.state) && t.afectados.is_empty()
        });
        if !quitar {
            return Vec::new();
        }
        self.tasks.remove(&id);
        vec![self.parche(vec![ViewChange::Tasks {
            tasks: self.vistas_de_tasks(),
            cursor: self.cursor_del_tablero(),
        }])]
    }

    pub(super) fn desalojar_del_tablero(&mut self) {
        if self.tasks.len() < MAX_TASKS {
            return;
        }
        let viejo = self
            .tasks
            .iter()
            .find(|(_, t)| t.vista.state == crate::dto::TaskStateView::Done)
            .or_else(|| {
                self.tasks
                    .iter()
                    .find(|(_, t)| Self::terminal(t.vista.state))
            })
            .map(|(k, _)| *k);
        if let Some(viejo) = viejo {
            debug_assert!(
                self.tasks[&viejo].afectados.is_empty(),
                "se desaloja una task con un refresco pendiente"
            );
            self.tasks.remove(&viejo);
        }
    }

    /// Relanza la transferencia que chocó, con la política elegida (#274).
    ///
    /// Repite el MISMO verbo: un «sobrescribir» sobre una copia que se
    /// convirtiera en un movimiento borraría el origen que nadie mandó tocar.
    /// Y vuelve a viajar con su `Reintento`, porque el segundo intento puede
    /// chocar otra vez —`Skip` y `RenameAuto` no, pero `Newer` sí— y entonces
    /// hay que poder volver a preguntar.
    pub(super) fn lanzar_reintento(
        con: Reintento,
        politica: norte_proto::CollisionPolicy,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        // El destino cambia; el origen también deja de estar si es un
        // movimiento. Se apuntan los dos padres, como en la transferencia
        // original.
        let mut afectados: Vec<VPath> = con.to.parent().into_iter().collect();
        if con.mover
            && let Some(padre) = con.from.parent()
            && !afectados.contains(&padre)
        {
            afectados.push(padre);
        }
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let encolada = if con.mover {
                backend
                    .move_(con.from.clone(), con.to.clone(), politica)
                    .await
            } else {
                backend
                    .copy(con.from.clone(), con.to.clone(), politica)
                    .await
            };
            let mensaje = match encolada {
                Ok(task) => Mensaje::TaskNueva(Box::new((task, afectados, Some(con)))),
                Err(e) => Mensaje::TaskFallida(Box::new(e)),
            };
            let _ = buzon.send(mensaje).await;
        });
    }

    /// Entrega el secreto al core y devuelve el desenlace por el buzón (#327).
    ///
    /// El `TypedSecret` se MUEVE a la task y muere con ella, así que la copia
    /// del host se pisa con ceros en cuanto el core contesta. El `String` en
    /// claro que exige la llamada nace lo más tarde posible y vive lo mínimo.
    /// De las copias de más allá —los params, el frame, el `Value` del
    /// daemon— habla el ADR 0015.
    ///
    /// No devuelve nada: como todo lo que TARDA en esta ventana, la respuesta
    /// vuelve al actor como un mensaje más. El único escritor no espera a
    /// nadie, así que el cursor sigue respondiendo mientras el core autentica.
    pub(super) fn lanzar_secreto(
        conn: String,
        secreto: norte_frontend::secret::TypedSecret,
        slot: u32,
        dir: VPath,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let res = backend
                .provide_secret(conn, secreto.expose().to_owned())
                .await;
            drop(secreto);
            let _ = buzon
                .send(Mensaje::SecretoEntregado(Box::new((slot, dir, res))))
                .await;
        });
    }

    /// El reintento que ya tenía esta task, si la hay y es de esta época.
    ///
    /// Un reanuncio de la reconexión no sabe con qué se pidió la task, así que
    /// sustituirlo por `None` dejaría sin salida justo a la colisión que el
    /// lector encuentra al volver. La época importa: tras un relevo del daemon
    /// los ids vuelven a empezar, y lo que había con ese número era otra cosa.
    pub(super) fn reintento_heredado(&self, id: u64) -> Option<Reintento> {
        self.tasks
            .get(&id)
            .filter(|t| t.epoca == self.epoca_conexion)
            .and_then(|t| t.reintento.clone())
    }

    /// Ata la intención de «editar uno nuevo» a la task que lo crea (#290).
    ///
    /// Aquí y no antes: el id no existe hasta que el daemon contesta, y el
    /// gesto ya había vuelto. Solo a una task PROPIA y solo si la intención
    /// todavía no tiene id — una ajena que pase por aquí no puede adoptar la
    /// intención de esta ventana, que es justo el fallo que esto evita.
    pub(super) fn atar_la_creacion(&mut self, id: u64, ajena: bool, kind: norte_proto::TaskKind) {
        if !ajena
            && kind == norte_proto::TaskKind::Create
            && let Some(c) = self.abrir_al_crear.as_mut()
            && c.task.is_none()
        {
            c.task = Some(id);
        }
    }

    /// Conserva el detalle que un REANUNCIO no trae.
    ///
    /// El SDK vuelve a ofrecer las tasks al reconectar, y ese progreso no sabe
    /// nada del informe que ya se pidió por esta task. Proyectarlo tal cual
    /// borraba del tablero la única señal de que el directorio se quedó a
    /// medias, justo cuando la conexión se recupera y el lector vuelve a
    /// mirarlo.
    ///
    /// Solo se hereda de la MISMA época: tras un relevo del daemon el id
    /// vuelve a empezar en 1, y lo que había con ese número era otra task.
    fn heredar_detalle(&self, id: u64, vista: &mut crate::dto::TaskView) {
        let Some(anterior) = self
            .tasks
            .get(&id)
            .filter(|t| t.epoca == self.epoca_conexion)
        else {
            return;
        };
        if anterior.informe_pedido && Self::terminal(vista.state) && anterior.vista.detail.is_some()
        {
            vista.detail.clone_from(&anterior.vista.detail);
            vista.detail_hostile = anterior.vista.detail_hostile;
        }
    }

    /// Mete una Task recién encolada en el tablero y deja su progreso
    /// bombeando hacia el actor.
    pub(super) fn registrar_task(
        &mut self,
        task: crate::backend::HostTask,
        afectados: Vec<VPath>,
        reintento: Option<Reintento>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let id = task.id.get();
        let ajena = task.foreign;
        // Alta en la cuenta del lote (#271), antes de cualquier desalojo: lo
        // que se encoló se encoló aunque su fila no llegue a caber.
        if let Some(lote) = self.lote.as_mut()
            && !ajena
            && lote.encoladas + lote.rechazadas < lote.total
            && lote.ids.insert(id)
        {
            lote.encoladas += 1;
        }
        self.desalojar_del_tablero();
        // Y el techo DURO de lo retenido (#271). Solo se llega aquí con el
        // tablero lleno de tasks VIVAS, y solo desde el canal de ajenas: las
        // propias no pasan de `pedir_transferencia`, que rehúsa el lote entero
        // si no cabe. Una fila ajena que se cae no pierde nada —viene con
        // `afectados` vacío, o sea sin refresco que deber— salvo una fila que
        // esta ventana nunca prometió enseñar.
        if self.tasks.len() >= MAX_TASKS_RETAINED && !self.tasks.contains_key(&id) {
            tracing::debug!(task = id, "tablero lleno: no se retiene una task ajena");
            return Vec::new();
        }
        let mut rx = task.progress.clone();
        let nacio = rx.borrow().clone();
        self.atar_la_creacion(id, ajena, nacio.kind);
        // #311: la Task de sumas ya tiene id, así que la intención apuntada al
        // encolarla se convierte en el lote que espera su informe. Solo la
        // PROPIA: una task ajena del mismo kind es la comprobación de otra
        // ventana, y colgarle este informe le daría los digests de otro.
        if !ajena
            && nacio.kind == norte_proto::TaskKind::Checksum
            && let Some(encolada) = self.sumas_pendientes.take()
        {
            self.sumas = Some(SumasEnVuelo {
                task: task.id,
                epoca_conexion: self.epoca_conexion,
                informe_pedido: false,
                publicado: encolada.publicado,
            });
        }
        let mut vista = Self::vista_de(&nacio);
        vista.foreign = ajena;
        self.heredar_detalle(id, &mut vista);
        // Si esta task YA estaba en el tablero —una reconexión la reanuncia
        // por el canal de ajenas— lo que llega no sabe qué directorios tocaba,
        // así que se conserva lo apuntado: sustituirlo por una lista vacía
        // perdía el relistado justo en el camino donde la pantalla es más
        // probable que esté rancia.
        let afectados = if afectados.is_empty() {
            self.tasks
                .get(&id)
                .filter(|t| t.epoca == self.epoca_conexion)
                .map(|t| t.afectados.clone())
                .unwrap_or_default()
        } else {
            afectados
        };
        // Una mutación ACEPTADA es la prueba de que el journal volvió: el
        // daemon rehúsa mutar sin él (regla dura 4), así que si esta entró,
        // el aviso de «no se registra» dejó de ser verdad. No hay
        // notificación de recuperación —el TUI la tiene porque su journal es
        // embebido—, y un aviso que no sabe apagarse miente sobre lo único
        // que describe de toda la sesión.
        let apaga_el_aviso = self.journal_rehusado && !ajena && Self::muta(vista.kind.as_str());
        if apaga_el_aviso {
            self.journal_rehusado = false;
        }
        // Igual que con los afectados: si ya estaba, se conserva que su
        // informe se pidió. Una reconexión que reanuncia un lote terminado no
        // puede volver a abrir el mismo informe.
        let informe_pedido = self
            .tasks
            .get(&id)
            .is_some_and(|t| t.informe_pedido && t.epoca == self.epoca_conexion);
        let reintento = reintento.or_else(|| self.reintento_heredado(id));
        self.tasks.insert(
            id,
            TaskViva {
                vista,
                cancel: task.cancel,
                afectados,
                reintento,
                informe_pedido,
                epoca: self.epoca_conexion,
                progreso: task.progress.clone(),
            },
        );
        let buzon2 = buzon.clone();
        tokio::spawn(async move {
            // El estado de AHORA ya lo proyectó el registro; lo que bombea
            // esto son los CAMBIOS. El terminal va por la misma cola ordenada
            // que todo lo demás y se manda antes de soltar el canal: un
            // desenlace que se pierde deja al usuario mirando un progreso que
            // no avanza.
            while rx.changed().await.is_ok() {
                let snapshot = rx.borrow_and_update().clone();
                let terminal = matches!(
                    snapshot.state,
                    norte_proto::TaskState::Completed
                        | norte_proto::TaskState::Cancelled
                        | norte_proto::TaskState::Failed { .. }
                );
                if buzon2
                    .send(Mensaje::Progreso(Box::new(snapshot)))
                    .await
                    .is_err()
                    || terminal
                {
                    return;
                }
            }
        });
        // Puede nacer TERMINAL: el daemon la completó antes de que esta
        // llamada volviera, y entonces `rx.changed()` no dispara nunca y
        // `progreso` no se llama ni una vez. Sin esto, una copia rapidísima
        // dejaba el destino sin relistar para siempre — la carrera que la
        // tarea 5.1 nombra literalmente.
        let mut cambios = vec![ViewChange::Tasks {
            tasks: self.vistas_de_tasks(),
            cursor: self.cursor_del_tablero(),
        }];
        if apaga_el_aviso {
            cambios.push(self.cambio_de_banners());
        }
        cambios.extend(self.nacio_terminal(id, &nacio, backend, buzon));
        vec![self.parche(cambios)]
    }

    /// Lo que hay que atender cuando una task llega al tablero YA terminada.
    ///
    /// Todo esto lo haría `progreso`, y `progreso` no se va a llamar ni una
    /// vez: `rx.changed()` no dispara para un canal que nació con su valor
    /// final. Sin ello, una copia rapidísima dejaba el destino sin relistar
    /// para siempre — la carrera que la tarea 5.1 nombra literalmente.
    pub(super) fn nacio_terminal(
        &mut self,
        id: u64,
        nacio: &norte_proto::TaskProgress,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<ViewChange> {
        let Some(estado) = self
            .tasks
            .get(&id)
            .map(|t| t.vista.state)
            .filter(|e| Self::terminal(*e))
        else {
            return Vec::new();
        };
        // Su rato en el tablero se cuenta desde aquí, por lo mismo.
        Self::programar_caducidad(id, self.epoca_conexion, buzon);
        let mut cambios = Vec::new();
        if self.anota_desenlace_de_lote(id, estado) {
            cambios.push(self.cambio_de_banners());
        }
        cambios.extend(self.refrescar_afectados(id, backend, buzon));
        // Y su informe, por el mismo motivo que el relistado: es la única
        // señal de que el directorio se quedó a medias, y un lote rapidísimo
        // se quedaba sin ella justo cuando el desenlace de la Task más parece
        // que todo fue bien.
        self.pedir_informe_de_lote(nacio, backend, buzon);
        // #311: y el de las sumas, por lo mismo. Un lote de tres ficheros
        // pequeños nace terminal casi siempre, así que sin esto el camino
        // rápido —el que más se usa— no enseñaba nada.
        self.pedir_informe_de_sumas(nacio, backend, buzon);
        cambios
    }

    /// Aplica un snapshot de progreso al tablero.
    pub(super) fn progreso(
        &mut self,
        p: &norte_proto::TaskProgress,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Some(viva) = self.tasks.get_mut(&p.task_id.get()) else {
            return Vec::new();
        };
        let ajena = viva.vista.foreign;
        let era_terminal = Self::terminal(viva.vista.state);
        let epoca = viva.epoca;
        viva.vista = Self::vista_de(p);
        // De quién es la task no lo dice el progreso: lo dice de dónde vino.
        viva.vista.foreign = ajena;
        // Leído de la vista que se acaba de proyectar: volver a construirla
        // solo para mirar su estado cuesta dos `String` y un `path_display`
        // en cada tick de progreso de cada task del lote.
        let acabo = Self::terminal(viva.vista.state);
        let estado_final = viva.vista.state;
        // Acaba de terminar: empieza su rato en el tablero. Solo en la
        // TRANSICIÓN — el daemon repite el último progreso al reconectar, y
        // rearmar el reloj en cada repetición dejaría la fila ahí para
        // siempre, que es justo lo contrario de lo que se pide.
        if acabo && !era_terminal {
            Self::programar_caducidad(p.task_id.get(), epoca, buzon);
        }
        let mut cambios = vec![ViewChange::Tasks {
            tasks: self.vistas_de_tasks(),
            cursor: self.cursor_del_tablero(),
        }];
        // El desenlace entra en la cuenta del lote (#271). Solo cuando el lote
        // queda RESUELTO viaja algo: doscientas frases de «una más» no dicen
        // nada que la fila no diga ya.
        if acabo && self.anota_desenlace_de_lote(p.task_id.get(), estado_final) {
            cambios.push(self.cambio_de_banners());
        }
        // Un deshacer que TERMINA suelta su sesión: mientras corre, la fila
        // lo dice y `u` sobre ella se rehúsa —dos undos de la misma sesión
        // caminan la misma lista de entradas— y eso no puede quedarse pegado
        // para siempre.
        if acabo && let Some(sesion) = self.agencia.undos.remove(&p.task_id.get()) {
            self.agencia.sesiones.deshecha(&sesion);
            if self.agencia.panel {
                cambios.push(ViewChange::Agents {
                    agents: self.vista_agentes(),
                });
            }
        }
        // Si la que acaba de terminar es LA búsqueda, su vista deja de decir
        // «buscando…»: una lista que ya no crece y una que sigue creciendo se
        // leen igual si nadie las distingue.
        // CON el desenlace, no solo «ya no está viva»: una búsqueda que falló
        // al segundo directorio y otra que recorrió el árbol entero se
        // pintaban las dos como «N hallazgos», que es una afirmación falsa
        // sobre el disco — y quien la lee deja de buscar.
        //
        // Y quién es un desenlace lo dice el crate compartido, no un
        // `!= Running`: `Pending`, `Paused` y `Unknown` tampoco lo son, y con
        // aquel predicado una búsqueda encolada —o una de un daemon más
        // nuevo— se anunciaba terminada sin hallazgos.
        let lang = self.lang;
        let desenlace = norte_frontend::search_status::outcome_of(&p.state, |e| {
            clamp_display(norte_frontend::error::error_category_in(lang, e))
        });
        if let Some(desenlace) = desenlace
            && let Some(b) = self.busqueda.as_mut()
            && b.task == p.task_id
        {
            b.desenlace = desenlace;
            cambios.push(ViewChange::Search {
                search: self.vista_busqueda(),
            });
        }
        // Una mutación que terminó deja pantallas desactualizadas: la entrada
        // nueva está en el disco y no en el listado. Solo con un desenlace de
        // VERDAD —`Running` no lo es—, y una sola vez.
        if acabo {
            cambios.extend(self.refrescar_afectados(p.task_id.get(), backend, buzon));
            self.pedir_informe_de_lote(p, backend, buzon);
            cambios.extend(self.cerrar_comparacion(p));
            cambios.extend(self.cerrar_sincronizacion(p));
            self.pedir_informe_de_sync(p, backend, buzon);
            // #311: y el de las sumas, que es donde viajan los digests.
            self.pedir_informe_de_sumas(p, backend, buzon);
            cambios.extend(self.decir_el_recuento(p));
            cambios.extend(self.ofrecer_reintento(p));
            self.abrir_lo_creado(p, backend, buzon);
            self.avisar_del_desenlace(p);
        }
        vec![self.parche(cambios)]
    }

    /// El TOTAL de un recuento, que es lo único que ese recuento produce
    /// (#139, #290).
    ///
    /// `fs.dir_size` no publica nada ni muta nada: su resultado **es** su
    /// progreso terminal. Sin esto, la ventana lanzaría la cuenta, la vería
    /// terminar en el tablero y no diría nunca cuánto ocupaba.
    ///
    /// **Un total con algo ilegible dentro se dice DISTINTO**: un recuento
    /// sirve para decidir si algo CABE en el destino, así que darlo redondo
    /// sin haberlo podido contar entero es una respuesta equivocada, no una
    /// incompleta. Con ilegibles se dice «al menos», que es lo que se sabe.
    ///
    /// `unreadable: None` —un daemon 0.52, que no los contaba— se lee como
    /// cero, igual que en el TUI (`refresh.rs`): callar el total porque el
    /// otro extremo es viejo sería peor que darlo. Las dos superficies tienen
    /// que decir lo mismo ante el mismo progreso.
    ///
    /// Solo con `Completed`: una cuenta cancelada o fallida no tiene total que
    /// dar, y pintar el parcial de una cancelación como si fuera la respuesta
    /// es el mismo error de arriba con otro nombre.
    /// Saca un aviso por el escritorio cuando un agente PIDE permiso (#285).
    ///
    /// De los tres avisos, este es el que justifica el mecanismo: una
    /// aprobación tiene TTL y se deniega sola si nadie contesta, así que no
    /// enterarse cambia el desenlace. Una copia terminada sigue terminada
    /// cuando vuelves.
    ///
    /// Lleva la OPERACIÓN y quién la pide, no las rutas: el cuerpo de la
    /// petición puede ser largo y el diálogo lo enseña entero cuando se abra.
    /// Lo que la notificación tiene que conseguir es que alguien mire.
    pub(super) fn avisar_de_aprobacion(
        &mut self,
        req: &norte_proto::methods::PolicyApprovalRequired,
    ) {
        if self.enfocada {
            return;
        }
        let (quien, _) =
            norte_frontend::display_name(req.session.as_deref().unwrap_or_default().as_bytes());
        let (op, _) = norte_frontend::display_name(req.op.as_bytes());
        let titulo = clamp_display(norte_i18n::t_in(self.lang, "notify-approval-title"));
        let cuerpo = clamp_display(norte_i18n::ta_in(
            self.lang,
            "notify-approval-body",
            &[("op", &op), ("who", &quien)],
        ));
        self.nativo(crate::dto::NativeEffect::Notify { titulo, cuerpo });
    }

    /// Saca un aviso por el escritorio cuando una task TERMINA (#285).
    ///
    /// Solo con la ventana SIN foco: si está delante, la barra y el tablero
    /// cuentan ya lo mismo, y repetirlo por fuera es ruido. Es la única
    /// condición — un aviso que además dependiera de cuánto tardó la task
    /// necesitaría un umbral, y elegirlo bien es otra decisión.
    ///
    /// El nombre del fichero SÍ va dentro, y por eso pasa por el mismo
    /// enmascarado que el listado: una notificación acaba en el historial del
    /// escritorio y puede verse en la pantalla de bloqueo, así que un nombre
    /// con bidi o con caracteres de control no puede fingir ahí lo que no
    /// puede fingir aquí.
    pub(super) fn avisar_del_desenlace(&mut self, p: &norte_proto::TaskProgress) {
        if self.enfocada {
            return;
        }
        let (clave, cuenta) = match &p.state {
            norte_proto::TaskState::Completed => ("notify-task-done", p.entries_done),
            norte_proto::TaskState::Failed { .. } => ("notify-task-failed", p.entries_done),
            // Cancelar lo pidió quien está delante: no hace falta contárselo.
            _ => return,
        };
        // Qué fichero iba, si el progreso lo dice. `current` es una ruta del
        // otro extremo: se enmascara y se acorta igual que una fila.
        //
        // Sobre los bytes CRUDOS del último segmento, no sobre
        // `display_lossy()`: ahí los U+FFFD ya están puestos, y enmascarar un
        // texto que ya es UTF-8 impecable devuelve «fiel» siempre. Aquí el
        // veredicto no se usa —una notificación del escritorio no tiene dónde
        // poner una insignia— pero el ENMASCARADO sí, y sobre el lossy no
        // hacía nada: es el mismo error que se acaba de arreglar en el
        // diálogo de colisión, dos funciones más abajo.
        let detalle = p.current.as_ref().map_or_else(
            || cuenta.to_string(),
            |path| {
                let bytes = path
                    .file_name()
                    .map_or_else(Vec::new, |s| s.as_bytes().to_vec());
                let (texto, _) = norte_frontend::display_name(&bytes);
                clamp_display(texto)
            },
        );
        let titulo = clamp_display(norte_i18n::t_in(self.lang, clave));
        let cuerpo = clamp_display(norte_i18n::ta_in(
            self.lang,
            "notify-task-body",
            &[("what", &detalle), ("kind", clase_de_task(p.kind))],
        ));
        self.nativo(crate::dto::NativeEffect::Notify { titulo, cuerpo });
    }

    /// Una transferencia que CHOCÓ abre la pregunta que faltaba (#274).
    ///
    /// La ventana manda siempre `CollisionPolicy::Fail`, que es el default
    /// seguro —sobrescribir o renombrar son decisiones del lector—, pero no
    /// tenía dónde tomarlas: quedaba una task fallida en el tablero y ningún
    /// camino hacia delante, mientras el TUI sí ofrece las cuatro salidas.
    ///
    /// Solo con un `Conflict` y solo si la task trae con qué reintentar: un
    /// borrado o un undo no tienen otra política que ofrecer, y una task ajena
    /// no es de esta ventana.
    pub(super) fn ofrecer_reintento(&mut self, p: &norte_proto::TaskProgress) -> Vec<ViewChange> {
        if !matches!(
            p.state,
            norte_proto::TaskState::Failed {
                error: norte_proto::Error::Conflict { .. }
            }
        ) {
            return Vec::new();
        }
        let Some(con) = self
            .tasks
            .get(&p.task_id.get())
            .and_then(|t| t.reintento.clone())
        else {
            return Vec::new();
        };
        // El destino, en su propio campo y enmascarado: es un nombre de
        // fichero del otro extremo, y es LO que el lector tiene que mirar para
        // decidir si sobrescribe. Por el embudo, que enmascara los bytes
        // CRUDOS: sobre `display_lossy()` los U+FFFD ya estaban puestos y el
        // veredicto salía «fiel» — sin insignia, en la única pantalla donde
        // se aprueba sobrescribir.
        let destino = Self::linea_con_encoding(&con.to, con.enc);
        let modal = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id: modal,
            title_key: "modal-collision-title".to_owned(),
            destination: Some(destino),
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body: vec![crate::dto::DialogLine {
                text: clamp_display(norte_i18n::t_in(self.lang, "modal-collision-body")),
                hostile: false,
            }],
            overflow_note: String::new(),
            // Las MISMAS cuatro que el TUI, y en el mismo orden: es la tabla
            // de `dialog.*` del catálogo compartido, no una lista inventada
            // aquí.
            choices: vec![
                DialogChoice {
                    id: "overwrite".to_owned(),
                    label_key: "dialog-overwrite".to_owned(),
                    // Sobrescribir DESTRUYE lo que hay en el destino.
                    destructive: true,
                },
                DialogChoice {
                    id: "newer".to_owned(),
                    label_key: "dialog-newer".to_owned(),
                    // También sobrescribe, solo que condicionado a la fecha.
                    destructive: true,
                },
                DialogChoice {
                    id: "rename".to_owned(),
                    label_key: "dialog-rename".to_owned(),
                    destructive: false,
                },
                DialogChoice {
                    id: "skip".to_owned(),
                    label_key: "dialog-skip".to_owned(),
                    destructive: false,
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
            dest_check: crate::dto::DestCheckView::NotAsked,
        };
        self.dialogos.push(Dialogo {
            id: modal,
            vista,
            tecleado: Tecleado::Texto(String::new()),
            // Se abrió SOLO —llega cuando la task termina, encima de lo que el
            // lector estuviera haciendo—, así que la primera respuesta solo lo
            // reconoce. Es la misma regla que una aprobación de agente, y aquí
            // importa igual: la primera opción es «sobrescribir».
            reconocido: false,
            al_confirmar: Some(Pendiente::Reintentar { con }),
        });
        vec![ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        }]
    }

    pub(super) fn decir_el_recuento(&mut self, p: &norte_proto::TaskProgress) -> Vec<ViewChange> {
        if p.kind != norte_proto::TaskKind::DirSize
            || !matches!(p.state, norte_proto::TaskState::Completed)
        {
            return Vec::new();
        }
        let tamano = norte_frontend::human_bytes(p.bytes_done);
        let cuantas = p.entries_done.to_string();
        let saltados = p.unreadable.unwrap_or(0);
        let mensaje = if saltados > 0 {
            norte_i18n::ta_in(
                self.lang,
                "msg-dir-size-partial",
                &[
                    ("size", &tamano),
                    ("count", &cuantas),
                    ("skipped", &saltados.to_string()),
                ],
            )
        } else {
            norte_i18n::ta_in(
                self.lang,
                "msg-dir-size",
                &[("size", &tamano), ("count", &cuantas)],
            )
        };
        self.status.message = Some(clamp_display(mensaje));
        vec![ViewChange::Status(self.status.clone())]
    }

    /// Un lote de renombrado que acaba de terminar: se le pide su informe.
    ///
    /// Es la ÚNICA señal de que el directorio se quedó A MEDIAS, y hay que
    /// pedirla AUNQUE la Task diga `Completed`: el desenlace de la Task
    /// habla del lote, y el informe habla de lo que quedó en el disco.
    ///
    /// Se pide también para un lote AJENO —otro cliente de esta sesión— por
    /// el mismo motivo: el directorio medio renombrado es el mismo mire quien
    /// lo mire, y quien tiene esta ventana delante es quien lo va a ver.
    pub(super) fn pedir_informe_de_lote(
        &mut self,
        p: &norte_proto::TaskProgress,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        // Tres clases tienen informe, y las tres por el mismo motivo: lo que
        // hay que decir no cabe en el desenlace de una Task. Las dos primeras
        // cuentan lo que quedó a medias; la tercera cuenta algo de un
        // empaquetado que salió BIEN (#250).
        #[derive(Clone, Copy)]
        enum Cual {
            Lote,
            Undo,
            Empaquetado,
        }
        let cual = match p.kind {
            norte_proto::TaskKind::RenameBatch => Cual::Lote,
            norte_proto::TaskKind::Undo => Cual::Undo,
            // **Solo un empaquetado que COMPLETÓ.** Los otros dos informes
            // hablan de lo que quedó a medias, así que un `Failed` o un
            // `Cancelled` es justo cuando más falta hacen; este habla de un
            // archivo, y de un empaquetado cancelado no hay ninguno —la
            // cancelación deja el destino limpio—. El informe existe igual
            // (se calcula antes de escribir), y pintarlo diría «empaquetado,
            // pero…» sobre algo que nadie empaquetó.
            norte_proto::TaskKind::Pack if matches!(p.state, norte_proto::TaskState::Completed) => {
                Cual::Empaquetado
            }
            _ => return,
        };
        let id = p.task_id;
        match self.tasks.get_mut(&id.get()) {
            Some(t) if !t.informe_pedido => t.informe_pedido = true,
            _ => return,
        }
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        let epoca = self.epoca_conexion;
        tokio::spawn(async move {
            let cual = match cual {
                Cual::Lote => Informe::Lote(backend.rename_batch_report(id).await),
                Cual::Undo => Informe::Undo(backend.undo_report(id).await),
                Cual::Empaquetado => Informe::Empaquetado(backend.archive_pack_report(id).await),
            };
            let _ = buzon
                .send(Mensaje::Informe(Box::new((epoca, id.get(), cual))))
                .await;
        });
    }

    /// Encolar una mutación falló: se dice, y si fue por el journal se
    /// queda dicho.
    ///
    /// `error_key` devuelve una CLAVE Fluent, y el contrato de
    /// `StatusView.message` dice «ya traducido por el host»: sin traducir, el
    /// usuario leía `err-not-found` en la barra.
    pub(super) fn task_fallida(&mut self, e: &Error) -> Vec<BridgeEnvelope<UiUpdate>> {
        let clave = norte_frontend::error::error_key(e);
        self.status.message = Some(clamp_display(norte_i18n::t_in(self.lang, clave)));
        // Un rechazo por journal no es una mutación que salió mal: es que
        // ESTA SESIÓN no muta hasta que el fichero se arregle (regla dura 4).
        // Eso dura más que un mensaje.
        self.journal_rehusado |= matches!(e, Error::JournalUnavailable);
        // Una creación que ni llegó a encolarse suelta su intención: sin task
        // no hay desenlace que la consuma, y quedarse pegada haría que el
        // SIGUIENTE `edit-new` abriera el fichero de este, que no existe.
        if self
            .abrir_al_crear
            .as_ref()
            .is_some_and(|c| c.task.is_none())
        {
            self.abrir_al_crear = None;
        }
        let cambio = self.cambio_de_banners();
        let parche = self.parche(vec![cambio]);
        let aviso = self.sobre(UiUpdate::Notice(UiNotice::Message {
            key: clave.to_owned(),
            detail: None,
        }));
        vec![parche, aviso]
    }

    /// Una entrada del lote la rechazó el daemon al encolar (#271).
    ///
    /// No pinta nada: cuenta. Con `CollisionPolicy::Fail` contra un destino
    /// poblado los rechazos son la norma, y N mensajes de los que sobrevive el
    /// último no dicen ni cuántos hubo.
    ///
    /// Sin lote abierto —no debería pasar, el bucle solo manda esto dentro de
    /// uno— cae a la barra, que es lo que hacía antes: perder el aviso entero
    /// es peor que pintarlo donde ya se pintaba.
    pub(super) fn rechazo_de_lote(&mut self, e: &Error) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Some(lote) = self.lote.as_mut() else {
            return self.task_fallida(e);
        };
        lote.rechazadas += 1;
        // Un rechazo por journal sigue significando lo mismo aunque venga de
        // un lote: esta sesión NO muta hasta que el fichero se arregle (regla
        // dura 4), y eso dura más que cualquier resumen — y es lo único de un
        // rechazo suelto que SÍ viaja antes del final.
        let antes = self.journal_rehusado;
        self.journal_rehusado |= matches!(e, Error::JournalUnavailable);
        let banner_nuevo = self.journal_rehusado != antes;
        if !self.resumen_de_lote_si_cerrado() && !banner_nuevo {
            // Un parche por rechazo es la tormenta que esto existe para
            // apagar: mientras el lote siga abierto, nada viaja.
            return Vec::new();
        }
        let cambio = self.cambio_de_banners();
        vec![self.parche(vec![cambio])]
    }

    /// Anota el desenlace de UNA task del lote (#271). `true` si con ella el
    /// lote quedó resuelto y `status.message` ya lleva el resumen.
    ///
    /// El id se saca de la cuenta al anotarlo: un progreso terminal puede
    /// llegar más de una vez —un reanuncio tras reconectar trae el estado
    /// final otra vez— y la segunda no es un segundo desenlace.
    pub(super) fn anota_desenlace_de_lote(&mut self, id: u64, estado: TaskStateView) -> bool {
        let Some(lote) = self.lote.as_mut() else {
            return false;
        };
        if !lote.ids.remove(&id) {
            return false;
        }
        if estado == TaskStateView::Done {
            lote.hechas += 1;
        } else {
            lote.fallidas += 1;
        }
        self.resumen_de_lote_si_cerrado()
    }

    /// Si el lote está resuelto, pone el resumen en la barra y lo cierra.
    pub(super) fn resumen_de_lote_si_cerrado(&mut self) -> bool {
        let Some(lote) = self.lote.as_ref() else {
            return false;
        };
        if !lote.cerrado() {
            return false;
        }
        // Rechazada al encolar y terminada mal son el mismo desenlace para
        // quien mira: no llegó. Distinguirlas pediría dos números más en una
        // frase que tiene que caber en la barra.
        let total = lote.total.to_string();
        let bien = lote.hechas.to_string();
        let mal = (lote.rechazadas + lote.fallidas).to_string();
        self.lote = None;
        self.status.message = Some(clamp_display(norte_i18n::ta_in(
            self.lang,
            "msg-batch-summary",
            &[("total", &total), ("ok", &bien), ("fail", &mal)],
        )));
        true
    }

    /// Un informe llegó: al tablero, y delante si dejó algo a medias.
    pub(super) fn informe(
        &mut self,
        epoca: u64,
        task_id: u64,
        cual: &Informe,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // Un informe que salió ANTES de la reconexión habla de una task de
        // otro daemon, y el id puede estar reutilizado: colgarlo de la fila
        // que hoy lleva ese número abriría un «se quedó a medias» sobre un
        // directorio que no es.
        if epoca != self.epoca_conexion {
            return Vec::new();
        }
        match cual {
            Informe::Lote(r) => self.informe_de_lote(task_id, r),
            Informe::Undo(r) => self.informe_de_undo(task_id, r),
            Informe::Empaquetado(r) => self.informe_de_empaquetado(r),
        }
    }

    /// El informe de un empaquetado llegó (#250).
    ///
    /// **Un informe limpio no dice nada, y eso es el diseño**: la respuesta
    /// corriente es que el archivo viaja entero, y avisar de ello enseñaría a
    /// no leer el aviso que sí importa. Un error tampoco se pinta: contra un
    /// daemon N-1 el método no existe, y «no se pudo preguntar» no es un
    /// hallazgo sobre el archivo.
    pub(super) fn informe_de_empaquetado(
        &mut self,
        res: &Result<norte_proto::methods::ArchivePackReportResult, Error>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Ok(informe) = res else {
            return Vec::new();
        };
        if informe.risky.is_empty() {
            return Vec::new();
        }
        // Recortado dice «al menos»: la lista se corta en
        // `ARCHIVE_PACK_REPORT_MAX`, y pintar el tope como si fuera el total es
        // la mentira que `truncated` existe para impedir.
        let clave = if informe.truncated {
            "msg-pack-warnings-partial"
        } else {
            "msg-pack-warnings"
        };
        let texto = norte_i18n::ta_in(
            self.lang,
            clave,
            &[("risky", &informe.risky.len().to_string())],
        );
        self.status.message = Some(clamp_display(texto));
        let cambio = ViewChange::Status(self.status.clone());
        vec![self.parche(vec![cambio])]
    }

    /// El informe de un undo llegó: al tablero, y delante si algo no volvió.
    ///
    /// Misma forma que [`Self::informe_de_lote`] porque es la misma pregunta
    /// —qué quedó sin deshacer— hecha sobre otra clase de Task.
    pub(super) fn informe_de_undo(
        &mut self,
        task_id: u64,
        resultado: &Result<norte_proto::methods::PolicyUndoReportResult, Error>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // Una fila que ya no está NO tira el informe: el tablero está acotado
        // y la task pudo caerse mientras el informe volaba, pero lo que se
        // perdía así era justo el «se quedó a medias», que jamás se doblega
        // dentro de «fue bien». Sin fila se salta el detalle y se enseña
        // igual lo que haya que decir.
        let fallo_la_task = self.tasks.get(&task_id).is_some_and(|viva| {
            matches!(
                viva.vista.state,
                crate::dto::TaskStateView::Failed | crate::dto::TaskStateView::Cancelled
            )
        });
        let (detalle, cuerpo) = match resultado {
            Ok(r) => (
                norte_i18n::ta_in(self.lang, "task-undo-done", &[("n", &r.undone.to_string())]),
                self.cuerpo_de_undo(r),
            ),
            Err(e) => {
                let clave = if matches!(e, Error::Unsupported) {
                    "modal-undo-unsupported"
                } else {
                    "modal-undo-report-failed"
                };
                (
                    norte_i18n::t_in(self.lang, "task-undo-unverified"),
                    vec![crate::dto::DialogLine {
                        text: clamp_display(norte_i18n::t_in(self.lang, clave)),
                        hostile: false,
                    }],
                )
            }
        };
        if let Some(t) = self.tasks.get_mut(&task_id) {
            t.vista.detail = Some(clamp_display(detalle));
            t.vista.detail_hostile = false;
        }
        let mut cambios = vec![ViewChange::Tasks {
            tasks: self.vistas_de_tasks(),
            cursor: self.cursor_del_tablero(),
        }];
        let hay_que_decirlo = match resultado {
            Ok(r) => !Self::undo_limpio(r),
            Err(_) => fallo_la_task,
        };
        let mut caidos = Vec::new();
        if hay_que_decirlo {
            let (cambio, cayeron) =
                self.abrir_informe("modal-undo-report-title".to_owned(), cuerpo);
            cambios.push(cambio);
            caidos = cayeron;
        }
        let mut salidas = vec![self.parche(cambios)];
        salidas.extend(caidos);
        salidas
    }

    /// `true` si el undo devolvió TODO lo que tocaba.
    ///
    /// Lo saltado cuenta como no-limpio: una entrada irreversible o una
    /// creación que se queda porque el destino no tiene papelera son cosas
    /// que NO volvieron, y un informe que las callara diría que el árbol
    /// está como estaba.
    pub(super) fn undo_limpio(r: &norte_proto::methods::PolicyUndoReportResult) -> bool {
        r.blocked.is_none()
            && r.batch_stuck.is_none()
            && r.compensations_lost == 0
            && r.denied_total == 0
            && r.skipped_irreversible == 0
            && r.skipped_created_no_trash == 0
    }

    /// El cuerpo del informe de un undo: qué volvió y qué no.
    pub(super) fn cuerpo_de_undo(
        &self,
        r: &norte_proto::methods::PolicyUndoReportResult,
    ) -> Vec<crate::dto::DialogLine> {
        let linea = |texto: String| crate::dto::DialogLine {
            text: clamp_display(texto),
            hostile: false,
        };
        let mut cuerpo = vec![linea(norte_i18n::ta_in(
            self.lang,
            "modal-undo-summary",
            &[
                ("undone", &r.undone.to_string()),
                ("skipped", &r.skipped_irreversible.to_string()),
            ],
        ))];
        if r.skipped_created_no_trash > 0 {
            cuerpo.push(linea(norte_i18n::ta_in(
                self.lang,
                "modal-undo-left-in-place",
                &[("n", &r.skipped_created_no_trash.to_string())],
            )));
        }
        if let Some(b) = &r.blocked {
            // El `seq` es una referencia OPACA: sirve para CITAR la entrada
            // contra el journal del server, no para interpretarla aquí.
            cuerpo.push(linea(norte_i18n::ta_in(
                self.lang,
                "modal-undo-blocked",
                &[
                    ("seq", &b.seq.to_string()),
                    (
                        "error",
                        &norte_i18n::t_in(self.lang, norte_frontend::error::error_key(&b.error)),
                    ),
                ],
            )));
        }
        if let Some(paso) = &r.batch_stuck {
            cuerpo.push(linea(norte_i18n::t_in(self.lang, "modal-undo-batch-stuck")));
            cuerpo.push(Self::linea_de_ruta(&paso.to));
        }
        if r.compensations_lost > 0 {
            cuerpo.push(linea(norte_i18n::ta_in(
                self.lang,
                "modal-batch-compensations-lost",
                &[("n", &r.compensations_lost.to_string())],
            )));
        }
        if r.denied_total > 0 {
            cuerpo.push(linea(norte_i18n::ta_in(
                self.lang,
                "modal-undo-denied",
                &[("n", &r.denied_total.to_string())],
            )));
        }
        cuerpo
    }

    /// El informe llegó: se apunta en el tablero y, si el lote dejó algo a
    /// medias, se dice DELANTE.
    ///
    /// Dos superficies y no una: la fila del tablero se queda con el resumen
    /// —sobrevive a que alguien cierre lo que sea—, y el diálogo es lo que
    /// hace que un directorio medio renombrado no pase inadvertido. Un lote
    /// limpio no abre nada: no hay nada que buscar.
    pub(super) fn informe_de_lote(
        &mut self,
        task_id: u64,
        resultado: &Result<norte_proto::methods::FsRenameBatchReportResult, Error>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // Ver [`Self::informe_de_undo`]: sin fila, el informe se enseña
        // igual. Lo que no se hace es inventarse una fila para colgarlo.
        let fallo_la_task = self.tasks.get(&task_id).is_some_and(|viva| {
            matches!(
                viva.vista.state,
                crate::dto::TaskStateView::Failed | crate::dto::TaskStateView::Cancelled
            )
        });
        let (detalle, cuerpo) = match resultado {
            Ok(r) => (Self::detalle_de_lote(self.lang, r), self.cuerpo_de_lote(r)),
            Err(e) => {
                let clave = if matches!(e, Error::Unsupported) {
                    "modal-batch-unsupported"
                } else {
                    "modal-batch-report-failed"
                };
                (
                    norte_i18n::t_in(self.lang, "task-batch-unverified"),
                    vec![crate::dto::DialogLine {
                        text: clamp_display(norte_i18n::t_in(self.lang, clave)),
                        hostile: false,
                    }],
                )
            }
        };
        // El detalle de la fila es «qué va por dentro» mientras corre; ya
        // terminada, lo que importa es en qué quedó. No hay más progreso
        // detrás que lo pise: el estado es terminal.
        if let Some(t) = self.tasks.get_mut(&task_id) {
            t.vista.detail = Some(clamp_display(detalle));
            t.vista.detail_hostile = false;
        }
        let mut cambios = vec![ViewChange::Tasks {
            tasks: self.vistas_de_tasks(),
            cursor: self.cursor_del_tablero(),
        }];
        // Se abre por lo que el INFORME dice, no por cómo terminó la Task:
        // un lote `Completed` con un paso atascado es exactamente el caso
        // que el desenlace de la Task no cuenta.
        let hay_que_decirlo = match resultado {
            Ok(r) => !Self::lote_limpio(r),
            // Un informe que no se pudo pedir sobre un lote que además falló
            // deja el directorio sin explicación: eso se dice delante. Si el
            // lote terminó bien, la fila del tablero basta.
            Err(_) => fallo_la_task,
        };
        let mut caidos = Vec::new();
        if hay_que_decirlo {
            let (cambio, cayeron) =
                self.abrir_informe("modal-batch-report-title".to_owned(), cuerpo);
            cambios.push(cambio);
            caidos = cayeron;
        }
        let mut salidas = vec![self.parche(cambios)];
        salidas.extend(caidos);
        salidas
    }

    /// `true` si el lote no dejó nada que buscar ni que rematar.
    pub(super) fn lote_limpio(r: &norte_proto::methods::FsRenameBatchReportResult) -> bool {
        r.stuck.is_none()
            && r.uncertain.is_none()
            && r.failed_pair.is_none()
            && r.compensations_lost == 0
            && r.rolled_back == 0
    }

    /// El resumen de una línea que se queda en la fila del tablero.
    pub(super) fn detalle_de_lote(
        lang: norte_i18n::Lang,
        r: &norte_proto::methods::FsRenameBatchReportResult,
    ) -> String {
        if Self::lote_limpio(r) {
            return norte_i18n::ta_in(lang, "task-batch-applied", &[("n", &r.applied.to_string())]);
        }
        norte_i18n::ta_in(
            lang,
            "task-batch-half",
            &[
                ("applied", &r.applied.to_string()),
                ("back", &r.rolled_back.to_string()),
            ],
        )
    }

    /// El cuerpo del informe: qué se aplicó, qué no se pudo devolver, y CÓMO
    /// SE LLAMA AHORA lo que se quedó a medias.
    ///
    /// El nombre de ahora es lo único accionable que hay aquí, así que va
    /// como línea de ruta —enmascarada y marcada— y no dentro de una frase:
    /// una ruta metida en una frase la puede suplantar otra ruta.
    pub(super) fn cuerpo_de_lote(
        &self,
        r: &norte_proto::methods::FsRenameBatchReportResult,
    ) -> Vec<crate::dto::DialogLine> {
        let frase = |clave: &str| crate::dto::DialogLine {
            text: clamp_display(norte_i18n::t_in(self.lang, clave)),
            hostile: false,
        };
        let mut cuerpo = vec![crate::dto::DialogLine {
            text: clamp_display(norte_i18n::ta_in(
                self.lang,
                "modal-batch-summary",
                &[
                    ("applied", &r.applied.to_string()),
                    ("back", &r.rolled_back.to_string()),
                ],
            )),
            hostile: false,
        }];
        if let Some(paso) = &r.stuck {
            cuerpo.push(frase("modal-batch-stuck"));
            cuerpo.push(Self::linea_de_ruta(&paso.to));
            cuerpo.push(frase(if paso.journalled {
                "modal-batch-stuck-journalled"
            } else {
                "modal-batch-stuck-unjournalled"
            }));
        }
        if let Some(paso) = &r.uncertain {
            cuerpo.push(frase("modal-batch-uncertain"));
            cuerpo.push(Self::linea_de_ruta(&paso.to));
        }
        if r.compensations_lost > 0 {
            cuerpo.push(crate::dto::DialogLine {
                text: clamp_display(norte_i18n::ta_in(
                    self.lang,
                    "modal-batch-compensations-lost",
                    &[("n", &r.compensations_lost.to_string())],
                )),
                hostile: false,
            });
        }
        cuerpo
    }

    /// Apila un diálogo, con techo.
    ///
    /// El techo existe porque la pila la alimenta el WIRE desde la tarea 5.3
    /// (aprobaciones e informes, también de tasks ajenas). Se cae el más
    /// viejo SIN reconocer —lo que nadie ha llegado a mirar— y nunca el de
    /// arriba, que es el que se está contestando; si todos están reconocidos,
    /// el más viejo. Que se cayó alguno se DICE: una pregunta que desaparece
    /// en silencio es peor que una pila larga.
    pub(super) fn apilar_dialogo(&mut self, dialogo: Dialogo) -> Vec<BridgeEnvelope<UiUpdate>> {
        let mut fuera = Vec::new();
        if self.dialogos.len() >= MAX_DIALOGS {
            // Se sacrifica un INFORME antes que una decisión: el informe
            // también vive en la fila del tablero, y una aprobación que
            // desaparece deja a un agente esperando. Si solo quedan
            // decisiones, cae la más vieja — a esa el daemon le acabará
            // aplicando su TTL, que es una denegación.
            let victima = self
                .dialogos
                .iter()
                .position(|d| d.al_confirmar.is_none())
                .or_else(|| self.dialogos.iter().position(|d| !d.reconocido))
                .unwrap_or(0);
            self.dialogos.remove(victima);
            fuera.extend(self.decir("msg-dialog-dropped"));
        }
        self.dialogos.push(dialogo);
        fuera
    }

    /// Abre el diálogo de un informe. Solo informa: no tiene nada que
    /// ejecutar, y su única respuesta lo cierra.
    pub(super) fn abrir_informe(
        &mut self,
        title_key: String,
        cuerpo: Vec<crate::dto::DialogLine>,
    ) -> (ViewChange, Vec<BridgeEnvelope<UiUpdate>>) {
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id,
            title_key,
            destination: None,
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body: cuerpo,
            overflow_note: String::new(),
            choices: vec![DialogChoice {
                id: "ok".to_owned(),
                label_key: "dialog-ok".to_owned(),
                destructive: false,
            }],
            input: None,
            input_hostile: false,
            input_secret: false,
            dest_check: crate::dto::DestCheckView::NotAsked,
        };
        let caidos = self.apilar_dialogo(Dialogo {
            id,
            vista,
            tecleado: Tecleado::Texto(String::new()),
            // Se abre SOLO, cuando el daemon contesta.
            reconocido: false,
            al_confirmar: None,
        });
        (
            ViewChange::Dialogs {
                dialogs: self.vistas_de_dialogos(),
            },
            caidos,
        )
    }

    /// `task.cancel`: le pide parar a UNA task, y dice a cuál o que no hay.
    ///
    /// Qué task es depende de dónde está el foco, y no por gusto: con el
    /// panel de procesos delante, el tablero pinta un cursor, y una tecla que
    /// cancelara otra cosa dejaría ese cursor pintando una selección que no
    /// manda. Sin ese panel enfocado se cancela la ÚLTIMA viva, que es lo que
    /// hace el TUI con la misma tecla.
    pub(super) fn cancelar_por_comando(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match self.task_a_cancelar() {
            Objetivo::Ninguna => (self.aplicada(), self.decir("msg-no-tasks")),
            Objetivo::Terminada => (self.aplicada(), self.decir("msg-task-finished")),
            Objetivo::Viva(id) => {
                // Una ventana sin efectos no aborta la task de OTRO cliente:
                // cancelar una copia deja el destino limpio o un
                // `.norte-partial`, o sea que toca el disco. Las propias sí,
                // que para lanzarlas ya hacía falta el interruptor.
                if self.efectos == crate::commands::Efectos::SoloLectura
                    && self.tasks.get(&id).is_some_and(|t| t.vista.foreign)
                {
                    return Self::no_muta();
                }
                let (ack, mut fuera) = self.cancelar(id);
                fuera.extend(self.decir("msg-cancelling"));
                (ack, fuera)
            }
        }
    }

    /// Mueve la fila elegida del tablero.
    ///
    /// Sin necesitar el foco del panel de procesos: el tablero se pinta
    /// también cuando ese hueco no existe —las tasks salen en el sobre— y un
    /// comando que solo funcionara con un hueco concreto abierto sería una
    /// tecla que depende de la disposición.
    pub(super) fn mover_en_tablero(
        &mut self,
        atras: bool,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let ids = self.ids_del_tablero();
        if ids.is_empty() {
            return (self.aplicada(), self.decir("msg-no-tasks"));
        }
        if atras {
            self.cursor_procesos.up(&ids);
        } else {
            self.cursor_procesos.down(&ids);
        }
        // FOTO y no parche. Desde el puente 57 el cursor sí tiene por dónde
        // viajar (`ViewChange::Tasks`), así que esto ya no es «no hay
        // contrato»: es que una tecla que solo mueve la elección no necesita
        // reenviar el tablero entero, y la foto es lo que este camino lleva
        // haciendo sin queja. Cambiarlo es una optimización, no un arreglo.
        let snap = self.snapshot();
        (
            self.aplicada(),
            vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))],
        )
    }

    /// Quita del tablero la fila elegida, si YA terminó.
    ///
    /// Una viva no se descarta: pararla es `task.cancel`, y quitar de la
    /// vista algo que sigue escribiendo en el disco es perder de vista
    /// justo lo que hay que mirar.
    pub(super) fn descartar_task(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let ids = self.ids_del_tablero();
        if ids.is_empty() {
            return (self.aplicada(), self.decir("msg-no-tasks"));
        }
        let i = self.cursor_procesos.fila_o_cero(&ids);
        let Some((&id, viva)) = self.tasks_visibles().nth(i) else {
            return (self.aplicada(), self.decir("msg-no-tasks"));
        };
        if !Self::terminal(viva.vista.state) {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-task-running".to_owned(),
                },
                self.decir("host-task-running"),
            );
        }
        self.tasks.remove(&id);
        self.undos_sin_task(id);
        // El cursor NO se re-acota aquí, y antes sí: la regla del tipo
        // compartido es leer la fila por la IDENTIDAD de la elegida y caer a
        // la posición recordada solo si esa ya no está. Descartar la última
        // deja la selección en la que ahora es la última —igual que antes— y,
        // a diferencia de antes, si el tablero vuelve a crecer la elección
        // vuelve a donde estaba en vez de haberse quedado pegada.
        let mut fuera = vec![self.parche(vec![ViewChange::Tasks {
            tasks: self.vistas_de_tasks(),
            cursor: self.cursor_del_tablero(),
        }])];
        let snap = self.snapshot();
        fuera.push(self.sobre(UiUpdate::Snapshot(Box::new(snap))));
        (self.aplicada(), fuera)
    }

    /// Suelta el «deshaciendo» de una sesión cuya task se descarta.
    ///
    /// Descartar la fila de un undo que terminó es lo mismo que verlo
    /// terminar: si no se soltara aquí, esa sesión se quedaría marcada como
    /// «deshaciendo» para siempre y `u` sobre ella se rehusaría sin motivo.
    pub(super) fn undos_sin_task(&mut self, task_id: u64) {
        if let Some(sesion) = self.agencia.undos.remove(&task_id) {
            self.agencia.sesiones.deshecha(&sesion);
        }
    }

    /// A qué task le toca parar.
    pub(super) fn task_a_cancelar(&self) -> Objetivo {
        if self.procesos_tienen_el_foco() {
            // La del cursor, sea cual sea su estado: la eligió un humano
            // mirándola. Si ya terminó se DICE, en vez de saltar a otra —
            // cancelar una task que no es la señalada es peor que no
            // cancelar nada.
            let Some((id, viva)) = self
                .tasks_visibles()
                .nth(self.cursor_procesos.fila_o_cero(&self.ids_del_tablero()))
            else {
                return Objetivo::Ninguna;
            };
            return if Self::sigue_viva(viva) {
                Objetivo::Viva(*id)
            } else {
                Objetivo::Terminada
            };
        }
        // El tablero va por id, y el daemon los reparte crecientes: la última
        // viva es la de id mayor.
        self.tasks
            .iter()
            .rev()
            .find(|(_, t)| Self::sigue_viva(t))
            .map_or(Objetivo::Ninguna, |(id, _)| Objetivo::Viva(*id))
    }

    /// `true` si esta clase de task ESCRIBE.
    ///
    /// Por la clave del catálogo y no por `TaskKind`, que es no exhaustivo:
    /// una clase de un daemon más nuevo cae en `unknown` y NO cuenta como
    /// mutación, que es el lado seguro — apagar el aviso del journal por algo
    /// que este host no sabe qué hace sería apagarlo por si acaso.
    pub(super) fn muta(clase: &str) -> bool {
        matches!(
            clase,
            "copy"
                | "move"
                | "delete"
                | "mkdir"
                | "rename-batch"
                | "undo"
                | "pack"
                | "sync"
                // #314: cambiar permisos MUTA, con journal y reversa.
                | "set-mode"
        )
    }

    /// `true` si a esta task todavía se le puede pedir que pare.
    ///
    /// Pregunta al progreso EN VIVO y no a la vista proyectada: entre que el
    /// daemon marca el desenlace y el `Mensaje::Progreso` sale del buzón, la
    /// vista dice que sigue corriendo. Sobre esa foto se contestaba
    /// «cancelando…» a algo ya terminado y se elegía como «última viva» a una
    /// que ya no lo era, dejando corriendo la que de verdad quedaba.
    pub(super) fn sigue_viva(t: &TaskViva) -> bool {
        !t.progreso.borrow().state.is_terminal()
    }

    /// `true` si el foco está en el panel de procesos.
    pub(super) fn procesos_tienen_el_foco(&self) -> bool {
        self.roles
            .get(RoleId::Active)
            .and_then(|s| kind_de(&self.arbol, s))
            .is_some_and(|k| k.as_str() == "processes")
    }

    /// Dice una frase: en la barra de estado Y como aviso.
    ///
    /// Las dos cosas y por la misma llamada, que es lo que ya hace una task
    /// fallida: la barra es donde se lee al mirar, y el aviso es lo que el
    /// renderer puede anunciar a un lector de pantalla.
    pub(super) fn decir_con(
        &mut self,
        clave: &str,
        args: &[(&str, &str)],
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        self.status.message = Some(clamp_display(norte_i18n::ta_in(self.lang, clave, args)));
        let parche = self.parche(vec![ViewChange::Status(self.status.clone())]);
        let aviso = self.sobre(UiUpdate::Notice(UiNotice::Message {
            key: clave.to_owned(),
            detail: None,
        }));
        vec![parche, aviso]
    }

    /// Como [`Self::decir_con`], sin argumentos.
    pub(super) fn decir(&mut self, clave: &str) -> Vec<BridgeEnvelope<UiUpdate>> {
        self.status.message = Some(clamp_display(norte_i18n::t_in(self.lang, clave)));
        let parche = self.parche(vec![ViewChange::Status(self.status.clone())]);
        let aviso = self.sobre(UiUpdate::Notice(UiNotice::Message {
            key: clave.to_owned(),
            detail: None,
        }));
        vec![parche, aviso]
    }

    /// Pide la cancelación de una task. Idempotente por contrato: pedirla dos
    /// veces no es un error ni cambia nada.
    pub(super) fn cancelar(&mut self, task_id: u64) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(viva) = self.tasks.get(&task_id) else {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        };
        (viva.cancel)();
        (self.aplicada(), Vec::new())
    }

    /// El tablero que cruza el puente, acotado a [`MAX_TASKS`].
    ///
    /// El desalojo de `registrar_task` solo puede tirar tasks TERMINADAS, así
    /// que un lote más grande que el tope —marcar tres mil ficheros y pulsar
    /// F5 es el flujo normal— no tiene nada que desalojar y el mapa crece por
    /// encima del tope que el contrato del puente promete. Se acota aquí, que
    /// es donde el número significa algo: cuántas filas viajan.
    ///
    /// Se quedan las MÁS NUEVAS (el mapa está ordenado por id, que es
    /// monótono): lo que interesa de un lote en marcha es su frente, no las
    /// primeras que se encolaron.
    pub(super) fn vistas_de_tasks(&self) -> Vec<TaskView> {
        self.tasks_visibles()
            .map(|(_, t)| t.vista.clone())
            .collect()
    }

    /// Las tasks que CRUZAN el puente, en el orden en que se pintan.
    ///
    /// UNA sola definición de «las visibles», y no por gusto: el tablero se
    /// recorta a [`MAX_TASKS`] y el cursor es un ÍNDICE. Mientras el recorte
    /// vivía solo aquí y el cursor contaba sobre el mapa entero, con más de
    /// 256 tasks —marcar tres mil ficheros y pulsar F5 es el flujo normal, y
    /// el desalojo solo se lleva las TERMINADAS— la fila resaltada y la task
    /// que se cancelaba eran dos tasks distintas. Es literalmente lo que el
    /// rustdoc del panel prohíbe: «dos listas de tareas se separan, y la que
    /// se ve deja de ser la que se cancela».
    pub(super) fn tasks_visibles(&self) -> impl Iterator<Item = (&u64, &TaskViva)> {
        let sobran = self.tasks.len().saturating_sub(MAX_TASKS);
        self.tasks.iter().skip(sobran)
    }

    /// Cuántas filas tiene el tablero PINTADO.
    pub(super) fn filas_de_tablero(&self) -> usize {
        self.tasks.len().min(MAX_TASKS)
    }

    /// Los ids de las tasks PINTADAS, en el orden en que se pintan.
    ///
    /// Es lo que el cursor del panel necesita: guarda la IDENTIDAD de la
    /// elegida, no su posición, porque el tablero se mueve solo y una fila que
    /// caduca por encima haría que la misma posición nombrara otra tarea.
    pub(super) fn ids_del_tablero(&self) -> Vec<u64> {
        self.tasks_visibles().map(|(id, _)| *id).collect()
    }

    /// Qué fila del tablero está elegida, sobre las filas PINTADAS.
    ///
    /// Índice sobre lo que el renderer resalta, no sobre el mapa entero: con
    /// el tablero recortado por el tope señalaba a otra. `None` con cero
    /// filas, porque un índice sin fila detrás resalta la nada.
    pub(super) fn cursor_del_tablero(&self) -> Option<u64> {
        self.cursor_procesos
            .fila(&self.ids_del_tablero())
            .and_then(|i| u64::try_from(i).ok())
    }

    pub(super) fn terminal(estado: TaskStateView) -> bool {
        matches!(
            estado,
            TaskStateView::Done | TaskStateView::Failed | TaskStateView::Cancelled
        )
    }
}
