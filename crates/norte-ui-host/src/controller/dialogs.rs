//! La pila de diálogos: teclear en uno y responderlo.
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
    /// Teclea en el campo de un diálogo.
    ///
    /// El renderer manda el texto ENTERO tras la edición y no un delta: el
    /// caret es suyo, y reconstruirlo en Rust sería mantener dos ideas de
    /// dónde está el cursor.
    pub(super) fn escribir_en_dialogo(
        &mut self,
        id: ModalId,
        texto: &str,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(dialogo) = self.dialogos.iter_mut().find(|d| d.id == id) else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        if dialogo.vista.input.is_none() {
            // Un diálogo de decisión no tiene dónde escribir, y aceptar texto
            // que nadie va a leer sería peor que decirlo.
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        }
        if !matches!(dialogo.tecleado, Tecleado::Texto(_)) {
            // Un campo de CONTRASEÑA no se teclea por aquí (#327): el host no
            // guarda lo que se escribe, y la contraseña cruza una sola vez con
            // la respuesta. Un renderer que lo mande igual está metiendo
            // material secreto por el camino de un nombre de fichero, así que
            // se descarta ANTES de tocarlo — sin guardarlo, sin proyectarlo y
            // sin contestar con una frase que hable de «nombres».
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        }
        if texto.len() > MAX_NOMBRE {
            // Ni se recorta ni se acepta a medias: un nombre no es una
            // cadena de pantalla, y recortarlo es inventarse otro.
            return (
                ActionAck::Unavailable {
                    reason_key: "host-name-too-long".to_owned(),
                },
                Vec::new(),
            );
        }
        let Tecleado::Texto(crudo) = &mut dialogo.tecleado else {
            // Imposible: lo filtra el guard de arriba, antes de mirar nada.
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        texto.clone_into(crudo);
        // Lo que se PINTA es otra cosa: enmascarado (un `U+202E` en el
        // nombre que te van a pedir aprobar se ve) y acotado.
        let (pintable, hostil) = norte_frontend::display_name(texto.as_bytes());
        dialogo.vista.input = Some(clamp_display(pintable));
        dialogo.vista.input_hostile = hostil;
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// El lector quiere cerrar: se pregunta, o se cierra.
    ///
    /// La decisión de SI hay que preguntar es la compartida
    /// (`settings::quit_needs_confirm`), la misma que usa el terminal; lo que
    /// cada frontend calcula por su cuenta es qué cuenta como «queda trabajo».
    /// Aquí es que haya alguna task VIVA en el tablero: lo que se perdería al
    /// cerrar es una copia a medias, no una marca.
    ///
    /// Cerrar no preguntaba nunca. El rustdoc de `quit_needs_confirm` ya
    /// nombraba un `confirm_quit_should_open` de la ventana que no existía.
    pub(super) fn pedir_salir(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let hay_trabajo = self
            .tasks
            .values()
            .any(|t| t.vista.state == crate::dto::TaskStateView::Running);
        if !norte_frontend::settings::quit_needs_confirm(
            self.config.common.ui_confirm_quit,
            hay_trabajo,
        ) {
            self.nativo(crate::dto::NativeEffect::CloseWindow);
            return (self.aplicada(), Vec::new());
        }
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        // El cuerpo DICE cuánto hay en marcha cuando lo hay: «¿seguro?» a
        // secas no es una pregunta que se pueda contestar.
        let cuerpo = if hay_trabajo {
            let n = self
                .tasks
                .values()
                .filter(|t| t.vista.state == crate::dto::TaskStateView::Running)
                .count();
            vec![crate::dto::DialogLine {
                text: clamp_display(norte_i18n::ta_in(
                    self.lang,
                    "modal-quit-pending",
                    &[("n", &n.to_string())],
                )),
                hostile: false,
            }]
        } else {
            Vec::new()
        };
        self.dialogos.push(Dialogo {
            id,
            reconocido: true,
            vista: DialogView {
                id,
                title_key: "modal-quit-title".to_owned(),
                destination: None,
                subject: None,
                asker: None,
                deadline: None,
                deadline_at_ms: None,
                body: cuerpo,
                overflow_note: String::new(),
                overflow_hostile: false,
                choices: vec![
                    DialogChoice {
                        id: "confirm".to_owned(),
                        label_key: "dialog-quit".to_owned(),
                        // Cerrar con trabajo en marcha PIERDE ese trabajo: el
                        // botón lo dice con su forma, como el de borrar.
                        destructive: hay_trabajo,
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
            },
            tecleado: Tecleado::Texto(String::new()),
            al_confirmar: Some(Pendiente::Salir),
        });
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Responde a un diálogo.
    ///
    /// Un id que no es el del diálogo abierto —porque ya se contestó, porque
    /// el renderer tardó— no hace nada y lo dice: confirmar dos veces NO
    /// borra dos veces.
    /// Lo que la respuesta AFIRMATIVA de un diálogo pone en marcha.
    ///
    /// Separado de [`Self::responder_dialogo`], que se queda con lo que es
    /// igual para todos: que el id sea el del diálogo abierto, que la
    /// respuesta esté entre las que se ofrecieron, la cerradura de solo
    /// lectura y el cierre. Aquí solo vive lo que cada pendiente hace.
    #[expect(
        clippy::too_many_lines,
        reason = "despachador exhaustivo: un brazo por pendiente, sin lógica dentro"
    )]
    pub(super) fn ejecutar_pendiente(
        &mut self,
        dialogo: Dialogo,
        secreto: Option<&str>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (Option<&'static str>, Vec<BridgeEnvelope<UiUpdate>>) {
        let mut salidas = Vec::new();
        // El motivo por el que la respuesta NO hizo nada, si lo hubo: viaja al
        // acuse en vez de quedarse solo en la barra.
        let mut rehusado: Option<&'static str> = None;
        match dialogo.al_confirmar {
            Some(Pendiente::Borrar { paths, permanente }) => {
                Self::lanzar_borrado(paths, permanente, backend, buzon);
            }
            Some(Pendiente::InstruccionIa { dir }) => {
                let instruccion = dialogo.tecleado.texto().to_owned();
                salidas.extend(self.lanzar_plan_ia(dir, instruccion, backend, buzon));
            }
            Some(Pendiente::PlantillaLote { dir, nombres }) => {
                let plantilla = dialogo.tecleado.texto().to_owned();
                salidas.extend(
                    self.lanzar_plan_de_plantilla(dir, &nombres, &plantilla, backend, buzon),
                );
            }
            Some(Pendiente::Salir) => {
                // Ya se preguntó y la respuesta fue que sí: quien hospeda
                // vuelca la sesión y destruye la ventana.
                self.nativo(crate::dto::NativeEffect::CloseWindow);
            }
            Some(Pendiente::ConsultaSemantica) => {
                let consulta = dialogo.tecleado.texto().to_owned();
                salidas.extend(self.lanzar_semantica(consulta, backend, buzon));
            }
            Some(Pendiente::EntregarSecreto { conn, slot, dir }) => {
                // El campo vacío no llega aquí: `responder_dialogo` deja el
                // confirmar INERTE mientras no haya nada, y ahí es donde tiene
                // que estar —a esta altura el diálogo ya se ha ido de la pila,
                // y «no cerrar» ya no es una opción—. Lo que sí se comprueba
                // es la FORMA: sin ella, un error de cableado entregaría el
                // texto de un campo normal como si fuera una contraseña.
                let (Tecleado::Secreto, Some(secreto)) = (&dialogo.tecleado, secreto) else {
                    rehusado = Some("host-secret-empty");
                    return (rehusado, salidas);
                };
                // Se envuelve NADA MÁS llegar: a partir de aquí la copia del
                // host se pisa con ceros cuando la task acaba, en vez de
                // quedarse en el heap hasta que alguien reutilice el bloque.
                let mut secreto_seguro = norte_frontend::secret::TypedSecret::default();
                secreto_seguro.set(secreto);
                Self::lanzar_secreto(conn, secreto_seguro, slot, dir, backend, buzon);
            }
            Some(Pendiente::Renombrar { from, siembra }) => {
                let (motivo, partes) = self.confirmar_rename(
                    &from,
                    &siembra,
                    dialogo.tecleado.texto(),
                    backend,
                    buzon,
                );
                rehusado = motivo;
                salidas.extend(partes);
            }
            Some(Pendiente::Transferir {
                origen,
                origen_dir,
                paths,
                destino,
                mover,
            }) => {
                self.enviar_lote(&paths, &origen_dir, &destino, mover, backend, buzon);
                // Las marcas las CONSUME el envío, no el desenlace (mismo
                // criterio que el TUI y que mc): una selección a medio
                // consumir significaría cosas distintas según qué task de
                // las N terminó.
                //
                // Y las del hueco de ORIGEN, no las del que tenga el foco
                // ahora: `FocusSlot` no está vedada mientras hay un
                // diálogo abierto, así que un clic en el otro panel entre
                // la pregunta y la respuesta borraba las marcas del panel
                // equivocado y dejaba intactas las que se acababan de
                // enviar — y el lector volvía a pulsar F5 sobre lo mismo.
                if let Some(h) = self.huecos.get_mut(&origen) {
                    h.pane.clear_marks();
                }
                salidas.push(self.parche_filas());
            }
            Some(Pendiente::Soltar { paths, destino }) => {
                // `origen_dir` es el DESTINO a propósito: solo se usa para
                // apuntar qué directorios quedan desactualizados cuando se
                // mueve, y aquí nunca se mueve. Lo de donde salió es de otro
                // proceso y esta ventana no lo lista.
                //
                // Y NO se tocan las marcas: las de este panel las puso el
                // lector para otra cosa, y lo que se copia no salió de ahí.
                self.enviar_lote(&paths, &destino, &destino, false, backend, buzon);
                salidas.push(self.parche_filas());
            }
            Some(Pendiente::Buscar { root }) => {
                let patron = dialogo.tecleado.texto().to_owned();
                if patron.is_empty() {
                    // Un patrón vacío casaría el árbol entero: no es una
                    // búsqueda, es un listado recursivo, y se dice en vez
                    // de lanzarlo.
                    self.status.message = Some(clamp_display(norte_i18n::t_in(
                        self.lang,
                        "err-empty-pattern",
                    )));
                    let cambio = ViewChange::Status(self.status.clone());
                    salidas.push(self.parche(vec![cambio]));
                } else {
                    salidas.extend(self.lanzar_busqueda(root, patron, backend, buzon));
                }
            }
            // Los dos que crean un nodo VACÍO a partir de un nombre tecleado.
            // Juntos porque son la misma forma —validar el segmento, encolar,
            // apuntar el directorio a refrescar— y este `match` es un
            // despachador que ya roza su tope.
            Some(p @ (Pendiente::CrearDirectorio { .. } | Pendiente::CrearFichero { .. })) => {
                let (dir, fichero) = match p {
                    Pendiente::CrearDirectorio { dir } => (dir, false),
                    Pendiente::CrearFichero { dir } => (dir, true),
                    _ => unreachable!("el patrón de arriba solo deja esos dos"),
                };
                let (motivo, partes) = if fichero {
                    self.crear_fichero(&dir, dialogo.tecleado.texto(), backend, buzon)
                } else {
                    self.crear_directorio(&dir, dialogo.tecleado.texto(), backend, buzon)
                };
                rehusado = motivo;
                salidas.extend(partes);
            }
            // #309: el favorito. El destino lo capturó el diálogo al abrirse,
            // no se relee aquí.
            Some(Pendiente::GuardarFavorito { destino }) => {
                let (motivo, partes) =
                    self.guardar_favorito(&destino, dialogo.tecleado.texto(), buzon);
                rehusado = motivo;
                salidas.extend(partes);
            }
            // #318: el perfil. A diferencia del favorito, lo que se guarda se
            // lee AHORA: es el estado de la pantalla, no una respuesta que el
            // diálogo capturó al abrirse.
            Some(Pendiente::GuardarPerfil) => {
                let (motivo, partes) = self.guardar_perfil(dialogo.tecleado.texto(), buzon);
                rehusado = motivo;
                salidas.extend(partes);
            }
            // Los dos que fabrican ficheros a partir de lo tecleado, juntos:
            // este `match` es un despachador y ya roza su tope.
            Some(p @ (Pendiente::Partir { .. } | Pendiente::Empaquetar { .. })) => {
                let (motivo, partes) =
                    self.ejecutar_de_archivo(p, dialogo.tecleado.texto(), backend, buzon);
                rehusado = motivo;
                salidas.extend(partes);
            }
            // #311: copiar la lista de sumas. Los bytes se montaron al abrir
            // el diálogo, con el escapado de coreutils: lo que se pinta va
            // saneado, y copiar ESO daría un `SHA256SUMS` que no comprueba los
            // ficheros que nombra.
            Some(Pendiente::CopiarSumas { bytes }) => {
                // Cuántas LÍNEAS lleva: es el número que el mensaje enseña, y
                // el payload termina siempre en salto.
                let count = bytes.split(|b| *b == b'\n').count().saturating_sub(1);
                if self.nativo(crate::dto::NativeEffect::CopyBytes { bytes, count }) {
                    salidas.extend(self.decir("msg-checksum-copied"));
                } else {
                    // Nadie escucha el canal nativo: no hay portapapeles al
                    // que copiar, y decirlo es mejor que un botón que no hace
                    // nada.
                    rehusado = Some("host-no-desktop");
                }
            }
            Some(Pendiente::Permisos { targets }) => {
                let tecleado = dialogo.tecleado.texto().to_owned();
                let (motivo, partes) = self.cambiar_permisos(targets, &tecleado, backend, buzon);
                rehusado = motivo;
                salidas.extend(partes);
            }
            Some(Pendiente::Patron { marcar }) => {
                let patron = dialogo.tecleado.texto().to_owned();
                let (motivo, partes) = self.aplicar_patron(marcar, &patron);
                rehusado = motivo;
                salidas.extend(partes);
            }
            Some(Pendiente::DeshacerSesion { sesion }) => {
                let (motivo, partes) = self.deshacer_sesion(&sesion, backend, buzon);
                rehusado = motivo;
                salidas.extend(partes);
            }
            Some(Pendiente::AprobarExtension {
                id,
                capabilities,
                digest,
            }) => {
                let (motivo, partes) = self.conceder(&id, &capabilities, digest, backend, buzon);
                rehusado = motivo;
                salidas.extend(partes);
            }
            Some(Pendiente::Decidir {
                approval_id,
                session,
            }) => {
                // Y se apunta a QUIÉN se le dijo que sí desde aquí: la fila
                // del panel de agentes distingue «pidió N veces» de «se le
                // aprobaron M», que no son lo mismo cuando contestó otra
                // ventana, cuando se denegó, o cuando caducó.
                if let Some(sesion) = &session {
                    self.agencia.sesiones.aprobada(sesion);
                    if self.agencia.panel {
                        let cambio = ViewChange::Agents {
                            agents: self.vista_agentes(),
                        };
                        salidas.push(self.parche(vec![cambio]));
                    }
                }
                // Solo `approve` aprueba. Cualquier otra respuesta —y el
                // cierre del diálogo— DENIEGA: una decisión de seguridad
                // no tiene respuesta por defecto que diga «sí».
                //
                // Y si el sí NO llega, se dice. Un `policy.decide` que falla
                // —el daemon se cayó entre la pregunta y la respuesta— deja
                // la operación denegada por silencio mientras esta ventana da
                // por hecho que la autorizó: «lo dije» y «llegó» no son lo
                // mismo en una superficie de seguridad. Denegar es al revés:
                // si esa no llega, el desenlace es el mismo que se pidió.
                lanzar_aprobacion(approval_id, backend, buzon);
            }
            // Una colisión no se contesta con «confirmar»: cada salida ES una
            // política, y quien las traduce es `responder_dialogo`, que sabe
            // cuál se pulsó. Llegar aquí sería una respuesta que este diálogo
            // no ofreció, y esas no se interpretan.
            Some(Pendiente::Reintentar { .. }) | None => {}
        }
        (rehusado, salidas)
    }

    /// Un nombre TECLEADO, como `Segment`, o la clave del motivo por el que
    /// no vale.
    ///
    /// El guard del carácter de sustitución vive aquí y no solo en el rename
    /// porque el camino de VUELTA lo comparten: `escribir_en_dialogo` proyecta
    /// `clamp_display(display_name(texto))` en cada tecla, y el renderer
    /// vuelve a sembrar el campo con esa proyección si tuvo que reconstruir el
    /// nodo. Sin el guard, crear un directorio escribía en el disco el U+FFFD
    /// que había puesto la pantalla.
    pub(super) fn segmento_tecleado(nombre: &str) -> Result<norte_proto::Segment, &'static str> {
        if nombre.contains('\u{FFFD}') {
            return Err("msg-transfer-name-fffd");
        }
        norte_proto::Segment::new(nombre.as_bytes().to_vec()).map_err(|_| "err-bad-name")
    }

    pub(super) fn responder_dialogo(
        &mut self,
        id: ModalId,
        choice: &str,
        secreto: Option<&str>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(pos) = self.dialogos.iter().position(|d| d.id == id) else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        // Una respuesta que el diálogo no ofreció no se interpreta: no hay
        // respuestas implícitas en una superficie de decisión.
        if !self.dialogos[pos]
            .vista
            .choices
            .iter()
            .any(|c| c.id == choice)
        {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        }
        // La PRIMERA respuesta a un diálogo que se abrió SOLO no lo contesta:
        // solo lo reconoce. Vive AQUÍ y no en el camino de teclas porque el
        // ratón es la entrada primaria de esta superficie: el diálogo se
        // pinta en el mismo sitio que el anterior y con la misma primera
        // opción, así que un clic ya en marcha sobre «Confirmar» aterrizaba
        // sobre el «Aprobar» de una aprobación de agente recién llegada.
        //
        // Las respuestas que DENIEGAN están exentas por el mismo motivo que
        // `Escape`: quitarse de encima algo que uno no ha pedido tiene que
        // salir a la primera, y denegar es el desenlace seguro.
        if !self.dialogos[pos].reconocido && choice != "deny" && choice != "cancel" {
            self.dialogos[pos].reconocido = true;
            self.status.message = Some(clamp_display(norte_i18n::t_in(
                self.lang,
                "host-dialog-acknowledge",
            )));
            return (
                self.aplicada(),
                vec![self.parche(vec![ViewChange::Status(self.status.clone())])],
            );
        }
        if let Some(rechazo) = self.rechaza_por_solo_lectura(pos) {
            return rechazo;
        }
        // Confirmar con el campo de contraseña VACÍO es inerte: no entrega y
        // no cierra (#327). Entregar la cadena vacía reproduce #320 —un
        // secreto vacío hace que la conexión autentique con la cadena ambiente,
        // o sea con una identidad que nadie pidió—, y cerrar el diálogo
        // convertiría un dedo que se adelanta en una navegación abandonada.
        //
        // Vive AQUÍ, antes del `remove`, y no dentro de `ejecutar_pendiente`:
        // allí el diálogo ya se ha ido de la pila y «no cerrar» ya no es una
        // opción. Es el mismo sitio que la TUI eligió (`ALLOW_ASK_SECRET`).
        //
        // Se juzga lo que ACABA de llegar y no un estado guardado: el host no
        // tiene ninguno, y así el campo que el lector ve vacío es exactamente
        // el que se evalúa. Con un buffer en el host los dos podían diferir
        // —un diálogo apilado encima descarta el nodo del campo, y al volver
        // se pinta vacío sobre un buffer que no lo estaba—, y entonces
        // «confirmar sobre un campo vacío no hace nada» dejaba de ser cierto
        // justo donde se había prometido.
        if choice == "confirm" && matches!(self.dialogos[pos].tecleado, Tecleado::Secreto) {
            if secreto.is_none_or(str::is_empty) {
                return (
                    ActionAck::Unavailable {
                        reason_key: "host-secret-empty".to_owned(),
                    },
                    Vec::new(),
                );
            }
            // Y una que no cabe se RECHAZA, no se recorta. Recortar era peor
            // que el tope: entregar los primeros 256 caracteres de una frase
            // de paso más larga falla la autenticación sin decir por qué, y el
            // lector no tiene forma de sospecharlo — el campo va enmascarado.
            if secreto.is_some_and(|s| s.chars().count() > SECRET_MAX_CHARS) {
                return (
                    ActionAck::Unavailable {
                        reason_key: "host-secret-too-long".to_owned(),
                    },
                    Vec::new(),
                );
            }
        }
        let dialogo = self.dialogos.remove(pos);
        let mut salidas = Vec::new();
        // `confirm` es la respuesta afirmativa de los diálogos normales;
        // `approve`, la de una aprobación. Nombres distintos a propósito: en
        // una superficie de seguridad, «confirmar» y «aprobar» no deberían
        // poder confundirse en un renderer.
        let mut rehusado = None;
        if choice == "confirm" || choice == "approve" {
            let (motivo, partes) = self.ejecutar_pendiente(dialogo, secreto, backend, buzon);
            rehusado = motivo;
            salidas.extend(partes);
        } else if let Some(Pendiente::Decidir { approval_id, .. }) = dialogo.al_confirmar {
            // Denegar explícitamente, y también al cerrar: dejar al agente
            // esperando una respuesta que no llega es peor que decirle que no.
            let backend = Arc::clone(backend);
            tokio::spawn(async move {
                let _ = backend.policy_decide(approval_id, false).await;
            });
        } else if let Some(Pendiente::Reintentar { con }) = &dialogo.al_confirmar {
            // Las cuatro salidas de una colisión no son «confirmar» (#274):
            // cada una ES una política distinta, y cuál se pulsó es la
            // respuesta entera. `cancel` no traduce a ninguna y entonces no se
            // relanza nada — la task fallida se queda como estaba.
            if let Some(politica) = politica_de_colision(choice) {
                Self::lanzar_reintento(con.clone(), politica, backend, buzon);
            }
        }
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        salidas.push(self.parche(vec![cambio]));
        // Un rechazo se ACUSA como tal. Contestar `Applied` a un nombre que
        // no se escribió le dice al renderer que la operación salió, y la
        // misma superficie ya contestaba `Unavailable` cuando el rechazo era
        // por tener varias marcas: dos respuestas para la misma cosa.
        match rehusado {
            Some(reason_key) => (
                ActionAck::Unavailable {
                    reason_key: reason_key.to_owned(),
                },
                salidas,
            ),
            None => (self.aplicada(), salidas),
        }
    }

    /// Cuántos elementos como mucho enseña el cuerpo de un diálogo.
    ///
    /// El cuerpo no puede crecer con la selección —un lote de mil ficheros no
    /// cabe en una pregunta— así que se acota. Que se acotó lo dice
    /// [`Self::nota_de_recorte`]: una lista recortada en silencio describe
    /// una operación más pequeña que la que se va a ejecutar, y esta es la
    /// última pantalla donde todavía se puede decir que no.
    pub(super) const MAX_LINEAS_DIALOGO: usize = 16;

    /// La frase que dice que el cuerpo enseña menos de lo que hay. Vacía si
    /// los enseña todos.
    pub(super) fn nota_de_recorte(&self, mostrados: usize, total: usize) -> String {
        if mostrados >= total {
            return String::new();
        }
        clamp_display(norte_i18n::ta_in(
            self.lang,
            "dialog-body-truncated",
            &[
                ("shown", &mostrados.to_string()),
                ("total", &total.to_string()),
            ],
        ))
    }

    /// Una ruta como LÍNEA de diálogo: enmascarada, acotada, y diciendo si
    /// lo pintado difiere de lo real.
    ///
    /// Una sola función porque los cinco diálogos que enseñan rutas —crear,
    /// buscar, borrar, transferir y aprobar— tienen que decirlo igual, y el
    /// sitio donde uno de ellos se olvida del `bool` es exactamente donde
    /// alguien aprueba otra cosa.
    pub(super) fn linea_de_ruta(p: &VPath) -> crate::dto::DialogLine {
        let (texto, hostil) = norte_frontend::path_display(p);
        // El RECORTE también altera lo pintado, y ocurre DESPUÉS del
        // veredicto de `path_display`: una ruta UTF-8 limpia y larga —doce
        // segmentos de 255 bytes bastan— se pintaba con `…` al final y se
        // declaraba fiel. La elipsis es un carácter legal en un nombre, así
        // que quien lee no puede distinguir «se llama así» de «esto está
        // cortado», y en el informe de un lote ese nombre es lo único
        // accionable que hay: se va a teclear a mano.
        let recortado = texto.len() > crate::bridge::MAX_STRING_BYTES;
        crate::dto::DialogLine {
            text: clamp_display(texto),
            hostile: hostil || recortado,
        }
    }

    /// Como [`Self::linea_de_ruta`], pero con una REINTERPRETACIÓN concreta:
    /// la que había cuando se lanzó la operación por la que se pregunta.
    ///
    /// Dos cosas que se hacían mal donde se pregunta por una colisión, y las
    /// dos hacen que se apruebe otra cosa:
    ///
    /// - se enmascaraba sobre `display_lossy()`, que YA había metido los
    ///   U+FFFD. `display_name` recibía entonces UTF-8 impecable y declaraba
    ///   la ruta FIEL, así que la insignia de hostil no salía — en la única
    ///   pantalla donde se aprueba sobrescribir un fichero;
    /// - no se aplicaba `pane.names-encoding`, así que en un panel cp866 el
    ///   terminal preguntaba por `Папка` y la ventana por `??????`. Aprobar
    ///   un nombre que no es el que llevas viendo no es aprobar.
    ///
    /// La codificación llega por PARÁMETRO y no se lee del hueco activo: la
    /// colisión aparece asíncrona, encima de lo que sea que el lector esté
    /// haciendo, y entre el envío y la pregunta cabe cambiar de hueco o
    /// ciclar la codificación. Quien lanza la captura (`Reintento::enc`).
    ///
    /// Y la ruta ENTERA, no el nombre: «notas.txt» no dice CUÁL notas.txt, y
    /// con dos paneles y un lote esa es justo la pregunta.
    pub(super) fn linea_con_encoding(
        p: &VPath,
        enc: Option<norte_encoding::NameEncoding>,
    ) -> crate::dto::DialogLine {
        let (texto, hostil) = norte_frontend::path_display_with(p, enc);
        // El RECORTE también altera lo pintado, y la elipsis es un carácter
        // legal en un nombre: mismo razonamiento que `linea_de_ruta`.
        let recortado = texto.len() > crate::bridge::MAX_STRING_BYTES;
        crate::dto::DialogLine {
            text: clamp_display(texto),
            hostile: hostil || recortado,
        }
    }

    pub(super) fn vistas_de_dialogos(&self) -> Vec<DialogView> {
        self.dialogos.iter().map(|d| d.vista.clone()).collect()
    }
}
