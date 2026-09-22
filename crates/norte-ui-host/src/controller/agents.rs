//! El panel de sesiones de agente, y deshacer una entera.
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
    /// Abre el gestor de extensiones y PIDE el catálogo.
    ///
    /// Se abre vacío y diciendo que está cargando, no esperando: una ventana
    /// congelada mientras el daemon contesta es peor que una lista que
    /// aparece medio segundo después. Y «cargando» no es lo mismo que
    /// «ninguna»: una lista vacía sin ese aviso se lee como que no hay nada
    /// instalado.
    /// Abre el panel de sesiones de AGENTE.
    ///
    /// No pide nada al daemon: no hay método que enumere sesiones vivas, así
    /// que lo que se enseña es lo que ESTA ventana ha visto pedir permiso —y
    /// el panel lo dice—. Eso es también lo que hace que el operando del
    /// deshacer se ELIJA en vez de teclearse, que es lo que la tarea 5.3
    /// rechazó: un id de sesión tecleado se puede equivocar, y deshacer la
    /// sesión equivocada es deshacer el trabajo de otro.
    pub(super) fn abrir_agentes(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.agencia.panel = true;
        self.agencia.sesiones.al_abrir();
        let cambio = ViewChange::Agents {
            agents: self.vista_agentes(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// La proyección del panel de agentes.
    pub(super) fn vista_agentes(&self) -> Option<crate::dto::AgentsView> {
        // Una ventana sin efectos NO se suscribe al canal de aprobaciones,
        // así que su lista está vacía por ESO y no porque nadie haya pedido
        // nada. La pantalla lo dice, en vez de afirmar lo que no sabe.
        let escucha = self.efectos == crate::commands::Efectos::Completo;
        self.agencia
            .panel
            .then(|| self.agencia.sesiones.vista_de(self.lang, escucha))
    }

    /// Las teclas mientras el panel de agentes está abierto.
    pub(super) fn tecla_en_agentes(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        /// Cuántas filas mueve una página.
        const PAGINA: i64 = 10;
        // `Home`/`End` y `u` siguen siendo teclas fijas: el catálogo
        // compartido no tiene verbo para «al principio» ni para «deshacer la
        // sesión», y esperar a que los tenga habría dejado el panel sin
        // extremos y sin su única operación.
        let verbo = match k.key.as_str() {
            "Home" | "home" | "End" | "end" => None,
            "u" if !k.ctrl && !k.alt && !k.meta => None,
            _ => self.verbo_de_dialogo(k),
        };
        match (verbo.as_deref(), k.key.as_str()) {
            (Some("dialog.cancel"), _) => self.agencia.panel = false,
            (Some("dialog.down"), _) => self.agencia.sesiones.mover(1),
            (Some("dialog.up"), _) => self.agencia.sesiones.mover(-1),
            (Some("dialog.page-down"), _) => self.agencia.sesiones.mover(PAGINA),
            (Some("dialog.page-up"), _) => self.agencia.sesiones.mover(-PAGINA),
            (_, "Home" | "home") => self.agencia.sesiones.mover(i64::MIN / 2),
            (_, "End" | "end") => self.agencia.sesiones.mover(i64::MAX / 2),
            // `u` DESHACE la sesión entera, y pregunta antes: es la operación
            // más grande que esta ventana puede lanzar de un tirón —revierte
            // todo lo que un agente hizo, en orden inverso— y no hay ninguna
            // otra que toque tantas cosas con una tecla.
            // Y exige la tecla PELADA, a diferencia del resto de letras de
            // este host: `ctrl+u` es memoria muscular de otra cosa, y esta es
            // la operación más grande que la ventana puede lanzar de un
            // tirón.
            (_, "u") if !k.ctrl && !k.alt && !k.meta => return self.preguntar_por_deshacer(),
            _ => return (self.aplicada(), Vec::new()),
        }
        let _ = (backend, buzon);
        let cambio = ViewChange::Agents {
            agents: self.vista_agentes(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Un click sobre una fila del panel de agentes: la elige.
    pub(super) fn elegir_agente(
        &mut self,
        row: u32,
        generation: u64,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // Con un diálogo encima, el panel no recibe: es modal para el teclado
        // y tiene que serlo también para el ratón.
        if !self.agencia.panel || !self.dialogos.is_empty() {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        }
        if !self.agencia.sesiones.senalar(row as usize, generation) {
            // La lista cambió entre el pintado y el clic: se rehúsa en vez de
            // recortar, porque recortar es elegir por el lector.
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        }
        let cambio = ViewChange::Agents {
            agents: self.vista_agentes(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Abre la pregunta de deshacer una sesión entera.
    pub(super) fn preguntar_por_deshacer(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.efectos == crate::commands::Efectos::SoloLectura {
            return Self::no_muta();
        }
        let Some(sesion) = self.agencia.sesiones.elegida() else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-no-session".to_owned(),
                },
                Vec::new(),
            );
        };
        // Uno cada vez: dos `policy.undo_session` de la misma sesión caminan
        // la MISMA lista de entradas —cada uno la fotografía antes de que el
        // otro registre sus compensaciones—, y el segundo devuelve un informe
        // lleno de bloqueos que no son de nadie.
        if self.agencia.sesiones.tiene_undo_vivo(&sesion) {
            let fuera = self.decir("host-undo-already-running");
            return (
                ActionAck::Unavailable {
                    reason_key: "host-undo-already-running".to_owned(),
                },
                fuera,
            );
        }
        // El id, enmascarado, en su propio campo: es una clave opaca del
        // daemon que puede llevar cualquier byte, y una decisión sobre «esta
        // sesión» que no dice cuál no es una decisión.
        let (pintable, hostil) = norte_frontend::display_name(sesion.as_bytes());
        let modal = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id: modal,
            title_key: "modal-undo-session-title".to_owned(),
            destination: None,
            subject: Some(crate::dto::DialogLine {
                text: clamp_display(pintable),
                hostile: hostil,
            }),
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            // El cuerpo dice qué ALCANCE tiene, que es lo que no se ve en la
            // fila: deshacer una sesión revierte TODO lo que hizo, no lo
            // último, y lo que no se pueda revertir —algo irreversible, algo
            // que la policy deniegue ahora— se dirá en el informe.
            body: vec![crate::dto::DialogLine {
                text: clamp_display(norte_i18n::t_in(self.lang, "modal-undo-session-scope")),
                hostile: false,
            }],
            overflow_note: String::new(),
            overflow_hostile: false,
            choices: vec![
                DialogChoice {
                    id: "confirm".to_owned(),
                    label_key: "dialog-confirm".to_owned(),
                    // Deshacer ESCRIBE: mueve ficheros de vuelta y borra los
                    // que la sesión creó.
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
            vista: vista.clone(),
            tecleado: Tecleado::Texto(String::new()),
            reconocido: true,
            al_confirmar: Some(Pendiente::DeshacerSesion { sesion }),
        });
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Confirma el deshacer de UNA sesión: comprueba que no haya otro en
    /// marcha, lo lanza, y repinta la fila.
    ///
    /// La sesión es la que se LEYÓ en la pregunta, no la señalada ahora: la
    /// lista se reordena sola —una petición nueva sube a su sesión al primer
    /// puesto— y el diálogo se queda las teclas, no los mensajes de fondo.
    pub(super) fn deshacer_sesion(
        &mut self,
        sesion: &str,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (Option<&'static str>, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.agencia.sesiones.tiene_undo_vivo(sesion) {
            return (
                Some("host-undo-already-running"),
                self.decir("host-undo-already-running"),
            );
        }
        self.agencia.sesiones.deshaciendo(sesion);
        // Sin ALCANCE conocido: un `undo_session` toca los directorios que la
        // sesión tocara, que esta ventana no sabe. Se relista lo que está EN
        // PANTALLA, que es donde el lector estaba mirando trabajar al agente.
        let visibles = self.dirs_visibles();
        Self::lanzar_deshacer(sesion.to_owned(), visibles, backend, buzon);
        let mut fuera = Vec::new();
        if self.agencia.panel {
            let cambio = ViewChange::Agents {
                agents: self.vista_agentes(),
            };
            fuera.push(self.parche(vec![cambio]));
        }
        (None, fuera)
    }

    /// Lanza el deshacer de una sesión entera.
    ///
    /// Como cualquier otra operación larga: es una Task, aparece en el
    /// tablero y su informe —lo que NO volvió— llega por el camino que la 5.3
    /// ya construyó.
    pub(super) fn lanzar_deshacer(
        sesion: String,
        afectados: Vec<VPath>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            match backend.undo_session(sesion.clone()).await {
                Ok(task) => {
                    // El id de la task y la sesión, juntos: el desenlace
                    // llega por el progreso, que solo trae el id.
                    let _ = buzon
                        .send(Mensaje::Fondo(Box::new(Fondo::UndoDeSesion(
                            task.id.get(),
                            sesion,
                        ))))
                        .await;
                    let _ = buzon
                        .send(Mensaje::TaskNueva(Box::new((task, afectados, None))))
                        .await;
                }
                Err(e) => {
                    let _ = buzon.send(Mensaje::TaskFallida(Box::new(e))).await;
                }
            }
        });
    }
}
