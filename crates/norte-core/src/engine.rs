//! [`Engine`]: the core's embedded API (M0). The JSON-RPC daemon (M1) will
//! wrap this same API; frontends contain no business logic.

use std::sync::{Arc, RwLock};

use norte_proto::{
    CapabilityFlags, CollisionPolicy, DeleteMode, Entry, Error, ResumePolicy, Segment,
    SymlinkPolicy, TaskId, TaskKind, VPath, VerifyPolicy,
    methods::{PlanHash, RelPath},
};
use norte_vfs::{EntryStream, Provider};

use crate::observer::{MutationObserver, NoopObserver};
use crate::ops;
use crate::ops::OnExists;
use crate::scheduler::{Priority, Scheduler, TaskHandle};
use crate::sessions::SessionPool;

/// Options for a copy/move (ADR 0005): what to do on collisions and with
/// symlinks. `Default` = M0's strict behavior (`Fail` + `Preserve`).
///
/// ```
/// use norte_core::TransferOptions;
/// use norte_proto::{CollisionPolicy, SymlinkPolicy};
/// let opts = TransferOptions::default();
/// assert_eq!(opts.on_collision, CollisionPolicy::Fail);
/// assert_eq!(opts.symlinks, SymlinkPolicy::Preserve);
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TransferOptions {
    /// What to do if the destination already exists.
    pub on_collision: CollisionPolicy,
    /// What to do with the source's symlinks.
    pub symlinks: SymlinkPolicy,
    /// Resuming interrupted transfers (ADR 0012); default `Off` = M1's
    /// contract (cancelling leaves a clean destination).
    pub resume: ResumePolicy,
    /// Verifying the partial on resume (only with `resume=On`).
    pub verify: VerifyPolicy,
    /// QUEUED instead of in parallel (ADR 0149): one at a time.
    pub queued: bool,
}

/// Where an [`Engine`]'s journal comes from, which since #177 is no longer
/// always "has it or doesn't".
///
/// The three variants are the three startups that exist: an engine with no
/// journal (tests, embedders), the daemon's —which opens it itself and
/// refuses to start if it cannot— and an embedded frontend's, which does not
/// open it until it is needed.
enum JournalSource {
    /// No journal: `undo_session` and `sync.apply` answer `Unsupported`, and
    /// the observer records nothing.
    None,
    /// Already opened by whoever built the engine (the daemon). This process
    /// has owned the chain since before the engine existed.
    Open(Arc<crate::journal::SqliteJournal>),
    /// The state directory's, which will open on the first mutation —or on
    /// the first question that needs the chain— and may not be openable.
    Lazy(Arc<crate::embedded::LazyJournal>),
}

/// Embedded core: registry of providers by scheme + operations. Reads
/// (`stat`/`list`) are direct; mutations (`copy`/`move_`/`delete`) are Tasks
/// with progress and cancellation.
pub struct Engine {
    /// Cache of providers + remote sessions' lifecycle (#47): per-process
    /// providers by scheme, remotes by `scheme://authority` (one session per
    /// host, with single-flight/eviction/backoff), and composite archive ones
    /// by `fmt+scheme://authority`.
    sessions: SessionPool,
    /// Establishes remote providers on demand (phase 6e, ADR 0015). With no
    /// connector, a scheme with no registered provider is `Unsupported`
    /// (M0/M1).
    connector: RwLock<Option<Arc<dyn crate::connect::RemoteConnector>>>,
    /// Observes connection warnings (#44: TLS degradation). With no observer,
    /// the warnings are dropped (the connector's `tracing::warn!` persists in
    /// the log).
    connection_observer: RwLock<Option<Arc<dyn crate::connect::ConnectionObserver>>>,
    sched: Scheduler,
    observer: Arc<dyn MutationObserver>,
    /// Whether someone already took the hook dispatcher (ADR 0100): it belongs to ONE.
    hooks_taken: std::sync::atomic::AtomicBool,
    /// Where this engine's journal comes from: the READ source for undo
    /// (M3-2) and for `sync.apply`'s gate. It is the SAME object as
    /// `observer` in the two variants that have one.
    ///
    /// Not an `Option<Arc<..>>` since #177 because the embedded arm does not
    /// yet know whether it HAS one: it opens it on the first mutation. Asking
    /// it is [`Self::journal`], which is `async` for exactly that reason.
    journal: JournalSource,
    /// The anchors of the directories the EMBEDDED BACKEND has listed (#301,
    /// ADR 0073), **if this engine belongs to a frontend** (#317).
    ///
    /// Lives here and not in `Backend` because `Backend::Embedded` is an
    /// `Arc` of the engine and nothing else: two of its clones —the one that
    /// lists a pane and the one that copies— share nothing else, and a
    /// per-clone memory would never see what the other listed. It is the
    /// equivalent of the `Inner` the SDK uses for the remote path.
    ///
    /// # `Option`, and that is the point
    ///
    /// An anchor says **who looked**, i.e. a human in front of a screen.
    /// That is only true in a process with ONE client: an embedded
    /// frontend's. In the daemon there are many, each with its own idea of
    /// what it is looking at, and a shared cache would pass client A's
    /// listing to client B's write — which is what ADR 0082 rejects among
    /// its alternatives.
    ///
    /// It used to be an always-present field whose rustdoc said "the daemon
    /// does not touch it". That was true, but it was held up by a prose
    /// promise and not the type: it took nothing more than someone mounting
    /// a `Backend::Embedded` over the daemon's engine —for an internal job,
    /// for plugins— for the promise to fall with nothing turning red. Now
    /// [`Self::with_client_anchors`] installs it, and the one that calls it
    /// is exactly [`crate::embedded::engine_in`], which is the frontend
    /// engines' constructor and the one the daemon does not use.
    anchors: Option<std::sync::Mutex<crate::anchor::AnchorCache>>,
    /// Policy gate consulted PRE-effect on every mutation (M3-3). Default
    /// [`AllowAll`](crate::policy::AllowAll): the embedded/human engine is
    /// not sandboxed unless a policy is installed with [`Self::with_policy`].
    policy: Arc<dyn crate::policy::PolicyGate>,
    /// Whether the `policy` above was installed by SOMEONE
    /// ([`Self::with_policy`]) or is the default one.
    ///
    /// Changes no decision: `AllowAll` gates equally permissively in both
    /// cases. It exists because the daemon has to be able to WARN about the
    /// second case (#166) — "permissive policy on purpose" and "policy
    /// nobody installed" are the same thing to the gate and different things
    /// to the operator.
    policy_explicit: bool,
    /// Resolves a policy `Ask`. Default [`DenyAll`](crate::approval::DenyAll)
    /// (headless fail-closed).
    approvals: Arc<dyn crate::approval::ApprovalResolver>,
    /// Anti-bomb limits for composite archive providers (#95.2). Default
    /// [`norte_vfs_archive::Limits::default`]; the operator lowers them via
    /// [`Self::set_archive_limits`] BEFORE the first navigation into a
    /// container (composite providers are cached with the limits in effect
    /// at their first use).
    archive_limits: RwLock<norte_vfs_archive::Limits>,
    /// FIXED executable for reading RAR (`[archive] rar_delegate`), or `None`
    /// to probe `PATH`. Set at startup with [`Self::set_rar_delegate`], never
    /// from the Project layer.
    rar_delegate: RwLock<Option<std::path::PathBuf>>,
    /// AI provider for the reviewable rename (M4-A2, ADR 0031). `None` = no
    /// AI (`ai_rename_plan` → `Unsupported`). Injected with
    /// [`Self::set_ai_provider`].
    ai_provider: RwLock<Option<norte_ai::SharedAiProvider>>,
    /// EMBEDDINGS provider (M4-IA-2, ADR 0031 A3). Separate from the chat
    /// one: `[ai].embed_provider` can name a different provider/model.
    /// `None` = no embeddings (`index.embed` → `Unsupported`). Injected with
    /// [`Self::set_ai_embed_provider`].
    ai_embed: RwLock<Option<norte_ai::SharedAiProvider>>,
    /// `[ai]` config (opt-in/local-only/denied-paths). Default disabled → the
    /// gate rejects every AI operation.
    ai_config: RwLock<crate::ai::AiConfig>,
    /// Search index (M4, ADR 0034). `None` = no index (`index.*` →
    /// `Unsupported`, fail-closed like AI). Injected with
    /// [`Self::with_index`]; the daemon installs it.
    index: Option<Arc<norte_index::Index>>,
    /// THE spool of retained sync plans (ADR 0049). `None` = no retention,
    /// and then `sync.plan` answers `Unsupported` (fail-closed, like the
    /// index): a plan that cannot be retained cannot be applied either, and
    /// serving it would show an approval dialog for something that
    /// afterward does not exist.
    ///
    /// Startup installs it with [`Self::set_spool`], **once and with a
    /// single `Spool::new`**: the registry of issued plans lives behind an
    /// `Arc` inside the handle, so a second `Spool::new` over the same
    /// directory is not another handle but a spool that recognizes not even
    /// one plan.
    spool: RwLock<Option<crate::sync::Spool>>,
    /// BOUNDED ring of rename batch reports, by `task_id`
    /// ([`Self::rename_batch_report`]).
    ///
    /// The report is retained HERE, and not in the daemon, because there are
    /// two consumers —the socket (`fs.rename_batch_report`) and the embedded
    /// `Backend`— and two rings would be two retention policies
    /// contradicting each other from the start. The stored actor is the
    /// task's OWNER: the daemon needs it to decide who can read it.
    batch_reports: std::sync::Mutex<std::collections::VecDeque<BatchReportEntry>>,
    /// BOUNDED ring of `sync.apply` reports, by `task_id`
    /// ([`Self::sync_report`]).
    ///
    /// The exact twin of `batch_reports` and for the same reason: there are
    /// two consumers —the socket (`sync.report`) and the embedded
    /// `Backend`— and two rings would be two retention policies
    /// contradicting each other from the start. The stored actor is the
    /// Task's OWNER; the daemon needs it to decide who can read it.
    sync_reports: std::sync::Mutex<std::collections::VecDeque<SyncReportEntry>>,
    /// BOUNDED ring of `archive.test` reports, by `task_id`
    /// ([`Engine::archive_test_report`]).
    ///
    /// The third of the same family and for the same reason: a Task cannot
    /// return a value, and what `archive.test` has to report —which entry
    /// failed and why— does not fit in a `Failed`. The same eviction, with
    /// the same "what counts for nothing goes first" rule.
    test_reports: std::sync::Mutex<std::collections::VecDeque<TestReportEntry>>,
    /// BOUNDED ring of undo reports, by `task_id`
    /// ([`Engine::undo_report`]).
    ///
    /// It used to live in the daemon, and was the only one of the family not
    /// here: the embedded `Backend` undid (`undo_after`) and had nowhere to
    /// read what came back from, so it answered `Unsupported` to its own undo.
    undo_reports: std::sync::Mutex<std::collections::VecDeque<UndoReportEntry>>,
    /// One undo at a time (#358). Taken by the whole undo Task, from the
    /// first step to the last: two undos that picked the same stack (a
    /// double click, two frontends, a retry) do not step on each other, and
    /// the second one, on entering, looks at the journal again and skips
    /// what the first already returned.
    ///
    /// Two limits, written so nobody takes them as closed:
    /// - An IN-FLIGHT `fs.rename_batch`'s rollback also writes compensations
    ///   and does not take this turn. An undo that picks entries from a
    ///   batch still running can cross paths with it; `is_free` and the
    ///   no-replace rename bound it, as before #358.
    /// - An agent's undo whose policy gate asks (`ask`) holds the turn while
    ///   it waits for approval (30 s in the daemon). A human undo waits
    ///   behind it. There is no deadlock —approving does not go through an
    ///   undo—, only waiting.
    undo_in_progress: Arc<tokio::sync::Mutex<()>>,
    /// BOUNDED ring of `archive.pack` reports, by `task_id`
    /// ([`Engine::archive_pack_report`]).
    ///
    /// Fourth of the family, and the one that stretches its reason
    /// furthest: the other three count what went WRONG, and this one counts
    /// something that went RIGHT and still has to be said — an `a\b.txt`
    /// saved, which on Windows is a `b.txt` inside an `a` folder. A
    /// `Completed` is true and does not cover it (#250).
    pack_reports: std::sync::Mutex<std::collections::VecDeque<PackReportEntry>>,
    /// BOUNDED ring of `fs.checksum` reports, by `task_id`
    /// ([`Self::checksum_report`]).
    ///
    /// Twin of `pack_reports` and for the same reason: there are two
    /// consumers —the socket (`fs.checksum_report`) and the embedded
    /// `Backend`— and two rings would be two retention policies
    /// contradicting each other from the start.
    checksum_reports: std::sync::Mutex<std::collections::VecDeque<ChecksumReportEntry>>,
    /// BOUNDED ring of `fs.dir_usage` reports, by `task_id`
    /// ([`Self::dir_usage_report`]).
    ///
    /// The sixth of the family. Here what does not fit in a Task's outcome
    /// is the LIST of measured children: `fs.dir_size` could return its
    /// total through progress because it was a number, and a map is not.
    dir_usage_reports: std::sync::Mutex<std::collections::VecDeque<DirUsageReportEntry>>,
}

/// The policy gate, capturable (#171).
///
/// Exists so a Task's body can ask ON ITS OWN, with no `&self`: it is what
/// separates "gate everything before starting" from "gate each unit as its
/// turn comes". See [`Engine::policy_checker`].
#[derive(Clone)]
struct PolicyChecker {
    policy: Arc<dyn crate::policy::PolicyGate>,
    approvals: Arc<dyn crate::approval::ApprovalResolver>,
}

impl PolicyChecker {
    /// The policy half of [`Self::gate`].
    async fn check(
        &self,
        actor: &crate::journal::Actor,
        op: crate::policy::PolicyOp,
        paths: &[&VPath],
    ) -> Result<(), Error> {
        use crate::policy::{Decision, DenyReason};
        let denied = |reason: DenyReason| Error::PolicyDenied {
            rule: reason.rule_id().to_owned(),
        };
        match self.policy.evaluate(actor, op, paths) {
            Decision::Allow => Ok(()),
            Decision::Deny(reason) => {
                tracing::info!(?reason, op = op.kind(), "policy denied the operation");
                Err(denied(reason))
            }
            // A plugin runs with nobody in front of it (ADR 0101): an `ask`
            // rule on it is a `deny` with its reason, not a modal nobody
            // looks at and that expires by TTL anyway.
            Decision::Ask if matches!(actor, crate::journal::Actor::Plugin { .. }) => {
                tracing::info!(
                    op = op.kind(),
                    "policy asks a plugin for confirmation: denied"
                );
                Err(denied(DenyReason::NotApproved))
            }
            Decision::Ask => {
                let req = crate::approval::ApprovalRequest {
                    actor: actor.clone(),
                    op,
                    // Redacted like spans (rule 10): they are display ONLY
                    // for the approving frontend, never reparsed.
                    //
                    // And CAPPED. The DECISION is made over the whole
                    // `paths` (above, `policy.evaluate`); what is trimmed is
                    // what is shown to the human. From the batch rename a
                    // single gate can carry thousands of paths, and this
                    // list is broadcast to every human connection and
                    // retained for the TTL: with no cap, an agent under an
                    // `ask` rule turns every request into megabytes of
                    // notification and evicts slow subscribers for a full
                    // outbox. No frontend paints that many paths anyway.
                    paths: paths
                        .iter()
                        .take(APPROVAL_PATHS_SHOWN)
                        .map(|p| span_path(p))
                        .collect(),
                    // And the TOTAL travels with them. Trimming the list is
                    // necessary; trimming it SILENTLY would turn the modal
                    // into a lie — the human would approve 32 innocent
                    // paths without knowing the decision covered eight
                    // thousand.
                    paths_total: paths.len() as u64,
                };
                match self.approvals.request(req).await {
                    crate::approval::ApprovalOutcome::Approved => Ok(()),
                    crate::approval::ApprovalOutcome::Denied
                    | crate::approval::ApprovalOutcome::TimedOut => {
                        Err(denied(DenyReason::NotApproved))
                    }
                }
            }
        }
    }
}

impl Engine {
    /// Engine with the no-op observer (the journal arrives in M3).
    #[must_use]
    pub fn new() -> Self {
        Self::with_observer(Arc::new(NoopObserver))
    }

    /// Engine with its own mutation observer (the journal's seam), with no
    /// undo source (`undo_session` → `Unsupported`).
    #[must_use]
    pub fn with_observer(observer: Arc<dyn MutationObserver>) -> Self {
        Self::build(observer, JournalSource::None)
    }

    /// The real constructor: the three public ones only choose WHICH
    /// observer and WHICH journal source, and the rest of the engine is
    /// identical in all three. Just one, and not three copies of twenty
    /// fields, because a new field forgotten in one copy is an engine with
    /// half its pieces.
    fn build(observer: Arc<dyn MutationObserver>, journal: JournalSource) -> Self {
        Self {
            sessions: SessionPool::new(),
            connector: RwLock::new(None),
            connection_observer: RwLock::new(None),
            sched: Scheduler::new(4),
            observer,
            journal,
            policy: Arc::new(crate::policy::AllowAll),
            policy_explicit: false,
            approvals: Arc::new(crate::approval::DenyAll),
            archive_limits: RwLock::new(norte_vfs_archive::Limits::default()),
            rar_delegate: RwLock::new(None),
            ai_provider: RwLock::new(None),
            ai_embed: RwLock::new(None),
            ai_config: RwLock::new(crate::ai::AiConfig::default()),
            index: None,
            spool: RwLock::new(None),
            batch_reports: std::sync::Mutex::new(std::collections::VecDeque::new()),
            sync_reports: std::sync::Mutex::new(std::collections::VecDeque::new()),
            test_reports: std::sync::Mutex::new(std::collections::VecDeque::new()),
            undo_reports: std::sync::Mutex::new(std::collections::VecDeque::new()),
            undo_in_progress: Arc::new(tokio::sync::Mutex::new(())),
            pack_reports: std::sync::Mutex::new(std::collections::VecDeque::new()),
            checksum_reports: std::sync::Mutex::new(std::collections::VecDeque::new()),
            dir_usage_reports: std::sync::Mutex::new(std::collections::VecDeque::new()),
            anchors: None,
            hooks_taken: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Gives this engine the anchor memory of ONE client (#301, #317).
    ///
    /// Only for an embedded frontend engine, where the only one listing is
    /// the human looking. [`crate::embedded::engine_in`] calls it; an engine
    /// that does not go through there —the daemon's— does not have it, and
    /// then a `Backend::Embedded` over it anchors nothing and writes behave
    /// as in 0.53. Failing that way is correct: losing the check is losing a
    /// check, and sharing it between clients would answer "who looked" with
    /// someone else's name.
    #[must_use]
    pub fn with_client_anchors(mut self) -> Self {
        self.anchors = Some(std::sync::Mutex::new(crate::anchor::AnchorCache::default()));
        self
    }

    /// Does this engine have client anchor memory ([`Self::with_client_anchors`])?
    ///
    /// Asked by the test that fixes the daemon's engine as NOT having it: the
    /// property is held up by the type, and this is what lets it be checked
    /// from outside instead of by reading the code.
    #[must_use]
    pub fn has_client_anchors(&self) -> bool {
        self.anchors.is_some()
    }

    /// Retains the anchor of the directory the EMBEDDED BACKEND just listed
    /// (#301). Does nothing with no client memory installed.
    ///
    /// A poisoned lock is swallowed silently, and that is the correct
    /// answer: what is lost is a write's CHECK, never the write. Panicking
    /// would turn another thread's failure into the listing's death.
    pub(crate) fn remember_dir_anchor(&self, dir: &VPath, anchor: Option<norte_proto::DirAnchor>) {
        let Some(anchors) = self.anchors.as_ref() else {
            return;
        };
        if let Ok(mut cache) = anchors.lock() {
            cache.remember(dir, anchor);
        }
    }

    /// `dir`'s retained anchor, if the embedded backend listed it (#301).
    pub(crate) fn remembered_dir_anchor(&self, dir: &VPath) -> Option<norte_proto::DirAnchor> {
        self.anchors.as_ref()?.lock().ok()?.get(dir)
    }

    /// Composes the RAR provider for `aref`, or says it cannot.
    ///
    /// The two negatives are item 11's boundary:
    ///
    /// - the interior has to be `file://` **with no authority**: the
    ///   delegate receives a filesystem path, and no such path exists for
    ///   an `sftp://` nor for an entry inside another archive;
    /// - with no `7z` or `unrar` installed there is no reader:
    ///   `Unsupported`, with the phrase naming what to install in the log
    ///   (the wire carries no prose).
    fn rar_provider_for(
        &self,
        aref: &norte_proto::ArchiveRef,
        key: String,
    ) -> Result<Arc<dyn Provider>, Error> {
        if aref.outer.scheme() != "file" || aref.outer.authority().is_some() {
            tracing::warn!(
                outer = %aref.outer.scheme(),
                "rar only mounts over a LOCAL file: the delegate needs a path"
            );
            return Err(Error::Unsupported);
        }
        let archive = norte_vfs_local::vpath_to_native(&aref.outer)?;
        let pinned = self
            .rar_delegate
            .read()
            .expect("rar_delegate lock is sound")
            .clone();
        let delegate = match pinned {
            Some(program) => norte_vfs_rar::Delegate::pinned(program),
            None => norte_vfs_rar::Delegate::discover().map_err(|e| {
                tracing::warn!(error = %e, "no RAR reader installed");
                Error::from(e)
            })?,
        };
        let provider: Arc<dyn Provider> = Arc::new(norte_vfs_rar::RarProvider::new(
            archive,
            delegate,
            norte_vfs_rar::RarLimits::default(),
        ));
        Ok(self.sessions.insert_composite(key, provider))
    }

    /// The executable that reads RAR, if the config FIXES one (`[archive]
    /// rar_delegate`). `None` = probe `PATH`.
    ///
    /// The value comes from the System/User layers and NEVER from Project: a
    /// repository does not choose which binary launches when entering it.
    ///
    /// # Panics
    /// If the internal lock is poisoned, like the rest of the engine's.
    pub fn set_rar_delegate(&self, program: Option<std::path::PathBuf>) {
        *self
            .rar_delegate
            .write()
            .expect("rar_delegate lock is sound") = program;
    }

    /// Sets the archive providers' anti-bomb limits (#95.2, ADR 0018's
    /// config→provider channel). Call at STARTUP, before the first
    /// navigation into a container: an already composed `ArchiveProvider`
    /// (cached by scheme) keeps the limits it was born with.
    ///
    /// # Panics
    /// If the internal lock is poisoned (another thread panicked mid-write)
    /// — unrecoverable, same criterion as the rest of the engine's locks.
    pub fn set_archive_limits(&self, limits: norte_vfs_archive::Limits) {
        *self
            .archive_limits
            .write()
            .expect("archive_limits lock is sound") = limits;
    }

    /// Engine whose observer AND undo source is the same `SqliteJournal`
    /// (M3-2). The journal is single-writer (spec §4): one Engine per file.
    #[must_use]
    pub fn with_journal(journal: Arc<crate::journal::SqliteJournal>) -> Self {
        Self::build(
            Arc::clone(&journal) as Arc<dyn MutationObserver>,
            JournalSource::Open(journal),
        )
    }

    /// EMBEDDED engine: its journal is the state directory's, and it opens
    /// on the first mutation (#177) — or on the first `undo`/`sync.apply`,
    /// which is the same thing through another door: those are the three
    /// things that need the chain.
    ///
    /// Same as [`Self::with_journal`], the observer and the undo source are
    /// THE SAME object; the difference is that here that object does not yet
    /// have a file open behind it, and may never get one (another process
    /// may hold the lock). Building it touches no disk and takes the
    /// journal from nobody: that is the whole point.
    ///
    /// `norte-tui` and `norte-cli` use it with no daemon, via
    /// [`crate::embedded::engine_in`].
    #[must_use]
    pub fn with_lazy_journal(journal: Arc<crate::embedded::LazyJournal>) -> Self {
        Self::build(
            Arc::clone(&journal) as Arc<dyn MutationObserver>,
            JournalSource::Lazy(journal),
        )
    }

    /// Plugs in the hooks (ADR 0100): every row this engine's journal commits
    /// is offered to `tx`, wherever the mutation comes from. With no journal
    /// there are no rows and no hooks — an engine with no journal has no
    /// undo either, and it is the same reason.
    ///
    /// With the embedded mode's lazy journal the endpoint is saved and set
    /// on every handle that opens; that is why it is `async`.
    pub async fn enable_hooks(&self, tx: crate::hooks::HookSender) {
        match &self.journal {
            JournalSource::None => {}
            JournalSource::Open(j) => j.set_hook_sender(tx),
            JournalSource::Lazy(l) => l.set_hook_sender(tx).await,
        }
    }

    /// Does this engine carry a journal (open or lazy)? With no rows there
    /// are no rows, and with no rows there are no hooks to dispatch.
    #[must_use]
    pub fn has_journal(&self) -> bool {
        !matches!(self.journal, JournalSource::None)
    }

    /// Claims the hook dispatcher slot: `true` the FIRST time, and only
    /// then. An engine has one dispatcher; a second one starting up would
    /// silently overwrite the first one's endpoint.
    pub fn claim_hooks_slot(&self) -> bool {
        !self
            .hooks_taken
            .swap(true, std::sync::atomic::Ordering::AcqRel)
    }

    /// Where "this session is not being recorded" warnings go (#177).
    ///
    /// No-op if this engine carries no lazy journal (the daemon's cannot be
    /// left with no journal: it refuses to start). Installing it is part of
    /// the frontend's startup, and arrives in time even if a mutation beats
    /// it to it — see
    /// [`crate::embedded::LazyJournal::set_warning_sink`]. Returns whether
    /// the sink was INSTALLED: `false` when this engine cannot be left
    /// without a journal midway (the daemon's) nor can have one
    /// (an `Engine::new()`). The `Backend` checks it so as not to hand a
    /// frontend a channel that is never going to ring and would make it
    /// believe it is covered.
    pub fn set_journal_warning_sink(
        &self,
        sink: Arc<dyn crate::embedded::JournalWarningSink>,
    ) -> bool {
        if let JournalSource::Lazy(l) = &self.journal {
            l.set_warning_sink(sink);
            return true;
        }
        false
    }

    /// This engine's journal, OPENING it if lazy and this is the first time
    /// it is asked for.
    ///
    /// `async` on purpose, and it is #177's knot: the three questions asked
    /// of this field —do I journal this mutation?, can I undo?, can I apply a
    /// sync plan?— arrive at different times, and two of them BEFORE the
    /// process has mutated anything. With laziness put only in the observer,
    /// those two would answer "no journal" about an engine that would open
    /// it just fine. Not here: asking is opening.
    ///
    /// That there are no two handles of the same file —nor two owners of the
    /// chain— is guaranteed by
    /// [`LazyJournal`](crate::embedded::LazyJournal)'s window: ONE, shared
    /// with the observer, with attempts serialized under its lock and the
    /// handle destroyed on release. Since #179 opening is no longer unique;
    /// what stays unique is the OWNER at every instant.
    async fn journal(&self) -> Option<Arc<crate::journal::SqliteJournal>> {
        match &self.journal {
            JournalSource::None => None,
            JournalSource::Open(j) => Some(Arc::clone(j)),
            JournalSource::Lazy(l) => l.get().await,
        }
    }

    /// Opens the lazy journal right now and says whether this session ends up
    /// recorded.
    ///
    /// For the caller that is about to mutate and needs to TELL the human
    /// beforehand (today: `norte ai rename`, which asks for confirmation to
    /// rename a whole directory with the names a model proposed). Without
    /// this, the answer would arrive after the yes.
    ///
    /// Takes the exclusive lock HERE, not on the first mutation, and this
    /// process keeps it until it releases it
    /// ([`LazyJournal::release`](crate::embedded::LazyJournal::release),
    /// which nobody calls on their own today): if what comes next is a
    /// question to the human, `norte daemon run` cannot start while they
    /// think it over. It only makes sense one step from mutating, and it is
    /// the price of the answer arriving before the yes and not after.
    ///
    /// **Skips #179's retry brake on purpose.** This is the one caller for
    /// which paying the lock's 250 ms wait is obviously worth it: answering
    /// `false` from a verdict half a minute old would tell the human "this
    /// is not going to be recorded" about a journal that right now is free,
    /// and with that in front of them they will decide not to.
    pub async fn ensure_journal(&self) -> bool {
        match &self.journal {
            JournalSource::None => false,
            JournalSource::Open(_) => true,
            JournalSource::Lazy(l) => l.acquire_now().await.is_some(),
        }
    }

    /// Releases the journal if it has gone `idle` unused (#179). `true` if
    /// the file is free on return.
    ///
    /// The daemon's engine and the one with no journaling answer `true`
    /// doing nothing: they have no window to release. The daemon's,
    /// moreover, is an owner on purpose — it refuses to start with no
    /// journal, so releasing it would be taking away from itself what it
    /// requires to have.
    ///
    /// # This is NOT cancel-safe (see
    /// [`LazyJournal::release`](crate::embedded::LazyJournal::release)).
    /// Run it whole, in the BODY of a `select!` branch, never in its
    /// condition.
    pub async fn release_journal_if_idle(&self, idle: std::time::Duration) -> bool {
        match &self.journal {
            JournalSource::None | JournalSource::Open(_) => true,
            JournalSource::Lazy(l) => l.release_if_idle(idle).await,
        }
    }

    /// The same, saying WHY not.
    ///
    /// `None` = this session DOES record, or this engine has no window to
    /// lose (the daemon's, or an `Engine::new()` that journals nothing by
    /// construction).
    ///
    /// Exists because since #178 the two reasons no longer mean the same
    /// thing and a `bool` confuses them: with `Busy` the operation HAPPENS
    /// with no record and it has to be warned about; with `Failed` the
    /// operation is going to be REFUSED by the engine's gate and warning
    /// would be the preamble to a question whose premise is false. `norte ai
    /// rename` checks it, which asks before letting a model rename a whole
    /// directory.
    ///
    /// Skips the retry brake, like [`Self::ensure_journal`] and for the same
    /// reason.
    pub async fn journal_obstacle(&self) -> Option<crate::embedded::NoJournal> {
        let JournalSource::Lazy(l) = &self.journal else {
            return None;
        };
        // ONE single attempt, and that is why `resolve_now` and not
        // `acquire_now` followed by `resolve`: that pair paid for two
        // openings, and if `SQLite`'s error text differed between them —it
        // is partly written by whoever can write the file— the sink would
        // receive two warnings for a single question.
        l.resolve_now().await.err()
    }

    /// Installs the policy gate and the approval resolver (M3-3): from here
    /// on, agent mutations are evaluated PRE-effect.
    #[must_use]
    pub fn with_policy(
        mut self,
        policy: Arc<dyn crate::policy::PolicyGate>,
        approvals: Arc<dyn crate::approval::ApprovalResolver>,
    ) -> Self {
        self.policy = policy;
        self.approvals = approvals;
        self.policy_explicit = true;
        self
    }

    /// Whether someone called [`Self::with_policy`] on this engine.
    ///
    /// The daemon consults it at startup: mounting over an engine with no
    /// explicit policy lets ANY actor through, agents included, and
    /// `sync.apply` under that gap is a call that rewrites a subtree (#166).
    /// It is not a gate — it is what is needed for the gap to show up in the
    /// log instead of in the surprise.
    #[must_use]
    pub fn has_explicit_policy(&self) -> bool {
        self.policy_explicit
    }

    /// Installs the search index (M4, ADR 0034). Without it, `index.*`
    /// answers `Unsupported` (fail-closed). The daemon installs it (the DB's
    /// single writer).
    #[must_use]
    pub fn with_index(mut self, index: Arc<norte_index::Index>) -> Self {
        self.index = Some(index);
        self
    }

    /// Installs THE sync plan spool (ADR 0049). Without it,
    /// [`Self::sync_plan_as`] answers [`Error::Unsupported`] (fail-closed).
    ///
    /// The handle is **cloned**, never rebuilt: the registry of what this
    /// process issued lives inside it and is what makes a plan applicable
    /// only by whoever produced it. Whoever calls this twice with two
    /// different `Spool::new`s orphans the first one's plans.
    ///
    /// # Panics
    /// Only if the internal lock is poisoned (another thread panicked
    /// mid-write) — unrecoverable, same criterion as the rest of the locks.
    pub fn set_spool(&self, spool: crate::sync::Spool) {
        *self.spool.write().expect("spool lock is sound") = Some(spool);
    }

    /// The installed spool, cloned. `None` = no retention.
    ///
    /// # Panics
    /// Only if the internal lock is poisoned.
    #[must_use]
    pub fn spool(&self) -> Option<crate::sync::Spool> {
        self.spool.read().expect("spool lock is sound").clone()
    }

    /// The ONE gate for every mutation of this engine: policy first, journal
    /// after.
    ///
    /// Evaluates the policy PRE-effect; an `Ask` suspends until approval.
    /// `Err` [`Error::PolicyDenied`] with the cause (`rule`) if denied — the
    /// wire distinguishes it from an OS/provider `PermissionDenied` (M3-3b).
    /// And after that, [`Self::journal_gate`]: an UNREADABLE journal refuses
    /// (#178).
    ///
    /// **That order, and not the other.** The journal is requested AFTER the
    /// policy has said yes, because requesting it takes the file's exclusive
    /// lock (#177) and an operation the policy was going to deny has no
    /// reason to take it away from the daemon.
    ///
    /// The check living HERE and not in every caller is what makes it
    /// complete: the engine's nine mutation points go through this function,
    /// and adding the ninth requires remembering nothing. Pinned by
    /// `toda_mutacion_pasa_por_el_gate_del_journal` in
    /// `tests/embedded_journal.rs`, which is what stops the ninth from being
    /// forgotten anyway.
    async fn gate(
        &self,
        actor: &crate::journal::Actor,
        op: crate::policy::PolicyOp,
        paths: &[&VPath],
    ) -> Result<(), Error> {
        self.policy_gate(actor, op, paths).await?;
        self.journal_gate().await
    }

    /// Refuses the mutation if this session's journal cannot be OPENED
    /// (#178).
    ///
    /// Only the `Failed` case: no permissions, corrupt, not-a-database, or
    /// from an era before today's chain. Continuing there would be mutating
    /// with no record and no undo, which hard rule 4 prohibits and which
    /// `norte daemon run` already refuses with that same entry — the
    /// asymmetry was the bug.
    ///
    /// **`Busy` does NOT refuse**, and that half is what stops the fix from
    /// being worse than the hole: the usual occupant is benign (a live
    /// daemon, another window) or transient (another `norte cp` from a
    /// script, a restarting daemon), and denying there would turn "there is
    /// a daemon" into "the file manager does not work" and would let a
    /// passing occupant take down a three-hour session.
    ///
    /// Engines with no lazy journal pass through unaffected: the daemon's
    /// (which does not start with no journal, so it already failed closed
    /// earlier) and an embedder's with `Engine::new()` (which records
    /// NOTHING by construction and for which there is no file to fix).
    ///
    /// # What this gate guarantees, and its deadline
    /// **"It was not unreadable the last time it was checked", and that can
    /// be up to [`FRENO_AFTER_FAILURE`](crate::embedded::FRENO_TRAS_FALLO) old.**
    /// #179's brake makes a `Busy` verdict be remembered for thirty seconds
    /// with no reopening; if in that window the file goes from BUSY to
    /// UNREADABLE —someone releases the lock and right after corrupts it—
    /// this gate keeps answering `Ok(())` from the old classification and
    /// that window's mutations go through with no record.
    ///
    /// This is accepted, and it is worth understanding why it is NOT a
    /// regression: a `Busy` fails open by design (above), and whoever can
    /// hold the lock keeps the session unrecorded **indefinitely**, not
    /// thirty seconds — it is the half of #178 that is still open and that
    /// #203 follows up on. A 30 s lag inside a permanent hole adds no
    /// capacity at all. What CANNOT be done is closing it by skipping the
    /// brake here: that returns `ESPERA_POR_EL_LOCK` for EVERY mutation
    /// while a daemon is alive, which is exactly the cost the brake exists
    /// not to pay.
    async fn journal_gate(&self) -> Result<(), Error> {
        let JournalSource::Lazy(lazy) = &self.journal else {
            return Ok(());
        };
        match lazy.resolve().await {
            Ok(_) | Err(crate::embedded::NoJournal::Busy) => Ok(()),
            Err(crate::embedded::NoJournal::Failed(reason)) => {
                // The reason carries the file and goes to the operator's
                // LOG; the category that crosses to the frontend carries
                // neither (see the variant's rustdoc).
                // "Operation" and not "mutation": since phase 7 this gate is
                // also crossed by a READ (`journal_page`, the timeline), and
                // telling the operator a mutation was refused when someone
                // just opened a screen is a log line that sends them looking
                // for a change that never happened.
                tracing::error!(
                    reason = %reason,
                    "operation refused: this session's journal cannot be opened (#178)"
                );
                Err(Error::JournalUnavailable)
            } // NO wildcard arm, and that is the fail-closed: `NoJournal` is
              // `#[non_exhaustive]` from outside the crate, but in here the
              // compiler demands exhaustiveness, so a NEW reason breaks the
              // build instead of sneaking through as "go ahead" via a `_`.
              // What cannot be classified does not journal, and what does
              // not journal does not mutate: let whoever adds the reason decide.
        }
    }

    async fn policy_gate(
        &self,
        actor: &crate::journal::Actor,
        op: crate::policy::PolicyOp,
        paths: &[&VPath],
    ) -> Result<(), Error> {
        self.policy_checker().check(actor, op, paths).await
    }

    /// The policy gate WITHOUT `&self`: two `Arc`s that DO fit inside a
    /// Task's `'static` body (#171).
    ///
    /// Asking the policy FROM INSIDE the Task —which is what the executor
    /// does going forward and what undo did not do— requires being able to
    /// capture the gate. It is the same thing `sync::exec::SyncTargets`
    /// already carries around to consult step by step.
    fn policy_checker(&self) -> PolicyChecker {
        PolicyChecker {
            policy: Arc::clone(&self.policy),
            approvals: Arc::clone(&self.approvals),
        }
    }

    /// Registers a provider under its scheme (overwrites the previous one if
    /// there was one).
    ///
    /// # Panics
    /// Never in practice: only from poisoning of the internal lock.
    pub fn register_provider(&self, provider: Arc<dyn Provider>) {
        self.sessions.register_process(provider);
    }

    /// Configures the remote provider connector (phase 6e, ADR 0015 A): faced
    /// with a remote `VPath` with no provider, the Engine asks it for the
    /// connection and caches the result by `scheme://authority`.
    ///
    /// # Panics
    /// Never in practice: only from poisoning of the internal lock.
    pub fn set_connector(&self, connector: Arc<dyn crate::connect::RemoteConnector>) {
        *self.connector.write().expect("connector lock is sound") = Some(connector);
    }

    /// Installs the connection warning observer (#44): the engine hands it
    /// every `ConnectionWarning` from a remote establishment. With no
    /// observer, the warnings are dropped (the connector's `tracing::warn!`
    /// stays in the log).
    ///
    /// # Panics
    /// Never in practice: only from poisoning of the internal lock.
    pub fn set_connection_observer(&self, observer: Arc<dyn crate::connect::ConnectionObserver>) {
        *self
            .connection_observer
            .write()
            .expect("connection_observer lock is sound") = Some(observer);
    }

    /// Installs an observer CHAINED to whichever one was there: `make`
    /// receives the previous one and returns the new one, which must forward
    /// to it whatever it receives.
    ///
    /// Exists because the slot is for ONE and there are TWO facts that come
    /// out through it —degradation (#44) and failure (#322)— that the
    /// frontend takes as separate channels. Before, the second installer
    /// overwrote the first and left its channel mute forever, silently.
    ///
    /// And it is ONE operation and not "read then put": with two calls, two
    /// concurrent installers read the same previous one and the second loses
    /// the first — the same mute failure, now with a race. Here the swap
    /// happens under the same write lock.
    ///
    /// # Panics
    /// Never in practice: only from poisoning of the internal lock.
    pub fn chain_connection_observer<F>(&self, make: F)
    where
        F: FnOnce(
            Option<Arc<dyn crate::connect::ConnectionObserver>>,
        ) -> Arc<dyn crate::connect::ConnectionObserver>,
    {
        let mut slot = self
            .connection_observer
            .write()
            .expect("connection_observer lock is sound");
        let previous = slot.take();
        *slot = Some(make(previous));
    }

    /// Registers `host:port`'s host key after explicit user confirmation
    /// (TOFU flow, `connection.trust_host_key` method).
    ///
    /// # Neither a policy gate nor a journal entry, and why (#204, rule 4)
    ///
    /// This writes `known_hosts`, which is the most sensitive file norte
    /// writes besides the journal and the keyring: it decides which host
    /// keys it will accept from now on. And it goes through neither the gate
    /// nor the journal. Both absences are deliberate and are stated here so
    /// nobody has to deduce them again:
    ///
    /// * **Who can reach it.** The daemon's dispatch rejects
    ///   `connection.trust_host_key` for any actor other than `Actor::User`,
    ///   with `INVALID_REQUEST`, the same as
    ///   `policy.grant_scope`/`decide`/`undo_session` (#66): blessing a
    ///   host's identity is an act of human governance, not a file
    ///   operation. An agent cannot reach it, so a scopes gate here would
    ///   defend a door that is already closed — and by paths, which is not
    ///   the dimension this permission is measured in. Through the embedded
    ///   API there are no agents: `Backend::Embedded` is only built by the
    ///   CLI and the TUI with no `--daemon`, and the MCP bridge ALWAYS goes
    ///   through the socket with `agent_session`.
    ///
    /// * **Classification (rule 4): `Irreversible`, with a reason.** The
    ///   journal describes the user's file tree and its undo returns it to a
    ///   previous state; `known_hosts` is not part of that tree, and
    ///   "untrusting a key" is not an operation this program offers nor one
    ///   an `undo_session` should be able to do blindly — retiring a host
    ///   key in an `undo` the user requested for ANOTHER reason would break
    ///   connections that had nothing to do with it. What IS left is a
    ///   trace: the connector re-verifies the fingerprint against the key
    ///   the host presents NOW (anti-TOCTOU, ADR 0015 D) and the decision is
    ///   made by a human in front of the
    ///   fingerprint.
    ///
    /// If an agent ever needed this door, what is needed is NOT a path
    /// scope: it is its own policy op, and then yes, a journal entry saying
    /// which key was accepted and when.
    ///
    /// # Errors
    /// [`Error::Unsupported`] with no connector; the connector's (e.g.
    /// [`Error::HostKeyMismatch`] if the host no longer presents that key).
    ///
    /// # Panics
    /// Never in practice: only from poisoning of the internal lock.
    pub async fn trust_host_key(
        &self,
        host: &str,
        port: Option<u16>,
        fingerprint: &str,
    ) -> Result<(), Error> {
        let connector = self
            .connector
            .read()
            .expect("connector lock is sound")
            .clone()
            .ok_or(Error::Unsupported)?;
        connector.trust_host_key(host, port, fingerprint).await
    }

    /// Saves for THIS session connection `conn`'s secret, which a human just
    /// typed (`connection.provide_secret` method, #325).
    ///
    /// # Why it exists, and what it does NOT do
    ///
    /// The secret resolver looks at an environment variable, the keyring, and
    /// an `age` file (ADR 0015). When an entry declares `secret = "prompt"`
    /// and none of the three has anything, the core cannot continue alone:
    /// it returns [`Error::SecretNeeded`] and the frontend asks. This door is
    /// where the answer comes back through.
    ///
    /// **The secret lives in memory and only until the daemon stops.** It is
    /// not written to `connections.toml`, nor to the keyring, nor to the
    /// `age` file; that "remembering" is another decision and this method
    /// does not make it.
    ///
    /// # Who can reach it, and classification
    ///
    /// The same two absences as [`Self::trust_host_key`], for the same
    /// reasons: the daemon's dispatch rejects it for any actor other than
    /// `Actor::User` (typing a password is a human act; an agent that could
    /// inject session credentials would be choosing which identity the user
    /// acts under), and there is no journal entry because it touches
    /// neither the file tree nor leaves anything to undo — it disappears
    /// when the daemon stops.
    ///
    /// # Errors
    /// [`Error::Unsupported`] with no connector; the connector's.
    ///
    /// # Panics
    /// Never in practice: only from poisoning of the internal lock.
    // `skip_all` and not `skip(self)`: the second argument is a PASSWORD,
    // and with `skip(self)` `tracing` would format it into the span. It is
    // written here and not only in the connector because this is the public
    // method, and the one someone will extend.
    #[tracing::instrument(level = "info", skip_all, fields(conn = %conn))]
    pub async fn provide_secret(&self, conn: &str, secret: &str) -> Result<(), Error> {
        // The empty string is rejected HERE and not only in the TUI's
        // dialog. A frontend leaving the confirm inert with an empty field
        // is presentation; an empty secret not entering the resolver is
        // policy, it lives in the core (rule 7), and without this guard a
        // client with a bug feeds in an empty one that —since the session
        // step goes first— covers up the other three sources until someone
        // stops the daemon.
        //
        // `PermissionDenied` and not a new variant: it is what
        // `ConnectError::SecretEmpty` (#320) degrades to a few steps down,
        // so saying the same here invents no new taxonomy — an empty
        // credential is a credential that does not authenticate.
        if secret.is_empty() {
            tracing::warn!("empty secret rejected");
            return Err(Error::PermissionDenied);
        }
        let connector = self
            .connector
            .read()
            .expect("connector lock is sound")
            .clone()
            .ok_or(Error::Unsupported)?;
        connector.provide_secret(conn, secret).await
    }

    /// The registration/cache key for `p`: process providers go by scheme;
    /// remote ones by `scheme://authority`.
    fn provider_key(p: &VPath) -> String {
        match p.authority() {
            Some(a) => format!("{}://{a}", p.scheme()),
            None => p.scheme().to_owned(),
        }
    }

    /// Closes `p`'s remote session (#140). `false` if there was none.
    ///
    /// A PROCESS scheme —`file://`, `mem://`, a provider-plugin's— is not
    /// closed: there is no session to release, and saying yes would be
    /// lying about something that stays exactly the same. The next
    /// operation on that authority connects again through the usual path:
    /// closing releases, it does not forbid.
    pub fn close_connection(&self, p: &VPath) -> bool {
        // Registered by the whole scheme = a process provider, not a session.
        if self.sessions.lookup(p.scheme()).is_some() {
            return false;
        }
        self.sessions.close(&Self::provider_key(p))
    }

    async fn provider_for(&self, p: &VPath) -> Result<Arc<dyn Provider>, Error> {
        let key = Self::provider_key(p);
        // First the process provider registered for the whole scheme
        // (local, tests' mem): it has priority and triggers no connections.
        if let Some(prov) = self.sessions.lookup(p.scheme()) {
            return Ok(prov);
        }
        if let Some(prov) = self.sessions.lookup(&key) {
            return Ok(prov);
        }
        // Archives as directories (ADR 0018): a composite scheme = a
        // provider by composition over the CONTAINER's provider. Before the
        // connector: the interior can be local or an already live
        // connection.
        if let Some(aref) = p.archive_split().map_err(|_| Error::InvalidPath)? {
            // #56: nested LAYER cap BEFORE composing anything — counts the
            // scheme's format tokens (peeled left→right, the same
            // longest-match as the split).
            let mut layers = 0usize;
            let mut sch = p.scheme();
            while let Some(f) = norte_proto::scheme_archive_format(sch) {
                layers += 1;
                sch = &sch[f.len() + 1..];
            }
            let max_nesting = self
                .archive_limits
                .read()
                .expect("archive_limits lock is sound")
                .max_nesting;
            if layers > max_nesting {
                tracing::warn!(layers, max_nesting, "archive nesting over the cap");
                return Err(Error::LimitExceeded {
                    limit: Error::LIMIT_NESTING.into(),
                });
            }
            // `rar` does not compose over an inner provider: the external
            // delegate needs a real PATH, so the interior has to be a local
            // `file://` with no authority. Anything else —sftp, s3, or an
            // archive inside another archive— is refused HERE, before
            // composing anything, instead of bringing in the whole
            // container for a download nobody asked for.
            if aref.format == "rar" {
                return self.rar_provider_for(&aref, key);
            }
            let format = match aref.format.as_str() {
                "tar" => norte_vfs_archive::Format::Tar,
                "zip" => norte_vfs_archive::Format::Zip,
                "tar+gz" => norte_vfs_archive::Format::TarGz,
                // A format from proto's whitelist with no provider here: a
                // core version older than the proto. Honest: unknown.
                _ => return Err(Error::Unsupported),
            };
            // #56: the outer one can itself be an archive path — layer by
            // layer recursion, bounded by the max_nesting gate above (never
            // unbounded).
            let inner = Box::pin(self.provider_for(&aref.outer)).await?;
            tracing::debug!(scheme = %p.scheme(), %key, "composing archive provider");
            // expect: poisoned = another thread panicked mid-write —
            // unrecoverable, the same convention as the rest of the
            // engine's locks (see `set_archive_limits`'s `# Panics`).
            let limits = *self
                .archive_limits
                .read()
                .expect("archive_limits lock is sound");
            let provider: Arc<dyn Provider> =
                Arc::new(norte_vfs_archive::ArchiveProvider::with_limits(
                    inner,
                    format,
                    p.scheme().to_owned(),
                    limits,
                ));
            // Double-check in the pool: if another request registered
            // first, its wins (the extra ArchiveProvider is only RAM).
            return Ok(self.sessions.insert_composite(key, provider));
        }
        let connector = self
            .connector
            .read()
            .expect("connector lock is sound")
            .clone()
            .ok_or(Error::Unsupported)?;
        let Some(authority) = p.authority() else {
            return Err(Error::Unsupported);
        };
        let observer = self
            .connection_observer
            .read()
            .expect("connection_observer lock is sound")
            .clone();
        // Canonical dedup (#47): resolves the canonical form BEFORE
        // dialing (a local read of connections.toml) — an alias of a live
        // session hits here and never opens a second one.
        let (cache_key, alias) = match connector.canonical_authority(p.scheme(), authority).await {
            Some(canonical) if canonical != authority => {
                let ckey = format!("{}://{canonical}", p.scheme());
                // alias_current re-reads the canonical one UNDER the lock:
                // if the session fell between the lookup and here, it never
                // re-inserts a dead `Arc` as an alias (review #47's MAJOR-2
                // finding).
                if let Some(prov) = self.sessions.alias_current(&ckey, key.clone()) {
                    return Ok(prov);
                }
                (ckey, Some(key))
            }
            _ => (key, None),
        };
        // The dial goes through the pool (#47): single-flight per key,
        // timeout, cancelable by the waiter's drop, backoff on transient
        // failures.
        self.sessions
            .connect_remote(cache_key, alias, p.scheme(), authority, connector, observer)
            .await
    }

    /// A node's metadata (direct, no Task).
    ///
    /// # Errors
    /// [`Error::Unsupported`] if there is no provider for the scheme; the provider's.
    pub async fn stat(&self, p: &VPath) -> Result<Entry, Error> {
        self.provider_for(p).await?.stat(p).await
    }

    /// A directory's listing (direct, no Task).
    ///
    /// # Errors
    /// [`Error::Unsupported`] if there is no provider for the scheme; the provider's.
    pub async fn list(&self, p: &VPath) -> Result<EntryStream, Error> {
        self.provider_for(p).await?.list(p).await
    }

    /// [`Self::stat`] with options (#108 block 2): per-entry attributes.
    ///
    /// # Errors
    /// [`Error::Unsupported`] if there is no provider for the scheme; the provider's.
    pub async fn stat_with(&self, p: &VPath, opt: &norte_vfs::ListOptions) -> Result<Entry, Error> {
        self.provider_for(p).await?.stat_with(p, opt).await
    }

    /// [`Self::list`] with options (#108 block 2): per-entry attributes.
    ///
    /// # Errors
    /// [`Error::Unsupported`] if there is no provider for the scheme; the provider's.
    pub async fn list_with(
        &self,
        p: &VPath,
        opt: &norte_vfs::ListOptions,
    ) -> Result<EntryStream, Error> {
        self.provider_for(p).await?.list_with(p, opt).await
    }

    /// `p`'s provider's attrs catalog, SANITIZED: `AttrCatalog::new` is the
    /// one path to the wire and also the embedded backend's (ADR 0039 §4 —
    /// an in-process catalog never sneaks through unsanitized).
    ///
    /// # Errors
    /// [`Error::Unsupported`] if there is no provider for the scheme.
    pub async fn attr_catalog(&self, p: &VPath) -> Result<norte_proto::AttrCatalog, Error> {
        Ok(norte_proto::AttrCatalog::new(
            self.provider_for(p).await?.attrs().to_vec(),
        ))
    }

    /// Injects the AI provider for the reviewable rename (M4-A2, ADR 0031).
    ///
    /// # Panics
    /// Only from poisoning of the internal lock (unrecoverable).
    pub fn set_ai_provider(&self, provider: norte_ai::SharedAiProvider) {
        *self.ai_provider.write().expect("ai_provider lock is sound") = Some(provider);
    }

    /// Injects the EMBEDDINGS provider for `index.embed` /
    /// `index.search_semantic` (M4-IA-2, ADR 0031 A3). Separate from
    /// [`Self::set_ai_provider`]: `[ai].embed_provider` can name a different
    /// provider/model than the rename's.
    ///
    /// # Panics
    /// Only from poisoning of the internal lock (unrecoverable).
    pub fn set_ai_embed_provider(&self, provider: norte_ai::SharedAiProvider) {
        *self.ai_embed.write().expect("ai_embed lock is sound") = Some(provider);
    }

    /// Sets the `[ai]` config (opt-in/local-only/denied-paths). Without it
    /// the gate rejects every AI operation (default disabled).
    ///
    /// # Panics
    /// Only from poisoning of the internal lock.
    pub fn set_ai_config(&self, config: crate::ai::AiConfig) {
        *self.ai_config.write().expect("ai_config lock is sound") = config;
    }

    /// Suggests a REVIEWABLE rename plan for `dir`'s files according to
    /// `instruction` (spec §9, ADR 0031). Mutates NOTHING — the plan is the
    /// product; applying it is N governed `fs.move`s (journal + undo +
    /// policy). The opt-in gate is evaluated BEFORE any name goes out to
    /// the provider; hostile names (non-UTF8) are rejected fail-loud
    /// without being sent.
    ///
    /// # Errors
    /// [`Error::Unsupported`] if no AI provider is installed;
    /// [`Error::PolicyDenied`] if the gate rejects (AI off, local-only over
    /// remote, or `dir` under a `denied_prefix`); the listing's or the
    /// provider's mapped to the wire's taxonomy.
    ///
    /// # Panics
    /// Only from poisoning of an internal lock (unrecoverable).
    pub async fn ai_rename_plan(
        &self,
        dir: &VPath,
        instruction: &str,
    ) -> Result<crate::ai::RenamePlan, Error> {
        self.ai_rename_plan_for(dir, instruction, &[]).await
    }

    /// [`Self::ai_rename_plan`] over a SUBSET of `dir` (#121).
    ///
    /// `only` are BASE names. Empty = the whole directory, which is what it
    /// did before this parameter existed.
    ///
    /// What it buys is not convenience: with first-class selection (#103),
    /// marking five files and asking for a plan sent the directory's
    /// thousand to the provider. That is more than what the human pointed
    /// at, and the AI gate exists precisely to bound what leaves the
    /// machine.
    ///
    /// A name not in the listing is IGNORED instead of rejecting the plan:
    /// between marking and asking, a file may have gone away, and punishing
    /// the reader for that race fixes nothing. If after filtering none are
    /// left, the provider is not called — a plan over nothing is not a
    /// question.
    ///
    /// # Errors
    /// [`Self::ai_rename_plan`]'s.
    ///
    /// # Panics
    /// Only from poisoning of an internal lock (unrecoverable).
    pub async fn ai_rename_plan_for(
        &self,
        dir: &VPath,
        instruction: &str,
        only: &[String],
    ) -> Result<crate::ai::RenamePlan, Error> {
        use futures::StreamExt;

        /// Cap on the accumulated reply (#M4 security): see the drain loop.
        const MAX_REPLY_BYTES: usize = 512 * 1024;

        // The cap is checked HERE and not only in the daemon's dispatch,
        // like `fs.set_mode`'s: `ntc` runs embedded by default, so a cap
        // that only lives on the wire does not protect the most used path.
        // What it bounds is an O(names × entries) filter over a DIRECT
        // call —no Task— that can only die by timeout.
        if only.len() > norte_proto::methods::AI_RENAME_NAMES_MAX {
            tracing::debug!(n = only.len(), "ai.rename_plan over the cap");
            return Err(Error::InvalidPath);
        }

        let provider = self
            .ai_provider
            .read()
            .expect("ai_provider lock is sound")
            .clone()
            .ok_or(Error::Unsupported)?;

        // PRE-content gate: nothing goes out until it passes (spec §9). The
        // config clone avoids holding the lock across the awaits.
        {
            let config = self
                .ai_config
                .read()
                .expect("ai_config lock is sound")
                .clone();
            crate::ai::AiGate::new(&config)
                .check(crate::ai::AiOp::Rename, provider.is_local(), &[dir])
                .map_err(|reason| ai_denied_to_error(&reason))?;
        }

        // Base names of the dir's files (through the provider, never the
        // FS directly — rule 9). Every entry whose path falls under a
        // `denied_prefix` is OMITTED (review #M4's security MINOR: the name
        // of a denied dir that is a direct child of `dir` must not go out —
        // the gate only checks `dir`).
        let denied = {
            let config = self.ai_config.read().expect("ai_config lock is sound");
            config.denied_prefixes.clone()
        };
        // The subset, as a SET: the filter below runs for every entry of
        // the listing, and a linear search over 4096 names in a
        // million-entry directory is minutes of CPU in a call that is not
        // a Task and can only die by timeout.
        let only_set: std::collections::HashSet<&[u8]> =
            only.iter().map(std::string::String::as_bytes).collect();
        let mut stream = self.list(dir).await?;
        let mut names = Vec::new();
        while let Some(item) = stream.next().await {
            let entry = item?;
            if denied
                .iter()
                .any(|prefix| crate::policy::is_under(prefix, &entry.path))
            {
                continue;
            }
            if let Some(name) = entry.path.file_name() {
                // The subset is filtered AGAINST THE LISTING and by bytes
                // (#121): the name the frontend marked has to exist here,
                // and comparing it as text would lose the non-UTF8 ones —
                // which are exactly the ones it matters most not to confuse.
                if !only_set.is_empty() && !only_set.contains(name.as_bytes()) {
                    continue;
                }
                names.push(name.clone());
            }
        }
        if names.is_empty() && !only.is_empty() {
            // What was marked is no longer there. The provider is not
            // called: a plan over nothing is not a question, and sending
            // the instruction with an empty list spends quota just to get
            // the same answer back.
            return Ok(crate::ai::RenamePlan::default());
        }

        let req = crate::ai::build_rename_prompt(&names, instruction)
            .map_err(|e| ai_to_proto_error(&e))?;
        // What is measured about the exchange, and what is NOT. Here it is
        // known whether the typed-output contract actually traveled
        // (`structured`), and without that there is no way to know whether
        // it is any use: a declared but ineffective capability is exactly
        // what this path had.
        //
        // Never the instruction, never the names, never the reply: they are
        // user data and the log is no place for them (rule 10). Only the
        // provider, the entry count, the time, and what happened.
        let structured = provider
            .capabilities()
            .contains(norte_ai::AiCaps::JSON_OUTPUT);
        let provider_id = provider.id();
        let started = std::time::Instant::now();
        // A connection failure is measured TOO: if only the path that gets
        // to parsing were measured, `structured` would say how the contract
        // is doing among exchanges that already worked, which is the wrong
        // sample — the ones that fall over on network or auth are the ones
        // most worth counting.
        let mut chat = match provider.chat(req).await {
            Ok(c) => c,
            Err(e) => {
                tracing::info!(
                    provider_id,
                    structured,
                    entries = names.len(),
                    ms = started.elapsed().as_millis(),
                    result = ai_error_category(&e),
                    "AI rename plan"
                );
                return Err(ai_to_proto_error(&e));
            }
        };
        // Reply cap (review #M4's security MAJOR): a compromised/MITM
        // endpoint can stream sub-1MiB deltas forever (http.rs's per-line
        // cap does not bound the ACCUMULATED total) → OOM. A legitimate
        // `[{from,to}]` plan fits well within 512 KiB.
        let mut reply = String::new();
        while let Some(delta) = chat.next().await {
            let delta = delta.map_err(|e| ai_to_proto_error(&e))?;
            if reply.len() + delta.len() > MAX_REPLY_BYTES {
                tracing::warn!(
                    max = MAX_REPLY_BYTES,
                    "AI provider reply over the cap; aborting"
                );
                return Err(Error::Internal { panic: false });
            }
            reply.push_str(&delta);
        }
        let plan = crate::ai::validate_rename_reply(&reply, &names);
        let category = plan
            .as_ref()
            .map_or_else(|e| ai_error_category(e), |_| "ok");
        tracing::info!(
            provider_id,
            structured,
            entries = names.len(),
            bytes = reply.len(),
            ms = started.elapsed().as_millis(),
            result = category,
            "AI rename plan"
        );
        plan.map_err(|e| ai_to_proto_error(&e))
    }

    /// AI ORGANIZE plan (phase 8, `ai.organize_plan`).
    ///
    /// The twin of [`Self::ai_rename_plan_for`], with the same gate, the same
    /// denied-prefix filtering, the same reply cap, and the same validation
    /// belt — the only thing that changes is that the destination can carry
    /// subdirectories, and that difference is checked by
    /// [`crate::ai::validate_organize_reply`] with the SAME function the
    /// core uses when executing.
    ///
    /// **Mutates nothing.** The plan is the product; applying it is
    /// [`Self::organize`].
    ///
    /// # Errors
    /// [`Self::ai_rename_plan_for`]'s: no provider, [`Error::Unsupported`];
    /// the AI gate; whatever the provider answers; and
    /// [`Error::InvalidPath`] if `only` exceeds the cap.
    ///
    /// # Panics
    /// No: the `expect`s are on this struct's own locks.
    pub async fn ai_organize_plan_for(
        &self,
        dir: &VPath,
        instruction: &str,
        only: &[String],
    ) -> Result<crate::ai::OrganizePlanReply, Error> {
        use futures::StreamExt;

        /// The same accumulated-reply cap as the rename plan, and for the
        /// same reason: a compromised endpoint can stream forever and the
        /// per-line cap does not bound the accumulated total.
        const MAX_REPLY_BYTES: usize = 512 * 1024;

        if only.len() > norte_proto::methods::AI_RENAME_NAMES_MAX {
            return Err(Error::InvalidPath);
        }
        let provider = self
            .ai_provider
            .read()
            .expect("ai_provider lock is sound")
            .clone()
            .ok_or(Error::Unsupported)?;
        // PRE-content gate: nothing goes out until it passes. Organize is
        // evaluated as `Rename` because that is what it is —proposing new
        // names for this directory's files—, and giving it its own `AiOp`
        // would force every existing config to allow it again for something
        // it had already decided.
        {
            let config = self
                .ai_config
                .read()
                .expect("ai_config lock is sound")
                .clone();
            crate::ai::AiGate::new(&config)
                .check(crate::ai::AiOp::Rename, provider.is_local(), &[dir])
                .map_err(|reason| ai_denied_to_error(&reason))?;
        }
        let denied = {
            let config = self.ai_config.read().expect("ai_config lock is sound");
            config.denied_prefixes.clone()
        };
        let only_set: std::collections::HashSet<&[u8]> =
            only.iter().map(std::string::String::as_bytes).collect();
        let mut stream = self.list(dir).await?;
        let mut names = Vec::new();
        while let Some(item) = stream.next().await {
            let entry = item?;
            if denied
                .iter()
                .any(|prefix| crate::policy::is_under(prefix, &entry.path))
            {
                continue;
            }
            if let Some(name) = entry.path.file_name() {
                if !only_set.is_empty() && !only_set.contains(name.as_bytes()) {
                    continue;
                }
                names.push(name.clone());
            }
        }
        if names.is_empty() && !only.is_empty() {
            return Ok(crate::ai::OrganizePlanReply::default());
        }
        let req = crate::ai::build_organize_prompt(&names, instruction)
            .map_err(|e| ai_to_proto_error(&e))?;
        let structured = provider
            .capabilities()
            .contains(norte_ai::AiCaps::JSON_OUTPUT);
        let provider_id = provider.id();
        let started = std::time::Instant::now();
        let mut chat = match provider.chat(req).await {
            Ok(c) => c,
            Err(e) => {
                tracing::info!(
                    provider_id,
                    structured,
                    entries = names.len(),
                    ms = started.elapsed().as_millis(),
                    result = ai_error_category(&e),
                    "AI organize plan"
                );
                return Err(ai_to_proto_error(&e));
            }
        };
        let mut reply = String::new();
        while let Some(delta) = chat.next().await {
            let delta = delta.map_err(|e| ai_to_proto_error(&e))?;
            if reply.len() + delta.len() > MAX_REPLY_BYTES {
                tracing::warn!(
                    max = MAX_REPLY_BYTES,
                    "AI provider reply over the cap; aborting"
                );
                return Err(Error::Internal { panic: false });
            }
            reply.push_str(&delta);
        }
        let plan = crate::ai::validate_organize_reply(&reply, &names);
        let category = plan
            .as_ref()
            .map_or_else(|e| ai_error_category(e), |_| "ok");
        // Never the instruction, never the names, never the reply: they are
        // user data and the log is no place for them (rule 10).
        tracing::info!(
            provider_id,
            structured,
            entries = names.len(),
            bytes = reply.len(),
            ms = started.elapsed().as_millis(),
            result = category,
            "AI organize plan"
        );
        plan.map_err(|e| ai_to_proto_error(&e))
    }

    /// Total entries omitted from `p`'s container's index (#93), `None` if
    /// the provider lists everything that exists (see
    /// [`norte_vfs::Provider::list_skipped`]).
    ///
    /// # Errors
    /// [`Error::Unsupported`] if there is no provider for the scheme; the provider's.
    pub async fn list_skipped(&self, p: &VPath) -> Result<Option<u64>, Error> {
        self.provider_for(p).await?.list_skipped(p).await
    }

    /// Directory `p`'s anchor (#295): the OPAQUE identity of the node a
    /// listing returns, so a copy writing there afterward can say what it was.
    ///
    /// Asked FOLLOWING links, because what the human was looking at is the
    /// directory whose content was listed, not the link it was reached
    /// through. A `~/backups -> /mnt/disk/backups` and `/mnt/disk/backups`
    /// give the same anchor, which is exactly what is needed: the same
    /// destination approved under two names cannot be two destinations.
    ///
    /// `None` = this provider does not know how to give a node identity (a
    /// bucket, an SFTP with no extensions). Then there is no anchor, the
    /// client sends none, and the write behaves as in 0.53.
    ///
    /// # Errors
    /// [`Error::Unsupported`] if there is no provider for the scheme; the provider's.
    pub async fn dir_anchor(&self, p: &VPath) -> Result<Option<norte_proto::DirAnchor>, Error> {
        let id = self
            .provider_for(p)
            .await?
            .node_id(p, norte_vfs::FollowLinks::Yes)
            .await?;
        Ok(id.map(crate::anchor::for_node))
    }

    /// Reading a file as a stream (direct, no Task), with an optional
    /// range — the viewer reads headers of huge files without swallowing
    /// the rest (ADR 0005).
    ///
    /// # Errors
    /// [`Error::Unsupported`] if there is no provider for the scheme; the provider's.
    pub async fn read(
        &self,
        p: &VPath,
        range: Option<norte_proto::ByteRange>,
    ) -> Result<norte_vfs::ByteStream, Error> {
        self.provider_for(p).await?.read(p, range).await
    }

    /// Live search under a subtree (spec §17.1a, M4): cancelable Task
    /// ([`TaskKind::Search`]) + `mpsc` channel of hit batches. The name
    /// matches by glob OR regex, the content by multi-encoding literal OR
    /// regex; at least one criterion, glob/regex and `content`/`content_regex`
    /// mutually exclusive per axis. The matchers are COMPILED here, BEFORE
    /// the Task: an invalid pattern or an illegal combination is a REQUEST
    /// error (not a failure of an already-launched Task).
    ///
    /// **Policy gate**: NONE. `fs.search` is pure READ and is treated
    /// EXACTLY like `fs.list`/`fs.read`, which do NOT go through the
    /// engine's gate (reads are direct — see [`Self::list`]/[`Self::read`]
    /// and `handle_fs_list` in the daemon, which call the provider with no
    /// `PolicyOp`). There is no read `PolicyOp`; introducing one just for
    /// `search` would let an agent LIST but not SEARCH the same subtree, a
    /// pointless asymmetry. The `actor` is propagated to the Task (for
    /// consistency with mutations and for future auditing), but it gates
    /// nothing.
    ///
    /// **What the `actor` DOES decide** (#165): the walk's
    /// [`walk_exclusions`](crate::policy::walk_exclusions). The daemon's
    /// read gate looks at the ROOT, so an AGENT's search over `$HOME`
    /// —legitimate— would descend into the daemon's state directory and
    /// return `journal.db` and the sync spools. It is not a gate: it is
    /// where not to descend, and the human carries none.
    ///
    /// **Progress mapping** (the TUI consumes it): `entries_done` = entries
    /// examined (including those skipped on error); `bytes_done` = number of
    /// accumulated hits (there are no real bytes in a search — the field is
    /// reused); `current` = last entry seen. `max_hits` reached ⇒ `Completed`
    /// (not `Failed`), the client infers "truncated" by comparing the total
    /// with the cap.
    ///
    /// # Errors
    /// [`Error::InvalidPath`] if the criteria do not validate/compile (the
    /// daemon translates it to `INVALID_PARAMS`; the sanitized detail is
    /// obtained with [`crate::search::SearchMatchers::compile`], which
    /// returns the glob/regex compiler's message). [`Error::Unsupported`] if
    /// `root`'s scheme has no registered provider.
    ///
    /// **What the actor decides, and what the request decides** (0.81.0):
    /// the POLICY's exclusions (`policy::walk_exclusions`) are computed
    /// first, and the ones `params.exclude_roots` brings are ADDED to them.
    /// They are added and not substituted, so a request can narrow the walk
    /// and cannot widen it: there is no way to lift a veto by putting paths
    /// in a list.
    pub async fn search_as(
        &self,
        params: norte_proto::methods::FsSearchParams,
        actor: crate::journal::Actor,
    ) -> Result<
        (
            TaskHandle,
            tokio::sync::mpsc::Receiver<norte_proto::methods::SearchHits>,
        ),
        Error,
    > {
        let matchers = crate::search::SearchMatchers::compile(&params).map_err(|e| {
            tracing::debug!(error = %e, "invalid fs.search criteria");
            Error::InvalidPath
        })?;
        let provider = self.provider_for(&params.root).await?;
        let root = params.root;
        // What an AGENT cannot walk even if its root is legitimate (#165):
        // the daemon's state directory hangs off `$HOME`, and the daemon's
        // read gate only looks at the search's root. The human is not
        // sandboxed, so they search their own files.
        let mut excluded = crate::policy::walk_exclusions(&actor);
        // And what the READER does not want to look at (0.81.0). They are
        // ADDED, in this order and with no way to be removed: the policy's
        // is what cannot be read, and this is a preference. A request does
        // not lift a veto by adding paths to a list.
        excluded.extend(params.exclude_roots.iter().cloned());
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let key = root.scheme().to_owned();
        let handle = self.sched.submit(
            &key,
            TaskKind::Search,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move {
                    crate::search::run_walk(provider, root, matchers, excluded, tx, &ctx).await
                })
            }),
        );
        Ok((handle, rx))
    }

    /// Compares TWO trees as a cancelable Task (`fs.compare`, 0.39.0, ADR
    /// 0048): [`TaskKind::Compare`] + an `mpsc` channel of row batches
    /// ([`CompareRowsBatch`](norte_proto::methods::CompareRowsBatch)),
    /// bounded by
    /// [`COMPARE_ROWS_MAX_BATCH`](norte_proto::methods::COMPARE_ROWS_MAX_BATCH)
    /// and coalesced, same as [`Self::search_as`].
    ///
    /// **Mutates nothing**: no journal, no undo, not a byte is written (hard
    /// rule 4 does not apply; the why, at length, is in the `compare` module,
    /// which is private and so is not linked).
    ///
    /// **Policy gate**: NONE here, for the same reason as
    /// [`Self::search_as`] — the READ gate lives in the daemon
    /// (`read_gate` over BOTH roots, plus `content_gate` when the hash rung
    /// is on), which is what ties a connection to an actor. Through the
    /// embedded API there is no `Actor::Agent` that the process itself did
    /// not write.
    ///
    /// **Progress mapping**: `entries_done` = ROWS emitted (C1's contract:
    /// it is how a client detects a lost `compare.rows`); `bytes_done` stays
    /// at zero — without the hash rung not a single byte is read.
    ///
    /// # Errors
    /// [`Error::InvalidPath`] if the two roots are the SAME (comparing
    /// something against itself is not a request, it is a typo, and it costs
    /// more than any other params error: an hour of work to answer "all the
    /// same"). [`Error::Unsupported`] if `follow_symlinks` comes in as
    /// `true` — the engine accepts the field and IGNORES it, so silently
    /// serving a walk different from the one requested would be a lie — or
    /// if the scheme of either root has no registered provider.
    pub async fn compare_as(
        &self,
        params: norte_proto::methods::FsCompareParams,
        actor: crate::journal::Actor,
    ) -> Result<
        (
            TaskHandle,
            tokio::sync::mpsc::Receiver<norte_proto::methods::CompareRowsBatch>,
        ),
        Error,
    > {
        // BEFORE resolving providers and creating any Task: the two
        // rejections are the REQUEST's, not failures of an already-launched
        // Task (same criterion as `search_as`'s matcher compilation).
        if params.follow_symlinks {
            tracing::debug!("fs.compare with follow_symlinks: not supported");
            return Err(Error::Unsupported);
        }
        // STRUCTURAL equality of `VPath` (scheme + authority + segments,
        // already normalized with no `.`/`..`). It does not detect two paths
        // the file system resolves to the same place — a symlinked root, a
        // file reached by two paths, the same SFTP host under two
        // authorities: that would require resolving real per-provider
        // identity, which is not in the trait today. Comparing a tree
        // against itself that way is not dangerous (nothing is written),
        // just expensive and with everything `Same`.
        if params.left == params.right {
            tracing::debug!("fs.compare of a root against itself");
            return Err(Error::InvalidPath);
        }
        let left = self.provider_for(&params.left).await?;
        let right = self.provider_for(&params.right).await?;
        let opts = norte_compare::CompareOptions {
            criteria: params.criteria,
            max_depth: params.max_depth,
            mtime_tolerance_ms: params.mtime_tolerance_ms,
            // Already rejected above; passed off explicitly so the engine
            // does not depend on that remote check.
            follow_symlinks: false,
            // `DescendSide` can only be one of the two sides: the typo that
            // would have given `Side::Unknown` — that is, descending into
            // NEITHER without saying so — dies in the wire deserializer, and
            // there is no check this arm (the embedded one, which does not
            // go through the daemon) could forget.
            descend_orphans: params.descend_orphans.map(norte_proto::methods::Side::from),
        };
        let (left_root, right_root) = (params.left, params.right);
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        // The scheduler queue is the LEFT root's (the one that launched the
        // comparison): a cross-provider comparison has to be queued
        // somewhere, and picking the other side would not change anything.
        let key = left_root.scheme().to_owned();
        let handle = self.sched.submit(
            &key,
            TaskKind::Compare,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move {
                    // The flow is built INSIDE: it borrows the two
                    // providers, so the borrow has to be born here.
                    crate::compare::run_compare(left, left_root, right, right_root, opts, tx, &ctx)
                        .await
                })
            }),
        );
        Ok((handle, rx))
    }

    /// How much what is passed to it occupies, as a cancelable Task
    /// (`fs.dir_size`, 0.49.0, #139).
    ///
    /// **Mutates nothing**: it walks and sums. No journal and no undo (rule 4
    /// does not apply), like [`Self::compare_as`].
    ///
    /// **The total is not returned here**: it travels in the Task's progress
    /// (`bytes_done`/`entries_done`), which is what a frontend already knows
    /// how to paint, and the last snapshot is the result.
    ///
    /// **Policy gate**: none here, for the same reason as
    /// `compare_as` — the READ gate lives in the daemon, which is what ties a
    /// connection to an actor.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidPath`] if not even one path is passed — measuring
    /// nothing is not a request — and whatever provider resolution returns.
    pub async fn dir_size_as(
        &self,
        params: norte_proto::methods::FsDirSizeParams,
        actor: crate::journal::Actor,
    ) -> Result<TaskHandle, Error> {
        // BEFORE creating any Task: it is a rejection of the REQUEST, not the
        // failure of an already-launched Task (same criterion as
        // `compare_as`).
        let Some(first) = params.paths.first() else {
            tracing::debug!("fs.dir_size with no paths");
            return Err(Error::InvalidPath);
        };
        // Overlapping roots are REJECTED, as in `fs.compare` and `sync.plan`
        // (#247). Without this, `["file:///a", "file:///a/b"]` counted `b`
        // twice and returned a number bigger than the space it occupies —
        // exactly the opposite of what the method exists for, which is
        // answering "does this fit in the destination?". It is rejected
        // instead of deduplicated because deduplicating is deciding on the
        // caller's behalf what they meant, and a pane's selection never
        // nests (they are siblings): whoever sends nested roots does it from
        // a script, and there an error is an answer.
        for (i, a) in params.paths.iter().enumerate() {
            for b in params.paths.iter().skip(i + 1) {
                if let Some(relation) = structural_overlap(a, b) {
                    tracing::debug!(?relation, "fs.dir_size with overlapping roots");
                    return Err(Error::OverlappingRoots { relation });
                }
            }
        }
        // The scheduler queue is the FIRST root's. A mixed selection of
        // providers has to be queued somewhere, and picking another one
        // would not change anything.
        let key = first.scheme().to_owned();
        let mut roots = Vec::with_capacity(params.paths.len());
        for p in params.paths {
            let provider = self.provider_for(&p).await?;
            roots.push((provider, p));
        }
        let handle = self.sched.submit(
            &key,
            TaskKind::DirSize,
            Priority::Normal,
            actor,
            Box::new(move |ctx| Box::pin(async move { crate::ops::dir_size(roots, &ctx).await })),
        );
        Ok(handle)
    }

    /// The digest of a batch of files' content, as a cancelable Task
    /// (`fs.checksum`, 0.59.0, #311).
    ///
    /// **Mutates nothing**: reading is not writing, so rule 4 does not apply
    /// — no journal and no undo. The READ gate lives in the daemon, which is
    /// what ties a connection to an actor, same as in `fs.dir_size`.
    ///
    /// The digests do NOT travel in the return value: they are collected
    /// with [`Self::checksum_report`], which is the only read path and the
    /// same split as `archive.pack`. Returning the `Arc` as well would leak a
    /// handle into the public API for a test's convenience.
    ///
    /// # Errors
    /// [`Error::InvalidPath`] with the empty list — summarizing nothing is
    /// not a request — or above
    /// [`FS_CHECKSUM_MAX_PATHS`](norte_proto::methods::FS_CHECKSUM_MAX_PATHS),
    /// which is REJECTED instead of truncated: a silently truncated report
    /// reads as "everything checked" for files nobody looked at.
    /// [`Error::Unsupported`] if some scheme has no provider.
    ///
    /// # Panics
    /// If the report ring's lock is poisoned, which is a prior panic of this
    /// same process — same criterion as `archive.pack`: the ring's WRITE path
    /// does not carry on with a state nothing is known about. The read one
    /// ([`Self::checksum_report`]) does tolerate it.
    pub async fn checksum_as(
        &self,
        params: norte_proto::methods::FsChecksumParams,
        actor: crate::journal::Actor,
    ) -> Result<TaskHandle, Error> {
        // BEFORE creating any Task: it is a rejection of the REQUEST, not the
        // failure of an already-launched Task (same criterion as
        // `dir_size_as`).
        let Some(first) = params.paths.first() else {
            tracing::debug!("fs.checksum with no paths");
            return Err(Error::InvalidPath);
        };
        if params.paths.len() > norte_proto::methods::FS_CHECKSUM_MAX_PATHS {
            tracing::debug!(n = params.paths.len(), "fs.checksum above the cap");
            return Err(Error::InvalidPath);
        }
        // The scheduler queue is the FIRST path's, as in `dir_size_as`: a
        // selection spanning several providers has to be queued somewhere.
        let key = first.scheme().to_owned();
        let mut paths = Vec::with_capacity(params.paths.len());
        for p in params.paths {
            let provider = self.provider_for(&p).await?;
            paths.push((provider, p));
        }
        // The report is born already knowing WHAT is being computed: it can
        // be requested without having sent the request (`task.list` shows
        // other actors' tasks), and a reader that assumed sha256 by default
        // would paint digests of something else.
        let report = Arc::new(std::sync::Mutex::new(
            norte_proto::methods::FsChecksumReportResult {
                algo: params.algo,
                ..Default::default()
            },
        ));
        let owner = actor.clone();
        let live = Arc::clone(&report);
        let handle = self.sched.submit(
            &key,
            TaskKind::Checksum,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move { crate::ops::checksum(paths, live, &ctx).await })
            }),
        );
        {
            let mut ring = self
                .checksum_reports
                .lock()
                .expect("checksum_reports lock is sound");
            ring.push_back((handle.id(), owner, report));
            evict_checksum_reports(&mut ring);
        }
        Ok(handle)
    }

    /// Report of an already-launched `fs.checksum`, by `task_id`, plus the
    /// ACTOR that requested it (#311). `None` if that id was never a batch of
    /// checksums for this instance, or if the ring already evicted it.
    ///
    /// It is a SNAPSHOT: final once the Task is terminal, partial before —
    /// which is exactly what makes it useful to request while it runs. The
    /// actor comes out with it because whoever serves this over the wire has
    /// to decide whether the requester could see that task.
    ///
    /// This path does NOT panic on a poisoned lock: it is a READ path, same
    /// as its twins.
    #[must_use]
    pub fn checksum_report(
        &self,
        task_id: TaskId,
    ) -> Option<(
        crate::journal::Actor,
        norte_proto::methods::FsChecksumReportResult,
    )> {
        let ring = self
            .checksum_reports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ring.iter()
            .find(|(id, _, _)| *id == task_id)
            .map(|(_, owner, r)| {
                (
                    owner.clone(),
                    r.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone(),
                )
            })
    }

    /// What a directory is made of, child by child, as a cancelable Task
    /// (`fs.dir_usage`, 0.75.0, phase 4).
    ///
    /// **Mutates nothing**: measuring is not writing, so rule 4 does not
    /// apply — no journal and no undo. The READ gate lives in the daemon,
    /// which is what ties a connection to an actor, same as in `fs.dir_size`.
    ///
    /// The children do NOT travel in the return value: they are collected
    /// with [`Self::dir_usage_report`], which is the only read path. This is
    /// the difference with `fs.dir_size`, whose total fit in the progress
    /// because it was a number; a LIST does not fit there.
    ///
    /// # Errors
    /// [`Error::InvalidPath`] with `depth` at zero — describing zero levels
    /// is not a request — or above
    /// [`DIR_USAGE_MAX_DEPTH`](norte_proto::methods::DIR_USAGE_MAX_DEPTH).
    /// [`Error::Unsupported`] with any `depth` greater than one: today only
    /// one level is served, and **it is REJECTED instead of truncated** — a
    /// server that silently truncates leaves the client believing it has the
    /// two levels it asked for. [`Error::Unsupported`] also if the scheme has
    /// no provider.
    ///
    /// # Panics
    /// If the report ring's lock is poisoned, which is a prior panic of this
    /// same process — same criterion as its twins: the ring's WRITE path does
    /// not carry on with a state nothing is known about. The read one
    /// ([`Self::dir_usage_report`]) does tolerate it.
    pub async fn dir_usage_as(
        &self,
        params: norte_proto::methods::FsDirUsageParams,
        actor: crate::journal::Actor,
    ) -> Result<TaskHandle, Error> {
        // BEFORE creating any Task: it is a rejection of the REQUEST, not the
        // failure of an already-launched Task (same criterion as
        // `checksum_as`).
        if params.depth == 0 || params.depth > norte_proto::methods::DIR_USAGE_MAX_DEPTH {
            tracing::debug!(depth = params.depth, "fs.dir_usage with depth out of range");
            return Err(Error::InvalidPath);
        }
        if params.depth > 1 {
            tracing::debug!(
                depth = params.depth,
                "fs.dir_usage: today only one level is served"
            );
            return Err(Error::Unsupported);
        }
        let key = params.path.scheme().to_owned();
        let provider = self.provider_for(&params.path).await?;
        let report = Arc::new(std::sync::Mutex::new(
            norte_proto::methods::FsDirUsageReportResult::default(),
        ));
        let owner = actor.clone();
        let live = Arc::clone(&report);
        let root = params.path;
        let handle = self.sched.submit(
            &key,
            TaskKind::DirUsage,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move { crate::ops::dir_usage(provider, root, live, &ctx).await })
            }),
        );
        {
            let mut ring = self
                .dir_usage_reports
                .lock()
                .expect("dir_usage_reports lock is sound");
            ring.push_back((handle.id(), owner, report));
            evict_dir_usage_reports(&mut ring);
        }
        Ok(handle)
    }

    /// The map an already-launched `fs.dir_usage` has measured so far, by
    /// `task_id`, plus the ACTOR that requested it (phase 4). `None` if that
    /// id was never a map for this instance, or if the ring already evicted
    /// it.
    ///
    /// It is a SNAPSHOT: final once the Task is terminal, partial before —
    /// which is exactly what makes it useful to request while it runs,
    /// because a map can be painted incrementally. The actor comes out with
    /// it because whoever serves this over the wire has to decide whether
    /// the requester could see that task.
    ///
    /// This path does NOT panic on a poisoned lock: it is a READ path, same
    /// as its twins.
    #[must_use]
    pub fn dir_usage_report(
        &self,
        task_id: TaskId,
    ) -> Option<(
        crate::journal::Actor,
        norte_proto::methods::FsDirUsageReportResult,
    )> {
        let ring = self
            .dir_usage_reports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ring.iter()
            .find(|(id, _, _)| *id == task_id)
            .map(|(_, owner, r)| {
                (
                    owner.clone(),
                    r.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone(),
                )
            })
    }

    /// Plans a ONE-way synchronization as a cancelable Task
    /// (`sync.plan`, 0.40.0, ADR 0049): [`TaskKind::SyncPlan`] + a channel of
    /// [`SyncPlanEvent`](crate::sync::SyncPlanEvent) — step batches bounded
    /// by [`SYNC_STEPS_MAX_BATCH`](norte_proto::methods::SYNC_STEPS_MAX_BATCH)
    /// and, at the end, ONE
    /// [`SyncPlanDone`](norte_proto::methods::SyncPlanDone). A single channel
    /// for both: the order "the steps, then the close" is contract, and a
    /// FIFO queue guarantees it without anyone having to reorder.
    ///
    /// **Mutates nothing**: planning is [`Self::compare_as`]'s comparison
    /// with a decision per row. What writes is `sync.apply` (hard rule 4 does
    /// not apply here; the long explanation is in the `sync` module).
    ///
    /// **Retains**, though: the plan is written to the spool as it is
    /// planned and stays tied to `conn_id`, which is what lets `sync.apply`
    /// carry nothing but a hash. Without a spool installed
    /// ([`Self::set_spool`]) this is [`Error::Unsupported`].
    ///
    /// **Policy gate**: NONE here, for the same reason as
    /// [`Self::compare_as`] — the READ gate lives in the daemon (`read_gate`
    /// over BOTH roots, plus `content_gate` when the hash rung is on), which
    /// is what ties a connection to an actor.
    ///
    /// # The two roots cannot overlap, and it is checked TWICE
    /// Copying `/a` onto `/a/sub` copies a tree into itself.
    /// [`FS_COMPARE`](norte_proto::methods::FS_COMPARE) does allow that pair
    /// — comparing only costs a walk and writes not a byte —; planning
    /// writes inside the source itself does not have that license.
    ///
    /// 1. **Structural**: scheme, authority and segments, literally. Catches
    ///    the two roots being equal and one being inside the other.
    /// 2. **Real identity**: [`Provider::node_id`] of the two roots,
    ///    resolving links. Two different paths naming the same directory —
    ///    `/data` being a symlink to `/srv/data` — respond with the same id,
    ///    and that is
    ///    [`RootOverlap::Same`](norte_proto::RootOverlap::Same). It is what
    ///    the structural check cannot see and neither can the walk's guard:
    ///    a walk's rows over `/data` all hang from `/data` and never
    ///    "reach" the other root.
    /// 3. **FOLDED containment**, when either provider does not declare
    ///    `CASE_SENSITIVE`: the same segments, compared by the key
    ///    `norte-compare` matches names with. It is what catches
    ///    `source=/Data` against `dest=/data/backup` on APFS or NTFS — three
    ///    distinct directories for the bytes and one inside another for the
    ///    volume.
    ///
    /// What remains UNCOVERED, said here instead of overpromised:
    ///
    /// - **Folded containment on a volume that distinguishes case but folds
    ///   something else.** ext4 with `+F` folds and ext4 without it does
    ///   not, and `Capabilities` does not distinguish it per directory; and
    ///   #145's folding (`ß`→`ss`) expands, so neither of the two tables
    ///   covers it.
    /// - **The same host under two authorities**: `node_id` is `None` on
    ///   SFTP and FTP, so there is no identity to compare there.
    /// - **Two different providers**: only ids of the SAME provider object
    ///   are compared, because the [`NodeId`](norte_vfs::NodeId) of two
    ///   backends is not comparable (a synthetic provider's index and an
    ///   ext4 inode can coincide without meaning anything).
    ///
    /// What is left is bounded by the walk's guard (`OverlapDetected` prunes
    /// the subtree as soon as a row reaches the other root) and the
    /// executor's per-step revalidation.
    ///
    /// # Errors
    /// [`Error::Unsupported`] if there is no spool installed, if the scheme
    /// of either root has no provider, or if the caller sent
    /// `compare.follow_symlinks`/`compare.descend_orphans` — which in
    /// `sync.plan` **are not theirs to set**: the planner fixes the latter to
    /// the SOURCE side, and silently serving a walk different from the one
    /// requested is worse than not offering it. [`Error::InvalidPath`] if
    /// `include` goes over
    /// [`SYNC_MAX_INCLUDE`](norte_proto::methods::SYNC_MAX_INCLUDE) (it is
    /// refused, never truncated: a silently shortened list synchronizes
    /// something nobody asked for). [`Error::OverlappingRoots`] if the roots
    /// overlap.
    pub async fn sync_plan_as(
        &self,
        params: norte_proto::methods::SyncPlanParams,
        conn_id: u64,
        actor: crate::journal::Actor,
    ) -> Result<
        (
            TaskHandle,
            tokio::sync::mpsc::Receiver<crate::sync::SyncPlanEvent>,
        ),
        Error,
    > {
        use norte_proto::methods::{SYNC_MAX_INCLUDE, Side};

        // With no retention there is no plan to approve: fail-closed BEFORE
        // touching a provider.
        let spool = self.spool().ok_or(Error::Unsupported)?;
        // REQUEST rejections, before resolving providers and creating any
        // Task (same criterion as `compare_as`). They go here and not only in
        // the daemon because the EMBEDDED arm calls this method without
        // going through it.
        if params.compare.follow_symlinks {
            tracing::debug!("sync.plan with follow_symlinks: not supported");
            return Err(Error::Unsupported);
        }
        if params.compare.descend_orphans.is_some() {
            tracing::debug!("sync.plan with descend_orphans: not the caller's to set");
            return Err(Error::Unsupported);
        }
        if params
            .include
            .as_ref()
            .is_some_and(|inc| inc.len() > SYNC_MAX_INCLUDE)
        {
            tracing::debug!("sync.plan: include above the cap");
            return Err(Error::InvalidPath);
        }
        if let Some(relation) = structural_overlap(&params.source, &params.dest) {
            tracing::debug!(?relation, "sync.plan with overlapping roots");
            return Err(Error::OverlappingRoots { relation });
        }

        let source = self.provider_for(&params.source).await?;
        let dest = self.provider_for(&params.dest).await?;
        if same_node(&source, &params.source, &dest, &params.dest).await {
            tracing::debug!("sync.plan: the two roots are the same directory");
            return Err(Error::OverlappingRoots {
                relation: norte_proto::RootOverlap::Same,
            });
        }
        // 3rd gate: the containment a FOLDING volume sees. It is asked of
        // EACH ROOT and not of the provider (ADR 0054): the two can be on
        // different mounts of the same `file://`, and it is the mount that
        // does not distinguish case — or that also EXPANDS, an ext4 with
        // `+F` — that decides whether these two roots overlap.
        let (source_caps, dest_caps) = tokio::join!(
            source.capabilities_at(&params.source),
            dest.capabilities_at(&params.dest)
        );
        // A root that cannot answer does NOT bring planning down: the
        // provider's own is declared, same as in `compare::probed_sides`,
        // and the root keeps failing where it has to fail — its own
        // listing, with its own error row. Planning toward a destination
        // that does not exist yet is the ordinary case for a mirror, and
        // bringing it down here would be a wire method that starts failing
        // where it used to answer.
        let caps = crate::compare::degraded(dest_caps, dest.as_ref(), &params.dest);
        let sides = norte_compare::Sides::from_capabilities(
            crate::compare::degraded(source_caps, source.as_ref(), &params.source),
            caps,
        );
        if sides.folds_case()
            && let Some(relation) = folded_overlap(&params.source, &params.dest, sides)
        {
            tracing::debug!(?relation, "sync.plan with roots overlapping once folded");
            return Err(Error::OverlappingRoots { relation });
        }
        let opts = norte_sync::SyncOptions {
            source_root: params.source.clone(),
            dest_root: params.dest,
            mode: params.mode,
            on_unknown: params.on_unknown,
            // The frontend already translated the direction: here the
            // source is the LEFT side of the comparison by construction, and
            // `SyncPlanParams` carries no `Side` that could contradict it.
            source_side: Side::Left,
            dest_has_trash: caps.flags.contains(norte_proto::CapabilityFlags::TRASH),
            // What the provider PROMISES about its trash, not what is
            // assumed of it: one that does not name its destination leaves
            // undo without a `reversal_ref`, and the plan has to mark it
            // IRREVERSIBLE before anyone approves anything (hard rule 4). It
            // comes from the same provider object the capabilities come
            // from, after the async operation that forced the lazy probe.
            dest_trash_restorable: dest.trash_restorable(),
            dest_writable: !caps.flags.contains(norte_proto::CapabilityFlags::READ_ONLY),
        };
        let job = crate::sync::SyncPlanJob {
            source,
            dest,
            opts,
            compare: params.compare,
            include: params.include,
            spool,
            conn_id,
        };
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        // The scheduler queue is the SOURCE's, for the same reason as in
        // `compare_as`: it has to be queued somewhere and the other side
        // does not change anything.
        let key = params.source.scheme().to_owned();
        let handle = self.sched.submit(
            &key,
            TaskKind::SyncPlan,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move { crate::sync::run_sync_plan(job, tx, &ctx).await })
            }),
        );
        Ok((handle, rx))
    }

    /// Executes an APPROVED plan (`sync.apply`, 0.40.0, ADR 0049) as a
    /// cancelable Task: [`TaskKind::Sync`] plus the report that fills in as
    /// it progresses.
    ///
    /// **The only parameter is the hash**, and everything else comes out of
    /// that. The two roots, the mode and the criteria are read from the
    /// SPOOL, which is where the approved plan stayed retained and tied to
    /// `conn_id`: by the SHAPE of the request, nothing can be executed other
    /// than what a human saw.
    ///
    /// # The gate runs over the roots that come out of the SPOOL, here and now
    /// `sync.plan`'s does NOT count: between planning and applying, up to
    /// [`SYNC_PLAN_TTL_MS`](norte_proto::methods::SYNC_PLAN_TTL_MS)
    /// milliseconds pass, and within that window a scope expires and a
    /// `policy.toml` rule changes. So they are requested here, with the
    /// paths read from the file:
    ///
    /// - [`PolicyOp::Copy`](crate::policy::PolicyOp::Copy) over BOTH roots
    ///   — copying is reading the source and writing the destination, and it
    ///   is exactly what [`Self::copy_with_as`] asks for to copy a tree —,
    /// - [`PolicyOp::Mkdir`](crate::policy::PolicyOp::Mkdir) over the
    ///   destination if the plan creates any directory,
    /// - [`PolicyOp::Delete`](crate::policy::PolicyOp::Delete) over the
    ///   destination if the plan overwrites or deletes, with the mode that
    ///   is actually going to be used (trash or permanent, according to what
    ///   the destination declares).
    ///
    /// **It gates over the ROOTS, not over each step**, same as a recursive
    /// copy from `fs.copy`: an agent's scope boundary is per root, so a step
    /// cannot escape a scope that covers `dest_root`. What IS left out is a
    /// `deny` rule of `policy.toml` over a SPECIFIC path inside the tree — a
    /// per-step gate would put an `ask` per step in a half-million-step plan,
    /// which is not an interface. It is the same coverage `fs.copy` of a
    /// tree has today.
    ///
    /// # Hard rule 4: with no journal, nothing is applied
    /// [`Error::Unsupported`], fail-closed like the spool. The plan promises
    /// a [`StepReversal`](norte_proto::methods::StepReversal) per step and
    /// only the journal can fulfill it; applying it without one would be
    /// overwriting and burying with no trace and no way back.
    ///
    /// The third leg is NOT fail-closed and it is worth not assuming so: an
    /// `Engine`'s default gate is [`AllowAll`](crate::policy::AllowAll), so a
    /// daemon mounted on an engine WITHOUT `with_policy` lets any actor
    /// rewrite an entire tree with a single call. It is the same open door
    /// `fs.copy` and `fs.delete` have had since M3 — and no binary in this
    /// tree leaves it that way — but here the radius is different.
    ///
    /// # The plan is spent no matter what
    /// When the Task ends — completed, failed or cancelled — the spool is
    /// erased. As long as it is not erased, that hash counts as "applying"
    /// and replanning the same tree with the same options (which gives the
    /// same digest) is refused.
    ///
    /// # Errors
    /// [`Error::JournalUnavailable`] if this session's journal cannot be
    /// OPENED (#178: unreadable ≠ busy — with the file simply busy the
    /// answer is `Unsupported`, same as always).
    /// [`Error::Unsupported`] with no spool or no journal, or if the scheme
    /// of either root has no provider; [`Error::PlanStale`] if the hash does
    /// not name a plan alive for THIS connection (it does not exist, it
    /// expired, it was tampered with, or it is already being applied);
    /// [`Error::PlanNotExecutable`] if the plan carried blocks;
    /// [`Error::PolicyDenied`] if the gate denies; [`Error::Io`] if the spool
    /// cannot be read because of a daemon failure.
    ///
    /// # Panics
    /// Only from poisoning of the report's `Mutex` (another thread panicked
    /// while holding it) — unrecoverable, same criterion as the rest of the
    /// core.
    #[tracing::instrument(skip(self, actor), fields(conn_id = conn_id, plan_hash = plan_hash.as_str()))]
    pub async fn sync_apply_as(
        &self,
        plan_hash: &PlanHash,
        conn_id: u64,
        actor: crate::journal::Actor,
    ) -> Result<
        (
            TaskHandle,
            Arc<std::sync::Mutex<norte_proto::methods::SyncReportResult>>,
        ),
        Error,
    > {
        let spool = self.spool().ok_or(Error::Unsupported)?;
        // An UNREADABLE journal is said with its own category (#178) and not
        // as "unsupported": there is a specific file to fix and the user has
        // a right to be told. It goes before the `ok_or` because both paths
        // end with no journal and only one of them is actionable.
        //
        // This is the ONLY place where the journal is requested before the
        // policy, the reverse of what `gate`'s rustdoc says, and it is
        // needed: the batch (`alloc_batch`) and the `BatchJournal` are built
        // below with this handle, and the plan is single-use — discovering
        // here that there is no journal after having spent the approval
        // would leave the human with no plan and no sync.
        //
        // What that order would cost — holding the exclusive lock while an
        // `Ask` from policy waits on a human — cannot happen, and not by
        // luck: the only engine with a LAZY journal is the embedded one, and
        // a `Backend::Embedded` answers `Unsupported` to `policy_decide`, so
        // there is nobody who could approve and nothing to suspend. The
        // daemon's has `JournalSource::Open`: it is already open since
        // startup and `journal_gate` lets it through without touching the
        // file.
        self.journal_gate().await?;
        let journal = self.journal().await.ok_or_else(|| {
            tracing::warn!("sync.apply with no journal: no batch to undo, not applied");
            Error::Unsupported
        })?;
        // `open` takes the RIGHT to apply this plan (it is single-use), so
        // from here on every exit path has to return it with `remove`:
        // otherwise that hash stays "applying" and cannot be replanned.
        let reader = spool
            .open(conn_id, plan_hash)
            .await
            .map_err(|e| spool_open_error(&e))?;
        // And if this future is DROPPED before returning, the right is
        // released just the same. It really happens: the gate below can end
        // up suspended in a policy `ask` for a minute, and the daemon
        // withdraws that dispatch with an `rpc.cancel`. Without this,
        // dropping there would leave the hash "applying" forever — neither
        // applicable nor replannable, and diagnosed as an internal error.
        let mut claim = ApplyClaim {
            spool: &spool,
            conn_id,
            plan_hash,
            armed: true,
        };
        let outcome = self
            .sync_apply_opened(reader, plan_hash, conn_id, actor, &spool, &journal)
            .await;
        // It came back: from here on the usual paths take over — the Task
        // body spends the plan when it ends, and an error spends it here.
        claim.armed = false;
        if outcome.is_err() {
            let _ = spool.remove(conn_id, plan_hash).await;
        }
        outcome
    }

    /// The part of [`Self::sync_apply_as`] that runs with the plan already
    /// open.
    ///
    /// It is split out so that EVERY error exit returns the right to apply
    /// (the caller's `remove`): with a single body it would have to be
    /// remembered on every `?`, which is exactly the kind of thing that gets
    /// forgotten.
    #[expect(
        clippy::too_many_lines,
        reason = "one line over since the English names made rustfmt wrap; splitting it would \
                  defeat the single-exit reason above"
    )]
    async fn sync_apply_opened(
        &self,
        reader: crate::sync::SpoolReader,
        plan_hash: &PlanHash,
        conn_id: u64,
        actor: crate::journal::Actor,
        spool: &crate::sync::Spool,
        journal: &Arc<crate::journal::SqliteJournal>,
    ) -> Result<
        (
            TaskHandle,
            Arc<std::sync::Mutex<norte_proto::methods::SyncReportResult>>,
        ),
        Error,
    > {
        use crate::policy::PolicyOp;
        use norte_proto::DeleteMode;
        use norte_proto::methods::DestTrash;

        // A plan with blocks is not executed even if its hash matches: the
        // hash says "this is the plan you were shown", never "this plan can
        // be executed" (see the note in `norte-sync`'s `hash` module).
        if !reader.summary().executable {
            return Err(Error::PlanNotExecutable);
        }
        let counts = reader.summary().counts;
        let source_root = reader.header().options.source_root.clone();
        let dest_root = reader.header().options.dest_root.clone();
        let trash = plan_dest_trash(&reader);

        self.gate(&actor, PolicyOp::Copy, &[&source_root, &dest_root])
            .await?;
        if counts.create_dir > 0 {
            self.gate(&actor, PolicyOp::Mkdir, &[&dest_root]).await?;
        }
        // The kind of delete that is actually going to happen: the same one
        // each step's reversal comes from, so the gate asks about what
        // happens. `Absent` is exactly "the destination has no trash" — see
        // [`plan_dest_trash`] —, so the gate keeps asking about the same
        // thing it did before this value existed.
        let mode = if trash == DestTrash::Absent {
            DeleteMode::Permanent
        } else {
            DeleteMode::Trash
        };
        if counts.overwrite > 0 || counts.delete_tree > 0 {
            self.gate(&actor, PolicyOp::Delete { mode }, &[&dest_root])
                .await?;
        }

        let source = self.provider_for(&source_root).await?;
        let dest = self.provider_for(&dest_root).await?;
        // The batch is reserved AFTER the gate: a denied plan does not
        // consume an id.
        let batch_id = journal.journal().alloc_batch().await.map_err(Error::from)?;
        let recorder = crate::sync::exec::BatchJournal {
            journal: Arc::clone(journal),
            actor: actor.clone(),
            batch_id,
        };
        let targets = crate::sync::exec::SyncTargets {
            source,
            dest,
            source_root,
            dest_root,
            // The SAME policy that just gated the roots, to ask it again
            // step by step: the root's gate resolves the scope boundary, but
            // a `deny` rule of `policy.toml` on a path inside the tree is
            // only seen by asking about that path.
            policy: Arc::clone(&self.policy),
            delete_mode: mode,
            // Opened inside the Task, which is where there is a `task_id` to
            // say in the log that this destination does not know how to
            // confine itself.
            dest_confined: None,
        };
        let report = Arc::new(std::sync::Mutex::new(crate::sync::exec::new_report(
            batch_id, trash,
        )));
        let report_task = Arc::clone(&report);
        let spool_task = spool.clone();
        let hash_task = plan_hash.clone();
        // The plan's steps, plus what the dialog already knew: the total
        // steps and bytes, so the bar has a denominator from the first
        // instant. `current` is NOT touched, for the same reason as in
        // `sync.plan`: it would carry a `VPath` into a broadcast every human
        // connection sees, and this Task's gate is per ROOT.
        let total = counts
            .create_dir
            .saturating_add(counts.copy)
            .saturating_add(counts.overwrite)
            .saturating_add(counts.delete_tree)
            .saturating_add(counts.skip);
        let key = targets.dest_root.scheme().to_owned();
        // The OWNER, for the ring below: the daemon decides with it who can
        // read this report, and `submit` takes `actor` by value.
        let owner = actor.clone();
        let handle = self.sched.submit(
            &key,
            TaskKind::Sync,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move {
                    // The denominator CAN be exceeded, and that has to be
                    // known: the plan's bytes are a lower bound (a provider
                    // that lists with no size contributes none — over
                    // `file://` that is the normal case) while the report's
                    // are counted as they are written. `TaskProgress::bytes_total`
                    // is "estimated" by contract, so this is within what is
                    // promised, but a bar that divides with no cap will go
                    // past 100%.
                    ctx.progress.update(|p| {
                        p.entries_total = Some(total);
                        p.bytes_total = Some(counts.bytes);
                    });
                    let steps = futures::StreamExt::map(reader.steps(), |item| {
                        item.map_err(|e| spool_read_error(&e))
                    });
                    // The `catch_unwind` is not paranoia: the scheduler
                    // already wraps the whole body in one, so a provider's
                    // panic would take the `remove` below down with it and
                    // leave that hash marked "applying" FOREVER — replanning
                    // the same tree gives the same digest and would be
                    // refused for the entire life of the connection. Here
                    // the panic is caught, the plan is spent, and afterward
                    // it answers what the scheduler would have answered.
                    let out = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(crate::sync::exec::run(
                        targets,
                        &recorder,
                        steps,
                        &ctx,
                        &report_task,
                    )))
                    .await;
                    // The plan is spent in ANY terminal state. As long as it
                    // is not erased, its hash counts as "applying" and the
                    // same tree cannot be replanned.
                    if let Err(e) = spool_task.remove(conn_id, &hash_task).await {
                        tracing::warn!(error = %e, "sync.apply: the applied plan could not be removed");
                    }
                    match out {
                        Err(_) => {
                            tracing::error!("sync.apply: panic in the executor");
                            Err(Error::Internal { panic: true })
                        }
                        // `into_wire` and not a `?`: the conversion is LOSSY
                        // (the #160 state has no category in the taxonomy)
                        // and it is the one that leaves it said in the log
                        // before losing it.
                        Ok(r) => r.map_err(crate::sync::exec::ApplyError::into_wire),
                    }
                })
            }),
        );
        // Retains the report for whoever only has the `task_id`: the socket
        // (`sync.report`) and the embedded `Backend`. The direct caller
        // already carries the LIVE `Arc` in hand; this is for the other two,
        // which can only request it afterward. Bounded ring, same as the
        // renames one.
        {
            let mut ring = self
                .sync_reports
                .lock()
                .expect("sync_reports lock is sound");
            ring.push_back((handle.id(), owner, Arc::clone(&report)));
            evict_sync_reports(&mut ring);
        }
        Ok((handle, report))
    }

    /// Builds an archive as a cancelable Task (`archive.pack`, 0.50.0, #132).
    ///
    /// **Writes nothing inside any container**: the archive provider stays
    /// `READ_ONLY` (ADR 0018). It reads the sources through their provider
    /// and writes ONE new file through the destination's, which can be a
    /// different one. It mutates, so it goes to the journal (rule 4) as a
    /// creation, and undoing it is deleting the archive.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidPath`] with no sources, or if any does not hang from
    /// `base` — the stored name is computed against it, and with no name
    /// there is no entry —, and whatever provider resolution returns.
    ///
    /// # Panics
    ///
    /// If the report ring's mutex is poisoned, which only happens if another
    /// thread panicked while holding it. Same treatment as its three twins
    /// on the ring's WRITE path: here it does panic, because a half-updated
    /// ring is not a stale report but an entry nobody will ever be able to
    /// read.
    pub async fn pack_as(
        &self,
        params: norte_proto::methods::ArchivePackParams,
        actor: crate::journal::Actor,
    ) -> Result<TaskHandle, Error> {
        if params.sources.is_empty() {
            return Err(Error::InvalidPath);
        }
        // MUTATION gate (rule 9 and ADR 0025): packing READS the sources and
        // WRITES the destination, which is exactly what a copy does, so it
        // asks about the same thing and with the same op. Before resolving
        // providers: a denied request opens no connections.
        let mut refs: Vec<&VPath> = params.sources.iter().collect();
        refs.push(&params.dest);
        self.gate(&actor, crate::policy::PolicyOp::Copy, &refs)
            .await?;
        let dest_provider = self.provider_for(&params.dest).await?;
        let mut sources = Vec::with_capacity(params.sources.len());
        for p in params.sources {
            let provider = self.provider_for(&p).await?;
            sources.push((provider, p));
        }
        // The queue is the DESTINATION's: it is the only provider this Task
        // writes through, and queuing by the source would mix writes to the
        // same destination across different queues.
        let key = params.dest.scheme().to_owned();
        let observer = Arc::clone(&self.observer);
        let dest = params.dest;
        let base = params.base;
        let format = params.format;
        let level = params.level;
        let report = Arc::new(std::sync::Mutex::new(
            norte_proto::methods::ArchivePackReportResult::default(),
        ));
        let owner = actor.clone();
        let live = Arc::clone(&report);
        let handle = self.sched.submit(
            &key,
            TaskKind::Pack,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move {
                    crate::pack::pack(
                        sources,
                        crate::pack::Dest {
                            provider: dest_provider,
                            dest,
                        },
                        crate::pack::Packed {
                            base,
                            format,
                            level,
                        },
                        observer,
                        live,
                        &ctx,
                    )
                    .await
                })
            }),
        );
        {
            let mut ring = self
                .pack_reports
                .lock()
                .expect("pack_reports lock is sound");
            ring.push_back((handle.id(), owner, report));
            evict_pack_reports(&mut ring);
        }
        // The report does NOT travel in the return value, unlike in
        // `test_archive_as`: here the only read path is
        // [`Self::archive_pack_report`], and returning the `Arc<Mutex<…>>` as
        // well would leak a handle into the public API for a test's
        // convenience.
        Ok(handle)
    }

    /// Report of an already-launched `archive.pack`, by `task_id`, plus the
    /// ACTOR that requested it (#250). `None` if that id was never a pack job
    /// for this instance, or if the ring already evicted it.
    ///
    /// A snapshot, like its three twins, and it is also served before the
    /// terminal state: the report is computed over the entry list BEFORE
    /// writing, so it is already final while the archive is still being
    /// written — and what it says stays true of an archive cancelled
    /// halfway.
    #[must_use]
    pub fn archive_pack_report(
        &self,
        task_id: TaskId,
    ) -> Option<(
        crate::journal::Actor,
        norte_proto::methods::ArchivePackReportResult,
    )> {
        let ring = self
            .pack_reports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ring.iter()
            .find(|(id, _, _)| *id == task_id)
            .map(|(_, owner, r)| {
                (
                    owner.clone(),
                    r.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone(),
                )
            })
    }

    /// Tests an archive as a cancelable Task (`archive.test`, 0.50.0,
    /// #132). Returns the handle and the LIVE report.
    ///
    /// Mutates nothing: no journal, no undo, not a byte written. What
    /// verifies it is the format's READER — zip's checks the CRC of every
    /// entry it reads whole —, so this walks and collects instead of having
    /// a second opinion on the same integrity.
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`] if the name does not match any format in
    /// [`ARCHIVE_FORMATS`](norte_proto::ARCHIVE_FORMATS) — testing "just any
    /// file" means nothing — and whatever provider resolution returns.
    ///
    /// # Panics
    ///
    /// If the report ring's mutex is poisoned, which only happens if another
    /// thread panicked while holding it. Same treatment as its two twins on
    /// the ring's WRITE path: here it does panic, because a half-updated
    /// ring is not a stale report, it is an entry nobody will ever be able
    /// to read.
    pub async fn test_archive_as(
        &self,
        params: norte_proto::methods::ArchiveTestParams,
        actor: crate::journal::Actor,
    ) -> Result<
        (
            TaskHandle,
            Arc<std::sync::Mutex<norte_proto::methods::ArchiveTestResult>>,
        ),
        Error,
    > {
        let name = params
            .path
            .file_name()
            .map(|s| s.as_bytes().to_vec())
            .ok_or(Error::InvalidPath)?;
        let token = crate::pack::name_format(&name).ok_or(Error::Unsupported)?;
        let root = norte_proto::VPath::archive_compose(token, &params.path, &[])
            .map_err(|_| Error::InvalidPath)?;
        let provider = self.provider_for(&root).await?;
        let checked = crate::pack::that_is_checked(token);
        let report = Arc::new(std::sync::Mutex::new(
            norte_proto::methods::ArchiveTestResult::default(),
        ));
        let key = params.path.scheme().to_owned();
        let owner = actor.clone();
        let live = Arc::clone(&report);
        let handle = self.sched.submit(
            &key,
            TaskKind::TestArchive,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move {
                    crate::pack::test_archive(provider, root, checked, live, &ctx).await
                })
            }),
        );
        {
            let mut ring = self
                .test_reports
                .lock()
                .expect("test_reports lock is sound");
            ring.push_back((handle.id(), owner, Arc::clone(&report)));
            evict_test_reports(&mut ring);
        }
        Ok((handle, report))
    }

    /// Report of an already-launched `archive.test`, by `task_id`, plus the
    /// ACTOR that requested it. `None` if that id was never a test for this
    /// instance, or if the ring already evicted it (its cap is the same as
    /// its two twins').
    ///
    /// A snapshot, like its two twins, and for the same reason it is also
    /// served before the terminal state: testing a large archive takes a
    /// while, and the partial report is the only thing that says how far it
    /// has gotten.
    #[must_use]
    pub fn archive_test_report(
        &self,
        task_id: TaskId,
    ) -> Option<(
        crate::journal::Actor,
        norte_proto::methods::ArchiveTestResult,
    )> {
        let ring = self
            .test_reports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ring.iter()
            .find(|(id, _, _)| *id == task_id)
            .map(|(_, owner, r)| {
                (
                    owner.clone(),
                    r.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone(),
                )
            })
    }

    /// Splits a file into numbered parts as a cancelable Task
    /// (`file.split`, 0.50.0, #132). Mutates: one creation per part in the
    /// journal.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidPath`] with a part below
    /// [`FILE_SPLIT_MIN_BYTES`](norte_proto::methods::FILE_SPLIT_MIN_BYTES),
    /// [`Error::LimitExceeded`] if it would come out to more than
    /// [`FILE_SPLIT_MAX_PARTS`](norte_proto::methods::FILE_SPLIT_MAX_PARTS)
    /// — checked BEFORE writing anything —, and whatever provider resolution
    /// returns.
    pub async fn split_as(
        &self,
        params: norte_proto::methods::FileSplitParams,
        actor: crate::journal::Actor,
    ) -> Result<TaskHandle, Error> {
        self.gate(
            &actor,
            crate::policy::PolicyOp::Copy,
            &[&params.path, &params.dest_dir],
        )
        .await?;
        let src = self.provider_for(&params.path).await?;
        // Measured BEFORE creating the Task: a rejection of the REQUEST is an
        // RPC error, not the failure of something already running.
        crate::pack::measures_the_distribution(&*src, &params.path, params.part_bytes).await?;
        let dest_provider = self.provider_for(&params.dest_dir).await?;
        let key = params.dest_dir.scheme().to_owned();
        let observer = Arc::clone(&self.observer);
        let handle = self.sched.submit(
            &key,
            TaskKind::Split,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move {
                    crate::pack::split(
                        src,
                        params.path,
                        params.part_bytes,
                        dest_provider,
                        params.dest_dir,
                        observer,
                        &ctx,
                    )
                    .await
                })
            }),
        );
        Ok(handle)
    }

    /// Joins a split's parts back together as a cancelable Task
    /// (`file.combine`, 0.50.0, #132). Mutates: one creation in the journal.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidPath`] if the first one does not end in `.NNN`,
    /// [`Error::NotFound`] if there is not even one part, [`Error::Conflict`]
    /// on a gap or a short intermediate part, and whatever provider
    /// resolution returns.
    pub async fn combine_as(
        &self,
        params: norte_proto::methods::FileCombineParams,
        actor: crate::journal::Actor,
    ) -> Result<TaskHandle, Error> {
        // The gate covers the parts' DIRECTORY, not just the first one:
        // combining reads `.002`…`.999` as siblings derived by convention,
        // and `is_under` is an exact segment prefix, so a scope pinned to the
        // file `…/x.bin.001` covered the first one and no other. An agent
        // with that scope could carry the whole set to a destination it
        // could actually read.
        let parts_dir = params.first.parent().ok_or(Error::InvalidPath)?;
        self.gate(
            &actor,
            crate::policy::PolicyOp::Copy,
            &[&params.first, &parts_dir, &params.dest],
        )
        .await?;
        let src = self.provider_for(&params.first).await?;
        let dest_provider = self.provider_for(&params.dest).await?;
        let key = params.dest.scheme().to_owned();
        let observer = Arc::clone(&self.observer);
        let handle = self.sched.submit(
            &key,
            TaskKind::Combine,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move {
                    crate::pack::combine(
                        src,
                        params.first,
                        dest_provider,
                        params.dest,
                        observer,
                        &ctx,
                    )
                    .await
                })
            }),
        );
        Ok(handle)
    }

    /// Report of an already-launched plan application, by `task_id`, plus the
    /// ACTOR that requested it (`sync.report`, ADR 0049). `None` if that id
    /// was never an application for this instance, or if the ring already
    /// evicted it ([`SYNC_REPORTS_MAX`]).
    ///
    /// It is a SNAPSHOT: final once the Task is terminal, partial before —
    /// and it is served the same way before the terminal state, because a
    /// half-million-step plan takes a while and the partial report is the
    /// only thing that says how far it has gotten.
    ///
    /// The actor comes out with it for the same reason as in
    /// [`Self::rename_batch_report`]: whoever serves this over the wire has
    /// to decide whether the requester could see that Task, and deciding
    /// that here would force the engine to know the daemon's visibility
    /// rules.
    ///
    /// Like its twin, this path does NOT panic on a poisoned lock: it is a
    /// READ path and every `sync.report` goes through it.
    #[must_use]
    pub fn sync_report(
        &self,
        task_id: TaskId,
    ) -> Option<(
        crate::journal::Actor,
        norte_proto::methods::SyncReportResult,
    )> {
        let ring = self
            .sync_reports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ring.iter()
            .find(|(id, _, _)| *id == task_id)
            .map(|(_, owner, r)| {
                (
                    owner.clone(),
                    r.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone(),
                )
            })
    }

    /// (Re)builds the index for `root` as a cancelable Task (M4, ADR 0034).
    /// Walks the provider (read; like `fs.search`, with no mutation gate) and
    /// feeds [`norte_index::Index::build`]. The `report` fills in on
    /// completion (the `undo_session` pattern).
    ///
    /// # Errors
    /// [`Error::Unsupported`] if there is no index installed
    /// ([`Self::with_index`]) or `root`'s scheme has no provider.
    ///
    /// # Panics
    /// Only if the `report`'s internal lock is poisoned (another thread
    /// panicked mid-write) — unrecoverable, same criterion as the other
    /// locks.
    pub async fn index_build_as(
        &self,
        root: VPath,
        actor: crate::journal::Actor,
    ) -> Result<
        (
            TaskHandle,
            Arc<std::sync::Mutex<Option<norte_index::BuildReport>>>,
        ),
        Error,
    > {
        let index = self.index.clone().ok_or(Error::Unsupported)?;
        let provider = self.provider_for(&root).await?;
        let key = root.scheme().to_owned();
        let report = Arc::new(std::sync::Mutex::new(None));
        let report_task = Arc::clone(&report);
        let handle = self.sched.submit(
            &key,
            TaskKind::Index,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move {
                    let entries =
                        crate::index_build::walk_for_index(provider, root.clone(), &ctx).await?;
                    let r = index
                        .build(&root, entries, &ctx.cancel)
                        .await
                        .map_err(|e| {
                            tracing::warn!(error = %e, "index.build failed");
                            // SQLite's BUSY/LOCKED = transient → retryable
                            // (rust review MAJOR).
                            Error::Io {
                                retryable: e.is_retryable(),
                            }
                        })?;
                    *report_task.lock().expect("report lock is sound") = Some(r);
                    Ok(())
                })
            }),
        );
        Ok((handle, report))
    }

    /// Task `index.embed`: embeddings of `root`'s already-indexed files
    /// (M4-IA-2, ADR 0031 A3). Filtered BEFORE reading (`denied_prefixes`,
    /// a text heuristic), bounded prefixes, skip by hash — see the private
    /// `index_embed` module (not linked: the docs gate refuses to link to a
    /// private item from public docs).
    ///
    /// # Errors
    /// [`Error::Unsupported`] with no index installed ([`Self::with_index`]),
    /// no embeddings provider ([`Self::set_ai_embed_provider`]) or none
    /// resolvable in the config; [`Error::NotFound`] with no prior
    /// `index.build` of `root` (fail-loud in the RESPONSE, not in the join);
    /// [`Error::PolicyDenied`] if the AI gate refuses (AI off, local-only
    /// over remote, `root` under a `denied_prefix`).
    ///
    /// # Panics
    /// Only from poisoning of an internal lock (unrecoverable).
    #[tracing::instrument(skip(self, actor), fields(root = %span_path(&root)))]
    pub async fn index_embed_as(
        &self,
        root: VPath,
        actor: crate::journal::Actor,
    ) -> Result<TaskHandle, Error> {
        let index = self.index.clone().ok_or(Error::Unsupported)?;
        let embedder = self
            .ai_embed
            .read()
            .expect("ai_embed lock is sound")
            .clone()
            .ok_or(Error::Unsupported)?;
        // PRE-content gate (spec §9): nothing is read or sent out until it
        // passes. Cloning the config avoids holding the lock across the
        // awaits.
        let (model, denied) = {
            let config = self
                .ai_config
                .read()
                .expect("ai_config lock is sound")
                .clone();
            let model = config
                .embed_provider_config()
                .ok_or(Error::Unsupported)?
                .model
                .clone();
            crate::ai::AiGate::new(&config)
                .check(crate::ai::AiOp::Embed, embedder.is_local(), &[&root])
                .map_err(|reason| ai_denied_to_error(&reason))?;
            (model, config.denied_prefixes.clone())
        };
        // Fail-loud pre-check in the RESPONSE: with no prior build there is
        // no universe to embed — an immediate `NotFound` is better than a
        // Task that fails on join.
        // Via `has_files_for_embed` and not `files_for_embed(...).is_empty()`
        // (#122): the question is "is there a universe?", and answering it
        // by materializing the entire candidate list — with its
        // `VPath::parse` per row — would sweep the tree twice per embed, one
        // of those just to throw it away.
        let no_build = !index.has_files_for_embed(&root).await.map_err(|e| {
            tracing::warn!(error = %e, "index.embed: index pre-check failed");
            Error::Io {
                retryable: e.is_retryable(),
            }
        })?;
        if no_build {
            return Err(Error::NotFound);
        }
        let provider = self.provider_for(&root).await?;
        let key = root.scheme().to_owned();
        let handle = self.sched.submit(
            &key,
            TaskKind::Embed,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move {
                    crate::index_embed::embed_for_index(
                        provider, embedder, index, root, model, denied, &ctx,
                    )
                    .await
                })
            }),
        );
        Ok(handle)
    }

    /// Queries `root`'s index for `text` (M4). Direct read (not a Task).
    ///
    /// Hits from a protected subtree are DROPPED for an agent or a plugin
    /// (#165): the index is normally built by the human, so it can contain
    /// the daemon's state directory even if an agent cannot list it. The
    /// filter runs AFTER the index's `limit`, so a query whose hits all fall
    /// in protected territory returns fewer rows than requested — which is
    /// exactly what it should return.
    ///
    /// # Errors
    /// [`Error::Unsupported`] with no index; an index error maps to `Io`.
    pub async fn index_query_as(
        &self,
        root: &VPath,
        text: &str,
        limit: u32,
        actor: crate::journal::Actor,
    ) -> Result<Vec<norte_index::IndexHit>, Error> {
        let index = self.index.clone().ok_or(Error::Unsupported)?;
        let hits = index.query(root, text, limit).await.map_err(|e| {
            tracing::debug!(error = %e, "index.query failed");
            Error::Io {
                retryable: e.is_retryable(),
            }
        })?;
        Ok(drop_excluded(hits, &crate::policy::walk_exclusions(&actor)))
    }

    /// `index.search_semantic`: ONE embed call for the query + a cosine sweep
    /// in Rust over the root's vectors (`None` ⇒ all of them). No ANN (ADR
    /// 0031: only if a real corpus justifies it). `k` is clamped to
    /// `[1, INDEX_SEMANTIC_MAX_K]`. Scores are ALWAYS finite (anti-NaN belt:
    /// `serde_json` serializes `NaN` as `null` and would poison the entire
    /// response on the client).
    ///
    /// # Errors
    /// [`Error::Unsupported`] with no index, no embeddings provider or none
    /// resolvable in the config; [`Error::PolicyDenied`] if the AI gate
    /// refuses (AI off, local-only over remote, `root` denied); provider
    /// errors mapped to the wire taxonomy; index errors to `Io`.
    ///
    /// # Panics
    /// Only from poisoning of an internal lock (unrecoverable).
    #[tracing::instrument(
        skip(self, query),
        fields(root = root.map_or_else(|| "<all>".into(), span_path))
    )]
    pub async fn index_search_semantic(
        &self,
        root: Option<&VPath>,
        query: &str,
        k: u32,
    ) -> Result<Vec<(VPath, f64)>, Error> {
        let index = self.index.clone().ok_or(Error::Unsupported)?;
        let embedder = self
            .ai_embed
            .read()
            .expect("ai_embed lock is sound")
            .clone()
            .ok_or(Error::Unsupported)?;
        // PRE-embed gate (spec §9): the query does not go out to the
        // provider until it passes. Cloning the config avoids holding the
        // lock across the await.
        let model = {
            let config = self
                .ai_config
                .read()
                .expect("ai_config lock is sound")
                .clone();
            let model = config
                .embed_provider_config()
                .ok_or(Error::Unsupported)?
                .model
                .clone();
            let paths: Vec<&VPath> = root.into_iter().collect();
            crate::ai::AiGate::new(&config)
                .check(crate::ai::AiOp::Embed, embedder.is_local(), &paths)
                .map_err(|reason| ai_denied_to_error(&reason))?;
            model
        };
        let k = crate::index_embed::clamp_k(k);
        let qvec = embedder
            .embed(&[query.to_owned()])
            .await
            .map_err(|e| ai_to_proto_error(&e))?
            .into_iter()
            .next()
            // A lying provider (0 vectors for 1 input): the PROVIDER's
            // misbehavior, not retryable — same criterion as `flush_batch`.
            .ok_or(Error::ProviderUnavailable { retryable: false })?;
        // Garbage-provider belt: an empty query vector, with non-finite
        // components or zero norm, cannot score anything — an honest error
        // beats 0 hits in silence. (Finite norm ⇒ finite components.)
        let norm2: f32 = qvec.iter().map(|x| x * x).sum();
        if qvec.is_empty() || !norm2.is_finite() || norm2 <= 0.0 {
            return Err(Error::ProviderUnavailable { retryable: false });
        }
        let vectors = index
            .embeddings_for_root(root, &model)
            .await
            .map_err(|e| crate::index_embed::index_to_proto(&e))?;
        // Bounded heap and the query's norm HOISTED (#122): the count is the
        // same, memory is O(k) instead of O(index), and the query stops
        // being renormalized once per row. The final order breaks ties by
        // path, so two files with the same score always come out the same
        // way.
        let norm_q = norm2.sqrt();
        let scored = vectors.into_iter().filter_map(|(path, v)| {
            crate::index_embed::cosine_prenormed(&qvec, norm_q, &v).map(|s| {
                crate::index_embed::Puntuado {
                    score: f64::from(s),
                    path,
                }
            })
        });
        Ok(crate::index_embed::mejores_k(scored, k))
    }

    /// Copy (recursive if a dir) as a Task, with the default policies
    /// (`Fail` + `Preserve`).
    ///
    /// # Errors
    /// [`Error::Unsupported`] if some scheme has no registered provider.
    pub async fn copy(&self, from: &VPath, to: &VPath) -> Result<TaskHandle, Error> {
        self.copy_with(from, to, TransferOptions::default()).await
    }

    /// Copy with explicit collision and symlink policies (ADR 0005), as
    /// `User` (the human path, no sandbox).
    ///
    /// # Errors
    /// [`Error::Unsupported`] if some scheme has no registered provider.
    pub async fn copy_with(
        &self,
        from: &VPath,
        to: &VPath,
        opts: TransferOptions,
    ) -> Result<TaskHandle, Error> {
        self.copy_with_as(from, to, opts, crate::journal::Actor::User)
            .await
    }

    /// Copy with policies and an explicit ACTOR (the agentic path, M3-3):
    /// gates by policy PRE-effect and records the real actor in the journal.
    ///
    /// # Errors
    /// [`Error::JournalUnavailable`] if this session's journal cannot be
    /// opened (#178); [`Error::PolicyDenied`] if policy denies;
    /// [`Error::Unsupported`] if some scheme has no registered provider.
    pub async fn copy_with_as(
        &self,
        from: &VPath,
        to: &VPath,
        opts: TransferOptions,
        actor: crate::journal::Actor,
    ) -> Result<TaskHandle, Error> {
        self.copy_anchored(from, to, opts, actor, None).await
    }

    /// Like [`Engine::copy_with_as`], but with the ANCHOR the client
    /// observed for the destination directory when it listed it (#295, ADR
    /// 0073).
    ///
    /// With an anchor, the transfer refuses to write if that directory is no
    /// longer the node the human was looking at when they approved — which is
    /// the only thing able to tell apart a `dest/sub -> /etc` planted
    /// beforehand from a legitimate `~/copies -> /mnt/disk/copies`, because
    /// from inside the core the two look the same (ADR 0072).
    ///
    /// With no anchor (`None`) it does exactly what 0.53 did: it confines the
    /// same way and that check does not happen.
    ///
    /// # Errors
    /// Those of [`Engine::copy_with_as`], plus [`Error::Conflict`] with
    /// [`ConflictKind::EscapesRoot`](norte_proto::ConflictKind::EscapesRoot)
    /// if the destination directory is no longer the anchored node.
    #[tracing::instrument(skip(self, actor, dest_anchor), fields(from = %span_path(from), to = %span_path(to), anchored = dest_anchor.is_some()))]
    pub async fn copy_anchored(
        &self,
        from: &VPath,
        to: &VPath,
        opts: TransferOptions,
        actor: crate::journal::Actor,
        dest_anchor: Option<norte_proto::DirAnchor>,
    ) -> Result<TaskHandle, Error> {
        self.gate(&actor, crate::policy::PolicyOp::Copy, &[from, to])
            .await?;
        let src = self.provider_for(from).await?;
        let dst = self.provider_for(to).await?;
        let observer = Arc::clone(&self.observer);
        let (from, to) = (from.clone(), to.clone());
        let key = to.scheme().to_owned();
        // To the queue or in parallel, as whoever launched it asked (ADR
        // 0149).
        let lane = if opts.queued {
            crate::scheduler::Lane::Cola
        } else {
            crate::scheduler::Lane::Parallel
        };
        Ok(self.sched.submit_en(
            lane,
            &key,
            TaskKind::Copy,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move {
                    ops::copy_task(src, dst, from, to, opts, dest_anchor, observer, &ctx).await
                })
            }),
        ))
    }

    /// Moves a task that has not started yet up or down the serial QUEUE
    /// (ADR 0149).
    ///
    /// `false` if it was already running, if it was not in the queue, or if
    /// it was already at the end it is being moved toward.
    #[must_use]
    pub fn mover_en_cola(&self, id: norte_proto::TaskId, up: bool) -> bool {
        self.sched.mover_en_cola(id, up)
    }

    /// Move as a Task with the default policies: rename if same provider;
    /// copy+delete with a single plan if cross-provider or if rename returns
    /// `Unsupported` (EXDEV).
    ///
    /// # Errors
    /// [`Error::Unsupported`] if some scheme has no registered provider.
    pub async fn move_(&self, from: &VPath, to: &VPath) -> Result<TaskHandle, Error> {
        self.move_with(from, to, TransferOptions::default()).await
    }

    /// Move with explicit collision and symlink policies (ADR 0005), as
    /// `User`.
    ///
    /// # Errors
    /// [`Error::Unsupported`] if some scheme has no registered provider.
    pub async fn move_with(
        &self,
        from: &VPath,
        to: &VPath,
        opts: TransferOptions,
    ) -> Result<TaskHandle, Error> {
        self.move_with_as(from, to, opts, crate::journal::Actor::User)
            .await
    }

    /// Move with policies and an explicit ACTOR (the agentic path, M3-3).
    ///
    /// # Errors
    /// [`Error::JournalUnavailable`] if this session's journal cannot be
    /// opened (#178); [`Error::PolicyDenied`] if policy denies;
    /// [`Error::Unsupported`] if some scheme has no registered provider.
    pub async fn move_with_as(
        &self,
        from: &VPath,
        to: &VPath,
        opts: TransferOptions,
        actor: crate::journal::Actor,
    ) -> Result<TaskHandle, Error> {
        self.move_anchored(from, to, opts, actor, None).await
    }

    /// Like [`Engine::move_with_as`], with the destination directory's anchor
    /// (#295, ADR 0073; see [`Engine::copy_anchored`]).
    ///
    /// It works for both paths: the copy one writes just like a copy, and
    /// rename resolves its destination by path once — with the directory
    /// turned into a link, it drops the file on the other side just the
    /// same.
    ///
    /// # Errors
    /// Those of [`Engine::move_with_as`], plus [`Error::Conflict`] if the
    /// destination directory is no longer the anchored node.
    #[tracing::instrument(skip(self, actor, dest_anchor), fields(from = %span_path(from), to = %span_path(to), anchored = dest_anchor.is_some()))]
    pub async fn move_anchored(
        &self,
        from: &VPath,
        to: &VPath,
        opts: TransferOptions,
        actor: crate::journal::Actor,
        dest_anchor: Option<norte_proto::DirAnchor>,
    ) -> Result<TaskHandle, Error> {
        self.gate(&actor, crate::policy::PolicyOp::Move, &[from, to])
            .await?;
        let src = self.provider_for(from).await?;
        let dst = self.provider_for(to).await?;
        let observer = Arc::clone(&self.observer);
        let (from, to) = (from.clone(), to.clone());
        let key = to.scheme().to_owned();
        let lane = if opts.queued {
            crate::scheduler::Lane::Cola
        } else {
            crate::scheduler::Lane::Parallel
        };
        Ok(self.sched.submit_en(
            lane,
            &key,
            TaskKind::Move,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move {
                    ops::move_task(src, dst, from, to, opts, dest_anchor, observer, &ctx).await
                })
            }),
        ))
    }

    /// The REVIEWABLE plan for a batch of renames inside `dir` (spec §17, ADR
    /// 0042). READS the directory and its capabilities; mutates and
    /// journals nothing.
    ///
    /// `pairs` are `(from, to)` in RAW base-name BYTES (rule 1): the caller
    /// sends INTENT. The order, the temporaries and the verdicts are decided
    /// by the core, so a client — which can be an agent — cannot sneak in an
    /// order the human never saw.
    ///
    /// The token it returns ([`crate::rename::DirPlan::hash`]) is TIED to
    /// `dir`: a hash approved for one directory is not valid against another
    /// whose re-plan produces the same steps.
    ///
    /// **Policy gate: NONE HERE**, same as [`Self::list`],
    /// [`Self::stat`] and [`Self::search_as`]. There is no read `PolicyOp`:
    /// the READ gate lives in the daemon (`read_gate`, #80), which is what
    /// decides whether an actor can look inside a directory, and the
    /// MUTATION one applies in full in [`Self::rename_batch`], which is
    /// where there is an effect.
    ///
    /// **DAEMON OBLIGATION**: `fs.rename_batch_plan` **and also
    /// `fs.rename_batch`** have to go through `read_gate` like `fs.list`.
    /// Without it, this method is a name oracle — every name in the
    /// directory, plus the NFC/NFD twin structure that `fs.list` does not
    /// even expose — for an agent with no scope, and it also reveals
    /// `Unsupported`/`NotFound`/`PlanStale` for a directory it has no rights
    /// to.
    ///
    /// That the second one is a MUTATION does not exempt it, and it is the
    /// easy mistake to make: [`Self::rename_batch_as`] starts by planning,
    /// that is, by reading, and its mutation gate cannot run before that
    /// because it is not known which paths need gating until the plan
    /// exists. With no `read_gate` in front, a scopeless agent gets exactly
    /// the oracle this paragraph closes here — and a finer one, because
    /// `plan_hash` is deterministic and computable offline, so the answer
    /// responds to a specific hypothesis about a specific name.
    ///
    /// The gate lives in the daemon and not here because it is the daemon
    /// that ties a connection to an actor: through the embedded API there is
    /// no `Actor::Agent` that the process itself did not write (same
    /// criterion `Backend::plugins_set_approval` documents). Duplicating it
    /// here, besides, would ask TWICE under an `ask` rule.
    ///
    /// # Errors
    /// [`Error::InvalidPath`] if some name is not a legal directory entry or
    /// if `pairs` exceeds
    /// [`FS_RENAME_BATCH_MAX_PAIRS`](norte_proto::methods::FS_RENAME_BATCH_MAX_PAIRS);
    /// [`Error::LimitExceeded`] if `dir` has more than
    /// [`RENAME_BATCH_MAX_LISTING`] entries; [`Error::Unsupported`] if
    /// `dir`'s scheme has no provider or the provider is read-only; the
    /// provider's own while listing.
    pub async fn rename_batch_plan(
        &self,
        dir: &VPath,
        pairs: &[(Vec<u8>, Vec<u8>)],
    ) -> Result<crate::rename::DirPlan, Error> {
        self.rename_batch_plan_as(dir, pairs, crate::journal::Actor::User)
            .await
    }

    /// [`Self::rename_batch_plan`] with an explicit ACTOR (the agentic
    /// path).
    ///
    /// The actor travels along for symmetry with the mutations and for
    /// future auditing, but it GATES nothing: planning is a read, and an
    /// agent that can list a directory can see what a rename would do to it.
    /// What is gated is executing it ([`Self::rename_batch_as`]).
    ///
    /// # Errors
    /// Those of [`Self::rename_batch_plan`].
    // `dir` via `span_path`, like all its siblings: with a plain `skip(self,
    // pairs)` it was logged via `Debug`, which is the entire wire value —
    // with a `user:pass@` if the path carries one — and now hung off every
    // line of the request (ADR 0127).
    #[tracing::instrument(
        skip(self, dir, pairs),
        fields(dir = %span_path(dir), actor = ?actor, pairs = pairs.len())
    )]
    pub async fn rename_batch_plan_as(
        &self,
        dir: &VPath,
        pairs: &[(Vec<u8>, Vec<u8>)],
        actor: crate::journal::Actor,
    ) -> Result<crate::rename::DirPlan, Error> {
        // The actor GATES nothing here (that is the daemon's `read_gate`),
        // but it IS traced: a plan that goes well enumerates an entire
        // directory and `read_gate` only leaves a trace when it DENIES —
        // without this, the agentic read that WAS allowed is not
        // attributable in the audit (M3-5).
        let (plan, _provider) = self.plan_for(dir, pairs).await?;
        Ok(plan)
    }

    /// Plans against the directory AS IT STANDS RIGHT NOW and also returns
    /// the provider that served it, so the execution path can re-plan with
    /// exactly the same entries.
    #[tracing::instrument(skip(self, pairs), fields(dir = %span_path(dir), pairs = pairs.len()))]
    async fn plan_for(
        &self,
        dir: &VPath,
        pairs: &[(Vec<u8>, Vec<u8>)],
    ) -> Result<(crate::rename::DirPlan, Arc<dyn Provider>), Error> {
        // Wire cap, checked here too: the embedded engine is a public API
        // and the planner is linear in pairs AND in listing entries — an
        // unbounded batch is unbounded work.
        if pairs.len() > norte_proto::methods::FS_RENAME_BATCH_MAX_PAIRS {
            tracing::debug!(pairs = pairs.len(), "rename batch above the cap");
            return Err(Error::InvalidPath);
        }
        // The planner's precondition (each side is ONE directory entry) is
        // checked HERE, at the only impure point the names pass through.
        // `plan_batch` is `pub` and treats names as opaque bytes: in release
        // it would happily plan a `to = "../.."`.
        for name in pairs.iter().flat_map(|(f, t)| [f, t]) {
            if Segment::new(name.clone()).is_err() {
                tracing::debug!("rename name that is not a directory entry");
                return Err(Error::InvalidPath);
            }
        }
        let provider = self.provider_for(dir).await?;
        // The LISTING goes first, and the order remains the correct one
        // even though `capabilities_at` already probes on its own (ADR
        // 0054): a directory that cannot be listed has no plan to compute,
        // and asking about its capabilities first would only bring forward
        // work just to throw it away.
        //
        // And it asks about the DIRECTORY, not about the provider, which is
        // what `NameCaps`'s docs promise: planning over a volume that folds
        // as if it distinguished case is an `External` collision that goes
        // unreported and a plan the human approves without the line that
        // mattered to them.
        let names = list_base_names(&*provider, dir).await?;
        let caps = provider.capabilities_at(dir).await?;
        if caps.flags.contains(CapabilityFlags::READ_ONLY) {
            return Err(Error::Unsupported);
        }
        let name_caps = crate::rename::NameCaps::from_capabilities(caps);
        let owned: Vec<(Vec<u8>, Vec<u8>)> = pairs.to_vec();
        // The planner is SYNCHRONOUS and assigns a comparison key per
        // listing entry: over a directory of a hundred thousand files that
        // is measurable CPU work, and `fs.rename_batch_plan` is a DIRECT
        // response (ADR 0042) that runs on the async executor. Off of it
        // (rules 2 and 3): a blocking thread cannot leave the daemon's other
        // connections unattended.
        crate::blocking::spawn_blocking(move || {
            crate::rename::plan_batch(&owned, &names, name_caps)
        })
        .await
        .map(|plan| (crate::rename::DirPlan::bind(dir, plan), provider))
        .map_err(|e| {
            let panic = e.is_panic();
            tracing::error!(error = %e, panic, "the rename planner did not finish");
            Error::Internal { panic }
        })
    }

    /// Executes a batch of renames inside `dir` as ONE Task and ONE
    /// undoable journal unit (spec §17, ADR 0042).
    ///
    /// `plan_hash` is the plan's FRESHNESS token
    /// ([`Self::rename_batch_plan`]), tied to the directory. The directory is
    /// re-planned here and the tokens have to match: a drift that changes
    /// some verdict answers [`Error::PlanStale`] instead of executing a plan
    /// different from the one requested.
    ///
    /// Watch what it does NOT guarantee. The digest is a public,
    /// deterministic function of `(dir, steps, verdicts)`, with no secret and
    /// no state on the server, so anyone can compute it without ever having
    /// called [`Self::rename_batch_plan`]: the token matching does NOT prove
    /// a human saw the plan. What it proves is that the plan being executed
    /// is the one the re-plan produces RIGHT NOW. Consent is what the policy
    /// gate provides, not this hash.
    ///
    /// Returns the Task and its [`crate::rename::BatchReport`], which fills
    /// in as it runs and is complete once it ends. **Look at it even if the
    /// Task fails**: if the rollback got stuck halfway, that is where the
    /// step that stayed applied is, and under what names — `Failed{error}`
    /// only tells the cause.
    ///
    /// No step overwrites anything: they are plain
    /// [`norte_vfs::Provider::rename`] calls, and the planner rejects the
    /// entire plan on any collision.
    ///
    /// # Errors
    /// [`Error::PlanNotExecutable`] if the plan has collisions;
    /// [`Error::PlanStale`] if the directory drifted; [`Error::PolicyDenied`]
    /// if the gate denies; [`Error::JournalUnavailable`] if this session's
    /// journal cannot be opened (#178); those of [`Self::rename_batch_plan`].
    pub async fn rename_batch(
        &self,
        dir: &VPath,
        pairs: &[(Vec<u8>, Vec<u8>)],
        plan_hash: &PlanHash,
    ) -> Result<
        (
            TaskHandle,
            Arc<std::sync::Mutex<crate::rename::BatchReport>>,
        ),
        Error,
    > {
        self.rename_batch_as(dir, pairs, plan_hash, crate::journal::Actor::User)
            .await
    }

    /// [`Self::rename_batch`] with an explicit ACTOR (the agentic path,
    /// M3-3).
    ///
    /// # Errors
    /// Those of [`Self::rename_batch`].
    ///
    /// # Panics
    /// Only from poisoning of the report's `Mutex` (another thread panicked
    /// while holding it) — unrecoverable, same criterion as the rest of the
    /// core.
    #[tracing::instrument(skip(self, pairs, plan_hash, actor), fields(dir = %span_path(dir)))]
    pub async fn rename_batch_as(
        &self,
        dir: &VPath,
        pairs: &[(Vec<u8>, Vec<u8>)],
        plan_hash: &PlanHash,
        actor: crate::journal::Actor,
    ) -> Result<
        (
            TaskHandle,
            Arc<std::sync::Mutex<crate::rename::BatchReport>>,
        ),
        Error,
    > {
        let (plan, provider) = self.plan_for(dir, pairs).await?;
        // DRIFT is checked first, and the order is contractual (see
        // `Error::PlanStale`'s docs): "re-planning the same pairs produced a
        // plan different from the one approved". A directory that drifted
        // into becoming collision-prone answering `PlanNotExecutable` would
        // tell the human the plan they read had collisions — and it did not.
        // With this order, `PlanNotExecutable` is left for what it actually
        // names: the caller approved a plan that was already dead (it
        // ignored `executable: false`), and its token matches.
        if plan.hash() != plan_hash {
            return Err(Error::PlanStale);
        }
        if !plan.executable() {
            return Err(Error::PlanNotExecutable);
        }
        let steps: Vec<crate::rename::exec::PlannedStep> = plan
            .plan()
            .steps
            .iter()
            .map(|s| crate::rename::exec::absolute(dir, s))
            .collect::<Result<_, _>>()?;
        // ONE gate for the whole batch, with ALL the paths it touches — the
        // temporaries included, which are also files created in the
        // directory. Policy resolves the slice to the MOST restrictive
        // verdict, so a batch that so much as touches a denied name is
        // denied WHOLE and never halfway. It goes before reserving the batch
        // id: a denied batch consumes nothing.
        let mut gate_paths: Vec<&VPath> = Vec::with_capacity(steps.len() * 2);
        for s in &steps {
            gate_paths.push(&s.from);
            gate_paths.push(&s.to);
        }
        self.gate(&actor, crate::policy::PolicyOp::Move, &gate_paths)
            .await?;
        drop(gate_paths);

        let recorder: Arc<dyn crate::rename::exec::StepJournal> = match self.journal().await {
            Some(j) => {
                let batch_id = j.journal().alloc_batch().await.map_err(Error::from)?;
                Arc::new(crate::rename::exec::BatchJournal {
                    journal: Arc::clone(&j),
                    actor: actor.clone(),
                    batch_id,
                })
            }
            // With no journal (embedded tests, `Engine::new`): the renames
            // happen and reach the observer, but there is no batch to group
            // them and no undo to serve. Honest: with no journal there is
            // also no undo.
            //
            // WATCH for the day the observer stops being the journal: with
            // `with_journal` the two are the SAME object (see its
            // constructor), so writing straight to the journal skips
            // nobody. If a fan-out observer is ever installed ALONGSIDE a
            // journal, this branch has to emit to both or batch renames will
            // be the only thing invisible to it.
            // FIXED (#205), and it was needed here as much as in `ops`: with
            // no fix, every rename in the batch went back to asking the
            // observer, so a long batch that starts with the file busy left
            // rows starting from the middle. Worse than in `ops`, too: those
            // rows go with no `batch_id`, meaning the batch the wire
            // announces as ONE undoable unit ended up half-recorded AND
            // ungrouped, and undo unwound the queue leaving the head
            // renamed.
            //
            // It is fixed WITHOUT resolving again: `self.journal()` already
            // asked, and asking again could answer yes — the brake is short
            // in the tests, and `with_retry_brake` is public — which would
            // leave the batch recorded whole but ungrouped, the other way of
            // breaking the same promise.
            None => Arc::new(crate::rename::exec::ObserverJournal {
                // An absent lazy journal does NOT record; an embedder's
                // observer does receive, even with no journal behind it. The
                // report tells the truth in both cases (#205).
                records: !matches!(self.journal, JournalSource::Lazy(_)),
                observer: if matches!(self.journal, JournalSource::Lazy(_)) {
                    // A lazy journal that is not there right now: this batch
                    // records nothing, and will not ask again.
                    Arc::new(crate::observer::NoopObserver)
                } else {
                    // `Engine::new()`/`with_observer`: there is no window to
                    // lose and the embedder's observer has to keep
                    // receiving its renames.
                    Arc::clone(&self.observer)
                },
                actor: actor.clone(),
            }),
        };

        let report = Arc::new(std::sync::Mutex::new(crate::rename::BatchReport::default()));
        let report_task = Arc::clone(&report);
        let key = dir.scheme().to_owned();
        let owner = actor.clone();
        let handle = self.sched.submit(
            &key,
            TaskKind::RenameBatch,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move {
                    let out = crate::rename::exec::run(
                        &*provider,
                        &*recorder,
                        &steps,
                        &ctx.cancel,
                        &ctx.progress,
                        &report_task,
                    )
                    .await;
                    // A rollback that could not finish is not a detail: it
                    // names the step that stayed applied.
                    let stuck = report_task.lock().expect("batch report lock").stuck.clone();
                    if let Some(s) = stuck {
                        tracing::error!(
                            from = %span_path(&s.from),
                            to = %span_path(&s.to),
                            pair_index = s.pair_index,
                            still_applied = s.still_applied,
                            "the rename batch left a rename applied that could not be undone",
                        );
                    }
                    out
                })
            }),
        );
        // Retains the report for whoever only has the `task_id`: the socket
        // (`fs.rename_batch_report`) and the embedded `Backend`. The direct
        // caller already carries the LIVE `Arc` in hand; this is for the
        // other two, which can only request it afterward. Bounded ring: a
        // daemon's memory over months does not grow with every batch.
        {
            let mut ring = self
                .batch_reports
                .lock()
                .expect("batch_reports lock is sound");
            ring.push_back((handle.id(), owner, Arc::clone(&report)));
            evict_batch_reports(&mut ring);
        }
        Ok((handle, report))
    }

    /// Applies an ORGANIZE plan (phase 8, `fs.organize`): creates the
    /// missing folders and moves, ALL under a single batch.
    ///
    /// It is one method and not N client calls for a specific reason: the
    /// `fs.create`s and the `fs.move`s have to share `batch_id`. Split
    /// apart, undoing the batch would bring the files back and forget the
    /// folders — leaving the human with a tree of empty directories they
    /// never made.
    ///
    /// The moves are journaled with [`crate::OP_ORGANIZED`] and not with
    /// `renamed`, and that is also correctness: a `renamed` batch is undone
    /// by the rename executor, which assumes ONE common directory and builds
    /// the reverse chain from it. Here there is no common directory — moving
    /// into subdirectories is exactly what this does.
    ///
    /// # Errors
    /// [`Error::PlanStale`] if `plan_hash` is not the reviewed plan's;
    /// [`Error::InvalidPath`] if some destination fails validation (a `..`,
    /// an absolute path, a self-contradicting plan);
    /// [`Error::PolicyDenied`] if policy denies any of the paths it touches —
    /// the batch is denied WHOLE, never halfway; those of [`Self::mkdir_as`]
    /// and the provider's own.
    ///
    /// # Panics
    /// None: the `expect`s are on its own locks.
    pub async fn organize(
        &self,
        dir: &VPath,
        moves: &[norte_proto::methods::OrganizeMove],
        plan_hash: &PlanHash,
        actor: crate::journal::Actor,
    ) -> Result<TaskHandle, Error> {
        let plan = crate::organize::OrganizePlan::bind(dir, moves)?;
        // DRIFT first, same as in the rename batch and for the same reason:
        // what gets applied has to be what a human read.
        if plan.hash() != plan_hash {
            return Err(Error::PlanStale);
        }
        let provider = self.provider_for(dir).await?;
        let folders = plan.folders(dir);
        let steps: Vec<(VPath, VPath)> = plan
            .steps()
            .iter()
            .map(|p| (dir.clone().join(p.current.clone()), p.dest(dir)))
            .collect();

        // ONE gate for everything it touches — the folders it is going to
        // create and both sides of every move —, before reserving the
        // batch. Policy resolves the slice to the most restrictive verdict,
        // so a plan that so much as touches a denied name is denied whole.
        let mut gate: Vec<&VPath> = Vec::with_capacity(folders.len() + steps.len() * 2);
        gate.extend(folders.iter());
        for (from, to) in &steps {
            gate.push(from);
            gate.push(to);
        }
        self.gate(&actor, crate::policy::PolicyOp::Move, &gate)
            .await?;
        drop(gate);

        let journal = self.journal().await;
        let batch_id = match &journal {
            Some(j) => Some(j.journal().alloc_batch().await.map_err(Error::from)?),
            // With no journal there is no batch to group and no undo to
            // serve, and it says so: honest, like in the rename batch.
            None => None,
        };
        let key = dir.scheme().to_owned();
        let handle = self.sched.submit(
            &key,
            TaskKind::RenameBatch,
            Priority::Normal,
            actor.clone(),
            Box::new(move |ctx| {
                Box::pin(async move {
                    let total = (folders.len() + steps.len()) as u64;
                    ctx.progress.update(|p| p.entries_total = Some(total));
                    // Folders first, from the highest to the deepest: a
                    // provider does not invent parents, and the core has to
                    // know which ones it created to be able to delete them
                    // on undo.
                    for c in &folders {
                        if ctx.cancel.is_cancelled() {
                            return Err(Error::Cancelled);
                        }
                        // A folder that ALREADY exists is neither a failure
                        // nor journaled: this batch did not create it, so
                        // undoing cannot delete it.
                        if provider.stat(c).await.is_ok() {
                            continue;
                        }
                        provider.mkdir(c).await?;
                        if let (Some(j), Some(b)) = (journal.as_ref(), batch_id) {
                            j.journal()
                                .record_entry(&crate::journal::NewEntry {
                                    op: "created",
                                    path: c.to_wire().as_bytes(),
                                    path_to: None,
                                    reversal: crate::journal::Reversal::Delete,
                                    reversal_ref: None,
                                    actor: &actor,
                                    undoes_seq: None,
                                    batch_id: Some(b),
                                })
                                .await
                                .map_err(Error::from)?;
                        }
                        ctx.progress.update(|p| p.entries_done += 1);
                    }
                    for (from, to) in &steps {
                        if ctx.cancel.is_cancelled() {
                            return Err(Error::Cancelled);
                        }
                        provider.rename(from, to).await?;
                        if let (Some(j), Some(b)) = (journal.as_ref(), batch_id) {
                            j.journal()
                                .record_entry(&crate::journal::NewEntry {
                                    op: crate::undo::OP_ORGANIZED,
                                    path: to.to_wire().as_bytes(),
                                    path_to: Some(from.to_wire().as_bytes()),
                                    reversal: crate::journal::Reversal::RenameBack,
                                    reversal_ref: None,
                                    actor: &actor,
                                    undoes_seq: None,
                                    batch_id: Some(b),
                                })
                                .await
                                .map_err(Error::from)?;
                        }
                        ctx.progress.update(|p| p.entries_done += 1);
                    }
                    Ok(())
                })
            }),
        );
        Ok(handle)
    }

    /// Report of an already-launched batch, by `task_id`, plus the ACTOR
    /// that requested it (spec §17). `None` if that id was never a batch for
    /// this instance, or if the ring already evicted it
    /// ([`BATCH_REPORTS_MAX`]).
    ///
    /// It is a SNAPSHOT: final once the Task is terminal, partial before.
    /// The actor comes out with it because whoever serves this over the wire
    /// has to decide whether the requester could see that task — deciding
    /// that here would force the engine to know the daemon's visibility
    /// rules.
    ///
    /// This path does NOT panic on a poisoned lock: it is a READ path and
    /// every `fs.rename_batch_report` goes through it, so someone else's
    /// panic with the mutex in hand would turn the whole method into a
    /// per-connection bomb. A possibly stale report serves whoever is
    /// looking for their file better than an error — and the report is
    /// data, not an invariant a half-finished panic could have broken.
    #[must_use]
    pub fn rename_batch_report(
        &self,
        task_id: TaskId,
    ) -> Option<(crate::journal::Actor, crate::rename::BatchReport)> {
        let ring = self
            .batch_reports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ring.iter()
            .find(|(id, _, _)| *id == task_id)
            .map(|(_, owner, r)| {
                (
                    owner.clone(),
                    r.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone(),
                )
            })
    }

    /// PERMANENT deletion (recursive, post-order) as a Task. The trash one
    /// is [`Self::delete_with`] with [`DeleteMode::Trash`].
    ///
    /// # Errors
    /// [`Error::Unsupported`] if the scheme has no registered provider.
    pub async fn delete(&self, path: &VPath) -> Result<TaskHandle, Error> {
        self.delete_with(path, DeleteMode::Permanent).await
    }

    /// Delete with an explicit mode (ADR 0009): `Trash` moves the whole
    /// tree to the provider's trash (a single operation; with no `TRASH`
    /// capability the task fails `Unsupported` — the engine NEVER degrades
    /// to permanent on its own); `Permanent` really deletes.
    ///
    /// # Errors
    /// [`Error::Unsupported`] if the scheme has no registered provider.
    pub async fn delete_with(&self, path: &VPath, mode: DeleteMode) -> Result<TaskHandle, Error> {
        self.delete_with_as(path, mode, crate::journal::Actor::User)
            .await
    }

    /// Delete with an explicit mode and ACTOR (the agentic path, M3-3).
    ///
    /// # Errors
    /// [`Error::JournalUnavailable`] if this session's journal cannot be
    /// opened (#178); [`Error::PolicyDenied`] if policy denies;
    /// [`Error::Unsupported`] if the scheme has no registered provider.
    #[tracing::instrument(skip(self, actor), fields(path = %span_path(path), ?mode))]
    pub async fn delete_with_as(
        &self,
        path: &VPath,
        mode: DeleteMode,
        actor: crate::journal::Actor,
    ) -> Result<TaskHandle, Error> {
        self.gate(&actor, crate::policy::PolicyOp::Delete { mode }, &[path])
            .await?;
        let provider = self.provider_for(path).await?;
        let observer = Arc::clone(&self.observer);
        let path = path.clone();
        let key = path.scheme().to_owned();
        Ok(self.sched.submit(
            &key,
            TaskKind::Delete,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(
                    async move { ops::delete_task(provider, path, mode, observer, &ctx).await },
                )
            }),
        ))
    }

    /// Creates ONE directory as a Task (#104, F7). No `-p` (missing parent
    /// = `NotFound`), destination occupied = `Conflict{Exists}`. Journals
    /// `Created` with undo (rule 4).
    ///
    /// # Errors
    /// [`Error::Unsupported`] if the scheme has no registered provider.
    pub async fn mkdir(&self, path: &VPath) -> Result<TaskHandle, Error> {
        self.mkdir_as(path, crate::journal::Actor::User).await
    }

    /// [`Self::mkdir`] with an explicit ACTOR (the agentic path, M3-3):
    /// gated by [`crate::policy::PolicyOp::Mkdir`] PRE-effect.
    ///
    /// # Errors
    /// [`Error::JournalUnavailable`] if this session's journal cannot be
    /// opened (#178); [`Error::PolicyDenied`] if policy denies;
    /// [`Error::Unsupported`] if the scheme has no registered provider.
    #[tracing::instrument(skip(self, actor), fields(path = %span_path(path)))]
    pub async fn mkdir_as(
        &self,
        path: &VPath,
        actor: crate::journal::Actor,
    ) -> Result<TaskHandle, Error> {
        self.gate(&actor, crate::policy::PolicyOp::Mkdir, &[path])
            .await?;
        let provider = self.provider_for(path).await?;
        let observer = Arc::clone(&self.observer);
        let path = path.clone();
        let key = path.scheme().to_owned();
        Ok(self.sched.submit(
            &key,
            TaskKind::Mkdir,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move { ops::mkdir_task(provider, path, observer, &ctx).await })
            }),
        ))
    }

    /// Creates an EMPTY file (#290).
    ///
    /// # Errors
    /// [`Error::Unsupported`] if the scheme has no registered provider.
    pub async fn create_file(&self, path: &VPath) -> Result<TaskHandle, Error> {
        self.create_file_as(path, None, crate::journal::Actor::User)
            .await
    }

    /// [`Self::create_file`] with an explicit ACTOR: gated by
    /// [`crate::policy::PolicyOp::Create`] PRE-effect.
    ///
    /// # Errors
    /// [`Error::JournalUnavailable`] if this session's journal cannot be
    /// opened (#178); [`Error::PolicyDenied`] if policy denies;
    /// [`Error::Unsupported`] if the scheme has no registered provider.
    #[tracing::instrument(skip(self, actor), fields(path = %span_path(path)))]
    pub async fn create_file_as(
        &self,
        path: &VPath,
        dest_anchor: Option<norte_proto::DirAnchor>,
        actor: crate::journal::Actor,
    ) -> Result<TaskHandle, Error> {
        self.gate(&actor, crate::policy::PolicyOp::Create, &[path])
            .await?;
        let provider = self.provider_for(path).await?;
        let observer = Arc::clone(&self.observer);
        let path = path.clone();
        let key = path.scheme().to_owned();
        Ok(self.sched.submit(
            &key,
            TaskKind::Create,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move {
                    ops::create_task(provider, path, dest_anchor, observer, &ctx).await
                })
            }),
        ))
    }

    /// Writes a file with content from memory as a Task (ADR 0101): a
    /// hook's sidecar. Gated PRE-effect by
    /// [`crate::policy::PolicyOp::Create`] and, if `on_exists` is
    /// [`OnExists::Replace`], also by `Delete{Trash}`: replacing is burying
    /// what was there and creating, two permissions.
    ///
    /// # Errors
    /// [`Error::InvalidPath`] if `content` exceeds
    /// [`norte_plugin_host::MAX_SIDECAR_BYTES`]; [`Error::JournalUnavailable`]
    /// if the journal cannot be opened (#178); [`Error::PolicyDenied`] if
    /// policy denies; [`Error::Unsupported`] with no provider for the
    /// scheme.
    #[tracing::instrument(skip(self, actor, content), fields(path = %span_path(path)))]
    pub async fn write_file_as(
        &self,
        path: &VPath,
        content: Vec<u8>,
        on_exists: OnExists,
        actor: crate::journal::Actor,
    ) -> Result<TaskHandle, Error> {
        if content.len() > norte_plugin_host::MAX_SIDECAR_BYTES {
            return Err(Error::InvalidPath);
        }
        self.gate(&actor, crate::policy::PolicyOp::Create, &[path])
            .await?;
        if on_exists == OnExists::Replace {
            self.gate(
                &actor,
                crate::policy::PolicyOp::Delete {
                    mode: norte_proto::DeleteMode::Trash,
                },
                &[path],
            )
            .await?;
        }
        let provider = self.provider_for(path).await?;
        let observer = Arc::clone(&self.observer);
        let path = path.clone();
        let key = path.scheme().to_owned();
        Ok(self.sched.submit(
            &key,
            TaskKind::Create,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move {
                    ops::write_task(provider, path, content, on_exists, observer, &ctx).await
                })
            }),
        ))
    }

    /// Changes the POSIX permissions of a batch of paths (#314, ADR 0081).
    ///
    /// # Errors
    /// [`Error::InvalidPath`] with no paths, above the cap, or with bits
    /// that are not permission bits; [`Error::PolicyDenied`] if policy
    /// denies; [`Error::Unsupported`] if some scheme has no provider.
    pub async fn set_mode(
        &self,
        params: norte_proto::methods::FsSetModeParams,
    ) -> Result<TaskHandle, Error> {
        self.set_mode_as(params, crate::journal::Actor::User).await
    }

    /// [`Self::set_mode`] with an explicit ACTOR: gated by
    /// [`crate::policy::PolicyOp::SetMode`] PRE-effect, over ALL the paths.
    ///
    /// The gate covers the whole list and runs before the first write: a
    /// batch that started changing permissions and ran into policy halfway
    /// would leave half the selection changed for a request that was
    /// denied.
    ///
    /// # Errors
    /// Those of [`Self::set_mode`].
    #[tracing::instrument(skip(self, params, actor), fields(n = params.paths.len()))]
    pub async fn set_mode_as(
        &self,
        params: norte_proto::methods::FsSetModeParams,
        actor: crate::journal::Actor,
    ) -> Result<TaskHandle, Error> {
        let Some(first) = params.paths.first() else {
            tracing::debug!("fs.set_mode with no paths");
            return Err(Error::InvalidPath);
        };
        if params.paths.len() > norte_proto::methods::FS_SET_MODE_MAX_PATHS {
            tracing::debug!(n = params.paths.len(), "fs.set_mode above the cap");
            return Err(Error::InvalidPath);
        }
        // The upper bits say what CLASS the node is. Silently trimming them
        // would leave a permission nobody asked for; it is rejected.
        if params.mode & !norte_proto::methods::MODE_PERMISSION_BITS != 0 {
            tracing::debug!(
                mode = params.mode,
                "fs.set_mode with bits that are not permission bits"
            );
            return Err(Error::InvalidPath);
        }
        // setuid and setgid, by hand only (ADR 0081). Not because those bits
        // are THE danger — a `chmod 0777` on `~/.ssh` does far more harm and
        // carries none — but because they are what the approver CANNOT SEE:
        // the approval request carries the op and the paths, not the mode,
        // so a human would say yes to "set-mode on 12 paths" without knowing
        // whether it was `0600` or `4777`. As long as the mode does not
        // travel in that question, an agent does not set them; the human
        // does, from a dialog that does show them.
        const SPECIAL_BITS: u32 = 0o6000;
        if params.mode & SPECIAL_BITS != 0 && !matches!(actor, crate::journal::Actor::User) {
            tracing::warn!(
                mode = params.mode,
                "fs.set_mode with setuid/setgid from an actor that is not the human: denied"
            );
            return Err(Error::PolicyDenied {
                rule: "set-mode.special-bits".to_owned(),
            });
        }
        // `dir_mode` goes through the SAME two checks as `mode`, and BEFORE
        // the gate just like them (#315): a mode that is going to be
        // rejected cannot spend a human's approval beforehand — one that,
        // besides, would have seen it with no such value inside.
        //
        // And it only means something with `recursive`: with no descent
        // into the tree there are no directories to apply it to, and
        // applying it to the requested paths would give them a permission
        // the caller never asked for. It is discarded here, which is where
        // the wire says it is ignored.
        let dir_mode = params.recursive.then_some(params.dir_mode).flatten();
        if dir_mode.is_some_and(|m| m & !norte_proto::methods::MODE_PERMISSION_BITS != 0) {
            tracing::debug!("fs.set_mode with a dir_mode that is not permission bits");
            return Err(Error::InvalidPath);
        }
        if dir_mode.is_some_and(|m| m & SPECIAL_BITS != 0)
            && !matches!(actor, crate::journal::Actor::User)
        {
            tracing::warn!("fs.set_mode: dir_mode with setuid/setgid from a non-human actor");
            return Err(Error::PolicyDenied {
                rule: "set-mode.special-bits".to_owned(),
            });
        }
        let refs: Vec<&VPath> = params.paths.iter().collect();
        // The question carries the SCOPE, not just the mode (#315): a
        // recursive over one root is a hundred thousand nodes and
        // `paths_total` says 1.
        self.gate(
            &actor,
            crate::policy::PolicyOp::SetMode {
                mode: params.mode,
                recursive: params.recursive,
                dir_mode,
            },
            &refs,
        )
        .await?;
        let key = first.scheme().to_owned();
        let mut paths = Vec::with_capacity(params.paths.len());
        for p in &params.paths {
            let provider = self.provider_for(p).await?;
            paths.push((provider, p.clone()));
        }
        let observer = Arc::clone(&self.observer);
        // A recursive is N journal entries that were ONE action, so they go
        // under a batch (#315) — like the rename executor. With no journal
        // there is no batch to request, and then the entries go loose: what
        // is lost is being able to say they were one, not the undo.
        let batch = if params.recursive {
            match self.journal().await {
                Some(j) => j.journal().alloc_batch().await.ok(),
                None => None,
            }
        } else {
            None
        };
        let options = ops::SetModeOptions {
            mode: params.mode,
            recursive: params.recursive,
            dir_mode,
            batch,
        };
        Ok(self.sched.submit(
            &key,
            TaskKind::SetMode,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move { ops::set_mode(paths, options, observer, &ctx).await })
            }),
        ))
    }

    /// Capabilities of the LOCATION `p` (so the frontend can decide, e.g.,
    /// whether F8 goes to trash or warns about permanent deletion).
    ///
    /// Since ADR 0054 it answers for the location and not the whole
    /// provider: the wire method (`fs.capabilities`) always took a path and
    /// until now answered the same thing for any of them, which is false as
    /// soon as a machine mounts two different filesystems.
    ///
    /// # Errors
    /// [`Error::Unsupported`] if the scheme has no registered provider, and
    /// whatever `p` produces in the provider ([`Error::NotFound`] if it does
    /// not exist).
    pub async fn capabilities(&self, p: &VPath) -> Result<norte_proto::Capabilities, Error> {
        self.provider_for(p).await?.capabilities_at(p).await
    }

    /// Sweeps orphaned `.norte-partial` staging (prior crashes, ADR 0012 /
    /// #11) under `dir`, delegating to the provider that serves it. A
    /// ONE-OFF operation (not a Task) and NOT recorded in the journal (it is
    /// not a user mutation). Providers with no local staging return 0.
    ///
    /// There is no automatic sweep at startup: `gc_partials` is single-dir
    /// and there is no reliable managed root until the journal records
    /// in-flight staging (debt, a future M3 increment).
    ///
    /// # Errors
    /// [`Error::Unsupported`] if the scheme has no provider; the provider's
    /// own.
    pub async fn gc_partials(
        &self,
        dir: &VPath,
        older_than: std::time::Duration,
    ) -> Result<usize, Error> {
        self.provider_for(dir)
            .await?
            .gc_partials(dir, older_than)
            .await
    }

    /// Undoes, LIFO, the reversible mutations of session `actor`, without
    /// stepping on the user's work (strict: stops at the first conflict).
    /// Every undo is recorded as a compensating entry (append-only). A
    /// cancelable Task with progress; the [`crate::UndoReport`] fills in
    /// inside the returned `Arc<Mutex<…>>` and is complete once the Task
    /// ends.
    ///
    /// The `actor` SELECTS which session to undo AND acts as the executor:
    /// the policy gate is evaluated with it and the compensations record it
    /// (so an agent that undoes its own session stays subject to its
    /// scope/policy, and the compensations carry the real actor). The case
    /// "a human undoes an agent's session" (performer ≠ target) is
    /// [`Self::undo_session_for`].
    ///
    /// Undo goes through the policy gate (M3-3, rule 9): each reversal is
    /// evaluated as its inverse `PolicyOp`, unit by unit, INSIDE the Task
    /// (#171); a denial blocks that unit and the loop continues with the
    /// rest (`report.denied`), while a real conflict does stop the LIFO
    /// (`report.blocked`).
    ///
    /// **The provider is resolved BEFORE the gate**, during planning, and
    /// that has to be written down because the text here used to say the
    /// opposite: when the gate moved inside the Task, `provider_for` was
    /// left outside. So the journal CAN drive opening a remote connection
    /// before policy weighs in on the reversal. That is accepted because
    /// resolving a provider mutates nothing and the journal's paths were
    /// written by this very core; what is not accepted is the rustdoc
    /// claiming a guarantee the code does not give.
    ///
    /// # Errors
    /// [`Error::JournalUnavailable`] if this session's journal exists but
    /// cannot be OPENED (#178).
    /// [`Error::Unsupported`] if the Engine has no journal
    /// ([`Self::with_journal`]), or if some journal path has no registered
    /// provider.
    ///
    /// # Panics
    /// If the report's internal `Mutex` is poisoned (only if a prior holder
    /// panicked while holding it — does not happen in practice).
    pub async fn undo_session(
        &self,
        actor: crate::journal::Actor,
    ) -> Result<(TaskHandle, Arc<std::sync::Mutex<crate::UndoReport>>), Error> {
        self.undo_session_for(&actor.clone(), actor).await
    }

    /// Like [`Self::undo_session`] but separating WHO gets undone from WHO
    /// executes (M3-4, M3-2 debt): `target` selects the journal entries to
    /// revert; `executor` passes the policy gate and signs the
    /// compensations. The human-undoes-agent case uses
    /// `(Agent{session}, User)`: the undo does not die because the agent's
    /// scope expired — the human executes it.
    ///
    /// # Errors
    /// Those of [`Self::undo_session`].
    ///
    /// # Panics
    /// Like [`Self::undo_session`] (poisoned report Mutex; does not happen).
    pub async fn undo_session_for(
        &self,
        target: &crate::journal::Actor,
        executor: crate::journal::Actor,
    ) -> Result<(TaskHandle, Arc<std::sync::Mutex<crate::UndoReport>>), Error> {
        // An UNREADABLE journal is said with its own category and not as
        // "unsupported" (#178): both paths end with no chain to read, but
        // only one of them has a specific file to fix.
        self.journal_gate().await?;
        let journal = self.journal().await.ok_or(Error::Unsupported)?;
        let entries = journal
            .journal()
            .revertible_for(target)
            .await
            .map_err(Error::from)?;
        self.undo_entries(entries, executor).await
    }

    /// Undoes, LIFO, what the HUMAN did AFTER `after_seq` (phase 7 of the
    /// WOW program, `journal.undo_after`).
    ///
    /// The `after_seq` entry stays: it is the state to return to.
    ///
    /// It is [`Self::undo_session_for`] with a different SELECTION criterion
    /// and nothing else — the same units, the same per-unit policy gate, the
    /// same strict LIFO that stops at the first block, the same counters and
    /// the same report. That is not a coincidence that has to be kept in
    /// sync by hand: the two call the same private body, where all of that
    /// lives once. A second, "similar", undo written separately would have
    /// diverged at the first rule anyone tuned.
    ///
    /// A BATCH goes in whole or not at all: if the cut falls in the middle
    /// of one, that whole batch is left out. Splitting it would revert half
    /// a unit believing it whole, and taking it in whole would undo entries
    /// before the cut. The why, along with the query's shape, is on
    /// `SELECT_REVERTIBLE_AFTER`.
    ///
    /// `upto_seq` is the CEILING (0.80.0): nothing with a bigger `seq` gets
    /// undone. It is the newest thing the human had counted in front of
    /// them; without it, what happened after the timeline was painted would
    /// enter an undo that never counted it. `None` = no ceiling.
    ///
    /// # Errors
    /// Those of [`Self::undo_session_for`], and [`Error::NotFound`] if `seq`
    /// names no entry — a cut that does not exist is not interpreted as
    /// "from the beginning".
    ///
    /// # Panics
    /// Like [`Self::undo_session`] (poisoned report Mutex; does not happen).
    pub async fn undo_after(
        &self,
        after_seq: i64,
        upto_seq: Option<i64>,
    ) -> Result<(TaskHandle, Arc<std::sync::Mutex<crate::UndoReport>>), Error> {
        self.journal_gate().await?;
        let journal = self.journal().await.ok_or(Error::Unsupported)?;
        // The cut has to NAME an entry that exists, not just be a number.
        // `seq > 0` would select EVERYTHING the human ever did since the
        // beginning of time, and that zero is exactly what comes out of a
        // stale cursor or a client that translates "nothing is marked" to
        // zero. Undoing too little gets asked for again; undoing someone's
        // entire history because their client sent a zero, no.
        //
        // Checking it against the journal, instead of a plain `>= 1`, also
        // covers the cursor of an entry that is no longer there.
        if journal
            .journal()
            .entry_hash_at(after_seq)
            .await
            .map_err(Error::from)?
            .is_none()
        {
            return Err(Error::NotFound);
        }
        let entries = journal
            .journal()
            .revertible_for_after(&crate::journal::Actor::User, after_seq, upto_seq)
            .await
            .map_err(Error::from)?;
        self.undo_entries(entries, crate::journal::Actor::User)
            .await
    }

    /// One PAGE of the journal going back (phase 7, `journal.list`).
    ///
    /// A pure read: it touches nothing and does not go through the policy
    /// gate, which governs mutations. Who can ask it is decided by the edge
    /// — the daemon only serves it to a human connection —, which is where
    /// it is known who is on the other side of the socket.
    ///
    /// # Errors
    /// [`Error::Unsupported`] with no journal; the category specific to an
    /// UNREADABLE journal (#178), which is not the same thing;
    /// [`Error::Internal`] from sqlite.
    pub async fn journal_page(
        &self,
        before_seq: Option<i64>,
        limit: u32,
        actor_kind: Option<&str>,
    ) -> Result<Vec<crate::journal::PageEntry>, Error> {
        self.journal_gate().await?;
        let journal = self.journal().await.ok_or(Error::Unsupported)?;
        // The cap is applied HERE and not only at each edge: the two that
        // exist today do it, and the third one that comes along would
        // inherit the bound instead of having to remember to set it.
        let limit = limit.clamp(1, norte_proto::methods::JOURNAL_LIST_MAX_PAGE);
        journal
            .journal()
            .page(before_seq, limit, actor_kind)
            .await
            .map_err(Error::from)
    }

    /// The SHARED body of an undo: groups into units, resolves each one's
    /// provider, and launches the Task that reverts them LIFO with the
    /// policy gate unit by unit.
    ///
    /// What varies between undoing a session and undoing up to a point is
    /// WHICH entries go in, and that is decided by the caller. Everything
    /// else — and that is where the rules that hurt if they diverge are —
    /// lives here.
    ///
    /// # Errors
    /// Those of [`Self::undo_session_for`].
    ///
    /// # Panics
    /// Like [`Self::undo_session`] (poisoned report Mutex; does not happen).
    #[expect(
        clippy::too_many_lines,
        reason = "the Task's body is ONE sequence — turn, recheck, gate, \
                  reversal, report — and splitting it hides the order, which is the rule"
    )]
    async fn undo_entries(
        &self,
        entries: Vec<crate::journal::JournalEntry>,
        executor: crate::journal::Actor,
    ) -> Result<(TaskHandle, Arc<std::sync::Mutex<crate::UndoReport>>), Error> {
        // The journal is requested again here instead of received from the
        // caller: this is the one that travels INSIDE the Task to write the
        // compensations, and every caller already checked its own in order
        // to read the entries.
        let journal = self.journal().await.ok_or(Error::Unsupported)?;
        let report = Arc::new(std::sync::Mutex::new(crate::UndoReport::default()));

        // A batch (`fs.rename_batch`) is ONE unit: it reverts whole or is
        // not touched at all (design §7). Grouping happens before the gate
        // so policy sees the whole batch, same as on the way in.
        let units = crate::undo::undo_units(entries);

        // Planning: it ONLY resolves each unit's provider, which is the only
        // thing that needs `&self` (the Task's body is `'static`). The gate
        // is asked INSIDE, unit by unit — #171.
        //
        // What this takes off the caller's thread is what used to grow
        // unbounded: `undo_gate_targets` parses up to `unit.len() * 2`
        // `VPath`s, and a sync unit has one step per entry. With a
        // half-million-entry `Mirror`, `policy.undo_session` spent half a
        // million parses before returning a `task_id`, with no progress and
        // no way to cancel. Now it returns the id right away and the work
        // goes inside, with the token in the loop (hard rule 3).
        //
        // The ANCHOR IS parsed here, but it is ONE path per unit and not two
        // per entry: it is what says which provider to ask.
        let mut plan: Vec<(Vec<crate::journal::JournalEntry>, Arc<dyn Provider>)> =
            Vec::with_capacity(units.len());
        for unit in units {
            let Some(first) = unit.first() else {
                continue; // impossible: `undo_units` never produces empty units.
            };
            let anchor = wire_engine(&first.path)?;
            let provider = self.provider_for(&anchor).await?;
            plan.push((unit, provider));
        }
        let checker = self.policy_checker();

        let report_task = Arc::clone(&report);
        let owner = executor.clone();
        let in_progress = Arc::clone(&self.undo_in_progress);
        let key = "undo".to_owned();
        let handle = self.sched.submit(
            &key,
            TaskKind::Undo,
            Priority::Normal,
            executor,
            Box::new(move |ctx| {
                Box::pin(async move {
                    // The total is UNITS: a batch advances the counter once,
                    // because for the human it is a single undo step.
                    let total = plan.len() as u64;
                    let task_id = ctx.progress.snapshot().task_id;
                    ctx.progress.update(|p| p.entries_total = Some(total));
                    // One undo at a time (#358), and the wait can be
                    // cancelled: an undo queued behind another long one
                    // holds nobody up.
                    let _turn = tokio::select! {
                        g = in_progress.lock() => g,
                        () = ctx.cancel.cancelled() => return Err(Error::Cancelled),
                    };
                    // #358: what was chosen could have been undone by
                    // another undo in the meantime. It is asked once, now
                    // with the turn: from here on only this Task writes
                    // undo compensations.
                    let seqs: Vec<i64> = plan
                        .iter()
                        .flat_map(|(u, _)| u.iter().map(|e| e.seq))
                        .collect();
                    let already_undone = crate::undo::deshechas(&journal, &seqs).await?;
                    for (unit, provider) in plan {
                        if ctx.cancel.is_cancelled() {
                            return Err(Error::Cancelled);
                        }
                        let unit = match crate::undo::validity(unit, &already_undone) {
                            crate::undo::Validity::Whole(u) | crate::undo::Validity::InPart(u) => u,
                            crate::undo::Validity::Undone => {
                                tracing::info!(
                                    %task_id,
                                    "unit already undone by another undo; skipping"
                                );
                                ctx.progress.update(|p| p.entries_done += 1);
                                continue;
                            }
                            // A batch half-undone by another undo: it is
                            // neither continued nor skipped — it stops, like
                            // on a drift.
                            crate::undo::Validity::Parada(seq) => {
                                report_task.lock().expect("undo report lock").blocked =
                                    Some((seq, Error::PlanStale));
                                break;
                            }
                        };
                        // The gate, HERE and per unit (#171). Two things
                        // change compared to asking it all up front: the
                        // parsing work goes inside the Task, and the
                        // verdict is NOW's — a scope that expires halfway is
                        // seen by this unit, not a snapshot from half an
                        // hour ago.
                        //
                        // And a denial does NOT kill the undo: it blocks ITS
                        // unit, leaves a row in the report and continues. It
                        // is the same decision the forward executor made in
                        // task 9 — `Deny` and `Ask` are a row, not a modal —
                        // and for the same reason: a half-million-step plan
                        // cannot be stopped dead by one. `blocked` keeps
                        // meaning what it meant: the LIFO stopped from
                        // DRIFT, and the tree stayed consistent.
                        let denial = undo_unit_denial(&checker, &ctx.actor, &unit).await;
                        if let Some(err) = denial {
                            if let Some(first) = unit.first() {
                                let mut r = report_task.lock().expect("undo report lock");
                                r.denied_total = r.denied_total.saturating_add(1);
                                if r.denied.len() < crate::undo::UNDO_MAX_DENIED_REPORTED {
                                    r.denied.push((first.seq, err));
                                }
                            }
                            ctx.progress.update(|p| p.entries_done += 1);
                            continue;
                        }
                        // `undone` does count ENTRIES: it is what the unit
                        // undid from the journal, and a batch undoes all of
                        // its own.
                        let members = unit.len() as u64;
                        // Which undo applies to the unit is decided by
                        // `revert_unit`, by the SHAPE of its entries: a
                        // loose one, a rename batch (whole or nothing), or a
                        // sync one (whatever can be, naming what cannot).
                        let outcome = crate::undo::revert_unit(
                            &*provider,
                            &journal,
                            &unit,
                            &ctx.actor,
                            &ctx.cancel,
                            task_id,
                            &report_task,
                        )
                        .await?;
                        match outcome {
                            crate::undo::Reverted::Done => {
                                report_task.lock().expect("undo report lock").undone += members;
                            }
                            // The unit already split its entries across the
                            // counters (a sync batch reverts part and skips
                            // part): adding `members` here would count as
                            // undone what did not come back.
                            crate::undo::Reverted::Accounted => {}
                            crate::undo::Reverted::SkippedIrreversible => {
                                report_task
                                    .lock()
                                    .expect("undo report lock")
                                    .skipped_irreversible += 1;
                            }
                            crate::undo::Reverted::SkippedNoTrash => {
                                report_task
                                    .lock()
                                    .expect("undo report lock")
                                    .skipped_created_no_trash += 1;
                            }
                            // It is counted and CONTINUES, unlike a block
                            // (#371): the node was changed by the reader,
                            // it is not an unexplained divergence, and
                            // stopping everything for a file the reader
                            // edited themselves would leave the rest of the
                            // copy undone.
                            crate::undo::Reverted::SkippedNotOurs => {
                                report_task
                                    .lock()
                                    .expect("undo report lock")
                                    .skipped_not_ours += 1;
                            }
                            crate::undo::Reverted::Blocked { seq, error } => {
                                report_task.lock().expect("undo report lock").blocked =
                                    Some((seq, error));
                                break; // strict: stops at the first block.
                            }
                            // The tree did NOT come back: the Task FAILS.
                            // `Completed` promises a restored tree and here
                            // it is not one; the step that stayed applied
                            // goes with names in `UndoReport::batch_stuck`.
                            crate::undo::Reverted::Stuck { seq, error } => {
                                report_task.lock().expect("undo report lock").blocked =
                                    Some((seq, error.clone()));
                                return Err(error);
                            }
                        }
                        ctx.progress.update(|p| p.entries_done += 1);
                    }
                    Ok(())
                })
            }),
        );
        {
            let mut ring = self
                .undo_reports
                .lock()
                .expect("undo_reports lock is sound");
            ring.push_back((handle.id(), owner, Arc::clone(&report)));
            evict_undo_reports(&mut ring);
        }
        Ok((handle, report))
    }

    /// Releases the report of an undo whose id never reached anyone.
    ///
    /// The engine retains the report when it LAUNCHES the Task, and the
    /// daemon can later answer OVERLOADED without delivering the id. That
    /// report has nobody left to serve, and keeping it only left an id in
    /// the ring that any other human connection could request by
    /// enumeration.
    pub fn forget_undo_report(&self, task_id: TaskId) {
        self.undo_reports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|(id, _, _)| *id != task_id);
    }

    /// The report of an undo (`policy.undo_report`, #71): a snapshot, final
    /// once the Task is terminal. `None` if that id was never an undo or the
    /// ring already evicted it ([`UNDO_REPORTS_MAX`]). The actor is whoever
    /// executed it: the daemon needs it to decide who can read it.
    ///
    /// # Panics
    /// Never in practice: the report's lock only gets poisoned if the Task
    /// panics mid-write, and it is read regardless.
    #[must_use]
    pub fn undo_report(
        &self,
        task_id: TaskId,
    ) -> Option<(crate::journal::Actor, crate::UndoReport)> {
        let ring = self
            .undo_reports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ring.iter()
            .find(|(id, _, _)| *id == task_id)
            .map(|(_, owner, r)| {
                (
                    owner.clone(),
                    r.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone(),
                )
            })
    }
}

/// Shape of a `VPath` for tracing SPANS: `display_lossy`, unless the
/// authority's userinfo contains a `:` — an inline password in the URL
/// (`sftp://u:pass@h`) is rejected when the connection is parsed, but the
/// span opens BEFORE that: it must never reach a log (rule 10). `pub(crate)`:
/// it also redacts `index_embed`'s logs.
pub(crate) fn span_path(p: &VPath) -> String {
    match p.authority() {
        Some(a) if a.rsplit_once('@').is_some_and(|(ui, _)| ui.contains(':')) => {
            format!("<{} ***>", p.scheme())
        }
        _ => p.display_lossy().clone(),
    }
}

/// Translates a [`Spool::open`](crate::sync::Spool::open) failure into the
/// wire's taxonomy.
///
/// The three shapes of "there is no live plan with that hash" — it is not
/// there, it expired, it was tampered with — are the SAME answer for the
/// client, and it is a useful answer: replan. Only a real I/O failure is the
/// daemon's fault.
fn spool_open_error(e: &crate::sync::SpoolError) -> Error {
    if e.is_stale() {
        Error::PlanStale
    } else {
        Error::Io { retryable: false }
    }
}

/// The same, but MID-execution: the file was truncated or edited after
/// opening it.
///
/// Here `PlanStale` can no longer be the answer — the Task exists and has
/// written — so it comes out as a Task failure. What was applied up to that
/// point stays journaled under its batch and is undoable.
fn spool_read_error(e: &crate::sync::SpoolError) -> Error {
    match e {
        // A file this binary wrote minutes ago and that stops parsing
        // halfway is not a daemon failure: it is a truncated or tampered
        // file. `Io` says so and `Internal` would bury it as our own fault.
        crate::sync::SpoolError::Io(_) | crate::sync::SpoolError::Malformed(_) => {
            Error::Io { retryable: false }
        }
        _ => Error::Internal { panic: false },
    }
}

/// Is `path` AT `root` or below it?
///
/// Delegated to [`RelPath::under`] (`norte-proto`), which does the same
/// per-segment, raw-byte comparison (hard rule 1) and is the only
/// implementation since #172 — there used to be three hand-written copies.
/// The root itself DOES count as contained, which is what this caller needs
/// (see [`structural_overlap`] and [`folded_overlap`]).
fn is_at_or_under(root: &VPath, path: &VPath) -> bool {
    RelPath::under(root, path).is_some()
}

/// How two roots overlap by LOOKING at them, with no disk access.
///
/// The three cases are distinct and none is a degenerate case of another:
/// "they are the same tree" is not "one is inside the other", and that is
/// exactly the phrase a frontend paints. `None` = they do not overlap
/// structurally, which is **not** the same as not overlapping at all (see
/// [`Engine::sync_plan_as`]).
fn structural_overlap(source: &VPath, dest: &VPath) -> Option<norte_proto::RootOverlap> {
    use norte_proto::RootOverlap;
    if source == dest {
        Some(RootOverlap::Same)
    } else if is_at_or_under(dest, source) {
        Some(RootOverlap::SourceInsideDest)
    } else if is_at_or_under(source, dest) {
        Some(RootOverlap::DestInsideSource)
    } else {
        None
    }
}

/// [`is_at_or_under`] with the key that MATCHES names, instead of their raw
/// bytes.
///
/// Same shape — scheme, authority and then segment by segment — and a single
/// difference: each segment is compared by its
/// [`key_for`](norte_compare::key_for), which is exactly the key
/// `norte-compare` uses to decide two names are the SAME name. Reused and not
/// rewritten: a third copy of the folding table would be a third answer to
/// "do these two names collide?" (#151, #129).
fn folded_is_at_or_under(root: &VPath, path: &VPath, sides: norte_compare::Sides) -> bool {
    if path.scheme() != root.scheme() || path.authority() != root.authority() {
        return false;
    }
    let mut rest = path.segments();
    root.segments().all(|segment| {
        rest.next().is_some_and(|other| {
            norte_compare::key_for(other, sides) == norte_compare::key_for(segment, sides)
        })
    })
}

/// [`structural_overlap`] over a pair of roots that does NOT distinguish
/// case.
///
/// It exists because the literal check does not catch the containment a
/// folding volume DOES see: on APFS or NTFS, `source=/Data` and
/// `dest=/data/backup` are not byte-for-byte equal, neither hangs from the
/// other byte-for-byte, and their [`NodeId`](norte_vfs::NodeId)s are
/// different because they are different directories — they pass all three
/// gates, and afterward the plan copies a tree into itself. The walk's guard
/// compares the same way and does not see it either.
///
/// It applies when EITHER provider does not declare
/// [`CapabilityFlags::CASE_SENSITIVE`](norte_proto::CapabilityFlags::CASE_SENSITIVE),
/// which is the same criterion
/// [`Sides`](norte_compare::Sides) uses to decide it should fold a matching
/// key: a side that does not distinguish case cannot hold both spellings, so
/// comparing AGAINST it is folding even if the other side is ext4. And it is
/// real case folding, not a `to_lowercase` (the 22 code points from #129).
///
/// Folding comes with NFC normalization, because it is the same key: on a
/// volume that folds case, `/Café` NFC and `/cafe\u{301}` NFD name the same
/// directory for any purpose this check cares about.
fn folded_overlap(
    source: &VPath,
    dest: &VPath,
    sides: norte_compare::Sides,
) -> Option<norte_proto::RootOverlap> {
    use norte_proto::RootOverlap;
    let source_in_dest = folded_is_at_or_under(dest, source, sides);
    let dest_in_source = folded_is_at_or_under(source, dest, sides);
    match (source_in_dest, dest_in_source) {
        // Each inside the other can only be the same path once folded.
        (true, true) => Some(RootOverlap::Same),
        (true, false) => Some(RootOverlap::SourceInsideDest),
        (false, true) => Some(RootOverlap::DestInsideSource),
        (false, false) => None,
    }
}

/// Are the two roots the SAME directory, however they are spelled?
///
/// One `stat` per root, once per plan. Three things, all deliberate:
///
/// - **Only if the provider is the SAME object.** A [`NodeId`](norte_vfs::NodeId)
///   from two different backends is not comparable — a synthetic provider's
///   index and an ext4 inode can coincide without meaning anything — and a
///   false match here rejects a legitimate plan.
/// - **[`FollowLinks::Yes`](norte_vfs::FollowLinks::Yes)**, because the case
///   this exists to catch is exactly a root that is a symlink: with the
///   link's own identity, `/data` and `/srv/data` answer differently and the
///   check is useless. Listing a directory crosses the link just the same,
///   so this is the identity the walk is actually going to traverse.
/// - **An error or a `None` are NOT an overlap.** `node_id` is `None` on
///   SFTP and FTP (a known residual case), and a `NotFound` means the root
///   is not there — something the comparison below will say with an error
///   row, as `fs.compare` does today, instead of killing the request with a
///   different taxonomy.
async fn same_node(
    source_provider: &Arc<dyn Provider>,
    source: &VPath,
    dest_provider: &Arc<dyn Provider>,
    dest: &VPath,
) -> bool {
    use norte_vfs::FollowLinks;
    if !Arc::ptr_eq(source_provider, dest_provider) {
        return false;
    }
    let a = source_provider.node_id(source, FollowLinks::Yes).await;
    let b = dest_provider.node_id(dest, FollowLinks::Yes).await;
    match (a, b) {
        (Ok(Some(a)), Ok(Some(b))) => a == b,
        _ => false,
    }
}

/// Maps an AI gate denial to [`Error::PolicyDenied`] with the wire's CLOSED
/// vocabulary (M4-A2/IA-2): the category, never the config's detail. Shared
/// by `ai_rename_plan` and `index_embed_as`.
fn ai_denied_to_error(reason: &crate::ai::AiDenied) -> Error {
    Error::PolicyDenied {
        rule: match reason {
            crate::ai::AiDenied::Disabled => "ai-disabled",
            crate::ai::AiDenied::LocalOnly => "ai-local-only",
            crate::ai::AiDenied::DeniedPath => "ai-denied-path",
        }
        .to_owned(),
    }
}

/// The CATEGORY of an AI failure, for the metric. Never its text: a
/// `Protocol`'s `Display` carries the fragment that caused the rejection,
/// and that fragment was written by the model over the user's names — or it
/// is a real directory name, in the collision error.
fn ai_error_category(e: &norte_ai::AiError) -> &'static str {
    use norte_ai::AiError as A;
    match e {
        A::Protocol(_) => "parse",
        A::Auth => "auth",
        A::RateLimited { .. } => "rate_limit",
        A::Transport(_) => "transport",
        A::Cancelled => "cancelled",
        A::Unsupported => "unsupported",
        _ => "other",
    }
}

/// Maps a [`norte_ai::AiError`] to the wire's taxonomy (M4-A2). The wire
/// carries the category; the log gets what can be said without repeating
/// the user's data.
///
/// **`Protocol` is not logged with its text.** Its `Display` carries the
/// fragment that caused the rejection, and on the rename path that fragment
/// can be a name the model wrote — or a REAL one from the directory, in
/// `validate_rename_reply`'s collision error. It is user content, and a
/// daemon log or a diagnostic dump is no place for it (rule 10). What IS
/// said is that it was a protocol error: the category is what a log needs
/// for someone to know where to look.
///
/// The rest of the variants do carry text: `Http` is the status and the
/// provider's body, which has never seen any user name.
pub(crate) fn ai_to_proto_error(e: &norte_ai::AiError) -> Error {
    use norte_ai::AiError as A;
    match e {
        A::Auth => Error::PermissionDenied,
        A::Cancelled => Error::Cancelled,
        A::Unsupported => Error::Unsupported,
        A::RateLimited { .. } | A::Transport(_) => Error::ProviderUnavailable { retryable: true },
        A::Protocol(_) => {
            tracing::warn!("AI provider: response that does not match the format");
            Error::Internal { panic: false }
        }
        // Http and any future variant (AiError is non_exhaustive): a coarse
        // category, detail to the log.
        _ => {
            tracing::warn!(error = %e, "AI provider: unexpected response or status");
            Error::Internal { panic: false }
        }
    }
}

/// How many paths of an operation reach the frontend that approves it.
///
/// A PRESENTATION cap, not a decision one: policy is always evaluated over
/// the complete list. See the comment on [`Engine::gate`].
const APPROVAL_PATHS_SHOWN: usize = 32;

/// Cap on the directory entries a batch rename plans against.
///
/// The pairs cap (`FS_RENAME_BATCH_MAX_PAIRS`) does not bound the other
/// dimension, and the planner is linear in BOTH: one comparison key per
/// listing entry, plus two indexes. `fs.rename_batch_plan` is a DIRECT
/// response (ADR 0042): no Task, no cancellation token and inside the
/// dispatch, so a directory of millions of entries would be unbounded work
/// with no way to stop it. A directory a human is going to review entry by
/// entry fits here with plenty of room; above it, `LimitExceeded` is honest
/// and cheap.
pub const RENAME_BATCH_MAX_LISTING: usize = 100_000;

/// Returns the right to apply a plan if the `sync.apply` that claimed it is
/// abandoned halfway (see the comment on [`Engine::sync_apply_as`]).
///
/// It only releases the in-memory marker: it is a `Drop`, so it cannot wait
/// on a file deletion, and the file is collected by the TTL, the startup
/// sweep, or the connection closing.
struct ApplyClaim<'a> {
    spool: &'a crate::sync::Spool,
    conn_id: u64,
    plan_hash: &'a PlanHash,
    armed: bool,
}

impl Drop for ApplyClaim<'_> {
    fn drop(&mut self) {
        if self.armed {
            tracing::debug!(
                conn_id = self.conn_id,
                plan_hash = self.plan_hash.as_str(),
                "sync.apply abandoned before creating the Task: the plan is returned"
            );
            self.spool.abandon(self.conn_id, self.plan_hash);
        }
    }
}

/// One entry of a report ring: the Task, the ACTOR that requested it (so
/// whoever serves the report over the wire can decide whether the requester
/// could see that task) and the LIVE report, which the Task keeps filling
/// in.
type ReportEntry<T> = (TaskId, crate::journal::Actor, Arc<std::sync::Mutex<T>>);

/// The rename batch report ring ([`Engine::rename_batch_report`]).
type BatchReportEntry = ReportEntry<crate::rename::BatchReport>;

/// The plan application report ring ([`Engine::sync_report`]).
type SyncReportEntry = ReportEntry<norte_proto::methods::SyncReportResult>;

/// The `archive.test` report ring ([`Engine::archive_test_report`]).
type TestReportEntry = ReportEntry<norte_proto::methods::ArchiveTestResult>;

/// The undo report ring ([`Engine::undo_report`]).
type UndoReportEntry = ReportEntry<crate::UndoReport>;

/// The `archive.pack` report ring ([`Engine::archive_pack_report`]).
type PackReportEntry = ReportEntry<norte_proto::methods::ArchivePackReportResult>;

/// The `fs.checksum` report ring ([`Engine::checksum_report`]).
type ChecksumReportEntry = ReportEntry<norte_proto::methods::FsChecksumReportResult>;

/// Entry of the `fs.dir_usage` ring (0.75.0, phase 4).
type DirUsageReportEntry = ReportEntry<norte_proto::methods::FsDirUsageReportResult>;

/// Prunes the batch report ring down to its caps, ALWAYS sacrificing what is
/// least needed.
///
/// Two rules, and both exist because this ring is the ONLY channel by which
/// anyone finds out their directory was left half-renamed:
///
/// 1. **Per-class sub-cap** ([`BATCH_REPORTS_AGENTS_MAX`], same pattern as
///    M3-3b's scopes one): agent batches do not exhaust the ring. Without
///    it, `fs.rename_batch` is reachable by a scoped agent and 33 trivial
///    batches evict the report the human has not read yet — including the
///    one for the batch that same agent left half-applied.
/// 2. **What counts for nothing is evicted first**: between two reports, the
///    one that says everything went fine is thrown out before the one that
///    names a stuck step. A clean report is reconstructible by looking at
///    the directory; a stuck one is not.
///
/// AGE stays the criterion within each category, and if everything retained
/// is loud, the oldest is thrown out anyway: a daemon's memory over months
/// cannot grow with every batch, not even the bad ones.
fn evict_batch_reports(ring: &mut std::collections::VecDeque<BatchReportEntry>) {
    evict_reports(
        ring,
        BATCH_REPORTS_MAX,
        BATCH_REPORTS_AGENTS_MAX,
        // Does this report have something only it knows? It is evaluated
        // NOW and not on insertion: on insertion every report is empty — it
        // is at eviction time, once the old ones have finished, that it is
        // known which ones hurt.
        |r: &crate::rename::BatchReport| {
            r.stuck.is_some() || r.uncertain.is_some() || r.compensations_lost > 0
        },
    );
}

/// Prunes the `sync.apply` report ring with the same two rules.
///
/// Here "loud" is a report with FAILURES: a plan that applied whole leaves
/// the tree as the dialog promised and its report is reconstructible by
/// looking at it, while one with dropped steps names files that were not
/// copied and a journal batch to look them up in. `failures` is truncated
/// and `failed` is not, so the counter is what decides.
fn evict_sync_reports(ring: &mut std::collections::VecDeque<SyncReportEntry>) {
    evict_reports(
        ring,
        SYNC_REPORTS_MAX,
        SYNC_REPORTS_AGENTS_MAX,
        |r: &norte_proto::methods::SyncReportResult| r.failed > 0,
    );
}

/// The SHARED eviction for the two report rings, with the two rules both
/// need because both are the ONLY channel by which anyone finds out their
/// tree was left half-done:
///
/// 1. **Per-class sub-cap** (same pattern as M3-3b's scopes one): agent
///    reports occupy at most `agents_max` of the ring, so there are always
///    `max - agents_max` slots an agent cannot fill. Without it, an agent
///    with a scope and 33 trivial operations evicts the report the human
///    has not read yet — including the one for the operation that same
///    agent left half-done. What the sub-cap gives is a FLOOR, not immunity:
///    above it the second pass evicts by age with no regard for the owner,
///    so an agent CAN push out old, clean human reports. The loud ones
///    survive, which is what matters.
/// 2. **What counts for nothing is evicted first**: between two reports, the
///    one that says everything went fine is thrown out before the one that
///    names something broken. A clean report is reconstructible by looking
///    at the tree; a broken one is not.
///
/// AGE stays the criterion within each category, and if everything retained
/// is loud, the oldest is thrown out anyway: a daemon's memory over months
/// cannot grow with every operation, not even the bad ones.
fn evict_reports<T>(
    ring: &mut std::collections::VecDeque<ReportEntry<T>>,
    max: usize,
    agents_max: usize,
    loud: fn(&T) -> bool,
) {
    fn drop_one<T>(
        ring: &mut std::collections::VecDeque<ReportEntry<T>>,
        agents_only: bool,
        loud: fn(&T) -> bool,
    ) {
        let candidates = || {
            ring.iter()
                .enumerate()
                .filter(|(_, e)| !agents_only || !matches!(e.1, crate::journal::Actor::User))
        };
        let quiet = candidates()
            .find(|(_, e)| !loud(&e.2.lock().expect("report lock")))
            .map(|(i, _)| i);
        let victim = quiet.or_else(|| candidates().map(|(i, _)| i).next());
        if let Some(i) = victim {
            ring.remove(i);
        }
    }
    while ring
        .iter()
        .filter(|e| !matches!(e.1, crate::journal::Actor::User))
        .count()
        > agents_max
    {
        drop_one(ring, true, loud);
    }
    while ring.len() > max {
        drop_one(ring, false, loud);
    }
}

/// How many batch reports [`Engine::rename_batch_report`] retains.
///
/// A ring, not a map: a report is requested once, right after its Task's
/// terminal state, and one nobody collects has to expire on its own or the
/// daemon accumulates memory for every batch that ran in its lifetime. The
/// same criterion (and the same order of magnitude) as the undo report ring
/// ([`UNDO_REPORTS_MAX`]).
pub const BATCH_REPORTS_MAX: usize = 32;

/// How many of the [`BATCH_REPORTS_MAX`] the set of NON-human actors can
/// occupy. A per-class sub-cap, like M3-3b's per-connection scopes one: the
/// human keeps their margin no matter what happens on the other side. See
/// [`evict_batch_reports`].
pub const BATCH_REPORTS_AGENTS_MAX: usize = 16;

/// How many application reports [`Engine::sync_report`] retains. Same
/// criterion and same order of magnitude as [`BATCH_REPORTS_MAX`]: it is
/// requested once, right after its Task's terminal state, and one nobody
/// collects has to expire on its own.
pub const SYNC_REPORTS_MAX: usize = 32;

/// The per-class sub-cap of the `sync.apply` ring, twin of
/// [`BATCH_REPORTS_AGENTS_MAX`]. Not re-exported at the crate root for the
/// same reason as its twin: the cap a client needs to know is the total.
pub(crate) const SYNC_REPORTS_AGENTS_MAX: usize = 16;

/// How many `archive.test` reports are retained. The third of the family,
/// same criterion.
pub const TEST_REPORTS_MAX: usize = 32;

/// Per-class sub-cap of the `archive.test` ring.
pub(crate) const TEST_REPORTS_AGENTS_MAX: usize = 16;

/// How many undo reports [`Engine::undo_report`] retains. Eight, the number
/// it had when it lived in the daemon: undos are rare and a person requests
/// them.
pub const UNDO_REPORTS_MAX: usize = 8;

/// Per-class sub-cap of the undo ring. Today every undo is executed by the
/// human, so it does not bite; it is there so that stays true the day it
/// stops being so — `Engine::undo_session` accepts any actor —, same as in
/// the twin rings: half.
pub(crate) const UNDO_REPORTS_AGENTS_MAX: usize = 4;

/// Eviction for the undo ring: what counts for nothing goes first. An undo
/// that stopped, was left half-done, lost compensations, or had denied
/// units is the only place the human finds out the tree did not come back
/// whole.
fn evict_undo_reports(ring: &mut std::collections::VecDeque<UndoReportEntry>) {
    evict_reports(
        ring,
        UNDO_REPORTS_MAX,
        UNDO_REPORTS_AGENTS_MAX,
        |r: &crate::UndoReport| {
            r.blocked.is_some()
                || r.batch_stuck.is_some()
                || r.compensations_lost > 0
                || r.denied_total > 0
        },
    );
}

/// Eviction for the `archive.test` ring: what counts for nothing — an
/// archive that passed whole — is sacrificed before a report with failures,
/// which is the only place anyone can find out which entry is corrupt.
fn evict_test_reports(ring: &mut std::collections::VecDeque<TestReportEntry>) {
    evict_reports(
        ring,
        TEST_REPORTS_MAX,
        TEST_REPORTS_AGENTS_MAX,
        |r: &norte_proto::methods::ArchiveTestResult| !r.failed.is_empty() || r.truncated,
    );
}

/// Cap of the `archive.pack` ring. Same number as `archive.test`'s and
/// **tied on purpose**: they are reports of the same family and the same
/// size, and sharing a cap is a decision, not the reflection of having
/// copied the constant next door. Changing one and not the other should
/// cost writing down why.
pub(crate) const PACK_REPORTS_MAX: usize = TEST_REPORTS_MAX;

/// Per-class sub-cap of the `archive.pack` ring, tied the same way.
pub(crate) const PACK_REPORTS_AGENTS_MAX: usize = TEST_REPORTS_AGENTS_MAX;

/// Eviction for the `archive.pack` ring (#250), with the same rule as its
/// three twins: what counts for nothing falls first. Here "counts for
/// nothing" is an archive whose names all travel intact, and what is
/// protected is the report that says some did not.
fn evict_pack_reports(ring: &mut std::collections::VecDeque<PackReportEntry>) {
    evict_reports(
        ring,
        PACK_REPORTS_MAX,
        PACK_REPORTS_AGENTS_MAX,
        |r: &norte_proto::methods::ArchivePackReportResult| !r.risky.is_empty() || r.truncated,
    );
}

/// Cap of the `fs.checksum` ring. Same number as its twins and with its own
/// name: tying it to `archive.pack`'s would make touching the packing cap
/// move this one with nobody asking for it.
pub(crate) const CHECKSUM_REPORTS_MAX: usize = TEST_REPORTS_MAX;

/// Per-class sub-cap of the `fs.checksum` ring.
pub(crate) const CHECKSUM_REPORTS_AGENTS_MAX: usize = TEST_REPORTS_AGENTS_MAX;

/// Eviction for the `fs.checksum` ring (#311), with the same rule as its
/// twins: what counts for nothing falls first.
///
/// Here "counts for something" is a report with some path WITH NO digest:
/// the one that says everything could be read is reconstructed by
/// requesting it again, and the one that says one could not be read is the
/// one someone is looking for. It is the inverse of its twins — there
/// "loud" is a failure, here too, but the clean report is the one carrying
/// the digests that cost hours of I/O — and it is kept because the missing
/// one is recomputed and the reason for the failure is not remembered on
/// its own.
fn evict_checksum_reports(ring: &mut std::collections::VecDeque<ChecksumReportEntry>) {
    evict_reports(
        ring,
        CHECKSUM_REPORTS_MAX,
        CHECKSUM_REPORTS_AGENTS_MAX,
        |r: &norte_proto::methods::FsChecksumReportResult| {
            r.entries.iter().any(|e| e.digest.is_none())
        },
    );
}

/// Cap of the `fs.dir_usage` ring. Same number as its twins and with its own
/// name, for the same reason as `fs.checksum`'s: tying it to the one next
/// door would make touching that one move this one with nobody asking for
/// it.
pub(crate) const DIR_USAGE_REPORTS_MAX: usize = TEST_REPORTS_MAX;

/// Per-class sub-cap of the `fs.dir_usage` ring.
pub(crate) const DIR_USAGE_REPORTS_AGENTS_MAX: usize = TEST_REPORTS_AGENTS_MAX;

/// Eviction for the `fs.dir_usage` ring (phase 4), with the family's rule:
/// what counts for nothing falls first.
///
/// "Counts for something" is a map that is NOT the whole map: one that was
/// left unlisted (`listed` at `false`), one with children that could not be
/// fully measured (`partial`), or one whose cap ate names (`omitted`). A
/// complete map is reconstructed by measuring again; what is not remembered
/// on its own is WHY this one is incomplete.
fn evict_dir_usage_reports(ring: &mut std::collections::VecDeque<DirUsageReportEntry>) {
    evict_reports(
        ring,
        DIR_USAGE_REPORTS_MAX,
        DIR_USAGE_REPORTS_AGENTS_MAX,
        |r: &norte_proto::methods::FsDirUsageReportResult| {
            !r.listed || r.omitted > 0 || r.children.iter().any(|c| c.partial)
        },
    );
}

/// All of `dir`'s base names, in raw bytes (rule 1).
///
/// MATERIALIZES the entire listing: the planner needs to see the complete
/// directory to judge collisions, and a half listing would produce half
/// verdicts. That is why an error mid-stream PROPAGATES instead of returning
/// what was managed to be read.
pub(crate) async fn list_base_names(
    provider: &dyn Provider,
    dir: &VPath,
) -> Result<Vec<Vec<u8>>, Error> {
    use futures::StreamExt;
    let mut stream = provider.list(dir).await?;
    let mut names = Vec::with_capacity(64);
    while let Some(item) = stream.next().await {
        let entry = item?;
        if names.len() >= RENAME_BATCH_MAX_LISTING {
            tracing::warn!(
                max = RENAME_BATCH_MAX_LISTING,
                "directory above the plannable cap for a batch rename",
            );
            return Err(Error::LimitExceeded {
                limit: Error::LIMIT_ENTRIES.into(),
            });
        }
        if let Some(n) = entry.path.file_name() {
            names.push(n.as_bytes().to_vec());
        }
    }
    Ok(names)
}

/// What to evaluate in policy to undo ONE undo unit.
///
/// A `Created`'s reversal DELETES → `Delete` (one path), and since #65 it
/// ALWAYS goes to trash (or is skipped): it is gated as `Trash`, not
/// `Permanent` — a "permanent-only" deny must not stop the LIFO over a
/// reversal that never deletes permanently. `rename_back` / `restore_trash`
/// RELOCATE → `Move` with TWO endpoints (the restoration's source and
/// destination): both have to pass the gate, same as a normal Move (security
/// M2). The second endpoint is `path_to` (rename) or `reversal_ref` (trash).
///
/// For a BATCH, the endpoints of ALL its members are collected and asked
/// about ONCE PER CLASS of operation: policy resolves each slice to the most
/// restrictive verdict, so a batch that so much as touches a denied name is
/// denied whole and never halfway — the same rule [`Engine::rename_batch_as`]
/// applies on the way in.
///
/// **Per class, and not a single question with the most restrictive class.**
/// A rename batch carries the same reversal in all its entries, but a
/// SYNC one mixes: restoring what was buried is a `Move` and deleting what
/// was created is a `Delete`. Merging them into `Delete` was not
/// conservative — `delete` and `move` are INDEPENDENT permissions in `OpSet`
/// and in `policy.toml`, not one inside the other — so it let a `move`
/// policy denies through under `delete` permission; and conversely, it
/// denied the whole batch for a member whose reversal nobody had forbidden.
///
/// `None` only for an empty unit, which [`crate::undo::undo_units`] never
/// produces.
fn undo_gate_targets(unit: &[crate::journal::JournalEntry]) -> Result<Option<UndoGates>, Error> {
    let mut anchor: Option<VPath> = None;
    let mut moves: Vec<VPath> = Vec::new();
    let mut deletes: Vec<VPath> = Vec::new();
    let mut set_modes: std::collections::BTreeMap<u32, Vec<VPath>> =
        std::collections::BTreeMap::new();
    for e in unit {
        let path = wire_engine(&e.path)?;
        if anchor.is_none() {
            anchor = Some(path.clone());
        }
        match e.reversal.as_str() {
            // An `irreversible` entry has no reversal to execute
            // (`revert_entry` skips it untouched), so there is no gate to
            // open. Gating it would let a `deny` on a path this undo is NOT
            // going to touch block the whole unit and, with the strict
            // LIFO, the entire session behind it: the hijack
            // `revert_sync_batch` exists to prevent, one layer up.
            "irreversible" => {}
            "rename_back" => {
                moves.push(path);
                if let Some(to) = e.path_to.as_deref() {
                    moves.push(wire_engine(to)?);
                }
            }
            "restore_trash" => {
                moves.push(path);
                if let Some(from) = e.reversal_ref.as_deref() {
                    moves.push(wire_engine(from)?);
                }
            }
            // #314: the reversal of a permission change is ANOTHER
            // permission change, and `set-mode` is an INDEPENDENT
            // permission. It used to fall into the catch-all below, and
            // that reproduced exactly the bug the rustdoc above tells for
            // `delete`/`move`: an actor with `delete` could undo a chmod
            // policy did not grant it, and one with `set-mode` could not
            // undo its own — and with the strict LIFO, that blocks the
            // entire session behind it.
            // #314: the mode the reversal IS GOING TO SET travels in
            // `reversal_ref`, and grouping happens by it: the question
            // asked of the human says which one it is, so a unit that
            // restores two different modes has to ask twice instead of
            // showing one for both.
            "set_mode_back" => {
                let mode = e
                    .reversal_ref
                    .as_deref()
                    .and_then(|b| std::str::from_utf8(b).ok())
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(0);
                set_modes.entry(mode).or_default().push(path);
            }
            // `delete`, and any label this core does not know: the unknown
            // one never gets to act (`revert_entry` blocks it), but it is
            // asked about the same way, under the class that takes away the
            // MOST.
            _ => deletes.push(path),
        }
    }
    if anchor.is_none() {
        return Ok(None);
    }
    let mut gates: Vec<(crate::policy::PolicyOp, Vec<VPath>)> = Vec::with_capacity(3);
    for (mode, paths) in set_modes {
        gates.push((
            // Undo is NEVER recursive, whatever the original was: the
            // journal keeps one entry per NODE, so this question's paths
            // are exactly the ones about to be touched. Setting
            // `recursive: true` here would say "and everything hanging off
            // it", which is more than this undo does.
            crate::policy::PolicyOp::SetMode {
                mode,
                recursive: false,
                dir_mode: None,
            },
            paths,
        ));
    }
    if !deletes.is_empty() {
        gates.push((
            crate::policy::PolicyOp::Delete {
                mode: DeleteMode::Trash,
            },
            deletes,
        ));
    }
    if !moves.is_empty() {
        gates.push((crate::policy::PolicyOp::Move, moves));
    }
    Ok(Some(UndoGates { gates }))
}

/// Asks policy about ONE undo unit, and returns the reason if it denies it
/// (#171).
///
/// Runs INSIDE the Task: this is where the cost of parsing up to
/// `unit.len() * 2` `VPath`s is paid, which is what used to be done entirely
/// on the caller's thread, before the Task existed and with nothing able to
/// cancel it.
///
/// A journal path that fails to parse counts as a denial of ITS unit and
/// nobody else's: bringing down the entire undo for one broken row would
/// take away from the human the rest, which are fine.
async fn undo_unit_denial(
    checker: &PolicyChecker,
    actor: &crate::journal::Actor,
    unit: &[crate::journal::JournalEntry],
) -> Option<Error> {
    let targets = match undo_gate_targets(unit) {
        Ok(Some(targets)) => targets,
        Ok(None) => return None,
        Err(e) => return Some(e),
    };
    for (undo_op, paths) in &targets.gates {
        let gate_paths: Vec<&VPath> = paths.iter().collect();
        if let Err(err) = checker.check(actor, *undo_op, &gate_paths).await {
            return Some(err);
        }
    }
    None
}

/// What policy has to approve before undoing a unit, and where that unit
/// lives.
struct UndoGates {
    /// The gates, by operation class. Empty = the unit does nothing (all
    /// its entries are `irreversible`), and then there is nothing to ask.
    ///
    /// The provider no longer comes out of here: it is resolved by whoever
    /// plans, from the unit's first entry (#171). All of a unit's paths
    /// live on the same provider — `inverse_chain` checks it for a rename
    /// batch and `one_provider` for a sync one — and the first entry works
    /// even if the whole unit is `irreversible` and gates no path.
    gates: Vec<(crate::policy::PolicyOp, Vec<VPath>)>,
}

/// Leaves out hits that fall in an excluded subtree (#165). Split from
/// [`Engine::index_query_as`] to be testable with no index and no
/// environment: the real exclusions are resolved by the process's
/// [`crate::policy::walk_exclusions`].
fn drop_excluded(
    hits: Vec<norte_index::IndexHit>,
    excluded: &[VPath],
) -> Vec<norte_index::IndexHit> {
    if excluded.is_empty() {
        return hits;
    }
    hits.into_iter()
        .filter(|h| !excluded.iter().any(|x| crate::policy::is_under(x, &h.path)))
        .collect()
}

/// Reconstructs a `VPath` from the journal's `to_wire` bytes (undo M3-2).
fn wire_engine(bytes: &[u8]) -> Result<VPath, Error> {
    let s = std::str::from_utf8(bytes).map_err(|_| Error::InvalidPath)?;
    VPath::parse(s).map_err(|_| Error::InvalidPath)
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod batch_report_ring_tests {
    use super::*;
    use crate::journal::Actor;
    use crate::rename::{BatchReport, StuckStep};

    fn entry(id: u64, owner: Actor, loud: bool) -> BatchReportEntry {
        let mut r = BatchReport::default();
        if loud {
            r.stuck = Some(StuckStep {
                from: VPath::parse("mem:///a").expect("path"),
                to: VPath::parse("mem:///b").expect("path"),
                pair_index: 0,
                error: Error::Io { retryable: false },
                journalled: true,
                still_applied: 1,
            });
        }
        (TaskId::new(id), owner, Arc::new(std::sync::Mutex::new(r)))
    }

    fn agent() -> Actor {
        Actor::Agent {
            session: "s1".into(),
        }
    }

    fn ids(ring: &std::collections::VecDeque<BatchReportEntry>) -> Vec<u64> {
        ring.iter().map(|e| e.0.get()).collect()
    }

    /// A report that NAMES a stuck step survives one that says everything
    /// went fine, even if the latter is older: the clean one can be
    /// reconstructed by looking at the directory and the other cannot.
    #[test]
    fn eviction_sacrifices_the_report_that_counts_for_nothing_first() {
        let mut ring: std::collections::VecDeque<BatchReportEntry> = (0..BATCH_REPORTS_MAX as u64)
            .map(|i| entry(i, Actor::User, i == 0))
            .collect();
        ring.push_back(entry(999, Actor::User, false));
        evict_batch_reports(&mut ring);
        assert_eq!(ring.len(), BATCH_REPORTS_MAX);
        assert!(ids(&ring).contains(&0), "the stuck one stays");
        assert!(!ids(&ring).contains(&1), "the older clean one leaves");
    }

    /// Per-class sub-cap: an AGENT's batches do not evict the report the
    /// human has not read yet — not even if the agent sends many more.
    #[test]
    fn an_agents_batches_do_not_evict_the_humans_report() {
        let mut ring: std::collections::VecDeque<BatchReportEntry> =
            std::collections::VecDeque::new();
        ring.push_back(entry(1, Actor::User, true));
        for i in 0..(BATCH_REPORTS_MAX as u64 * 2) {
            ring.push_back(entry(100 + i, agent(), false));
            evict_batch_reports(&mut ring);
        }
        assert!(ids(&ring).contains(&1), "the human's report is still there");
        assert!(
            ring.iter().filter(|e| !matches!(e.1, Actor::User)).count() <= BATCH_REPORTS_AGENTS_MAX,
            "the agents' sub-cap is respected",
        );
    }

    /// If EVERYTHING retained is loud, the oldest is thrown out anyway: a
    /// daemon's memory over months cannot grow even with the bad batches.
    #[test]
    fn with_everything_loud_the_ring_stays_bounded() {
        let mut ring: std::collections::VecDeque<BatchReportEntry> =
            (0..(BATCH_REPORTS_MAX as u64 + 5))
                .map(|i| entry(i, Actor::User, true))
                .collect();
        evict_batch_reports(&mut ring);
        assert_eq!(ring.len(), BATCH_REPORTS_MAX);
        assert!(!ids(&ring).contains(&0), "the oldest one left");
    }
}

/// The DESTINATION trash of a retained plan, from the same pair of options
/// the [`DestTrash`](norte_proto::methods::DestTrash) shown to whoever
/// approved it came from (#170).
///
/// A function and not a line inside `sync_apply_opened` because there it is
/// needed twice: the report carries it — so "can this batch be returned?"
/// can be answered with the report in hand and without having kept
/// `sync.plan_done` — and the delete gate asks about the delete class that
/// is actually going to happen. The two have to come from the SAME place or
/// the report could end up naming one trash and the gate asking about
/// another.
fn plan_dest_trash(reader: &crate::sync::SpoolReader) -> norte_proto::methods::DestTrash {
    norte_proto::methods::DestTrash::of(
        reader.header().options.dest_has_trash,
        reader.header().options.dest_trash_restorable,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(wire: &str) -> norte_index::IndexHit {
        norte_index::IndexHit {
            path: VPath::parse(wire).expect("wire"),
            kind: norte_proto::EntryKind::File,
            size: None,
            mtime_ms: None,
        }
    }

    /// #165: the index is built by the human and can contain the daemon's
    /// state directory; an agent that queries it does not get it.
    #[test]
    fn hits_from_a_protected_subtree_do_not_come_out() {
        let hits = vec![
            hit("file:///home/u/docs/carta.txt"),
            hit("file:///home/u/.config/norte/journal.db"),
            hit("file:///home/u/.config/norte"),
            hit("file:///home/u/.config/norte-backup/journal.db"),
        ];
        let excluded = vec![VPath::parse("file:///home/u/.config/norte").expect("wire")];
        let remaining: Vec<String> = drop_excluded(hits.clone(), &excluded)
            .into_iter()
            .map(|h| h.path.to_wire())
            .collect();
        assert_eq!(
            remaining,
            vec![
                "file:///home/u/docs/carta.txt".to_owned(),
                "file:///home/u/.config/norte-backup/journal.db".to_owned(),
            ]
        );
        // With no exclusions (the human) not one falls out.
        assert_eq!(drop_excluded(hits, &[]).len(), 4);
    }

    /// #166: an `Engine` with no `with_policy` gates with `AllowAll`, and
    /// that is not distinguishable from a policy that is permissive on
    /// purpose. The daemon warns at startup, and to warn it needs to be able
    /// to ASK.
    #[test]
    fn a_default_policy_is_distinguished_from_an_installed_one() {
        let without_policy = Engine::new();
        assert!(
            !without_policy.has_explicit_policy(),
            "a freshly made Engine has no policy installed"
        );

        let with_policy = Engine::new().with_policy(
            Arc::new(crate::policy::AllowAll),
            Arc::new(crate::approval::DenyAll),
        );
        assert!(
            with_policy.has_explicit_policy(),
            "AllowAll installed ON PURPOSE does count as a policy"
        );
    }
}
