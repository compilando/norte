//! Sumas de comprobación: pedirlas, encolarlas y leer su informe.
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
    /// La Task de sumas terminó: se pide su informe (#311).
    ///
    /// Los mismos guards que el de sync, y por lo mismo: la CLASE, la ÉPOCA de
    /// conexión y la idempotencia —una reconexión reanuncia el terminal, y
    /// esto es una RPC.
    pub(super) fn pedir_informe_de_sumas(
        &mut self,
        p: &norte_proto::TaskProgress,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        let Some(s) = self.sumas.as_ref() else {
            return;
        };
        if s.task != p.task_id
            || s.epoca_conexion != self.epoca_conexion
            || !matches!(p.kind, norte_proto::TaskKind::Checksum)
            || s.informe_pedido
        {
            return;
        }
        if let Some(s) = self.sumas.as_mut() {
            s.informe_pedido = true;
        }
        let estado = p.state.clone();
        let id = p.task_id;
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let informe = backend.checksum_report(id).await;
            let _ = buzon
                .send(Mensaje::Fondo(Box::new(Fondo::InformeDeSumas(
                    id,
                    estado,
                    Box::new(informe),
                ))))
                .await;
        });
    }

    /// El informe de sumas llegó: se juzga y se abre el diálogo (#311).
    ///
    /// **Un informe PARCIAL no se compara con nada.** Cancelar deja `pending`
    /// por encima de cero con la Task ya terminal, y juzgar eso acusaría —«no
    /// cuadra o falta»— a ficheros que nadie llegó a leer, que es el peor
    /// error posible en la única herramienta cuyo trabajo es comprobar.
    pub(super) fn informe_de_sumas(
        &mut self,
        task: norte_proto::TaskId,
        estado: &norte_proto::TaskState,
        informe: Result<norte_proto::methods::FsChecksumReportResult, Error>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        use norte_frontend::checksums;

        let Some(sumas) = self.sumas.take().filter(|s| s.task == task) else {
            return Vec::new();
        };
        let Ok(informe) = informe else {
            return self.decir("err-checksum-failed");
        };
        if *estado != norte_proto::TaskState::Completed || informe.pending > 0 {
            return self.decir("err-checksum-partial");
        }
        let calculado: Vec<checksums::Computed> = informe
            .entries
            .iter()
            .map(|e| (e.digest.clone(), e.miss))
            .collect();
        let (lineas, copiable, mensaje) = if let Some(publicado) = sumas.publicado {
            let veredictos = checksums::judge(&publicado.lines, &publicado.asked, &calculado);
            let filas: Vec<crate::dto::DialogLine> = publicado
                .lines
                .iter()
                .zip(&veredictos)
                .map(|(linea, v)| self.fila_de_suma(&linea.name, None, Some(*v)))
                .collect();
            let mensaje = match checksums::summarize(&veredictos, publicado.refused) {
                checksums::Summary::Unreadable { n, refused } => self.decir_con(
                    "msg-checksum-unreadable-lines",
                    &[("n", &n.to_string()), ("refused", &refused.to_string())],
                ),
                checksums::Summary::AllOk { n } => {
                    self.decir_con("msg-checksum-all-ok", &[("n", &n.to_string())])
                }
                checksums::Summary::Bad { n } => {
                    self.decir_con("msg-checksum-bad", &[("n", &n.to_string())])
                }
            };
            // Una comprobación no trae digests: no hay lista que copiar.
            (filas, Vec::new(), mensaje)
        } else {
            let filas: Vec<crate::dto::DialogLine> = informe
                .entries
                .iter()
                .map(|e| {
                    let nombre = e
                        .path
                        .file_name()
                        .map(|s| s.as_bytes().to_vec())
                        .unwrap_or_default();
                    let veredicto = e.miss.map(|m| match m {
                        norte_proto::methods::ChecksumMiss::NotAFile => {
                            checksums::Verdict::NotAFile
                        }
                        _ => checksums::Verdict::Missing,
                    });
                    self.fila_de_suma(&nombre, e.digest.as_deref(), veredicto)
                })
                .collect();
            let copiable: Vec<checksums::Computed> = calculado.clone();
            (filas, copiable, Vec::new())
        };
        // Lo que se copiaría, en BYTES y con el escapado de coreutils: un
        // nombre no tiene por qué ser texto (regla 1).
        let para_copiar: Vec<(Vec<u8>, Option<String>)> = informe
            .entries
            .iter()
            .zip(copiable)
            .map(|(e, (digest, _))| {
                (
                    e.path
                        .file_name()
                        .map(|s| s.as_bytes().to_vec())
                        .unwrap_or_default(),
                    digest,
                )
            })
            .collect();
        let bytes = checksums::to_sums_bytes(&para_copiar);
        let mut fuera = mensaje;
        fuera.extend(self.abrir_sumas(lineas, bytes));
        fuera
    }

    /// Una fila del diálogo de sumas: el veredicto —o el digest recortado— y
    /// el nombre, saneado como cualquier otro que pinte esta ventana.
    pub(super) fn fila_de_suma(
        &self,
        nombre: &[u8],
        digest: Option<&str>,
        veredicto: Option<norte_frontend::checksums::Verdict>,
    ) -> crate::dto::DialogLine {
        let (texto, hostil) = norte_frontend::display::display_name(nombre);
        let estado = match (veredicto, digest) {
            (Some(v), _) => norte_i18n::t_in(self.lang, v.label_key()),
            (None, Some(d)) => d.chars().take(12).collect::<String>(),
            (None, None) => norte_i18n::t_in(self.lang, "checksum-unreadable"),
        };
        crate::dto::DialogLine {
            text: clamp_display(format!("{estado}  {texto}")),
            hostile: hostil,
        }
    }

    /// Abre el diálogo con las sumas ya juzgadas (#311).
    ///
    /// Confirmar COPIA la lista al portapapeles cuando hay digests que copiar,
    /// y cuando no —una comprobación no los trae— el diálogo solo se cierra.
    pub(super) fn abrir_sumas(
        &mut self,
        body: Vec<crate::dto::DialogLine>,
        bytes: Vec<u8>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let copiable = !bytes.is_empty();
        let mut choices = Vec::new();
        if copiable {
            choices.push(DialogChoice {
                id: "confirm".to_owned(),
                label_key: "dialog-copy".to_owned(),
                destructive: false,
            });
        }
        choices.push(DialogChoice {
            id: "cancel".to_owned(),
            label_key: "dialog-close".to_owned(),
            destructive: false,
        });
        let vista = DialogView {
            id,
            title_key: "modal-checksums-title".to_owned(),
            destination: None,
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body,
            overflow_note: String::new(),
            choices,
            input: None,
            input_hostile: false,
            input_secret: false,
        };
        self.dialogos.push(Dialogo {
            id,
            vista: vista.clone(),
            tecleado: Tecleado::Texto(String::new()),
            reconocido: true,
            al_confirmar: copiable.then_some(Pendiente::CopiarSumas { bytes }),
        });
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        vec![self.parche(vec![cambio])]
    }

    /// Calcula las sumas de lo marcado, o comprueba el fichero de sumas bajo
    /// el cursor (#311, ADR 0080).
    ///
    /// Comprobar lee el fichero ANTES de lanzar nada: sin sus líneas no hay
    /// rutas que pedir. Ese `read` va spawneado, como todo lo que habla con el
    /// backend desde aquí, y vuelve por el buzón.
    pub(super) fn lanzar_sumas(
        &mut self,
        verificar: bool,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if verificar {
            let Some(sums) = self.hueco().pane.selected().map(|e| e.path.clone()) else {
                return (
                    ActionAck::Unavailable {
                        reason_key: "host-nothing-selected".to_owned(),
                    },
                    self.decir("host-nothing-selected"),
                );
            };
            let backend2 = Arc::clone(backend);
            let buzon2 = buzon.clone();
            tokio::spawn(async move {
                // Un byte MÁS que el tope, para poder distinguir «cabe» de «no
                // cabe»: un fichero de sumas recortado en silencio comprueba
                // media lista y se lee como «todo correcto».
                let bytes = backend2
                    .read(
                        sums.clone(),
                        Some(norte_proto::ByteRange {
                            offset: 0,
                            len: Some(SUMS_MAX_BYTES + 1),
                        }),
                    )
                    .await;
                let _ = buzon2
                    .send(Mensaje::Fondo(Box::new(Fondo::FicheroDeSumas(
                        Box::new(sums),
                        Box::new(bytes),
                    ))))
                    .await;
            });
            return (self.aplicada(), Vec::new());
        }
        let paths = self.hueco().pane.marked_paths();
        if paths.is_empty() {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-nothing-selected".to_owned(),
                },
                self.decir("host-nothing-selected"),
            );
        }
        let fuera = self.encolar_sumas(paths, None, backend, buzon);
        (self.aplicada(), fuera)
    }

    /// El fichero de sumas llegó: se lee y se lanza la Task (#311).
    pub(super) fn fichero_de_sumas(
        &mut self,
        sums: &VPath,
        bytes: Result<Vec<u8>, Error>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Ok(bytes) = bytes else {
            return self.decir("err-checksum-not-a-sums-file");
        };
        if bytes.len() as u64 > SUMS_MAX_BYTES {
            return self.decir("err-checksum-sums-too-big");
        }
        let leido = norte_frontend::checksums::parse_sums(&bytes);
        if leido.lines.is_empty() {
            // Decir POR QUÉ cuando se sabe: un fichero de PowerShell es un
            // fichero de sumas perfectamente válido en otra codificación.
            return self.decir(if norte_frontend::checksums::looks_utf16(&bytes) {
                "err-checksum-sums-utf16"
            } else {
                "err-checksum-not-a-sums-file"
            });
        }
        // Contra el directorio del FICHERO DE SUMAS, no contra el del panel:
        // un `SHA256SUMS` habla de lo que tiene al lado.
        let Some(base) = sums.parent() else {
            return self.decir("err-checksum-not-a-sums-file");
        };
        let (paths, asked) = norte_frontend::checksums::resolve_targets(&base, &leido.lines);
        if paths.is_empty() {
            return self.decir("err-checksum-not-a-sums-file");
        }
        let publicado = Publicado {
            lines: leido.lines,
            asked,
            refused: leido.refused,
        };
        self.encolar_sumas(paths, Some(publicado), backend, buzon)
    }

    /// Encola la Task de sumas y apunta qué informe hay que esperar.
    pub(super) fn encolar_sumas(
        &mut self,
        paths: Vec<VPath>,
        publicado: Option<Publicado>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let params = norte_proto::methods::FsChecksumParams {
            paths,
            algo: norte_proto::methods::ChecksumAlgo::Sha256,
        };
        let backend2 = Arc::clone(backend);
        let buzon2 = buzon.clone();
        tokio::spawn(async move {
            match backend2.checksum(params).await {
                Ok(task) => {
                    let _ = buzon2
                        .send(Mensaje::TaskNueva(Box::new((task, Vec::new(), None))))
                        .await;
                }
                Err(e) => {
                    let _ = buzon2.send(Mensaje::TaskFallida(Box::new(e))).await;
                }
            }
        });
        // La Task todavía no tiene id: lo que se apunta aquí es la INTENCIÓN,
        // y `apuntar_sumas` la casa con el id cuando la task nace.
        self.sumas_pendientes = Some(SumasEncoladas { publicado });
        self.decir("msg-checksum-started")
    }
}
