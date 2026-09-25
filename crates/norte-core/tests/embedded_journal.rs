//! #167/#177: the embedded engine carries a journal, and opens it on the
//! FIRST mutation — not at startup.
//!
//! What is pinned down here is the difference between the two. Opening it at
//! startup takes `SQLite`'s EXCLUSIVE lock on `journal.db` for the process's
//! whole life, so an `ntc` just BROWSING kept the daemon from starting (and
//! with it `norte mcp serve`), and denied `norte audit` its read. Opening it
//! on the first mutation keeps the record without keeping the obstruction.

use std::path::Path;
use std::sync::{Arc, Mutex};

use norte_core::embedded::{JournalStatus, JournalWarningSink, LazyJournal, NoJournal};
use norte_core::journal::Actor;
use norte_core::{Engine, SqliteJournal};
use norte_proto::{TaskState, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn vp(w: &str) -> VPath {
    VPath::parse(w).expect("wire")
}

/// A warnings sink that KEEPS them: the test needs to count them, not watch them.
#[derive(Default)]
struct Warnings(Mutex<Vec<NoJournal>>);

impl JournalWarningSink for Warnings {
    fn on_no_journal(&self, why: &NoJournal) {
        self.0.lock().expect("warnings lock").push(why.clone());
    }

    /// This sink only counts losses; `States` watches the recoveries.
    fn on_journal_recovered(&self) {}
}

impl Warnings {
    fn seen(&self) -> Vec<NoJournal> {
        self.0.lock().expect("warnings lock").clone()
    }
}

/// An embedded engine over `dir` as the state directory, with a `MemProvider`
/// registered so there is something to mutate.
fn lazy_engine(dir: &Path) -> (Engine, Arc<LazyJournal>, Arc<MemProvider>) {
    let lazy = Arc::new(LazyJournal::in_state_dir(dir));
    let engine = Engine::with_lazy_journal(Arc::clone(&lazy));
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    (engine, lazy, mem)
}

/// The file that gets contested.
fn journal_path(dir: &Path) -> std::path::PathBuf {
    dir.join("journal.db")
}

/// **The regression from #177.** A session that only BROWSES does not touch
/// the journal, so the daemon and `norte audit` can open it while it lives.
///
/// The list of reads is not decorative: it is the surface a TUI crosses
/// before mutating anything, and any one of them resolving the journal would
/// bring the bug back. The last one — `sync_apply` without a spool — is the
/// most fragile of all: in `Engine::sync_apply_as` the spool is checked
/// BEFORE the journal, and swapping those two lines is enough for opening the
/// sync dialog to take the file away from the daemon.
#[tokio::test]
async fn browsing_does_not_take_the_lock() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (engine, lazy, mem) = lazy_engine(dir.path());
    mem.mkdir(&vp("mem:///sub")).await.expect("fixture");

    engine.stat(&vp("mem:///")).await.ok();
    engine.list(&vp("mem:///")).await.ok();
    engine.capabilities(&vp("mem:///")).await.ok();
    engine
        .rename_batch_plan(&vp("mem:///"), &[("sub".into(), "other".into())])
        .await
        .ok();
    // `sync.apply` without a spool has to refuse WITHOUT opening the journal.
    let hash = norte_proto::methods::PlanHash::parse(&"0".repeat(64)).expect("hash");
    assert!(
        matches!(
            engine.sync_apply_as(&hash, 0, Actor::User).await,
            Err(norte_proto::Error::Unsupported)
        ),
        "without a spool nothing applies"
    );

    assert!(!lazy.attempted(), "browsing does not open the journal");

    // And this is what used to fail: the daemon starting on the same state
    // directory, or a `norte audit` reading.
    SqliteJournal::open(&journal_path(dir.path()))
        .await
        .expect("the journal is still free while the session only browses");
}

/// The first mutation DOES open it, and it gets recorded (hard rule 4, #167).
#[tokio::test]
async fn the_first_mutation_opens_the_journal_and_leaves_a_row() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (engine, lazy, _mem) = lazy_engine(dir.path());

    let h = engine.mkdir(&vp("mem:///new")).await.expect("mkdir");
    assert_eq!(h.join().await, TaskState::Completed);

    assert!(lazy.attempted(), "the mutation opens the journal");
    let journal = lazy.get().await.expect("this session owns it");
    let entries = journal.journal().entries().await.expect("entries");
    assert_eq!(entries.len(), 1, "the mutation left its row: {entries:?}");

    // And now it really is its own: the second one to arrive is shut out.
    // With a SHORT deadline on purpose: the default is five seconds waiting on
    // a lock this test knows nobody will release.
    assert!(
        SqliteJournal::open_with_busy_timeout(
            &journal_path(dir.path()),
            std::time::Duration::from_millis(250)
        )
        .await
        .is_err(),
        "after mutating, this session owns the file"
    );
}

/// The warning fires WHEN the journal is needed, not before, and ONLY ONCE per
/// session — and the mutation still goes ahead (today, #178).
#[tokio::test]
async fn the_warning_fires_on_the_first_mutation_and_only_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Another process (here: another handle) takes the lock FIRST.
    let _owner = SqliteJournal::open(&journal_path(dir.path()))
        .await
        .expect("the first one takes it");

    let (engine, lazy, _mem) = lazy_engine(dir.path());
    let warnings = Arc::new(Warnings::default());
    lazy.set_warning_sink(Arc::clone(&warnings) as Arc<dyn JournalWarningSink>);

    engine.stat(&vp("mem:///")).await.ok();
    assert!(
        warnings.seen().is_empty(),
        "browsing cannot warn of anything: the journal was never even asked for"
    );

    for n in 0..2 {
        let h = engine
            .mkdir(&vp(&format!("mem:///d{n}")))
            .await
            .expect("mkdir");
        assert_eq!(
            h.join().await,
            TaskState::Completed,
            "without a journal it still mutates (#178), the session does not break"
        );
    }

    assert_eq!(
        warnings.seen(),
        vec![NoJournal::Busy],
        "one warning, on the first mutation, and not one per mutation"
    );
}

/// A sink installed AFTER the attempt has already failed gets the warning all
/// the same: otherwise, a session with no record would be left mute by a
/// startup race, which is exactly the failure #177 calls "worse than today".
#[tokio::test]
async fn a_late_sink_gets_the_pending_warning() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _owner = SqliteJournal::open(&journal_path(dir.path()))
        .await
        .expect("the first one takes it");

    let (engine, lazy, _mem) = lazy_engine(dir.path());
    let h = engine.mkdir(&vp("mem:///d")).await.expect("mkdir");
    assert_eq!(h.join().await, TaskState::Completed);

    let warnings = Arc::new(Warnings::default());
    lazy.set_warning_sink(Arc::clone(&warnings) as Arc<dyn JournalWarningSink>);
    assert_eq!(
        warnings.seen(),
        vec![NoJournal::Busy],
        "the pending warning is delivered to the first sink that shows up"
    );
}

/// **The knot in #177.** `undo_session` asks whether this engine has a journal
/// BEFORE anything has mutated. With a cache lazy only in the observer, it
/// would answer `Unsupported` about an engine that would open the journal
/// just fine; the lazy accessor opens it on demand and answers the truth.
#[tokio::test]
async fn undo_opens_the_journal_on_demand() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (engine, lazy, _mem) = lazy_engine(dir.path());

    let (h, _report) = engine
        .undo_session(Actor::User)
        .await
        .expect("a lazy engine over a free journal CAN undo");
    assert_eq!(h.join().await, TaskState::Completed);
    assert!(lazy.attempted(), "undo needed the journal: it opened it");
}

/// And if the journal belongs to someone else, `undo_session` says no — the
/// same thing it used to say: without a chain there is nothing to revert.
#[tokio::test]
async fn undo_without_a_journal_is_still_unsupported() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _owner = SqliteJournal::open(&journal_path(dir.path()))
        .await
        .expect("the first one takes it");
    let (engine, _lazy, _mem) = lazy_engine(dir.path());

    assert!(
        matches!(
            engine.undo_session(Actor::User).await,
            Err(norte_proto::Error::Unsupported)
        ),
        "no chain, no undo"
    );
}

/// The verdict is REMEMBERED between mutations: within the brake's window, a
/// session that found the journal busy does not pay the lock's wait again for
/// every mutation (#179 asks for the retry, not the retry on every row). What
/// DOES change from #177 is that the decision is no longer forever: see
/// `a_passing_occupant_does_not_condemn_the_session`.
#[tokio::test]
async fn the_verdict_is_remembered_within_the_brake() {
    let dir = tempfile::tempdir().expect("tempdir");
    let owner = SqliteJournal::open(&journal_path(dir.path()))
        .await
        .expect("the first one takes it");
    let (engine, lazy, _mem) = lazy_engine(dir.path());

    let h = engine.mkdir(&vp("mem:///d")).await.expect("mkdir");
    assert_eq!(h.join().await, TaskState::Completed);
    assert!(
        lazy.get().await.is_none(),
        "the lock belonged to someone else"
    );

    drop(owner);
    let h = engine.mkdir(&vp("mem:///d2")).await.expect("mkdir");
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(
        lazy.attempts(),
        1,
        "the 30 s brake had not elapsed: not one more attempt"
    );
}

/// A reason that is NOT the lock arrives as [`NoJournal::Failed`] with its
/// text — the branch that ends up in the TUI's status bar, and the only one
/// that stringifies a core error to show it to someone.
///
/// What the mutation does with that reason is #178's business and is pinned
/// by `an_unreadable_journal_refuses_the_mutation`; what is checked here is
/// the CLASSIFICATION, which is what everything else hangs off.
#[tokio::test]
async fn a_journal_that_cannot_be_opened_is_not_busy() {
    let dir = tempfile::tempdir().expect("tempdir");
    // A DIRECTORY where the file goes: `SQLite` cannot open it, and not
    // because of any lock.
    std::fs::create_dir(journal_path(dir.path())).expect("occupy the name");

    let (_engine, lazy, _mem) = lazy_engine(dir.path());
    let warnings = Arc::new(Warnings::default());
    lazy.set_warning_sink(Arc::clone(&warnings) as Arc<dyn JournalWarningSink>);

    assert!(lazy.get().await.is_none(), "it could not be opened");
    match warnings.seen().as_slice() {
        [NoJournal::Failed(reason)] => assert!(!reason.is_empty(), "the reason gets recorded"),
        other => panic!("this is nobody's lock: {other:?}"),
    }
}

/// Two mutations at once share ONE opening attempt and ONE warning.
///
/// This is why the cell is a `OnceCell` and not an `Option` behind a mutex:
/// two concurrent attempts would be two handles on the same file, and the
/// second would see itself `Busy` against the FIRST one's lock — a process
/// refusing to journal because of itself.
#[tokio::test]
async fn two_simultaneous_mutations_share_the_attempt() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _owner = SqliteJournal::open(&journal_path(dir.path()))
        .await
        .expect("the first one takes it");

    let (engine, lazy, _mem) = lazy_engine(dir.path());
    let warnings = Arc::new(Warnings::default());
    lazy.set_warning_sink(Arc::clone(&warnings) as Arc<dyn JournalWarningSink>);

    let (pa, pb) = (vp("mem:///a"), vp("mem:///b"));
    let (a, b) = tokio::join!(engine.mkdir(&pa), engine.mkdir(&pb));
    assert_eq!(a.expect("mkdir a").join().await, TaskState::Completed);
    assert_eq!(b.expect("mkdir b").join().await, TaskState::Completed);
    assert_eq!(
        warnings.seen(),
        vec![NoJournal::Busy],
        "one attempt, one warning, even with the mutations arriving at once"
    );
}

/// `ensure_journal` is the contract `norte ai rename` depends on to be able to
/// say "this will not be undoable" BEFORE asking.
#[tokio::test]
async fn ensure_journal_opens_it_and_answers_the_truth() {
    let free = tempfile::tempdir().expect("tempdir");
    let (engine, lazy, _mem) = lazy_engine(free.path());
    assert!(engine.ensure_journal().await, "the file was free");
    assert!(
        lazy.attempted(),
        "and it opened it without anyone mutating anything"
    );

    let taken = tempfile::tempdir().expect("tempdir");
    let _owner = SqliteJournal::open(&journal_path(taken.path()))
        .await
        .expect("the first one takes it");
    let (engine, _lazy, _mem) = lazy_engine(taken.path());
    assert!(
        !engine.ensure_journal().await,
        "the file belonged to someone else"
    );
}

/// The wiring the TUI uses: the warning leaves the core and arrives through
/// the `Backend`'s channel, and an engine that CANNOT be left without a
/// journal delivers no channel (one that would never sound would make it
/// believe itself covered).
#[tokio::test]
async fn the_backend_delivers_the_warning_through_its_channel() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _owner = SqliteJournal::open(&journal_path(dir.path()))
        .await
        .expect("the first one takes it");

    let (engine, _lazy, _mem) = lazy_engine(dir.path());
    let mut backend = norte_core::backend::Backend::Embedded(Arc::new(engine));
    let mut rx = backend
        .take_journal_warnings()
        .expect("a lazy embedded engine CAN be left without a journal");

    let norte_core::backend::Backend::Embedded(engine) = &backend else {
        unreachable!("it is the embedded one")
    };
    let h = engine.mkdir(&vp("mem:///d")).await.expect("mkdir");
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(
        rx.try_recv(),
        Ok(JournalStatus::Lost(NoJournal::Busy)),
        "the warning reached the channel"
    );

    let mut without_journal = norte_core::backend::Backend::Embedded(Arc::new(Engine::new()));
    assert!(
        without_journal.take_journal_warnings().is_none(),
        "an engine that journals nothing cannot promise to warn about it"
    );
}

// ---------------------------------------------------------------------------
// #179: the ownership window opens and closes more than once.
// ---------------------------------------------------------------------------

/// A sink that records EVERYTHING that reaches it, losses and recoveries.
#[derive(Default)]
struct States(Mutex<Vec<JournalStatus>>);

impl JournalWarningSink for States {
    fn on_no_journal(&self, why: &NoJournal) {
        self.0
            .lock()
            .expect("states lock")
            .push(JournalStatus::Lost(why.clone()));
    }

    fn on_journal_recovered(&self) {
        self.0
            .lock()
            .expect("states lock")
            .push(JournalStatus::Recovered);
    }

    fn on_journal_squatted(&self) {
        self.0
            .lock()
            .expect("states lock")
            .push(JournalStatus::Squatted);
    }
}

impl States {
    fn seen(&self) -> Vec<JournalStatus> {
        self.0.lock().expect("states lock").clone()
    }
}

/// Like [`lazy_engine`], with whatever retry brake the test needs.
fn engine_with_brake(
    dir: &Path,
    brake: std::time::Duration,
) -> (Engine, Arc<LazyJournal>, Arc<MemProvider>) {
    let lazy = Arc::new(LazyJournal::with_retry_brake(dir, brake));
    let engine = Engine::with_lazy_journal(Arc::clone(&lazy));
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    (engine, lazy, mem)
}

/// **#179.1.** The first mutation's occupant was just passing through, and the
/// session starts recording again as soon as it lets go: a quarter-second
/// overlap used to leave a three-hour session marked.
#[tokio::test]
async fn a_passing_occupant_does_not_condemn_the_session() {
    let dir = tempfile::tempdir().expect("tempdir");
    let owner = SqliteJournal::open(&journal_path(dir.path()))
        .await
        .expect("the first one takes it");

    let (engine, lazy, _mem) = engine_with_brake(dir.path(), std::time::Duration::ZERO);
    let warnings = Arc::new(States::default());
    lazy.set_warning_sink(Arc::clone(&warnings) as Arc<dyn JournalWarningSink>);

    let h = engine.mkdir(&vp("mem:///d")).await.expect("mkdir");
    assert_eq!(h.join().await, TaskState::Completed);
    assert!(
        lazy.get().await.is_none(),
        "the lock belonged to someone else"
    );

    // The passer-by lets go.
    owner.close().await;

    let h = engine.mkdir(&vp("mem:///d2")).await.expect("mkdir");
    assert_eq!(h.join().await, TaskState::Completed);
    let journal = lazy
        .get()
        .await
        .expect("the file was left free and got retried");
    let entries = journal.journal().entries().await.expect("entries");
    assert_eq!(
        entries.len(),
        1,
        "the mutation after the retry DID get recorded: {entries:?}"
    );
    assert_eq!(
        warnings.seen(),
        vec![
            JournalStatus::Lost(NoJournal::Busy),
            JournalStatus::Recovered
        ],
        "the frontend's permanent indicator has to be able to turn off"
    );
}

/// A daemon-presence probe that answers whatever the test tells it to (#203).
struct DaemonSays(bool);

impl norte_core::embedded::DaemonPresence for DaemonSays {
    fn any_daemon_listening(&self) -> bool {
        self.0
    }
}

/// **#203.** A `Busy` that has lasted minutes AND with no daemon listening
/// stops looking like the benign case.
///
/// It is the half the generic warning could not give: `Busy` fires the same
/// whether there is a live daemon — the normal case — or whether someone is
/// holding `journal.db` with a `begin exclusive`, and a warning that always
/// fires is a warning nobody looks at.
///
/// The delay is injected at zero, so here it escalates on the FIRST attempt;
/// in production it is five minutes and the first warnings are the usual
/// ones. What this test pins down is the verdict, not the clock — the clock is
/// pinned by its twin below, and neither of them sleeps.
#[tokio::test]
async fn a_persistent_busy_with_no_daemon_is_said_differently() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _owner = SqliteJournal::open(&journal_path(dir.path()))
        .await
        .expect("the first one takes it");

    let states = Arc::new(States::default());
    let lazy = Arc::new(
        LazyJournal::with_retry_brake(dir.path(), std::time::Duration::ZERO)
            .with_daemon_presence(Arc::new(DaemonSays(false)))
            .with_suspicion_delay(std::time::Duration::ZERO),
    );
    lazy.set_warning_sink(Arc::clone(&states) as Arc<dyn JournalWarningSink>);

    assert!(lazy.resolve().await.is_err());
    assert_eq!(
        states.seen(),
        vec![JournalStatus::Squatted],
        "with no daemon and the delay elapsed, the phrasing is the strong one"
    );

    // And it does not repeat: an indicator that flickers is an indicator that
    // gets ignored.
    assert!(lazy.resolve().await.is_err());
    assert!(lazy.resolve().await.is_err());
    assert_eq!(states.seen().len(), 1);
}

/// And BEFORE the delay it does not escalate, no matter how many attempts are
/// made: the delay is what separates "a daemon taking a while to start" from
/// "someone is holding your journal".
///
/// Measured in verdicts and not in the clock — the delay is set to an hour, so
/// no attempt in this test can meet it no matter how slow the machine.
#[tokio::test]
async fn before_the_delay_the_warning_is_still_the_usual_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _owner = SqliteJournal::open(&journal_path(dir.path()))
        .await
        .expect("the first one takes it");

    let states = Arc::new(States::default());
    let lazy = Arc::new(
        LazyJournal::with_retry_brake(dir.path(), std::time::Duration::ZERO)
            .with_daemon_presence(Arc::new(DaemonSays(false)))
            .with_suspicion_delay(std::time::Duration::from_hours(1)),
    );
    lazy.set_warning_sink(Arc::clone(&states) as Arc<dyn JournalWarningSink>);

    for _ in 0..3 {
        assert!(lazy.resolve().await.is_err());
    }
    assert_eq!(states.seen(), vec![JournalStatus::Lost(NoJournal::Busy)]);
}

/// With a daemon listening it does NOT escalate, no matter how long it lasts:
/// that is the benign case, and confusing it is exactly the noise #203 comes
/// to remove.
#[tokio::test]
async fn a_busy_with_a_live_daemon_stays_at_the_usual_warning() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _owner = SqliteJournal::open(&journal_path(dir.path()))
        .await
        .expect("the first one takes it");

    let states = Arc::new(States::default());
    let lazy = Arc::new(
        LazyJournal::with_retry_brake(dir.path(), std::time::Duration::ZERO)
            .with_daemon_presence(Arc::new(DaemonSays(true)))
            .with_suspicion_delay(std::time::Duration::ZERO),
    );
    lazy.set_warning_sink(Arc::clone(&states) as Arc<dyn JournalWarningSink>);

    for _ in 0..3 {
        assert!(lazy.resolve().await.is_err());
    }
    assert_eq!(
        states.seen(),
        vec![JournalStatus::Lost(NoJournal::Busy)],
        "there is a daemon: it is the ordinary case and it is said once"
    );
}

/// **#179, the brake.** Retrying cannot cost `WAIT_FOR_THE_LOCK` per mutation.
/// Measured in ATTEMPTS, not in the clock: "took less than X" measures the
/// machine's load as much as the code.
#[tokio::test]
async fn the_retry_carries_a_brake() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _owner = SqliteJournal::open(&journal_path(dir.path()))
        .await
        .expect("the first one takes it");

    let (engine, lazy, _mem) = engine_with_brake(dir.path(), std::time::Duration::from_hours(1));

    for n in 0..3 {
        let h = engine
            .mkdir(&vp(&format!("mem:///d{n}")))
            .await
            .expect("mkdir");
        assert_eq!(h.join().await, TaskState::Completed);
    }
    assert_eq!(
        lazy.attempts(),
        1,
        "inside the brake's window the lock's wait does not get paid again"
    );
}

/// **The `ChainState` trap, and the reason this is not small.**
///
/// Reopening a journal this process ALREADY HAD forces rereading `last_seq`
/// and `last_hash` from the file. With the old pair, the insert collides
/// against `seq`'s PK — and since `last_seq` only advances on success, EVERY
/// following mutation fails: effect applied with no row, in a loop.
#[tokio::test]
async fn reopening_rereads_the_files_chain() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (engine, lazy, _mem) = engine_with_brake(dir.path(), std::time::Duration::ZERO);

    let h = engine.mkdir(&vp("mem:///one")).await.expect("mkdir");
    assert_eq!(h.join().await, TaskState::Completed);
    assert!(lazy.release().await, "nobody else holds the handle");

    // ANOTHER writer advances the chain while this session does not have it.
    {
        let other = SqliteJournal::open(&journal_path(dir.path()))
            .await
            .expect("really released: the file is free");
        other
            .journal()
            .record(
                "created",
                b"mem:///from-someone-else",
                None,
                norte_core::journal::Reversal::Delete,
                None,
                &Actor::User,
            )
            .await
            .expect("the other one's row");
        other.close().await;
    }

    let h = engine.mkdir(&vp("mem:///two")).await.expect("mkdir");
    assert_eq!(
        h.join().await,
        TaskState::Completed,
        "the mutation after reopening must NOT collide with seq's PK"
    );

    let journal = lazy.get().await.expect("reopened");
    let entries = journal.journal().entries().await.expect("entries");
    let seqs: Vec<i64> = entries.iter().map(|e| e.seq).collect();
    assert_eq!(
        seqs,
        vec![1, 2, 3],
        "the chain follows the OTHER writer: {entries:?}"
    );
    assert_eq!(
        entries[2].path.as_slice(),
        b"mem:///two",
        "and the last one is ours: {entries:?}"
    );
}

/// Two simultaneous mutations over a FREE journal open ONE handle, not two:
/// the second would see itself `Busy` against the first one's lock — a
/// process refusing to journal because of itself.
#[tokio::test]
async fn two_simultaneous_mutations_open_a_single_handle() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (engine, lazy, _mem) = engine_with_brake(dir.path(), std::time::Duration::ZERO);
    let warnings = Arc::new(States::default());
    lazy.set_warning_sink(Arc::clone(&warnings) as Arc<dyn JournalWarningSink>);

    let (pa, pb) = (vp("mem:///a"), vp("mem:///b"));
    let (a, b) = tokio::join!(engine.mkdir(&pa), engine.mkdir(&pb));
    assert_eq!(a.expect("mkdir a").join().await, TaskState::Completed);
    assert_eq!(b.expect("mkdir b").join().await, TaskState::Completed);

    assert_eq!(lazy.attempts(), 1, "one attempt, not one per mutation");
    assert!(
        warnings.seen().is_empty(),
        "nothing to warn about: it opened on the first try"
    );
    let journal = lazy.get().await.expect("owner");
    assert_eq!(
        journal.journal().count().await.expect("count"),
        2,
        "both mutations got recorded"
    );
}

/// Releasing while someone else is holding the handle does NOT release:
/// opening a second handle on the same file would be this process taking the
/// journal away from itself.
/// #179, the policy: it releases when it has gone a while WITHOUT being used,
/// and not before.
///
/// The process used to take the journal on the first mutation and not give it
/// back until exiting: a copy at 09:00 left `norte daemon run` and
/// `norte audit` unable to open the file all day.
#[tokio::test]
async fn releasing_when_idle_waits_until_it_is() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (engine, lazy, _mem) = engine_with_brake(dir.path(), std::time::Duration::ZERO);

    let h = engine.mkdir(&vp("mem:///one")).await.expect("mkdir");
    assert_eq!(h.join().await, TaskState::Completed);

    // Just used: it does not release, and it is still ours.
    assert!(
        !lazy
            .release_if_idle(std::time::Duration::from_mins(1))
            .await,
        "just used: releasing it would be releasing what someone asked for a moment ago"
    );
    assert!(
        SqliteJournal::open(&journal_path(dir.path()))
            .await
            .is_err(),
        "and the file is still held by this session"
    );

    // With the threshold at zero, it is idle by definition.
    assert!(lazy.release_if_idle(std::time::Duration::ZERO).await);
    {
        let other = SqliteJournal::open(&journal_path(dir.path()))
            .await
            .expect("really released: the file is free");
        other.close().await;
    }

    // And the window REOPENS on its own on the next mutation, rereading the
    // chain — which is what makes releasing safe.
    let h = engine.mkdir(&vp("mem:///two")).await.expect("mkdir");
    assert_eq!(h.join().await, TaskState::Completed);
    assert!(
        !lazy
            .release_if_idle(std::time::Duration::from_mins(1))
            .await,
        "it is ours again"
    );
}

/// Never having had it, "release if idle" answers that the file is free:
/// there is nothing to release, and answering `false` would make the caller
/// believe it holds it.
#[tokio::test]
async fn releasing_when_idle_without_ever_holding_it_is_true() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (_engine, lazy, _mem) = engine_with_brake(dir.path(), std::time::Duration::ZERO);
    assert!(lazy.release_if_idle(std::time::Duration::ZERO).await);
}

#[tokio::test]
async fn releasing_with_the_handle_borrowed_does_not_release() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (engine, lazy, _mem) = engine_with_brake(dir.path(), std::time::Duration::ZERO);
    let h = engine.mkdir(&vp("mem:///d")).await.expect("mkdir");
    assert_eq!(h.join().await, TaskState::Completed);

    let borrowed = lazy.get().await.expect("owner");
    assert!(!lazy.release().await, "there is a live Arc out there");
    drop(borrowed);
    assert!(lazy.release().await, "not anymore");
}

/// The reason for an unreadable journal goes through a sanitizer before
/// reaching a terminal: whoever can write the file writes part of that
/// phrase, and `SQLite`'s prose interpolates identifiers from the file
/// itself.
#[tokio::test]
async fn the_reason_for_a_broken_journal_carries_no_controls_to_the_screen() {
    let dir = tempfile::tempdir().expect("tempdir");
    // A file that is not a database, with a hostile identifier inside:
    // `SQLite` will return it in its message.
    std::fs::write(
        journal_path(dir.path()),
        b"i am not sqlite \x1b[31m\x07 \x1b]0;pwned\x07",
    )
    .expect("fixture");

    let (_engine, lazy, _mem) = lazy_engine(dir.path());
    let warnings = Arc::new(Warnings::default());
    lazy.set_warning_sink(Arc::clone(&warnings) as Arc<dyn JournalWarningSink>);
    assert!(lazy.get().await.is_none(), "it is not a database");

    match warnings.seen().as_slice() {
        [NoJournal::Failed(reason)] => {
            assert!(
                !reason.chars().any(char::is_control),
                "not one control byte reaches the status bar: {reason:?}"
            );
            assert!(reason.chars().count() <= 201, "bounded: {}", reason.len());
        }
        other => panic!("this is nobody's lock: {other:?}"),
    }
}

/// `ensure_journal` skips the brake: it is what `norte ai rename` asks BEFORE
/// requesting confirmation, and answering from a half-minute-old verdict
/// would tell the human "this is not being recorded" about a free file.
#[tokio::test]
async fn ensure_journal_does_not_answer_from_the_brake() {
    let dir = tempfile::tempdir().expect("tempdir");
    let owner = SqliteJournal::open(&journal_path(dir.path()))
        .await
        .expect("the first one takes it");
    // LONG brake: a normal mutation would not retry for the whole session.
    let (engine, lazy, _mem) = engine_with_brake(dir.path(), std::time::Duration::from_hours(1));

    assert!(
        !engine.ensure_journal().await,
        "the file belonged to someone else"
    );
    owner.close().await;
    assert!(
        engine.ensure_journal().await,
        "it was left free: the question a human sees is not answered from the cache"
    );
    assert_eq!(lazy.attempts(), 2, "and that took trying again");
}

// ---------------------------------------------------------------------------
// #178: an unreadable journal fails CLOSED; a busy one does not.
// ---------------------------------------------------------------------------

/// **#178.** A `journal.db` that is not a database — which anyone with write
/// access to the state directory can leave — REFUSES the mutation, with its
/// own category and without touching anything.
///
/// It used to go ahead behind a warning, meaning corrupting one file disabled
/// the recording of ALL embedded sessions — including `norte ai rename
/// --yes`'s, which needs it the most — silently and forever, while
/// `norte daemon run` with that same entry refuses to start. The asymmetry
/// was the bug.
#[tokio::test]
async fn an_unreadable_journal_refuses_the_mutation() {
    let dir = tempfile::tempdir().expect("tempdir");
    // A DIRECTORY where the file goes: `SQLite` cannot open it, and not
    // because of any lock.
    std::fs::create_dir(journal_path(dir.path())).expect("occupy the name");

    let (engine, lazy, mem) = lazy_engine(dir.path());
    let warnings = Arc::new(States::default());
    lazy.set_warning_sink(Arc::clone(&warnings) as Arc<dyn JournalWarningSink>);

    assert!(
        matches!(
            engine.mkdir(&vp("mem:///d")).await,
            Err(norte_proto::Error::JournalUnavailable)
        ),
        "an unreadable journal for the mutation with its own category"
    );
    assert!(
        mem.stat(&vp("mem:///d")).await.is_err(),
        "and nothing got touched: the refusal is PRIOR to the effect"
    );
    // The reason, with the file named, still arrives through the warnings
    // channel — which is in-process and CAN carry paths.
    match warnings.seen().as_slice() {
        [JournalStatus::Lost(NoJournal::Failed(reason))] => {
            assert!(reason.contains("journal.db"), "the file is named: {reason}");
        }
        other => panic!("this is nobody's lock: {other:?}"),
    }
}

/// And its twin, which is what keeps the fix from being worse than the hole:
/// an OCCUPIED journal lets things proceed.
///
/// The usual occupant is benign — a live daemon, another window — and
/// refusing would turn "there is a daemon" into "the file manager does not
/// work". A transient one (another script's `norte cp`, a daemon restarting)
/// cannot bring down a three-hour session.
#[tokio::test]
async fn a_busy_journal_lets_things_proceed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _owner = SqliteJournal::open(&journal_path(dir.path()))
        .await
        .expect("the first one takes it");

    let (engine, _lazy, mem) = lazy_engine(dir.path());
    let h = engine
        .mkdir(&vp("mem:///d"))
        .await
        .expect("busy does NOT refuse");
    assert_eq!(h.join().await, TaskState::Completed);
    assert!(
        mem.stat(&vp("mem:///d")).await.is_ok(),
        "the mutation happened"
    );
}

/// The engine WITHOUT a journal by construction (`Engine::new()`, a library
/// embedder) does not get caught in #178's refusal: it has no lazy journal, so
/// there is no file to fix and there was never a record to lose.
#[tokio::test]
async fn an_engine_without_a_lazy_journal_does_not_miss_it() {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let h = engine.mkdir(&vp("mem:///d")).await.expect("mkdir");
    assert_eq!(h.join().await, TaskState::Completed);
}

/// **The whole claim of #178, and the only thing that holds it up.** EVERY
/// mutation point in the engine passes through the journal's gate.
///
/// Without this, coverage was given by a single `mkdir`: a future
/// `Engine::hardlink_as` that forgot the gate would break no test and reopen
/// the hole through the new door, silently. The list is `gate`'s eight
/// callers, and it grows with them.
#[tokio::test]
async fn every_mutation_passes_through_the_journals_gate() {
    use norte_proto::Error as E;

    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(journal_path(dir.path())).expect("occupy the name with a directory");
    let (engine, _lazy, mem) = lazy_engine(dir.path());
    // A tree with something to copy, move, rename and delete.
    mem.mkdir(&vp("mem:///d")).await.expect("fixture");
    {
        let mut sink = mem.write(&vp("mem:///d/a.txt")).await.expect("write");
        sink.write(bytes::Bytes::from_static(b"alive"))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");
    }

    let (from, to) = (vp("mem:///d/a.txt"), vp("mem:///d/b.txt"));
    // The rename plan is requested for real: `rename_batch` compares the hash
    // BEFORE the gate, so a made-up one would fail with `PlanStale` and prove
    // nothing about the journal. (That the order is that way is not a
    // problem: comparing does not touch the tree.)
    let pairs: Vec<(Vec<u8>, Vec<u8>)> = vec![(b"a.txt".to_vec(), b"c.txt".to_vec())];
    let plan = engine
        .rename_batch_plan(&vp("mem:///d"), &pairs)
        .await
        .expect("planning is READING: it does not pass through the journal's gate");
    let refused: Vec<(&str, Result<(), norte_proto::Error>)> = vec![
        ("copy", engine.copy(&from, &to).await.map(|_| ())),
        ("move", engine.move_(&from, &to).await.map(|_| ())),
        ("mkdir", engine.mkdir(&vp("mem:///new")).await.map(|_| ())),
        (
            "delete",
            engine.delete(&vp("mem:///d/a.txt")).await.map(|_| ()),
        ),
        (
            "rename_batch",
            engine
                .rename_batch(&vp("mem:///d"), &pairs, plan.hash())
                .await
                .map(|_| ()),
        ),
        (
            "undo_session",
            engine.undo_session(Actor::User).await.map(|_| ()),
        ),
        // #314: the ninth one. The pin exists exactly so the one that arrives
        // is not forgotten, and this one arrived — so here it is.
        (
            "set_mode",
            engine
                .set_mode(norte_proto::methods::FsSetModeParams {
                    paths: vec![vp("mem:///d/a.txt")],
                    mode: 0o600,
                    recursive: false,
                    dir_mode: None,
                })
                .await
                .map(|_| ()),
        ),
    ];
    for (name, r) in refused {
        assert!(
            matches!(r, Err(E::JournalUnavailable)),
            "{name} has to pass through the journal's gate: {r:?}"
        );
    }

    // And none of that touched the tree: the refusal is PRIOR to the effect.
    assert!(
        mem.stat(&from).await.is_ok(),
        "the file is still where it was"
    );
    assert!(
        mem.stat(&to).await.is_err(),
        "the destination was not created"
    );
    assert!(
        mem.stat(&vp("mem:///new")).await.is_err(),
        "the directory was not created"
    );
}

// ---------------------------------------------------------------------------
// #205: an operation stays ENTIRELY inside the journal, or entirely outside.
// ---------------------------------------------------------------------------

/// A `MemProvider` that RELEASES the journal as soon as it deletes the first
/// node.
///
/// It is #179's retry firing halfway through a long operation, with no races:
/// the occupant leaves the file exactly between the first entry and the
/// second, which is the exact window in which a Task could start recording
/// partway through.
struct ReleasesJournalOnDelete {
    inner: Arc<MemProvider>,
    owner: tokio::sync::Mutex<Option<SqliteJournal>>,
    /// Deletes seen. It releases on the SECOND one, not the first, and
    /// therein lies the point: the first entry's row was already attempted
    /// (and did not land, the file belonged to someone else), so what is left
    /// is an operation with an unrecorded head and a recorded tail — the exact
    /// half-and-half #205 describes, not a verdict change before starting.
    seen: std::sync::atomic::AtomicU64,
}

#[async_trait::async_trait]
impl Provider for ReleasesJournalOnDelete {
    fn scheme(&self) -> &str {
        self.inner.scheme()
    }
    fn capabilities(&self) -> norte_proto::Capabilities {
        self.inner.capabilities()
    }
    async fn stat(&self, p: &VPath) -> Result<norte_proto::Entry, norte_proto::Error> {
        self.inner.stat(p).await
    }
    async fn list(&self, p: &VPath) -> Result<norte_vfs::EntryStream, norte_proto::Error> {
        self.inner.list(p).await
    }
    async fn read(
        &self,
        p: &VPath,
        range: Option<norte_proto::ByteRange>,
    ) -> Result<norte_vfs::ByteStream, norte_proto::Error> {
        self.inner.read(p, range).await
    }
    async fn write(&self, p: &VPath) -> Result<Box<dyn norte_vfs::ByteSink>, norte_proto::Error> {
        self.inner.write(p).await
    }
    async fn mkdir(&self, p: &VPath) -> Result<(), norte_proto::Error> {
        self.inner.mkdir(p).await
    }
    async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), norte_proto::Error> {
        self.inner.rename(from, to).await
    }
    async fn remove(&self, p: &VPath) -> Result<(), norte_proto::Error> {
        self.inner.remove(p).await?;
        if self.seen.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 1
            && let Some(j) = self.owner.lock().await.take()
        {
            // For real, and waiting: releasing without closing would leave the
            // lock in place for an indefinite while and the test would depend
            // on the clock.
            j.close().await;
        }
        Ok(())
    }
}

/// **#205.** An operation that starts WITHOUT a journal stays without a
/// journal entirely, even if the file gets freed halfway through.
///
/// Without pinning the verdict, `ops` used to ask PER MUTATION: the first
/// entry left no row, the occupant let go, and the following ones did — half
/// an operation recorded inside ONE Task and ONE actor. `undo_session` then
/// unwinds the recorded tail and leaves the head that is not, unable to name
/// what was left, because there are no rows for it.
///
/// "Nothing got recorded" gets fixed by hand; "got half-recorded" is a trap,
/// and #179's retry is what opened it.
#[tokio::test]
async fn an_operation_does_not_get_recorded_halfway() {
    let dir = tempfile::tempdir().expect("tempdir");
    let owner = SqliteJournal::open(&journal_path(dir.path()))
        .await
        .expect("the first one takes it");

    // ZERO brake: without pinning the verdict, the second entry would retry
    // and find the file free. That is what makes the test discriminating.
    let lazy = Arc::new(LazyJournal::with_retry_brake(
        dir.path(),
        std::time::Duration::ZERO,
    ));
    let engine = Engine::with_lazy_journal(Arc::clone(&lazy));
    let mem = Arc::new(MemProvider::new());
    mem.mkdir(&vp("mem:///d")).await.expect("fixture");
    for n in 0..3 {
        let mut sink = mem
            .write(&vp(&format!("mem:///d/f{n}.txt")))
            .await
            .expect("write");
        sink.write(bytes::Bytes::from_static(b"x"))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");
    }
    let provider = Arc::new(ReleasesJournalOnDelete {
        inner: Arc::clone(&mem),
        owner: tokio::sync::Mutex::new(Some(owner)),
        seen: std::sync::atomic::AtomicU64::new(0),
    });
    engine.register_provider(provider as Arc<dyn Provider>);

    // A permanent delete of the tree: four entries, one mutation each.
    let h = engine
        .delete_with(&vp("mem:///d"), norte_proto::DeleteMode::Permanent)
        .await
        .expect("delete");
    assert_eq!(h.join().await, TaskState::Completed);
    assert!(
        mem.stat(&vp("mem:///d")).await.is_err(),
        "the whole tree got deleted: the effect does not depend on the journal"
    );

    // And the file was left free halfway through, so now this session does
    // open it.
    let journal = lazy.get().await.expect("the occupant let go of it");
    assert_eq!(
        journal.journal().count().await.expect("count"),
        0,
        "the operation started without a journal: NONE of its entries got \
         recorded, not even the ones after the file was freed. Without pinning \
         the verdict it is 3 of 4 — unrecorded head, recorded tail — which is \
         the operation undo would undo halfway"
    );
}

/// And the other half of the same property: an operation that starts WITH a
/// journal records all of its entries.
///
/// Both together are "entirely in or entirely out". Without this one, pinning
/// the verdict to `NoopObserver` for everything would pass the test above.
#[tokio::test]
async fn an_operation_that_starts_with_a_journal_records_everything() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (engine, lazy, mem) = lazy_engine(dir.path());
    mem.mkdir(&vp("mem:///d")).await.expect("fixture");
    for n in 0..3 {
        let mut sink = mem
            .write(&vp(&format!("mem:///d/f{n}.txt")))
            .await
            .expect("write");
        sink.write(bytes::Bytes::from_static(b"x"))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");
    }

    let h = engine
        .delete_with(&vp("mem:///d"), norte_proto::DeleteMode::Permanent)
        .await
        .expect("delete");
    assert_eq!(h.join().await, TaskState::Completed);

    let journal = lazy.get().await.expect("owner");
    assert_eq!(
        journal.journal().count().await.expect("count"),
        4,
        "three files and their directory: the whole operation"
    );
}

/// A provider that tries to RELEASE the journal right when the Task is
/// mutating, and records what it was told.
///
/// The instant matters, and that is why it is asked from in here: between the
/// engine dispatching the Task and its body pinning the verdict there is
/// nobody holding the handle, and releasing THERE is harmless (there is still
/// no effect, and the `pin` reopens it). The window that matters is the other
/// one, the one that runs from the `pin` to the last row, and it is only
/// reachable from inside the effect.
struct ReleasesWhileMutating {
    inner: Arc<MemProvider>,
    lazy: Arc<LazyJournal>,
    released: Mutex<Option<bool>>,
}

#[async_trait::async_trait]
impl Provider for ReleasesWhileMutating {
    fn scheme(&self) -> &str {
        self.inner.scheme()
    }
    fn capabilities(&self) -> norte_proto::Capabilities {
        self.inner.capabilities()
    }
    async fn stat(&self, p: &VPath) -> Result<norte_proto::Entry, norte_proto::Error> {
        self.inner.stat(p).await
    }
    async fn list(&self, p: &VPath) -> Result<norte_vfs::EntryStream, norte_proto::Error> {
        self.inner.list(p).await
    }
    async fn read(
        &self,
        p: &VPath,
        range: Option<norte_proto::ByteRange>,
    ) -> Result<norte_vfs::ByteStream, norte_proto::Error> {
        self.inner.read(p, range).await
    }
    async fn write(&self, p: &VPath) -> Result<Box<dyn norte_vfs::ByteSink>, norte_proto::Error> {
        self.inner.write(p).await
    }
    async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), norte_proto::Error> {
        self.inner.rename(from, to).await
    }
    async fn remove(&self, p: &VPath) -> Result<(), norte_proto::Error> {
        self.inner.remove(p).await
    }
    async fn mkdir(&self, p: &VPath) -> Result<(), norte_proto::Error> {
        // The effect is about to happen and its row does not exist yet: THIS
        // is the window.
        let r = self.lazy.release().await;
        *self.released.lock().expect("lock") = Some(r);
        self.inner.mkdir(p).await
    }
}

/// The pinned handle is held for the WHOLE Task, so `release` cannot close the
/// window between a mutation's gate and its row.
///
/// It is the precondition #179's idleness timer is missing, and the half of
/// #205 that is not about undo: pinning the handle at the start gives this for
/// free, and without it `release` from another thread would leave an effect
/// with no row and no error.
#[tokio::test]
async fn while_a_task_mutates_the_journal_cannot_be_released() {
    let dir = tempfile::tempdir().expect("tempdir");
    let lazy = Arc::new(LazyJournal::in_state_dir(dir.path()));
    let engine = Engine::with_lazy_journal(Arc::clone(&lazy));
    let mem = Arc::new(MemProvider::new());
    let provider = Arc::new(ReleasesWhileMutating {
        inner: Arc::clone(&mem),
        lazy: Arc::clone(&lazy),
        released: Mutex::new(None),
    });
    engine.register_provider(Arc::clone(&provider) as Arc<dyn Provider>);

    let h = engine.mkdir(&vp("mem:///new")).await.expect("mkdir");
    assert_eq!(h.join().await, TaskState::Completed);

    assert_eq!(
        *provider.released.lock().expect("lock"),
        Some(false),
        "with the Task mid-mutation, releasing the journal has to be REFUSED: \
         closing it there would leave this effect with no row and no error"
    );
    let journal = lazy.get().await.expect("owner");
    assert_eq!(
        journal.journal().count().await.expect("count"),
        1,
        "and the row arrived"
    );
}

/// A `MemProvider` that releases the journal after the SECOND rename.
///
/// [`ReleasesJournalOnDelete`]'s twin for the other path that pins its verdict
/// outside `ops`: the rename batch, which when it starts with no journal
/// records through the raw observer.
struct ReleasesJournalOnRename {
    inner: Arc<MemProvider>,
    owner: tokio::sync::Mutex<Option<SqliteJournal>>,
    seen: std::sync::atomic::AtomicU64,
}

#[async_trait::async_trait]
impl Provider for ReleasesJournalOnRename {
    fn scheme(&self) -> &str {
        self.inner.scheme()
    }
    fn capabilities(&self) -> norte_proto::Capabilities {
        self.inner.capabilities()
    }
    async fn stat(&self, p: &VPath) -> Result<norte_proto::Entry, norte_proto::Error> {
        self.inner.stat(p).await
    }
    async fn list(&self, p: &VPath) -> Result<norte_vfs::EntryStream, norte_proto::Error> {
        self.inner.list(p).await
    }
    async fn read(
        &self,
        p: &VPath,
        range: Option<norte_proto::ByteRange>,
    ) -> Result<norte_vfs::ByteStream, norte_proto::Error> {
        self.inner.read(p, range).await
    }
    async fn write(&self, p: &VPath) -> Result<Box<dyn norte_vfs::ByteSink>, norte_proto::Error> {
        self.inner.write(p).await
    }
    async fn mkdir(&self, p: &VPath) -> Result<(), norte_proto::Error> {
        self.inner.mkdir(p).await
    }
    async fn remove(&self, p: &VPath) -> Result<(), norte_proto::Error> {
        self.inner.remove(p).await
    }
    async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), norte_proto::Error> {
        self.inner.rename(from, to).await?;
        if self.seen.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 1
            && let Some(j) = self.owner.lock().await.take()
        {
            j.close().await;
        }
        Ok(())
    }
}

/// **#205 in the rename batch**, which pins its verdict in `engine` and not in
/// `ops`, and therefore had the same crack of its own.
///
/// When `rename_batch_as` finds no journal on starting, it records through
/// the raw observer. Without pinning it, every step would ask again — and the
/// rows arriving partway would ALSO go with no `batch_id`, meaning the batch
/// the wire announces as one undoable unit would end up half-recorded and
/// ungrouped.
#[tokio::test]
async fn a_rename_batch_also_does_not_get_recorded_halfway() {
    let dir = tempfile::tempdir().expect("tempdir");
    let owner = SqliteJournal::open(&journal_path(dir.path()))
        .await
        .expect("the first one takes it");

    let lazy = Arc::new(LazyJournal::with_retry_brake(
        dir.path(),
        std::time::Duration::ZERO,
    ));
    let engine = Engine::with_lazy_journal(Arc::clone(&lazy));
    let mem = Arc::new(MemProvider::new());
    mem.mkdir(&vp("mem:///d")).await.expect("fixture");
    for n in 0..3 {
        let mut sink = mem
            .write(&vp(&format!("mem:///d/a{n}.txt")))
            .await
            .expect("write");
        sink.write(bytes::Bytes::from_static(b"x"))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");
    }
    let provider = Arc::new(ReleasesJournalOnRename {
        inner: Arc::clone(&mem),
        owner: tokio::sync::Mutex::new(Some(owner)),
        seen: std::sync::atomic::AtomicU64::new(0),
    });
    engine.register_provider(provider as Arc<dyn Provider>);

    let pairs: Vec<(Vec<u8>, Vec<u8>)> = (0..3)
        .map(|n| {
            (
                format!("a{n}.txt").into_bytes(),
                format!("b{n}.txt").into_bytes(),
            )
        })
        .collect();
    let plan = engine
        .rename_batch_plan(&vp("mem:///d"), &pairs)
        .await
        .expect("plan");
    let (h, _report) = engine
        .rename_batch(&vp("mem:///d"), &pairs, plan.hash())
        .await
        .expect("rename_batch");
    assert_eq!(h.join().await, TaskState::Completed);
    assert!(
        mem.stat(&vp("mem:///d/b0.txt")).await.is_ok(),
        "the renames happened"
    );

    let journal = lazy.get().await.expect("the occupant let go of it halfway");
    assert_eq!(
        journal.journal().count().await.expect("count"),
        0,
        "the batch started without a journal: NONE of its steps got recorded, \
         and certainly not some yes and some no"
    );
}

/// Builds an engine whose journal belongs to someone else, and who releases it
/// as soon as the provider sees its first mutation.
async fn scenario_that_releases(dir: &Path) -> (Engine, Arc<LazyJournal>, Arc<MemProvider>) {
    let owner = SqliteJournal::open(&journal_path(dir))
        .await
        .expect("the first one takes it");
    let lazy = Arc::new(LazyJournal::with_retry_brake(
        dir,
        std::time::Duration::ZERO,
    ));
    let engine = Engine::with_lazy_journal(Arc::clone(&lazy));
    let mem = Arc::new(MemProvider::new());
    let provider = Arc::new(ReleasesOnMutate {
        inner: Arc::clone(&mem),
        owner: tokio::sync::Mutex::new(Some(owner)),
        seen: std::sync::atomic::AtomicU64::new(0),
    });
    engine.register_provider(provider as Arc<dyn Provider>);
    (engine, lazy, mem)
}

/// A directory with three files, so the operation has entries to split.
async fn little_tree(mem: &Arc<MemProvider>, root: &str) {
    mem.mkdir(&vp(root)).await.expect("mkdir");
    for n in 0..3 {
        let mut sink = mem
            .write(&vp(&format!("{root}/f{n}.txt")))
            .await
            .expect("write");
        sink.write(bytes::Bytes::from_static(b"x"))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");
    }
}

/// How many rows `lazy`'s journal has, which by this point is free.
async fn row_count(lazy: &Arc<LazyJournal>) -> i64 {
    lazy.get()
        .await
        .expect("the occupant let go of it")
        .journal()
        .count()
        .await
        .expect("count")
}

/// **The whole rule of #205, and the only thing that holds it up.** EVERY Task
/// that mutates pins its verdict: starts without a journal → ends without a
/// journal, entirely.
///
/// The twin of `every_mutation_passes_through_the_journals_gate` for #205, and
/// for the same reason: without it the rule lives in a comment, and the day
/// someone adds an `Engine::hardlink_as` that forgets to pin, no test notices
/// — the operation will start getting recorded partway through and undo will
/// undo it halfway, silently.
///
/// Each case runs in its own state directory: what is being asserted is that
/// the journal was left EMPTY, and sharing it would let the one next door
/// fill it.
#[tokio::test]
async fn every_task_that_mutates_pins_its_verdict() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (engine, lazy, mem) = scenario_that_releases(dir.path()).await;
    little_tree(&mem, "mem:///src").await;
    let h = engine
        .copy(&vp("mem:///src"), &vp("mem:///dst"))
        .await
        .expect("copy");
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(row_count(&lazy).await, 0, "copy_tree pins its verdict");

    // Inside the SAME provider a move is ONE rename, so this also exercises
    // the `rename_with_policy` path, which is the other one `move_task`
    // delegates to.
    let dir = tempfile::tempdir().expect("tempdir");
    let (engine, lazy, mem) = scenario_that_releases(dir.path()).await;
    little_tree(&mem, "mem:///src").await;
    let h = engine
        .move_(&vp("mem:///src"), &vp("mem:///dst"))
        .await
        .expect("move");
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(row_count(&lazy).await, 0, "move pins its verdict");

    let dir = tempfile::tempdir().expect("tempdir");
    let (engine, lazy, mem) = scenario_that_releases(dir.path()).await;
    little_tree(&mem, "mem:///d").await;
    let h = engine
        .delete_with(&vp("mem:///d"), norte_proto::DeleteMode::Permanent)
        .await
        .expect("delete");
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(row_count(&lazy).await, 0, "delete pins its verdict");

    // A single mutation, so there is no half to split — but the verdict still
    // has to be the one from the start.
    let dir = tempfile::tempdir().expect("tempdir");
    let (engine, lazy, _mem) = scenario_that_releases(dir.path()).await;
    let h = engine.mkdir(&vp("mem:///new")).await.expect("mkdir");
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(row_count(&lazy).await, 0, "mkdir pins its verdict");

    // #314: a permissions batch is SEVERAL mutations in a row, which is the
    // case this test exists to cover — the observer is pinned once, before
    // the first one, and does not get asked again along the way.
    let dir = tempfile::tempdir().expect("tempdir");
    let (engine, lazy, mem) = scenario_that_releases(dir.path()).await;
    little_tree(&mem, "mem:///d").await;
    let h = engine
        .set_mode(norte_proto::methods::FsSetModeParams {
            paths: vec![vp("mem:///d/f0.txt"), vp("mem:///d/f1.txt")],
            mode: 0o600,
            recursive: false,
            dir_mode: None,
        })
        .await
        .expect("set_mode");
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(row_count(&lazy).await, 0, "set_mode pins its verdict");
}

/// The provider for [`every_task_that_mutates_pins_its_verdict`]: releases the
/// journal as soon as it sees its SECOND mutation, whatever kind it is.
struct ReleasesOnMutate {
    inner: Arc<MemProvider>,
    owner: tokio::sync::Mutex<Option<SqliteJournal>>,
    seen: std::sync::atomic::AtomicU64,
}

impl ReleasesOnMutate {
    /// Releases on the FIRST mutation, not the second.
    ///
    /// What is checked here is that the Task's verdict does not change, not
    /// where the cut falls — `an_operation_does_not_get_recorded_halfway`
    /// handles that, with its 3-of-4. And it has to be the first: a `move`
    /// inside the same provider is ONE rename, so waiting for the second would
    /// never release and the case would pass without proving anything.
    async fn maybe_release(&self) {
        if self.seen.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0
            && let Some(j) = self.owner.lock().await.take()
        {
            j.close().await;
        }
    }
}

#[async_trait::async_trait]
impl Provider for ReleasesOnMutate {
    fn scheme(&self) -> &str {
        self.inner.scheme()
    }
    fn capabilities(&self) -> norte_proto::Capabilities {
        self.inner.capabilities()
    }
    async fn stat(&self, p: &VPath) -> Result<norte_proto::Entry, norte_proto::Error> {
        self.inner.stat(p).await
    }
    async fn list(&self, p: &VPath) -> Result<norte_vfs::EntryStream, norte_proto::Error> {
        self.inner.list(p).await
    }
    async fn read(
        &self,
        p: &VPath,
        range: Option<norte_proto::ByteRange>,
    ) -> Result<norte_vfs::ByteStream, norte_proto::Error> {
        self.inner.read(p, range).await
    }
    async fn write(&self, p: &VPath) -> Result<Box<dyn norte_vfs::ByteSink>, norte_proto::Error> {
        let sink = self.inner.write(p).await?;
        self.maybe_release().await;
        Ok(sink)
    }
    async fn mkdir(&self, p: &VPath) -> Result<(), norte_proto::Error> {
        self.inner.mkdir(p).await?;
        self.maybe_release().await;
        Ok(())
    }
    async fn remove(&self, p: &VPath) -> Result<(), norte_proto::Error> {
        self.inner.remove(p).await?;
        self.maybe_release().await;
        Ok(())
    }
    async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), norte_proto::Error> {
        self.inner.rename(from, to).await?;
        self.maybe_release().await;
        Ok(())
    }
    // #314: changing permissions is one more mutation, and without forwarding
    // it this double used to answer `Unsupported` from the trait's default —
    // the Task would finish "fine" having mutated nothing and without
    // releasing the journal, which is exactly the opposite of what this test
    // checks.
    async fn set_mode(&self, p: &VPath, mode: u32) -> Result<(), norte_proto::Error> {
        self.inner.set_mode(p, mode).await?;
        self.maybe_release().await;
        Ok(())
    }
    // So the PREVIOUS mode can be read: the trait's default drops the
    // options, and with them the `posix.mode` the reversal needs.
    async fn stat_with(
        &self,
        p: &VPath,
        opt: &norte_vfs::ListOptions,
    ) -> Result<norte_proto::Entry, norte_proto::Error> {
        self.inner.stat_with(p, opt).await
    }
}
