//! Create, delete, pack, split and change permissions.
//!
//! Part of `controller`: these are `Estado` methods, moved here without
//! touching them (ADR 0086). The only writer is still the actor.

// These modules are the same `impl Estado` split into pieces, so they use
// the same imports as the parent. Enumerating them here would be a
// forty-line list per file, in 32 files, that goes stale the moment the
// parent imports something — `super::*` tracks it on its own.
#[allow(clippy::wildcard_imports)]
use super::*;

/// A file being created in order to edit it (#290).
#[derive(Debug)]
pub(super) struct Creacion {
    /// The task creating it. `None` while it is being enqueued: the id does
    /// not exist until the daemon answers, and the gesture has already
    /// returned.
    pub(super) task: Option<u64>,
    /// What to open once that task finishes SUCCESSFULLY.
    path: VPath,
}

impl Estado {
    /// Creates the TYPED directory inside this other one.
    ///
    /// The name is validated HERE, with the same rule as any other segment:
    /// not empty, not `/`, not NUL, not `.`/`..`. A name that is not valid
    /// enqueues nothing and says so; the typed text is not lost because the
    /// dialog reopens with it.
    ///
    /// The same belt as rename: a TOUCHED name still carrying the
    /// replacement character is not written. The old asymmetry ("create has
    /// no seed to inherit residue from") was false about the ROUND TRIP: the
    /// host paints its own masked projection in the field, and the renderer
    /// re-seeds it with that if it had to rebuild the node — an approval
    /// dialog sneaking in on top is enough.
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

    /// The two pending actions that build files from what was TYPED: split
    /// by size and pack by name (#132, #290).
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
            // The caller already filtered; naming them here makes a third
            // one a compile error.
            _ => (None, Vec::new()),
        }
    }

    /// Splits `path` into chunks of the typed size (#132, #290).
    ///
    /// The size is read by the same function as the TUI: `10M` is 10 MiB and
    /// not ten million, which is what it means in a file manager. A zero is
    /// refused — zero-byte chunks never finish.
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

    /// Packs `sources` into the typed container (#132, #290).
    ///
    /// The FORMAT comes from the name and travels explicit: a name with an
    /// extension we do not know how to WRITE is refused here instead of
    /// packing into something nobody asked for — a `.rar` falls there,
    /// because it is delegated and only for reading.
    ///
    /// The base for the stored names is the pane's directory: whoever
    /// unpacks expects to see what was on screen, not absolute paths.
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

    /// Opens a delete confirmation. Does NOT delete.
    ///
    /// Every path — key, menu, gesture — goes through here. A destructive
    /// operation with two doors ends up with one that has no lock, and the
    /// forgotten one is always the one not used daily.
    pub(super) fn pedir_borrado(
        &mut self,
        permanente: bool,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let hueco = self.hueco();
        let mut paths: Vec<VPath> = hueco.pane.marked_paths();
        if paths.is_empty() {
            // With no marks, whatever is under the cursor. With no cursor,
            // nothing to delete: and that does not open a dialog over an
            // empty batch.
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
        // Is there a trash here? The terminal asks on delete and TWO things
        // come out of the answer: whether the delete is permanent, and
        // whether it is SAID. The window used to do neither, so it offered
        // the same dialog for "this can be recovered" and for "this cannot".
        //
        // **Three states, not two, and that is the part that matters.** The
        // terminal `await`s a fresh `capabilities` at the moment of
        // deletion, so its `is_ok_and` collapses either a real answer or a
        // real failure — never an "I haven't asked yet". Here it comes from
        // the slot's cache, which arrives AFTER the listing and on its own:
        // there is a whole window, between the rows being painted and the
        // answer coming back, during which nothing is known. And if the
        // request fails, nothing is known for the entire session.
        //
        // Turning that "unknown" into "no trash" would really delete in a
        // place that does have one. The asymmetry rules, and it runs the
        // opposite of how it looks: assuming a trash where there is none
        // costs an `Unsupported` and a `shift+F8`; assuming there is none
        // where there is one costs the bytes. So only an explicit NO makes
        // the deletion permanent.
        let dir = self.hueco().pane.dir().clone();
        let papelera = self
            .caps_de_ruta(&dir)
            .map(|c| c.flags.contains(norte_proto::CapabilityFlags::TRASH));
        let permanente = permanente || papelera == Some(false);
        // The body's names could come from a potential attacker: they are
        // painted with the canonical sanitizing and clamped, same as in the
        // listing.
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
            // A delete goes nowhere.
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
                    // The destructiveness is STATED in the dialog's own
                    // contract: the renderer does not have to guess which
                    // answer deletes.
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
            // "⚠ NO trash: this cannot be undone", with the terminal's key.
            // It goes through the same channel as a copy's warnings because
            // it is the same question — what happens to the bytes when this
            // finishes — and because a warning in the BODY can be
            // impersonated by a file name. A destructive button says the
            // answer deletes; this says there is no going back.
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

    /// The gestures that operate on what is MARKED — or on what is under the
    /// cursor — and launch a task: count, pack, unpack and test (#132, #139,
    /// #290).
    ///
    /// Together for the same reason as the layout ones: `aplicar_efecto` is
    /// a dispatcher and cannot grow one arm per new gesture.
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
            // The caller already filtered: naming them here is what makes
            // adding one more a compile error.
            _ => (self.aplicada(), Vec::new()),
        }
    }

    /// `pane.pack` (#132, #290): asks for the container's NAME.
    ///
    /// The name is typed because it is where the format comes from. Nothing
    /// else is validated here besides there being something to pack: the
    /// extension is resolved on confirm, which is when there is a name.
    pub(super) fn pedir_empaquetado(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // `marked_paths` falls back to the cursor with no marks, same as in
        // a transfer: a single source for "what this operates on".
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
        let location_line = Self::linea_de_ruta(&dir);
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
            body: vec![location_line],
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
            fields: Vec::new(),
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

    /// `pane.split-file` (#132, #290): asks for the chunks' SIZE.
    ///
    /// The chunks go to the destination pane, like a copy and for the same
    /// reason: splitting a gigabyte file in the place it already sits
    /// usually does not fit.
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
        let location_line = Self::linea_de_ruta(&dest_dir);
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id,
            title_key: "modal-split-title".to_owned(),
            destination: Some(location_line),
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
            fields: Vec::new(),
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

    /// `pane.combine-files` (#132, #290): joins the chunks starting from the
    /// `.001` under the cursor.
    ///
    /// Only from the FIRST one, and the rule lives in the shared crate:
    /// starting from `.007` would join half of a thing, and the core only
    /// looks forward.
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

    /// `pane.unpack` (#132, #290): copies the INSIDE of the container under
    /// the cursor to the destination pane.
    ///
    /// With no method of its own and no need for one: the copy engine
    /// accepts an archive's inside as a source, so this is the copy the
    /// reader could have made by hand — with its journal, its undo and its
    /// cancellation.
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
        // The SAME function that decides whether `Enter` goes into a
        // container (`norte_frontend::nav`): two extension tables would be
        // two places for one to fall out of sync, and then the same entry
        // navigates on one surface and does not unpack on the other.
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
        // A copy like any other, with `Fail` and its retry: if the
        // destination already has what is inside, the reader decides the
        // same way as in a transfer (#274).
        let a_la_cola = self.encolar;
        Self::lanzar_reintento(
            Reintento {
                from: raiz,
                to: destino,
                mover: false,
                // The one from the slot it is being unpacked from, captured
                // here: see the field.
                enc: self.hueco().pane.name_encoding(),
            },
            norte_proto::CollisionPolicy::Fail,
            a_la_cola,
            backend,
            buzon,
        );
        (self.aplicada(), self.decir("msg-unpack-started"))
    }

    /// `pane.test-archive` (#132, #290): tests the container under the
    /// cursor. Writes nothing; its result is the Task's outcome.
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
                // Testing changes nothing: there are no directories to
                // refresh.
                Ok(task) => Mensaje::TaskNueva(Box::new((task, Vec::new(), None))),
                Err(e) => Mensaje::TaskFallida(Box::new(e)),
            };
            let _ = buzon.send(mensaje).await;
        });
        (self.aplicada(), self.decir("msg-test-archive-started"))
    }

    pub(super) fn pedir_mkdir(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let dir = self.hueco().pane.dir().clone();
        let location_line = Self::linea_de_ruta(&dir);
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id,
            title_key: "modal-mkdir-title".to_owned(),
            // The directory it is created in is NOT a destination: it is
            // the context. A destination is where something that already
            // exists is MOVED to.
            destination: None,
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body: vec![location_line],
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
            // With a text field: that is what tells the renderer typing
            // happens here, without having to infer it from the title.
            input: Some(String::new()),
            input_hostile: false,
            input_secret: false,
            fields: Vec::new(),
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

    /// Asks for the octal MODE for what is marked (#314, ADR 0081).
    ///
    /// The field comes prefilled with the entry under the cursor's
    /// permissions **if the listing carries them** — it carries them when
    /// the column scheme requests `posix.mode` — and empty if not.
    /// Prefilling is not decoration: stripping the execute bit off something
    /// that had it, because it was not visible which it was, is exactly the
    /// mistake a blank field invites.
    ///
    /// The body says how MANY entries this is about, for the same reason as
    /// the terminal: typing a mode believing it applies to one and having it
    /// apply to fifty is what this dialog has to make hard.
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
            fields: Vec::new(),
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

    /// Sends the permission change the dialog confirmed (#314).
    ///
    /// What was typed is read with the SAME parser as the terminal
    /// ([`norte_frontend::chmod::parse_mode`]): an invalid mode is reported
    /// and the dialog stays open with what was written, which is what every
    /// prompt here does.
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
        // The directories to refresh: the PARENTS of what changes, because
        // what looks different after a chmod is their rows' permissions
        // column.
        let mut refrescar: Vec<VPath> = targets.iter().filter_map(VPath::parent).collect();
        refrescar.sort();
        refrescar.dedup();
        let params = norte_proto::methods::FsSetModeParams {
            paths: targets,
            mode,
            // The window does not offer recursive yet (#315): its dialog is
            // a text field and this is a checkbox. It is set to `false`
            // explicitly and not by default so that the day the checkbox
            // shows up nobody has to go hunting for where it was decided.
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

    /// Asks for a NEW file's name to edit it (#290).
    ///
    /// Only on a local pane: what opens afterward is the desktop
    /// application, and `xdg-open` cannot be given an `sftp://`. It is
    /// stated BEFORE typing the name, which is when it still does some
    /// good.
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
        let location_line = Self::linea_de_ruta(&dir);
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
            body: vec![location_line],
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
            fields: Vec::new(),
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

    /// Creates the empty file and NOTES that it has to be opened once it
    /// exists.
    ///
    /// Opening it here would be opening something not yet on disk: creation
    /// is a Task, and until its outcome there is no file to hand the
    /// desktop.
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

    /// The freshly created file exists: ASKS whether it is still a file and,
    /// if it is, opens it with the desktop.
    ///
    /// Only with a GOOD outcome. Opening after a failure would launch the
    /// editor over a file that is not there, and what that editor shows — an
    /// empty buffer that creates the file on save — would look like it
    /// worked.
    ///
    /// # Why there is an `fs.stat` in between (#303)
    ///
    /// norte ANNOUNCES the name by creating it, and between that and
    /// `xdg-open` there is a window in which anyone writing to that
    /// directory can unlink it and leave a symlink: the human would end up
    /// writing to a file nobody showed them, and the `Created` entry's
    /// `undo` goes by PATH and not by identity. `fs.stat` is `lstat` — it
    /// describes the link, not its target — so the check sees what is
    /// really there.
    ///
    /// **It narrows the window, it does not close it**: there is still a gap
    /// between the `stat` and the `open`, and closing it would require
    /// handing a descriptor to the desktop program, which `xdg-open` does
    /// not accept. It is the SAME decision the TUI makes in
    /// `gestures::edit_created`, and it lives here for that reason: a
    /// decision duplicated between frontends silently drifts apart (ADR
    /// 0077).
    ///
    /// The answer comes back through the mailbox as just another message
    /// ([`Mensaje::CreadoComprobado`]): the state is touched by a single
    /// writer, and waiting here would block the whole actor for a round trip
    /// to the daemon.
    pub(super) fn abrir_lo_creado(
        &mut self,
        p: &norte_proto::TaskProgress,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        // By ID, not by kind. `task.progress` is broadcast to EVERY human
        // connection, so an `fs.create` from the TUI — or from another
        // window on the same daemon — used to arrive here, swallow the
        // intent and open a file that was not there yet: exactly the bug
        // this order exists to prevent. And the other way around, the one
        // that really was created never opened.
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
            // With no attributes: the only thing being asked is WHAT it is,
            // and requesting attributes would be provider work nobody is
            // going to read.
            let veredicto = match backend.stat(path.clone(), Vec::new()).await {
                Ok(e) => Veredicto::from(e.kind == norte_proto::EntryKind::File),
                // `NotFound` is an ANSWER — there is nothing there — and also
                // the most likely outcome of an attack: unlink and do not
                // replace. Anything else is not the same as tampering: a
                // handed-off daemon or a timeout are not manipulation, and
                // saying yes is a false accusation that teaches ignoring the
                // real warning.
                Err(Error::NotFound) => Veredicto::YaNoEsElFichero,
                Err(_) => Veredicto::NoSeSabe,
            };
            let _ = buzon
                .send(Mensaje::CreadoComprobado(Box::new((path, veredicto))))
                .await;
        });
    }

    /// The `fs.stat` answer for [`Self::abrir_lo_creado`]: opens, or says why
    /// not.
    ///
    /// A single message for the three causes that are the SAME (a link, a
    /// folder, no longer there): saying which one would confirm to whoever
    /// planted the link that their link is in place. Not being able to ask
    /// is something else and it is stated separately.
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

    /// Enqueues one Task per entry and hooks its progress to the actor.
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

    /// The SECOND lock for read-only mode, over the SINGLE point where every
    /// mutation is launched.
    ///
    /// Cheap, and unreachable today: in read-only, no mutating `Pendiente`
    /// is ever born and the approval channel is not even taken. "Unreachable
    /// today" is exactly what stops being true the day someone adds the next
    /// dialog, and this is the door it would come through.
    ///
    /// Closes the dialog on rejecting it: leaving it open would invite
    /// pressing again what is not going to happen.
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
                        // Granting capabilities is the security decision of
                        // the extensions system: a window declaring itself
                        // read-only does not make it.
                        | Pendiente::AprobarExtension { .. }
                        // Uninstalling DELETES configuration files.
                        | Pendiente::DesinstalarExtension { .. }
                        // Undoing a session WRITES: it moves files back and
                        // deletes what the agent created.
                        | Pendiente::DeshacerSesion { .. }
                        // And undoing up to a point, for the same reason.
                        | Pendiente::DeshacerHasta { .. }
                        // Requesting a plan writes nothing to disk, and it
                        // still counts: it sends a directory's contents to a
                        // model, which is not something a window declaring
                        // itself read-only should do.
                        | Pendiente::InstruccionIa { .. }
                        // And the template batch (#310) ends in a rename.
                        | Pendiente::PlantillaLote { .. }
                        // Neither does this: the query leaves the process.
                        | Pendiente::ConsultaSemantica // `EntregarSecreto` is NOT here, and it is deliberate
                                                       // (#327): delivering the password enables READING a
                                                       // place that could not be entered, which is exactly
                                                       // what a read-only window does. Vetoing it would
                                                       // leave the `prompt` connection unusable in read-only
                                                       // for no gain — the secret goes to the daemon's
                                                       // memory, not to disk, and whatever gets authorized
                                                       // afterward is still governed by policy.
                                                       //
                                                       // Stated here because this function's rustdoc warns
                                                       // that this is the door the next dialog would come
                                                       // through, and silence is indistinguishable from an
                                                       // oversight.
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
