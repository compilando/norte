//! Crear, borrar, empaquetar, partir y cambiar permisos.
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
    /// Crea el directorio TECLEADO dentro de este otro.
    ///
    /// El nombre se valida AQUÍ, con la misma regla que cualquier otro
    /// segmento: ni vacío, ni `/`, ni NUL, ni `.`/`..`. Un nombre que no vale
    /// no encola nada y lo dice; el texto tecleado no se pierde porque el
    /// diálogo se vuelve a abrir con él.
    ///
    /// El mismo cinturón que el rename: un nombre TOCADO que aún lleva el
    /// carácter de sustitución no se escribe. La asimetría de antes («crear
    /// no tiene siembra de la que heredar residuos») era falsa del ROUND
    /// TRIP: el host pinta su propia proyección enmascarada en el campo, y el
    /// renderer vuelve a sembrarlo con ella si tuvo que reconstruir el nodo —
    /// un diálogo de aprobación que se cuele por encima basta.
    pub(super) fn crear_directorio(
        &mut self,
        dir: &VPath,
        nombre: &str,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (Option<&'static str>, Vec<BridgeEnvelope<UiUpdate>>) {
        let seg = match Self::segmento_tecleado(nombre) {
            Ok(seg) => seg,
            Err(clave) => {
                self.status.message = Some(clamp_display(norte_i18n::t_in(self.lang, clave)));
                let cambio = ViewChange::Status(self.status.clone());
                return (Some(clave), vec![self.parche(vec![cambio])]);
            }
        };
        let destino = dir.join(seg);
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        let dir = dir.clone();
        tokio::spawn(async move {
            match backend.mkdir(destino).await {
                Ok(task) => {
                    let _ = buzon
                        .send(Mensaje::TaskNueva(Box::new((task, vec![dir], None))))
                        .await;
                }
                Err(e) => {
                    let _ = buzon.send(Mensaje::TaskFallida(Box::new(e))).await;
                }
            }
        });
        (None, Vec::new())
    }

    /// Los dos pendientes que fabrican ficheros a partir de lo TECLEADO:
    /// partir por tamaño y empaquetar por nombre (#132, #290).
    pub(super) fn ejecutar_de_archivo(
        &mut self,
        pendiente: Pendiente,
        tecleado: &str,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (Option<&'static str>, Vec<BridgeEnvelope<UiUpdate>>) {
        match pendiente {
            Pendiente::Partir { path, dest_dir } => {
                self.partir_fichero(path, dest_dir, tecleado, backend, buzon)
            }
            Pendiente::Empaquetar { dir, sources } => {
                self.empaquetar(&dir, sources, tecleado, backend, buzon)
            }
            // El llamante ya filtró; nombrarlos aquí hace que un tercero sea
            // un error de compilación.
            _ => (None, Vec::new()),
        }
    }

    /// Parte `path` en trozos del tamaño que se tecleó (#132, #290).
    ///
    /// El tamaño lo lee la misma función que el TUI: `10M` son 10 MiB y no
    /// diez millones, que es lo que significa en un gestor de ficheros. Un
    /// cero se rehúsa — trozos de cero bytes no terminan nunca.
    pub(super) fn partir_fichero(
        &mut self,
        path: VPath,
        dest_dir: VPath,
        tamano: &str,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (Option<&'static str>, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(part_bytes) = norte_frontend::nav::parse_size(tamano) else {
            return (Some("msg-split-bad-size"), self.decir("msg-split-bad-size"));
        };
        let afectados = vec![dest_dir.clone()];
        let params = norte_proto::methods::FileSplitParams {
            path,
            part_bytes,
            dest_dir,
        };
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let mensaje = match backend.split_file(params).await {
                Ok(task) => Mensaje::TaskNueva(Box::new((task, afectados, None))),
                Err(e) => Mensaje::TaskFallida(Box::new(e)),
            };
            let _ = buzon.send(mensaje).await;
        });
        (None, Vec::new())
    }

    /// Empaqueta `sources` en el contenedor que se tecleó (#132, #290).
    ///
    /// El FORMATO sale del nombre y viaja explícito: un nombre sin extensión
    /// que sepamos ESCRIBIR se rehúsa aquí en vez de empaquetar en algo que
    /// nadie pidió — un `.rar` cae ahí, porque se delega y solo para leer.
    ///
    /// La base de los nombres guardados es el directorio del panel: quien
    /// desempaquete espera ver lo que se veía en pantalla, no rutas absolutas.
    pub(super) fn empaquetar(
        &mut self,
        dir: &VPath,
        sources: Vec<VPath>,
        nombre: &str,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (Option<&'static str>, Vec<BridgeEnvelope<UiUpdate>>) {
        let seg = match Self::segmento_tecleado(nombre) {
            Ok(seg) => seg,
            Err(clave) => return (Some(clave), self.decir(clave)),
        };
        let Some(format) = norte_frontend::nav::format_by_name(seg.as_bytes()) else {
            return (
                Some("msg-pack-unknown-format"),
                self.decir("msg-pack-unknown-format"),
            );
        };
        let params = norte_proto::methods::ArchivePackParams {
            sources,
            dest: dir.join(seg),
            format,
            level: None,
            base: dir.clone(),
        };
        let afectados = vec![dir.clone()];
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let mensaje = match backend.pack(params).await {
                Ok(task) => Mensaje::TaskNueva(Box::new((task, afectados, None))),
                Err(e) => Mensaje::TaskFallida(Box::new(e)),
            };
            let _ = buzon.send(mensaje).await;
        });
        (None, Vec::new())
    }

    /// Abre la confirmación de un borrado. NO borra.
    ///
    /// Todas las vías —tecla, menú, gesto— pasan por aquí. Una operación
    /// destructiva con dos puertas acaba teniendo una sin cerrojo, y la que
    /// se olvida es siempre la que no se usa a diario.
    pub(super) fn pedir_borrado(
        &mut self,
        permanente: bool,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let hueco = self.hueco();
        let mut paths: Vec<VPath> = hueco.pane.marked_paths();
        if paths.is_empty() {
            // Sin marcas, lo que hay bajo el cursor. Sin cursor, nada que
            // borrar: y eso no abre un diálogo sobre un lote vacío.
            match hueco.pane.selected() {
                Some(e) => paths.push(e.path.clone()),
                None => {
                    return (
                        ActionAck::Unavailable {
                            reason_key: "msg-nothing-selected".to_owned(),
                        },
                        Vec::new(),
                    );
                }
            }
        }
        // ¿Hay papelera aquí? El terminal lo pregunta al borrar y de la
        // respuesta salen DOS cosas: que el borrado sea permanente, y que se
        // DIGA. La ventana no hacía ninguna de las dos, así que ofrecía el
        // mismo diálogo para «esto se puede recuperar» y para «esto no».
        //
        // **Tres estados, no dos, y esa es la parte que importa.** El
        // terminal `await`ea un `capabilities` fresco en el momento de
        // borrar, así que su `is_ok_and` colapsa una respuesta de verdad o un
        // fallo de verdad — nunca un «todavía no he preguntado». Aquí sale de
        // la caché del hueco, que llega DESPUÉS del listado y por su cuenta:
        // hay una ventana entera, entre que las filas se pintan y la
        // respuesta vuelve, en la que no consta nada. Y si la petición falla,
        // no consta en toda la sesión.
        //
        // Convertir ese «no consta» en «no hay papelera» borraba de verdad en
        // un sitio que sí la tiene. La asimetría manda, y va al revés de lo
        // que parece: suponer papelera donde no la hay cuesta un
        // `Unsupported` y un `shift+F8`; suponer que no la hay donde sí la
        // hay cuesta los bytes. Así que solo un NO explícito hace permanente
        // el borrado.
        let dir = self.hueco().pane.dir().clone();
        let papelera = self
            .caps_de_ruta(&dir)
            .map(|c| c.flags.contains(norte_proto::CapabilityFlags::TRASH));
        let permanente = permanente || papelera == Some(false);
        // Los nombres del cuerpo son de un atacante potencial: se pintan con
        // el saneado canónico y acotados, igual que en el listado.
        let cuerpo: Vec<crate::dto::DialogLine> = paths
            .iter()
            .take(Self::MAX_LINEAS_DIALOGO)
            .map(Self::linea_de_ruta)
            .collect();
        let nota = self.nota_de_recorte(cuerpo.len(), paths.len());
        let hostil_fuera = norte_frontend::overflow_hostile(&paths, cuerpo.len());
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id,
            title_key: if permanente {
                "modal-delete-permanent-title"
            } else {
                "modal-delete-title"
            }
            .to_owned(),
            // Un borrado no va a ninguna parte.
            destination: None,
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body: cuerpo,
            overflow_note: nota,
            overflow_hostile: hostil_fuera,
            choices: vec![
                DialogChoice {
                    id: "confirm".to_owned(),
                    label_key: "dialog-confirm".to_owned(),
                    // Lo destructivo se DICE en el propio contrato del
                    // diálogo: el renderer no tiene que adivinar cuál de las
                    // respuestas borra.
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
            // «⚠ SIN papelera: esto no se puede deshacer», con la clave del
            // terminal. Va por el mismo canal que los avisos de una copia
            // porque es la misma pregunta —qué pasa con los bytes cuando esto
            // termine— y porque un aviso en el CUERPO lo puede suplantar un
            // nombre de fichero. Un botón destructivo dice que la respuesta
            // borra; esto dice que no hay vuelta.
            dest_check: crate::dto::DestCheckView::Done {
                warnings: if permanente {
                    vec![clamp_display(norte_i18n::t_in(
                        self.lang,
                        "modal-delete-permanent-warning",
                    ))]
                } else {
                    Vec::new()
                },
            },
        };
        self.dialogos.push(Dialogo {
            id,
            vista: vista.clone(),
            tecleado: Tecleado::Texto(String::new()),
            reconocido: true,
            al_confirmar: Some(Pendiente::Borrar { paths, permanente }),
        });
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Los gestos que operan sobre lo MARCADO —o lo que hay bajo el cursor— y
    /// lanzan una task: contar, empaquetar, desempaquetar y comprobar (#132,
    /// #139, #290).
    ///
    /// Juntos por la misma razón que los de disposición: `aplicar_efecto` es
    /// un despachador y no puede crecer un brazo por cada gesto nuevo.
    pub(super) fn efecto_sobre_entradas(
        &mut self,
        efecto: Efecto,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match efecto {
            Efecto::TamanoDeDirectorio => self.contar_tamano(backend, buzon),
            Efecto::Empaquetar => self.pedir_empaquetado(),
            Efecto::Desempaquetar => self.desempaquetar(backend, buzon),
            Efecto::ComprobarArchivo => self.comprobar_archivo(backend, buzon),
            Efecto::PartirFichero => self.pedir_partido(),
            Efecto::Juntar => self.juntar_trozos(backend, buzon),
            // El llamante ya filtró: nombrarlos aquí es lo que hace que
            // añadir uno más sea un error de compilación.
            _ => (self.aplicada(), Vec::new()),
        }
    }

    /// `pane.pack` (#132, #290): pide el NOMBRE del contenedor.
    ///
    /// El nombre se teclea porque de él sale el formato. Aquí no se valida
    /// nada más que haya algo que empaquetar: la extensión se resuelve al
    /// confirmar, que es cuando hay nombre.
    pub(super) fn pedir_empaquetado(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // `marked_paths` cae al cursor sin marcas, igual que en una
        // transferencia: una sola fuente de «sobre qué opera esto».
        let sources: Vec<VPath> = self.hueco().pane.marked_paths();
        if sources.is_empty() {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-nothing-selected".to_owned(),
                },
                self.decir("msg-nothing-selected"),
            );
        }
        let dir = self.hueco().pane.dir().clone();
        let donde = Self::linea_de_ruta(&dir);
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id,
            title_key: "modal-pack-title".to_owned(),
            destination: None,
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body: vec![donde],
            overflow_note: String::new(),
            overflow_hostile: false,
            choices: vec![
                DialogChoice {
                    id: "confirm".to_owned(),
                    label_key: "dialog-confirm".to_owned(),
                    destructive: false,
                },
                DialogChoice {
                    id: "cancel".to_owned(),
                    label_key: "dialog-cancel".to_owned(),
                    destructive: false,
                },
            ],
            input: Some(String::new()),
            input_hostile: false,
            input_secret: false,
            dest_check: crate::dto::DestCheckView::NotAsked,
        };
        self.dialogos.push(Dialogo {
            id,
            vista: vista.clone(),
            tecleado: Tecleado::Texto(String::new()),
            reconocido: true,
            al_confirmar: Some(Pendiente::Empaquetar { dir, sources }),
        });
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// `pane.split-file` (#132, #290): pide el TAMAÑO de los trozos.
    ///
    /// Los trozos van al panel destino, como una copia y por lo mismo: partir
    /// un fichero de un giga en el sitio donde ya está suele no caber.
    pub(super) fn pedir_partido(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(entrada) = self.hueco().pane.selected().cloned() else {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-nothing-selected".to_owned(),
                },
                self.decir("msg-nothing-selected"),
            );
        };
        let dest_dir = match self.directorio_destino() {
            Ok(d) => d,
            Err(reason_key) => {
                return (
                    ActionAck::Unavailable {
                        reason_key: reason_key.to_owned(),
                    },
                    self.decir(reason_key),
                );
            }
        };
        let donde = Self::linea_de_ruta(&dest_dir);
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id,
            title_key: "modal-split-title".to_owned(),
            destination: Some(donde),
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body: vec![crate::dto::DialogLine {
                text: clamp_display(norte_i18n::t_in(self.lang, "modal-split-hint")),
                hostile: false,
            }],
            overflow_note: String::new(),
            overflow_hostile: false,
            choices: vec![
                DialogChoice {
                    id: "confirm".to_owned(),
                    label_key: "dialog-confirm".to_owned(),
                    destructive: false,
                },
                DialogChoice {
                    id: "cancel".to_owned(),
                    label_key: "dialog-cancel".to_owned(),
                    destructive: false,
                },
            ],
            input: Some(String::new()),
            input_hostile: false,
            input_secret: false,
            dest_check: crate::dto::DestCheckView::NotAsked,
        };
        self.dialogos.push(Dialogo {
            id,
            vista: vista.clone(),
            tecleado: Tecleado::Texto(String::new()),
            reconocido: true,
            al_confirmar: Some(Pendiente::Partir {
                path: entrada.path.clone(),
                dest_dir,
            }),
        });
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// `pane.combine-files` (#132, #290): junta los trozos desde el `.001`
    /// bajo el cursor.
    ///
    /// Solo desde el PRIMERO, y la regla vive en el crate compartido: empezar
    /// por el `.007` uniría media cosa, y el core solo busca hacia delante.
    pub(super) fn juntar_trozos(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(entrada) = self.hueco().pane.selected().cloned() else {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-nothing-selected".to_owned(),
                },
                self.decir("msg-nothing-selected"),
            );
        };
        let nombre = entrada
            .path
            .file_name()
            .map(|s| s.as_bytes().to_vec())
            .unwrap_or_default();
        let Some(base) = norte_frontend::nav::base_de_trozos(&nombre) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-combine-needs-first".to_owned(),
                },
                self.decir("msg-combine-needs-first"),
            );
        };
        let dir = self.hueco().pane.dir().clone();
        let params = norte_proto::methods::FileCombineParams {
            first: entrada.path.clone(),
            dest: dir.join(base),
        };
        let afectados = vec![dir];
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let mensaje = match backend.combine_files(params).await {
                Ok(task) => Mensaje::TaskNueva(Box::new((task, afectados, None))),
                Err(e) => Mensaje::TaskFallida(Box::new(e)),
            };
            let _ = buzon.send(mensaje).await;
        });
        (self.aplicada(), self.decir("msg-combine-started"))
    }

    /// `pane.unpack` (#132, #290): copia el INTERIOR del contenedor bajo el
    /// cursor al panel destino.
    ///
    /// Sin método propio y sin hacerle falta: el motor de copia acepta el
    /// interior de un archivo como origen, así que esto es la copia que el
    /// lector podría haber hecho a mano — con su journal, su undo y su
    /// cancelación.
    pub(super) fn desempaquetar(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(entrada) = self.hueco().pane.selected().cloned() else {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-nothing-selected".to_owned(),
                },
                self.decir("msg-nothing-selected"),
            );
        };
        // La MISMA función que decide si `Enter` entra en un contenedor
        // (`norte_frontend::nav`): dos tablas de extensiones serían dos sitios
        // donde una se olvida, y entonces la misma entrada se navega en una
        // superficie y no se desempaqueta en la otra.
        let Some(raiz) = norte_frontend::nav::archive_root_for(&entrada) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-unpack-not-archive".to_owned(),
                },
                self.decir("msg-unpack-not-archive"),
            );
        };
        let destino = match self.directorio_destino() {
            Ok(d) => d,
            Err(reason_key) => {
                return (
                    ActionAck::Unavailable {
                        reason_key: reason_key.to_owned(),
                    },
                    self.decir(reason_key),
                );
            }
        };
        // Una copia como cualquier otra, con `Fail` y su reintento: si el
        // destino ya tiene lo que va dentro, el lector decide igual que en una
        // transferencia (#274).
        Self::lanzar_reintento(
            Reintento {
                from: raiz,
                to: destino,
                mover: false,
                // La del hueco desde el que se desempaqueta, capturada aquí:
                // ver el campo.
                enc: self.hueco().pane.name_encoding(),
            },
            norte_proto::CollisionPolicy::Fail,
            backend,
            buzon,
        );
        (self.aplicada(), self.decir("msg-unpack-started"))
    }

    /// `pane.test-archive` (#132, #290): comprueba el contenedor bajo el
    /// cursor. No escribe nada; su resultado es el desenlace de la Task.
    pub(super) fn comprobar_archivo(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(entrada) = self.hueco().pane.selected().cloned() else {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-nothing-selected".to_owned(),
                },
                self.decir("msg-nothing-selected"),
            );
        };
        if norte_frontend::nav::archive_root_for(&entrada).is_none() {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-unpack-not-archive".to_owned(),
                },
                self.decir("msg-unpack-not-archive"),
            );
        }
        let params = norte_proto::methods::ArchiveTestParams {
            path: entrada.path.clone(),
        };
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let mensaje = match backend.test_archive(params).await {
                // Comprobar no cambia nada: no hay directorios que refrescar.
                Ok(task) => Mensaje::TaskNueva(Box::new((task, Vec::new(), None))),
                Err(e) => Mensaje::TaskFallida(Box::new(e)),
            };
            let _ = buzon.send(mensaje).await;
        });
        (self.aplicada(), self.decir("msg-test-archive-started"))
    }

    pub(super) fn pedir_mkdir(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let dir = self.hueco().pane.dir().clone();
        let donde = Self::linea_de_ruta(&dir);
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id,
            title_key: "modal-mkdir-title".to_owned(),
            // El directorio en el que se crea NO es un destino: es el
            // contexto. Un destino es a dónde se MUEVE algo que ya existe.
            destination: None,
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body: vec![donde],
            overflow_note: String::new(),
            overflow_hostile: false,
            choices: vec![
                DialogChoice {
                    id: "confirm".to_owned(),
                    label_key: "dialog-confirm".to_owned(),
                    destructive: false,
                },
                DialogChoice {
                    id: "cancel".to_owned(),
                    label_key: "dialog-cancel".to_owned(),
                    destructive: false,
                },
            ],
            // Con campo de texto: es lo que hace que el renderer sepa que
            // aquí se teclea, sin que tenga que deducirlo del título.
            input: Some(String::new()),
            input_hostile: false,
            input_secret: false,
            dest_check: crate::dto::DestCheckView::NotAsked,
        };
        self.dialogos.push(Dialogo {
            id,
            vista: vista.clone(),
            tecleado: Tecleado::Texto(String::new()),
            reconocido: true,
            al_confirmar: Some(Pendiente::CrearDirectorio { dir }),
        });
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Pide el MODO en octal para lo marcado (#314, ADR 0081).
    ///
    /// El campo viene prellenado con los permisos de la entrada bajo el cursor
    /// **si el listado los trae** —los trae cuando el esquema de columnas pide
    /// `posix.mode`—, y vacío si no. Prellenar no es adorno: quitarle el bit de
    /// ejecución a algo que lo tenía, porque no se veía cuál era, es justo el
    /// error que un campo en blanco invita a cometer.
    ///
    /// El cuerpo dice sobre CUÁNTAS entradas va, por lo mismo que en la
    /// terminal: teclear un modo creyendo que va sobre una y que vaya sobre
    /// cincuenta es lo que este diálogo tiene que hacer difícil.
    pub(super) fn pedir_permisos(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let targets = self.hueco().pane.marked_paths();
        if targets.is_empty() {
            let fuera = self.decir("host-nothing-selected");
            return (
                ActionAck::Unavailable {
                    reason_key: "host-nothing-selected".to_owned(),
                },
                fuera,
            );
        }
        let modo = self
            .hueco()
            .pane
            .selected()
            .and_then(norte_frontend::chmod::mode_of)
            .map(norte_frontend::chmod::format_mode)
            .unwrap_or_default();
        let cuantas = norte_i18n::ta_in(
            self.lang,
            "modal-chmod-count",
            &[("n", &targets.len().to_string())],
        );
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id,
            title_key: "modal-chmod-title".to_owned(),
            destination: None,
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body: vec![crate::dto::DialogLine {
                text: clamp_display(cuantas),
                hostile: false,
            }],
            overflow_note: String::new(),
            overflow_hostile: false,
            choices: vec![
                DialogChoice {
                    id: "confirm".to_owned(),
                    label_key: "dialog-confirm".to_owned(),
                    destructive: false,
                },
                DialogChoice {
                    id: "cancel".to_owned(),
                    label_key: "dialog-cancel".to_owned(),
                    destructive: false,
                },
            ],
            input: Some(modo.clone()),
            input_hostile: false,
            input_secret: false,
            dest_check: crate::dto::DestCheckView::NotAsked,
        };
        self.dialogos.push(Dialogo {
            id,
            vista: vista.clone(),
            tecleado: Tecleado::Texto(modo),
            reconocido: true,
            al_confirmar: Some(Pendiente::Permisos { targets }),
        });
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Manda el cambio de permisos que el diálogo confirmó (#314).
    ///
    /// Lo tecleado se lee con el MISMO parser que la terminal
    /// ([`norte_frontend::chmod::parse_mode`]): un modo que no vale se dice y
    /// el diálogo se queda abierto con lo escrito, que es lo que hacen aquí
    /// todos los prompts.
    pub(super) fn cambiar_permisos(
        &mut self,
        targets: Vec<VPath>,
        tecleado: &str,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (Option<&'static str>, Vec<BridgeEnvelope<UiUpdate>>) {
        let mode = match norte_frontend::chmod::parse_mode(tecleado) {
            Ok(m) => m,
            Err(e) => {
                let clave = e.message_key();
                return (Some(clave), self.decir(clave));
            }
        };
        // Los directorios a refrescar: los PADRES de lo que cambia, porque lo
        // que se ve distinto tras un chmod es la columna de permisos de sus
        // filas.
        let mut refrescar: Vec<VPath> = targets.iter().filter_map(VPath::parent).collect();
        refrescar.sort();
        refrescar.dedup();
        let params = norte_proto::methods::FsSetModeParams {
            paths: targets,
            mode,
            // La ventana todavía no ofrece el recursivo (#315): su diálogo es
            // un campo de texto y esto es una casilla. Va a `false` explícito
            // y no por defecto para que el día que aparezca la casilla no haya
            // que buscar dónde se decidía.
            recursive: false,
            dir_mode: None,
        };
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            match backend.set_mode(params).await {
                Ok(task) => {
                    let _ = buzon
                        .send(Mensaje::TaskNueva(Box::new((task, refrescar, None))))
                        .await;
                }
                Err(e) => {
                    let _ = buzon.send(Mensaje::TaskFallida(Box::new(e))).await;
                }
            }
        });
        (None, Vec::new())
    }

    /// Pide el nombre de un fichero NUEVO para editarlo (#290).
    ///
    /// Solo en un panel local: lo que se abre después es la aplicación del
    /// escritorio, y a `xdg-open` no se le puede dar un `sftp://`. Se dice
    /// ANTES de teclear el nombre, que es cuando todavía sirve de algo.
    pub(super) fn pedir_fichero_nuevo(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let dir = self.hueco().pane.dir().clone();
        if !norte_frontend::shell::is_local(&dir) {
            let fuera = self.decir("host-not-local");
            return (
                ActionAck::Unavailable {
                    reason_key: "host-not-local".to_owned(),
                },
                fuera,
            );
        }
        let donde = Self::linea_de_ruta(&dir);
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id,
            title_key: "modal-new-file-title".to_owned(),
            destination: None,
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body: vec![donde],
            overflow_note: String::new(),
            overflow_hostile: false,
            choices: vec![
                DialogChoice {
                    id: "confirm".to_owned(),
                    label_key: "dialog-confirm".to_owned(),
                    destructive: false,
                },
                DialogChoice {
                    id: "cancel".to_owned(),
                    label_key: "dialog-cancel".to_owned(),
                    destructive: false,
                },
            ],
            input: Some(String::new()),
            input_hostile: false,
            input_secret: false,
            dest_check: crate::dto::DestCheckView::NotAsked,
        };
        self.dialogos.push(Dialogo {
            id,
            vista,
            tecleado: Tecleado::Texto(String::new()),
            reconocido: true,
            al_confirmar: Some(Pendiente::CrearFichero { dir }),
        });
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Crea el fichero vacío y APUNTA que hay que abrirlo cuando exista.
    ///
    /// Abrirlo aquí sería abrir algo que todavía no está en el disco: la
    /// creación es una Task, y hasta su desenlace no hay fichero que darle al
    /// escritorio.
    pub(super) fn crear_fichero(
        &mut self,
        dir: &VPath,
        nombre: &str,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (Option<&'static str>, Vec<BridgeEnvelope<UiUpdate>>) {
        let seg = match Self::segmento_tecleado(nombre) {
            Ok(seg) => seg,
            Err(clave) => {
                self.status.message = Some(clamp_display(norte_i18n::t_in(self.lang, clave)));
                let cambio = ViewChange::Status(self.status.clone());
                return (Some(clave), vec![self.parche(vec![cambio])]);
            }
        };
        let destino = dir.join(seg);
        let backend_c = Arc::clone(backend);
        let buzon_c = buzon.clone();
        let dir_c = dir.clone();
        let abrir = destino.clone();
        tokio::spawn(async move {
            match backend_c.create_file(destino).await {
                Ok(task) => {
                    let _ = buzon_c
                        .send(Mensaje::TaskNueva(Box::new((task, vec![dir_c], None))))
                        .await;
                }
                Err(e) => {
                    let _ = buzon_c.send(Mensaje::TaskFallida(Box::new(e))).await;
                }
            }
        });
        self.abrir_al_crear = Some(Creacion {
            task: None,
            path: abrir,
        });
        (None, Vec::new())
    }

    /// El fichero recién creado existe: PREGUNTA si sigue siendo un fichero y,
    /// si lo es, lo abre con el escritorio.
    ///
    /// Solo con un desenlace BUENO. Abrir tras un fallo lanzaría el editor
    /// sobre un fichero que no está, y lo que ese editor enseñe —un buffer
    /// vacío que al guardar crea el fichero— parecería que funcionó.
    ///
    /// # Por qué hay un `fs.stat` en medio (#303)
    ///
    /// norte ANUNCIA el nombre creándolo, y entre eso y el `xdg-open` hay una
    /// ventana en la que cualquiera que escriba en ese directorio puede
    /// desenlazarlo y dejar un symlink: el humano acabaría escribiendo en un
    /// fichero que nadie le enseñó, y el `undo` de la entrada `Created` va por
    /// RUTA y no por identidad. `fs.stat` es `lstat` —describe el enlace, no
    /// su destino—, así que la pregunta ve lo que hay de verdad.
    ///
    /// **Estrecha la ventana, no la cierra**: entre el `stat` y el `open`
    /// queda hueco, y cerrarlo pediría entregarle un descriptor al programa
    /// del escritorio, cosa que `xdg-open` no acepta. Es la MISMA decisión que
    /// toma la TUI en `gestures::edit_created`, y está aquí por eso: una
    /// decisión duplicada entre frontends diverge en silencio (ADR 0077).
    ///
    /// La respuesta vuelve por el buzón como un mensaje más
    /// ([`Mensaje::CreadoComprobado`]): el estado lo toca un solo escritor, y
    /// esperar aquí bloquearía el actor entero por un viaje al daemon.
    pub(super) fn abrir_lo_creado(
        &mut self,
        p: &norte_proto::TaskProgress,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        // Por ID, no por kind. `task.progress` se difunde a TODA conexión
        // humana, así que un `fs.create` de la TUI —o de otra ventana sobre el
        // mismo daemon— llegaba aquí, se comía la intención y abría un fichero
        // que todavía no estaba: justo el fallo que este orden existe para
        // evitar. Y al revés, el que sí se creó no se abría nunca.
        if self
            .abrir_al_crear
            .as_ref()
            .is_none_or(|c| c.task != Some(p.task_id.get()))
        {
            return;
        }
        let Some(c) = self.abrir_al_crear.take() else {
            return;
        };
        let path = c.path;
        if p.state != norte_proto::TaskState::Completed {
            return;
        }
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            // Sin atributos: lo único que se pregunta es QUÉ es, y pedir
            // atributos sería trabajo del provider que nadie va a leer.
            let veredicto = match backend.stat(path.clone(), Vec::new()).await {
                Ok(e) => Veredicto::from(e.kind == norte_proto::EntryKind::File),
                // `NotFound` es una RESPUESTA —ahí no hay nada—, y además la
                // del desenlace más probable de un ataque: desenlazar y no
                // reponer. Lo demás no es lo mismo que un enlace: un daemon
                // relevado o un timeout no son manipulación, y decir que sí es
                // una acusación falsa que enseña a ignorar el mensaje bueno.
                Err(Error::NotFound) => Veredicto::YaNoEsElFichero,
                Err(_) => Veredicto::NoSeSabe,
            };
            let _ = buzon
                .send(Mensaje::CreadoComprobado(Box::new((path, veredicto))))
                .await;
        });
    }

    /// La respuesta del `fs.stat` de [`Self::abrir_lo_creado`]: abre, o dice
    /// por qué no.
    ///
    /// Un solo mensaje para las tres causas que son la MISMA (un enlace, una
    /// carpeta, ya no está): decir cuál sería confirmarle al que puso el enlace
    /// que su enlace está puesto. No se pudo preguntar es otra cosa y lo dice
    /// aparte.
    pub(super) fn abrir_lo_comprobado(
        &mut self,
        path: norte_proto::VPath,
        veredicto: Veredicto,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        match veredicto {
            Veredicto::EsElFichero => {}
            Veredicto::YaNoEsElFichero => return self.decir("host-created-changed"),
            Veredicto::NoSeSabe => return self.decir("host-created-unchecked"),
        }
        if self.nativo(crate::dto::NativeEffect::OpenPath { path }) {
            return Vec::new();
        }
        self.decir("host-no-desktop")
    }

    /// Encola una Task por entrada y engancha su progreso al actor.
    pub(super) fn lanzar_borrado(
        paths: Vec<VPath>,
        permanente: bool,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        let mode = if permanente {
            norte_proto::DeleteMode::Permanent
        } else {
            norte_proto::DeleteMode::Trash
        };
        for path in paths {
            let backend = Arc::clone(backend);
            let buzon = buzon.clone();
            let afectados: Vec<VPath> = path.parent().into_iter().collect();
            tokio::spawn(async move {
                match backend.delete(path, mode).await {
                    Ok(task) => {
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

    /// La SEGUNDA cerradura del modo solo lectura, sobre el punto ÚNICO donde
    /// se lanzan todas las mutaciones.
    ///
    /// Barata, y hoy inalcanzable: en solo lectura ningún `Pendiente` que
    /// mute llega a nacer y el canal de aprobaciones ni se toma. «Inalcanzable
    /// hoy» es exactamente lo que deja de ser verdad cuando alguien añada el
    /// siguiente diálogo, y esta es la puerta por la que pasaría.
    ///
    /// Cierra el diálogo al rechazarlo: dejarlo abierto invitaría a pulsar
    /// otra vez lo que no va a ocurrir.
    pub(super) fn rechaza_por_solo_lectura(
        &mut self,
        pos: usize,
    ) -> Option<(ActionAck, Vec<BridgeEnvelope<UiUpdate>>)> {
        if self.efectos != crate::commands::Efectos::SoloLectura {
            return None;
        }
        let muta = self
            .dialogos
            .get(pos)?
            .al_confirmar
            .as_ref()
            .is_some_and(|p| {
                matches!(
                    p,
                    Pendiente::Borrar { .. }
                        | Pendiente::Transferir { .. }
                        | Pendiente::Soltar { .. }
                        | Pendiente::CrearDirectorio { .. }
                        | Pendiente::CrearFichero { .. }
                        | Pendiente::Decidir { .. }
                        | Pendiente::Renombrar { .. }
                        // Conceder capabilities es la decisión de seguridad
                        // del sistema de extensiones: una ventana que se
                        // declara de solo lectura no la toma.
                        | Pendiente::AprobarExtension { .. }
                        // Deshacer una sesión ESCRIBE: mueve ficheros de
                        // vuelta y borra lo que el agente creó.
                        | Pendiente::DeshacerSesion { .. }
                        // Pedir un plan no escribe en el disco, y aun así
                        // entra: manda el contenido de un directorio a un
                        // modelo, que no es algo que deba hacer una ventana
                        // que se declara de solo lectura.
                        | Pendiente::InstruccionIa { .. }
                        // Y el lote por plantilla (#310) acaba en un rename.
                        | Pendiente::PlantillaLote { .. }
                        // Tampoco: la consulta sale del proceso.
                        | Pendiente::ConsultaSemantica // `EntregarSecreto` NO está, y es deliberado (#327):
                                                       // entregar la contraseña habilita LEER un sitio al que
                                                       // no se podía entrar, que es justo lo que una ventana
                                                       // de solo lectura sí hace. Vetarlo dejaría la conexión
                                                       // `prompt` inservible en solo lectura sin ganar nada
                                                       // — el secreto va a la memoria del daemon, no al
                                                       // disco, y lo que se autorice después lo sigue
                                                       // gobernando la política.
                                                       //
                                                       // Se dice aquí porque el rustdoc de esta función avisa
                                                       // de que esta es la puerta por la que pasaría el
                                                       // siguiente diálogo, y un silencio no se distingue de
                                                       // un olvido.
                )
            });
        if !muta {
            return None;
        }
        self.dialogos.remove(pos);
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        Some((
            ActionAck::Unavailable {
                reason_key: "host-read-only".to_owned(),
            },
            vec![self.parche(vec![cambio])],
        ))
    }
}
