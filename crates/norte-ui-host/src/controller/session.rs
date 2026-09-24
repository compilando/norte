//! Reading and flushing the session.
//!
//! Part of `controller`: these are methods of `State`, moved here without
//! touching them (ADR 0086). The only writer is still the actor.

// These modules are the same `impl State` split into pieces, so they use
// the same imports as the parent. Listing them here would be a forty-line
// list per file, across 32 files, that goes out of sync the moment the
// parent imports something — `super::*` keeps it in sync on its own.
#[allow(clippy::wildcard_imports)]
use super::*;

impl State {
    /// Reads the session and applies it, if it can.
    ///
    /// Three things are decided here, and all three belong to ADR 0059:
    ///
    /// - **Who writes.** A LOOSE window does not write. The session is a
    ///   document with a single writer, and two windows saving theirs on top
    ///   of each other is exactly what produces a screen nobody asked for.
    /// - **What gets applied.** Only what this host understands. A slot of an
    ///   unknown kind is NOT touched, not even to delete it.
    /// - **What is NOT overwritten.** If what is stored is of a newer schema,
    ///   it starts from the configuration and leaves it alone: starting with
    ///   no session is recoverable; clobbering a future version's is not.
    ///
    /// And the LAYOUT saved under this profile's key is set before the slots,
    /// ahead of `--layout`'s and the configuration's — the same order as the
    /// terminal's: the session is more specific than both, because it is how
    /// the screen was when it closed. This is ADR 0058's D8: close one
    /// frontend, open the other, pick up where you were. Until now the window
    /// did not read it, so toggling a panel lasted only until it closed.
    pub(super) async fn leer_session(&mut self, backend: &dyn HostBackend) {
        let Ok((session, owner)) = backend.session_get().await else {
            // With no readable session it still starts: it is memory of
            // where you were, not a requirement to exist.
            return;
        };
        self.session.revision = session.revision;
        self.session.owner = owner;
        if session.version > norte_frontend::session::SCHEMA_VERSION {
            self.session.future = true;
            return;
        }
        if session.version == 0 {
            // Nobody has written it yet.
            return;
        }
        // Through the VALIDATING constructor, like the terminal, and not
        // through plain serde: a body whose layout has no listing parses just
        // the same, and setting it would leave `slots` empty and the next
        // key on `slot()`'s `expect` (#242). A body that is not valid is
        // left alone and it starts from the configuration, like a future
        // one.
        let Ok(body) =
            norte_frontend::session::SessionBody::from_value(session.version, &session.body)
        else {
            tracing::warn!("the stored session is not understood: starting from configuration");
            return;
        };
        // THIS window's layout (ADR 0139), and if it does not have one yet —
        // first time after the change, or a profile that only ever used the
        // terminal — the shared one, so as not to start from factory
        // defaults.
        let own_key = norte_frontend::session::window_layout_key(&self.session_key());
        if let Some(tree) = body
            .layouts
            .get(&own_key)
            .or_else(|| body.layouts.get(&self.session_key()))
            .cloned()
        {
            // Without waking anything: the listings are requested later, once
            // the session has said where each one was. Waking them here would
            // request the startup directory just to throw it away an instant
            // later.
            self.set_tree(tree, None);
        }
        self.apply_session(&body);
        self.palette_recent.clone_from(&body.palette_recent);
        self.popular = norte_frontend::history::Popular::from_entries(body.popular.clone());
        self.session.known = body.slots.keys().copied().collect();
        for (id, slot_state) in &body.slots {
            self.session.touched.insert(*id, slot_state.touched_ms);
        }
        self.session.read = body;
    }

    /// Under which `layouts` key this window's screen goes.
    ///
    /// The same as the terminal's (`App::session_key`): the active profile's
    /// name, or `default` with none. A profile whose directory is not UTF-8
    /// falls back to `default`, which is what the selector warns about with
    /// `carries_state`.
    pub(super) fn session_key(&self) -> String {
        self.profile_active
            .as_ref()
            .and_then(|n| n.to_str())
            .filter(|s| !s.is_empty())
            .map_or_else(|| "default".to_owned(), ToOwned::to_owned)
    }

    /// A one-second tick over the status bar's notice (spec 2026-09-10, `[ui]
    /// notice_seconds`): past the cap, the message leaves the bar, goes to
    /// the log via `tracing`, and `notices_unread` counts one more. With `0`
    /// nothing expires. Opening the log panel resets the count to zero.
    /// Returns the state patch if something changed; tests advance it tick by
    /// tick, with no clock.
    ///
    /// The count is by TEXT: repeating the same action within the deadline
    /// does not reset it (revision m10). Resetting it on assignment would
    /// require a setter at the ~40 places that write `status.message`; left
    /// noted here.
    pub(super) fn expire_notice(&mut self) -> Option<BridgeEnvelope<UiUpdate>> {
        let mut changed = false;
        let log_open = self
            .tree
            .slot_ids()
            .into_iter()
            .any(|id| self.tree.kind_of(id).is_some_and(|k| k.as_str() == "log"));
        if log_open && self.status.notices_unread != 0 {
            self.status.notices_unread = 0;
            changed = true;
        }
        match self.status.message.as_deref() {
            None => {
                self.message_ticks = 0;
                self.message_counted = None;
            }
            Some(msg) => {
                if self.message_counted.as_deref() == Some(msg) {
                    self.message_ticks = self.message_ticks.saturating_add(1);
                } else {
                    self.message_counted = Some(msg.to_owned());
                    self.message_ticks = 1;
                }
                let cap = self.config.common.ui_chrome.notice_seconds();
                if cap > 0 && self.message_ticks >= cap {
                    let text = self.status.message.take().unwrap_or_default();
                    self.message_ticks = 0;
                    self.message_counted = None;
                    self.status.notices_unread = self.status.notices_unread.saturating_add(1);
                    // `info`, not `warn`: "copied 1 file" is not a warning,
                    // and the level is what the log panel filters by.
                    tracing::info!(target: "norte::notice", "{text}");
                    changed = true;
                }
            }
        }
        changed.then(|| self.parche(vec![ViewChange::Status(self.status.clone())]))
    }

    /// Checks whether the screen changed since the last write and, if it did,
    /// writes it OUTSIDE the actor. This is the session's tick, and also what
    /// every tree change calls without waiting for the tick.
    ///
    /// A loose window does not write; a future session is not clobbered; and
    /// with a dialog in front, what is being decided is not saved, like in
    /// the terminal. With a `put` in flight, it waits for the answer: two
    /// crossed writes with the same revision are a sure conflict.
    pub(super) fn push_session(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) {
        if !self.session.owner
            || self.session.future
            || self.session.in_flight.is_some()
            || !self.dialogs.is_empty()
        {
            return;
        }
        let now = u64::try_from(now_ms()).unwrap_or(0);
        let mut body = self.capture_session();
        if self.session.no_history {
            body.degrade_for_size();
        }
        let alive: Vec<SlotId> = self.slots.keys().map(|id| SlotId(*id)).collect();
        let Some(sealed) = self.session.policy.prepare(&mut body, &alive, now) else {
            return;
        };
        for SlotId(id) in sealed {
            self.session.touched.insert(id, now);
        }
        let Ok(json) = serde_json::to_value(&body) else {
            return;
        };
        let arc_body = std::sync::Arc::new(body);
        self.session.in_flight = Some(std::sync::Arc::clone(&arc_body));
        let backend = Arc::clone(backend);
        let mailbox = mailbox.clone();
        let revision = self.session.revision;
        tokio::spawn(async move {
            let res = backend
                .session_put(norte_frontend::session::SCHEMA_VERSION, revision, json)
                .await;
            let _ = mailbox
                .send(Message::SessionPlaced(Box::new((res, arc_body))))
                .await;
        });
    }

    /// Requests HANDOFF to the terminal (phase 9): flushes the screen WITH
    /// the marks and releases the session.
    ///
    /// Both things are spawned, and in that order: releasing before writing
    /// would leave the terminal reading the screen from a second ago, and
    /// writing without releasing would leave it unable to write its own. The
    /// outcome comes back through the mailbox ([`Message::HandedOff`]), which
    /// is where it is decided whether the terminal is launched or this stays
    /// as it was.
    ///
    /// **Only the OWNER hands off.** A loose window has no screen to hand
    /// over, and releasing someone else's does nothing: offering it anyway
    /// would promise a handoff that stays half-done, with the terminal open
    /// over someone else's listing.
    pub(super) fn request_handoff(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if !self.session.owner {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-handoff-not-owner".to_owned(),
                },
                Vec::new(),
            );
        }
        let body = self.capture_session_for_handoff();
        let Ok(json) = serde_json::to_value(&body) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-handoff-failed".to_owned(),
                },
                Vec::new(),
            );
        };
        let revision = self.session.revision;
        let backend2 = Arc::clone(backend);
        let mailbox2 = mailbox.clone();
        tokio::spawn(async move {
            // It is only released if the screen WENT IN: releasing after a
            // `put` that failed would leave the terminal claiming an old
            // body, which is worse than not handing off.
            let written = backend2
                .session_put(norte_frontend::session::SCHEMA_VERSION, revision, json)
                .await
                .is_ok();
            let released = if written {
                backend2.session_release().await.unwrap_or(false)
            } else {
                false
            };
            let _ = mailbox2
                .send(Message::HandedOff { released })
                .await;
        });
        (self.applied(), self.say("msg-handoff-running"))
    }

    /// The handoff answered: the screen is handed over, or nothing happened.
    pub(super) fn handoff_finished(&mut self, soltada: bool) -> Vec<BridgeEnvelope<UiUpdate>> {
        if !soltada {
            return self.say("msg-handoff-failed");
        }
        // We are no longer the owner: stopping writing is the honest thing,
        // and the bar's indicator says so on its own.
        self.session.owner = false;
        // And a handoff stays IN PROGRESS until whoever hosts it says whether
        // the terminal opened: that is what authorizes a `HandoffFailed`.
        self.handoff_in_progress = true;
        // Launching the terminal and closing is up to the host. If it
        // cannot, it says so and does NOT close: the session is loose but the
        // screen is still here, which is the cheap failure.
        if !self.native(crate::dto::NativeEffect::HandoffToTerminal { daemon: true }) {
            return self.say("msg-handoff-no-terminal");
        }
        self.say("msg-handoff-running")
    }

    /// The handoff's terminal did not open: this window STAYS, recovers the
    /// session it had released, and says why (phase 9).
    ///
    /// This is decision 4 of ADR 0123 on the window's side: when something
    /// fails, nothing happens and the process stays where it was. The screen
    /// is written in the core, so recovering it means asking for ownership
    /// again — through the usual path, which is the writer's policy: if
    /// another frontend claimed it in the meantime, it keeps it and this
    /// window stays loose, which is the truth.
    pub(super) fn handoff_failed(
        &mut self,
        no_terminal: bool,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // Only if there IS a handoff in progress: anyone who talks to the
        // host can send the action, and without this check it would be
        // enough to send it to paint "the terminal did not start" over a
        // window that had asked for nothing.
        if !std::mem::take(&mut self.handoff_in_progress) {
            return (Self::stale(StaleAction::Modal), Vec::new());
        }
        self.session.policy.ask_soon();
        let key = if no_terminal {
            "msg-handoff-no-terminal"
        } else {
            "msg-handoff-terminal-failed"
        };
        (self.applied(), self.say(key))
    }

    /// The tick's `session.put` answered.
    ///
    /// Four answers, and each says something different: it went in, and what
    /// was sent becomes the last thing written; another window wrote in
    /// between, and it rereads to write over its revision; the body does not
    /// fit, and from now on it goes with no history; this window is no longer
    /// the owner, and the indicator says so. Everything else is noted and
    /// retried on the next tick.
    pub(super) fn session_placed(
        &mut self,
        res: Result<u64, Error>,
        arc_body: std::sync::Arc<norte_frontend::session::SessionBody>,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        self.session.in_flight = None;
        match res {
            Ok(rev) => {
                self.session.revision = rev;
                self.session.policy.sent(arc_body);
                Vec::new()
            }
            Err(Error::Conflict { .. }) => {
                self.session.policy.resend();
                let backend = Arc::clone(backend);
                let mailbox = mailbox.clone();
                // The reread comes back through the mailbox like everything
                // else, and while it is in flight nothing is written: what
                // conflicted stays pending until it is known which revision
                // it is against.
                self.session.in_flight = Some(arc_body);
                tokio::spawn(async move {
                    let res = backend.session_get().await;
                    let _ = mailbox.send(Message::SessionReread(res)).await;
                });
                Vec::new()
            }
            Err(Error::LimitExceeded { .. }) => {
                self.session.policy.resend();
                self.session.no_history = true;
                Vec::new()
            }
            Err(Error::PermissionDenied) => {
                self.session.policy.resend();
                self.session.owner = false;
                let change = self.banner_change();
                vec![self.parche(vec![change])]
            }
            Err(e) => {
                tracing::warn!(error = %e, "the session could not be written; retrying");
                self.session.policy.resend();
                Vec::new()
            }
        }
    }

    /// The session reread after a conflict: its revision is taken and
    /// whatever is someone else's is kept, WITHOUT applying it to the screen
    /// — this window is the one that just moved, and its own goes on top on
    /// the next tick.
    ///
    /// The revision only advances if the body is understood: advancing with a
    /// body that could not be read would write what was PREVIOUSLY READ over
    /// a revision that no longer reflects it, and that clobbers what the
    /// other window just saved. With no body, the next tick conflicts and
    /// rereads again, which is the honest thing. A body from the FUTURE
    /// switches off writing entirely, as on startup.
    pub(super) fn session_reread(
        &mut self,
        res: Result<(norte_proto::methods::Session, bool), Error>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        self.session.in_flight = None;
        let Ok((session, owner)) = res else {
            return Vec::new();
        };
        let was_owner = self.session.owner;
        self.session.owner = owner;
        match norte_frontend::session::SessionBody::from_value(session.version, &session.body) {
            Ok(body) => {
                self.session.revision = session.revision;
                self.session.read = body;
            }
            Err(norte_frontend::session::SessionError::FromTheFuture { .. }) => {
                self.session.future = true;
            }
            Err(e) => {
                tracing::warn!(error = %e, "the reread session is not understood; retrying");
            }
        }
        if was_owner == owner && !self.session.future {
            return Vec::new();
        }
        let change = self.banner_change();
        vec![self.parche(vec![change])]
    }

    /// The slots `[profile.start]` seeds, already filtered to the ones THIS
    /// screen has.
    ///
    /// Who wins is decided by
    /// [`norte_frontend::config::profile_start_seeds`], which belongs to both
    /// frontends: the session rules, and the profile only says where to open
    /// a slot the session knows nothing about. An id the profile names that
    /// this layout does not place has nowhere to open, so it is dropped here.
    ///
    /// It notes what was seeded. Without that count, a reader with no saved
    /// session — a fresh install, or a `session_get` that could not even be
    /// read — went back to the profile's startup directory every time they
    /// entered and left it: for them the session never knows anything, so the
    /// veto above does not veto.
    pub(super) fn profile_seed(&mut self) -> Vec<(u32, VPath)> {
        let seeds: Vec<(u32, VPath)> = norte_frontend::config::profile_start_seeds(
            &self.config.common.profile_start,
            &self.session.known,
            &self.session.seeded,
        )
        .into_iter()
        .filter(|(id, _)| self.slots.contains_key(id))
        .collect();
        for (id, _) in &seeds {
            self.session.seeded.insert(*id);
        }
        // An id the profile names and this layout does not place has nowhere
        // to open. It is SAID, like in the terminal: staying quiet about it
        // is the same kind of silence the whole key used to have before ADR
        // 0098.
        let placed: std::collections::BTreeSet<u32> = self.slots.keys().copied().collect();
        let orphans = norte_frontend::config::profile_start_orphans(
            &self.config.common.profile_start,
            &placed,
        );
        if !orphans.is_empty() {
            let ids: Vec<String> = orphans.iter().map(u32::to_string).collect();
            self.status.message = Some(clamp_display(norte_i18n::ta_in(
                self.lang,
                "msg-profile-start-orphans",
                &[("n", &orphans.len().to_string()), ("ids", &ids.join(", "))],
            )));
        }
        seeds
    }

    /// Returns the ACTIVE panel to the directory written on startup.
    ///
    /// Goes AFTER applying the session, and that is the whole fix: the
    /// session writes every slot's location, so a command-line argument can
    /// only win by being set on top again. A directory someone just typed is
    /// more specific than where it closed yesterday — the same rule that
    /// makes `--layout` win over `[ui] layout`.
    ///
    /// Only the active one: the other panel stays wherever the session left
    /// it. And only the LOCATION — order and hidden state are preferences,
    /// and are not touched.
    pub(super) fn pin_dir_requested(&mut self) {
        let Some(dir) = self.dir_requested.take() else {
            return;
        };
        let active = self.active();
        if let Some(slot) = self.slots.get_mut(&active) {
            slot.pane.begin_loading(dir);
            // The cursor the session saved was a row of ANOTHER directory:
            // applying it over what was typed would put the cursor on a
            // random row. The same rule as `pin_start_dir` in the terminal.
            slot.cursor_to_restore = None;
        }
    }

    /// Places each slot where the session says it was.
    pub(super) fn apply_session(&mut self, body: &norte_frontend::session::SessionBody) {
        // The cap BEFORE seeding (rust-reviewer MAJOR, phase 1): a slot is
        // born with the factory one, and seeding with it clamped a history of
        // 64 the session saved in full down to 30 — and the next write made
        // it permanent.
        let cap = self.config.common.ui_chrome.history_size();
        for (id, slot) in &mut self.slots {
            slot.history.set_capacity(cap);
            let Some(slot_state) = body.slots.get(id) else {
                continue;
            };
            slot.pane.begin_loading(slot_state.path.clone());
            // Order and hidden state USED TO BE written to the session and
            // nobody read them: the window remembered where you were and
            // forgot how you were looking at it, so sorting by size or
            // hiding dotfiles lasted only until it closed.
            slot.pane.set_sort(slot_state.sort.clone());
            slot.pane.set_show_hidden(slot_state.show_hidden);
            // The CURSOR used to be saved and nobody read it: the window went
            // back to the place and to the `..` row. `lands_on` applies it
            // once the rows arrive, like the terminal does in
            // `restore_cursor`.
            slot.cursor_to_restore = usize::try_from(slot_state.cursor).ok();
            slot.history
                .seed(slot_state.back.clone(), slot_state.forward.clone());
            slot.history.seed_jump(slot_state.jump.clone());
            // A HANDOFF's marks (phase 9), and only with `--attach`. They go
            // to `marks_to_restore`, the same mechanism a refresh already
            // uses to keep the selection: `lands_on` consumes it AFTER
            // `set_listing` — which clears what was marked — and through
            // `restore_marks`, which goes through the `..` row's funnel.
            //
            // Through there and not through a path of its own, and that is
            // this fix's lesson: an earlier version seeded them in
            // `land_listing`, and the STARTUP listing does not go
            // through there — it goes through `list_initial` — so they
            // never arrived. `lands_on` is where they all pass through.
            if self.attach && !slot_state.marks.is_empty() {
                slot.marks_to_restore.clone_from(&slot_state.marks);
            }
        }
    }

    /// The CURRENT screen as a session body.
    ///
    /// MARKS do not go in: they are a working selection, not a place you
    /// were, and restoring them would make a new window open with half a
    /// dozen files selected that nobody chose.
    pub(super) fn capture_session(&self) -> norte_frontend::session::SessionBody {
        self.capture_session_with_marks(false)
    }

    /// The same screen WITH what is marked (phase 9): what is flushed for a
    /// handoff between frontends.
    ///
    /// The difference with its sibling is the only one that matters: in a
    /// handoff, seconds pass between releasing and claiming, so returning
    /// what is selected is returning the work that was being done. In an
    /// ordinary startup, hours have passed, and the reasoning above still
    /// holds.
    pub(super) fn capture_session_for_handoff(&self) -> norte_frontend::session::SessionBody {
        self.capture_session_with_marks(true)
    }

    fn capture_session_with_marks(&self, marks: bool) -> norte_frontend::session::SessionBody {
        // It starts from what was READ and only overwrites its own: another
        // frontend's slots and the OTHER profiles' layouts stay there.
        //
        // This window's layout goes under its profile's key, like the
        // terminal's: ADR 0058's D8 — close one frontend, open the other,
        // pick up where you were — and what the reader expects on reopening:
        // the panels they left open. Writing it stopped once, out of fear
        // that browsing the selector would change the terminal's startup,
        // but that IS sharing the screen, and what D5 protects is something
        // else: that a window's SIZE does not rewrite the tree.
        //
        // Since ADR 0139 it goes under the window's OWN key
        // (`<profile>@window`): the terminal and the window each remember
        // their own sizes and positions, and the last one to write no longer
        // clobbers what the other adjusted. On a HANDOFF the shared one is
        // also written: handing over the screen is exactly the terminal
        // opening with this one.
        let mut body = self.session.read.clone();
        body.layouts.insert(
            norte_frontend::session::window_layout_key(&self.session_key()),
            self.tree.clone(),
        );
        if marks {
            body.layouts.insert(self.session_key(), self.tree.clone());
        }
        body.palette_recent.clone_from(&self.palette_recent);
        body.popular = self.popular.entries().to_vec();
        for (id, slot) in &self.slots {
            body.slots.insert(
                *id,
                norte_frontend::session::SlotState {
                    path: slot.pane.dir().clone(),
                    cursor: slot.pane.cursor() as u64,
                    back: slot.history.trail().to_vec(),
                    forward: slot.history.forward_trail().to_vec(),
                    jump: slot.history.jump().cloned(),
                    sort: slot.pane.sort(),
                    columns: Vec::new(),
                    show_hidden: slot.pane.show_hidden(),
                    // The age stamp exactly as it was LAST WRITTEN, not
                    // "now": stamping every capture with the clock meant no
                    // body was ever equal to the previous one and the tick
                    // wrote every second. The policy stamps it when it
                    // prepares the body, and `touched` remembers the stamp. A
                    // slot never stamped goes to zero and the first write
                    // stamps it, which is what the terminal does.
                    touched_ms: self.session.touched.get(id).copied().unwrap_or(0),
                    // By PATH, which is the row's identity: a restored index
                    // over a listing that changed points at a different file.
                    // The cap belongs to the model.
                    marks: if marks {
                        slot.pane
                            .marked_entries()
                            .iter()
                            .take(norte_frontend::session::MARKS_CAP)
                            .map(|e| e.path.clone())
                            .collect()
                    } else {
                        Vec::new()
                    },
                },
            );
        }
        body
    }
}
