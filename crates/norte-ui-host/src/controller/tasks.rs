//! The tasks board: progress, outcome, report and cancellation.
//!
//! Part of `controller`: these are `State` methods, moved here without
//! touching them (ADR 0086). The only writer is still the actor.

// These modules are the same `impl State` split into pieces, so they use
// the same imports as the parent. Enumerating them here would be a
// forty-line list per file, in 32 files, that goes stale the moment the
// parent imports something — `super::*` tracks it on its own.
use super::sums::Published;
#[allow(clippy::wildcard_imports)]
use super::*;

/// A checksums batch in flight (#311).
///
/// The Task is already on the board; what is awaited here is its REPORT,
/// which is where the digests travel — they do not fit in a Task's outcome
/// nor in its progress.
pub(super) struct ChecksumsInFlight {
    /// The Task whose report is awaited.
    pub(super) task: norte_proto::TaskId,
    /// Which CONNECTION epoch that Task lives in: after a handoff the
    /// daemon's ids start over at 1, and a report for another task with the
    /// same number would count the check for something else.
    pub(super) epoch_connection: u64,
    /// Its report has already been requested: requesting it is an RPC and a
    /// reconnection re-announces the outcome.
    pub(super) report_requested: bool,
    /// What the checksums file was publishing, if this is a CHECK. `None` =
    /// only computing.
    pub(super) published: Option<Published>,
}

/// A finished Task's report, by class.
///
/// Two classes have one — a rename batch and an undo — and both for the same
/// reason: what was left halfway does not fit in a Task's outcome.
pub(super) enum Report {
    /// A rename batch's (#272).
    Batch(Result<norte_proto::methods::FsRenameBatchReportResult, Error>),
    /// A session undo's.
    Undo(Result<norte_proto::methods::PolicyUndoReportResult, Error>),
    /// A pack's (#250). The only one of the three that counts something
    /// about a Task that came out FINE: the archive was written whole and
    /// can still carry names that land somewhere else on another system.
    Packed(Result<norte_proto::methods::ArchivePackReportResult, Error>),
}

/// What a `task.cancel` points at.
///
/// Three cases and not two: "there is none" and "the pointed-at one already
/// finished" read differently, and collapsing them would make cancelling a
/// finished task say there are no tasks while the board shows four.
pub(super) enum Target {
    /// There is none to ask to stop.
    Ninguna,
    /// The pointed-at one is already terminal.
    Finished,
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
    pub(super) pause: Option<crate::backend::Pause>,
    /// How to move it up or down the queue (ADR 0149); `None` if it cannot
    /// be.
    pub(super) cola: Option<crate::backend::Pause>,
    /// Its report has already been requested. Carried by the classes that
    /// HAVE a report — a rename batch and an undo — and it avoids
    /// requesting it twice if the daemon repeats the last progress (a
    /// reconnection re-announces tasks, terminal ones included).
    report_requested: bool,
    /// Which connection epoch it was registered in. A repeated id from a
    /// DIFFERENT epoch is a different task, not the same one.
    epoch: u64,
    /// The LIVE progress, to ask it whether it is still running.
    ///
    /// `vista` is a projection that updates when `Message::Progress` leaves
    /// the mailbox, so deciding what to cancel based on it is deciding based
    /// on a stale snapshot: it used to answer "cancelling…" about something
    /// already finished, and choosing "the last alive one" could skip the
    /// one that is really running. The TUI asks the live state for this
    /// exact reason.
    pub(super) progress: tokio::sync::watch::Receiver<norte_proto::TaskProgress>,
    /// The directories this task leaves OUT OF DATE.
    ///
    /// Noted down when enqueuing and not derived from progress: progress
    /// says which file is currently in flight, not which screens lie once
    /// it finishes. Empty = nothing to refresh (a search, an unrelated task
    /// whose id is all that is known).
    pub(super) affected: Vec<VPath>,
    /// What to retry with if it COLLIDES (#274). `None` for everything that
    /// is not a transfer: a delete or an undo have no other policy to
    /// offer.
    retry: Option<Retry>,
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
pub(super) struct Batch {
    /// How many entries were requested.
    pub(super) total: usize,
    /// How many became a task.
    pub(super) queued: usize,
    /// How many the daemon rejected on enqueuing.
    pub(super) rejected: usize,
    /// The ids of the ones enqueued, to recognize their outcome. An id not
    /// here belongs to something else (a search, an undo, another client).
    pub(super) ids: std::collections::BTreeSet<u64>,
    /// GOOD terminal outcomes of the enqueued ones.
    pub(super) done: usize,
    /// Bad terminal outcomes: failed or cancelled.
    pub(super) failed: usize,
}

impl Batch {
    /// Everything requested is resolved.
    fn closed(&self) -> bool {
        self.queued + self.rejected >= self.total && self.done + self.failed >= self.queued
    }
}

impl State {
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
    pub(super) fn programar_expiration(id: u64, epoch: u64, mailbox: &mpsc::Sender<Message>) {
        let mailbox = mailbox.clone();
        tokio::spawn(async move {
            tokio::time::sleep(TTL_TASK_TERMINAL).await;
            let _ = mailbox.send(Message::TaskExpired(id, epoch)).await;
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
    /// - that it does not owe a refresh (`affected`), which is the
    ///   invariant cap-based eviction already asserts: dropping the row
    ///   would sweep away the re-read of the directory that mutation
    ///   changed.
    pub(super) fn expire_task(
        &mut self,
        id: u64,
        epoch: u64,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let remove = self.tasks.get(&id).is_some_and(|t| {
            t.epoch == epoch && Self::terminal(t.vista.state) && t.affected.is_empty()
        });
        if !remove {
            return Vec::new();
        }
        self.tasks.remove(&id);
        self.annotate_strip(mailbox);
        let mut sends = vec![self.parche(vec![ViewChange::Tasks {
            tasks: self.vistas_de_tasks(),
            cursor: self.board_cursor(),
        }])];
        // And HERE is where the panel that opened on its own closes
        // (ADR 0115). Asking only from `progress` left half the gesture
        // undone: when the last row expires no more progress arrives, so
        // nobody checked again and the panel stayed up for the rest of the
        // session. The terminal did not have the bug because its loop
        // re-evaluates the same condition every round — the divergence
        // ADR 0077 goes after.
        sends.extend(self.processes_automaticos(backend, mailbox));
        sends
    }

    pub(super) fn evict_from_board(&mut self) {
        if self.tasks.len() < MAX_TASKS {
            return;
        }
        let old = self
            .tasks
            .iter()
            .find(|(_, t)| t.vista.state == crate::dto::TaskStateView::Done)
            .or_else(|| {
                self.tasks
                    .iter()
                    .find(|(_, t)| Self::terminal(t.vista.state))
            })
            .map(|(k, _)| *k);
        if let Some(old) = old {
            debug_assert!(
                self.tasks[&old].affected.is_empty(),
                "evicting a task with a pending refresh"
            );
            self.tasks.remove(&old);
        }
    }

    /// Relaunches the transfer that collided, with the chosen policy (#274).
    ///
    /// Repeats the SAME verb: an "overwrite" over a copy that turned into a
    /// move would delete the source nobody asked to touch. And it travels
    /// again with its `Retry`, because the second attempt can collide
    /// again — `Skip` and `RenameAuto` cannot, but `Newer` can — and then it
    /// has to be possible to ask again.
    pub(super) fn launch_retry(
        con: Retry,
        policy: norte_proto::CollisionPolicy,
        a_la_cola: bool,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) {
        // The destination changes; the source also stops being there if it
        // is a move. Both parents are noted down, like in the original
        // transfer.
        let mut affected: Vec<VPath> = con.to.parent().into_iter().collect();
        if con.mover
            && let Some(padre) = con.from.parent()
            && !affected.contains(&padre)
        {
            affected.push(padre);
        }
        let backend = Arc::clone(backend);
        let mailbox = mailbox.clone();
        tokio::spawn(async move {
            let queued = if con.mover {
                backend
                    .move_(con.from.clone(), con.to.clone(), policy, a_la_cola)
                    .await
            } else {
                backend
                    .copy(con.from.clone(), con.to.clone(), policy, a_la_cola)
                    .await
            };
            let message = match queued {
                Ok(task) => Message::TaskNew(Box::new((task, affected, Some(con)))),
                Err(e) => Message::TaskFailed(Box::new(e)),
            };
            let _ = mailbox.send(message).await;
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
    pub(super) fn launch_secret(
        conn: String,
        secret: norte_frontend::secret::TypedSecret,
        slot: u32,
        dir: VPath,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) {
        let backend = Arc::clone(backend);
        let mailbox = mailbox.clone();
        tokio::spawn(async move {
            let res = backend
                .provide_secret(conn, secret.expose().to_owned())
                .await;
            drop(secret);
            let _ = mailbox
                .send(Message::SecretDelivered(Box::new((slot, dir, res))))
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
    pub(super) fn retry_inherited(&self, id: u64) -> Option<Retry> {
        self.tasks
            .get(&id)
            .filter(|t| t.epoch == self.epoch_connection)
            .and_then(|t| t.retry.clone())
    }

    /// Ties the intent of "editing a new one" to the task that creates it
    /// (#290).
    ///
    /// Here and not earlier: the id does not exist until the daemon answers,
    /// and the gesture had already returned. Only to an OWN task and only if
    /// the intent has no id yet — an unrelated one passing through here
    /// cannot adopt this window's intent, which is exactly the bug this
    /// prevents.
    pub(super) fn bind_the_creation(
        &mut self,
        id: u64,
        foreign: bool,
        kind: norte_proto::TaskKind,
    ) {
        if !foreign
            && kind == norte_proto::TaskKind::Create
            && let Some(c) = self.open_on_create.as_mut()
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
    fn inherit_detail(&self, id: u64, vista: &mut crate::dto::TaskView) {
        let Some(anterior) = self
            .tasks
            .get(&id)
            .filter(|t| t.epoch == self.epoch_connection)
        else {
            return;
        };
        if anterior.report_requested
            && Self::terminal(vista.state)
            && anterior.vista.detail.is_some()
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
        affected: Vec<VPath>,
        retry: Option<Retry>,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let id = task.id.get();
        let foreign = task.foreign;
        // Registered in the batch's count (#271), before any eviction: what
        // was enqueued was enqueued even if its row does not end up fitting.
        if let Some(batch) = self.batch.as_mut()
            && !foreign
            && batch.queued + batch.rejected < batch.total
            && batch.ids.insert(id)
        {
            batch.queued += 1;
        }
        self.evict_from_board();
        // And the HARD ceiling on what is retained (#271). This is only
        // reached with the board full of LIVE tasks, and only from the
        // unrelated-tasks channel: own ones never get past
        // `request_transfer`, which refuses the whole batch if it does
        // not fit. An unrelated row that falls loses nothing — it arrives
        // with empty `affected`, i.e. no refresh it owes — except a row
        // this window never promised to show.
        if self.tasks.len() >= MAX_TASKS_RETAINED && !self.tasks.contains_key(&id) {
            tracing::debug!(task = id, "board full: an unrelated task is not retained");
            return Vec::new();
        }
        let mut rx = task.progress.clone();
        let born = rx.borrow().clone();
        self.bind_the_creation(id, foreign, born.kind);
        // #311: the checksums Task already has an id, so the intent noted
        // down when enqueuing it turns into the batch awaiting its report.
        // Only the OWN one: an unrelated task of the same kind is another
        // window's check, and hanging this report off it would give it
        // someone else's digests.
        if !foreign
            && born.kind == norte_proto::TaskKind::Checksum
            && let Some(encolada) = self.checksums_pending.take()
        {
            self.checksums = Some(ChecksumsInFlight {
                task: task.id,
                epoch_connection: self.epoch_connection,
                report_requested: false,
                published: encolada.published,
            });
        }
        let mut vista = Self::vista_de(&born);
        vista.foreign = foreign;
        self.inherit_detail(id, &mut vista);
        // If this task was ALREADY on the board — a reconnection
        // re-announces it through the unrelated-tasks channel — what
        // arrives does not know which directories it touched, so what was
        // noted down is kept: replacing it with an empty list lost the
        // re-listing exactly on the path where the screen is most likely
        // to be stale.
        let affected = if affected.is_empty() {
            self.tasks
                .get(&id)
                .filter(|t| t.epoch == self.epoch_connection)
                .map(|t| t.affected.clone())
                .unwrap_or_default()
        } else {
            affected
        };
        // An ACCEPTED mutation is proof the journal came back: the daemon
        // refuses to mutate without it (hard rule 4), so if this one got
        // in, the "not being recorded" warning stopped being true. There is
        // no recovery notification — the TUI has one because its journal is
        // embedded — and a warning that does not know how to turn off lies
        // about the one thing it describes for the whole session.
        let turns_off_the_notice =
            self.journal_refused && !foreign && Self::mutates(vista.kind.as_str());
        if turns_off_the_notice {
            self.journal_refused = false;
        }
        // Same as with the affected directories: if it was already there,
        // whether its report was requested is preserved. A reconnection
        // re-announcing a finished batch cannot reopen the same report.
        let report_requested = self
            .tasks
            .get(&id)
            .is_some_and(|t| t.report_requested && t.epoch == self.epoch_connection);
        let retry = retry.or_else(|| self.retry_inherited(id));
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
                affected,
                retry,
                report_requested,
                epoch: self.epoch_connection,
                progress: task.progress.clone(),
            },
        );
        let buzon2 = mailbox.clone();
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
                    .send(Message::Progress(Box::new(snapshot)))
                    .await
                    .is_err()
                    || terminal
                {
                    return;
                }
            }
        });
        // It can be born TERMINAL: the daemon completed it before this call
        // returned, and then `rx.changed()` never fires and `progress` is
        // never called even once. Without this, a very fast copy left the
        // destination un-re-listed forever — the race task 5.1 names
        // literally.
        //
        // The light bar also sees it from here: one born finished is the
        // fast copy whose "✓" is the only thing that will say it happened.
        self.annotate_strip(mailbox);
        let mut changes = vec![ViewChange::Tasks {
            tasks: self.vistas_de_tasks(),
            cursor: self.board_cursor(),
        }];
        if turns_off_the_notice {
            changes.push(self.banner_change());
        }
        changes.extend(self.born_terminal(id, &born, backend, mailbox));
        vec![self.parche(changes)]
    }

    /// What has to be handled when a task arrives on the board ALREADY
    /// finished.
    ///
    /// All of this would be done by `progress`, and `progress` is not going
    /// to be called even once: `rx.changed()` does not fire for a channel
    /// born with its final value. Without it, a very fast copy left the
    /// destination un-re-listed forever — the race task 5.1 names literally.
    pub(super) fn born_terminal(
        &mut self,
        id: u64,
        born: &norte_proto::TaskProgress,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> Vec<ViewChange> {
        let Some(state) = self
            .tasks
            .get(&id)
            .map(|t| t.vista.state)
            .filter(|e| Self::terminal(*e))
        else {
            return Vec::new();
        };
        // Its time on the board is counted from here, for the same reason.
        Self::programar_expiration(id, self.epoch_connection, mailbox);
        let mut changes = Vec::new();
        if self.records_batch_outcome(id, state) {
            changes.push(self.banner_change());
        }
        changes.extend(self.refresh_affected(id, backend, mailbox));
        // And its report, for the same reason as the re-listing: it is the
        // only signal that the directory was left halfway, and a very fast
        // batch used to be left without it right when the Task's outcome
        // most looks like everything went fine.
        self.request_batch_report(born, backend, mailbox);
        // #311: and the checksums' one, for the same reason. A batch of
        // three small files is born terminal almost always, so without this
        // the fast path — the most used one — showed nothing.
        self.request_checksums_report(born, backend, mailbox);
        changes
    }

    /// Applies a progress snapshot to the board.
    pub(super) fn progress(
        &mut self,
        p: &norte_proto::TaskProgress,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Some(viva) = self.tasks.get_mut(&p.task_id.get()) else {
            return Vec::new();
        };
        let foreign = viva.vista.foreign;
        let era_terminal = Self::terminal(viva.vista.state);
        let epoch = viva.epoch;
        // The rate BEFORE projecting: it is measured between this snapshot
        // and the previous one (spec 2026-09-15, ADR 0115). The wire does
        // not carry it, so whoever is watching estimates it — and the host
        // writes it, so the window and the terminal say the same speed with
        // the same units.
        viva.rate.observe(p, super::now_ms());
        let (pace, remains) = (
            norte_frontend::tasks::human_rate(viva.rate.bps()),
            norte_frontend::tasks::human_eta(viva.rate.eta_secs(p)),
        );
        viva.vista = Self::vista_de(p);
        viva.vista.rate = pace;
        viva.vista.eta = remains;
        // Whose task it is is not said by progress: it is said by where it
        // came from.
        viva.vista.foreign = foreign;
        // Read from the view that was just projected: rebuilding it only to
        // look at its state costs two `String`s and a `path_display` on
        // every progress tick of every task in the batch.
        let ended = Self::terminal(viva.vista.state);
        let state_final = viva.vista.state;
        // It just finished: its time on the board starts. Only on the
        // TRANSITION — the daemon repeats the last progress on reconnecting,
        // and rearming the clock on every repeat would leave the row there
        // forever, which is exactly the opposite of what is asked for.
        if ended && !era_terminal {
            Self::programar_expiration(p.task_id.get(), epoch, mailbox);
        }
        // The panel that opens and closes on its own (`[ui] processes_panel
        // = "auto"`, ADR 0115): a panel taking up space to say "nothing
        // running" does not earn it, and hunting for the button right when a
        // copy starts does not either. It only closes what it itself opened,
        // and goes through BOTH HALVES of the gesture, never through the
        // toggle.
        self.annotate_strip(mailbox);
        let del_panel = self.processes_automaticos(backend, mailbox);
        let mut changes = self.line_changes();
        changes.push(ViewChange::Tasks {
            tasks: self.vistas_de_tasks(),
            cursor: self.board_cursor(),
        });
        // The outcome enters the batch's count (#271). Something only
        // travels when the batch is RESOLVED: two hundred "one more"
        // phrases say nothing the row does not already say.
        if ended && self.records_batch_outcome(p.task_id.get(), state_final) {
            changes.push(self.banner_change());
        }
        // An undo that FINISHES releases its session: while it runs, the
        // row says so and `u` over it is refused — two undos of the same
        // session walk the same entry list — and that cannot stay stuck
        // forever.
        if ended && let Some(session) = self.agency.undos.remove(&p.task_id.get()) {
            self.agency.sessions.undone(&session);
            if self.agency.panel {
                changes.push(ViewChange::Agents {
                    agents: self.vista_agents(),
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
        let outcome = norte_frontend::search_status::outcome_of(&p.state, |e| {
            clamp_display(norte_frontend::error::error_category_in(lang, e))
        });
        if let Some(outcome) = outcome
            && let Some(b) = self.search.as_mut()
            && b.task == p.task_id
        {
            b.outcome = outcome;
            changes.push(ViewChange::Search {
                search: self.vista_search(),
            });
        }
        // A mutation that finished leaves screens out of date: the new entry
        // is on disk and not in the listing. Only with a REAL outcome —
        // `Running` is not one — and only once.
        if ended {
            changes.extend(self.refresh_affected(p.task_id.get(), backend, mailbox));
            self.request_batch_report(p, backend, mailbox);
            // And the timeline (#359): what just happened — or was just
            // undone — has to show up in a panel that stays open.
            self.reload_lines();
            changes.extend(self.close_comparison(p));
            changes.extend(self.close_sync(p));
            self.request_sync_report(p, backend, mailbox);
            // #311: and the checksums' one, which is where the digests
            // travel.
            self.request_checksums_report(p, backend, mailbox);
            changes.extend(self.report_the_count(p));
            changes.extend(self.offer_retry(p));
            self.open_the_created(p, backend, mailbox);
            self.notify_of_outcome(p);
        }
        // The automatic panel travels BEHIND the patch and separately:
        // opening or closing a slot rebuilds the whole layout, so those are
        // already-built envelopes — with their own snapshot — and not one
        // more change in this list.
        let mut outside = vec![self.parche(changes)];
        outside.extend(del_panel);
        outside
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
    pub(super) fn notify_of_approval(
        &mut self,
        req: &norte_proto::methods::PolicyApprovalRequired,
    ) {
        if self.focused {
            return;
        }
        let (who, _) =
            norte_frontend::display_name(req.session.as_deref().unwrap_or_default().as_bytes());
        let (op, _) = norte_frontend::display_name(req.op.as_bytes());
        let title = clamp_display(norte_i18n::t_in(self.lang, "notify-approval-title"));
        let body = clamp_display(norte_i18n::ta_in(
            self.lang,
            "notify-approval-body",
            &[("op", &op), ("who", &who)],
        ));
        self.native(crate::dto::NativeEffect::Notify { title, body });
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
    pub(super) fn notify_of_outcome(&mut self, p: &norte_proto::TaskProgress) {
        if self.focused {
            return;
        }
        let (key, count) = match &p.state {
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
        let detail = p.current.as_ref().map_or_else(
            || count.to_string(),
            |path| {
                let bytes = path
                    .file_name()
                    .map_or_else(Vec::new, |s| s.as_bytes().to_vec());
                let (text, _) = norte_frontend::display_name(&bytes);
                clamp_display(text)
            },
        );
        let title = clamp_display(norte_i18n::t_in(self.lang, key));
        let body = clamp_display(norte_i18n::ta_in(
            self.lang,
            "notify-task-body",
            &[("what", &detail), ("kind", task_class(p.kind))],
        ));
        self.native(crate::dto::NativeEffect::Notify { title, body });
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
    pub(super) fn offer_retry(&mut self, p: &norte_proto::TaskProgress) -> Vec<ViewChange> {
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
            .and_then(|t| t.retry.clone())
        else {
            return Vec::new();
        };
        // The destination, in its own field and masked: it is a file name
        // from the other end, and it is WHAT the reader has to look at to
        // decide whether to overwrite. Through the funnel, which masks the
        // RAW bytes: over `display_lossy()` the U+FFFDs were already in
        // place and the verdict came out "faithful" — no badge, on the one
        // screen where overwriting is approved.
        let dest = Self::line_with_encoding(&con.to, con.enc);
        let modal = ModalId(self.next_modal);
        self.next_modal += 1;
        let vista = DialogView {
            id: modal,
            title_key: "modal-collision-title".to_owned(),
            destination: Some(dest),
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
        self.dialogs.push(Dialog {
            id: modal,
            vista,
            typed: Typed::Text(String::new()),
            // It opened ON ITS OWN — it arrives when the task finishes, on
            // top of whatever the reader was doing — so the first answer
            // only acknowledges it. It is the same rule as an agent
            // approval, and here it matters just as much: the first option
            // is "overwrite".
            recognized: false,
            on_confirm: Some(Pending::Retry { con }),
        });
        vec![ViewChange::Dialogs {
            dialogs: self.dialog_views(),
        }]
    }

    pub(super) fn report_the_count(&mut self, p: &norte_proto::TaskProgress) -> Vec<ViewChange> {
        if p.kind != norte_proto::TaskKind::DirSize
            || !matches!(p.state, norte_proto::TaskState::Completed)
        {
            return Vec::new();
        }
        let size = norte_frontend::human_bytes(p.bytes_done);
        let how_many = p.entries_done.to_string();
        let skipped = p.unreadable.unwrap_or(0);
        let message = if skipped > 0 {
            norte_i18n::ta_in(
                self.lang,
                "msg-dir-size-partial",
                &[
                    ("size", &size),
                    ("count", &how_many),
                    ("skipped", &skipped.to_string()),
                ],
            )
        } else {
            norte_i18n::ta_in(
                self.lang,
                "msg-dir-size",
                &[("size", &size), ("count", &how_many)],
            )
        };
        self.status.message = Some(clamp_display(message));
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
    pub(super) fn request_batch_report(
        &mut self,
        p: &norte_proto::TaskProgress,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) {
        // Three classes have a report, and all three for the same reason:
        // what has to be said does not fit in a Task's outcome. The first
        // two count what was left halfway; the third counts something about
        // a pack that came out FINE (#250).
        #[derive(Clone, Copy)]
        enum Which {
            Batch,
            Undo,
            Packed,
        }
        let which = match p.kind {
            norte_proto::TaskKind::RenameBatch => Which::Batch,
            norte_proto::TaskKind::Undo => Which::Undo,
            // **Only a pack that COMPLETED.** The other two reports talk
            // about what was left halfway, so a `Failed` or a `Cancelled` is
            // exactly when they are most needed; this one talks about an
            // archive, and there is no such thing as a cancelled pack —
            // cancellation leaves the destination clean. The report exists
            // regardless (it is computed before writing), and painting it
            // would say "packed, but…" about something nobody packed.
            norte_proto::TaskKind::Pack if matches!(p.state, norte_proto::TaskState::Completed) => {
                Which::Packed
            }
            _ => return,
        };
        let id = p.task_id;
        match self.tasks.get_mut(&id.get()) {
            Some(t) if !t.report_requested => t.report_requested = true,
            _ => return,
        }
        let backend = Arc::clone(backend);
        let mailbox = mailbox.clone();
        let epoch = self.epoch_connection;
        tokio::spawn(async move {
            let which = match which {
                Which::Batch => Report::Batch(backend.rename_batch_report(id).await),
                Which::Undo => Report::Undo(backend.undo_report(id).await),
                Which::Packed => Report::Packed(backend.archive_pack_report(id).await),
            };
            let _ = mailbox
                .send(Message::Report(Box::new((epoch, id.get(), which))))
                .await;
        });
    }

    /// Enqueuing a mutation failed: it is reported, and if it was because of
    /// the journal it stays reported.
    ///
    /// `error_key` returns a Fluent KEY, and `StatusView.message`'s contract
    /// says "already translated by the host": untranslated, the user read
    /// `err-not-found` in the status bar.
    pub(super) fn task_failed(&mut self, e: &Error) -> Vec<BridgeEnvelope<UiUpdate>> {
        let key = norte_frontend::error::error_key(e);
        self.status.message = Some(clamp_display(norte_i18n::t_in(self.lang, key)));
        // A journal rejection is not a mutation gone wrong: it is that THIS
        // SESSION does not mutate until the file is fixed (hard rule 4).
        // That lasts longer than one message.
        self.journal_refused |= matches!(e, Error::JournalUnavailable);
        // A creation that never even got enqueued releases its intent: with
        // no task there is no outcome to consume it, and staying stuck
        // would make the NEXT `edit-new` open this one's file, which does
        // not exist.
        if self
            .open_on_create
            .as_ref()
            .is_some_and(|c| c.task.is_none())
        {
            self.open_on_create = None;
        }
        let change = self.banner_change();
        let parche = self.parche(vec![change]);
        let notice = self.over(UiUpdate::Notice(UiNotice::Message {
            key: key.to_owned(),
            detail: None,
        }));
        vec![parche, notice]
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
    pub(super) fn batch_rejection(&mut self, e: &Error) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Some(batch) = self.batch.as_mut() else {
            return self.task_failed(e);
        };
        batch.rejected += 1;
        // A journal rejection still means the same thing even coming from a
        // batch: this session does NOT mutate until the file is fixed (hard
        // rule 4), and that lasts longer than any summary — and it is the
        // only thing about a lone rejection that DOES travel before the end.
        let before = self.journal_refused;
        self.journal_refused |= matches!(e, Error::JournalUnavailable);
        let banner_new = self.journal_refused != before;
        if !self.batch_summary_if_closed() && !banner_new {
            // A patch per rejection is the storm this exists to silence:
            // while the batch stays open, nothing travels.
            return Vec::new();
        }
        let change = self.banner_change();
        vec![self.parche(vec![change])]
    }

    /// Notes down ONE batch task's outcome (#271). `true` if the batch was
    /// resolved by it and `status.message` already carries the summary.
    ///
    /// The id is removed from the count when noted down: terminal progress
    /// can arrive more than once — a re-announcement after reconnecting
    /// brings the final state again — and the second one is not a second
    /// outcome.
    pub(super) fn records_batch_outcome(&mut self, id: u64, state: TaskStateView) -> bool {
        let Some(batch) = self.batch.as_mut() else {
            return false;
        };
        if !batch.ids.remove(&id) {
            return false;
        }
        if state == TaskStateView::Done {
            batch.done += 1;
        } else {
            batch.failed += 1;
        }
        self.batch_summary_if_closed()
    }

    /// If the batch is resolved, puts the summary in the status bar and
    /// closes it.
    pub(super) fn batch_summary_if_closed(&mut self) -> bool {
        let Some(batch) = self.batch.as_ref() else {
            return false;
        };
        if !batch.closed() {
            return false;
        }
        // Rejected on enqueuing and terminated badly are the same outcome
        // for whoever is watching: it did not arrive. Telling them apart
        // would need two more numbers in a phrase that has to fit in the
        // status bar.
        let total = batch.total.to_string();
        let bien = batch.done.to_string();
        let bad = (batch.rejected + batch.failed).to_string();
        self.batch = None;
        self.status.message = Some(clamp_display(norte_i18n::ta_in(
            self.lang,
            "msg-batch-summary",
            &[("total", &total), ("ok", &bien), ("fail", &bad)],
        )));
        true
    }

    /// A report arrived: onto the board, and up front if it left something
    /// halfway.
    pub(super) fn report(
        &mut self,
        epoch: u64,
        task_id: u64,
        which: &Report,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // A report that came out BEFORE the reconnection talks about a task
        // from a different daemon, and the id can be reused: hanging it off
        // the row that carries that number today would open a "was left
        // halfway" about the wrong directory.
        if epoch != self.epoch_connection {
            return Vec::new();
        }
        match which {
            Report::Batch(r) => self.batch_report(task_id, r),
            Report::Undo(r) => self.undo_report(task_id, r),
            Report::Packed(r) => self.packaging_report(r),
        }
    }

    /// A pack's report arrived (#250).
    ///
    /// **A clean report says nothing, and that is the design**: the normal
    /// answer is that the archive travels whole, and warning about it would
    /// teach people to skip the warning that does matter. An error is not
    /// painted either: against an N-1 daemon the method does not exist, and
    /// "could not ask" is not a finding about the archive.
    pub(super) fn packaging_report(
        &mut self,
        res: &Result<norte_proto::methods::ArchivePackReportResult, Error>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Ok(report) = res else {
            return Vec::new();
        };
        if report.risky.is_empty() {
            return Vec::new();
        }
        // Trimmed says "at least": the list is cut off at
        // `ARCHIVE_PACK_REPORT_MAX`, and painting the cap as if it were the
        // total is the lie `truncated` exists to prevent.
        let key = if report.truncated {
            "msg-pack-warnings-partial"
        } else {
            "msg-pack-warnings"
        };
        let text = norte_i18n::ta_in(
            self.lang,
            key,
            &[("risky", &report.risky.len().to_string())],
        );
        self.status.message = Some(clamp_display(text));
        let change = ViewChange::Status(self.status.clone());
        vec![self.parche(vec![change])]
    }

    /// An undo's report arrived: onto the board, and up front if something
    /// did not come back.
    ///
    /// Same shape as [`Self::batch_report`] because it is the same
    /// question — what was left undone — asked about a different Task
    /// class.
    pub(super) fn undo_report(
        &mut self,
        task_id: u64,
        result: &Result<norte_proto::methods::PolicyUndoReportResult, Error>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // A row that is no longer there does NOT drop the report: the board
        // is capped and the task could have fallen off while the report was
        // in flight, but what got lost that way was exactly "was left
        // halfway", which never folds into "went fine". With no row, the
        // detail is skipped and whatever needs to be said is still shown.
        let task_failed = self.tasks.get(&task_id).is_some_and(|viva| {
            matches!(
                viva.vista.state,
                crate::dto::TaskStateView::Failed | crate::dto::TaskStateView::Cancelled
            )
        });
        let (detail, body) = match result {
            Ok(r) => (
                norte_i18n::ta_in(self.lang, "task-undo-done", &[("n", &r.undone.to_string())]),
                self.undo_body(r),
            ),
            Err(e) => {
                let key = if matches!(e, Error::Unsupported) {
                    "modal-undo-unsupported"
                } else {
                    "modal-undo-report-failed"
                };
                (
                    norte_i18n::t_in(self.lang, "task-undo-unverified"),
                    vec![crate::dto::DialogLine {
                        text: clamp_display(norte_i18n::t_in(self.lang, key)),
                        hostile: false,
                    }],
                )
            }
        };
        if let Some(t) = self.tasks.get_mut(&task_id) {
            t.vista.detail = Some(clamp_display(detail));
            t.vista.detail_hostile = false;
        }
        let mut changes = vec![ViewChange::Tasks {
            tasks: self.vistas_de_tasks(),
            cursor: self.board_cursor(),
        }];
        let hay_that_say_it = match result {
            Ok(r) => !Self::undo_clean(r),
            Err(_) => task_failed,
        };
        let mut fallen = Vec::new();
        if hay_that_say_it {
            let (change, fell) = self.open_report("modal-undo-report-title".to_owned(), body);
            changes.push(change);
            fallen = fell;
        }
        let mut outputs = vec![self.parche(changes)];
        outputs.extend(fallen);
        outputs
    }

    /// `true` if the undo returned EVERYTHING it should have.
    ///
    /// What was skipped counts as not-clean: an irreversible entry or a
    /// creation that stays because the destination has no trash are things
    /// that did NOT come back, and a report that stayed quiet about them
    /// would say the tree is as it was.
    pub(super) fn undo_clean(r: &norte_proto::methods::PolicyUndoReportResult) -> bool {
        norte_frontend::undo_report_is_clean(r)
    }

    /// An undo report's body: what came back and what did not. The lines are
    /// decided by `norte_frontend::undo_report_lines`, which is also what the
    /// terminal shows; here they are only painted.
    pub(super) fn undo_body(
        &self,
        r: &norte_proto::methods::PolicyUndoReportResult,
    ) -> Vec<crate::dto::DialogLine> {
        Self::paint_report(norte_frontend::undo_report_lines(r, self.lang))
    }

    /// A shared report's lines, painted: the phrases clamped, and each path
    /// as a path line — masked and flagged.
    fn paint_report(lines: Vec<norte_frontend::ReportLine>) -> Vec<crate::dto::DialogLine> {
        lines
            .into_iter()
            .map(|line| match line {
                norte_frontend::ReportLine::Phrase(text) => crate::dto::DialogLine {
                    text: clamp_display(text),
                    hostile: false,
                },
                norte_frontend::ReportLine::Path(p) => Self::path_line(&p),
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
    pub(super) fn batch_report(
        &mut self,
        task_id: u64,
        result: &Result<norte_proto::methods::FsRenameBatchReportResult, Error>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // See [`Self::undo_report`]: with no row, the report is shown
        // just the same. What is not done is inventing a row to hang it off
        // of.
        let task_failed = self.tasks.get(&task_id).is_some_and(|viva| {
            matches!(
                viva.vista.state,
                crate::dto::TaskStateView::Failed | crate::dto::TaskStateView::Cancelled
            )
        });
        let (detail, body) = match result {
            Ok(r) => (Self::batch_detail(self.lang, r), self.batch_body(r)),
            Err(e) => {
                let key = if matches!(e, Error::Unsupported) {
                    "modal-batch-unsupported"
                } else {
                    "modal-batch-report-failed"
                };
                (
                    norte_i18n::t_in(self.lang, "task-batch-unverified"),
                    vec![crate::dto::DialogLine {
                        text: clamp_display(norte_i18n::t_in(self.lang, key)),
                        hostile: false,
                    }],
                )
            }
        };
        // The row's detail is "what is currently in flight" while it runs;
        // once finished, what matters is what it ended up as. There is no
        // more progress behind it to overwrite it: the state is terminal.
        if let Some(t) = self.tasks.get_mut(&task_id) {
            t.vista.detail = Some(clamp_display(detail));
            t.vista.detail_hostile = false;
        }
        let mut changes = vec![ViewChange::Tasks {
            tasks: self.vistas_de_tasks(),
            cursor: self.board_cursor(),
        }];
        // It opens by what the REPORT says, not by how the Task finished: a
        // `Completed` batch with a stuck step is exactly the case the
        // Task's outcome does not count.
        let hay_that_say_it = match result {
            Ok(r) => !Self::batch_clean(r),
            // A report that could not be requested about a batch that also
            // failed leaves the directory with no explanation: that is said
            // up front. If the batch finished fine, the board's row is
            // enough.
            Err(_) => task_failed,
        };
        let mut fallen = Vec::new();
        if hay_that_say_it {
            let (change, fell) = self.open_report("modal-batch-report-title".to_owned(), body);
            changes.push(change);
            fallen = fell;
        }
        let mut outputs = vec![self.parche(changes)];
        outputs.extend(fallen);
        outputs
    }

    /// `true` if the batch left nothing to look for or to finish off.
    pub(super) fn batch_clean(r: &norte_proto::methods::FsRenameBatchReportResult) -> bool {
        norte_frontend::batch_report_is_clean(r)
    }

    /// The one-line summary that stays in the board's row.
    pub(super) fn batch_detail(
        lang: norte_i18n::Lang,
        r: &norte_proto::methods::FsRenameBatchReportResult,
    ) -> String {
        if Self::batch_clean(r) {
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
    pub(super) fn batch_body(
        &self,
        r: &norte_proto::methods::FsRenameBatchReportResult,
    ) -> Vec<crate::dto::DialogLine> {
        Self::paint_report(norte_frontend::batch_report_lines(r, self.lang))
    }

    /// Stacks a dialog, with a cap.
    ///
    /// The cap exists because the stack is fed by the WIRE since task 5.3
    /// (approvals and reports, unrelated tasks' too). The oldest UNACKNOWLEDGED
    /// one falls — the one nobody has gotten around to looking at — and never
    /// the top one, which is the one being answered; if all are acknowledged,
    /// the oldest one. That one fell IS SAID: a question disappearing in
    /// silence is worse than a long stack.
    pub(super) fn stack_dialog(&mut self, dialog: Dialog) -> Vec<BridgeEnvelope<UiUpdate>> {
        let mut outside = Vec::new();
        if self.dialogs.len() >= MAX_DIALOGS {
            // A REPORT is sacrificed before a decision: the report also
            // lives on the board's row, and an approval that disappears
            // leaves an agent waiting. If only decisions are left, the
            // oldest one falls — the daemon will eventually apply its TTL to
            // that one, which is a denial.
            let victim = self
                .dialogs
                .iter()
                .position(|d| d.on_confirm.is_none())
                .or_else(|| self.dialogs.iter().position(|d| !d.recognized))
                .unwrap_or(0);
            self.dialogs.remove(victim);
            outside.extend(self.say("msg-dialog-dropped"));
        }
        self.dialogs.push(dialog);
        outside
    }

    /// Opens a report's dialog. It only informs: it has nothing to execute,
    /// and its only answer closes it.
    pub(super) fn open_report(
        &mut self,
        title_key: String,
        body: Vec<crate::dto::DialogLine>,
    ) -> (ViewChange, Vec<BridgeEnvelope<UiUpdate>>) {
        let id = ModalId(self.next_modal);
        self.next_modal += 1;
        let vista = DialogView {
            id,
            title_key,
            destination: None,
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body,
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
        let fallen = self.stack_dialog(Dialog {
            id,
            vista,
            typed: Typed::Text(String::new()),
            // It opens ON ITS OWN, when the daemon answers.
            recognized: false,
            on_confirm: None,
        });
        (
            ViewChange::Dialogs {
                dialogs: self.dialog_views(),
            },
            fallen,
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
    pub(super) fn cancel_by_command(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match self.task_to_cancel() {
            Target::Ninguna => (self.applied(), self.say("msg-no-tasks")),
            Target::Finished => (self.applied(), self.say("msg-task-finished")),
            Target::Viva(id) => {
                // A window with no effects does not abort ANOTHER client's
                // task: cancelling a copy leaves the destination clean or a
                // `.norte-partial`, i.e. it touches disk. Its own ones,
                // though, since launching them already needed the toggle.
                if self.effects == crate::commands::Effects::SoloRead
                    && self.tasks.get(&id).is_some_and(|t| t.vista.foreign)
                {
                    return Self::no_mutates();
                }
                let (ack, mut outside) = self.cancel(id);
                outside.extend(self.say("msg-cancelling"));
                (ack, outside)
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
    pub(super) fn pause_by_command(
        &mut self,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let id = match self.task_to_cancel() {
            Target::Ninguna => return (self.applied(), self.say("msg-no-tasks")),
            Target::Finished => return (self.applied(), self.say("msg-task-finished")),
            Target::Viva(id) => id,
        };
        let Some(viva) = self.tasks.get(&id) else {
            return (self.applied(), self.say("msg-no-tasks"));
        };
        if self.effects == crate::commands::Effects::SoloRead && viva.vista.foreign {
            return Self::no_mutates();
        }
        let (class, state) = {
            let p = viva.progress.borrow();
            (p.kind, p.state.clone())
        };
        if !norte_frontend::tasks::pausable(class) {
            return (self.applied(), self.say("msg-pause-not-this"));
        }
        let Some(command) = viva.pause.clone() else {
            return (self.applied(), self.say("msg-pause-unsupported"));
        };
        let pause = state != norte_proto::TaskState::Paused;
        let mailbox = mailbox.clone();
        tokio::spawn(async move {
            if let Err(norte_proto::Error::Unsupported) = command(pause).await {
                let _ = mailbox.send(Message::Say("msg-pause-unsupported")).await;
            }
        });
        let notice = if pause { "msg-pausing" } else { "msg-resuming" };
        (self.applied(), self.say(notice))
    }

    /// Turns the serial queue on or off for whatever launches from now on
    /// (ADR 0149). What is already queued stays in its queue.
    pub(super) fn toggle_cola(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.enqueue = !self.enqueue;
        let notice = if self.enqueue {
            "msg-queue-on"
        } else {
            "msg-queue-off"
        };
        (self.applied(), self.say(notice))
    }

    /// Moves the board's pointed-at task up or down the queue (ADR 0149).
    ///
    /// The request goes outside the actor, like the pause: what really
    /// happened shows in the order they come out in, and a daemon that does
    /// not know the queue says so.
    pub(super) fn move_in_queue_by_command(
        &mut self,
        up: bool,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let id = match self.task_to_cancel() {
            Target::Ninguna => return (self.applied(), self.say("msg-no-tasks")),
            Target::Finished => return (self.applied(), self.say("msg-task-finished")),
            Target::Viva(id) => id,
        };
        let Some(command) = self.tasks.get(&id).and_then(|t| t.cola.clone()) else {
            return (self.applied(), self.say("msg-queued-not-moved"));
        };
        let mailbox = mailbox.clone();
        tokio::spawn(async move {
            if command(up).await.is_err() {
                let _ = mailbox.send(Message::Say("msg-queued-not-moved")).await;
            }
        });
        (self.applied(), self.say("msg-queued-moved"))
    }

    /// What is arriving at `slot`, 0–100 (ADR 0148): the percentage of the
    /// live WORK task whose affected directories include its own.
    ///
    /// With several, the LEAST advanced rules, as in the row's rule
    /// (`processes::progress_for`): what that pane needs to be at ease is
    /// whatever the most behind one still needs.
    pub(super) fn slot_progress(&self, slot: &Slot) -> Option<u8> {
        let dir = slot.pane.dir();
        self.tasks
            .values()
            .filter(|t| t.epoch == self.epoch_connection && !Self::terminal(t.vista.state))
            .filter(|t| t.affected.iter().any(|d| d == dir))
            .filter(|t| norte_frontend::tasks::counts_as_work(t.progress.borrow().kind))
            .filter_map(|t| norte_frontend::tasks::progress_pct(&t.progress.borrow()))
            .min()
    }

    /// The fine-grained line changes that need sending, compared with the
    /// last one that crossed: two pixels are not worth a listing.
    pub(super) fn line_changes(&mut self) -> Vec<ViewChange> {
        let now: Vec<(u32, Option<u8>)> = self
            .slots
            .iter()
            .map(|(id, h)| (*id, self.slot_progress(h)))
            .collect();
        let mut changes = Vec::new();
        for (id, pct) in now {
            if self.ultima_line.get(&id).copied() != Some(pct) {
                self.ultima_line.insert(id, pct);
                changes.push(ViewChange::SlotProgress {
                    slot_id: id,
                    progress: pct,
                });
            }
        }
        changes
    }

    /// Repeats the most recent failed transfer, with its SAME options
    /// (ADR 0148).
    ///
    /// The context was already saved for the collision dialog (#274); what
    /// was missing was being able to use it when what failed was not a
    /// collision — a network that dropped, a destination that filled up —
    /// and the operation had to be redone by hand.
    pub(super) fn retry_by_command(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.effects == crate::commands::Effects::SoloRead {
            return Self::no_mutates();
        }
        let visible: Vec<_> = self.tasks_visible().collect();
        let con = visible.into_iter().rev().find_map(|(_, t)| {
            matches!(
                t.vista.state,
                crate::dto::TaskStateView::Failed | crate::dto::TaskStateView::Cancelled
            )
            .then(|| t.retry.clone())
            .flatten()
        });
        let Some(con) = con else {
            return (self.applied(), self.say("msg-no-retry"));
        };
        // With the policy requested the first time: repeating is not
        // deciding something else, and a collision asks again just like
        // before.
        Self::launch_retry(
            con,
            norte_proto::CollisionPolicy::Fail,
            self.enqueue,
            backend,
            mailbox,
        );
        (self.applied(), self.say("msg-retrying"))
    }

    /// Moves the board's chosen row.
    ///
    /// Without needing the processes panel's focus: the board is painted
    /// even when that slot does not exist — tasks go out in the envelope —
    /// and a command that only worked with one specific slot open would be a
    /// key that depends on the layout.
    pub(super) fn move_on_board(
        &mut self,
        back: bool,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let ids = self.board_ids();
        if ids.is_empty() {
            return (self.applied(), self.say("msg-no-tasks"));
        }
        if back {
            self.cursor_processes.up(&ids);
        } else {
            self.cursor_processes.down(&ids);
        }
        // SNAPSHOT and not a patch. Since bridge 57 the cursor DOES have
        // somewhere to travel (`ViewChange::Tasks`), so this is no longer
        // "there is no contract": it is that a key that only moves the
        // choice does not need to resend the whole board, and the snapshot
        // is what this path has been doing without complaint. Changing it
        // is an optimization, not a fix.
        let snap = self.snapshot();
        (
            self.applied(),
            vec![self.over(UiUpdate::Snapshot(Box::new(snap)))],
        )
    }

    /// Removes the board's chosen row, if it has ALREADY finished.
    ///
    /// A live one is not discarded: stopping it is `task.cancel`, and
    /// removing from view something still writing to disk is losing sight
    /// of exactly what needs watching.
    pub(super) fn discard_task(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let ids = self.board_ids();
        if ids.is_empty() {
            return (self.applied(), self.say("msg-no-tasks"));
        }
        let i = self.cursor_processes.row_or_zero(&ids);
        let Some((&id, viva)) = self.tasks_visible().nth(i) else {
            return (self.applied(), self.say("msg-no-tasks"));
        };
        if !Self::terminal(viva.vista.state) {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-task-running".to_owned(),
                },
                self.say("host-task-running"),
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
        let mut outside = vec![self.parche(vec![ViewChange::Tasks {
            tasks: self.vistas_de_tasks(),
            cursor: self.board_cursor(),
        }])];
        let snap = self.snapshot();
        outside.push(self.over(UiUpdate::Snapshot(Box::new(snap))));
        (self.applied(), outside)
    }

    /// Releases the "undoing" of a session whose task is discarded.
    ///
    /// Discarding a finished undo's row is the same as seeing it finish: if
    /// it were not released here, that session would stay marked "undoing"
    /// forever and `u` over it would be refused for no reason.
    pub(super) fn undos_sin_task(&mut self, task_id: u64) {
        if let Some(session) = self.agency.undos.remove(&task_id) {
            self.agency.sessions.undone(&session);
        }
    }

    /// Which task is due to stop.
    pub(super) fn task_to_cancel(&self) -> Target {
        if self.processes_have_focus() {
            // The cursor's, whatever its state: a human chose it by looking
            // at it. If it already finished, it IS SAID, instead of jumping
            // to another one — cancelling a task that is not the pointed-at
            // one is worse than not cancelling anything.
            let Some((id, viva)) = self
                .tasks_visible()
                .nth(self.cursor_processes.row_or_zero(&self.board_ids()))
            else {
                return Target::Ninguna;
            };
            return if Self::follows_viva(viva) {
                Target::Viva(*id)
            } else {
                Target::Finished
            };
        }
        // The board goes by id, and the daemon hands them out increasing:
        // the last alive one is the one with the highest id.
        self.tasks
            .iter()
            .rev()
            .find(|(_, t)| Self::follows_viva(t))
            .map_or(Target::Ninguna, |(id, _)| Target::Viva(*id))
    }

    /// `true` if this task class WRITES.
    ///
    /// By the catalog's key and not by `TaskKind`, which is not exhaustive:
    /// a class from a newer daemon falls into `unknown` and does NOT count
    /// as a mutation, which is the safe side — turning off the journal
    /// warning for something this host does not know what it does would be
    /// turning it off just in case.
    pub(super) fn mutates(class: &str) -> bool {
        matches!(
            class,
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
    /// marking the outcome and `Message::Progress` leaving the mailbox, the
    /// view says it is still running. Over that snapshot, "cancelling…" used
    /// to be answered about something already finished, and the one chosen
    /// as "last alive" was one that no longer was, leaving the one that
    /// really remained still running.
    pub(super) fn follows_viva(t: &TaskViva) -> bool {
        !t.progress.borrow().state.is_terminal()
    }

    /// `true` if focus is on the processes panel.
    pub(super) fn processes_have_focus(&self) -> bool {
        self.roles
            .get(RoleId::Active)
            .and_then(|s| kind_de(&self.tree, s))
            .is_some_and(|k| k.as_str() == "processes")
    }

    /// States a phrase: in the status bar AND as a notification.
    ///
    /// Both things, and through the same call, which is what a failed task
    /// already does: the status bar is where it is read on looking, and the
    /// notification is what the renderer can announce to a screen reader.
    pub(super) fn say_with(
        &mut self,
        key: &str,
        args: &[(&str, &str)],
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        self.status.message = Some(clamp_display(norte_i18n::ta_in(self.lang, key, args)));
        let parche = self.parche(vec![ViewChange::Status(self.status.clone())]);
        let notice = self.over(UiUpdate::Notice(UiNotice::Message {
            key: key.to_owned(),
            detail: None,
        }));
        vec![parche, notice]
    }

    /// Like [`Self::say_with`], with no arguments.
    pub(super) fn say(&mut self, key: &str) -> Vec<BridgeEnvelope<UiUpdate>> {
        self.status.message = Some(clamp_display(norte_i18n::t_in(self.lang, key)));
        let parche = self.parche(vec![ViewChange::Status(self.status.clone())]);
        let notice = self.over(UiUpdate::Notice(UiNotice::Message {
            key: key.to_owned(),
            detail: None,
        }));
        vec![parche, notice]
    }

    /// Requests a task's cancellation. Idempotent by contract: requesting it
    /// twice is not an error and changes nothing.
    pub(super) fn cancel(&mut self, task_id: u64) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(viva) = self.tasks.get(&task_id) else {
            return (Self::stale(StaleAction::Generation), Vec::new());
        };
        (viva.cancel)();
        (self.applied(), Vec::new())
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
        self.tasks_visible().map(|(_, t)| t.vista.clone()).collect()
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
    pub(super) fn tasks_visible(&self) -> impl Iterator<Item = (&u64, &TaskViva)> {
        let leftover = self.tasks.len().saturating_sub(MAX_TASKS);
        self.tasks.iter().skip(leftover)
    }

    /// Opens or closes the processes panel on its own, and says what
    /// changed.
    ///
    /// Both HALVES of the gesture, never the toggle: reusing
    /// `toggle_slot` would close the panel when the second task starts
    /// and reopen the one the reader just closed (ADR 0115).
    pub(super) fn processes_automaticos(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
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
        let opens = self.strip.wants_panel(self.clock_strip());
        let has_work = self.has_work();
        let open = self.slot_of_kind("processes").is_some();
        if opens && !open {
            self.processes_auto = true;
            return self.open_slot_of_kind("processes", backend, mailbox).1;
        }
        if !has_work && open && self.processes_auto {
            self.processes_auto = false;
            return self.close_slot_of_kind("processes", backend, mailbox).1;
        }
        Vec::new()
    }

    /// The light bar's clock, in ms since the host started: tokio's, so
    /// tests can pause and fast-forward it.
    pub(super) fn clock_strip(&self) -> i64 {
        i64::try_from(self.strip_base.elapsed().as_millis()).unwrap_or(i64::MAX)
    }

    /// Shows the board to the light bar (ADR 0146) and schedules the next
    /// wake-up if the bar is going to change with no progress arriving.
    pub(super) fn annotate_strip(&mut self, mailbox: &mpsc::Sender<Message>) {
        let now = self.clock_strip();
        // Copies: progress lives behind a `watch`, and its guard cannot
        // cross the call. There are few of them (the board has a cap) and
        // they are small.
        //
        // Only THIS connection's: a task from a daemon that is no longer
        // there is never going to finish, and with it inside the burst
        // would never close.
        let snapshots: Vec<(norte_proto::TaskProgress, Option<f64>)> = self
            .tasks
            .values()
            .filter(|t| t.epoch == self.epoch_connection)
            .map(|t| (t.progress.borrow().clone(), t.rate.bps()))
            .collect();
        self.strip.update(
            now,
            snapshots
                .iter()
                .map(|(p, bps)| norte_frontend::task_strip::StripTask {
                    progress: p,
                    operand: p.current.as_ref(),
                    bps: *bps,
                }),
        );
        let Some(when) = self.strip.next_change_ms(now) else {
            return;
        };
        if self.strip_wake == Some(when) {
            return;
        }
        self.strip_wake = Some(when);
        let wait = std::time::Duration::from_millis(u64::try_from(when - now).unwrap_or(0));
        let mailbox = mailbox.clone();
        tokio::spawn(async move {
            tokio::time::sleep(wait).await;
            let _ = mailbox.send(Message::Strip).await;
        });
    }

    /// The time the bar asked for arrived: it is renoted, and a patch
    /// travels only if something depending on it changed (the status bar's
    /// elements are compared by `parche` alone; the panel, by
    /// `processes_automaticos`).
    pub(super) fn wake_strip(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        self.strip_wake = None;
        self.annotate_strip(mailbox);
        let mut sends = self.processes_automaticos(backend, mailbox);
        if sends.is_empty() && self.last_items.as_ref() != Some(&self.status_items_view()) {
            sends.push(self.parche(Vec::new()));
        }
        sends
    }

    /// How many rows the PAINTED board has.
    pub(super) fn board_rows(&self) -> usize {
        self.tasks.len().min(MAX_TASKS)
    }

    /// `true` if there is WORK in progress, the only thing that opens the
    /// panel on its own.
    ///
    /// Not `board_rows() > 0`: the board also lists the observational
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
        // Only this connection's, for the same reason as in `annotate_strip`:
        // a previous daemon's work is never going to finish, and the panel
        // would not close.
        self.tasks.values().any(|t| {
            t.epoch == self.epoch_connection
                && norte_frontend::tasks::counts_as_work(t.progress.borrow().kind)
        })
    }

    /// The PAINTED tasks' ids, in the order they are painted.
    ///
    /// It is what the panel's cursor needs: it stores the chosen one's
    /// IDENTITY, not its position, because the board moves on its own and a
    /// row expiring above it would make the same position name a different
    /// task.
    pub(super) fn board_ids(&self) -> Vec<u64> {
        self.tasks_visible().map(|(id, _)| *id).collect()
    }

    /// Which board row is chosen, over the PAINTED rows.
    ///
    /// An index over what the renderer highlights, not over the whole map:
    /// with the board trimmed by the cap, it pointed at a different one.
    /// `None` with zero rows, because an index with no row behind it
    /// highlights nothing.
    pub(super) fn board_cursor(&self) -> Option<u64> {
        self.cursor_processes
            .row(&self.board_ids())
            .and_then(|i| u64::try_from(i).ok())
    }

    pub(super) fn terminal(state: TaskStateView) -> bool {
        matches!(
            state,
            TaskStateView::Done | TaskStateView::Failed | TaskStateView::Cancelled
        )
    }
}
