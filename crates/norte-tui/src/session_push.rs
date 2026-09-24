//! Persisting and restoring the UI session (#230, #235, #236).
//!
//! It used to live in the root of the `ntc` binary, which is a crate
//! DIFFERENT from this lib: none of this could be imported from `tests/`,
//! so its tests had to be a `#[cfg(test)] mod` inside `main.rs`. Moved as
//! is —same signatures, same order, same comments— with no change beyond
//! the `pub` on what the event loop calls.

use std::sync::Arc;

use norte_core::backend::Backend;
use norte_i18n::{t, ta};
use norte_proto::{Error, VPath};

use crate::app::App;
use crate::listing::initial_pane;

/// Fetches the saved session and puts it on screen (L2).
///
/// Three things and in this order: who owns it is asked, the body is
/// applied, and the slots the session placed are listed —until the listing
/// arrives, the saved cursor has nowhere to land—.
///
/// None of this can prevent startup. A core that knows nothing of sessions,
/// an unreadable session or a directory that no longer exists leave what
/// there was: the configuration's layout, which is what there was before
/// this existed.
///
/// `start` is the command line's DIR when there was one: it wins over the
/// session in the active pane ([`App::pin_start_dir`]), and is listed with
/// the rest.
pub async fn restore_session(
    app: &mut App,
    backend: &Backend,
    start: Option<&VPath>,
    profile_start: &std::collections::BTreeMap<u32, VPath>,
) {
    // Seeding `[profile.start]` goes on EVERY path, including the ones with
    // no session to apply: a fresh install —or a profile copied from
    // another machine— is exactly the case the key exists for, and leaving
    // it behind these `return`s meant it was never seeded (ADR 0098).
    let seed = |app: &mut App| {
        let _ = app.seed_profile_start(profile_start);
    };
    let (session, owned) = match backend.session_get().await {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(error = %e, "no saved session");
            seed(app);
            if let Some(dir) = start {
                app.pin_start_dir(dir.clone());
            }
            return;
        }
    };
    // Detached or owned is a STATE, and the bar's persistent indicator
    // paints it (`App::session_banner`) for as long as it lasts: not a
    // startup message talking about "another window" to whoever just
    // opened the first one. The indicator is discreet, and help says what
    // it means.
    app.session.detached = !owned;
    app.session.revision = session.revision;
    // Revision 0 is "nobody has written it yet": there is nothing to apply
    // and nothing broken to report either.
    if session.revision == 0 {
        seed(app);
        if let Some(dir) = start {
            app.pin_start_dir(dir.clone());
        }
        restore_slots(app, backend, RESTORE_BUDGET).await;
        return;
    }
    // The ENVELOPE's version, which is the one the protocol documents
    // (#247): the body carried an undocumented copy and it was the only one
    // read, so a third-party client that did what the contract says had its
    // body interpreted as if it were version 0.
    app.apply_session_value(session.version, &session.body);
    // The order IS the precedence: the session, then what the profile says
    // about slots it does not know about, and on top of everything the
    // directory a human just typed.
    seed(app);
    if let Some(dir) = start {
        app.pin_start_dir(dir.clone());
    }
    restore_slots(app, backend, RESTORE_BUDGET).await;
}

/// How long the journal can go unused before this session releases it
/// (#179).
///
/// Thirty seconds: enough that a burst of copies does not pay for a close
/// and a reopen in between, and little enough that an `ntc` left open all
/// afternoon does not block `norte daemon run` or `norte audit` beyond the
/// stretch it was truly writing.
pub const JOURNAL_IDLE: std::time::Duration = std::time::Duration::from_secs(30);

/// What startup dedicates WHOLE to listing the session's slots (#235).
///
/// Five seconds for ALL the slots, not five per slot: what is capped is how
/// long the window can take to appear, and that does not depend on how many
/// slots the layout has.
const RESTORE_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

/// Fills the session's slots with a real listing, within `budget` (#235).
///
/// This runs BEFORE the event loop exists: there is no `Ctrl+C` wired up
/// yet, so a slot pointing at a dead SFTP used to hang the whole startup and
/// the only way out was another terminal. Rule 3 is about this exact thing,
/// on a path earlier than the task machinery.
///
/// **The listings run IN PARALLEL under a shared deadline**, and that is
/// what makes the budget startup's and not each slot's. In series, a single
/// panel parked on a downed host ate up the entire five seconds and the
/// other three —local, milliseconds long— were left unlisted for having
/// arrived late to a share that was never theirs: the common shape of the
/// bug (one dead remote among locals) left half the screen blank on every
/// startup for as long as the outage lasted.
///
/// What could not be listed stays on its path and **marked**
/// ([`Pane::unlisted`]): a screen with empty listings and no explanation
/// asserts those directories are empty, which is exactly what is not known.
/// The mark lasts until someone truly lists it, because the state lasts
/// until then.
async fn restore_slots(app: &mut App, backend: &Backend, budget: std::time::Duration) {
    let deadline = tokio::time::Instant::now() + budget;
    // The requests are collected first: the application's `&mut App` cannot
    // live inside the futures.
    let requests: Vec<(norte_frontend::layout::SlotId, VPath, Vec<String>)> = app
        .layout
        .slot_ids()
        .into_iter()
        .filter_map(|id| {
            let dir = app.panes.browser(id).map(|p| p.dir().clone())?;
            let attrs = app.columns.attr_ids_for(dir.scheme());
            Some((id, dir, attrs))
        })
        .collect();
    let listed = futures::future::join_all(requests.into_iter().map(|(id, dir, attrs)| {
        let backend = backend.clone();
        async move {
            let r = tokio::time::timeout_at(deadline, initial_pane(&backend, &dir, &attrs)).await;
            (id, r)
        }
    }))
    .await;

    for (id, listing) in listed {
        let Ok(listing) = listing else {
            tracing::warn!("a session slot did not list within the budget");
            if let Some(p) = app.panes.browser_mut(id) {
                p.unlisted = true;
            }
            continue;
        };
        match listing {
            Ok(pane) => {
                // Order and hidden-files are the SESSION's, not the new
                // listing's: kept when replacing the pane.
                let (sort, hidden) = app
                    .panes
                    .browser(id)
                    .map_or((None, None), |p| (Some(p.sort()), Some(p.show_hidden())));
                // Through the adoption door, which stamps the config's (the
                // `..` row) and restores the session's (sort and hidden) in
                // one single order. Inserted raw, the row fell off on every
                // startup with a saved session.
                app.adoptar_pane(id, pane, sort, hidden);
                app.restore_cursor(id);
            }
            // A directory that is no longer there does NOT leave startup
            // half-done: the pane stays empty at that path and the reader
            // navigates from there, which is the same thing that happens if
            // it is deleted with you inside it.
            //
            // But it is MARKED, and this was fixed: the mark was not for
            // "there wasn't time", it is for "this is not the directory's
            // content". A listing that failed and an empty directory were
            // painted identically, and the common case is not a deleted
            // directory — it is a remote connection that asks for its
            // password on reopening. The reader saw an empty panel over
            // `s3://…` and nothing else: not the reason, not that there was
            // something to do.
            //
            // It is not asked here. Restoring a session is not requesting a
            // connection, and a password asked for before the screen even
            // exists, for something nobody just did, is the shape ADR 0015
            // calls phishing. The question is opened by the first gesture
            // over that panel — `pane.refresh` or navigating.
            Err(e) => {
                tracing::warn!(error = %e, "a session slot could not be listed");
                if let Some(p) = app.panes.browser_mut(id) {
                    p.unlisted = true;
                }
                if let Error::SecretNeeded { conn, .. } = &e {
                    // And it SAYS which one, because with two remote panels
                    // "a password is needed" cannot be answered.
                    app.message = Some(ta(
                        "msg-session-secret-needed",
                        &[("conn", &norte_frontend::display_name(conn.as_bytes()).0)],
                    ));
                }
            }
        }
    }
}

/// How many ticks apart a DETACHED window asks again whether it can write
/// now (#234).
///
/// Thirty seconds. There is no notification to warn it —deliberately none:
/// the only one that can write is the one that changed something, so a
/// `session.changed` would have no correct recipient— and without asking
/// again, a window that outlives the owner never saves anything again and
/// its screen dies with it. Asking every second would be one round trip a
/// second forever in exchange for finding out sooner about something that
/// happens once.
const OWNER_RETRY_TICKS: u32 = 30;

/// How long the wait for the last dump lasts on exit.
///
/// Exiting does not hang over a session: if the core does not answer, the
/// last snapshot is lost and that is that.
const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// What the screen sends the session writer.
enum SessionOrder {
    /// Write this.
    ///
    /// `Arc` because the body carries the tree and every slot's state: the
    /// screen shares it with the writer instead of copying up to 1 MiB to
    /// it once a second, and incidentally the variant does not bloat the
    /// enum (`clippy::large_enum_variant`).
    Write(Arc<norte_frontend::session::SessionBody>),
    /// Can I write yet? A detached window does this every
    /// [`OWNER_RETRY_TICKS`] ticks.
    Ask,
    /// HANDOFF (phase 9): writes this body —which carries the marks— and
    /// then RELEASES the session, so the other frontend can claim it.
    ///
    /// Both things go in a single order and in this order on purpose:
    /// releasing before writing would leave the other one reading the
    /// screen from a second ago, and writing without releasing would leave
    /// it unable to write its own. The writer answers with
    /// [`SessionNotice::HandedOff`], which is what decides whether it
    /// launches into the other one or this stays as it was.
    Handoff(Arc<norte_frontend::session::SessionBody>),
}

/// What the writer tells the screen.
enum SessionNotice {
    /// It did not fit; history has been dropped. Stated ONCE.
    TooLarge,
    /// The body did not go through because another window wrote first.
    ///
    /// `orphans` are the slots IT had saved that this screen did not have:
    /// they come back here instead of being dropped, because the only path
    /// that reaches this notice is an ownership handoff, i.e. exactly when
    /// what is saved is NOT ours (#231).
    Retry {
        /// The other window's slots that had to be kept.
        orphans: std::collections::BTreeMap<u32, norte_frontend::session::SlotState>,
    },
    /// This window is now the owner: it can write again starting from the
    /// next revision.
    Owner {
        /// The revision in effect at the moment of taking it.
        revision: u64,
        /// The slots the previous owner had saved that this screen does not
        /// know about.
        ///
        /// A handoff does NOT go through `Conflict` —the revision adopted is
        /// exactly the current one, so the next write fits— and that was the
        /// gap: the window taking over the handoff overwrote, on its first
        /// tick, everything the other one had saved while this one ran
        /// detached.
        orphans: std::collections::BTreeMap<u32, norte_frontend::session::SlotState>,
    },
    /// This window has STOPPED being the owner: another one has it, or the
    /// daemon serving it left and the new connection claimed nothing.
    ///
    /// Without this notice, the writer used to shut itself down and nobody
    /// asked again: after a daemon handoff the window stopped saving for the
    /// rest of its life, believing itself the owner and without saying a
    /// word.
    Released,
    /// The HANDOFF (phase 9) finished: the screen is written and has been
    /// released —or it could not be—.
    ///
    /// **`false` is NOT ignored**: it means the session still has an owner,
    /// and launching the other frontend then would open a blank window over
    /// a screen nobody released. With `false` the handoff does not happen
    /// and says so; the process stays where it was, which is the worst that
    /// can happen and is not bad.
    HandedOff {
        /// This connection was the owner and has stopped being it.
        released: bool,
    },
}

/// The SCREEN side of the session writer (L2).
///
/// What used to be an `async` function inside the `select!` is now a
/// channel, and the reason is measurable: the dump ends in an `fsync`
/// (embedded arm) or a trip over the socket (daemon), and while that was in
/// flight the event loop did not process a key. Once a second, and right
/// while you navigate, which is when the body changes. Now the loop only
/// does `try_send` and `try_recv`: **this struct does not have a single
/// `await`, and that is why the fix cannot be undone without it showing**
/// (#230).
pub struct SessionPush {
    /// What is sent, what is not repeated and when ownership is asked for
    /// again. Lives in `norte-frontend` (#236): it is session policy, not
    /// the TUI's event loop's, and the next frontend inherits it instead of
    /// reinventing it.
    policy: norte_frontend::session::PushPolicy,
    /// Toward the writer. Capacity 1: if it is busy, this tick is skipped,
    /// which is coalescing and not loss —the next body carries the same and
    /// more—.
    commands: tokio::sync::mpsc::Sender<SessionOrder>,
    /// From the writer.
    notices: tokio::sync::mpsc::Receiver<SessionNotice>,
    /// The writer, to await it on exit.
    task: Option<tokio::task::JoinHandle<()>>,
}

impl SessionPush {
    /// The screen side WITHOUT a writer, to test what the loop decides with
    /// no core on the other side.
    ///
    /// Returns the two ends the real writer keeps, so a test can read what
    /// is sent and fake what is answered.
    #[cfg(test)]
    fn for_test() -> (
        Self,
        tokio::sync::mpsc::Receiver<SessionOrder>,
        tokio::sync::mpsc::Sender<SessionNotice>,
    ) {
        let (commands_tx, commands_rx) = tokio::sync::mpsc::channel(1);
        let (notices_tx, notices_rx) = tokio::sync::mpsc::channel(4);
        (
            Self {
                policy: norte_frontend::session::PushPolicy::new(OWNER_RETRY_TICKS),
                commands: commands_tx,
                notices: notices_rx,
                task: None,
            },
            commands_rx,
            notices_tx,
        )
    }

    /// Starts this run's session writer.
    pub fn start(backend: &Backend, revision: u64) -> Self {
        let (commands_tx, commands_rx) = tokio::sync::mpsc::channel(1);
        let (notices_tx, notices_rx) = tokio::sync::mpsc::channel(4);
        let b = backend.clone();
        let task = tokio::spawn(write_session(b, revision, commands_rx, notices_tx));
        Self {
            policy: norte_frontend::session::PushPolicy::new(OWNER_RETRY_TICKS),
            commands: commands_tx,
            notices: notices_rx,
            task: Some(task),
        }
    }

    /// Sends the last snapshot, releases the channel and awaits the writer.
    ///
    /// The snapshot goes with `send` and a deadline, not with `try_send`: on
    /// exit there is no "next tick" to retry it, so with the writer busy —a
    /// slow `fsync`, a stalled daemon— a `try_send` would have dropped
    /// exactly the write this path exists to not lose.
    pub async fn close(&mut self, last: Option<Arc<norte_frontend::session::SessionBody>>) {
        if let Some(body) = last {
            let _ = tokio::time::timeout(
                SHUTDOWN_GRACE,
                self.commands.send(SessionOrder::Write(body)),
            )
            .await;
        }
        let (empty, _) = tokio::sync::mpsc::channel(1);
        // Releasing the sender is what ends the writer's loop.
        self.commands = empty;
        if let Some(task) = self.task.take() {
            let _ = tokio::time::timeout(SHUTDOWN_GRACE, task).await;
        }
    }
}

/// The session writer: the ONLY one that talks to the core about this.
///
/// It holds the revision, the trim and the pause because it is the one that
/// sees the responses. The two negatives are answered differently, and
/// that is why they are here and not in the `Backend`: a conflict is fixed
/// by rereading —another window wrote— and an oversize is fixed by
/// dropping history, which takes up the most space and hurts the least to
/// lose.
async fn write_session(
    backend: Backend,
    mut revision: u64,
    mut commands: tokio::sync::mpsc::Receiver<SessionOrder>,
    notices: tokio::sync::mpsc::Sender<SessionNotice>,
) {
    use norte_frontend::session::{SCHEMA_VERSION, SessionBody};

    // Already known it does not fit: from here on it writes WITHOUT
    // history. Trimming just one tick's copy was trimming nothing — the
    // next tick captured the whole history again and what came out was a
    // refused `put` and a notice ONCE A SECOND.
    let mut truncating = false;
    // Did not fit even without history: this run stops writing.
    let mut stopped = false;
    // The last thing sent to write, to know what of a foreign document we
    // did not have.
    let mut last: Option<SessionBody> = None;
    // Phase 9: the current order is a HANDOFF, so after writing it the
    // session has to be released. Remembered here and not done in the
    // `match` arm because writing comes next, and releasing before that
    // would leave the other frontend reading the screen from a second ago.
    while let Some(order) = commands.recv().await {
        let handing_off = matches!(order, SessionOrder::Handoff(_));
        let mut body = match order {
            SessionOrder::Ask => {
                if let Some(rev) = ask_si_ya_es_mia(&backend, last.as_ref(), &notices).await {
                    revision = rev;
                    stopped = false;
                }
                continue;
            }
            SessionOrder::Write(body) | SessionOrder::Handoff(body) => (*body).clone(),
        };
        if stopped {
            // With writing stopped there is no screen to deliver, and
            // releasing anyway would leave the session with no owner and an
            // old body inside. It answers that it could not, which is the
            // truth.
            if handing_off {
                let _ = notices
                    .send(SessionNotice::HandedOff { released: false })
                    .await;
            }
            continue;
        }
        if truncating {
            body.degrade_for_size();
        }
        // Phase 9: if this is a handoff, the session is only released once
        // the screen IS written. Releasing it after a `put` that failed
        // would leave the other frontend claiming an old body, which is
        // worse than not handing off.
        let mut written = false;
        match backend
            .session_put(SCHEMA_VERSION, revision, body.to_value())
            .await
        {
            Ok(rev) => {
                revision = rev;
                last = Some(body);
                written = true;
            }
            // Another window wrote between our last `get` and this `put`.
            // It rereads to know against what, and what it had saved that
            // this screen does not have is kept via TWO paths: it goes into
            // the body being retried right now —if this is the last dump,
            // there is no "later"— and it is handed back to the screen,
            // which is the one that has to carry it in the next ones
            // (#231).
            Err(Error::Conflict { .. }) => {
                let mut orphans = std::collections::BTreeMap::new();
                if let Ok((session, _)) = backend.session_get().await {
                    revision = session.revision;
                    if let Ok(remote) = SessionBody::from_value(session.version, &session.body) {
                        orphans = foreign_orphans(&body, &remote);
                        for (id, state) in &orphans {
                            body.slots.insert(*id, state.clone());
                        }
                    }
                    // ONE IMMEDIATE retry and only one: with the real
                    // revision in hand, not retrying here would leave an
                    // exit's last snapshot hanging.
                    if let Ok(rev) = backend
                        .session_put(SCHEMA_VERSION, revision, body.to_value())
                        .await
                    {
                        revision = rev;
                        last = Some(body);
                        written = true;
                    }
                }
                let _ = notices.send(SessionNotice::Retry { orphans }).await;
            }
            Err(Error::LimitExceeded { .. }) => {
                if truncating {
                    // Does not fit even without history: retrying every
                    // second would be an error a second.
                    stopped = true;
                } else {
                    truncating = true;
                    // The decision of WHAT gets dropped is shared (#316):
                    // the window degrades with the same one, and before it
                    // did not degrade.
                    body.degrade_for_size();
                    let _ = notices.send(SessionNotice::TooLarge).await;
                    match backend
                        .session_put(SCHEMA_VERSION, revision, body.to_value())
                        .await
                    {
                        Ok(rev) => {
                            revision = rev;
                            written = true;
                        }
                        Err(_) => stopped = true,
                    }
                }
            }
            // This window no longer writes: it lost ownership, or the
            // daemon is shutting down (or left and the new connection
            // claimed nothing). It stops AND SAYS SO: without the notice
            // nobody ever asked again —`Ask` only comes from a window that
            // knows it is detached— and the screen was silently lost after
            // any daemon handoff.
            Err(Error::PermissionDenied | Error::Cancelled) => {
                stopped = true;
                let _ = notices.send(SessionNotice::Released).await;
            }
            // Any other failure —transport down, an `Io`, a timeout— does
            // NOT count the body as written: the screen considered it sent
            // once it put it into the channel, so without this it was lost
            // until the reader moved something again.
            Err(e) => {
                tracing::debug!(error = %e, "session could not be written");
                let _ = notices
                    .send(SessionNotice::Retry {
                        orphans: std::collections::BTreeMap::new(),
                    })
                    .await;
            }
        }
        // And the handoff, AFTER writing.
        if handing_off {
            stopped |= release_for_handoff(&backend, written, &notices).await;
        }
    }
}

/// `SessionOrder::Ask`: is it this window's yet? Returns the current
/// revision if it is, and notifies the screen.
///
/// The document that was there travels back with the notice: what the
/// previous session owner saved that this screen does not know about would
/// be lost on the handoff's first dump, and that gap was a panel's history
/// its owner was going to come back to (#231).
async fn ask_si_ya_es_mia(
    backend: &Backend,
    last: Option<&norte_frontend::session::SessionBody>,
    notices: &tokio::sync::mpsc::Sender<SessionNotice>,
) -> Option<u64> {
    use norte_frontend::session::SessionBody;

    let (session, owned) = backend.session_get().await.ok()?;
    if !owned {
        return None;
    }
    let revision = session.revision;
    let orphans = SessionBody::from_value(session.version, &session.body)
        .map(|remote| foreign_orphans(last.unwrap_or(&SessionBody::default()), &remote))
        .unwrap_or_default();
    let _ = notices
        .send(SessionNotice::Owner { revision, orphans })
        .await;
    Some(revision)
}

/// The second half of a handoff: release the session and report it.
/// Returns whether writing must stop.
///
/// Comes AFTER writing because the screen has to be where the other
/// frontend is going to read it before ceding the spot. And it is only
/// released if the `put` went through: releasing after a failure would
/// leave the other one claiming an old body, which is worse than not
/// handing off. A `release` answering `false` —we were not the owner—
/// leaves the handoff undone, and whoever asked for it finds out: launching
/// the other half then would open a blank window.
async fn release_for_handoff(
    backend: &Backend,
    written: bool,
    notices: &tokio::sync::mpsc::Sender<SessionNotice>,
) -> bool {
    let released = written && backend.session_release().await.unwrap_or(false);
    let _ = notices.send(SessionNotice::HandedOff { released }).await;
    // We are no longer the owner: stopping writing is the honest thing, and
    // the screen goes detached on receiving the notice.
    released
}

/// The slots `remote` has saved that `local` does not have.
///
/// This is what has to be kept from a foreign body: our slots are the good
/// ones —this screen is the one that just moved— but the ones that only
/// exist in its own are known to nobody else, and dropping them is dropping
/// the history of a panel its owner was going to come back to.
fn foreign_orphans(
    local: &norte_frontend::session::SessionBody,
    remote: &norte_frontend::session::SessionBody,
) -> std::collections::BTreeMap<u32, norte_frontend::session::SlotState> {
    remote
        .slots
        .iter()
        .filter(|(id, _)| !local.slots.contains_key(*id))
        .map(|(id, s)| (*id, s.clone()))
        .collect()
}

/// Sends the session to be written if it has changed, and handles whatever
/// the writer has to say (L2).
///
/// Called once a second. Coalescing is the point: the cursor moves on every
/// arrow key, and this ends up in a file.
///
/// **It is not `async`, and that is #230's fix.** Everything that can take
/// a while —the `put`, the `fsync`, the trip over the socket— lives in
/// `write_session` (private); here it only captures, compares and pushes
/// through a channel. Putting an `await` back into this function is going
/// back to blocking the event loop once a second.
pub fn push_session(app: &mut App, st: &mut SessionPush) {
    drain_notices(app, st);
    // The decision —detached, covered by a modal, or time to capture— is
    // the policy's; the channel plumbing is here.
    match st.policy.tick(app.session.detached, app.modal.is_some()) {
        norte_frontend::session::PushStep::Skip => return,
        norte_frontend::session::PushStep::Ask => {
            let _ = st.commands.try_send(SessionOrder::Ask);
            return;
        }
        norte_frontend::session::PushStep::Capture => {}
    }
    let Some(body) = capture_session(app, st) else {
        return;
    };
    // `try_send` and not `send`: with the writer busy, this tick is skipped
    // and the next one sends a newer body. And `last` is only updated if it
    // was truly sent, or a skipped body would be counted as written.
    match st.commands.try_send(SessionOrder::Write(Arc::clone(&body))) {
        Ok(()) => st.policy.sent(body),
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {}
        // The writer died (a panic inside the task). Without this the
        // screen kept retrying against a closed channel for the rest of the
        // run without saying a word.
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
            tracing::warn!("the session writer is gone: this window stops saving");
            app.session.detached = true;
        }
    }
}

/// Requests the HANDOFF (phase 9): dumps the screen WITH the marks and
/// releases the session.
///
/// Returns `false` if the writer could not even receive the order, which
/// is the only failure visible from here: the rest arrives through the
/// `HandedOff` notice, because writing and releasing are two trips to the
/// core and the event loop does not wait for either (#230).
///
/// NO blocking `send`: this is the event loop. With the writer busy it says
/// no instead of freezing the screen, and the reader presses it again —
/// which is correct for a gesture the human just requested and can repeat.
pub fn request_handoff(app: &mut App, st: &mut SessionPush) -> bool {
    let body = Arc::new(app.session_body_for_handoff());
    if st.commands.try_send(SessionOrder::Handoff(body)).is_err() {
        app.message = Some(t("msg-handoff-failed"));
        return false;
    }
    true
}

/// Asks for the session again on the next tick (phase 9): the handoff
/// released it for a window that never got to live.
///
/// Through the usual path —`Ask`, which the writer already knows how to
/// answer— and not with a `session.get` here: this is the event loop
/// (#230), and ownership is managed by the writer. If another frontend
/// claimed it in the meantime, `Ask` answers no and this terminal stays
/// detached, which is the truth.
pub fn reclaim_soon(st: &mut SessionPush) {
    st.policy.ask_soon();
}

/// What the writer has reported since the last round.
pub fn drain_notices(app: &mut App, st: &mut SessionPush) {
    while let Ok(notice) = st.notices.try_recv() {
        match notice {
            SessionNotice::TooLarge => app.message = Some(t("msg-session-too-large")),
            SessionNotice::Retry { orphans } => {
                app.adopt_session_orphans(orphans);
                // What was sent did not go through: the comparison must not
                // count it as written.
                st.policy.resend();
            }
            // Neither taking the session nor releasing it is announced: the
            // bar appears and disappears on its own, and a message for
            // every daemon handoff was noise over a fact that is already
            // visible.
            SessionNotice::Owner { revision, orphans } => {
                app.session.detached = false;
                app.session.revision = revision;
                app.adopt_session_orphans(orphans);
            }
            // Phase 9: the handoff finished. With `released` the screen now
            // belongs to another process and this one leaves; without it,
            // nothing happened and it SAYS SO — staying quiet would leave
            // the reader waiting for a window that is not going to open.
            SessionNotice::HandedOff { released } => {
                if released {
                    app.session.detached = true;
                    app.handoff_ready = true;
                } else {
                    app.message = Some(t("msg-handoff-failed"));
                }
            }
            SessionNotice::Released => {
                app.session.detached = true;
                // It asks again on the next tick and not within thirty
                // seconds: this is usually a daemon handoff, and the
                // session is already free.
                st.policy.ask_soon();
            }
        }
    }
}

/// The screen as it is NOW, if it has changed since the last thing sent.
///
/// `Arc` and not a cloned `Box`: the body can reach 1 MiB and this runs in
/// the event loop once a second. Sharing it with the writer costs nothing;
/// copying it does.
pub fn capture_session(
    app: &mut App,
    st: &mut SessionPush,
) -> Option<Arc<norte_frontend::session::SessionBody>> {
    let now = now_ms();
    let mut body = app.session_body();
    let alive = app.layout.slot_ids();
    // The policy trims, compares and seals. What is left here is the one
    // thing it cannot do: carrying the same seal over to the screen's
    // state, because capturing must not MUTATE —two captures in a row of
    // the same screen have to give the same document, or a second's
    // coalescing does not coalesce anything.
    let sealed = st.policy.prepare(&mut body, &alive, now)?;
    for id in sealed {
        app.touch_session_slot(id, now);
    }
    Some(Arc::new(body))
}

/// Now, in milliseconds since the epoch. Zero if the system clock is before
/// 1970, which only makes the age-based sweep sweep nothing.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}
#[cfg(test)]
mod session_push_tests {
    use super::*;
    use crate::app::Pane;

    fn app() -> App {
        let d = VPath::parse("file:///x").expect("test wire");
        App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()))
    }

    fn slot(path: &str) -> norte_frontend::session::SlotState {
        norte_frontend::session::SlotState {
            path: VPath::parse(path).expect("test wire"),
            cursor: 0,
            back: Vec::new(),
            forward: Vec::new(),
            jump: None,
            sort: norte_frontend::SortSpec::default(),
            columns: Vec::new(),
            show_hidden: false,
            touched_ms: 7,
            marks: Vec::new(),
        }
    }

    /// #235: restoring the session used to list ALL the slots in series,
    /// with no deadline and BEFORE the event loop existed — so a slot
    /// pointing at a dead SFTP hung startup with `Ctrl+C` still not wired
    /// up, and the only way out was another terminal.
    ///
    /// Paused tokio clock: the provider's latency and the deadline are the
    /// same virtual clock, so this is deterministic and does not sleep.
    #[tokio::test(start_paused = true)]
    async fn restoring_the_session_cannot_hang_startup() {
        use norte_core::backend::Backend;
        use std::sync::Arc;
        use std::time::Duration;

        let mem = norte_testkit::MemProvider::new();
        mem.faults()
            .set_latency_per_op(Some(Duration::from_hours(1)));
        let engine = norte_core::Engine::new();
        engine.register_provider(Arc::new(mem));
        let backend = Backend::Embedded(Arc::new(engine));

        let d = VPath::parse("mem:///").expect("test wire");
        let mut app = App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()));
        // The expected set is computed the way the code computes it, not
        // by hand: if the factory layout gains a browser, this keeps
        // telling the truth instead of failing over a hardcoded count.
        let browsers: Vec<_> = app
            .layout
            .slot_ids()
            .into_iter()
            .filter(|id| app.panes.browser(*id).is_some())
            .collect();
        assert!(browsers.len() >= 2, "the factory one has at least two");

        let budget = Duration::from_millis(50);
        let t0 = tokio::time::Instant::now();
        super::restore_slots(&mut app, &backend, budget).await;

        assert!(
            t0.elapsed() < Duration::from_secs(1),
            "startup is capped by the budget, not the provider's latency: {:?}",
            t0.elapsed()
        );
        // And the deadline is SHARED: in series, the first slot ate up all
        // of it and the rest were not even tried. All of them have to end
        // up marked.
        for id in browsers {
            assert!(
                app.panes.browser(id).is_some_and(|p| p.unlisted),
                "slot {id:?} stays marked, not faking an empty dir"
            );
        }
    }

    /// A slot that fails to restore does NOT fake an empty directory, and
    /// if what is missing is a password it SAYS SO by naming the
    /// connection.
    ///
    /// This is the common case on reopening norte: the previous daemon shut
    /// down from inactivity and took the session secret with it —it lives
    /// only in its memory, ADR 0015—, so the panel saved over `s3://…`
    /// comes back with `SecretNeeded`. Before: a `warn!` to the file and an
    /// empty panel, indistinguishable from a bucket with no objects.
    /// Neither the reason nor anything to do.
    ///
    /// It is NOT ASKED here, and it is deliberate: restoring a session is
    /// not requesting a connection. The question is opened by the first
    /// gesture over that panel.
    #[tokio::test]
    async fn a_slot_that_asks_for_a_secret_on_restore_says_so_and_does_not_fake_empty() {
        use norte_core::backend::Backend;
        use std::sync::Arc;

        /// Provider that only knows how to ask for `rosetta`'s password.
        struct AsksSecret;

        #[async_trait::async_trait]
        impl norte_vfs::Provider for AsksSecret {
            fn scheme(&self) -> &'static str {
                "mem"
            }
            fn capabilities(&self) -> norte_proto::Capabilities {
                norte_proto::Capabilities {
                    flags: norte_proto::CapabilityFlags::empty(),
                    max_path: None,
                }
            }
            async fn stat(&self, _p: &VPath) -> Result<norte_proto::Entry, Error> {
                Err(Error::SecretNeeded {
                    conn: "rosetta".to_owned(),
                    endpoint: "s3://cubo.example".to_owned(),
                })
            }
            async fn list(&self, _p: &VPath) -> Result<norte_vfs::EntryStream, Error> {
                Err(Error::SecretNeeded {
                    conn: "rosetta".to_owned(),
                    endpoint: "s3://cubo.example".to_owned(),
                })
            }
            async fn read(
                &self,
                _p: &VPath,
                _r: Option<norte_proto::ByteRange>,
            ) -> Result<norte_vfs::ByteStream, Error> {
                Err(Error::Unsupported)
            }
            async fn write(&self, _p: &VPath) -> Result<Box<dyn norte_vfs::ByteSink>, Error> {
                Err(Error::Unsupported)
            }
            async fn mkdir(&self, _p: &VPath) -> Result<(), Error> {
                Err(Error::Unsupported)
            }
            async fn remove(&self, _p: &VPath) -> Result<(), Error> {
                Err(Error::Unsupported)
            }
            async fn rename(&self, _f: &VPath, _t: &VPath) -> Result<(), Error> {
                Err(Error::Unsupported)
            }
        }

        let engine = norte_core::Engine::new();
        engine.register_provider(Arc::new(AsksSecret));
        let backend = Backend::Embedded(Arc::new(engine));
        let d = VPath::parse("mem:///").expect("test wire");
        let mut app = App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()));

        super::restore_slots(&mut app, &backend, std::time::Duration::from_secs(5)).await;

        let ids: Vec<_> = app
            .layout
            .slot_ids()
            .into_iter()
            .filter(|id| app.panes.browser(*id).is_some())
            .collect();
        for id in ids {
            assert!(
                app.panes.browser(id).is_some_and(|p| p.unlisted),
                "slot {id:?} fakes an empty directory: a listing that \
                 failed and a bucket with no objects read the same"
            );
        }
        let msg = app.message.clone().expect("says what is missing");
        assert!(
            msg.contains("rosetta"),
            "and WHICH connection: with two remote panels, \"a password is \
             needed\" cannot be answered. It said: {msg}"
        );
        // And NO dialog was opened: restoring is not requesting a
        // connection.
        assert!(
            app.modal.is_none(),
            "startup does not ask on its own; the first gesture does"
        );
    }

    /// And the mark TURNS OFF as soon as someone truly lists: it is a
    /// state, not a notice, so a key does not clear it and it does not
    /// survive the listing.
    #[test]
    fn the_unlisted_mark_goes_away_with_the_first_listing() {
        let d = VPath::parse("mem:///").expect("test wire");
        let mut p = Pane::new(d.clone(), Vec::new());
        p.unlisted = true;
        p.set_listing(d, Vec::new());
        assert!(!p.unlisted, "a real listing turns it off");
    }

    /// **#230, and this is a SHAPE test**: `push_session` is called from an
    /// ordinary `#[test]`, with no runtime and no `await`. If someone gives
    /// it back its `async`, this stops compiling — which is exactly the
    /// guarantee that was wanted, because the cost of that `await` was one
    /// lost key per second while navigating, and no assert shows that.
    #[test]
    fn sending_the_session_does_not_block_the_loop() {
        let mut app = app();
        let (mut st, mut commands, _notices) = SessionPush::for_test();
        push_session(&mut app, &mut st);
        assert!(
            matches!(commands.try_recv(), Ok(SessionOrder::Write(_))),
            "the first round sends the screen"
        );
        // And the same thing is not sent twice: coalescing is the whole
        // point of this.
        push_session(&mut app, &mut st);
        assert!(commands.try_recv().is_err(), "nothing has changed");
    }

    /// A DETACHED window does not write, but it asks again (#234): the
    /// owner may have closed, and there is no notification to report it.
    #[test]
    fn a_detached_window_does_not_write_and_asks_again() {
        let mut app = app();
        app.session.detached = true;
        let (mut st, mut commands, _notices) = SessionPush::for_test();
        for _ in 0..OWNER_RETRY_TICKS - 1 {
            push_session(&mut app, &mut st);
            assert!(
                commands.try_recv().is_err(),
                "detached neither writes nor asks"
            );
        }
        push_session(&mut app, &mut st);
        assert!(
            matches!(commands.try_recv(), Ok(SessionOrder::Ask)),
            "at {OWNER_RETRY_TICKS} ticks it asks"
        );
    }

    /// Releasing the session and taking it back do NOT write to the
    /// message bar: they are a state, the persistent indicator paints it,
    /// and a message per handoff was noise over something already visible.
    #[test]
    fn dropping_and_taking_the_session_leave_no_message() {
        let mut app = app();
        let (mut st, _commands, notices) = SessionPush::for_test();
        notices.try_send(SessionNotice::Released).expect("fits");
        push_session(&mut app, &mut st);
        assert!(app.session.detached, "detached");
        assert!(app.message.is_none(), "no message: {:?}", app.message);
        assert!(
            app.session_banner().is_some(),
            "the persistent indicator says so"
        );

        notices
            .try_send(SessionNotice::Owner {
                revision: 3,
                orphans: std::collections::BTreeMap::new(),
            })
            .expect("fits");
        push_session(&mut app, &mut st);
        assert!(!app.session.detached, "owner again");
        assert!(app.message.is_none(), "and neither here: {:?}", app.message);
        assert!(
            app.session_banner().is_none(),
            "the indicator left on its own"
        );
    }

    /// And when the writer says it is already the owner, this window
    /// starts writing again from whatever revision it is given.
    #[test]
    fn taking_ownership_writes_again() {
        let mut app = app();
        app.session.detached = true;
        let (mut st, mut commands, notices) = SessionPush::for_test();
        notices
            .try_send(SessionNotice::Owner {
                revision: 9,
                orphans: std::collections::BTreeMap::new(),
            })
            .expect("fits");
        push_session(&mut app, &mut st);
        assert!(!app.session.detached);
        assert_eq!(app.session.revision, 9);
        assert!(
            matches!(commands.try_recv(), Ok(SessionOrder::Write(_))),
            "and it writes now"
        );
    }

    /// A body that did not arrive is NOT counted as written: without this,
    /// a single round's conflict left the screen unsaved until the reader
    /// moved something again.
    #[test]
    fn what_did_not_arrive_is_sent_again() {
        let mut app = app();
        let (mut st, mut commands, notices) = SessionPush::for_test();
        push_session(&mut app, &mut st);
        assert!(commands.try_recv().is_ok());
        notices
            .try_send(SessionNotice::Retry {
                orphans: std::collections::BTreeMap::new(),
            })
            .expect("fits");
        push_session(&mut app, &mut st);
        assert!(
            matches!(commands.try_recv(), Ok(SessionOrder::Write(_))),
            "it is sent again even though the screen has not changed"
        );
    }

    /// **The last snapshot on exit WAITS its turn.**
    ///
    /// The channel has capacity 1 and on exit there is no next tick, so
    /// sending it with `try_send` dropped it right when the writer was busy
    /// —a slow `fsync`, a stalled daemon—, which is the case it was added
    /// for.
    #[tokio::test]
    async fn the_last_snapshot_on_exit_waits_its_turn() {
        let mut app = app();
        let (mut st, mut commands, _notices) = SessionPush::for_test();
        // The writer is busy: the channel already carries an unconsumed
        // order.
        st.commands.try_send(SessionOrder::Ask).expect("fits one");
        let last = capture_session(&mut app, &mut st).expect("there is a screen to save");
        let received = tokio::spawn(async move {
            let mut v = Vec::new();
            while let Some(o) = commands.recv().await {
                v.push(o);
            }
            v
        });
        st.close(Some(last)).await;
        let v = received.await.expect("join");
        assert_eq!(
            v.len(),
            2,
            "the one occupying the channel and the last snapshot"
        );
        assert!(matches!(v[1], SessionOrder::Write(_)));
    }

    /// Losing ownership mid-life shows in the bar's indicator, and it asks
    /// again on the next tick.
    ///
    /// This is what happens after a daemon handoff: the new connection has
    /// claimed nothing, the `put` comes back `PermissionDenied` and the
    /// writer shuts down.
    /// Without the state, the window believed itself the owner and never
    /// saved again for the rest of its life — nor did it show it.
    #[test]
    fn losing_ownership_is_reported_and_asked_again() {
        let mut app = app();
        let (mut st, mut commands, notices) = SessionPush::for_test();
        notices.try_send(SessionNotice::Released).expect("fits");
        push_session(&mut app, &mut st);
        assert!(app.session.detached, "this window no longer rules");
        assert!(app.session_banner().is_some(), "and the indicator shows it");
        // And on the next tick it asks, without waiting the thirty seconds.
        push_session(&mut app, &mut st);
        assert!(matches!(commands.try_recv(), Ok(SessionOrder::Ask)));
    }

    /// **The handoff keeps what the one who left had saved.**
    ///
    /// A handoff does not go through `Conflict` —exactly the current
    /// revision is adopted, so the next write fits—, and that was the gap:
    /// the window taking over the session overwrote, on its first dump,
    /// everything the other one had saved while this one ran detached.
    #[test]
    fn taking_the_handoff_does_not_overwrite_what_the_other_saved() {
        let mut app = app();
        app.session.detached = true;
        let (mut st, _commands, notices) = SessionPush::for_test();
        let mut orphans = std::collections::BTreeMap::new();
        orphans.insert(77, slot("file:///lo-suyo"));
        notices
            .try_send(SessionNotice::Owner {
                revision: 5,
                orphans,
            })
            .expect("fits");
        push_session(&mut app, &mut st);
        assert!(!app.session.detached);
        assert_eq!(app.session.revision, 5);
        assert_eq!(
            app.session_body().slots[&77].path,
            VPath::parse("file:///lo-suyo").expect("wire"),
            "the other window's is still there and gets written again"
        );
    }

    /// A dead writer does not leave the screen talking to itself: saving
    /// stops and the bar's indicator shows it.
    #[test]
    fn if_the_writer_dies_the_screen_finds_out() {
        let mut app = app();
        let (mut st, commands, _notices) = SessionPush::for_test();
        drop(commands);
        push_session(&mut app, &mut st);
        assert!(app.session.detached);
        assert!(app.session_banner().is_some());
    }

    /// **#231**: from a foreign body, what only existed in it is kept. The
    /// slots the LIVE layout has are ours —this screen is the one that just
    /// moved—; the rest go back to the orphans corner.
    #[test]
    fn from_a_conflict_the_other_slots_are_kept() {
        let mut local = norte_frontend::session::SessionBody::default();
        local.slots.insert(1, slot("file:///mio"));
        let mut remote = norte_frontend::session::SessionBody::default();
        remote.slots.insert(1, slot("file:///suyo"));
        remote.slots.insert(42, slot("file:///solo-suyo"));

        let foreign = foreign_orphans(&local, &remote);
        assert_eq!(foreign.len(), 1, "only what we did not have");
        assert!(foreign.contains_key(&42));

        let mut app = app();
        let alive = app.panes.slot_of(0).0;
        let mut with_alive = foreign.clone();
        with_alive.insert(alive, slot("file:///no-pises-mi-pantalla"));
        app.adopt_session_orphans(with_alive);
        let body = app.session_body();
        assert_eq!(
            body.slots[&42].path,
            VPath::parse("file:///solo-suyo").expect("wire"),
            "the foreign orphan is kept and gets written again"
        );
        assert_eq!(
            body.slots[&alive].path,
            VPath::parse("file:///x").expect("wire"),
            "and a LIVE slot is not overwritten by another window's session"
        );
    }
}
