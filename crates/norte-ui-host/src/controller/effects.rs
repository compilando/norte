//! Resolving an `Efecto` from the shared catalogue.
//!
//! Part of `controller`: these are methods of `Estado`, moved here without
//! touching them (ADR 0086). The only writer is still the actor.

// These modules are the same `impl Estado` split into pieces, so they use
// the same imports as the parent. Listing them here would be a forty-line
// list per file, across 32 files, that goes out of sync the moment the
// parent imports something — `super::*` keeps it in sync on its own.
#[allow(clippy::wildcard_imports)]
use super::*;

impl Estado {
    /// Runs what a command asks over the focused slot.
    ///
    /// It is the SAME path the renderer's direct actions take (a click, a
    /// drag): a key and a gesture that mean the same thing doing the same
    /// thing cannot depend on someone remembering to keep them in sync.
    ///
    /// **It is a DISPATCHER, and that is why it grows one line per new
    /// gesture.** What the lint measures here says nothing about its
    /// complexity: each arm is a name and a call, and the exhaustive `match`
    /// is exactly what makes adding an `Efecto` without handling it a
    /// compile error. Splitting the arms into functions to get under the
    /// threshold hides that split in a second place without improving
    /// anything — it has already been done three times, and all three times
    /// the next gesture brushed against it again. The groups that DO mean
    /// something — what opens, what lays out, what acts on entries — are
    /// grouped; the rest stays here in plain view.
    #[expect(
        clippy::too_many_lines,
        reason = "exhaustive dispatcher: one arm per gesture, no logic inside"
    )]
    pub(super) fn aplicar_efecto(
        &mut self,
        efecto: Efecto,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // Focus can be on a panel that is NOT a listing and that DOES take
        // keys — today, the process one. Then "down" is going down THROUGH
        // IT: until now the `active` role painted it focused while the
        // arrows moved the listing next to it, which is half a function and
        // the half that is not visible.
        //
        // It is decided by EFFECT and not by key, so `j`, `↓` and `g g` work
        // the same: the keymap says which command it is, and the surface
        // with focus says what it means there.
        // Cancel is decided BEFORE anything: with the process panel focused
        // and the board empty, `efecto_en_panel_enfocado` answers "applied"
        // to any effect, and that would turn "there is nothing to stop" into
        // silence.
        if matches!(efecto, Efecto::CancelarTask) {
            return self.cancelar_por_comando();
        }
        if matches!(efecto, Efecto::PausarTask) {
            return self.pausar_por_comando(mailbox);
        }
        if matches!(efecto, Efecto::ReintentarTask) {
            return self.reintentar_por_comando(backend, mailbox);
        }
        if matches!(efecto, Efecto::AlternarCola) {
            return self.alternar_cola();
        }
        if let Efecto::MoverEnCola { arriba } = efecto {
            return self.mover_en_cola_por_comando(arriba, mailbox);
        }
        // Walking and discarding the board, for the same reason and before
        // focus: they are BOARD commands, not the panel that paints it, and
        // with the process panel closed they still have to mean the same
        // thing.
        if let Efecto::TaskVecina { atras } = efecto {
            return self.mover_en_tablero(atras);
        }
        if matches!(efecto, Efecto::DescartarTask) {
            return self.descartar_task();
        }
        if let Some(outcome) = self.efecto_en_panel_enfocado(efecto) {
            return outcome;
        }
        // `Enter` on the timeline (#359) is "come back here": it asks with
        // the count before undoing anything.
        if self.linea_tiene_el_foco() && matches!(efecto, Efecto::Entrar) {
            return self.preguntar_deshacer_hasta();
        }
        if self.sitios_tienen_el_foco() && matches!(efecto, Efecto::Entrar | Efecto::Marcar) {
            // Entering and collapsing are handled by the side bar, and the
            // `cd` that comes out goes to the LISTING through the same path
            // as any other: that is what makes having it open not change
            // where operations go.
            return self.activar_sitio_del_cursor(backend, mailbox);
        }
        let slot = self.activo();
        match efecto {
            Efecto::Cursor(_)
            | Efecto::Pagina(_)
            | Efecto::Extremo { .. }
            | Efecto::Entrar
            | Efecto::Subir
            | Efecto::Rastro { .. }
            | Efecto::Marcar
            | Efecto::MarcarTodo
            | Efecto::InvertirMarcas
            | Efecto::MarcarExtension { .. }
            | Efecto::MarcarClase { .. }
            | Efecto::RestaurarMarcas
            | Efecto::MarcarSubiendo
            | Efecto::MarcarPagina { .. }
            | Efecto::MarcarHastaElBorde { .. }
            | Efecto::DesmarcarTodo => self.efecto_de_listado(efecto, slot, backend, mailbox),
            Efecto::Foco {
                atras,
                solo_listados,
            } => self.mover_foco(atras, solo_listados, backend, mailbox),
            Efecto::Destino => self.designar_destino(),
            Efecto::SaltoAtras => self.saltar_al_punto(backend, mailbox),
            Efecto::FijarSalto => self.fijar_punto_de_salto(),
            // Handled above, before the focused panel. The arm exists
            // because the `match` is exhaustive on purpose: a new effect
            // with no place has to be a compile error.
            // The BOARD's three are handled before getting here: they do not
            // depend on which panel has focus.
            Efecto::CancelarTask | Efecto::TaskVecina { .. } | Efecto::DescartarTask => {
                self.cancelar_por_comando()
            }
            Efecto::PausarTask => self.pausar_por_comando(mailbox),
            Efecto::ReintentarTask => self.reintentar_por_comando(backend, mailbox),
            Efecto::AlternarCola => self.alternar_cola(),
            Efecto::MoverEnCola { arriba } => self.mover_en_cola_por_comando(arriba, mailbox),
            Efecto::Tamano(_)
            | Efecto::Igualar
            | Efecto::Girar
            | Efecto::Disposiciones
            | Efecto::Partir { .. }
            | Efecto::CerrarHueco
            | Efecto::AlternarHueco { .. }
            | Efecto::PestanaNueva
            | Efecto::CerrarPestana
            | Efecto::CiclarPestana { .. }
            | Efecto::MoverPestana { .. }
            | Efecto::IrAPestana { .. } => {
                self.efecto_de_disposicion(efecto, backend, mailbox)
            }
            Efecto::Ordenar(col) => self.ordenar_por_columna(slot, col.into()),
            Efecto::Refrescar => self.refrescar_visibles(backend, mailbox),
            Efecto::AlternarOcultos => self.alternar_ocultos(),
            Efecto::CiclarEncoding => self.ciclar_encoding(),
            Efecto::Espejo | Efecto::EspejoObjetivo | Efecto::Traer | Efecto::Intercambiar => {
                self.gesto_de_panel(efecto, backend, mailbox)
            }
            // Apart from the group above: those NAVIGATE, and this one only
            // flips a switch.
            Efecto::EspejoPermanente => self.alternar_espejo_permanente(),
            Efecto::VolumenesDeLado { derecha } => {
                self.abrir_volumenes_de_lado(derecha, backend, mailbox)
            }
            Efecto::Columnas => self.abrir_columnas(),
            Efecto::Buscar => self.pedir_busqueda(),
            Efecto::BuscarRapido => self.buscar_rapido(),
            Efecto::CrearDirectorio
            | Efecto::CrearFichero
            | Efecto::Borrar { .. }
            | Efecto::Transferir { .. }
            | Efecto::Renombrar
            | Efecto::RenameIa
            // Phase 8: organize creates folders and moves, so a read-only
            // window does not request it either.
            | Efecto::Organizar
            | Efecto::RenameLote
            // #314: changing permissions writes, so a read-only window does
            // not do it either.
            | Efecto::Permisos
            | Efecto::BuscarSemantica
            | Efecto::Sincronizar
            // The two that LAUNCH a process: what that process does with the
            // files is not this window's decision.
            | Efecto::AbrirExterno
            | Efecto::EditarExterno
            | Efecto::CompararFicheros
            | Efecto::Terminal
            // The terminal PANEL (#362), and with more reason than
            // `Terminal`: that one launches an outside emulator, and this one
            // runs a shell INSIDE the window. In one that promises not to
            // write, this would be the widest possible back door — anything
            // at all is typed in there.
            //
            // It goes in THIS arm and not the layout one, where it used to be:
            // that one runs unconditionally, so the guard was never reached.
            // And the keymap's filter is not enough, because the panel bar's
            // button, the menu entry and the status bar's buttons call
            // `efecto_de` without going through it.
            | Efecto::AbrirTerminal
            // Phase 9: the handoff writes the session, releases it and closes
            // the window. None of the three are done by a read-only one.
            | Efecto::Relevo
                if self.efectos == crate::commands::Efectos::SoloLectura =>
            {
                Self::no_muta()
            }
            // And outside read-only, the panel opens. It goes here and not
            // with the layout because from there the guard above is not
            // reached.
            Efecto::AbrirTerminal => self.abrir_panel_de_terminal(backend, mailbox),
            // Copying the path touches nothing and goes in both modes:
            // putting text on the clipboard is as read-only as reading a
            // name.
            Efecto::CopiarRuta => self.copiar_rutas(),
            Efecto::Sumas { verificar } => self.lanzar_sumas(verificar, backend, mailbox),
            Efecto::MarcarPatron { marcar } => self.pedir_patron(marcar),
            Efecto::AbrirExterno => self.abrir_externo(),
            Efecto::EditarExterno => self.editar_externo(),
            Efecto::CompararFicheros => self.comparar_ficheros(),
            Efecto::Terminal => self.abrir_terminal(),
            Efecto::Relevo => self.pedir_relevo(backend, mailbox),
            Efecto::Comparar => self.pedir_comparacion(backend, mailbox),
            Efecto::Desconectar => self.desconectar(backend, mailbox),
            Efecto::TamanoDeDirectorio
            | Efecto::Empaquetar
            | Efecto::Desempaquetar
            | Efecto::ComprobarArchivo
            | Efecto::PartirFichero
            | Efecto::Juntar => self.efecto_sobre_entradas(efecto, backend, mailbox),
            // Like comparing: it needs the backend because it goes out to ask
            // as soon as it opens, and the panel is born saying it is
            // planning.
            Efecto::Sincronizar => self.pedir_sincronizacion(backend, mailbox),
            Efecto::Paleta
            | Efecto::IrA
            | Efecto::Ayuda
            | Efecto::Ajustes
            | Efecto::Extensiones
            | Efecto::Agentes
            | Efecto::Tema
            | Efecto::Menu
            | Efecto::Salir
            | Efecto::PerfilElegir
            | Efecto::PerfilGuardarComo
            | Efecto::PerfilVecino { .. }
            | Efecto::Volumenes
            | Efecto::Conexiones
            // History and the hotlist are two other selectors: they go with
            // the rest of what OPENS, and not each with its own arm — this
            // `match` dispatches, and grows one arm per new gesture.
            | Efecto::Historial
            | Efecto::Hotlist
            | Efecto::Populares
            | Efecto::HistorialDeLado { .. }
            | Efecto::Ver => self.efecto_que_abre(efecto, backend, mailbox),
            Efecto::CrearDirectorio
            | Efecto::CrearFichero
            | Efecto::Borrar { .. }
            | Efecto::Transferir { .. }
            | Efecto::Renombrar
            | Efecto::RenameIa
            // Phase 8: organize creates folders and moves, so a read-only
            // window does not request it either.
            | Efecto::Organizar
            | Efecto::RenameLote
            | Efecto::Permisos
            | Efecto::BuscarSemantica => self.efecto_que_muta(efecto, backend, mailbox),
        }
    }

    /// The effects that move the CURSOR or the listing: walking, entering,
    /// going up, going back and marking. None of this writes.
    pub(super) fn efecto_de_listado(
        &mut self,
        efecto: Efecto,
        slot: u32,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match efecto {
            Efecto::Cursor(delta) => self.aplicar(
                &UiAction::MoveCursor {
                    slot_id: slot,
                    delta,
                },
                backend,
                mailbox,
            ),
            Efecto::Pagina(pages) => {
                let rows = i64::from(self.hueco().visibles.max(1));
                self.aplicar(
                    &UiAction::MoveCursor {
                        slot_id: slot,
                        delta: pages.saturating_mul(rows),
                    },
                    backend,
                    mailbox,
                )
            }
            Efecto::Extremo { al_final } => {
                if al_final {
                    self.hueco_mut().pane.end();
                } else {
                    self.hueco_mut().pane.home();
                }
                (self.aplicada(), vec![self.parche_cursor()])
            }
            Efecto::Entrar => {
                // A key acts on what is under the cursor RIGHT NOW, so the
                // generation is this very instant's.
                let key = RowKey(self.hueco().pane.cursor() as u64);
                let generation = self.hueco().pane.listing_epoch();
                self.navegacion(
                    &UiAction::Activate {
                        slot_id: slot,
                        key,
                        generation,
                    },
                    backend,
                    mailbox,
                )
            }
            Efecto::Subir => self.navegacion(&UiAction::Parent { slot_id: slot }, backend, mailbox),
            Efecto::Rastro { atras } => self.navegacion(
                &UiAction::History {
                    slot_id: slot,
                    back: atras,
                },
                backend,
                mailbox,
            ),
            Efecto::Marcar => {
                let key = RowKey(self.hueco().pane.cursor() as u64);
                let generation = self.hueco().pane.listing_epoch();
                self.marcar(slot, key, generation)
            }
            Efecto::DesmarcarTodo => {
                self.hueco_mut().pane.clear_marks();
                (self.aplicada(), vec![self.parche_filas()])
            }
            Efecto::MarcarTodo => {
                self.hueco_mut().pane.mark_all();
                (self.aplicada(), vec![self.parche_filas()])
            }
            Efecto::InvertirMarcas => {
                self.hueco_mut().pane.invert_marks();
                (self.aplicada(), vec![self.parche_filas()])
            }
            // #313: the rule for what "the same extension" is, what counts as
            // a file, and what gets restored lives in `PaneState`, so nothing
            // is decided here — it is the same model as the terminal's.
            Efecto::MarcarExtension { marcar } => {
                self.hueco_mut().pane.mark_same_extension(marcar);
                (self.aplicada(), vec![self.parche_filas()])
            }
            Efecto::MarcarClase { dirs } => {
                self.hueco_mut().pane.mark_kind(dirs);
                (self.aplicada(), vec![self.parche_filas()])
            }
            Efecto::RestaurarMarcas => {
                self.hueco_mut().pane.restore_previous_marks();
                (self.aplicada(), vec![self.parche_filas()])
            }
            // Marking WHILE MOVING: the whole rule — what it advances to,
            // what decides whether the span gets marked or unmarked, and that
            // both edge ones clear the other side — lives in `PaneState`,
            // same as in the terminal. The window repaints rows AND cursor
            // because these DO move it (except the edge ones, which on
            // purpose do not).
            Efecto::MarcarSubiendo => {
                self.hueco_mut().pane.toggle_mark_and_retreat();
                (self.aplicada(), vec![self.parche_filas()])
            }
            Efecto::MarcarPagina { abajo } => {
                let n = self.hueco().pane.page_step();
                self.hueco_mut().pane.toggle_mark_page(n, abajo);
                (self.aplicada(), vec![self.parche_filas()])
            }
            Efecto::MarcarHastaElBorde { arriba } => {
                if arriba {
                    self.hueco_mut().pane.mark_to_top();
                } else {
                    self.hueco_mut().pane.mark_to_bottom();
                }
                (self.aplicada(), vec![self.parche_filas()])
            }
            // The rest do not get here: the `match` above dispatches them.
            _ => Self::no_muta(),
        }
    }

    /// Sends a NATIVE effect to the hosting process, if anyone is there.
    ///
    /// `false` = nobody is listening. It is not a host error: a frontend that
    /// does not know how to do these things does not subscribe, and then the
    /// honest thing is to tell whoever pressed the key that it does not
    /// happen here, instead of acknowledging something that is not going to
    /// occur.
    pub(super) fn nativo(&self, efecto: crate::dto::NativeEffect) -> bool {
        self.escritorio
            .nativos
            .as_ref()
            .is_some_and(|tx| tx.send(efecto).is_ok())
    }

    /// The paths of what is MARKED — or of the selected one, if there are no
    /// marks — to the clipboard.
    ///
    /// Marked first and the cursor as a fallback: it is the same rule as copy
    /// and move, and having two answers to "what does this act on" depending
    /// on the command is what makes a gesture apply to something else.
    ///
    /// In BYTES and in native form when there is one: what gets pasted has to
    /// open the same file, and a lossy-decoded path opens a different one.
    pub(super) fn copiar_rutas(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let slot = self.hueco();
        // `marked_paths` already falls back to the cursor when there are no
        // marks: it is the same rule as copy and move, and having two
        // answers to "what does this act on" depending on the command is
        // what applies a gesture to something else.
        let paths: Vec<VPath> = slot.pane.marked_paths();
        if paths.is_empty() {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-nothing-selected".to_owned(),
                },
                Vec::new(),
            );
        }
        let count = paths.len();
        let bytes = norte_frontend::shell::clipboard_bytes(&paths);
        if !self.nativo(crate::dto::NativeEffect::CopyBytes { bytes, count }) {
            return Self::sin_escritorio();
        }
        let outgoing = self.decir_con("msg-paths-copied", &[("n", &count.to_string())]);
        (self.aplicada(), outgoing)
    }

    /// Opens what is selected with the application the desktop chooses.
    pub(super) fn abrir_externo(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(path) = self.hueco().pane.selected().map(|e| e.path.clone()) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-nothing-selected".to_owned(),
                },
                Vec::new(),
            );
        };
        // Only what is on THIS disk: `xdg-open` cannot be handed an
        // `sftp://`, and pretending otherwise would open something else — or
        // nothing — without saying so.
        if !norte_frontend::shell::is_local(&path) {
            let outgoing = self.decir("host-not-local");
            return (
                ActionAck::Unavailable {
                    reason_key: "host-not-local".to_owned(),
                },
                outgoing,
            );
        }
        // `openers.toml` rules, and the desktop is the LAST resort (#28). The
        // table used to be read only by the terminal, so "PDFs with zathura"
        // held in `ntc` and not in the window: a whole documented feature
        // honored by a single surface.
        if let Some(effect) = self.programa_declarado(&path) {
            if !self.nativo(effect) {
                return Self::sin_escritorio();
            }
            return (self.aplicada(), self.decir("msg-opening-external"));
        }
        if !self.nativo(crate::dto::NativeEffect::OpenPath { path }) {
            return Self::sin_escritorio();
        }
        (self.aplicada(), self.decir("msg-opening-external"))
    }

    /// The program `openers.toml` declares for this file, already ready to
    /// run. `None` if there is no rule for its mimetype, or if the binary is
    /// not there.
    ///
    /// The mimetype is guessed from the NAME with the same function as the
    /// terminal's (`openers::guess_mime`): who opens what cannot depend on
    /// which surface asks for it.
    fn programa_declarado(&self, path: &VPath) -> Option<crate::dto::NativeEffect> {
        let native_path = norte_vfs::native::vpath_to_native(path).ok()?;
        let mime = norte_frontend::openers::guess_mime(
            path.file_name()
                .map_or(&[][..], norte_proto::Segment::as_bytes),
        );
        let opener = self.config.openers.resolve(mime)?;
        // `%d` is the PANEL's directory, not the file's: the child opens
        // where the reader is looking (#144).
        let dir =
            norte_vfs::native::vpath_to_native(self.hueco().pane.dir()).unwrap_or_else(|_| {
                native_path
                    .parent()
                    .map(std::path::Path::to_path_buf)
                    .unwrap_or_default()
            });
        let argv = Self::argv_resuelto(opener.argv(&[&native_path], &dir))?;
        Some(crate::dto::NativeEffect::RunProgram {
            title_key: "program-output-open".to_owned(),
            argv,
            cwd: Some(bytes_de_ruta(&dir)),
            detached: opener.detached(),
        })
    }

    /// An already-interpolated argv, with its program resolved to an ABSOLUTE
    /// path and in bytes, ready for `NativeEffect::RunProgram`.
    ///
    /// It is resolved before giving it a `cwd` (ADR 0082): a bare name with
    /// `current_dir` set would be looked up in the directory currently being
    /// viewed. A binary that is not there returns `None`, and the caller
    /// decides.
    ///
    /// Returns the argv and not the whole effect on purpose: `title_key`
    /// stays as a LITERAL in each caller, which is what `catalogo_del_host`'s
    /// sweep can follow. A key hidden behind a parameter is a key that will
    /// paint as its own identifier the day it is missing.
    fn argv_resuelto(mut argv: Vec<std::ffi::OsString>) -> Option<Vec<Vec<u8>>> {
        use std::os::unix::ffi::OsStrExt as _;
        let program = argv
            .first()
            .and_then(|p| norte_frontend::openers::resolve_program(p))?;
        argv[0] = program.into_os_string();
        Some(argv.iter().map(|a| a.as_bytes().to_vec()).collect())
    }

    /// `pane.edit`: the editor `[ui] editor` names, and if there is none,
    /// open.
    ///
    /// **`$EDITOR` is not used, and that is still deliberate**: it is a
    /// terminal editor and this window has nowhere to put one (#290). What
    /// was not deliberate was also ignoring `[ui] editor`, which names an
    /// explicit program and can perfectly well be graphical — its sibling key
    /// `[ui] diff` IS honored by this window, with this same machinery.
    pub(super) fn editar_externo(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let configured = self
            .config
            .common
            .ui_editor
            .as_ref()
            .filter(|c| !c.is_empty())
            .cloned();
        let Some(template) = configured else {
            return self.abrir_externo();
        };
        let Some(path) = self.hueco().pane.selected().map(|e| e.path.clone()) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-nothing-selected".to_owned(),
                },
                Vec::new(),
            );
        };
        let Ok(native_path) = norte_vfs::native::vpath_to_native(&path) else {
            // A local editor cannot open an `sftp://`, same as `xdg-open`: it
            // is said, instead of launching blindly.
            let outgoing = self.decir("host-not-local");
            return (
                ActionAck::Unavailable {
                    reason_key: "host-not-local".to_owned(),
                },
                outgoing,
            );
        };
        let dir =
            norte_vfs::native::vpath_to_native(self.hueco().pane.dir()).unwrap_or_else(|_| {
                native_path
                    .parent()
                    .map(std::path::Path::to_path_buf)
                    .unwrap_or_default()
            });
        let template = norte_frontend::openers::expand_argv(&template, &[&native_path], &dir);
        let Some(argv) = Self::argv_resuelto(template) else {
            let outgoing = self.decir("host-program-missing");
            return (
                ActionAck::Unavailable {
                    reason_key: "host-program-missing".to_owned(),
                },
                outgoing,
            );
        };
        let effect = crate::dto::NativeEffect::RunProgram {
            title_key: "program-output-edit".to_owned(),
            argv,
            cwd: Some(bytes_de_ruta(&dir)),
            detached: self.config.common.ui_editor_detached.unwrap_or(false),
        };
        if !self.nativo(effect) {
            return Self::sin_escritorio();
        }
        (self.aplicada(), self.decir("msg-opening-external"))
    }

    /// Opens a terminal sitting in the active panel's directory.
    pub(super) fn abrir_terminal(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let dir = self.hueco().pane.dir().clone();
        if !norte_frontend::shell::is_local(&dir) {
            // A terminal sits in a filesystem directory: over an `sftp://`
            // there is nowhere to sit it, and opening it in `$HOME` without
            // saying anything would be opening it somewhere else.
            let outgoing = self.decir("host-not-local");
            return (
                ActionAck::Unavailable {
                    reason_key: "host-not-local".to_owned(),
                },
                outgoing,
            );
        }
        if !self.nativo(crate::dto::NativeEffect::OpenTerminal { dir }) {
            return Self::sin_escritorio();
        }
        (self.aplicada(), self.decir("msg-opening-terminal"))
    }

    /// Compares TWO files (#312) with `[ui] diff`'s program.
    ///
    /// WHICH two is decided by `norte_frontend::diffpair` — what is marked,
    /// or this one against the one across from it — and WHICH program is
    /// decided by the same configuration as the terminal's, with the same
    /// interpolation (`openers::expand_argv`) and the same default value.
    /// What changes is how it runs: the terminal suspends and waits for a
    /// key; here it is run by the host, detached if `[ui] diff_detached`
    /// says the comparator opens a window, and waited for and its output
    /// captured if not.
    pub(super) fn comparar_ficheros(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        use std::os::unix::ffi::OsStrExt as _;
        let here = self.hueco();
        let marked: Vec<&norte_proto::Entry> = here.pane.marked_entries();
        let there = self
            .roles
            .get(norte_frontend::layout::RoleId::Target)
            .and_then(|SlotId(s)| self.huecos.get(&s))
            .and_then(|h| h.pane.selected());
        let pair = norte_frontend::diffpair::pair(&marked, here.pane.selected(), there);
        let (a, b) = match pair {
            Ok(p) => p,
            Err(e) => {
                let key = e.message_key();
                let outgoing = self.decir(key);
                return (
                    ActionAck::Unavailable {
                        reason_key: key.to_owned(),
                    },
                    outgoing,
                );
            }
        };
        let dir = self.hueco().pane.dir().clone();
        let (Ok(native_a), Ok(native_b), Ok(native_dir)) = (
            norte_vfs::native::vpath_to_native(&a),
            norte_vfs::native::vpath_to_native(&b),
            norte_vfs::native::vpath_to_native(&dir),
        ) else {
            let outgoing = self.decir("host-not-local");
            return (
                ActionAck::Unavailable {
                    reason_key: "host-not-local".to_owned(),
                },
                outgoing,
            );
        };
        let configured = self
            .config
            .common
            .ui_diff
            .as_ref()
            .filter(|c| !c.is_empty())
            .cloned();
        let detached = configured.is_some() && self.config.common.ui_diff_detached.unwrap_or(false);
        let template = configured.unwrap_or_else(|| {
            norte_frontend::diffpair::DEFAULT_ARGV
                .iter()
                .map(|s| (*s).to_owned())
                .collect()
        });
        let mut argv =
            norte_frontend::openers::expand_argv(&template, &[&native_a, &native_b], &native_dir);
        // The program is resolved to an absolute path BEFORE giving it a
        // `cwd` (ADR 0082): a bare name with `current_dir` set would be
        // looked up in the directory currently being viewed.
        let Some(program) = argv
            .first()
            .and_then(|p| norte_frontend::openers::resolve_program(p))
        else {
            let outgoing = self.decir("host-program-missing");
            return (
                ActionAck::Unavailable {
                    reason_key: "host-program-missing".to_owned(),
                },
                outgoing,
            );
        };
        argv[0] = program.into_os_string();
        let effect = crate::dto::NativeEffect::RunProgram {
            title_key: "program-output-compare".to_owned(),
            argv: argv.iter().map(|a| a.as_bytes().to_vec()).collect(),
            cwd: Some(native_dir.as_os_str().as_bytes().to_vec()),
            detached,
        };
        if !self.nativo(effect) {
            return Self::sin_escritorio();
        }
        (self.aplicada(), self.decir("msg-opening-external"))
    }

    /// What a program that ran while being waited for printed (#312): masked
    /// line by line, clamped, and shown.
    pub(super) fn programa_terminado(
        &mut self,
        title_key: &str,
        command: &str,
        output: &[u8],
        truncated: bool,
        failed: bool,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // Same caps as an extension's output: it is the same class of text,
        // from a different program.
        const MAX_LINEAS: usize = 2000;
        let text = String::from_utf8_lossy(output);
        let mut lines = Vec::new();
        let mut hostile = false;
        for line in text.lines().take(MAX_LINEAS) {
            let (displayable, flagged) = norte_frontend::display_name(line.as_bytes());
            hostile |= flagged;
            lines.push(clamp_display(displayable));
        }
        let was_truncated = truncated || text.lines().nth(MAX_LINEAS).is_some();
        let (cmd, cmd_hostile) = norte_frontend::display_name(command.as_bytes());
        self.escritorio.programa = Some(crate::dto::ProgramOutputView {
            // The key comes BACK from whoever is hosting: it is checked
            // against the ones this host emits, and whatever is not
            // recognized falls back to the generic one — an outside key does
            // not paint as its own identifier.
            title_key: match title_key {
                "program-output-compare" => "program-output-compare".to_owned(),
                _ => "program-output-title".to_owned(),
            },
            command: crate::dto::MaskedTextView {
                text: clamp_display(cmd),
                hostile: cmd_hostile,
            },
            lines,
            text_hostile: hostile,
            truncated: was_truncated,
            failed,
        });
        let change = ViewChange::ProgramOutput {
            output: self.escritorio.programa.clone(),
        };
        (self.aplicada(), vec![self.parche(vec![change])])
    }

    /// Closes a program's output panel.
    pub(super) fn cerrar_salida_de_programa(
        &mut self,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.escritorio.programa = None;
        (
            self.aplicada(),
            vec![self.parche(vec![ViewChange::ProgramOutput { output: None }])],
        )
    }

    /// Nobody listens to native effects: it is SAID.
    pub(super) fn sin_escritorio() -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        (
            ActionAck::Unavailable {
                reason_key: "host-no-desktop".to_owned(),
            },
            Vec::new(),
        )
    }

    /// The effects that open a SCREEN over the listing and touch nothing.
    pub(super) fn efecto_que_abre(
        &mut self,
        efecto: Efecto,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match efecto {
            Efecto::Paleta => self.abrir_paleta(backend, mailbox),
            Efecto::IrA => self.abrir_ir_a(backend, mailbox),
            Efecto::Ayuda => self.abrir_ayuda(backend, mailbox),
            Efecto::Ajustes => self.abrir_ajustes(),
            Efecto::Extensiones => self.abrir_extensiones(backend, mailbox),
            Efecto::Agentes => self.abrir_agentes(),
            Efecto::Tema => self.abrir_tema(),
            Efecto::Menu => self.abrir_menu(),
            Efecto::Salir => self.pedir_salir(),
            Efecto::PerfilElegir => self.pedir_perfiles(None, mailbox),
            Efecto::PerfilGuardarComo => self.pedir_guardar_perfil(),
            Efecto::PerfilVecino { atras } => self.pedir_perfiles(Some(!atras), mailbox),
            Efecto::Volumenes => self.abrir_volumenes(backend, mailbox),
            Efecto::Conexiones => self.abrir_conexiones(backend, mailbox),
            Efecto::Historial => self.abrir_historial(),
            Efecto::Hotlist => self.abrir_hotlist(),
            Efecto::Populares => self.abrir_populares(),
            Efecto::HistorialDeLado { derecha } => self.abrir_historial_de_lado(derecha),
            Efecto::Ver => self.pedir_visor(backend, mailbox),
            // The rest do not get here: the `match` above dispatches them.
            _ => Self::no_muta(),
        }
    }

    /// The effects that WRITE. None mutates here: all five open the question
    /// the mutation goes through, which is the only gate.
    pub(super) fn efecto_que_muta(
        &mut self,
        efecto: Efecto,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match efecto {
            Efecto::CrearDirectorio => self.pedir_mkdir(),
            Efecto::CrearFichero => self.pedir_fichero_nuevo(),
            Efecto::Borrar { permanente } => self.pedir_borrado(permanente),
            // With the backend because, like comparing, it goes out to ask as
            // soon as it opens: the dialog is born with no destination
            // warnings and they arrive afterwards.
            Efecto::Transferir { mover } => self.pedir_transferencia(mover, backend, mailbox),
            Efecto::Renombrar => self.pedir_rename(),
            Efecto::RenameIa => self.pedir_instruccion_ia(),
            Efecto::Organizar => self.pedir_plan_de_organizar(None, backend, mailbox),
            Efecto::RenameLote => self.pedir_plantilla_de_lote(None),
            Efecto::Permisos => self.pedir_permisos(),
            Efecto::BuscarSemantica => self.pedir_consulta_semantica(),
            // The rest do not get here: the `match` above dispatches them.
            _ => Self::no_muta(),
        }
    }
}

/// A native path in BYTES, which is how it crosses the bridge.
///
/// A free function so as not to repeat the Unix trait's `use` inside every
/// method: a `use` mid-function is what clippy calls
/// `items_after_statements`.
fn bytes_de_ruta(p: &std::path::Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt as _;
    p.as_os_str().as_bytes().to_vec()
}
