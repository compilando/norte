//! The tasks board: progress, outcome, report and cancellation.
//!
//! Part of `controller`: these are `Estado` methods, moved here without
//! touching them (ADR 0086). The only writer is still the actor.

// These modules are the same `impl Estado` split into pieces, so they use
// the same imports as the parent. Enumerating them here would be a
// forty-line list per file, in 32 files, that goes stale the moment the
// parent imports something — `super::*` tracks it on its own.
use super::sums::Publicado;
#[allow(clippy::wildcard_imports)]
use super::*;

/// A checksums batch in flight (#311).
///
/// The Task is already on the board; what is awaited here is its REPORT,
/// which is where the digests travel — they do not fit in a Task's outcome
/// nor in its progress.
pub(super) struct SumasEnVuelo {
    /// The Task whose report is awaited.
    pub(super) task: norte_proto::TaskId,
    /// Which CONNECTION epoch that Task lives in: after a handoff the
    /// daemon's ids start over at 1, and a report for another task with the
    /// same number would count the check for something else.
    pub(super) epoca_conexion: u64,
    /// Its report has already been requested: requesting it is an RPC and a
    /// reconnection re-announces the outcome.
    pub(super) informe_pedido: bool,
    /// What the checksums file was publishing, if this is a CHECK. `None` =
    /// only computing.
    pub(super) publicado: Option<Publicado>,
}

/// A finished Task's report, by class.
///
/// Two classes have one — a rename batch and an undo — and both for the same
/// reason: what was left halfway does not fit in a Task's outcome.
pub(super) enum Informe {
    /// A rename batch's (#272).
    Lote(Result<norte_proto::methods::FsRenameBatchReportResult, Error>),
    /// A session undo's.
    Undo(Result<norte_proto::methods::PolicyUndoReportResult, Error>),
    /// A pack's (#250). The only one of the three that counts something
    /// about a Task that came out FINE: the archive was written whole and
    /// can still carry names that land somewhere else on another system.
    Empaquetado(Result<norte_proto::methods::ArchivePackReportResult, Error>),
}

/// What a `task.cancel` points at.
///
/// Three cases and not two: "there is none" and "the pointed-at one already
/// finished" read differently, and collapsing them would make cancelling a
/// finished task say there are no tasks while the board shows four.
pub(super) enum Objetivo {
    /// There is none to ask to stop.
    Ninguna,
    /// The pointed-at one is already terminal.
    Terminada,
    /// This one.
    Viva(u64),
}

/// A task alive on the board.
pub(super) struct TaskViva {
    pub(super) vista: TaskView,
    /// THIS task's rate, estimated from its own snapshots (spec 2026-09-15,
    /// ADR 0115).
    ///
    /// It does not come from the wire: `TaskProgress` says how much is done
    /// and not at what speed. It is per task and not per board because two
    /// copies at once run at different speeds, and an average of the two
    /// describes neither.
    rate: norte_frontend::tasks::Rate,
    /// How to ask it to stop. Cancelling twice is not an error.
    pub(super) cancel: std::sync::Arc<dyn Fn() + Send + Sync>,
    /// How to pause or resume it (ADR 0147); `None` if it cannot be.
    pub(super) pause: Option<crate::backend::Pausa>,
    /// How to move it up or down the queue (ADR 0149); `None` if it cannot
    /// be.
    pub(super) cola: Option<crate::backend::Pausa>,
    /// Its report has already been requested. Carried by the classes that
    /// HAVE a report — a rename batch and an undo — and it avoids
    /// requesting it twice if the daemon repeats the last progress (a
    /// reconnection re-announces tasks, terminal ones included).
    informe_pedido: bool,
    /// Which connection epoch it was registered in. A repeated id from a
    /// DIFFERENT epoch is a different task, not the same one.
    epoca: u64,
    /// The LIVE progress, to ask it whether it is still running.
    ///
    /// `vista` is a projection that updates when `Mensaje::Progreso` leaves
    /// the mailbox, so deciding what to cancel based on it is deciding based
    /// on a stale snapshot: it used to answer "cancelling…" about something
    /// already finished, and choosing "the last alive one" could skip the
    /// one that is really running. The TUI asks the live state for this
    /// exact reason.
    pub(super) progreso: tokio::sync::watch::Receiver<norte_proto::TaskProgress>,
    /// The directories this task leaves OUT OF DATE.
    ///
    /// Noted down when enqueuing and not derived from progress: progress
    /// says which file is currently in flight, not which screens lie once
    /// it finishes. Empty = nothing to refresh (a search, an unrelated task
    /// whose id is all that is known).
    pub(super) afectados: Vec<VPath>,
    /// What to retry with if it COLLIDES (#274). `None` for everything that
    /// is not a transfer: a delete or an undo have no other policy to
    /// offer.
    reintento: Option<Reintento>,
}

/// The count of ONE transfer batch (#271).
///
/// A large batch against a populated destination produces many `Failed`
/// rows — `CollisionPolicy::Fail` is what is sent — and the board shows them
/// one by one up to its cap. What the reader needs is not row 213: it is
/// "of these 500, 460 fine and 40 not".
///
/// And rejections on ENQUEUING had the twin problem: each one painted a
/// message in the status bar and the next one overwrote it, so of N
/// rejections only the last one survived. They are counted instead of
/// reported.
///
/// ONE single phrase, and at the end: half the batch is not an answer, it is
/// noise overwriting itself. The batch closes when everything requested is
/// resolved — enqueued or rejected, and what was enqueued, terminal.
#[derive(Debug, Default)]
pub(super) struct Lote {
    /// How many entries were requested.
    pub(super) total: usize,
    /// How many became a task.
    pub(super) encoladas: usize,
    /// How many the daemon rejected on enqueuing.
    pub(super) rechazadas: usize,
    /// The ids of the ones enqueued, to recognize their outcome. An id not
    /// here belongs to something else (a search, an undo, another client).
    pub(super) ids: std::collections::BTreeSet<u64>,
    /// GOOD terminal outcomes of the enqueued ones.
    pub(super) hechas: usize,
    /// Bad terminal outcomes: failed or cancelled.
    pub(super) fallidas: usize,
}

impl Lote {
    /// Everything requested is resolved.
    fn cerrado(&self) -> bool {
        self.encoladas + self.rechazadas >= self.total
            && self.hechas + self.fallidas >= self.encoladas
    }
}

impl Estado {
    /// Makes room on the board by dropping the oldest FINISHED one.
    ///
    /// Evicting a WELL-finished one is preferred: a failed or cancelled one
    /// is the only surface saying what did not arrive — a failure leaves no
    /// journal entry — and in a large batch with collisions those are
    /// exactly the ones that pile up. A LIVE one is not touched: it has
    /// progress to pump and, perhaps, a directory to re-list.
    /// Sets a clock on a freshly finished task: at [`TTL_TASK_TERMINAL`] it
    /// leaves the board.
    ///
    /// Same pattern as an approval's TTL: a `spawn` that sleeps and sends a
    /// message to the actor, because the state is touched by a single
    /// writer.
    pub(super) fn programar_caducidad(id: u64, epoca: u64, buzon: &mpsc::Sender<Mensaje>) {
        let buzon = buzon.clone();
        tokio::spawn(async move {
            tokio::time::sleep(TTL_TASK_TERMINAL).await;
            let _ = buzon.send(Mensaje::TaskCaducada(id, epoca)).await;
        });
    }

    /// A finished task's time is up: off the board.
    ///
    /// Three things are checked first, and none is paranoia:
    ///
    /// - the EPOCH, because after a daemon handoff the ids start over and
    ///   this clock has been flying for ten seconds;
    /// - that it is still TERMINAL, because a re-announced id can be running
    ///   again;
    /// - that it does not owe a refresh (`afectados`), which is the
    ///   invariant cap-based eviction already asserts: dropping the row
    ///   would sweep away the re-read of the directory that mutation
    ///   changed.
    pub(super) fn caducar_task(
        &mut self,
        id: u64,
        epoca: u64,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let quitar = self.tasks.get(&id).is_some_and(|t| {
            t.epoca == epoca && Self::terminal(t.vista.state) && t.afectados.is_empty()
        });
        if !quitar {
            return Vec::new();
        }
        self.tasks.remove(&id);
        self.anotar_tira(buzon);
        let mut envios = vec![self.parche(vec![ViewChange::Tasks {
            tasks: self.vistas_de_tasks(),
            cursor: self.cursor_del_tablero(),
        }])];
        // And HERE is where the panel that opened on its own closes
        // (ADR 0115). Asking only from `progreso` left half the gesture
        // undone: when the last row expires no more progress arrives, so
        // nobody checked again and the panel stayed up for the rest of the
        // session. The terminal did not have the bug because its loop
        // re-evaluates the same condition every round — the divergence
        // ADR 0077 goes after.
        envios.extend(self.procesos_automaticos(backend, buzon));
        envios
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
                "evicting a task with a pending refresh"
            );
            self.tasks.remove(&viejo);
        }
    }

    /// Relaunches the transfer that collided, with the chosen policy (#274).
    ///
    /// Repeats the SAME verb: an "overwrite" over a copy that turned into a
    /// move would delete the source nobody asked to touch. And it travels
    /// again with its `Reintento`, because the second attempt can collide
    /// again — `Skip` and `RenameAuto` cannot, but `Newer` can — and then it
    /// has to be possible to ask again.
    pub(super) fn lanzar_reintento(
        con: Reintento,
        politica: norte_proto::CollisionPolicy,
        a_la_cola: bool,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        // The destination changes; the source also stops being there if it
        // is a move. Both parents are noted down, like in the original
        // transfer.
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
            let queued = if con.mover {
                backend
                    .move_(con.from.clone(), con.to.clone(), politica, a_la_cola)
                    .await
            } else {
                backend
                    .copy(con.from.clone(), con.to.clone(), politica, a_la_cola)
                    .await
            };
            let mensaje = match queued {
                Ok(task) => Mensaje::TaskNueva(Box::new((task, afectados, Some(con)))),
                Err(e) => Mensaje::TaskFallida(Box::new(e)),
            };
            let _ = buzon.send(mensaje).await;
        });
    }

    /// Delivers the secret to the core and returns the outcome through the
    /// mailbox (#327).
    ///
    /// The `TypedSecret` is MOVED into the task and dies with it, so the
    /// host's copy is overwritten with zeros as soon as the core answers.
    /// The plaintext `String` the call requires is born as late as possible
    /// and lives for the bare minimum. ADR 0015 talks about the copies
    /// beyond that — the params, the frame, the daemon's `Value`.
    ///
    /// Returns nothing: like everything that TAKES A WHILE in this window,
    /// the answer comes back to the actor as just another message. The sole
    /// writer waits for nobody, so the cursor keeps responding while the
    /// core authenticates.
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

    /// The retry this task already had, if any and if it belongs to this
    /// epoch.
    ///
    /// A reconnection's re-announcement does not know what the task was
    /// requested with, so replacing it with `None` would leave the very
    /// collision the reader finds on returning with no way out. The epoch
    /// matters: after a daemon handoff the ids start over, and whatever was
    /// there with that number was something else.
    pub(super) fn reintento_heredado(&self, id: u64) -> Option<Reintento> {
        self.tasks
            .get(&id)
            .filter(|t| t.epoca == self.epoca_conexion)
            .and_then(|t| t.reintento.clone())
    }

    /// Ties the intent of "editing a new one" to the task that creates it
    /// (#290).
    ///
    /// Here and not earlier: the id does not exist until the daemon answers,
    /// and the gesture had already returned. Only to an OWN task and only if
    /// the intent has no id yet — an unrelated one passing through here
    /// cannot adopt this window's intent, which is exactly the bug this
    /// prevents.
    pub(super) fn atar_la_creacion(&mut self, id: u64, ajena: bool, kind: norte_proto::TaskKind) {
        if !ajena
            && kind == norte_proto::TaskKind::Create
            && let Some(c) = self.abrir_al_crear.as_mut()
            && c.task.is_none()
        {
            c.task = Some(id);
        }
    }

    /// Keeps the detail a RE-ANNOUNCEMENT does not carry.
    ///
    /// The SDK offers the tasks again on reconnecting, and that progress
    /// knows nothing about the report already requested for this task.
    /// Projecting it as is used to erase from the board the only signal
    /// that the directory was left halfway, right when the connection
    /// recovers and the reader looks at it again.
    ///
    /// Only inherited from the SAME epoch: after a daemon handoff the id
    /// starts over at 1, and whatever was there with that number was a
    /// different task.
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

    /// Puts a freshly enqueued Task on the board and leaves its progress
    /// pumping toward the actor.
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
        // Registered in the batch's count (#271), before any eviction: what
        // was enqueued was enqueued even if its row does not end up fitting.
        if let Some(lote) = self.lote.as_mut()
            && !ajena
            && lote.encoladas + lote.rechazadas < lote.total
            && lote.ids.insert(id)
        {
            lote.encoladas += 1;
        }
        self.desalojar_del_tablero();
        // And the HARD ceiling on what is retained (#271). This is only
        // reached with the board full of LIVE tasks, and only from the
        // unrelated-tasks channel: own ones never get past
        // `pedir_transferencia`, which refuses the whole batch if it does
        // not fit. An unrelated row that falls loses nothing — it arrives
        // with empty `afectados`, i.e. no refresh it owes — except a row
        // this window never promised to show.
        if self.tasks.len() >= MAX_TASKS_RETAINED && !self.tasks.contains_key(&id) {
            tracing::debug!(task = id, "board full: an unrelated task is not retained");
            return Vec::new();
        }
        let mut rx = task.progress.clone();
        let nacio = rx.borrow().clone();
        self.atar_la_creacion(id, ajena, nacio.kind);
        // #311: the checksums Task already has an id, so the intent noted
        // down when enqueuing it turns into the batch awaiting its report.
        // Only the OWN one: an unrelated task of the same kind is another
        // window's check, and hanging this report off it would give it
        // someone else's digests.
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
        // If this task was ALREADY on the board — a reconnection
        // re-announces it through the unrelated-tasks channel — what
        // arrives does not know which directories it touched, so what was
        // noted down is kept: replacing it with an empty list lost the
        // re-listing exactly on the path where the screen is most likely
        // to be stale.
        let afectados = if afectados.is_empty() {
            self.tasks
                .get(&id)
                .filter(|t| t.epoca == self.epoca_conexion)
                .map(|t| t.afectados.clone())
                .unwrap_or_default()
        } else {
            afectados
        };
        // An ACCEPTED mutation is proof the journal came back: the daemon
        // refuses to mutate without it (hard rule 4), so if this one got
        // in, the "not being recorded" warning stopped being true. There is
        // no recovery notification — the TUI has one because its journal is
        // embedded — and a warning that does not know how to turn off lies
        // about the one thing it describes for the whole session.
        let apaga_el_aviso = self.journal_rehusado && !ajena && Self::muta(vista.kind.as_str());
        if apaga_el_aviso {
            self.journal_rehusado = false;
        }
        // Same as with the affected directories: if it was already there,
        // whether its report was requested is preserved. A reconnection
        // re-announcing a finished batch cannot reopen the same report.
        let informe_pedido = self
            .tasks
            .get(&id)
            .is_some_and(|t| t.informe_pedido && t.epoca == self.epoca_conexion);
        let reintento = reintento.or_else(|| self.reintento_heredado(id));
        self.tasks.insert(
            id,
            TaskViva {
                vista,
                // The rate starts from zero: a second snapshot is needed
                // for there to be a speed, and until then the row stays
                // silent.
                rate: norte_frontend::tasks::Rate::default(),
                cancel: task.cancel,
                pause: task.pause,
                cola: task.cola,
                afectados,
                reintento,
                informe_pedido,
                epoca: self.epoca_conexion,
                progreso: task.progress.clone(),
            },
        );
        let buzon2 = buzon.clone();
        tokio::spawn(async move {
            // The NOW state was already projected by the registration; what
            // this pumps are the CHANGES. The terminal one goes through the
            // same ordered queue as everything else and is sent before
            // releasing the channel: an outcome that gets lost leaves the
            // user looking at progress that does not advance.
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
        // It can be born TERMINAL: the daemon completed it before this call
        // returned, and then `rx.changed()` never fires and `progreso` is
        // never called even once. Without this, a very fast copy left the
        // destination un-re-listed forever — the race task 5.1 names
        // literally.
        //
        // The light bar also sees it from here: one born finished is the
        // fast copy whose "✓" is the only thing that will say it happened.
        self.anotar_tira(buzon);
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

    /// What has to be handled when a task arrives on the board ALREADY
    /// finished.
    ///
    /// All of this would be done by `progreso`, and `progreso` is not going
    /// to be called even once: `rx.changed()` does not fire for a channel
    /// born with its final value. Without it, a very fast copy left the
    /// destination un-re-listed forever — the race task 5.1 names literally.
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
        // Its time on the board is counted from here, for the same reason.
        Self::programar_caducidad(id, self.epoca_conexion, buzon);
        let mut cambios = Vec::new();
        if self.anota_desenlace_de_lote(id, estado) {
            cambios.push(self.cambio_de_banners());
        }
        cambios.extend(self.refrescar_afectados(id, backend, buzon));
        // And its report, for the same reason as the re-listing: it is the
        // only signal that the directory was left halfway, and a very fast
        // batch used to be left without it right when the Task's outcome
        // most looks like everything went fine.
        self.pedir_informe_de_lote(nacio, backend, buzon);
        // #311: and the checksums' one, for the same reason. A batch of
        // three small files is born terminal almost always, so without this
        // the fast path — the most used one — showed nothing.
        self.pedir_informe_de_sumas(nacio, backend, buzon);
        cambios
    }

    /// Applies a progress snapshot to the board.
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
        // The rate BEFORE projecting: it is measured between this snapshot
        // and the previous one (spec 2026-09-15, ADR 0115). The wire does
        // not carry it, so whoever is watching estimates it — and the host
        // writes it, so the window and the terminal say the same speed with
        // the same units.
        viva.rate.observe(p, super::ahora_ms());
        let (ritmo, queda) = (
            norte_frontend::tasks::human_rate(viva.rate.bps()),
            norte_frontend::tasks::human_eta(viva.rate.eta_secs(p)),
        );
        viva.vista = Self::vista_de(p);
        viva.vista.rate = ritmo;
        viva.vista.eta = queda;
        // Whose task it is is not said by progress: it is said by where it
        // came from.
        viva.vista.foreign = ajena;
        // Read from the view that was just projected: rebuilding it only to
        // look at its state costs two `String`s and a `path_display` on
        // every progress tick of every task in the batch.
        let acabo = Self::terminal(viva.vista.state);
        let estado_final = viva.vista.state;
        // It just finished: its time on the board starts. Only on the
        // TRANSITION — the daemon repeats the last progress on reconnecting,
        // and rearming the clock on every repeat would leave the row there
        // forever, which is exactly the opposite of what is asked for.
        if acabo && !era_terminal {
            Self::programar_caducidad(p.task_id.get(), epoca, buzon);
        }
        // The panel that opens and closes on its own (`[ui] processes_panel
        // = "auto"`, ADR 0115): a panel taking up space to say "nothing
        // running" does not earn it, and hunting for the button right when a
        // copy starts does not either. It only closes what it itself opened,
        // and goes through BOTH HALVES of the gesture, never through the
        // toggle.
        self.anotar_tira(buzon);
        let del_panel = self.procesos_automaticos(backend, buzon);
        let mut cambios = self.cambios_de_linea();
        cambios.push(ViewChange::Tasks {
            tasks: self.vistas_de_tasks(),
            cursor: self.cursor_del_tablero(),
        });
        // The outcome enters the batch's count (#271). Something only
        // travels when the batch is RESOLVED: two hundred "one more"
        // phrases say nothing the row does not already say.
        if acabo && self.anota_desenlace_de_lote(p.task_id.get(), estado_final) {
            cambios.push(self.cambio_de_banners());
        }
        // An undo that FINISHES releases its session: while it runs, the
        // row says so and `u` over it is refused — two undos of the same
        // session walk the same entry list — and that cannot stay stuck
        // forever.
        if acabo && let Some(sesion) = self.agencia.undos.remove(&p.task_id.get()) {
            self.agencia.sesiones.deshecha(&sesion);
            if self.agencia.panel {
                cambios.push(ViewChange::Agents {
                    agents: self.vista_agentes(),
                });
            }
        }
        // If the one that just finished is THE search, its view stops saying
        // "searching…": a list that no longer grows and one that keeps
        // growing read the same if nobody tells them apart.
        // WITH the outcome, not just "is no longer alive": a search that
        // failed on the second directory and another that walked the whole
        // tree both painted as "N hits", which is a false assertion about
        // the disk — and whoever reads it stops searching.
        //
        // And what counts as an outcome is said by the shared crate, not by
        // a `!= Running`: `Pending`, `Paused` and `Unknown` are not one
        // either, and with that predicate an enqueued search — or one from a
        // newer daemon — announced itself finished with no hits.
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
        // A mutation that finished leaves screens out of date: the new entry
        // is on disk and not in the listing. Only with a REAL outcome —
        // `Running` is not one — and only once.
        if acabo {
            cambios.extend(self.refrescar_afectados(p.task_id.get(), backend, buzon));
            self.pedir_informe_de_lote(p, backend, buzon);
            // And the timeline (#359): what just happened — or was just
            // undone — has to show up in a panel that stays open.
            self.recargar_lineas();
            cambios.extend(self.cerrar_comparacion(p));
            cambios.extend(self.cerrar_sincronizacion(p));
            self.pedir_informe_de_sync(p, backend, buzon);
            // #311: and the checksums' one, which is where the digests
            // travel.
            self.pedir_informe_de_sumas(p, backend, buzon);
            cambios.extend(self.decir_el_recuento(p));
            cambios.extend(self.ofrecer_reintento(p));
            self.abrir_lo_creado(p, backend, buzon);
            self.avisar_del_desenlace(p);
        }
        // The automatic panel travels BEHIND the patch and separately:
        // opening or closing a slot rebuilds the whole layout, so those are
        // already-built envelopes — with their own snapshot — and not one
        // more change in this list.
        let mut fuera = vec![self.parche(cambios)];
        fuera.extend(del_panel);
        fuera
    }

    /// A count's TOTAL, which is the only thing that count produces
    /// (#139, #290).
    ///
    /// `fs.dir_size` publishes nothing and mutates nothing: its result **is**
    /// its terminal progress. Without this, the window would launch the
    /// count, see it finish on the board, and never say how much it took up.
    ///
    /// **A total with something unreadable inside is stated DIFFERENTLY**: a
    /// count is used to decide whether something FITS at the destination, so
    /// giving it round without having been able to count it in full is a
    /// wrong answer, not an incomplete one. With unreadable ones, "at least"
    /// is said, which is what is known.
    ///
    /// `unreadable: None` — a 0.52 daemon, which did not count them — reads
    /// as zero, same as in the TUI (`refresh.rs`): staying quiet about the
    /// total because the other end is old would be worse than giving it.
    /// Both surfaces have to say the same thing given the same progress.
    ///
    /// Only with `Completed`: a cancelled or failed count has no total to
    /// give, and painting a cancellation's partial as if it were the answer
    /// is the same bug above under another name.
    /// Sends a desktop notification when an agent ASKS for permission
    /// (#285).
    ///
    /// Of the three notifications, this is the one that justifies the
    /// mechanism: an approval has a TTL and denies itself if nobody answers,
    /// so not noticing changes the outcome. A finished copy stays finished
    /// when you come back.
    ///
    /// Carries the OPERATION and who is asking, not the paths: the
    /// request's body can be long and the dialog shows it in full when it
    /// opens. What the notification has to achieve is that someone looks.
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

    /// Sends a desktop notification when a task FINISHES (#285).
    ///
    /// Only with the window WITHOUT focus: if it is up front, the status bar
    /// and the board already say the same thing, and repeating it outside is
    /// noise. It is the only condition — a notification that also depended
    /// on how long the task took would need a threshold, and picking a good
    /// one is a separate decision.
    ///
    /// The file's name DOES go inside, and that is why it goes through the
    /// same masking as the listing: a notification ends up in the desktop's
    /// history and can be seen on the lock screen, so a name with bidi or
    /// control characters cannot fake there what it cannot fake here.
    pub(super) fn avisar_del_desenlace(&mut self, p: &norte_proto::TaskProgress) {
        if self.enfocada {
            return;
        }
        let (clave, cuenta) = match &p.state {
            norte_proto::TaskState::Completed => ("notify-task-done", p.entries_done),
            norte_proto::TaskState::Failed { .. } => ("notify-task-failed", p.entries_done),
            // Cancelling was requested by whoever is in front: no need to
            // tell them.
            _ => return,
        };
        // Which file was in flight, if progress says so. `current` is a
        // path from the other end: it is masked and shortened the same as a
        // row.
        //
        // Over the last segment's RAW bytes, not over `display_lossy()`:
        // there, the U+FFFDs are already in place, and masking text that is
        // already flawless UTF-8 always returns "faithful". Here the verdict
        // is not used — a desktop notification has nowhere to put a badge —
        // but the MASKING is, and over the lossy version it did nothing: it
        // is the same bug just fixed in the collision dialog, two functions
        // below.
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

    /// A transfer that COLLIDED opens the missing question (#274).
    ///
    /// The window always sends `CollisionPolicy::Fail`, the safe default —
    /// overwriting or renaming are the reader's decisions — but had nowhere
    /// to make them: a failed task was left on the board with no path
    /// forward, while the TUI does offer the four outcomes.
    ///
    /// Only with a `Conflict` and only if the task carries something to
    /// retry with: a delete or an undo have no other policy to offer, and an
    /// unrelated task does not belong to this window.
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
        // The destination, in its own field and masked: it is a file name
        // from the other end, and it is WHAT the reader has to look at to
        // decide whether to overwrite. Through the funnel, which masks the
        // RAW bytes: over `display_lossy()` the U+FFFDs were already in
        // place and the verdict came out "faithful" — no badge, on the one
        // screen where overwriting is approved.
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
            overflow_hostile: false,
            // The SAME four as the TUI, and in the same order: it is the
            // shared catalog's `dialog.*` table, not a list invented here.
            choices: vec![
                DialogChoice {
                    id: "overwrite".to_owned(),
                    label_key: "dialog-overwrite".to_owned(),
                    // Overwriting DESTROYS what is at the destination.
                    destructive: true,
                },
                DialogChoice {
                    id: "newer".to_owned(),
                    label_key: "dialog-newer".to_owned(),
                    // It also overwrites, only conditioned on the date.
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
            fields: Vec::new(),
            dest_check: crate::dto::DestCheckView::NotAsked,
        };
        self.dialogos.push(Dialogo {
            id: modal,
            vista,
            tecleado: Tecleado::Texto(String::new()),
            // It opened ON ITS OWN — it arrives when the task finishes, on
            // top of whatever the reader was doing — so the first answer
            // only acknowledges it. It is the same rule as an agent
            // approval, and here it matters just as much: the first option
            // is "overwrite".
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

    /// A rename batch that just finished: its report is requested.
    ///
    /// It is the ONLY signal that the directory was left HALFWAY, and it has
    /// to be requested EVEN IF the Task says `Completed`: the Task's outcome
    /// talks about the batch, and the report talks about what was left on
    /// disk.
    ///
    /// It is also requested for an UNRELATED batch — another client of this
    /// session — for the same reason: the half-renamed directory is the same
    /// no matter who looks at it, and whoever has this window in front of
    /// them is who is going to see it.
    pub(super) fn pedir_informe_de_lote(
        &mut self,
        p: &norte_proto::TaskProgress,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        // Three classes have a report, and all three for the same reason:
        // what has to be said does not fit in a Task's outcome. The first
        // two count what was left halfway; the third counts something about
        // a pack that came out FINE (#250).
        #[derive(Clone, Copy)]
        enum Cual {
            Lote,
            Undo,
            Empaquetado,
        }
        let cual = match p.kind {
            norte_proto::TaskKind::RenameBatch => Cual::Lote,
            norte_proto::TaskKind::Undo => Cual::Undo,
            // **Only a pack that COMPLETED.** The other two reports talk
            // about what was left halfway, so a `Failed` or a `Cancelled` is
            // exactly when they are most needed; this one talks about an
            // archive, and there is no such thing as a cancelled pack —
            // cancellation leaves the destination clean. The report exists
            // regardless (it is computed before writing), and painting it
            // would say "packed, but…" about something nobody packed.
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

    /// Enqueuing a mutation failed: it is reported, and if it was because of
    /// the journal it stays reported.
    ///
    /// `error_key` returns a Fluent KEY, and `StatusView.message`'s contract
    /// says "already translated by the host": untranslated, the user read
    /// `err-not-found` in the status bar.
    pub(super) fn task_fallida(&mut self, e: &Error) -> Vec<BridgeEnvelope<UiUpdate>> {
        let clave = norte_frontend::error::error_key(e);
        self.status.message = Some(clamp_display(norte_i18n::t_in(self.lang, clave)));
        // A journal rejection is not a mutation gone wrong: it is that THIS
        // SESSION does not mutate until the file is fixed (hard rule 4).
        // That lasts longer than one message.
        self.journal_rehusado |= matches!(e, Error::JournalUnavailable);
        // A creation that never even got enqueued releases its intent: with
        // no task there is no outcome to consume it, and staying stuck
        // would make the NEXT `edit-new` open this one's file, which does
        // not exist.
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

    /// A batch entry was rejected by the daemon on enqueuing (#271).
    ///
    /// Paints nothing: it counts. With `CollisionPolicy::Fail` against a
    /// populated destination, rejections are the norm, and N messages of
    /// which only the last survives do not even say how many there were.
    ///
    /// With no batch open — should not happen, the loop only sends this
    /// inside one — it falls back to the status bar, which is what it used
    /// to do before: losing the whole notice entirely is worse than painting
    /// it where it was already being painted.
    pub(super) fn rechazo_de_lote(&mut self, e: &Error) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Some(lote) = self.lote.as_mut() else {
            return self.task_fallida(e);
        };
        lote.rechazadas += 1;
        // A journal rejection still means the same thing even coming from a
        // batch: this session does NOT mutate until the file is fixed (hard
        // rule 4), and that lasts longer than any summary — and it is the
        // only thing about a lone rejection that DOES travel before the end.
        let antes = self.journal_rehusado;
        self.journal_rehusado |= matches!(e, Error::JournalUnavailable);
        let banner_nuevo = self.journal_rehusado != antes;
        if !self.resumen_de_lote_si_cerrado() && !banner_nuevo {
            // A patch per rejection is the storm this exists to silence:
            // while the batch stays open, nothing travels.
            return Vec::new();
        }
        let cambio = self.cambio_de_banners();
        vec![self.parche(vec![cambio])]
    }

    /// Notes down ONE batch task's outcome (#271). `true` if the batch was
    /// resolved by it and `status.message` already carries the summary.
    ///
    /// The id is removed from the count when noted down: terminal progress
    /// can arrive more than once — a re-announcement after reconnecting
    /// brings the final state again — and the second one is not a second
    /// outcome.
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

    /// If the batch is resolved, puts the summary in the status bar and
    /// closes it.
    pub(super) fn resumen_de_lote_si_cerrado(&mut self) -> bool {
        let Some(lote) = self.lote.as_ref() else {
            return false;
        };
        if !lote.cerrado() {
            return false;
        }
        // Rejected on enqueuing and terminated badly are the same outcome
        // for whoever is watching: it did not arrive. Telling them apart
        // would need two more numbers in a phrase that has to fit in the
        // status bar.
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

    /// A report arrived: onto the board, and up front if it left something
    /// halfway.
    pub(super) fn informe(
        &mut self,
        epoca: u64,
        task_id: u64,
        cual: &Informe,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // A report that came out BEFORE the reconnection talks about a task
        // from a different daemon, and the id can be reused: hanging it off
        // the row that carries that number today would open a "was left
        // halfway" about the wrong directory.
        if epoca != self.epoca_conexion {
            return Vec::new();
        }
        match cual {
            Informe::Lote(r) => self.informe_de_lote(task_id, r),
            Informe::Undo(r) => self.informe_de_undo(task_id, r),
            Informe::Empaquetado(r) => self.informe_de_empaquetado(r),
        }
    }

    /// A pack's report arrived (#250).
    ///
    /// **A clean report says nothing, and that is the design**: the normal
    /// answer is that the archive travels whole, and warning about it would
    /// teach people to skip the warning that does matter. An error is not
    /// painted either: against an N-1 daemon the method does not exist, and
    /// "could not ask" is not a finding about the archive.
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
        // Trimmed says "at least": the list is cut off at
        // `ARCHIVE_PACK_REPORT_MAX`, and painting the cap as if it were the
        // total is the lie `truncated` exists to prevent.
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

    /// An undo's report arrived: onto the board, and up front if something
    /// did not come back.
    ///
    /// Same shape as [`Self::informe_de_lote`] because it is the same
    /// question — what was left undone — asked about a different Task
    /// class.
    pub(super) fn informe_de_undo(
        &mut self,
        task_id: u64,
        resultado: &Result<norte_proto::methods::PolicyUndoReportResult, Error>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // A row that is no longer there does NOT drop the report: the board
        // is capped and the task could have fallen off while the report was
        // in flight, but what got lost that way was exactly "was left
        // halfway", which never folds into "went fine". With no row, the
        // detail is skipped and whatever needs to be said is still shown.
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

    /// `true` if the undo returned EVERYTHING it should have.
    ///
    /// What was skipped counts as not-clean: an irreversible entry or a
    /// creation that stays because the destination has no trash are things
    /// that did NOT come back, and a report that stayed quiet about them
    /// would say the tree is as it was.
    pub(super) fn undo_limpio(r: &norte_proto::methods::PolicyUndoReportResult) -> bool {
        norte_frontend::undo_report_is_clean(r)
    }

    /// An undo report's body: what came back and what did not. The lines are
    /// decided by `norte_frontend::undo_report_lines`, which is also what the
    /// terminal shows; here they are only painted.
    pub(super) fn cuerpo_de_undo(
        &self,
        r: &norte_proto::methods::PolicyUndoReportResult,
    ) -> Vec<crate::dto::DialogLine> {
        Self::pintar_informe(norte_frontend::undo_report_lines(r, self.lang))
    }

    /// A shared report's lines, painted: the phrases clamped, and each path
    /// as a path line — masked and flagged.
    fn pintar_informe(lineas: Vec<norte_frontend::ReportLine>) -> Vec<crate::dto::DialogLine> {
        lineas
            .into_iter()
            .map(|linea| match linea {
                norte_frontend::ReportLine::Phrase(texto) => crate::dto::DialogLine {
                    text: clamp_display(texto),
                    hostile: false,
                },
                norte_frontend::ReportLine::Path(p) => Self::linea_de_ruta(&p),
            })
            .collect()
    }

    /// The report arrived: it is noted on the board and, if the batch left
    /// something halfway, it is said UP FRONT.
    ///
    /// Two surfaces and not one: the board's row keeps the summary — it
    /// survives whatever anyone closes — and the dialog is what keeps a
    /// half-renamed directory from going unnoticed. A clean batch opens
    /// nothing: there is nothing to look for.
    pub(super) fn informe_de_lote(
        &mut self,
        task_id: u64,
        resultado: &Result<norte_proto::methods::FsRenameBatchReportResult, Error>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // See [`Self::informe_de_undo`]: with no row, the report is shown
        // just the same. What is not done is inventing a row to hang it off
        // of.
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
        // The row's detail is "what is currently in flight" while it runs;
        // once finished, what matters is what it ended up as. There is no
        // more progress behind it to overwrite it: the state is terminal.
        if let Some(t) = self.tasks.get_mut(&task_id) {
            t.vista.detail = Some(clamp_display(detalle));
            t.vista.detail_hostile = false;
        }
        let mut cambios = vec![ViewChange::Tasks {
            tasks: self.vistas_de_tasks(),
            cursor: self.cursor_del_tablero(),
        }];
        // It opens by what the REPORT says, not by how the Task finished: a
        // `Completed` batch with a stuck step is exactly the case the
        // Task's outcome does not count.
        let hay_que_decirlo = match resultado {
            Ok(r) => !Self::lote_limpio(r),
            // A report that could not be requested about a batch that also
            // failed leaves the directory with no explanation: that is said
            // up front. If the batch finished fine, the board's row is
            // enough.
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

    /// `true` if the batch left nothing to look for or to finish off.
    pub(super) fn lote_limpio(r: &norte_proto::methods::FsRenameBatchReportResult) -> bool {
        norte_frontend::batch_report_is_clean(r)
    }

    /// The one-line summary that stays in the board's row.
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

    /// The report's body: what was applied, what could not be returned, and
    /// WHAT THE THING LEFT HALFWAY IS CALLED NOW.
    ///
    /// The lines are decided by `norte_frontend::batch_report_lines`, which
    /// is also what the CLI prints; here they are only painted. The current
    /// name goes as a path line — masked and flagged — never inside a
    /// sentence.
    pub(super) fn cuerpo_de_lote(
        &self,
        r: &norte_proto::methods::FsRenameBatchReportResult,
    ) -> Vec<crate::dto::DialogLine> {
        Self::pintar_informe(norte_frontend::batch_report_lines(r, self.lang))
    }

    /// Stacks a dialog, with a cap.
    ///
    /// The cap exists because the stack is fed by the WIRE since task 5.3
    /// (approvals and reports, unrelated tasks' too). The oldest UNACKNOWLEDGED
    /// one falls — the one nobody has gotten around to looking at — and never
    /// the top one, which is the one being answered; if all are acknowledged,
    /// the oldest one. That one fell IS SAID: a question disappearing in
    /// silence is worse than a long stack.
    pub(super) fn apilar_dialogo(&mut self, dialogo: Dialogo) -> Vec<BridgeEnvelope<UiUpdate>> {
        let mut fuera = Vec::new();
        if self.dialogos.len() >= MAX_DIALOGS {
            // A REPORT is sacrificed before a decision: the report also
            // lives on the board's row, and an approval that disappears
            // leaves an agent waiting. If only decisions are left, the
            // oldest one falls — the daemon will eventually apply its TTL to
            // that one, which is a denial.
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

    /// Opens a report's dialog. It only informs: it has nothing to execute,
    /// and its only answer closes it.
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
            overflow_hostile: false,
            choices: vec![DialogChoice {
                id: "ok".to_owned(),
                label_key: "dialog-ok".to_owned(),
                destructive: false,
            }],
            input: None,
            input_hostile: false,
            input_secret: false,
            fields: Vec::new(),
            dest_check: crate::dto::DestCheckView::NotAsked,
        };
        let caidos = self.apilar_dialogo(Dialogo {
            id,
            vista,
            tecleado: Tecleado::Texto(String::new()),
            // It opens ON ITS OWN, when the daemon answers.
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

    /// `task.cancel`: asks ONE task to stop, and says which or that there is
    /// none.
    ///
    /// Which task it is depends on where focus is, and not by whim: with the
    /// processes panel in front, the board paints a cursor, and a key
    /// cancelling something else would leave that cursor painting a
    /// selection with no say. Without that panel focused, the LAST alive one
    /// is cancelled, which is what the TUI does with the same key.
    pub(super) fn cancelar_por_comando(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match self.task_a_cancelar() {
            Objetivo::Ninguna => (self.aplicada(), self.decir("msg-no-tasks")),
            Objetivo::Terminada => (self.aplicada(), self.decir("msg-task-finished")),
            Objetivo::Viva(id) => {
                // A window with no effects does not abort ANOTHER client's
                // task: cancelling a copy leaves the destination clean or a
                // `.norte-partial`, i.e. it touches disk. Its own ones,
                // though, since launching them already needed the toggle.
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

    /// Pauses the chosen task, or resumes it if already paused (ADR 0147).
    ///
    /// Chosen the same way as cancel — the cursor's with the processes panel
    /// focused, and otherwise the most recent alive one — and looks at its
    /// LIVE state to decide the direction. The request goes to the daemon
    /// outside the actor; if it does not know how to pause (`Unsupported`, a
    /// 0.81 daemon) a message comes back saying so, because a pause that
    /// does not happen and is not reported is worse than not offering it.
    pub(super) fn pausar_por_comando(
        &mut self,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let id = match self.task_a_cancelar() {
            Objetivo::Ninguna => return (self.aplicada(), self.decir("msg-no-tasks")),
            Objetivo::Terminada => return (self.aplicada(), self.decir("msg-task-finished")),
            Objetivo::Viva(id) => id,
        };
        let Some(viva) = self.tasks.get(&id) else {
            return (self.aplicada(), self.decir("msg-no-tasks"));
        };
        if self.efectos == crate::commands::Efectos::SoloLectura && viva.vista.foreign {
            return Self::no_muta();
        }
        let (clase, estado) = {
            let p = viva.progreso.borrow();
            (p.kind, p.state.clone())
        };
        if !norte_frontend::tasks::pausable(clase) {
            return (self.aplicada(), self.decir("msg-pause-not-this"));
        }
        let Some(mando) = viva.pause.clone() else {
            return (self.aplicada(), self.decir("msg-pause-unsupported"));
        };
        let pausar = estado != norte_proto::TaskState::Paused;
        let buzon = buzon.clone();
        tokio::spawn(async move {
            if let Err(norte_proto::Error::Unsupported) = mando(pausar).await {
                let _ = buzon.send(Mensaje::Decir("msg-pause-unsupported")).await;
            }
        });
        let aviso = if pausar {
            "msg-pausing"
        } else {
            "msg-resuming"
        };
        (self.aplicada(), self.decir(aviso))
    }

    /// Turns the serial queue on or off for whatever launches from now on
    /// (ADR 0149). What is already queued stays in its queue.
    pub(super) fn alternar_cola(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.encolar = !self.encolar;
        let aviso = if self.encolar {
            "msg-queue-on"
        } else {
            "msg-queue-off"
        };
        (self.aplicada(), self.decir(aviso))
    }

    /// Moves the board's pointed-at task up or down the queue (ADR 0149).
    ///
    /// The request goes outside the actor, like the pause: what really
    /// happened shows in the order they come out in, and a daemon that does
    /// not know the queue says so.
    pub(super) fn mover_en_cola_por_comando(
        &mut self,
        arriba: bool,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let id = match self.task_a_cancelar() {
            Objetivo::Ninguna => return (self.aplicada(), self.decir("msg-no-tasks")),
            Objetivo::Terminada => return (self.aplicada(), self.decir("msg-task-finished")),
            Objetivo::Viva(id) => id,
        };
        let Some(mando) = self.tasks.get(&id).and_then(|t| t.cola.clone()) else {
            return (self.aplicada(), self.decir("msg-queued-not-moved"));
        };
        let buzon = buzon.clone();
        tokio::spawn(async move {
            if mando(arriba).await.is_err() {
                let _ = buzon.send(Mensaje::Decir("msg-queued-not-moved")).await;
            }
        });
        (self.aplicada(), self.decir("msg-queued-moved"))
    }

    /// What is arriving at `hueco`, 0–100 (ADR 0148): the percentage of the
    /// live WORK task whose affected directories include its own.
    ///
    /// With several, the LEAST advanced rules, as in the row's rule
    /// (`processes::progress_for`): what that pane needs to be at ease is
    /// whatever the most behind one still needs.
    pub(super) fn progreso_de_hueco(&self, hueco: &Hueco) -> Option<u8> {
        let dir = hueco.pane.dir();
        self.tasks
            .values()
            .filter(|t| t.epoca == self.epoca_conexion && !Self::terminal(t.vista.state))
            .filter(|t| t.afectados.iter().any(|d| d == dir))
            .filter(|t| norte_frontend::tasks::counts_as_work(t.progreso.borrow().kind))
            .filter_map(|t| norte_frontend::tasks::progress_pct(&t.progreso.borrow()))
            .min()
    }

    /// The fine-grained line changes that need sending, compared with the
    /// last one that crossed: two pixels are not worth a listing.
    pub(super) fn cambios_de_linea(&mut self) -> Vec<ViewChange> {
        let ahora: Vec<(u32, Option<u8>)> = self
            .huecos
            .iter()
            .map(|(id, h)| (*id, self.progreso_de_hueco(h)))
            .collect();
        let mut cambios = Vec::new();
        for (id, pct) in ahora {
            if self.ultima_linea.get(&id).copied() != Some(pct) {
                self.ultima_linea.insert(id, pct);
                cambios.push(ViewChange::SlotProgress {
                    slot_id: id,
                    progress: pct,
                });
            }
        }
        cambios
    }

    /// Repeats the most recent failed transfer, with its SAME options
    /// (ADR 0148).
    ///
    /// The context was already saved for the collision dialog (#274); what
    /// was missing was being able to use it when what failed was not a
    /// collision — a network that dropped, a destination that filled up —
    /// and the operation had to be redone by hand.
    pub(super) fn reintentar_por_comando(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.efectos == crate::commands::Efectos::SoloLectura {
            return Self::no_muta();
        }
        let visibles: Vec<_> = self.tasks_visibles().collect();
        let con = visibles.into_iter().rev().find_map(|(_, t)| {
            matches!(
                t.vista.state,
                crate::dto::TaskStateView::Failed | crate::dto::TaskStateView::Cancelled
            )
            .then(|| t.reintento.clone())
            .flatten()
        });
        let Some(con) = con else {
            return (self.aplicada(), self.decir("msg-no-retry"));
        };
        // With the policy requested the first time: repeating is not
        // deciding something else, and a collision asks again just like
        // before.
        Self::lanzar_reintento(
            con,
            norte_proto::CollisionPolicy::Fail,
            self.encolar,
            backend,
            buzon,
        );
        (self.aplicada(), self.decir("msg-retrying"))
    }

    /// Moves the board's chosen row.
    ///
    /// Without needing the processes panel's focus: the board is painted
    /// even when that slot does not exist — tasks go out in the envelope —
    /// and a command that only worked with one specific slot open would be a
    /// key that depends on the layout.
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
        // SNAPSHOT and not a patch. Since bridge 57 the cursor DOES have
        // somewhere to travel (`ViewChange::Tasks`), so this is no longer
        // "there is no contract": it is that a key that only moves the
        // choice does not need to resend the whole board, and the snapshot
        // is what this path has been doing without complaint. Changing it
        // is an optimization, not a fix.
        let snap = self.snapshot();
        (
            self.aplicada(),
            vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))],
        )
    }

    /// Removes the board's chosen row, if it has ALREADY finished.
    ///
    /// A live one is not discarded: stopping it is `task.cancel`, and
    /// removing from view something still writing to disk is losing sight
    /// of exactly what needs watching.
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
        // The cursor is NOT re-clamped here, and it used to be: the shared
        // type's rule is to read the row by the chosen one's IDENTITY and
        // fall back to the remembered position only if that one is gone.
        // Discarding the last one leaves the selection on what is now the
        // last one — same as before — and, unlike before, if the board grows
        // again the choice goes back to where it was instead of having
        // stayed stuck.
        let mut fuera = vec![self.parche(vec![ViewChange::Tasks {
            tasks: self.vistas_de_tasks(),
            cursor: self.cursor_del_tablero(),
        }])];
        let snap = self.snapshot();
        fuera.push(self.sobre(UiUpdate::Snapshot(Box::new(snap))));
        (self.aplicada(), fuera)
    }

    /// Releases the "undoing" of a session whose task is discarded.
    ///
    /// Discarding a finished undo's row is the same as seeing it finish: if
    /// it were not released here, that session would stay marked "undoing"
    /// forever and `u` over it would be refused for no reason.
    pub(super) fn undos_sin_task(&mut self, task_id: u64) {
        if let Some(sesion) = self.agencia.undos.remove(&task_id) {
            self.agencia.sesiones.deshecha(&sesion);
        }
    }

    /// Which task is due to stop.
    pub(super) fn task_a_cancelar(&self) -> Objetivo {
        if self.procesos_tienen_el_foco() {
            // The cursor's, whatever its state: a human chose it by looking
            // at it. If it already finished, it IS SAID, instead of jumping
            // to another one — cancelling a task that is not the pointed-at
            // one is worse than not cancelling anything.
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
        // The board goes by id, and the daemon hands them out increasing:
        // the last alive one is the one with the highest id.
        self.tasks
            .iter()
            .rev()
            .find(|(_, t)| Self::sigue_viva(t))
            .map_or(Objetivo::Ninguna, |(id, _)| Objetivo::Viva(*id))
    }

    /// `true` if this task class WRITES.
    ///
    /// By the catalog's key and not by `TaskKind`, which is not exhaustive:
    /// a class from a newer daemon falls into `unknown` and does NOT count
    /// as a mutation, which is the safe side — turning off the journal
    /// warning for something this host does not know what it does would be
    /// turning it off just in case.
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
                // #314: changing permissions MUTATES, with a journal entry
                // and a reversal.
                | "set-mode"
        )
    }

    /// `true` if this task can still be asked to stop.
    ///
    /// Asks the LIVE progress and not the projected view: between the daemon
    /// marking the outcome and `Mensaje::Progreso` leaving the mailbox, the
    /// view says it is still running. Over that snapshot, "cancelling…" used
    /// to be answered about something already finished, and the one chosen
    /// as "last alive" was one that no longer was, leaving the one that
    /// really remained still running.
    pub(super) fn sigue_viva(t: &TaskViva) -> bool {
        !t.progreso.borrow().state.is_terminal()
    }

    /// `true` if focus is on the processes panel.
    pub(super) fn procesos_tienen_el_foco(&self) -> bool {
        self.roles
            .get(RoleId::Active)
            .and_then(|s| kind_de(&self.arbol, s))
            .is_some_and(|k| k.as_str() == "processes")
    }

    /// States a phrase: in the status bar AND as a notification.
    ///
    /// Both things, and through the same call, which is what a failed task
    /// already does: the status bar is where it is read on looking, and the
    /// notification is what the renderer can announce to a screen reader.
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

    /// Like [`Self::decir_con`], with no arguments.
    pub(super) fn decir(&mut self, clave: &str) -> Vec<BridgeEnvelope<UiUpdate>> {
        self.status.message = Some(clamp_display(norte_i18n::t_in(self.lang, clave)));
        let parche = self.parche(vec![ViewChange::Status(self.status.clone())]);
        let aviso = self.sobre(UiUpdate::Notice(UiNotice::Message {
            key: clave.to_owned(),
            detail: None,
        }));
        vec![parche, aviso]
    }

    /// Requests a task's cancellation. Idempotent by contract: requesting it
    /// twice is not an error and changes nothing.
    pub(super) fn cancelar(&mut self, task_id: u64) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(viva) = self.tasks.get(&task_id) else {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        };
        (viva.cancel)();
        (self.aplicada(), Vec::new())
    }

    /// The board that crosses the bridge, capped to [`MAX_TASKS`].
    ///
    /// `registrar_task`'s eviction can only drop FINISHED tasks, so a batch
    /// larger than the cap — marking three thousand files and pressing F5
    /// is the normal flow — has nothing to evict and the map grows past the
    /// cap the bridge's contract promises. It is capped here, which is where
    /// the number means something: how many rows travel.
    ///
    /// The NEWEST ones stay (the map is ordered by id, which is monotonic):
    /// what matters about a batch in progress is its front, not the first
    /// ones enqueued.
    pub(super) fn vistas_de_tasks(&self) -> Vec<TaskView> {
        self.tasks_visibles()
            .map(|(_, t)| t.vista.clone())
            .collect()
    }

    /// The tasks that CROSS the bridge, in the order they are painted.
    ///
    /// ONE single definition of "the visible ones", and not by whim: the
    /// board is trimmed to [`MAX_TASKS`] and the cursor is an INDEX. While
    /// the trim lived only here and the cursor counted over the whole map,
    /// with more than 256 tasks — marking three thousand files and pressing
    /// F5 is the normal flow, and eviction only takes the FINISHED ones —
    /// the highlighted row and the task being cancelled were two different
    /// tasks. It is literally what the panel's rustdoc forbids: "two task
    /// lists split apart, and the one that is seen stops being the one that
    /// is cancelled".
    pub(super) fn tasks_visibles(&self) -> impl Iterator<Item = (&u64, &TaskViva)> {
        let sobran = self.tasks.len().saturating_sub(MAX_TASKS);
        self.tasks.iter().skip(sobran)
    }

    /// Opens or closes the processes panel on its own, and says what
    /// changed.
    ///
    /// Both HALVES of the gesture, never the toggle: reusing
    /// `alternar_hueco` would close the panel when the second task starts
    /// and reopen the one the reader just closed (ADR 0115).
    pub(super) fn procesos_automaticos(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        if self.config.common.ui_chrome.processes_panel()
            != norte_config::load::ProcessesPanel::Auto
        {
            return Vec::new();
        }
        // OPENS when the work burst has already LASTED (ADR 0146): what
        // finishes sooner is counted by the status bar without taking a
        // third of the screen away from the listing. CLOSES as always, when
        // no work row is left: that way the one that took a while is seen
        // finished.
        let abre = self.tira.wants_panel(self.reloj_tira());
        let has_work = self.has_work();
        let open = self.hueco_de_kind("processes").is_some();
        if abre && !open {
            self.procesos_auto = true;
            return self.abrir_hueco_de_kind("processes", backend, buzon).1;
        }
        if !has_work && open && self.procesos_auto {
            self.procesos_auto = false;
            return self.cerrar_hueco_de_kind("processes", backend, buzon).1;
        }
        Vec::new()
    }

    /// The light bar's clock, in ms since the host started: tokio's, so
    /// tests can pause and fast-forward it.
    pub(super) fn reloj_tira(&self) -> i64 {
        i64::try_from(self.tira_base.elapsed().as_millis()).unwrap_or(i64::MAX)
    }

    /// Shows the board to the light bar (ADR 0146) and schedules the next
    /// wake-up if the bar is going to change with no progress arriving.
    pub(super) fn anotar_tira(&mut self, buzon: &mpsc::Sender<Mensaje>) {
        let ahora = self.reloj_tira();
        // Copies: progress lives behind a `watch`, and its guard cannot
        // cross the call. There are few of them (the board has a cap) and
        // they are small.
        //
        // Only THIS connection's: a task from a daemon that is no longer
        // there is never going to finish, and with it inside the burst
        // would never close.
        let fotos: Vec<(norte_proto::TaskProgress, Option<f64>)> = self
            .tasks
            .values()
            .filter(|t| t.epoca == self.epoca_conexion)
            .map(|t| (t.progreso.borrow().clone(), t.rate.bps()))
            .collect();
        self.tira.update(
            ahora,
            fotos
                .iter()
                .map(|(p, bps)| norte_frontend::task_strip::StripTask {
                    progress: p,
                    operand: p.current.as_ref(),
                    bps: *bps,
                }),
        );
        let Some(when) = self.tira.next_change_ms(ahora) else {
            return;
        };
        if self.tira_despertar == Some(when) {
            return;
        }
        self.tira_despertar = Some(when);
        let espera = std::time::Duration::from_millis(u64::try_from(when - ahora).unwrap_or(0));
        let buzon = buzon.clone();
        tokio::spawn(async move {
            tokio::time::sleep(espera).await;
            let _ = buzon.send(Mensaje::Tira).await;
        });
    }

    /// The time the bar asked for arrived: it is renoted, and a patch
    /// travels only if something depending on it changed (the status bar's
    /// elements are compared by `parche` alone; the panel, by
    /// `procesos_automaticos`).
    pub(super) fn despertar_tira(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        self.tira_despertar = None;
        self.anotar_tira(buzon);
        let mut envios = self.procesos_automaticos(backend, buzon);
        if envios.is_empty()
            && self.ultimos_elementos.as_ref() != Some(&self.vista_elementos_de_estado())
        {
            envios.push(self.parche(Vec::new()));
        }
        envios
    }

    /// How many rows the PAINTED board has.
    pub(super) fn filas_de_tablero(&self) -> usize {
        self.tasks.len().min(MAX_TASKS)
    }

    /// `true` if there is WORK in progress, the only thing that opens the
    /// panel on its own.
    ///
    /// Not `filas_de_tablero() > 0`: the board also lists the observational
    /// classes — a search, a checksum, a directory size — and opening a
    /// third of the screen for a search covers the hits list to say what
    /// that list already says. The TUI never put those in its board, so
    /// without this separate count the two frontends opened the panel in
    /// different scenarios; the rule lives in one single place, in
    /// [`norte_frontend::tasks::counts_as_work`] (ADR 0077, ADR 0115).
    ///
    /// Asks the LIVE progress because that is where the typed class is; the
    /// projected view only carries its name.
    fn has_work(&self) -> bool {
        // Only this connection's, for the same reason as in `anotar_tira`:
        // a previous daemon's work is never going to finish, and the panel
        // would not close.
        self.tasks.values().any(|t| {
            t.epoca == self.epoca_conexion
                && norte_frontend::tasks::counts_as_work(t.progreso.borrow().kind)
        })
    }

    /// The PAINTED tasks' ids, in the order they are painted.
    ///
    /// It is what the panel's cursor needs: it stores the chosen one's
    /// IDENTITY, not its position, because the board moves on its own and a
    /// row expiring above it would make the same position name a different
    /// task.
    pub(super) fn ids_del_tablero(&self) -> Vec<u64> {
        self.tasks_visibles().map(|(id, _)| *id).collect()
    }

    /// Which board row is chosen, over the PAINTED rows.
    ///
    /// An index over what the renderer highlights, not over the whole map:
    /// with the board trimmed by the cap, it pointed at a different one.
    /// `None` with zero rows, because an index with no row behind it
    /// highlights nothing.
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
