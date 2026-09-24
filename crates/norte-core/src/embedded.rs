//! The journal of the EMBEDDED process (#167), opened on the FIRST mutation
//! (#177).
//!
//! Hard rule 4 —"every mutation goes through the journal"— was asserted in
//! `CLAUDE.md` and was honored in a single method: `sync.apply` requires a
//! journal and refuses without one, while copy, move, delete, rename and bury
//! over the embedded transport recorded nothing. This module is the other
//! half: the TUI and the daemon-less CLI use THE journal of the state
//! directory, the same one the daemon would open.
//!
//! # A single writer, and who holds it
//!
//! The journal's hash chain assumes a single owner (spec §4, ADR 0024), and
//! what enforces it is not a convention: [`crate::journal::Journal::open`] opens
//! `SQLite` with `locking_mode=EXCLUSIVE`. A second process —another embedded
//! TUI, or the running daemon— loses the race when opening, with
//! `database is locked`.
//!
//! That case does NOT stop startup. A `norte cp` that stops working because a
//! daemon is alive would be worse than the missing record this fixes: it
//! keeps going without a journal, it warns, and the TUI already dims what
//! requires a journal (`pane.sync-dirs`) with its reason visible.
//!
//! # Why LAZY
//!
//! Opening it at startup made the owner of the file a process that might
//! never write a single row in its whole life, and the owner holds it WHILE
//! ALIVE: a browsing `ntc` prevented the daemon from starting —and with it
//! `norte mcp serve`, i.e. agent governance— and denied `norte audit` its read
//! (#177). With [`LazyJournal`] the lock is taken on the first mutation, which
//! is the first instant it is needed and the only one where the nuisance is
//! justified. A corollary that orders the rest of the module: the definition
//! of "this process wants the journal" stopped being a list of subcommands
//! that had to be kept by hand and became the one that actually matters —
//! **it emits a [`Mutation`](crate::observer::Mutation), or it asks for the
//! chain to undo it**.
//!
//! The warning travels with that same laziness: there is nothing to warn
//! about until an open is attempted. That's why [`LazyJournal::set_warning_sink`]
//! exists — the TUI has no `tracing` subscriber and the warning has to reach
//! THE SCREEN, in the session, when it happens.
//!
//! # The ownership window opens and closes more than once (#179)
//!
//! The owner held it WHILE ALIVE, and the verdict was decided ONCE. The two
//! things were the same thing: an ownership window that only knew how to
//! open.
//!
//! - **It retries, with a brake.** If on the first mutation the journal
//!   belonged to someone else, this session tries again — at most once every
//!   [`FRENO_TRAS_FALLO`], so as not to pay `LOCK_WAIT` per mutation.
//!   The occupant is usually transient (another `norte cp` from a script, a
//!   `norte audit`, a daemon restarting) and a quarter of a second of overlap
//!   used to mark a three-hour session. Retrying after a failure is safe: a
//!   FAILED attempt builds no `ChainState`.
//! - **And it is released**, with [`LazyJournal::release`], which closes the
//!   pool and returns the file. Only when nobody else holds the handle:
//!   opening a second one over the same file would be this process taking the
//!   journal away from itself. The POLICY is [`LazyJournal::release_if_idle`],
//!   and it lives here and not in the frontend because the clock for "when it
//!   was last used" belongs to this window, and because deciding and closing
//!   have to happen under the SAME lock; the frontend only chooses how often
//!   to ask (the TUI, on its session tick).
//! - **Reopening RE-READS the chain, and that is not negotiable.**
//!   `ChainState` (`last_seq`, `last_hash`) comes out of the file on EVERY
//!   acquisition because [`crate::journal::Journal`] is built anew: a stale
//!   pair collides with the `seq` PK, and since `last_seq` only advances on a
//!   hit, EVERY following mutation would fail — an effect applied without its
//!   row, in a loop. That's why `release` DESTROYS the handle instead of
//!   keeping it.
//! - **Every transition reaches the sink**, recovery included
//!   ([`JournalStatus`]). The TUI paints a permanent indicator of "this
//!   session is NOT being recorded", and a session that started recording
//!   again silently would turn it into a lie.
//!
//! # `Failed` fails CLOSED, `Busy` does not (#178)
//!
//! The two reasons had the SAME consequence —no journal, a warning, and
//! carry on— so the classification bought nothing and it failed OPEN exactly
//! where the daemon fails closed (`daemon run` aborts on that very same
//! input). Anyone with write access to the state directory could disable the
//! recording of every embedded session —`ntc`, `norte cp/mv/rm/mkdir` and, the
//! valuable one, `norte ai rename --yes`— silently and forever, behind a
//! warning the user is trained to ignore because it also fires in the benign
//! case. And the laziness from #177 RAISED its severity: a session that had
//! not mutated yet had nothing, so an occupant that opened the file once and
//! went dormant also condemned the ones already running.
//!
//! Now they are separated, and that split is what keeps the fix from being
//! worse than the hole:
//!
//! - **`Failed` REFUSES**, with
//!   [`Error::JournalUnavailable`](norte_proto::Error::JournalUnavailable), and
//!   it does so BEFORE the effect: in [`crate::Engine`]'s gate, not in the
//!   observer. By the time the observer runs, the mutation has already
//!   happened, so failing there would not undo it — it would only say
//!   something that worked had failed.
//! - **`Busy` CARRIES ON**, unrecorded and warning. Refusing here would turn
//!   "there is a daemon" into "the file manager doesn't work", and a
//!   transient occupant would take down a three-hour session: the same reason
//!   the #179 retry exists, and it is also what cures this case on its own.
//!
//! There is no `--no-journal` to skip it, and that's on purpose: the way out
//! is to fix or remove the file, which is what the message says. A flag for
//! "mutate without recording" ends up as an alias, and with it the hole comes
//! back whole.
//!
//! # The verdict is fixed PER OPERATION, not per mutation (#205)
//!
//! The window knowing how to reopen exposed an edge that did not exist
//! before: with the journal asked about PER MUTATION, a `copy_tree` that
//! starts with the file busy and runs longer than [`FRENO_TRAS_FALLO`] started
//! recording halfway through — the first k entries without a row, the
//! following n-k with one, inside ONE Task and ONE actor. And then `undo`
//! walks back the recorded tail and leaves the head that isn't recorded:
//! half the copy undone, unable to name the other half, because it has no
//! rows. **"It wasn't recorded" is fixed by hand; "it was recorded halfway"
//! is a trap**, and it was worse than the hole the retry came to close.
//!
//! [`MutationObserver::pin_for_task`](crate::observer::MutationObserver::pin_for_task)
//! closes it: the body of every mutating Task resolves the journal ONCE,
//! before the first effect, and keeps whatever it gets —the handle, or a
//! no-op— for all of its mutations. The operation goes back to being entirely
//! in or entirely out, which is what it was when the verdict lasted the whole
//! session.
//!
//! Two corollaries worth having written down:
//!
//! - **the recovery warning talks about what is STARTING**, not "from now
//!   on": an operation already in flight keeps the verdict it started with,
//!   and a phrase promising otherwise would be false precisely for it;
//! - **while a Task is mutating, [`LazyJournal::release`] answers `false`**,
//!   because the pinned handle is a live `Arc`. That protects the window from
//!   PINNED → last row, which is where there are rows to lose. Nobody
//!   protects the one from the gate to the pin —the gate drops its `Arc` and
//!   the Task may wait in the queue— and there, releasing breaks nothing, but
//!   it can leave the whole operation unrecorded if someone else wins the
//!   reopen;
//! - **pinning looks again at whether the journal is UNREADABLE**, and
//!   refuses the Task if it is (#178). The gate looks before enqueuing and
//!   this looks when actually starting; thirty seconds fit between the two,
//!   in which a file can become corrupted.
//!
//! # What this mechanism does NOT cover
//!
//! - **Nobody releases the journal on its own.** [`LazyJournal::release`] is
//!   the primitive; there is no idleness timer that calls it, so a session
//!   that mutated at 09:00 keeps being the owner until the frontend decides to
//!   release. That policy is the half of #179 that does not live here.
//! - **`gc_partials` does not go through the gate**, so it sweeps its own
//!   `.norte-partial` files even when the journal is unreadable. It's this
//!   process's own garbage, not user data, and it never carried a row.
//! - **The state directory has to be LOCAL.** WAL + `EXCLUSIVE` over NFS/SMB
//!   depends on an `fcntl` that those systems don't always honor, and there
//!   two machines can believe they own the same file at the same time.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Why this session is NOT being recorded in the journal.
///
/// `#[non_exhaustive]`: a new reason shouldn't break whoever does a `match`
/// (the TUI maps it to Fluent, and its `_` arm is the safety net).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum NoJournal {
    /// Another process —a daemon, or another embedded one that already
    /// mutated— holds the exclusive lock.
    Busy,
    /// Could not open for a reason that isn't the lock (permissions, disk, a
    /// corrupt DB, a DB from an era before today's chain). Carries the error
    /// text: [`crate::journal::JournalError`] is not `Clone` and this travels
    /// over a channel up to the screen.
    Failed(String),
}

impl NoJournal {
    /// The phrase for the human, untranslated.
    ///
    /// It's the one for the log and for the CLI. The TUI does NOT use it
    /// except as a safety net: its interface goes through Fluent
    /// (`msg-journal-busy` / `msg-journal-refused`).
    ///
    /// **The two phrases say DIFFERENT things since #178**, and confusing them
    /// is the defect this issue exists to not repeat: `Busy` is "this
    /// happened and wasn't recorded", `Failed` is "this has NOT happened".
    #[must_use]
    pub fn text(&self) -> String {
        match self {
            Self::Busy => "otro proceso tiene el journal (un daemon, u otra sesión embebida): las \
                           mutaciones de ESTA sesión no quedan registradas (#167)"
                .to_owned(),
            Self::Failed(reason) => format!(
                "el journal no se pudo abrir ({reason}): esta sesión REHÚSA mutar mientras \
                 siga así, porque nada quedaría registrado ni se podría deshacer. Arregla \
                 lo que nombra el motivo —el directorio o el fichero— y vuelve a intentarlo \
                 (#178)"
            ),
        }
    }
}

/// What happened to THIS session's journal, in the order it happened.
///
/// It isn't a state you query: it's the event that crosses over to the
/// screen. It exists because the frontend's indicator has to be able to turn
/// OFF — a "NOT being recorded" that doesn't know how to become "now it is"
/// lies the moment the window reopens (#179).
///
/// `#[non_exhaustive]`: a new transition shouldn't break whoever does a
/// `match`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum JournalStatus {
    /// This session stopped being recorded, for this reason.
    Lost(NoJournal),
    /// And it started being recorded again: the window reopened.
    Recovered,
    /// **The journal has been busy for [`SUSPICION_AFTER`] and there is no
    /// daemon listening** (#203).
    ///
    /// [`NoJournal::Busy`] is almost always benign: a live daemon, another
    /// `ntc`, a `norte cp` from a script. That's why it carries on and only
    /// warns — and that's exactly why the warning gets ignored, which is what
    /// makes the attack cheap: anyone with the same uid holds the lock
    /// (`begin exclusive`) and every embedded session stops recording, behind
    /// a phrase that also fires when nothing is wrong.
    ///
    /// These two facts together DO distinguish one case from the other, and
    /// until now nobody combined them: *it's been busy for minutes* and *the
    /// daemon's socket doesn't answer*. It doesn't prove there's an
    /// attacker —a daemon that died mid a long transaction looks the same—
    /// but it stops being the ordinary case, and that is exactly what an
    /// indicator has to be able to say.
    ///
    /// What it does NOT do is refuse the mutation. Turning "someone has your
    /// journal" into "the file manager doesn't work" is the fix #178
    /// deliberately avoided.
    Squatted,
}

/// Whoever receives the changes of "this session is being recorded" or not.
///
/// The frontend implements it. The TUI pushes it over a channel up to the run
/// loop, which paints it IN the session: an `eprintln!` gets covered by the
/// alternate screen a second later and the session lasts hours.
///
/// **It is called with internal locks held and from inside a mutation**: the
/// implementation has to be short and non-blocking (a `send` to an unbounded
/// channel, storing in a `Mutex`), and **it cannot re-enter the
/// [`LazyJournal`] that called it** — the ownership window's lock is held and
/// re-entering it would block forever.
pub trait JournalWarningSink: Send + Sync {
    /// This session is NOT being recorded, and this is why. Once per
    /// EPISODE: as long as the reason doesn't change, it isn't repeated.
    ///
    /// **Don't panic here.** This runs inside the `on_mutation` of a mutation
    /// that has already been applied, so a panic would fail the Task of an
    /// operation that worked.
    fn on_no_journal(&self, why: &NoJournal);

    /// The session started being recorded AGAIN: the window reopened after an
    /// [`Self::on_no_journal`].
    ///
    /// No default body ON PURPOSE: a sink that forgets this leaves its
    /// indicator lit over a session that IS being recorded, which is exactly
    /// the lie #179 came to remove. Let the compiler ask about it.
    ///
    /// Not emitted on the FIRST open, which recovers nothing.
    fn on_journal_recovered(&self);

    /// The journal has been busy for minutes and there is NO daemon listening
    /// (#203): [`JournalStatus::Squatted`].
    ///
    /// With a default body, unlike [`Self::on_journal_recovered`], and the
    /// asymmetry is deliberate: forgetting the recovery leaves an indicator
    /// LYING, while forgetting this one only leaves the generic warning —
    /// which is what was shown until now— so the default falls on the side of
    /// saying less, never of saying something false.
    fn on_journal_squatted(&self) {
        self.on_no_journal(&NoJournal::Busy);
    }
}

/// How long to wait for the lock to free up before calling it busy.
///
/// Short ON PURPOSE. Whoever holds the lock holds it while alive (a daemon,
/// or a three-hour TUI session), so waiting doesn't get it: `sqlx`'s default
/// timeout is FIVE SECONDS, and with a live daemon that turned every embedded
/// `norte cp` into five seconds stalled before carrying on the same way,
/// unrecorded. What DOES fit in this timeout is the one wait that's worth
/// anything: the gap between two short-lived processes, one closing and the
/// next opening.
const LOCK_WAIT: std::time::Duration = std::time::Duration::from_millis(250);

/// At most one open attempt every so often, after one that failed.
///
/// The #179 retry is what saves the session from a transient occupant, but
/// without a brake it would cost `LOCK_WAIT` on EVERY mutation while the
/// occupant is still there —and the usual occupant, a daemon, stays there all
/// afternoon. Thirty seconds is the order of magnitude of "restarting a
/// daemon", not of "noticing it while copying".
pub const FRENO_TRAS_FALLO: std::time::Duration = std::time::Duration::from_secs(30);

/// How long a journal has been `Busy` before it's worth wondering whether a
/// daemon holds it (#203).
///
/// Five minutes, and the number has two sides. Too short, a slow-starting
/// daemon or another `ntc`'s session would get the strong phrase, which is
/// exactly the noise this issue comes to remove. Too long, the session spends
/// the whole afternoon unrecorded behind the mild warning. A daemon startup
/// and a script's `norte cp` live in seconds; five minutes is neither.
pub const SUSPICION_AFTER: std::time::Duration = std::time::Duration::from_mins(5);

/// Who answers "is a daemon listening?" (#203).
///
/// It's an injection and not a direct call for two reasons, and the second is
/// the one that rules: a test cannot spin up a daemon to test the case where
/// there ISN'T one, and `LazyJournal` has no business knowing where a socket
/// lives. The production default is [`DaemonSocketProbe`].
pub trait DaemonPresence: Send + Sync {
    /// `true` if something answers on this user's daemon socket.
    ///
    /// An orphaned socket (the daemon died without cleaning it up) answers
    /// `false`: what matters is whether there's someone ON THE OTHER END, not
    /// whether the file exists.
    fn any_daemon_listening(&self) -> bool;
}

/// The production probe: a `connect` to this uid's default socket.
///
/// Same criterion the daemon's startup uses to detect another one alive — a
/// `connect` gives it away, and an orphaned socket gives `ECONNREFUSED`.
#[derive(Debug, Default, Clone, Copy)]
pub struct DaemonSocketProbe;

impl DaemonPresence for DaemonSocketProbe {
    fn any_daemon_listening(&self) -> bool {
        // Synchronous and, in practice, non-blocking: a `connect` to a local
        // unix socket resolves on the spot, whether it exists or not. Runs
        // with the window's lock held, so nothing more expensive fits here.
        std::os::unix::net::UnixStream::connect(crate::daemon::default_socket_path(None)).is_ok()
    }
}

/// How long to wait for the pool to finish closing in [`LazyJournal::release`].
///
/// A cap and not an indefinite wait: the close runs with the window's lock
/// held, and every mutation needs that same lock. Generous on purpose —
/// closing is local and fast, so exhausting it is already an anomaly.
const CLOSE_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// The journal at `<state>/journal.db`, opened ON THE FIRST MUTATION (#177),
/// and reopened as many times as needed (#179).
///
/// It is at once the engine's [`MutationObserver`](crate::observer::MutationObserver)
/// and its read source for undo, and those two faces share ONE window: if
/// they were two, undo could open a second handle over the same file —or
/// answer "no journal" over a free file— and the chain would stop having a
/// single owner.
///
/// ```
/// # let rt = tokio::runtime::Builder::new_current_thread()
/// #     .enable_all().build().expect("runtime");
/// // The tempdir is created OUTSIDE the runtime: creating it is blocking I/O
/// // and inside an async context it would be exactly what hard rule 2
/// // forbids.
/// let dir = tempfile::tempdir().expect("tempdir");
/// let lazy = norte_core::embedded::LazyJournal::in_state_dir(dir.path());
/// // Freshly built, it hasn't touched disk: the daemon can still open it.
/// assert!(!lazy.attempted());
/// # rt.block_on(async {
/// // And asking for it opens it.
/// assert!(lazy.get().await.is_some());
/// assert!(lazy.attempted());
/// // Releasing it gives it back, and whoever asks for it next reopens it —
/// // re-reading the file's chain.
/// assert!(lazy.release().await);
/// assert!(lazy.get().await.is_some());
/// # });
/// ```
pub struct LazyJournal {
    /// The file. Stored resolved so it doesn't depend on the state directory
    /// still being the same by the time it's finally opened.
    path: PathBuf,
    /// How often a retry is allowed after a failed attempt.
    retry_brake: std::time::Duration,
    /// **The ownership window, whole and under ONE lock**: the handle while
    /// it's held, the last verdict when it isn't, and the last thing told to
    /// the sink.
    ///
    /// A [`tokio::sync::Mutex`] and not a `std` one: opening is `async` and
    /// the lock is held ACROSS the `await` on purpose — that's what
    /// serializes the attempts. Without that, two concurrent mutations would
    /// open two handles and the second would see `Busy` against the first
    /// one's lock, i.e. a process refusing to journal because of itself. It's
    /// the property the `OnceCell` that used to be here gave for free, and the
    /// one that has to be reproduced by hand now that the attempt can repeat.
    window: tokio::sync::Mutex<OwnershipWindow>,
    /// Who answers whether a daemon is listening (#203). Queried ONLY once a
    /// `Busy` has already lasted [`Self::suspicion_delay`], which is what
    /// makes the `connect` cost nothing in the normal case.
    presence: Arc<dyn DaemonPresence>,
    /// How long an episode has to have been busy before wondering about the
    /// daemon. [`SUSPICION_AFTER`] except in tests.
    suspicion_delay: std::time::Duration,
    /// Where the warnings go, and the warning waiting for somewhere to go.
    ///
    /// **Lock order: `window` → `sink` and `window` → `hooks`, and never the
    /// other way around.** `emit` ALWAYS runs with `window` held, and
    /// something invisible depends on that: "what was announced" lives in
    /// `window` and "what's still pending" lives here, i.e. in two different
    /// locks, and they're only consistent because both are touched inside the
    /// same critical section. A `set_warning_sink` that started reading
    /// `window` would invert the order and be a deadly embrace.
    sink: Mutex<SinkSlot>,
    /// Open attempts paid for. OUTSIDE the lock because
    /// [`LazyJournal::attempted`] is synchronous (a `Debug` calls it, and so
    /// do the tests) and because counting doesn't need exclusion.
    open_attempts: std::sync::atomic::AtomicU64,
    /// The hooks endpoint (ADR 0100), if the embedder installed one: it's set
    /// on every handle that gets opened, because the file can be opened and
    /// released several times in a session and the dispatcher is a single
    /// one.
    hooks: Mutex<Option<crate::hooks::HookSender>>,
}

/// The state of the ownership window.
#[derive(Default)]
struct OwnershipWindow {
    /// The handle WHILE this session is the owner.
    ///
    /// `None` doesn't mean "couldn't": it means "doesn't have it right now",
    /// which is also the freshly-built state and the one after a
    /// [`LazyJournal::release`].
    handle: Option<Arc<crate::journal::SqliteJournal>>,
    /// The last FAILED attempt: when, and what it said. The brake consults
    /// it. `None` while the handle is held, or before the first attempt.
    last_failure: Option<(std::time::Instant, NoJournal)>,
    /// The last thing told to the sink, so as not to repeat or contradict it.
    announced: Option<JournalStatus>,
    /// When the handle was last used, for the idleness policy (#179). `None`
    /// while it isn't held.
    last_used: Option<std::time::Instant>,
    /// Since when this EPISODE has been busy (#203): the first `Busy` sets it
    /// and anything else clears it —a good open, a `Failed`— because what's
    /// measured is "how long has THIS occupant lasted", not how many there
    /// have been.
    busy_since: Option<std::time::Instant>,
}

/// The sink and the PENDING warning, under a single lock.
///
/// Two fields and one lock, and not two locks or a `OnceLock` for the sink,
/// because the only property that matters is atomic between the two:
/// installing a sink and handing it the pending warning cannot interleave
/// with "resolve and warn", or the warning gets lost (sink installed an
/// instant late) or duplicated. Losing it is the failure #177 calls "worse
/// than today": a session that mutates unrecorded and without saying so.
#[derive(Default)]
struct SinkSlot {
    sink: Option<Arc<dyn JournalWarningSink>>,
    /// Only a LOSS is retained. A recovery with no sink has nothing to turn
    /// off —nobody turned anything on— so instead of queuing it CLEARS the
    /// pending one: handing a late sink a loss that has already recovered
    /// would light up an indicator for a session that IS recording.
    pending: Option<NoJournal>,
}

impl LazyJournal {
    /// The journal at `<state_dir>/journal.db`, still NOT opened.
    ///
    /// Doesn't touch disk: building it is free and takes the file away from
    /// nobody. That's the point of #177.
    #[must_use]
    pub fn in_state_dir(state_dir: &Path) -> Self {
        Self::with_retry_brake(state_dir, FRENO_TRAS_FALLO)
    }

    /// Like [`Self::in_state_dir`], with another retry brake.
    ///
    /// Exists for the tests —which cannot wait for [`FRENO_TRAS_FALLO`] to see
    /// a retry, nor rely on a clock to see that there wasn't one— and for an
    /// embedder with a different cadence. `Duration::ZERO` retries on every
    /// mutation, with whatever that costs.
    #[must_use]
    pub fn with_retry_brake(state_dir: &Path, retry_brake: std::time::Duration) -> Self {
        Self {
            path: state_dir.join("journal.db"),
            retry_brake,
            window: tokio::sync::Mutex::new(OwnershipWindow::default()),
            presence: Arc::new(DaemonSocketProbe),
            suspicion_delay: SUSPICION_AFTER,
            sink: Mutex::new(SinkSlot::default()),
            open_attempts: std::sync::atomic::AtomicU64::new(0),
            hooks: Mutex::new(None),
        }
    }

    /// Installs the hooks endpoint (ADR 0100): on whatever handle exists now,
    /// and on every one opened afterward.
    pub async fn set_hook_sender(&self, tx: crate::hooks::HookSender) {
        let w = self.window.lock().await;
        if let Some(j) = &w.handle {
            j.set_hook_sender(tx.clone());
        }
        *self
            .hooks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(tx);
    }

    /// With another daemon-presence probe (#203).
    ///
    /// For the tests, which need the "has been busy a good while and there's
    /// no daemon" case without spinning one up, and for an embedder that
    /// knows some other way whether there is one.
    #[must_use]
    pub fn with_daemon_presence(mut self, presence: Arc<dyn DaemonPresence>) -> Self {
        self.presence = presence;
        self
    }

    /// With another suspicion delay (#203).
    ///
    /// For the tests, which cannot wait for [`SUSPICION_AFTER`] nor rely on a
    /// clock to see that the delay was NOT met. `Duration::ZERO` suspects on
    /// the second attempt of the same episode — the first one opens the
    /// episode, and that's a design fact, not a matter of the delay.
    #[must_use]
    pub fn with_suspicion_delay(mut self, suspicion_delay: std::time::Duration) -> Self {
        self.suspicion_delay = suspicion_delay;
        self
    }

    /// Installs whoever receives the warnings, and hands it whatever was
    /// already pending.
    ///
    /// The second part matters: the sink is installed by the frontend's
    /// startup, and a startup that overlaps with an early mutation cannot
    /// leave an unrecorded session mute.
    ///
    /// **Only once per process.** A second sink replaces the first —the first
    /// receiver goes mute forever— and on top of that it no longer finds the
    /// pending warning, which the first one took. Whoever installs it is the
    /// frontend's startup, via
    /// [`crate::backend::Backend::take_journal_warnings`], which has a single
    /// owner for the same reason as its sibling channels.
    ///
    /// And there's a second, finer reason: what's pending is what's NOT
    /// DELIVERED, not the state. A sink that arrives after the first one took
    /// the loss believes it's covering a session that isn't covered, and will
    /// later receive a recovery for something it never showed.
    ///
    /// # Panics
    /// If the internal lock is poisoned (another thread panicked while
    /// holding it) — unrecoverable, same criterion as the engine's other
    /// locks.
    pub fn set_warning_sink(&self, sink: Arc<dyn JournalWarningSink>) {
        let mut slot = self.sink.lock().expect("sink lock is sound");
        if let Some(why) = slot.pending.take() {
            sink.on_no_journal(&why);
        }
        slot.sink = Some(sink);
    }

    /// Has an attempt ever been made to open the journal?
    ///
    /// "Attempted", not "succeeded" nor "right now": what this answers is
    /// whether this process has already paid for an open. For tests, and for
    /// whoever wants to know if the lock was ever in play.
    #[must_use]
    pub fn attempted(&self) -> bool {
        self.attempts() > 0
    }

    /// How many opens have been paid for.
    ///
    /// This is what the #179 brake measures in the tests: "it took less than
    /// X" measures the machine's load as much as the code, and against
    /// `LOCK_WAIT`'s 250 ms the margin wasn't enough to tell them apart.
    #[must_use]
    pub fn attempts(&self) -> u64 {
        self.open_attempts
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// This session's journal, opening it if it isn't currently held.
    ///
    /// `None` = this session is NOT being recorded, and the reason has
    /// already been warned about (once per episode, in here).
    ///
    /// **The returned [`Arc`] must not outlive the operation that asked for
    /// it.** While it's alive, [`Self::release`] cannot let go of the file
    /// (returns `false`), so storing it in a long-lived field turns release
    /// into a permanent, silent no-op. The consumers in this tree hold it for
    /// exactly as long as their Task lasts, which is correct.
    pub async fn get(&self) -> Option<Arc<crate::journal::SqliteJournal>> {
        self.resolve().await.ok()
    }

    /// Releases the journal: closes the pool and returns the file to whoever
    /// wants it (the daemon, a `norte audit`).
    ///
    /// `true` if, on return, this session doesn't hold it — including the
    /// case where it already didn't. `false` if someone else holds an
    /// [`Arc`] of the handle: there it is NOT released, because closing the
    /// pool under an in-flight mutation would kill it, and reopening while the
    /// other `Arc` is alive would open a SECOND handle over the same file.
    ///
    /// The next mutation reopens it, re-reading the file's chain — which is
    /// why the handle is DESTROYED here instead of kept (see the module note
    /// about `ChainState`).
    ///
    /// # Precondition: NO mutation may be between its gate and its row
    ///
    /// This is what has to be resolved BEFORE calling this from an idleness
    /// timer, and it's the reason that timer isn't written yet.
    ///
    /// [`crate::Engine`] checks the journal at the gate, BEFORE the effect,
    /// and writes it into the observer, AFTER. Between those two instants
    /// `release` can close the window; if on top of that another process
    /// takes the file in the meantime, the observer's reopen gives `Busy`,
    /// `on_mutation` answers `Ok(())` —the mutation already happened, failing
    /// there would only lie about something that worked— and the effect is
    /// left without a row. The warning to the frontend DOES go out, so it
    /// isn't mute, but hard rule 4 is broken.
    ///
    /// **Since #205 the precondition is satisfied on its own inside the
    /// engine**, and that's why this method can start having callers: the
    /// body of every mutating Task pins the journal at the start
    /// ([`MutationObserver::pin_for_task`](crate::observer::MutationObserver::pin_for_task))
    /// and holds that `Arc` until the end, so `Arc::try_unwrap` fails and this
    /// answers `false` for as long as an operation is in flight. It wasn't
    /// like this before: `crate::ops` took and released the handle PER ENTRY
    /// POINT, and between two entry points nobody held it.
    ///
    /// What's still left uncovered for the `Arc` is an observer other than
    /// this one —an embedder with its own
    /// [`crate::observer::MutationObserver`] that doesn't implement
    /// `pin_for_task`— and there the precondition is the caller's again.
    ///
    /// # This is NOT cancel-safe. Run it to completion or `tokio::spawn` it.
    ///
    /// Dropping this future at the close's `await` leaves the handle already
    /// TAKEN OUT of the window while the pool keeps closing on its own in
    /// `sqlx`'s worker: the next `resolve` opens against our own dying
    /// connection and gets a `Busy` that we made up ourselves — a false
    /// "unrecorded session" warning, and [`FRENO_TRAS_FALLO`] worth of real
    /// mutations left unrecorded until the retry cures it. In other words,
    /// exactly the bug this function exists to not have.
    ///
    /// A `tokio::select!` with this in one branch triggers it. Put it in the
    /// BODY of the branch, not in the condition.
    pub async fn release(&self) -> bool {
        let mut w = self.window.lock().await;
        Self::release_under_lock(&mut w).await
    }

    /// Releases the journal **if it has gone unused for `idle_for`** (#179).
    ///
    /// It's the policy the primitive didn't come with, and it lives here and
    /// not in the frontend for two reasons: the clock for "when it was used"
    /// belongs to this window —the frontend would have to spy on it— and the
    /// decision and the close have to happen under the SAME lock, or a
    /// mutation could slip in unrecorded between checking and releasing.
    ///
    /// Returns `true` if, on return, the file is free: it was just released,
    /// or we didn't have it. `false` = it's still ours, either because it
    /// isn't idle yet or because someone is holding the handle (an in-flight
    /// Task pins it whole, see [`Self::release`]).
    ///
    /// What this fixes is that an `ntc` that copied a file at 09:00 used to
    /// keep `journal.db` until it exited, so `norte daemon run` and `norte
    /// audit` couldn't open it all day. Reopening RE-READS the chain, which is
    /// what makes releasing safe.
    ///
    /// # This is NOT cancel-safe, for the same reason as [`Self::release`].
    pub async fn release_if_idle(&self, idle_for: std::time::Duration) -> bool {
        let mut w = self.window.lock().await;
        if w.handle.is_none() {
            return true;
        }
        // Without a usage stamp it isn't released: it was just acquired by a
        // path that didn't go through `resolve_under_lock`, and treating it
        // as idle would mean releasing something someone asked for a moment
        // ago.
        let idle_since = w.last_used.filter(|t| t.elapsed() >= idle_for);
        if idle_since.is_none() {
            return false;
        }
        Self::release_under_lock(&mut w).await
    }

    /// The body of [`Self::release`], with the window ALREADY held.
    async fn release_under_lock(w: &mut OwnershipWindow) -> bool {
        let Some(handle) = w.handle.take() else {
            w.last_used = None;
            return true;
        };
        match Arc::try_unwrap(handle) {
            Ok(j) => {
                // Actually close it, and wait for it to close: dropping the
                // `Arc` and moving on would leave the file's lock held for an
                // indefinite while —`sqlx` closes the connection in its
                // worker— and whoever opens next would get a `Busy` we made
                // up ourselves.
                //
                // With a CAP, and holding the window's lock the whole time:
                // `on_mutation` needs that same lock, so a close that never
                // returned would leave the process without journaling AND
                // without mutating, mute. The timeout turns the hang into a
                // degraded state that also gets reported.
                if tokio::time::timeout(CLOSE_WAIT, j.close()).await.is_err() {
                    tracing::warn!(
                        "the journal didn't finish closing in time: the file may stay busy a \
                         while longer"
                    );
                }
                w.last_used = None;
                true
            }
            Err(alive) => {
                w.handle = Some(alive);
                false
            }
        }
    }

    /// Like [`Self::get`], but WITHOUT the brake: if the journal isn't
    /// currently held, an open is attempted no matter what (`LOCK_WAIT`).
    ///
    /// For the caller that is about to show the answer to a human and cannot
    /// answer from a half-minute-old verdict — today
    /// [`crate::Engine::ensure_journal`], which is what `norte ai rename` asks
    /// BEFORE requesting confirmation. For an ordinary mutation the brake is
    /// exactly what you want; here it's what would make the question lie.
    pub async fn acquire_now(&self) -> Option<Arc<crate::journal::SqliteJournal>> {
        self.resolve_now().await.ok()
    }

    /// Like [`Self::resolve`], but WITHOUT the brake — and in ONE critical
    /// section.
    ///
    /// Being a single one matters: releasing the lock to clear the verdict
    /// and taking it again leaves a gap in which another mutation can fail
    /// and re-arm the brake, which would make this answer from the very cache
    /// its own contract promises to skip.
    ///
    /// # Errors
    /// Same as [`Self::resolve`].
    pub async fn resolve_now(&self) -> Result<Arc<crate::journal::SqliteJournal>, NoJournal> {
        let mut w = self.window.lock().await;
        w.last_failure = None;
        self.resolve_under_lock(&mut w).await
    }

    /// The journal, or the REASON there isn't one.
    ///
    /// [`Self::get`] discards the reason because an observer doesn't care.
    /// The mutation gate does NOT: `Busy` carries on and `Failed` refuses
    /// (#178), and that's the whole difference between "there's a live
    /// daemon" and "someone with write access to the state directory
    /// disabled recording".
    ///
    /// Opens if needed and if the brake allows it, exactly like `get` — with
    /// the same warning: **the returned [`Arc`] must not outlive the
    /// operation that asked for it**, or [`Self::release`] turns into a
    /// permanent, silent no-op.
    ///
    /// # Errors
    /// [`NoJournal::Busy`] if another process holds the lock;
    /// [`NoJournal::Failed`] with the file and the reason for everything else
    /// (permissions, corruption, a DB from an era before this chain).
    pub async fn resolve(&self) -> Result<Arc<crate::journal::SqliteJournal>, NoJournal> {
        let mut w = self.window.lock().await;
        self.resolve_under_lock(&mut w).await
    }

    /// The body of [`Self::resolve`], with the window ALREADY held.
    async fn resolve_under_lock(
        &self,
        w: &mut OwnershipWindow,
    ) -> Result<Arc<crate::journal::SqliteJournal>, NoJournal> {
        if let Some(j) = &w.handle {
            w.last_used = Some(std::time::Instant::now());
            return Ok(Arc::clone(j));
        }
        // The brake, and ONLY for `Busy`. What the brake saves is the lock
        // wait (`LOCK_WAIT`), and that wait is only paid when there is a lock
        // to wait for: a `Failed` —the directory doesn't exist, there are no
        // permissions, this isn't a database— returns on the spot, so
        // braking it saves nothing and does cost the one thing that matters
        // since #178, which is WHEN the session finds out the file has
        // already been fixed. With the brake in place, a `chmod` that
        // restored permissions left up to 30 s of refused mutations with no
        // way to force a retry from the interface; without it, the next
        // operation works. The same goes for a TRANSIENT `Failed` (an
        // `EMFILE` in a TUI with many connections), which is the case where
        // 30 s of refusal would be pure harm.
        if let Some((when, NoJournal::Busy)) = &w.last_failure
            && when.elapsed() < self.retry_brake
        {
            return Err(NoJournal::Busy);
        }
        self.open_attempts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let result = match crate::journal::SqliteJournal::open_with_busy_timeout(
            &self.path, LOCK_WAIT,
        )
        .await
        {
            Ok(j) => Ok(Arc::new(j)),
            Err(e) if is_lock_busy(&e) => Err(NoJournal::Busy),
            // The FILE goes into the reason, and not just `SQLite`'s error:
            // since #178 this isn't a warning, it's what has to be fixed for
            // the session to be able to mutate again, and "unable to open
            // database file" with no path in front tells nobody what to
            // touch. `Path::display` is the EXPLICIT lossy conversion hard
            // rule 1 calls for — this is text for a human and nobody
            // reparses it.
            Err(e) => Err(NoJournal::Failed(sanitized_reason(
                &e.to_string(),
                &self.path,
            ))),
        };
        match &result {
            Ok(j) => {
                if let Some(tx) = self
                    .hooks
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone()
                {
                    j.set_hook_sender(tx);
                }
                w.handle = Some(Arc::clone(j));
                w.last_failure = None;
                w.busy_since = None;
                self.announce(w, &JournalStatus::Recovered);
            }
            Err(why) => {
                let now = std::time::Instant::now();
                w.last_failure = Some((now, why.clone()));
                // The episode clock (#203) is ONLY for `Busy`: a `Failed` is
                // not an occupant, it's a broken file, and it already
                // refuses the mutation on its own since #178.
                let suspicious = match why {
                    NoJournal::Busy => {
                        let since = *w.busy_since.get_or_insert(now);
                        // The `connect` is only paid for once the delay has
                        // passed: in the normal case —a live daemon, which is
                        // most `Busy`s— this arm is never touched.
                        since.elapsed() >= self.suspicion_delay
                            && !self.presence.any_daemon_listening()
                    }
                    NoJournal::Failed(_) => {
                        w.busy_since = None;
                        false
                    }
                };
                if suspicious {
                    self.announce(w, &JournalStatus::Squatted);
                } else {
                    self.announce(w, &JournalStatus::Lost(why.clone()));
                }
            }
        }
        result
    }

    /// Emits a transition IF it says something new, and remembers that it
    /// said it.
    ///
    /// Two filters, and both are the difference between a useful indicator
    /// and one that gets ignored: a loss isn't repeated as long as the CLASS
    /// doesn't change (with the brake, that's two messages a minute for
    /// hours), and a recovery isn't emitted if there was nothing to recover —
    /// the FIRST open is normal, not news.
    fn announce(&self, w: &mut OwnershipWindow, event: &JournalStatus) {
        if w.announced
            .as_ref()
            .is_some_and(|already| same_class(already, event))
        {
            return;
        }
        if matches!(event, JournalStatus::Recovered)
            && !matches!(w.announced, Some(JournalStatus::Lost(_)))
        {
            w.announced = Some(event.clone());
            return;
        }
        w.announced = Some(event.clone());
        self.emit(event);
    }

    /// Carries the transition to the sink, or to the log if there's no sink
    /// yet.
    ///
    /// **Whoever installs a sink takes charge of delivery**, and that's why
    /// the `warn!` is the `else` and not an addition: the two frontends warn
    /// through different paths —the TUI to the screen (it installs no
    /// `tracing` subscriber, so there a `warn!` gets silently discarded) and
    /// the CLI to stderr through its own sink, which arrives whatever
    /// `RUST_LOG` says— and emitting through both at once would show the CLI
    /// user the same phrase twice. The `warn!` covers whoever installs
    /// neither (a library embedder, or a mutation that runs ahead of the
    /// frontend's startup): what cannot happen is for this to be MUTE.
    ///
    /// # Panics
    /// If the internal lock is poisoned (another thread panicked while
    /// holding it) — unrecoverable, same criterion as the engine's other
    /// locks.
    ///
    /// A `sink` that panics propagates from here up to `on_mutation` and
    /// fails the Task of a mutation that has ALREADY been applied. It's the
    /// responsibility of whoever implements it (see [`JournalWarningSink`]);
    /// in exchange, a warning swallowed in silence would be worse.
    fn emit(&self, event: &JournalStatus) {
        let mut slot = self.sink.lock().expect("sink lock is sound");
        match (&slot.sink, event) {
            (Some(s), JournalStatus::Lost(why)) => s.on_no_journal(why),
            (Some(s), JournalStatus::Recovered) => s.on_journal_recovered(),
            (Some(s), JournalStatus::Squatted) => s.on_journal_squatted(),
            (None, JournalStatus::Lost(why)) => {
                tracing::warn!(reason = %why.text(), "embedded session WITHOUT a journal");
                slot.pending = Some(why.clone());
            }
            (None, JournalStatus::Recovered) => {
                tracing::info!("the embedded session got its journal back");
                slot.pending = None;
            }
            // With no sink, what's pending stays a `Busy`: it's what a late
            // sink has to see, and `on_journal_squatted` falls back by
            // default to that same phrase. What DOES go up a level is the
            // LOG — this is the line an operator looks for when asking why
            // there's no recording (#203).
            (None, JournalStatus::Squatted) => {
                tracing::warn!(
                    "the journal has been busy for minutes and there is no daemon listening: \
                     someone is holding `journal.db`"
                );
                slot.pending = Some(NoJournal::Busy);
            }
        }
    }
}

/// By hand and not derived: [`crate::journal::SqliteJournal`] isn't `Debug`
/// (it carries `sqlx`'s pool inside), and all a test or log message needs from
/// this type is which point the decision is at.
impl std::fmt::Debug for LazyJournal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `try_lock`: formatting cannot wait for an open to finish, even less
        // so from a `Drop` or a log of the open itself.
        let state = match self.window.try_lock() {
            Err(_) => "opening".to_owned(),
            Ok(w) => match (&w.handle, &w.last_failure) {
                (Some(_), _) => "owner".to_owned(),
                (None, Some((_, why))) => format!("no journal ({why:?})"),
                (None, None) if self.attempted() => "released".to_owned(),
                (None, None) => "unopened".to_owned(),
            },
        };
        f.debug_struct("LazyJournal")
            .field("path", &self.path)
            .field("state", &state)
            .field("attempts", &self.attempts())
            .finish_non_exhaustive()
    }
}

#[async_trait::async_trait]
impl crate::observer::MutationObserver for LazyJournal {
    /// The seam of hard rule 4 in the embedded process, and the trigger for
    /// the open.
    ///
    /// Without a journal it returns `Ok(())`: the mutation has ALREADY
    /// HAPPENED (the observer is called after the effect), so failing here
    /// wouldn't undo it, it would only say that something that worked had
    /// failed. What is NOT acceptable is for it to also be mute, and that's
    /// why the warning lives in `LazyJournal::resolve`, which warns before
    /// this `Ok(())` returns — once per EPISODE since #179, not once per
    /// session: the window can be lost and recovered several times and the
    /// frontend has to learn of every change.
    async fn on_mutation(
        &self,
        mutation: &crate::observer::Mutation<'_>,
        actor: &crate::journal::Actor,
    ) -> Result<(), norte_proto::Error> {
        match self.get().await {
            Some(j) => {
                crate::observer::MutationObserver::on_mutation(j.as_ref(), mutation, actor).await
            }
            None => Ok(()),
        }
    }

    /// Fixes the verdict for an entire Task (#205): either the journal, or
    /// nothing, but the SAME for all of its mutations.
    ///
    /// Returning the [`crate::journal::SqliteJournal`] instead of itself is
    /// what takes this window out of the picture during the Task: the handle
    /// no longer gets re-resolved, so neither can the #179 retry start
    /// recording partway through, nor can a `release` stop doing so. And
    /// since the `Arc` lives as long as the Task does, `release` answers
    /// `false` in the meantime — which is exactly the precondition the
    /// idleness timer is missing.
    ///
    /// Without a journal a no-op is pinned, not `self`: if `self` were
    /// returned, every mutation would ask again and we'd be back to half and
    /// half.
    async fn pin_for_task(
        &self,
    ) -> Result<Option<Arc<dyn crate::observer::MutationObserver>>, norte_proto::Error> {
        // `resolve` and not `get`, because the REASON decides (#178) and
        // `get` discards it. NO catch-all arm: a new reason breaks the build
        // instead of sneaking in as "go ahead unrecorded", same as at the
        // gate.
        match self.resolve().await {
            Ok(j) => Ok(Some(j as Arc<dyn crate::observer::MutationObserver>)),
            // Busy: the Task runs UNRECORDED, entirely. It's the benign case,
            // the one that must not be able to take down a session.
            Err(NoJournal::Busy) => Ok(Some(Arc::new(crate::observer::NoopObserver))),
            // Unreadable: refused here too, and not only at the gate. Between
            // the gate and this point the whole scheduler queue fits, and a
            // file can become corrupted in that stretch; without this, the
            // Task would delete an entire tree in silence. No effect has
            // happened yet.
            Err(NoJournal::Failed(reason)) => {
                tracing::error!(
                    reason = %reason,
                    "Task refused while pinning its journal: cannot open (#178)"
                );
                Err(norte_proto::Error::JournalUnavailable)
            }
        }
    }
}

/// The UI session of a process WITHOUT a daemon (L2).
///
/// The daemon keeps the screen in a `SessionStore` and dumps it to
/// `<state_dir>/session.json`. An embedded frontend has no daemon, so it is
/// its own store: the same file and the SAME lock, so that an embedded
/// process and a daemon —or two embedded ones— don't step on each other's
/// screen.
///
/// One per process, and that's why it's a `OnceLock`: two live stores would be
/// two candidates for the same lock within the same process, and the second
/// would see itself locked out because of the first.
///
/// `state_dir` is resolved ONCE, so a test that calls into here writes the
/// user's REAL state and leaves the session taken away from whatever norte
/// instance has it open: to isolate it you have to set `XDG_STATE_HOME`
/// before the first call (the daemon has `state_dir` in its config for
/// exactly this reason). A change to `HOME` halfway through the process's
/// life isn't picked up either.
static UI_SESSION: std::sync::OnceLock<EmbeddedSession> = std::sync::OnceLock::new();

/// The embedded store: the live session, where it's written, and the right
/// to write it.
struct EmbeddedSession {
    /// This process's live session.
    store: Arc<crate::ui_session::SessionStore>,
    /// Where the state lives, if anywhere. Whether it CAN be written is
    /// another matter, and lives in [`EmbeddedSession::write_right`].
    dir: Option<PathBuf>,
    /// The right to write, which can be acquired LATER.
    ///
    /// It isn't fixed at startup, and that was the missing half (#234): the
    /// window that starts second doesn't have the lock, the first one closes
    /// a while later, and with the decision frozen this window would never
    /// write again for its whole life — its screen would die with it even
    /// though the file had been free for hours.
    write_right: std::sync::Mutex<WriteRight>,
    /// Serializes `take_dirty` + dump.
    ///
    /// The daemon has ONE writer that waits for every dump, so its writes
    /// come out in order by construction. Here whoever calls in writes, and
    /// two `session_put`s at once can interleave —A takes revision 1, B takes
    /// 2, B writes, A writes— and leave the OLD one on disk while memory says
    /// the new one. Today only the TUI calls this and does so serially, so
    /// this closes a door that's open, not a fire that's burning.
    writing: std::sync::Mutex<()>,
}

/// The state of the write right, retried every so often.
#[derive(Default)]
struct WriteRight {
    /// The right, if held.
    lock: Option<crate::ui_session::disk::SessionLock>,
    /// Already warned that it cannot be taken. Without this, a window that's
    /// locked out against a state directory that won't open warns every
    /// thirty seconds all day long: a blinking indicator is an indicator
    /// nobody looks at (#178).
    warned: bool,
    /// The file was written by a newer binary, so this process GIVES UP: it
    /// doesn't try again.
    ///
    /// Retrying had two costs and no upside: re-reading up to a megabyte
    /// every thirty seconds only to refuse it again, and a warning per round.
    /// The next startup looks again, which is when the answer may actually
    /// have changed.
    given_up: bool,
}

/// This process's store, opened the first time.
///
/// Synchronous on purpose: the two wrappers below call it from INSIDE a
/// `spawn_blocking` (rule 2).
fn ui_session_blocking() -> &'static EmbeddedSession {
    UI_SESSION.get_or_init(|| {
        let Some(dir) = norte_config::dirs::state_dir() else {
            // No state directory —an environment without `HOME`— means there's
            // a live screen and nowhere to save it. That's what already
            // happened before the session existed, not a new failure.
            return EmbeddedSession {
                store: Arc::new(crate::ui_session::SessionStore::default()),
                dir: None,
                write_right: std::sync::Mutex::new(WriteRight::default()),
                writing: std::sync::Mutex::new(()),
            };
        };
        let lock = take_the_lock(&dir, true);
        let recovered = crate::ui_session::disk::load_or_default(&dir);
        EmbeddedSession {
            dir: Some(dir),
            store: Arc::new(crate::ui_session::SessionStore::new(recovered.session)),
            // The right to write is TWO things: the lock, and that what was
            // on disk isn't from a newer binary.
            write_right: std::sync::Mutex::new(WriteRight {
                lock: lock.filter(|_| recovered.writable),
                warned: false,
                given_up: !recovered.writable,
            }),
            writing: std::sync::Mutex::new(()),
        }
    })
}

/// Attempts `dir`'s write lock. `warn_on_fail` decides whether a failure gets
/// counted: this is retried every thirty seconds and a warning per round is
/// noise, not information.
fn take_the_lock(dir: &Path, warn_on_fail: bool) -> Option<crate::ui_session::disk::SessionLock> {
    crate::ui_session::disk::lock(dir).unwrap_or_else(|e| {
        if warn_on_fail {
            tracing::warn!(error = %e, "could not take the UI session lock");
        }
        None
    })
}

/// Can this process write the session RIGHT NOW?
///
/// If it already has the lock, yes. If not, it tries again: the window that
/// had it may have closed (#234). Getting it late re-reads the file for two
/// different reasons, and both matter:
///
/// - If a NEWER binary wrote it while we were running, don't step on it: the
///   lock we just took is released and we carry on without writing.
/// - If another window of this same version wrote it, its DOCUMENT is the
///   current one and gets adopted whole, body included. Keeping only the
///   revision seemed enough and it wasn't: the client would get its own body
///   with the other one's number, its next write would land without
///   conflict, and whatever the other window had saved would disappear
///   without anything noticing. With the body in front, the client decides
///   what to keep —it's the only one that knows how to read it— and the
///   number doesn't go backwards either.
///
/// Synchronous: the two wrappers call it from inside a `spawn_blocking`
/// (rule 2).
fn can_write(s: &EmbeddedSession) -> bool {
    let Some(dir) = s.dir.as_ref() else {
        return false;
    };
    let mut guard = s
        .write_right
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if guard.lock.is_some() {
        return true;
    }
    if guard.given_up {
        return false;
    }
    let Some(lock) = take_the_lock(dir, !guard.warned) else {
        guard.warned = true;
        return false;
    };
    let recovered = crate::ui_session::disk::load_or_default(dir);
    if !recovered.writable {
        // A newer binary wrote while we were locked out: the lock we just
        // took is released and it isn't tried again for the rest of the
        // process.
        drop(lock);
        guard.given_up = true;
        return false;
    }
    s.store.adopt_from_disk(recovered.session);
    tracing::info!("the UI session came free: this window is writing it again");
    guard.lock = Some(lock);
    guard.warned = false;
    true
}

/// The saved session and whether this process can write it.
///
/// The `conn` that claims ownership is the same for the whole process: in
/// embedded mode there are no connections, there is ONE surface.
pub async fn session_get() -> (norte_proto::methods::Session, bool) {
    crate::blocking::spawn_blocking(|| {
        let s = ui_session_blocking();
        let owner = s.store.claim(0) && can_write(s);
        (s.store.get(), owner)
    })
    .await
    // A panic inside the closure is NOT "another window has the session",
    // which is what the frontend paints with `false`: it's reported, and then
    // degraded.
    .unwrap_or_else(|e| {
        tracing::warn!(error = %e, "reading the UI session crashed");
        (norte_proto::methods::Session::default(), false)
    })
}

/// Replaces the session and dumps it, if this process is the writer.
///
/// A LOCKED-OUT process —one that didn't get the lock— doesn't write even in
/// memory: it gets `PermissionDenied` before the store is touched. Accepting
/// it in memory and returning a new revision would promise it had saved
/// something that was going nowhere; opening a second window still doesn't
/// cost the first one its screen, which is what mattered.
///
/// **For a human only.** ADR 0059's `Actor::User` gate is enforced by the
/// daemon's handler, and there is no handler here: this function and its
/// sibling are `pub`, so whoever calls them is responsible for the surface
/// behind them being a person. Today none isn't —neither MCP nor plugins
/// reach the embedded `Backend`— and adding one means adding that gate.
///
/// # Errors
///
/// [`norte_proto::Error::PermissionDenied`] if this process isn't the writer
/// (doesn't have the lock, or the file is from a newer binary),
/// [`norte_proto::Error::Conflict`] with `StaleRevision` and
/// [`norte_proto::Error::LimitExceeded`] with `LIMIT_SESSION_BODY`: the same
/// three refusals the daemon gives, so the client doesn't have two paths.
pub async fn session_put(
    version: u32,
    revision: u64,
    body: serde_json::Value,
) -> Result<u64, norte_proto::Error> {
    crate::blocking::spawn_blocking(move || {
        let s = ui_session_blocking();
        // Not being the writer is said BEFORE touching memory, with the same
        // refusal the daemon gives: accepting the `put` and returning a new
        // revision would promise the caller it had saved something that
        // isn't going anywhere. Here "owner" means the lock, because in
        // embedded mode there is ONE surface and no connections disputing
        // anything.
        if !can_write(s) {
            return Err(norte_proto::Error::PermissionDenied);
        }
        let Some(dir) = s.dir.as_ref() else {
            return Err(norte_proto::Error::PermissionDenied);
        };
        let rev = s.store.put(version, revision, body).map_err(|e| match e {
            crate::ui_session::PutError::Conflict { .. } => norte_proto::Error::Conflict {
                conflict: norte_proto::ConflictKind::StaleRevision,
            },
            crate::ui_session::PutError::TooLarge { .. } => norte_proto::Error::LimitExceeded {
                limit: norte_proto::Error::LIMIT_SESSION_BODY.to_owned(),
            },
            // An embedded store doesn't close —a daemon's shutdown is what
            // closes it— but naming it here is what turns adding a close to
            // this arm into a compile error instead of a silence.
            crate::ui_session::PutError::Sealed => norte_proto::Error::Cancelled,
            // Same treatment as in the daemon (#247): a schema this core
            // can't read isn't written, because writing it would kill
            // persistence starting with the next launch.
            crate::ui_session::PutError::UnknownSchema { .. } => norte_proto::Error::Unsupported,
        })?;
        // `take_dirty` and the dump, under ONE lock: that's what keeps two
        // simultaneous writes from leaving the OLD one on disk.
        let _turn = s
            .writing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(session) = s.store.take_dirty()
            && let Err(e) = crate::ui_session::disk::write(dir, &session)
        {
            // A failed dump doesn't bring anything down, and it's marked
            // dirty again so the next `put` retries it whole.
            tracing::warn!(error = %e, "could not write the UI session; retrying");
            s.store.mark_dirty();
        }
        Ok(rev)
    })
    .await
    .unwrap_or(Err(norte_proto::Error::Internal { panic: true }))
}

/// A frontend's embedded engine: a lazy journal over `state_dir`, and ONE
/// client's anchor memory.
///
/// This is what `norte-tui` and `norte-cli` call instead of `Engine::new()`.
/// It doesn't open anything yet (see [`LazyJournal`]), so it doesn't matter
/// whether the command ends up mutating or not — which is what took out of
/// the way the list of subcommands that had to be kept by hand (#177).
///
/// **Here, and only here, [`crate::Engine::with_client_anchors`] is
/// installed** (#317). ADR 0073's anchor says who LOOKED, and that only means
/// something in a process with a client: a frontend's. The daemon's engine is
/// built through another path (`Engine::with_journal`) and therefore doesn't
/// have it, which is what keeps one client's listing from authorizing
/// another's write.
///
/// **One per process.** Every call mints a new [`LazyJournal`], i.e. another
/// candidate to own the SAME file: two engines of this kind alive at once in
/// a process end with the second one seeing `Busy` against the first one's
/// lock — a process refusing to journal because it's stopping itself.
/// `norte-cli` currently has two call sites (`run` and `ai_cmd`) and they are
/// mutually exclusive because `Cmd::Ai` exits earlier through its own branch;
/// if that changes, this has to become a process-wide `OnceLock`.
#[must_use]
pub fn engine_in(state_dir: &Path) -> crate::Engine {
    crate::Engine::with_lazy_journal(Arc::new(LazyJournal::in_state_dir(state_dir)))
        .with_client_anchors()
}

/// Do these two transitions say the SAME thing to whoever paints them?
///
/// By CLASS and not by value, and the difference is an avenue for spam: the
/// text of a [`NoJournal::Failed`] is partly written by whoever can write
/// `journal.db` (`SQLite` interpolates identifiers from the file into its
/// prose), and since #178 that reason no longer carries a retry brake — it
/// reopens on every refused mutation. Comparing the whole `String`, a reason
/// that varied between attempts would give a warning per mutation: a
/// blinking indicator is an indicator that gets ignored, which is exactly
/// what #178 came to fix.
///
/// What's lost is being able to say "now it fails for another reason", and it
/// doesn't matter: the indicator says the same thing in both cases ("this
/// journal cannot be opened") and the detail travels in the error of every
/// refused mutation.
fn same_class(a: &JournalStatus, b: &JournalStatus) -> bool {
    use {JournalStatus as S, NoJournal as N};
    matches!(
        (a, b),
        (S::Recovered, S::Recovered)
            | (S::Lost(N::Busy), S::Lost(N::Busy))
            | (S::Lost(N::Failed(_)), S::Lost(N::Failed(_)))
            // `Squatted` is its own class, and that's why it GOES UP from an
            // already-announced `Busy` instead of staying quiet: the session
            // has spent half an hour seeing "not being recorded" and what
            // changes now is that it no longer has an innocent explanation.
            // Going back down to `Busy` does stay quiet —a daemon starting up
            // isn't better news than the last one, it's the same one— and
            // the only way up out of this state is `Recovered`.
            | (S::Squatted, S::Squatted | S::Lost(N::Busy))
    )
}

/// Cap on the REASON inside [`NoJournal::Failed`], in characters.
const REASON_MAX: usize = 160;

/// Cap on the path that accompanies that reason, in characters, counted from
/// the TAIL.
///
/// Two caps and not one, and the order matters: with a single cap over
/// `"<path>: <reason>"`, a deep `NORTE_CONFIG_DIR` eats the budget and what
/// gets cut is the reason — i.e. the WHY, which is the only thing that tells
/// apart "the directory cannot be written" from "this isn't a database", and
/// those are different fixes. And of the path what's useful is the end
/// (`…/norte/journal.db`), not the beginning.
const PATH_MAX: usize = 80;

/// The text of an open error, fit for a terminal and for a log.
///
/// It's "`<path>`: `<reason>`", with a budget for each half, and both
/// sanitized.
///
/// **Whoever can write `<state>/journal.db` writes part of this phrase.**
/// `SQLite`'s prose interpolates identifiers from the file itself
/// ("malformed database schema (<whatever the attacker puts>) — …"), and from
/// here it goes to the TUI's status bar, to the CLI's stderr and to the log:
/// a control byte there is an escape sequence in whoever is looking's
/// terminal. Controls are stripped and the length is capped.
///
/// It doesn't replace anything else: whoever has that write access has
/// already wrecked the journal's integrity, and since #178 that session also
/// doesn't mutate. This only keeps the failure from turning into an
/// injection on the operator's screen.
fn sanitized_reason(reason: &str, path: &std::path::Path) -> String {
    format!(
        "{}: {}",
        truncate(&sanitize(&path.display().to_string()), PATH_MAX, Edge::Tail),
        sanitized_and_trimmed(reason)
    )
}

/// The reason, sanitized and capped from the beginning.
fn sanitized_and_trimmed(reason: &str) -> String {
    truncate(&sanitize(reason), REASON_MAX, Edge::Head)
}

/// Which end gets trimmed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Edge {
    /// The beginning is kept (a reason reads left to right).
    Head,
    /// The END is kept (what identifies a path is its tail).
    Tail,
}

/// Strips what a terminal would interpret instead of painting.
///
/// Controls (which are escape sequences) and Unicode's bidi reorderers: the
/// latter aren't `char::is_control` and they reorder what comes AFTER them,
/// so a name with a `U+202E` inside rewrites the operator's entire phrase
/// without changing a single byte of what it says.
fn sanitize(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_control() || matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            {
                ' '
            } else {
                c
            }
        })
        .collect()
}

/// Caps to `max` characters, marking with `…` that it was cut.
fn truncate(s: &str, max: usize, edge: Edge) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_owned();
    }
    match edge {
        Edge::Head => s.chars().take(max).chain(std::iter::once('…')).collect(),
        Edge::Tail => std::iter::once('…')
            .chain(s.chars().skip(n - max))
            .collect(),
    }
}

/// Whether this open error is "someone else has it", and not a real problem.
///
/// The `SQLite` CODE is checked, not its prose: `SQLITE_BUSY` (5),
/// `SQLITE_LOCKED` (6) and `SQLITE_PROTOCOL` (15), taking the low byte so the
/// extended ones count too (`SQLITE_BUSY_SNAPSHOT` = 261, …). The text
/// ("database is locked") belongs to `sqlx`'s presentation layer and nobody
/// guarantees it across versions; with the classification hanging off it, a
/// bump that reformats `Display` would turn every `Busy` into `Failed`
/// without any test noticing.
///
/// The 15 is here since #178 and because of #178: it's WAL locking-protocol
/// contention, its documented remedy is RETRY, and since `Failed` refuses the
/// mutation, misclassifying it no longer costs a journal row — it costs an
/// operation denied over a race that resolves itself.
///
/// What is NOT included, and not by oversight: `SQLITE_IOERR`'s lock flavors
/// (`_LOCK` = 3850, `_BLOCKED` = 2826) and `SQLITE_READONLY_CANTLOCK` (520).
/// They sound transient and aren't —an `fcntl` failing over NFS, a file that
/// is genuinely read-only— and their PRIMARIES (10 and 8) are huge drawers
/// that would sweep away half the I/O taxonomy with them. A false `Failed`
/// costs a refusal the user sees and can retry; a false `Busy` costs an
/// unrecorded mutation that nobody sees.
///
/// Deliberately NARROW for the same reason: widening this to "any error"
/// would turn a corrupt DB or a directory without permissions into a silent
/// `Busy`, i.e. into an unrecorded session the operator would believe was
/// recorded, on exactly the machine that needs it most.
fn is_lock_busy(e: &crate::journal::JournalError) -> bool {
    let crate::journal::JournalError::Sqlx(sqlx::Error::Database(db)) = e else {
        return false;
    };
    db.code()
        .and_then(|c| c.parse::<i32>().ok())
        .is_some_and(|c| matches!(c & 0xff, 5 | 6 | 15))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A lab embedded store: without the process's `OnceLock`, which can only
    /// be initialized once and would point at the real state.
    fn session_in(dir: &Path) -> EmbeddedSession {
        EmbeddedSession {
            store: Arc::new(crate::ui_session::SessionStore::default()),
            dir: Some(dir.to_path_buf()),
            write_right: std::sync::Mutex::new(WriteRight::default()),
            writing: std::sync::Mutex::new(()),
        }
    }

    /// **#234**: the right to write is NOT fixed at startup. The window that
    /// started second tries again, and when the first one closes, it writes.
    ///
    /// Before this the decision was frozen in the startup `OnceLock`: the
    /// second window never saved anything for its whole life even if the file
    /// had been free for hours, and its screen died with it.
    #[test]
    fn the_right_to_write_is_acquired_later() {
        let d = tempfile::tempdir().expect("tmp");
        let s = session_in(d.path());
        // Another window has it.
        let first = crate::ui_session::disk::lock(d.path())
            .expect("lock")
            .expect("free");
        assert!(!can_write(&s), "with the owner alive, it doesn't write");
        // The owner leaves.
        drop(first);
        assert!(can_write(&s), "and once it leaves, this window does");
        assert!(can_write(&s), "and it doesn't ask again every time");
    }

    /// Getting it late adopts the FILE's revision: the other window kept
    /// raising it while we were locked out, and dumping ours as-is would
    /// renumber it backwards.
    #[test]
    fn taking_it_late_adopts_the_files_revision() {
        let d = tempfile::tempdir().expect("tmp");
        crate::ui_session::disk::write(
            d.path(),
            &norte_proto::methods::Session {
                version: crate::ui_session::disk::SCHEMA_VERSION,
                revision: 42,
                body: serde_json::json!({ "from": "the other window" }),
            },
        )
        .expect("write");
        let s = session_in(d.path());
        assert_eq!(s.store.get().revision, 0, "ours starts at zero");
        assert!(can_write(&s));
        assert_eq!(
            s.store.get().revision,
            42,
            "the file's revision cannot go backwards"
        );
    }

    /// And if a NEWER binary wrote while we were locked out, it isn't
    /// stepped on: the lock just taken is released and it carries on without
    /// writing.
    #[test]
    fn a_file_from_the_future_takes_away_the_right_just_acquired() {
        let d = tempfile::tempdir().expect("tmp");
        crate::ui_session::disk::write(
            d.path(),
            &norte_proto::methods::Session {
                version: crate::ui_session::disk::SCHEMA_VERSION + 1,
                revision: 9,
                body: serde_json::json!({ "from": "a newer binary" }),
            },
        )
        .expect("write");
        let s = session_in(d.path());
        assert!(!can_write(&s));
        // And the lock stays FREE: a right that isn't going to be used isn't
        // held onto.
        assert!(
            crate::ui_session::disk::lock(d.path())
                .expect("lock")
                .is_some(),
            "the lock was released"
        );
    }
}
