//! The agent session panel, and undoing an entire one.
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
    /// Opens the extension manager and REQUESTS the catalogue.
    ///
    /// It opens empty and saying it is loading, not waiting: a window frozen
    /// while the daemon answers is worse than a list that appears half a
    /// second later. And "loading" is not the same as "none": an empty list
    /// without that notice reads as nothing being installed.
    // TODO(translation): review — this paragraph describes the extension
    // manager, not the agent panel opened below; it looks like a stale
    // doc comment left behind by an earlier edit.
    /// Opens the AGENT session panel.
    ///
    /// It asks the daemon for nothing: there is no method that enumerates
    /// live sessions, so what is shown is what THIS window has seen ask for
    /// permission — and the panel says so. That is also what makes the undo
    /// operand be CHOSEN instead of typed, which is what task 5.3 rejected: a
    /// typed session id can be mistyped, and undoing the wrong session is
    /// undoing someone else's work.
    pub(super) fn abrir_agentes(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.agencia.panel = true;
        self.agencia.sesiones.al_abrir();
        let change = ViewChange::Agents {
            agents: self.vista_agentes(),
        };
        (self.aplicada(), vec![self.parche(vec![change])])
    }

    /// The agent panel's projection.
    pub(super) fn vista_agentes(&self) -> Option<crate::dto::AgentsView> {
        // A window without effects does NOT subscribe to the approvals
        // channel, so its list is empty for THAT reason and not because
        // nobody has asked for anything. The screen says so, instead of
        // asserting what it does not know.
        let listening = self.efectos == crate::commands::Efectos::Completo;
        self.agencia
            .panel
            .then(|| self.agencia.sesiones.vista_de(self.lang, listening))
    }

    /// The keys while the agent panel is open.
    pub(super) fn tecla_en_agentes(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        /// How many rows a page moves.
        const PAGE: i64 = 10;
        // `Home`/`End` and `u` remain fixed keys: the shared catalogue has no
        // verb for "to the top" nor for "undo the session", and waiting until
        // it does would have left the panel without its ends and without its
        // one operation.
        let verb = match k.key.as_str() {
            "Home" | "home" | "End" | "end" => None,
            "u" if !k.ctrl && !k.alt && !k.meta => None,
            _ => self.verbo_de_dialogo(k),
        };
        match (verb.as_deref(), k.key.as_str()) {
            (Some("dialog.cancel"), _) => self.agencia.panel = false,
            (Some("dialog.down"), _) => self.agencia.sesiones.mover(1),
            (Some("dialog.up"), _) => self.agencia.sesiones.mover(-1),
            (Some("dialog.page-down"), _) => self.agencia.sesiones.mover(PAGE),
            (Some("dialog.page-up"), _) => self.agencia.sesiones.mover(-PAGE),
            (_, "Home" | "home") => self.agencia.sesiones.mover(i64::MIN / 2),
            (_, "End" | "end") => self.agencia.sesiones.mover(i64::MAX / 2),
            // `u` UNDOES the whole session, and asks first: it is the biggest
            // operation this window can launch in one go — it reverts
            // everything an agent did, in reverse order — and no other
            // touches as many things with one key.
            // And it demands the BARE key, unlike the rest of this host's
            // letters: `ctrl+u` is muscle memory for something else, and this
            // is the biggest operation the window can launch in one go.
            (_, "u") if !k.ctrl && !k.alt && !k.meta => return self.preguntar_por_deshacer(),
            _ => return (self.aplicada(), Vec::new()),
        }
        let _ = (backend, mailbox);
        let change = ViewChange::Agents {
            agents: self.vista_agentes(),
        };
        (self.aplicada(), vec![self.parche(vec![change])])
    }

    /// A click on a row of the agent panel: selects it.
    pub(super) fn elegir_agente(
        &mut self,
        row: u32,
        generation: u64,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // With a dialog on top, the panel does not receive: it is modal for
        // the keyboard, and it has to be for the mouse too.
        if !self.agencia.panel || !self.dialogos.is_empty() {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        }
        if !self.agencia.sesiones.senalar(row as usize, generation) {
            // The list changed between painting and the click: it is refused
            // instead of clamped, because clamping is choosing for the
            // reader.
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        }
        let change = ViewChange::Agents {
            agents: self.vista_agentes(),
        };
        (self.aplicada(), vec![self.parche(vec![change])])
    }

    /// Opens the question to undo an entire session.
    pub(super) fn preguntar_por_deshacer(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.efectos == crate::commands::Efectos::SoloLectura {
            return Self::no_muta();
        }
        let Some(session) = self.agencia.sesiones.elegida() else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-no-session".to_owned(),
                },
                Vec::new(),
            );
        };
        // One at a time: two `policy.undo_session` calls for the same
        // session walk the SAME list of entries — each photographs it before
        // the other records its compensations — and the second returns a
        // report full of locks that belong to nobody.
        if self.agencia.sesiones.tiene_undo_vivo(&session) {
            let outgoing = self.decir("host-undo-already-running");
            return (
                ActionAck::Unavailable {
                    reason_key: "host-undo-already-running".to_owned(),
                },
                outgoing,
            );
        }
        // The id, masked, in its own field: it is an opaque daemon key that
        // can carry any byte, and a decision about "this session" that does
        // not say which one is not a decision.
        let (displayable, hostile) = norte_frontend::display_name(session.as_bytes());
        let modal = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let view = DialogView {
            id: modal,
            title_key: "modal-undo-session-title".to_owned(),
            destination: None,
            subject: Some(crate::dto::DialogLine {
                text: clamp_display(displayable),
                hostile,
            }),
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            // The body says what SCOPE it has, which is not visible in the
            // row: undoing a session reverts EVERYTHING it did, not just the
            // last thing, and whatever cannot be reverted — something
            // irreversible, something the policy now denies — will be said
            // in the report.
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
                    // Undo WRITES: it moves files back and deletes the ones
                    // the session created.
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
            vista: view.clone(),
            tecleado: Tecleado::Texto(String::new()),
            reconocido: true,
            al_confirmar: Some(Pendiente::DeshacerSesion { sesion: session }),
        });
        let change = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![change])])
    }

    /// Confirms undoing ONE session: checks that no other one is in
    /// progress, launches it, and repaints the row.
    ///
    /// The session is the one that was READ at the question, not the one
    /// currently selected: the list reorders itself — a new request bumps
    /// its session to first place — and the dialog keeps the keys, not the
    /// background messages.
    pub(super) fn deshacer_sesion(
        &mut self,
        session: &str,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> (Option<&'static str>, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.agencia.sesiones.tiene_undo_vivo(session) {
            return (
                Some("host-undo-already-running"),
                self.decir("host-undo-already-running"),
            );
        }
        self.agencia.sesiones.deshaciendo(session);
        // With no known SCOPE: an `undo_session` touches whatever
        // directories the session touched, which this window does not know.
        // What is relisted is what is ON SCREEN, which is where the reader
        // was watching the agent work.
        let visible = self.dirs_visibles();
        Self::lanzar_deshacer(session.to_owned(), visible, backend, mailbox);
        let mut outgoing = Vec::new();
        if self.agencia.panel {
            let change = ViewChange::Agents {
                agents: self.vista_agentes(),
            };
            outgoing.push(self.parche(vec![change]));
        }
        (None, outgoing)
    }

    /// Launches undoing an entire session.
    ///
    /// Like any other long operation: it is a Task, it appears on the board,
    /// and its report — what did NOT come back — arrives through the path
    /// 5.3 already built.
    pub(super) fn lanzar_deshacer(
        session: String,
        affected: Vec<VPath>,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) {
        let backend = Arc::clone(backend);
        let mailbox = mailbox.clone();
        tokio::spawn(async move {
            match backend.undo_session(session.clone()).await {
                Ok(task) => {
                    // The task's id and the session, together: the outcome
                    // arrives via progress, which only carries the id.
                    let _ = mailbox
                        .send(Mensaje::Fondo(Box::new(Fondo::UndoDeSesion(
                            task.id.get(),
                            session,
                        ))))
                        .await;
                    let _ = mailbox
                        .send(Mensaje::TaskNueva(Box::new((task, affected, None))))
                        .await;
                }
                Err(e) => {
                    let _ = mailbox.send(Mensaje::TaskFallida(Box::new(e))).await;
                }
            }
        });
    }
}
