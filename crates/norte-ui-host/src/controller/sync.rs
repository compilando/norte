//! Compare two directories and synchronize them.
//!
//! Part of `controller`: these are `Estado` methods, moved here without
//! touching them (ADR 0086). The only writer is still the actor.

// These modules are the same `impl Estado` split into pieces, so they use
// the same imports as the parent. Enumerating them here would be a
// forty-line list per file, in 32 files, that goes stale the moment the
// parent imports something — `super::*` tracks it on its own.
#[allow(clippy::wildcard_imports)]
use super::*;

/// A requested plan whose Task has not come back yet.
///
/// Exists because of the id: the shared model needs it AT BIRTH to be able
/// to discard whatever comes from a different plan.
pub(super) struct SyncPedida {
    /// Which of this window's plans this is.
    pub(super) epoca: u64,
    /// It was abandoned before the Task came back.
    pub(super) abandonada: Arc<std::sync::atomic::AtomicBool>,
    /// The requested mode.
    modo: norte_proto::methods::SyncMode,
    /// Source root.
    origen: VPath,
    /// Destination root.
    destino: VPath,
    /// The SOURCE's name reinterpretation, frozen when requested.
    origen_encoding: Option<norte_encoding::NameEncoding>,
    /// The DESTINATION's, which can be a different one.
    destino_encoding: Option<norte_encoding::NameEncoding>,
}

/// A synchronization plan, with its SHARED model inside.
///
/// The model is `norte_frontend::sync::SyncView`, the same one the TUI uses:
/// what steps there are, what blocks them, whether it can be approved and
/// what state the Task is in. Not a single step or verdict is decided here;
/// the core produces the plan and only it can redeem it.
pub(super) struct Sincronizacion {
    /// Which of this window's plans this is.
    pub(super) epoca: u64,
    /// The PLAN's Task (the apply's is a different one, and the model holds
    /// it).
    task: norte_proto::TaskId,
    /// The view closed and whatever is left is extra.
    abandonada: Arc<std::sync::atomic::AtomicBool>,
    /// The shared model.
    pub(super) vista: norte_frontend::sync::SyncView,
    /// The window the renderer says it is painting.
    primera_visible: usize,
    /// How many steps fit in that window.
    ventana: usize,
    /// Its report has already been requested: it is an RPC, and a
    /// reconnection re-announces the terminal.
    informe_pedido: bool,
    /// Which CONNECTION epoch its Task lives in.
    ///
    /// After a handoff, the new daemon hands out ids starting from 1:
    /// without this, an unrelated task with the same number would close this
    /// write's history with someone else's proof.
    epoca_conexion: u64,
}

/// A comparison of two trees, with its SHARED pane inside.
///
/// The model — which rows there are, which categories are hidden, which one
/// is selected, which side the keys operate on — is
/// `norte_frontend::compare::ComparePane`, the same one the TUI paints.
/// Nothing is paired up again here and no verdict is decided: the core did
/// that, and reproducing it in the host would be a third copy.
pub(super) struct Comparacion {
    /// Which of this window's comparisons this is. Same reason as a
    /// search's epoch: the Task's id arrives late.
    pub(super) epoca: u64,
    /// The daemon's Task, as soon as it is known. Zero while it is not.
    pub(super) task: norte_proto::TaskId,
    /// The view closed and whatever is left of this comparison is extra.
    abandonada: Arc<std::sync::atomic::AtomicBool>,
    /// The shared MODEL: roots, rows, filters, selection, active side and —
    /// what matters most — what state it ended up in.
    ///
    /// `CompareState`'s five states are how a frontend says whether the
    /// answer is COMPLETE, and in a comparison that IS the answer. Having a
    /// `bool alive` here would have lost again the case that enum exists not
    /// to lose: batches dropped along the way.
    vista: norte_frontend::compare::CompareView,
    /// The window the renderer says it is painting.
    primera_visible: usize,
    /// How many rows fit in that window.
    ventana: usize,
}

impl Estado {
    /// A comparison's Task's outcome enters the model.
    ///
    /// `finish_from_task` translates it, and that is where the difference
    /// that matters lives: "finished" is not the same as "finished and
    /// everything arrived". A comparison that lost batches reads as
    /// INCOMPLETE, and one whose channel closed with no outcome observed
    /// reads as UNKNOWN — two states the CLI and the MCP each already got
    /// wrong on their own.
    /// The APPLY Task finished: its report is requested.
    ///
    /// The Task's outcome says whether it ran; what was done and what was
    /// NOT is told by the report, and without it "finished" reads as
    /// "succeeded" over a destination that may have been left halfway.
    pub(super) fn pedir_informe_de_sync(
        &mut self,
        p: &norte_proto::TaskProgress,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        let Some(sinc) = self.sincronizacion.as_ref() else {
            return;
        };
        // The SAME three guards as a batch's report, and for the same
        // reasons: the CLASS (any `fs.copy` can carry the same id after a
        // handoff), the connection EPOCH (the new daemon's ids start over at
        // 1) and idempotence (a reconnection re-announces the terminal, and
        // this is an RPC).
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

    /// The report arrived: it enters the model, which decides what phrase
    /// comes out.
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
        // `on_apply_ended` is the one that knows how to read the (outcome,
        // report) pair: an apply cancelled WITH a report says both halves —
        // "cancelled after applying N" — and one with no report lets the
        // error take over, because there is no count that can replace it.
        // The error's category comes back ALREADY localized into this
        // window's language, because the model receives it as a parameter:
        // reading it from the global would have put a write's outcome in
        // another window's language.
        if let Some(categoria) = sinc.vista.on_apply_ended(estado, informe, lang) {
            sinc.vista.error = Some(clamp_display(categoria));
        }
        let cambio = ViewChange::Sync {
            sync: self.vista_sincronizacion(),
        };
        vec![self.parche(vec![cambio])]
    }

    /// A PLAN Task's outcome enters the model.
    ///
    /// Without this, `run` used to stay at `Running` forever, and with it
    /// died the clause the shared model documents as its reason for
    /// existing: a CANCELLED or FAILED plan is not approved even if it has
    /// closed. `sync.plan_done` can already be on the channel when the
    /// reader presses `Escape`, so without the outcome the screen offered
    /// to approve a plan that had just been told to stop — and phase B hangs
    /// the write button off that field.
    ///
    /// And by progress, not by the channel closing: a Task that dies without
    /// closing its stream used to leave the panel at "planning…" forever.
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
            // The localized CATEGORY, never the English `Display`: this is
            // painted persistently and several variants interpolate data
            // from the other end.
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

    /// The keys while the differences panel is open.
    /// The keys while the differences panel is open.
    ///
    /// `Escape` TWICE and not once: the first asks to cancel the Task, the
    /// second closes no matter what. Without the second, closing depended on
    /// the row channel really closing, and there are ways it might not — a
    /// dead daemon, a provider hung off an NFS — that left the reader
    /// trapped on the one screen in norte with no way out.
    /// The keys while the synchronization panel is open.
    ///
    /// `Escape` TWICE, for the same reason as the differences panel: the
    /// first asks to cancel the live Task — the plan's, or the apply's if it
    /// is already writing — the second closes no matter what.
    /// `dialog.approve` approves, and when the plan deletes or leaves
    /// something with no way back it also answers the SECOND question: it is
    /// the last screen where saying no is still possible.
    #[expect(
        clippy::too_many_lines,
        reason = "dispatcher for a screen with two key regimes"
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
        // With the SECOND question in front, the keys belong to it: only `y`
        // answers yes, and anything else withdraws it. A question that can
        // be answered with any key is not a question.
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
        // `Home`/`End` stay fixed keys: the shared catalog has no verb for
        // "to the start" inside a dialog.
        let verbo = match k.key.as_str() {
            "Home" | "home" | "End" | "end" => None,
            _ => self.verbo_de_dialogo(k),
        };
        let Some(sinc) = self.sincronizacion.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        match (verbo.as_deref(), k.key.as_str()) {
            // Approving the plan is `dialog.approve`, not `dialog.confirm`:
            // what is being answered here is "yes, write" over a plan
            // already in front, which is exactly what that verb names — and
            // it is the same one the SECOND question is answered with.
            (Some("dialog.approve"), _) => self.pedir_aprobacion(backend, buzon),
            (Some("dialog.cancel"), _) => {
                // While the daemon is WRITING, `Escape` asks to cancel and
                // does not close: closing loses the report — and with it the
                // count, the failures and the undo handle — over a
                // destination rewritten halfway.
                let escribiendo = sinc.vista.is_submitted()
                    || matches!(
                        sinc.vista.state,
                        norte_frontend::sync::SyncState::Applying(_)
                    );
                if escribiendo {
                    // The FIRST time asks to stop and does not close: closing
                    // loses the report over a destination halfway rewritten.
                    //
                    // The second one DOES close, and that does not
                    // contradict the above: "wait for the report" holds
                    // while the report can arrive, and there are ways it
                    // never does — a dead daemon, a failing `sync.report`, a
                    // Task whose channel drops with no outcome. Without this
                    // exit, this screen — the one that WRITES — was the only
                    // one in norte with no way out.
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
                    // And it IS SAID what is lost by closing: the
                    // destination may have been left halfway and its report
                    // is not going to be seen anymore.
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
                // And the model finds out RIGHT AWAY: if `plan_done` is on
                // its way, without this the panel would switch to "ready to
                // approve" a plan the reader just told to stop.
                //
                // Only while something is RUNNING. Over an already-applied
                // plan, marking "cancelled" used to rewrite the outcome to
                // "cancelled after applying N; the rest was not applied"
                // over a synchronization that finished in full: two false
                // sentences about what is on disk, on the one screen that
                // describes it.
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
                // The scroll CAP is "how much there is minus how much fits",
                // not "how much there is minus one": with the latter, a
                // single arrow over a two-step plan and a window of two
                // hundred used to stop sending the first step.
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
            // What it does not understand is SWALLOWED: a panel that lets
            // keys through is not a screen.
            _ => (self.aplicada(), Vec::new()),
        }
    }

    /// `a`: asks to approve the plan. There may be a SECOND question.
    ///
    /// The second one is not ceremony: it is composed by the shared model
    /// with one branch per undo perspective, and it only appears when the
    /// plan deletes trees or leaves something with no way back. A plan that
    /// undoes in full and deletes nothing does not have it — always asking
    /// is what teaches people to answer without reading.
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

    /// Sends `sync.apply` with the hash the CORE returned.
    ///
    /// Through `SyncView::submit`, the ONLY door: it checks `can_approve` and
    /// throws the in-flight-apply latch in the same gesture. Splitting them
    /// leaves a window in which a second `a` — or an `Escape` — fits between
    /// the request going out and the daemon answering, and this window reads
    /// events between keys, so it is genuinely reachable.
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
                    // Is it KNOWN that it did not write? Only if the daemon
                    // answered no. A dead transport leaves the request up in
                    // the air.
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
                    // And the latch is released — when it should be —
                    // without this `a` stays dead forever over a plan nobody
                    // applied.
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

    /// The daemon accepted the apply: the model switches to APPLYING.
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
        // The Task being followed switches to the APPLY's: it is what
        // `Escape` points at now, and the one the report has to be requested
        // from.
        sinc.task = task;
        sinc.epoca_conexion = self.epoca_conexion;
        if !sinc.vista.on_apply_started(task) {
            // The model DENIES it — the reader asked to stop in the window
            // during which the apply had no id yet — and then canceling it
            // is OUR job: nobody else knows that id, and the model's
            // contract spells it out. Without this, the daemon kept
            // rewriting the destination of a plan the human cancelled.
            sinc.vista.on_apply_abandoned();
            let (_, mut fuera) = self.cancelar(task.get());
            fuera.extend(self.decir("msg-sync-cancelled-late"));
            fuera.push(self.parche(vec![ViewChange::Sync {
                sync: self.vista_sincronizacion(),
            }]));
            return fuera;
        }
        // It could be born TERMINAL: the daemon completed it before
        // answering and its progress never fires. It is the same race the
        // board already documents, and here it translates into a panel
        // stuck applying forever.
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
        // The filters' DIGITS do not go through the resolver: they are
        // positional — the nth of `CATEGORIES` — and there are no five verbs
        // to name them. It is the same decision as in the TUI.
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
                // Switching sides changes which pane `Enter` navigates to
                // and which side the file keys operate on.
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
            // 1..5: the filters, in the categories' fixed order, same as in
            // the TUI.
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
            // A key it does not understand is SWALLOWED just the same: a
            // panel that lets through what it does not understand is not a
            // screen, it is decoration.
            _ => (self.aplicada(), Vec::new()),
        }
    }

    /// Chooses a row from the differences panel.
    /// The differences panel's four actions, in one arm.
    ///
    /// Together and not four arms of the general dispatch: they are the same
    /// surface and none of them means anything without it.
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
            // The general dispatch only sends those four here.
            _ => (Self::obsoleta(StaleAction::Modal), Vec::new()),
        }
    }

    /// Chooses a row from the differences panel.
    pub(super) fn comparacion_selecciona(
        &mut self,
        id: u64,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(c) = self.comparacion.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        // `select` IGNORES an id that did not arrive, which is correct: the
        // alternative is a selection naming a nonexistent row.
        c.vista.pane.select(id);
        if c.vista.pane.selected_id() != Some(id) {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        }
        let cambio = ViewChange::Compare {
            compare: self.vista_comparacion(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Shows or hides a whole category.
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
            // A category that does not exist is a renderer from another
            // version, not an order.
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        };
        c.vista.pane.toggle_filter(*cat);
        let cambio = ViewChange::Compare {
            compare: self.vista_comparacion(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// The renderer says which window it paints.
    pub(super) fn comparacion_ventana(
        &mut self,
        primera: u64,
        cuantas: u32,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(c) = self.comparacion.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        c.primera_visible = usize::try_from(primera).unwrap_or(0);
        // Capped: whatever the renderer says fits cannot make a patch carry
        // half a million rows.
        c.ventana = usize::try_from(cuantas)
            .unwrap_or(Self::VENTANA_COMPARACION)
            .clamp(1, MAX_ROWS_PER_BATCH);
        let cambio = ViewChange::Compare {
            compare: self.vista_comparacion(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Opens the chosen row: navigates to the ACTIVE side's directory.
    ///
    /// Where to go is decided by the SHARED model (`navigation_target`): the
    /// row if it is a directory, its parent if it is a file, and `None` when
    /// that side is empty — an orphan looked at from the side that does not
    /// have it — which does NOT fall back to the other side.
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
        // The pane that navigates is the ACTIVE side's, not whichever has
        // focus: whoever is looking at the right cannot lose their left
        // directory by pressing `Enter`. That slot is FOCUSED and navigation
        // goes through the usual path, the one that records the trail and
        // requests the listing.
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

    /// The slot corresponding to the comparison's ACTIVE side.
    pub(super) fn hueco_del_lado(&self) -> Option<u32> {
        let c = self.comparacion.as_ref()?;
        let izquierdo = u32::try_from(c.vista.left_pane).ok()?;
        match c.vista.pane.active_side() {
            norte_proto::methods::Side::Right => self.hueco_destino().ok(),
            _ => Some(izquierdo),
        }
    }

    /// Requests the PLAN to synchronize the active pane onto the destination.
    ///
    /// The plan writes not a single byte: it says what it would do. What
    /// writes is `sync.apply`, and only against the hash this plan closes
    /// with.
    pub(super) fn pedir_sincronizacion(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // ONE at a time. Relaunching left the previous panel un-abandoned and
        // its Task uncancelled — the daemon kept walking a tree for a plan
        // that can no longer be seen — and, with a request in flight, the
        // second press killed both panels' one.
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
        // Which tree gets overwritten is decided by the SHARED rule, not a
        // local copy: two answers to "which of the two is rewritten" is the
        // cheapest bug to write and the most expensive to find, because both
        // produce a perfectly plausible plan.
        let enfocado = self.hueco().pane.dir().clone();
        let otro = self.huecos[&destino_slot].pane.dir().clone();
        // With the differences panel open the ACTIVE SIDE rules; without it,
        // the focused pane is the source. Both branches live in the shared
        // rule, and here only the data is passed to it.
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
            // Overlapping roots: the daemon rejects it with
            // `OverlappingRoots` and creates no Task. This local shortcut is
            // a courtesy — the authority is the core, which also catches
            // NESTED overlap — but opening a panel that is going to die is
            // worse than saying so beforehand.
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

    /// Enqueues `sync.plan` and hooks its event channel to the actor.
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
        // `Update` and not `Mirror`: the mode that does NOT delete is the
        // one that can be the default. Choosing mirror is a decision made on
        // purpose, and until there is somewhere to make it, it is not
        // offered.
        let modo = norte_proto::methods::SyncMode::Update;
        let params = norte_proto::methods::SyncPlanParams {
            source: origen.clone(),
            dest: destino.clone(),
            mode: modo,
            compare: norte_proto::methods::SyncCompareOptions::default(),
            on_unknown: norte_proto::methods::OnUnknown::default(),
            // With no `include`: the whole tree. Capping the plan to a
            // selection is what the differences panel does with its marks,
            // and that arrives when this window has that path.
            include: None,
        };
        let backend2 = Arc::clone(backend);
        let buzon2 = buzon.clone();
        let abandonada2 = Arc::clone(&abandonada);
        tokio::spawn(async move {
            let (task, mut rx) = match backend2.sync_plan(params).await {
                Ok(par) => par,
                Err(e) => {
                    // The failure IS REPORTED and also RELEASES the request:
                    // without the latter, a daemon that does not know how to
                    // plan — or overlapping roots — used to leave
                    // `sync_pedida` set forever and the next attempt refused
                    // itself.
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
        // The panel opens when the Task's id is KNOWN, and not before: the
        // shared model uses it to discard whatever comes from another plan,
        // and with a filler id it also discarded its own — the panel stayed
        // at zero steps and the plan closed "cannot approve".
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

    /// A plan event: a batch of steps, or its closing.
    /// The daemon accepted the plan and gave its Task: now the panel opens.
    ///
    /// The shared model is born WITH the id because that is what it uses to
    /// discard whatever comes from another plan; building it earlier, with a
    /// filler id, made it discard its own batches too and the panel stayed
    /// at zero steps and closed "cannot approve".
    pub(super) fn abrir_panel_de_sync(
        &mut self,
        epoca: u64,
        task: norte_proto::TaskId,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // FILTER before TAKING: an unconditional `take()` swept away a new
        // request when an old one's Task answered, and then no panel opened
        // at all while two traversals kept walking two trees on the daemon.
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
                // Each side's reinterpretations, exactly as the shared rule
                // decided them: there are TWO because the two panes are two
                // locations, and swapping them would name the file the
                // write lands on with different bytes.
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
        // The SHARED model decides what gets in: it discards whatever comes
        // from another plan by its `task_id`, and it is the one that knows
        // when the plan closes.
        let cambio = match ev {
            norte_client::SyncPlanEvent::Steps(lote) => sinc.vista.state.on_steps(lote),
            norte_client::SyncPlanEvent::Done(done) => sinc.vista.state.on_plan_done(done),
        };
        if !cambio {
            // That it was discarded IS STATED: a batch rejected after
            // closing is a violation of the daemon's contract, and staying
            // quiet about it hides it.
            tracing::warn!(epoca, "a plan event was discarded");
            return Vec::new();
        }
        let cambio = ViewChange::Sync {
            sync: self.vista_sincronizacion(),
        };
        vec![self.parche(vec![cambio])]
    }

    /// The window's steps, projected by the SHARED model.
    pub(super) fn pasos_proyectados(
        pasos: &[norte_proto::methods::SyncStep],
        papelera: norte_proto::methods::DestTrash,
        enc: norte_frontend::sync::SyncEncodings,
        lang: norte_i18n::Lang,
    ) -> Vec<crate::dto::SyncStepView> {
        pasos
            .iter()
            .map(|paso| {
                // The cells are composed by the SHARED model: what the step
                // does, why, whether undo brings it back — which NEVER comes
                // straight from `reversal`, because that is half an answer —
                // and both spellings when there are two.
                let c = norte_frontend::sync::render_step(paso, papelera, enc);
                crate::dto::SyncStepView {
                    id: c.id,
                    kind: clamp_display(norte_frontend::sync::step_label(paso.kind, lang)),
                    // The reason is only carried by steps that have one: a
                    // `Skip`, or one that cannot be undone. Empty is ABSENCE,
                    // not an invented phrase.
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

    /// The report's failures, once there is a report.
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

    /// Which root a path hangs off, by its stable id.
    ///
    /// `either` is stated: on a panel where an unqualified path means "from
    /// the source", staying quiet about it asserts the source.
    pub(super) fn nombre_de_ancla(anchor: norte_frontend::sync::RelAnchor) -> String {
        match anchor {
            norte_frontend::sync::RelAnchor::Dest => "dest".to_owned(),
            norte_frontend::sync::RelAnchor::Source => "source".to_owned(),
            norte_frontend::sync::RelAnchor::Either => "either".to_owned(),
        }
    }

    /// The anchor's label, already translated, or empty when there is
    /// nothing to say.
    ///
    /// The label and not just the id: the DTO promises this gets painted,
    /// and a `data-` no style reads does not paint it — `either` stayed
    /// silent, which on a panel where an unqualified path means "from the
    /// source" is asserting the source.
    pub(super) fn etiqueta_de_ancla(
        anchor: norte_frontend::sync::RelAnchor,
        lang: norte_i18n::Lang,
    ) -> String {
        norte_frontend::sync::anchor_label(anchor, lang).map_or_else(String::new, clamp_display)
    }

    /// The synchronization panel's projection, capped to its window.
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
            // The mode, by the SHARED label. Falling back to "update" for a
            // mode this build cannot name would assert the SAFE half of what
            // is being approved — "this does not delete" — about something
            // unknown, and the catalog itself forbids that in writing.
            mode: clamp_display(norte_frontend::sync::mode_label(v.mode, self.lang)),
            steps: filas,
            first_visible: primera as u64,
            // The RETAINED ones plus what the model dropped: without adding
            // them, this number and the status line's contradict each other
            // on a large plan, and both cross in the same message.
            total: (pasos.len() as u64).saturating_add(
                v.state
                    .plan()
                    .map_or(0, norte_frontend::sync::SyncPlan::dropped),
            ),
            // The SUMMARY, which is what a human reads before approving:
            // irreversible ones, bytes (with the ones that could not be
            // measured kept apart), what could not be read, and whether the
            // list hides steps. It does not fit in the status line and
            // cannot stay inside the model.
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
            // What PREVENTS applying, with ITS PATH: "the destination is
            // read-only" with no path sends people hunting for the problem
            // blindly, and a root blocker is said as "the whole tree", not
            // left empty.
            blockers: v
                .state
                .plan()
                .map(|p| {
                    p.done()
                        .blockers
                        .iter()
                        .map(|b| {
                            // With no reinterpretation: which root a
                            // BLOCKER's `rel` hangs off is not yet decided by
                            // any shared rule — the one that exists is for
                            // steps — and choosing it here would be
                            // inventing a second answer. Today it changes
                            // nothing because this window has no name-
                            // encoding override; the day it does, the rule
                            // goes up there and not here.
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
            // How many there REALLY are: the wire trims the list to 256 and
            // the total travels separately precisely so 40,000 does not read
            // as 256.
            blockers_total: v.state.plan().map_or(0, |p| p.done().blockers_total),
            status: clamp_display(norte_frontend::sync::status_line(v, self.lang)),
            // The model's key line offers `a approve` as soon as the plan
            // can be approved, and this phase does NOT have that key: saying
            // what cannot be done teaches pressing it right on the screen
            // where the next phase puts the write. While approving does not
            // exist, this screen says it only reads.
            hint: clamp_display(if v.can_approve() {
                norte_i18n::t_in(self.lang, "host-sync-read-only")
            } else {
                norte_i18n::t_in(self.lang, norte_frontend::sync::hint_id(v))
            }),
            confirming: v.confirming.as_ref().map(|c| clamp_display(c.text.clone())),
            // The report's failures, one by one. The count goes in the
            // status line, composed by the shared model; this is the
            // detail, and without it "3 failed" does not say which.
            failures: fallos,
            cancel_requested: v.cancel_requested,
            can_approve: v.can_approve(),
            running: matches!(v.run, norte_frontend::sync::SyncRunState::Running),
        })
    }

    /// How many comparison rows — or a plan's steps — get through if the
    /// renderer has not said its window yet.
    pub(super) const VENTANA_COMPARACION: usize = 200;

    /// Launches the two panes' comparison and opens the differences panel.
    ///
    /// The right root comes from the slot holding the `Target` role, through
    /// the SAME path as a transfer: two ways of deciding "the other pane"
    /// are two places they can drift apart, and with several candidates and
    /// none designated it asks to choose instead of breaking the tie.
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
            // The daemon would reject it just the same (`-32602`), and
            // opening a panel that promises an impossible answer is worse
            // than saying so beforehand.
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

    /// `pane.dir-size` (#139, #290): counts what is MARKED — or what is
    /// under the cursor — takes up and leaves it on the board.
    ///
    /// ONE Task for the whole batch, unlike copy or delete: the wire's
    /// method takes a list, and counting separately would force whoever asks
    /// to add up the bytes **and** the unreadable ones, which do not add up
    /// the same way — a round total made of two partial counts is a wrong
    /// answer, not an incomplete one.
    ///
    /// There are no affected directories to refresh: this writes nothing.
    /// Its result IS its terminal progress, which the board already knows
    /// how to read.
    pub(super) fn contar_tamano(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // `marked_paths` falls back to the cursor when there are no marks:
        // the same source of "what this operates on" a transfer uses.
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

    /// Enqueues `fs.compare` and hooks its row channel to the actor.
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
            // With no depth cap, like the TUI: a comparison that stops
            // halfway has not answered what it was asked.
            max_depth: None,
            // The FAT rule, which is the wire's default.
            mtime_tolerance_ms: 2000,
            // Without following links, like the core's default: destinations
            // are compared as BYTES, and following them could step outside
            // the tree that was asked about.
            follow_symlinks: false,
            // An orphan is emitted as ONE row and is not descended into,
            // which is what the default knows how to do. Descending into one
            // side is a synchronization plan's decision, not a comparison's,
            // which only looks.
            descend_orphans: None,
        };
        self.comparacion = Some(Comparacion {
            epoca,
            task: norte_proto::TaskId::new(0),
            abandonada: Arc::clone(&abandonada),
            vista: norte_frontend::compare::CompareView::new(
                izquierda,
                derecha,
                // The slot that launched the comparison IS the left side, and
                // that decides which pane an `Enter` navigates to. Without
                // it, whoever is looking at the right side lost their left
                // directory to go see the right one's.
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
            // The view may have closed while the daemon was accepting the
            // Task: in that window the actor has nobody to cancel, so
            // whoever does have it cancels it.
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

    /// A batch of compared rows. Matches by EPOCH, like search hits.
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
        // The SHARED pane is the one that counts, filters and selects: here
        // it is only given the rows.
        c.vista.pane.extend(lote.rows);
        let cambio = ViewChange::Compare {
            compare: self.vista_comparacion(),
        };
        vec![self.parche(vec![cambio])]
    }

    /// The differences panel's projection, capped to its window.
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
                // The cells are composed by the SHARED model: the masked
                // names with their flag, and the two glyphs in the middle.
                // Neither the pairing nor the verdict is recomputed here.
                let celdas = cells_for(r, None, None);
                let cara = |f: Option<&norte_frontend::compare::RowFace>| {
                    f.map(|f| crate::dto::CompareFaceView {
                        name: clamp_display(f.name.clone()),
                        hostile: f.hostile,
                        // Formatted with the SAME functions as a listing
                        // column: a size or a date cannot read differently
                        // depending on which panel paints them.
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
            // The phrase is composed by the SHARED model, and it is not a
            // detail: its five states are how it is said whether the answer
            // is complete, and a comparison that lost batches has to read
            // differently from one that finished. The TUI and the CLI each
            // already got this wrong on their own.
            status: clamp_display(norte_frontend::compare::status_line(
                &c.vista,
                c.vista.pane.marked_len(),
                self.lang,
            )),
            running: c.vista.state == norte_frontend::compare::CompareState::Running,
        })
    }
}
