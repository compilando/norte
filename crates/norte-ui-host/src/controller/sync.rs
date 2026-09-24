//! Compare two directories and synchronize them.
//!
//! Part of `controller`: these are `State` methods, moved here without
//! touching them (ADR 0086). The only writer is still the actor.

// These modules are the same `impl State` split into pieces, so they use
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
    pub(super) epoch: u64,
    /// It was abandoned before the Task came back.
    pub(super) abandonada: Arc<std::sync::atomic::AtomicBool>,
    /// The requested mode.
    modo: norte_proto::methods::SyncMode,
    /// Source root.
    source: VPath,
    /// Destination root.
    dest: VPath,
    /// The SOURCE's name reinterpretation, frozen when requested.
    source_encoding: Option<norte_encoding::NameEncoding>,
    /// The DESTINATION's, which can be a different one.
    dest_encoding: Option<norte_encoding::NameEncoding>,
}

/// A synchronization plan, with its SHARED model inside.
///
/// The model is `norte_frontend::sync::SyncView`, the same one the TUI uses:
/// what steps there are, what blocks them, whether it can be approved and
/// what state the Task is in. Not a single step or verdict is decided here;
/// the core produces the plan and only it can redeem it.
pub(super) struct Sync {
    /// Which of this window's plans this is.
    pub(super) epoch: u64,
    /// The PLAN's Task (the apply's is a different one, and the model holds
    /// it).
    task: norte_proto::TaskId,
    /// The view closed and whatever is left is extra.
    abandonada: Arc<std::sync::atomic::AtomicBool>,
    /// The shared model.
    pub(super) vista: norte_frontend::sync::SyncView,
    /// The window the renderer says it is painting.
    first_visible: usize,
    /// How many steps fit in that window.
    window: usize,
    /// Its report has already been requested: it is an RPC, and a
    /// reconnection re-announces the terminal.
    report_requested: bool,
    /// Which CONNECTION epoch its Task lives in.
    ///
    /// After a handoff, the new daemon hands out ids starting from 1:
    /// without this, an unrelated task with the same number would close this
    /// write's history with someone else's proof.
    epoch_connection: u64,
}

/// A comparison of two trees, with its SHARED pane inside.
///
/// The model — which rows there are, which categories are hidden, which one
/// is selected, which side the keys operate on — is
/// `norte_frontend::compare::ComparePane`, the same one the TUI paints.
/// Nothing is paired up again here and no verdict is decided: the core did
/// that, and reproducing it in the host would be a third copy.
pub(super) struct Comparison {
    /// Which of this window's comparisons this is. Same reason as a
    /// search's epoch: the Task's id arrives late.
    pub(super) epoch: u64,
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
    first_visible: usize,
    /// How many rows fit in that window.
    window: usize,
}

impl State {
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
    pub(super) fn request_sync_report(
        &mut self,
        p: &norte_proto::TaskProgress,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Message>,
    ) {
        let Some(sync) = self.sync.as_ref() else {
            return;
        };
        // The SAME three guards as a batch's report, and for the same
        // reasons: the CLASS (any `fs.copy` can carry the same id after a
        // handoff), the connection EPOCH (the new daemon's ids start over at
        // 1) and idempotence (a reconnection re-announces the terminal, and
        // this is an RPC).
        if sync.task != p.task_id
            || sync.epoch_connection != self.epoch_connection
            || !matches!(p.kind, norte_proto::TaskKind::Sync)
            || sync.report_requested
            || !matches!(
                sync.vista.state,
                norte_frontend::sync::SyncState::Applying(_)
            )
        {
            return;
        }
        let epoch = sync.epoch;
        let state = p.state.clone();
        let id = p.task_id;
        if let Some(s) = self.sync.as_mut() {
            s.report_requested = true;
        }
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let report = backend.sync_report(id).await;
            let _ = buzon
                .send(Message::Background(Box::new(Background::SyncReport(
                    epoch,
                    state,
                    Box::new(report),
                ))))
                .await;
        });
    }

    /// The report arrived: it enters the model, which decides what phrase
    /// comes out.
    pub(super) fn sync_report(
        &mut self,
        epoch: u64,
        state: &norte_proto::TaskState,
        report: Result<norte_proto::methods::SyncReportResult, Error>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let lang = self.lang;
        let Some(sync) = self.sync.as_mut().filter(|s| s.epoch == epoch) else {
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
        if let Some(category) = sync.vista.on_apply_ended(state, report, lang) {
            sync.vista.error = Some(clamp_display(category));
        }
        let change = ViewChange::Sync {
            sync: self.vista_sync(),
        };
        vec![self.parche(vec![change])]
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
    pub(super) fn close_sync(&mut self, p: &norte_proto::TaskProgress) -> Vec<ViewChange> {
        let lang = self.lang;
        let Some(sync) = self.sync.as_mut() else {
            return Vec::new();
        };
        if sync.task != p.task_id {
            return Vec::new();
        }
        sync.vista.run = norte_frontend::sync::SyncRunState::from_task_state(&p.state);
        if let norte_proto::TaskState::Failed { error } = &p.state {
            // The localized CATEGORY, never the English `Display`: this is
            // painted persistently and several variants interpolate data
            // from the other end.
            sync.vista.error = Some(clamp_display(norte_frontend::error::error_category_in(
                lang, error,
            )));
        }
        vec![ViewChange::Sync {
            sync: self.vista_sync(),
        }]
    }

    pub(super) fn close_comparison(&mut self, p: &norte_proto::TaskProgress) -> Vec<ViewChange> {
        let lang = self.lang;
        let Some(c) = self.comparison.as_mut() else {
            return Vec::new();
        };
        if c.task != p.task_id {
            return Vec::new();
        }
        let recibidas = c.vista.pane.len() as u64;
        c.vista
            .finish_from_task(&p.state, p.entries_done, recibidas, lang);
        vec![ViewChange::Compare {
            compare: self.vista_comparison(),
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
    pub(super) fn key_in_sync(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(sync) = self.sync.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        // With the SECOND question in front, the keys belong to it: only `y`
        // answers yes, and anything else withdraws it. A question that can
        // be answered with any key is not a question.
        if sync.vista.confirming.is_some() {
            let si = self.dialog_verb(k).is_some_and(|v| v == "dialog.approve");
            let Some(sync) = self.sync.as_mut() else {
                return (Self::stale(StaleAction::Modal), Vec::new());
            };
            sync.vista.confirming = None;
            if si {
                return self.apply_plan(backend, buzon);
            }
            let change = ViewChange::Sync {
                sync: self.vista_sync(),
            };
            return (self.applied(), vec![self.parche(vec![change])]);
        }
        // `Home`/`End` stay fixed keys: the shared catalog has no verb for
        // "to the start" inside a dialog.
        let verb = match k.key.as_str() {
            "Home" | "home" | "End" | "end" => None,
            _ => self.dialog_verb(k),
        };
        let Some(sync) = self.sync.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        match (verb.as_deref(), k.key.as_str()) {
            // Approving the plan is `dialog.approve`, not `dialog.confirm`:
            // what is being answered here is "yes, write" over a plan
            // already in front, which is exactly what that verb names — and
            // it is the same one the SECOND question is answered with.
            (Some("dialog.approve"), _) => self.request_approval(backend, buzon),
            (Some("dialog.cancel"), _) => {
                // While the daemon is WRITING, `Escape` asks to cancel and
                // does not close: closing loses the report — and with it the
                // count, the failures and the undo handle — over a
                // destination rewritten halfway.
                let writing = sync.vista.is_submitted()
                    || matches!(
                        sync.vista.state,
                        norte_frontend::sync::SyncState::Applying(_)
                    );
                if writing {
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
                    if !sync.vista.cancel_requested {
                        sync.vista.cancel_requested = true;
                        let task = sync.task;
                        if task.get() != 0 {
                            self.cancel(task.get());
                        }
                        let change = ViewChange::Sync {
                            sync: self.vista_sync(),
                        };
                        return (self.applied(), vec![self.parche(vec![change])]);
                    }
                    let task = sync.task;
                    self.sync = None;
                    if task.get() != 0 {
                        self.cancel(task.get());
                    }
                    let mut outside = vec![self.parche(vec![ViewChange::Sync { sync: None }])];
                    // And it IS SAID what is lost by closing: the
                    // destination may have been left halfway and its report
                    // is not going to be seen anymore.
                    outside.extend(self.say("msg-sync-closed-midway"));
                    return (self.applied(), outside);
                }
                if sync.vista.cancel_requested {
                    let task = sync.task;
                    sync.abandonada
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                    self.sync = None;
                    if task.get() != 0 {
                        self.cancel(task.get());
                    }
                    return (
                        self.applied(),
                        vec![self.parche(vec![ViewChange::Sync { sync: None }])],
                    );
                }
                sync.vista.cancel_requested = true;
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
                if matches!(sync.vista.run, norte_frontend::sync::SyncRunState::Running) {
                    sync.vista.run = norte_frontend::sync::SyncRunState::Cancelled;
                }
                let task = sync.task;
                if task.get() != 0 {
                    self.cancel(task.get());
                }
                let change = ViewChange::Sync {
                    sync: self.vista_sync(),
                };
                (self.applied(), vec![self.parche(vec![change])])
            }
            (Some("dialog.down" | "dialog.up" | "dialog.page-down" | "dialog.page-up"), _)
            | (_, "Home" | "home" | "End" | "end") => {
                let total = sync.vista.steps().len();
                if total == 0 {
                    return (self.applied(), Vec::new());
                }
                // The scroll CAP is "how much there is minus how much fits",
                // not "how much there is minus one": with the latter, a
                // single arrow over a two-step plan and a window of two
                // hundred used to stop sending the first step.
                let cap = total.saturating_sub(sync.window.max(1));
                let page = sync.window.max(1);
                sync.first_visible = match (verb.as_deref(), k.key.as_str()) {
                    (Some("dialog.down"), _) => sync.first_visible.saturating_add(1),
                    (Some("dialog.up"), _) => sync.first_visible.saturating_sub(1),
                    (Some("dialog.page-down"), _) => sync.first_visible.saturating_add(page),
                    (Some("dialog.page-up"), _) => sync.first_visible.saturating_sub(page),
                    (_, "Home" | "home") => 0,
                    _ => cap,
                }
                .min(cap);
                let change = ViewChange::Sync {
                    sync: self.vista_sync(),
                };
                (self.applied(), vec![self.parche(vec![change])])
            }
            // What it does not understand is SWALLOWED: a panel that lets
            // keys through is not a screen.
            _ => (self.applied(), Vec::new()),
        }
    }

    /// `a`: asks to approve the plan. There may be a SECOND question.
    ///
    /// The second one is not ceremony: it is composed by the shared model
    /// with one branch per undo perspective, and it only appears when the
    /// plan deletes trees or leaves something with no way back. A plan that
    /// undoes in full and deletes nothing does not have it — always asking
    /// is what teaches people to answer without reading.
    pub(super) fn request_approval(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let lang = self.lang;
        let Some(sync) = self.sync.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        if !sync.vista.can_approve() {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-sync-cannot-approve".to_owned(),
                },
                Vec::new(),
            );
        }
        let question = sync.vista.state.plan().and_then(|p| p.confirmation(lang));
        match question {
            Some(c) => {
                sync.vista.confirming = Some(c);
                let change = ViewChange::Sync {
                    sync: self.vista_sync(),
                };
                (self.applied(), vec![self.parche(vec![change])])
            }
            None => self.apply_plan(backend, buzon),
        }
    }

    /// Sends `sync.apply` with the hash the CORE returned.
    ///
    /// Through `SyncView::submit`, the ONLY door: it checks `can_approve` and
    /// throws the in-flight-apply latch in the same gesture. Splitting them
    /// leaves a window in which a second `a` — or an `Escape` — fits between
    /// the request going out and the daemon answering, and this window reads
    /// events between keys, so it is genuinely reachable.
    pub(super) fn apply_plan(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let epoch = self.sync.as_ref().map_or(0, |s| s.epoch);
        let Some(hash) = self.sync.as_mut().and_then(|s| s.vista.submit()) else {
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
            let result = backend2.sync_apply(hash).await;
            match result {
                Ok(task) => {
                    let id = task.id;
                    let _ = buzon2
                        .send(Message::TaskNew(Box::new((task, Vec::new(), None))))
                        .await;
                    let _ = buzon2
                        .send(Message::Background(Box::new(Background::SyncApplying(
                            epoch, id,
                        ))))
                        .await;
                }
                Err(e) => {
                    // Is it KNOWN that it did not write? Only if the daemon
                    // answered no. A dead transport leaves the request up in
                    // the air.
                    let safe = matches!(
                        e,
                        Error::PolicyDenied { .. }
                            | Error::Conflict { .. }
                            | Error::NotFound
                            | Error::PermissionDenied
                            | Error::InvalidPath
                            | Error::Unsupported
                            | Error::EncodingLoss
                    );
                    let _ = buzon2.send(Message::TaskFailed(Box::new(e))).await;
                    // And the latch is released — when it should be —
                    // without this `a` stays dead forever over a plan nobody
                    // applied.
                    let _ = buzon2
                        .send(Message::Background(Box::new(Background::SyncNoApplied(
                            epoch, safe,
                        ))))
                        .await;
                }
            }
        });
        let change = ViewChange::Sync {
            sync: self.vista_sync(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// The daemon accepted the apply: the model switches to APPLYING.
    pub(super) fn sync_applying(
        &mut self,
        epoch: u64,
        task: norte_proto::TaskId,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Message>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Some(sync) = self.sync.as_mut().filter(|s| s.epoch == epoch) else {
            return Vec::new();
        };
        // The Task being followed switches to the APPLY's: it is what
        // `Escape` points at now, and the one the report has to be requested
        // from.
        sync.task = task;
        sync.epoch_connection = self.epoch_connection;
        if !sync.vista.on_apply_started(task) {
            // The model DENIES it — the reader asked to stop in the window
            // during which the apply had no id yet — and then canceling it
            // is OUR job: nobody else knows that id, and the model's
            // contract spells it out. Without this, the daemon kept
            // rewriting the destination of a plan the human cancelled.
            sync.vista.on_apply_abandoned();
            let (_, mut outside) = self.cancel(task.get());
            outside.extend(self.say("msg-sync-cancelled-late"));
            outside.push(self.parche(vec![ViewChange::Sync {
                sync: self.vista_sync(),
            }]));
            return outside;
        }
        // It could be born TERMINAL: the daemon completed it before
        // answering and its progress never fires. It is the same race the
        // board already documents, and here it translates into a panel
        // stuck applying forever.
        let nacio = self
            .tasks
            .get(&task.get())
            .map(|t| t.progress.borrow().clone());
        let mut outside = vec![self.parche(vec![ViewChange::Sync {
            sync: self.vista_sync(),
        }])];
        if let Some(p) = nacio.filter(|p| p.state.is_terminal()) {
            self.request_sync_report(&p, backend, buzon);
        }
        outside.extend(Vec::new());
        outside
    }

    pub(super) fn key_in_comparison(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.comparison.is_none() {
            return (Self::stale(StaleAction::Modal), Vec::new());
        }
        // The filters' DIGITS do not go through the resolver: they are
        // positional — the nth of `CATEGORIES` — and there are no five verbs
        // to name them. It is the same decision as in the TUI.
        let digito = k.key.len() == 1 && k.key.chars().all(|c| ('1'..='5').contains(&c));
        let verb = if digito { None } else { self.dialog_verb(k) };
        let Some(c) = self.comparison.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        match (verb.as_deref(), k.key.as_str()) {
            (Some("dialog.cancel"), _) => {
                if c.vista.cancel_requested {
                    let task = c.task;
                    c.abandonada
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                    self.comparison = None;
                    if task.get() != 0 {
                        self.cancel(task.get());
                    }
                    return (
                        self.applied(),
                        vec![self.parche(vec![ViewChange::Compare { compare: None }])],
                    );
                }
                c.vista.cancel_requested = true;
                let task = c.task;
                if task.get() != 0 {
                    self.cancel(task.get());
                }
                let change = ViewChange::Compare {
                    compare: self.vista_comparison(),
                };
                (self.applied(), vec![self.parche(vec![change])])
            }
            (Some("dialog.pane"), _) => {
                // Switching sides changes which pane `Enter` navigates to
                // and which side the file keys operate on.
                c.vista.pane.swap_active_side();
                let change = ViewChange::Compare {
                    compare: self.vista_comparison(),
                };
                (self.applied(), vec![self.parche(vec![change])])
            }
            (Some("dialog.confirm"), _) => {
                let Some(id) = c.vista.pane.selected_id() else {
                    return (self.applied(), Vec::new());
                };
                self.comparison_active(id, backend, buzon)
            }
            (Some(v @ ("dialog.up" | "dialog.down")), _) => {
                let down = v == "dialog.down";
                let visible = c.vista.pane.visible_ids();
                if visible.is_empty() {
                    return (self.applied(), Vec::new());
                }
                let actual = c
                    .vista
                    .pane
                    .selected_id()
                    .and_then(|id| visible.iter().position(|v| *v == id))
                    .unwrap_or(0);
                let dest = if down {
                    (actual + 1).min(visible.len() - 1)
                } else {
                    actual.saturating_sub(1)
                };
                let id = visible[dest];
                c.vista.pane.select(id);
                let change = ViewChange::Compare {
                    compare: self.vista_comparison(),
                };
                (self.applied(), vec![self.parche(vec![change])])
            }
            // 1..5: the filters, in the categories' fixed order, same as in
            // the TUI.
            (_, d) if digito => {
                let i = d.chars().next().and_then(|c| c.to_digit(10)).unwrap_or(1) as usize - 1;
                let Some(cat) = norte_frontend::compare::CATEGORIES.get(i).copied() else {
                    return (self.applied(), Vec::new());
                };
                c.vista.pane.toggle_filter(cat);
                let change = ViewChange::Compare {
                    compare: self.vista_comparison(),
                };
                (self.applied(), vec![self.parche(vec![change])])
            }
            // A key it does not understand is SWALLOWED just the same: a
            // panel that lets through what it does not understand is not a
            // screen, it is decoration.
            _ => (self.applied(), Vec::new()),
        }
    }

    /// Chooses a row from the differences panel.
    /// The differences panel's four actions, in one arm.
    ///
    /// Together and not four arms of the general dispatch: they are the same
    /// surface and none of them means anything without it.
    pub(super) fn comparison_action(
        &mut self,
        action: &UiAction,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match action {
            UiAction::CompareSelectRow { id } => self.comparison_selecciona(*id),
            UiAction::CompareActivateRow { id } => self.comparison_active(*id, backend, buzon),
            UiAction::CompareToggleFilter { category } => self.comparison_filters(category),
            UiAction::CompareSetVisibleRange { first, count } => {
                self.comparison_window(*first, *count)
            }
            // The general dispatch only sends those four here.
            _ => (Self::stale(StaleAction::Modal), Vec::new()),
        }
    }

    /// Chooses a row from the differences panel.
    pub(super) fn comparison_selecciona(
        &mut self,
        id: u64,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(c) = self.comparison.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        // `select` IGNORES an id that did not arrive, which is correct: the
        // alternative is a selection naming a nonexistent row.
        c.vista.pane.select(id);
        if c.vista.pane.selected_id() != Some(id) {
            return (Self::stale(StaleAction::Generation), Vec::new());
        }
        let change = ViewChange::Compare {
            compare: self.vista_comparison(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// Shows or hides a whole category.
    pub(super) fn comparison_filters(
        &mut self,
        category: &str,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(c) = self.comparison.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        let Some(cat) = norte_frontend::compare::CATEGORIES
            .iter()
            .find(|c| c.id() == category)
        else {
            // A category that does not exist is a renderer from another
            // version, not an order.
            return (Self::stale(StaleAction::Generation), Vec::new());
        };
        c.vista.pane.toggle_filter(*cat);
        let change = ViewChange::Compare {
            compare: self.vista_comparison(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// The renderer says which window it paints.
    pub(super) fn comparison_window(
        &mut self,
        first: u64,
        how_many: u32,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(c) = self.comparison.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        c.first_visible = usize::try_from(first).unwrap_or(0);
        // Capped: whatever the renderer says fits cannot make a patch carry
        // half a million rows.
        c.window = usize::try_from(how_many)
            .unwrap_or(Self::WINDOW_COMPARISON)
            .clamp(1, MAX_ROWS_PER_BATCH);
        let change = ViewChange::Compare {
            compare: self.vista_comparison(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// Opens the chosen row: navigates to the ACTIVE side's directory.
    ///
    /// Where to go is decided by the SHARED model (`navigation_target`): the
    /// row if it is a directory, its parent if it is a file, and `None` when
    /// that side is empty — an orphan looked at from the side that does not
    /// have it — which does NOT fall back to the other side.
    pub(super) fn comparison_active(
        &mut self,
        id: u64,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(c) = self.comparison.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        c.vista.pane.select(id);
        let Some(dest) = c.vista.pane.navigation_target() else {
            let the_side =
                norte_frontend::compare::side_label(c.vista.pane.active_side(), self.lang);
            return (
                ActionAck::Unavailable {
                    reason_key: "compare-no-target".to_owned(),
                },
                self.say_with("compare-no-target", &[("side", &the_side)]),
            );
        };
        // The pane that navigates is the ACTIVE side's, not whichever has
        // focus: whoever is looking at the right cannot lose their left
        // directory by pressing `Enter`. That slot is FOCUSED and navigation
        // goes through the usual path, the one that records the trail and
        // requests the listing.
        if let Some(slot) = self.side_slot() {
            self.roles.set(RoleId::Active, SlotId(slot));
            self.reconcilia_roles();
        }
        let mut outputs = self.navigate(&dest, Trail::Record, backend, buzon);
        let change = ViewChange::Compare {
            compare: self.vista_comparison(),
        };
        outputs.push(self.parche(vec![change]));
        (self.applied(), outputs)
    }

    /// The slot corresponding to the comparison's ACTIVE side.
    pub(super) fn side_slot(&self) -> Option<u32> {
        let c = self.comparison.as_ref()?;
        let left_one = u32::try_from(c.vista.left_pane).ok()?;
        match c.vista.pane.active_side() {
            norte_proto::methods::Side::Right => self.slot_dest().ok(),
            _ => Some(left_one),
        }
    }

    /// Requests the PLAN to synchronize the active pane onto the destination.
    ///
    /// The plan writes not a single byte: it says what it would do. What
    /// writes is `sync.apply`, and only against the hash this plan closes
    /// with.
    pub(super) fn request_sync(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // ONE at a time. Relaunching left the previous panel un-abandoned and
        // its Task uncancelled — the daemon kept walking a tree for a plan
        // that can no longer be seen — and, with a request in flight, the
        // second press killed both panels' one.
        if self.sync.is_some() || self.sync_pedida.is_some() {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-sync-already".to_owned(),
                },
                Vec::new(),
            );
        }
        let dest_slot = match self.slot_dest() {
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
        let focused = self.slot().pane.dir().clone();
        let other = self.slots[&dest_slot].pane.dir().clone();
        // With the differences panel open the ACTIVE SIDE rules; without it,
        // the focused pane is the source. Both branches live in the shared
        // rule, and here only the data is passed to it.
        let roots = norte_frontend::sync::sync_roots(
            self.comparison.as_ref().map(|c| &c.vista),
            &norte_frontend::sync::Panes {
                focused_root: &focused,
                focused_encoding: None,
                other_root: &other,
                other_encoding: None,
            },
        );
        let (source, dest) = (roots.source.clone(), roots.dest.clone());
        if source == dest {
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
        (self.applied(), self.launch_sync_plan(roots, backend, buzon))
    }

    /// Enqueues `sync.plan` and hooks its event channel to the actor.
    pub(super) fn launch_sync_plan(
        &mut self,
        roots: norte_frontend::sync::SyncRoots,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Message>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let norte_frontend::sync::SyncRoots {
            source,
            dest,
            source_encoding,
            dest_encoding,
        } = roots;
        self.epoch_search += 1;
        let epoch = self.epoch_search;
        let abandonada = Arc::new(std::sync::atomic::AtomicBool::new(false));
        // `Update` and not `Mirror`: the mode that does NOT delete is the
        // one that can be the default. Choosing mirror is a decision made on
        // purpose, and until there is somewhere to make it, it is not
        // offered.
        let modo = norte_proto::methods::SyncMode::Update;
        let params = norte_proto::methods::SyncPlanParams {
            source: source.clone(),
            dest: dest.clone(),
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
                    let _ = buzon2.send(Message::TaskFailed(Box::new(e))).await;
                    let _ = buzon2
                        .send(Message::Background(Box::new(
                            Background::PlanDeSyncFallido(epoch),
                        )))
                        .await;
                    return;
                }
            };
            let id = task.id;
            let cancel = Arc::clone(&task.cancel);
            let _ = buzon2
                .send(Message::TaskNew(Box::new((task, Vec::new(), None))))
                .await;
            let _ = buzon2
                .send(Message::Background(Box::new(Background::PlanDeSyncVivo(
                    epoch, id,
                ))))
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
                    .send(Message::Background(Box::new(Background::SyncEvent(
                        epoch,
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
        self.sync = None;
        self.sync_pedida = Some(SyncPedida {
            epoch,
            abandonada,
            modo,
            source,
            dest,
            source_encoding,
            dest_encoding,
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
    pub(super) fn open_sync_panel(
        &mut self,
        epoch: u64,
        task: norte_proto::TaskId,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // FILTER before TAKING: an unconditional `take()` swept away a new
        // request when an old one's Task answered, and then no panel opened
        // at all while two traversals kept walking two trees on the daemon.
        if self.sync_pedida.as_ref().is_none_or(|p| p.epoch != epoch) {
            return Vec::new();
        }
        let Some(pedida) = self.sync_pedida.take() else {
            return Vec::new();
        };
        self.sync = Some(Sync {
            epoch,
            task,
            abandonada: pedida.abandonada,
            vista: norte_frontend::sync::SyncView::new(
                task,
                pedida.modo,
                pedida.source,
                pedida.dest,
                // Each side's reinterpretations, exactly as the shared rule
                // decided them: there are TWO because the two panes are two
                // locations, and swapping them would name the file the
                // write lands on with different bytes.
                pedida.source_encoding,
                pedida.dest_encoding,
            ),
            first_visible: 0,
            window: Self::WINDOW_COMPARISON,
            epoch_connection: self.epoch_connection,
            report_requested: false,
        });
        let change = ViewChange::Sync {
            sync: self.vista_sync(),
        };
        vec![self.parche(vec![change])]
    }

    pub(super) fn apply_sync_event(
        &mut self,
        epoch: u64,
        ev: norte_client::SyncPlanEvent,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Some(sync) = self.sync.as_mut() else {
            return Vec::new();
        };
        if sync.epoch != epoch {
            return Vec::new();
        }
        // The SHARED model decides what gets in: it discards whatever comes
        // from another plan by its `task_id`, and it is the one that knows
        // when the plan closes.
        let change = match ev {
            norte_client::SyncPlanEvent::Steps(batch) => sync.vista.state.on_steps(batch),
            norte_client::SyncPlanEvent::Done(done) => sync.vista.state.on_plan_done(done),
        };
        if !change {
            // That it was discarded IS STATED: a batch rejected after
            // closing is a violation of the daemon's contract, and staying
            // quiet about it hides it.
            tracing::warn!(epoch, "a plan event was discarded");
            return Vec::new();
        }
        let change = ViewChange::Sync {
            sync: self.vista_sync(),
        };
        vec![self.parche(vec![change])]
    }

    /// The window's steps, projected by the SHARED model.
    pub(super) fn steps_proyectados(
        steps: &[norte_proto::methods::SyncStep],
        trash: norte_proto::methods::DestTrash,
        enc: norte_frontend::sync::SyncEncodings,
        lang: norte_i18n::Lang,
    ) -> Vec<crate::dto::SyncStepView> {
        steps
            .iter()
            .map(|paso| {
                // The cells are composed by the SHARED model: what the step
                // does, why, whether undo brings it back — which NEVER comes
                // straight from `reversal`, because that is half an answer —
                // and both spellings when there are two.
                let c = norte_frontend::sync::render_step(paso, trash, enc);
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
                    anchor: Self::anchor_name(c.anchor),
                    anchor_label: Self::anchor_label(c.anchor, lang),
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
    pub(super) fn failures_proyectados(
        state: &norte_frontend::sync::SyncState,
        enc: norte_frontend::sync::SyncEncodings,
        lang: norte_i18n::Lang,
    ) -> Vec<crate::dto::SyncFailureView> {
        let norte_frontend::sync::SyncState::Applied(a) = state else {
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
                    anchor: Self::anchor_name(c.anchor),
                    anchor_label: Self::anchor_label(c.anchor, lang),
                }
            })
            .collect()
    }

    /// Which root a path hangs off, by its stable id.
    ///
    /// `either` is stated: on a panel where an unqualified path means "from
    /// the source", staying quiet about it asserts the source.
    pub(super) fn anchor_name(anchor: norte_frontend::sync::RelAnchor) -> String {
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
    pub(super) fn anchor_label(
        anchor: norte_frontend::sync::RelAnchor,
        lang: norte_i18n::Lang,
    ) -> String {
        norte_frontend::sync::anchor_label(anchor, lang).map_or_else(String::new, clamp_display)
    }

    /// The synchronization panel's projection, capped to its window.
    pub(super) fn vista_sync(&self) -> Option<crate::dto::SyncView> {
        let sync = self.sync.as_ref()?;
        let v = &sync.vista;
        let (source, source_hostile) = norte_frontend::path_display(&v.source_root);
        let (dest, dest_hostile) = norte_frontend::path_display(&v.dest_root);
        let steps = v.steps();
        let first = sync.first_visible.min(steps.len());
        let until = first.saturating_add(sync.window).min(steps.len());
        let trash = v.dest_trash();
        let enc = v.encodings();
        let rows = Self::steps_proyectados(
            steps.get(first..until).unwrap_or_default(),
            trash,
            enc,
            self.lang,
        );
        let failures = Self::failures_proyectados(&v.state, enc, self.lang);
        Some(crate::dto::SyncView {
            source: crate::dto::DialogLine {
                text: clamp_display(source),
                hostile: source_hostile,
            },
            dest: crate::dto::DialogLine {
                text: clamp_display(dest),
                hostile: dest_hostile,
            },
            // The mode, by the SHARED label. Falling back to "update" for a
            // mode this build cannot name would assert the SAFE half of what
            // is being approved — "this does not delete" — about something
            // unknown, and the catalog itself forbids that in writing.
            mode: clamp_display(norte_frontend::sync::mode_label(v.mode, self.lang)),
            steps: rows,
            first_visible: first as u64,
            // The RETAINED ones plus what the model dropped: without adding
            // them, this number and the status line's contradict each other
            // on a large plan, and both cross in the same message.
            total: (steps.len() as u64).saturating_add(
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
                            let path =
                                norte_frontend::sync::rel_display_or_root(&b.rel, None, self.lang);
                            crate::dto::SyncBlockerView {
                                label: clamp_display(norte_frontend::sync::blocker_label(
                                    b.kind, self.lang,
                                )),
                                path: clamp_display(path.text),
                                path_hostile: path.hostile,
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
            failures,
            cancel_requested: v.cancel_requested,
            can_approve: v.can_approve(),
            running: matches!(v.run, norte_frontend::sync::SyncRunState::Running),
        })
    }

    /// How many comparison rows — or a plan's steps — get through if the
    /// renderer has not said its window yet.
    pub(super) const WINDOW_COMPARISON: usize = 200;

    /// Launches the two panes' comparison and opens the differences panel.
    ///
    /// The right root comes from the slot holding the `Target` role, through
    /// the SAME path as a transfer: two ways of deciding "the other pane"
    /// are two places they can drift apart, and with several candidates and
    /// none designated it asks to choose instead of breaking the tie.
    pub(super) fn request_comparison(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let right = match self.directory_dest() {
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
        let left = self.slot().pane.dir().clone();
        if left == right {
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
            self.applied(),
            self.launch_comparison(left, right, backend, buzon),
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
    pub(super) fn count_size(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // `marked_paths` falls back to the cursor when there are no marks:
        // the same source of "what this operates on" a transfer uses.
        let paths: Vec<VPath> = self.slot().pane.marked_paths();
        if paths.is_empty() {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-nothing-selected".to_owned(),
                },
                self.say("msg-nothing-selected"),
            );
        }
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let message = match backend.dir_size(paths).await {
                Ok(task) => Message::TaskNew(Box::new((task, Vec::new(), None))),
                Err(e) => Message::TaskFailed(Box::new(e)),
            };
            let _ = buzon.send(message).await;
        });
        (self.applied(), Vec::new())
    }

    /// Enqueues `fs.compare` and hooks its row channel to the actor.
    pub(super) fn launch_comparison(
        &mut self,
        left: VPath,
        right: VPath,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Message>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        self.epoch_search += 1;
        let epoch = self.epoch_search;
        let abandonada = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let params = norte_proto::methods::FsCompareParams {
            left: left.clone(),
            right: right.clone(),
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
        self.comparison = Some(Comparison {
            epoch,
            task: norte_proto::TaskId::new(0),
            abandonada: Arc::clone(&abandonada),
            vista: norte_frontend::compare::CompareView::new(
                left,
                right,
                // The slot that launched the comparison IS the left side, and
                // that decides which pane an `Enter` navigates to. Without
                // it, whoever is looking at the right side lost their left
                // directory to go see the right one's.
                self.active() as usize,
                None,
                None,
            ),
            first_visible: 0,
            window: Self::WINDOW_COMPARISON,
        });
        let backend2 = Arc::clone(backend);
        let buzon2 = buzon.clone();
        tokio::spawn(async move {
            let (task, mut rx) = match backend2.compare(params).await {
                Ok(par) => par,
                Err(e) => {
                    let _ = buzon2.send(Message::TaskFailed(Box::new(e))).await;
                    return;
                }
            };
            let id = task.id;
            let cancel = Arc::clone(&task.cancel);
            let _ = buzon2
                .send(Message::TaskNew(Box::new((task, Vec::new(), None))))
                .await;
            let _ = buzon2
                .send(Message::Background(Box::new(Background::ComparisonViva(
                    epoch, id,
                ))))
                .await;
            // The view may have closed while the daemon was accepting the
            // Task: in that window the actor has nobody to cancel, so
            // whoever does have it cancels it.
            if abandonada.load(std::sync::atomic::Ordering::SeqCst) {
                cancel();
                return;
            }
            while let Some(batch) = rx.recv().await {
                if abandonada.load(std::sync::atomic::Ordering::SeqCst) {
                    cancel();
                    return;
                }
                if buzon2
                    .send(Message::Background(Box::new(Background::RowsComparadas(
                        epoch,
                        Box::new(batch),
                    ))))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        });
        let change = ViewChange::Compare {
            compare: self.vista_comparison(),
        };
        vec![self.parche(vec![change])]
    }

    /// A batch of compared rows. Matches by EPOCH, like search hits.
    pub(super) fn apply_rows_comparadas(
        &mut self,
        epoch: u64,
        batch: norte_proto::methods::CompareRowsBatch,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Some(c) = self.comparison.as_mut() else {
            return Vec::new();
        };
        if c.epoch != epoch {
            return Vec::new();
        }
        // The SHARED pane is the one that counts, filters and selects: here
        // it is only given the rows.
        c.vista.pane.extend(batch.rows);
        let change = ViewChange::Compare {
            compare: self.vista_comparison(),
        };
        vec![self.parche(vec![change])]
    }

    /// The differences panel's projection, capped to its window.
    pub(super) fn vista_comparison(&self) -> Option<crate::dto::CompareView> {
        use norte_frontend::compare::{Category, cells_for};

        let c = self.comparison.as_ref()?;
        let now = now_ms();
        let (left_side, left_hostile) = norte_frontend::path_display(&c.vista.left_root);
        let (right_side, right_hostile) = norte_frontend::path_display(&c.vista.right_root);
        let visible: Vec<&norte_proto::methods::CompareRow> = c.vista.pane.visible().collect();
        let first = c.first_visible.min(visible.len());
        let until = first.saturating_add(c.window).min(visible.len());
        let rows = visible
            .get(first..until)
            .unwrap_or_default()
            .iter()
            .map(|r| {
                // The cells are composed by the SHARED model: the masked
                // names with their flag, and the two glyphs in the middle.
                // Neither the pairing nor the verdict is recomputed here.
                let cells = cells_for(r, None, None);
                let side = |f: Option<&norte_frontend::compare::RowFace>| {
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
                                    now,
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
                    left: side(cells.left.as_ref()),
                    right: side(cells.right.as_ref()),
                    paired_under: norte_frontend::compare::paired_under_label(
                        r.paired_under,
                        self.lang,
                    )
                    .map(clamp_display),
                }
            })
            .collect();
        let filters = norte_frontend::compare::CATEGORIES
            .iter()
            .map(|cat| crate::dto::CompareFilterView {
                id: cat.id().to_owned(),
                label: clamp_display(cat.label(self.lang)),
                count: c.vista.pane.count_of(*cat) as u64,
                hidden: c.vista.pane.is_hidden(*cat),
            })
            .collect();
        Some(crate::dto::CompareView {
            left: clamp_display(left_side),
            left_hostile,
            right: clamp_display(right_side),
            right_hostile,
            rows,
            first_visible: first as u64,
            total: visible.len() as u64,
            selected: c.vista.pane.selected_id(),
            filters,
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
