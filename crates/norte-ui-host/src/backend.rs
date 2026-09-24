//! What the host asks of the world, in the SMALLEST form that serves it.
//!
//! This is not a second facade over the SDK: it is the short list of things
//! the controller needs, and it exists for one concrete reason — so its
//! tests can be deterministic without a daemon. Everything else is asked
//! directly of [`norte_client::RemoteBackend`].

use std::sync::Arc;

use futures::future::BoxFuture;
use norte_client::{ConnEvent, EntryStream};
use norte_proto::{
    AttrCatalog, Capabilities, CollisionPolicy, DeleteMode, Entry, Error, TaskId, TaskProgress,
    VPath, methods,
};
use tokio::sync::watch;

/// A Task in flight, in the minimal form the host needs: its id, its
/// progress and how to ask it to stop.
///
/// Deliberately not the SDK's `RemoteTask`. The host only needs these three
/// things, and asking for them this way is what lets a test fabricate them
/// without a daemon — which is where the rules that actually matter get
/// checked: that a terminal state is never lost and that cancelling is
/// idempotent.
pub struct HostTask {
    /// Id of the task in the daemon.
    pub id: TaskId,
    /// Live snapshots of the progress.
    pub progress: watch::Receiver<TaskProgress>,
    /// Requests cooperative cancellation. Calling it twice is not an error:
    /// cancelling is idempotent by contract.
    pub cancel: Arc<dyn Fn() + Send + Sync>,
    /// Pauses (`true`) or resumes (`false`) the task (ADR 0147), or `None` if
    /// this task cannot be paused from here. Returns `Unsupported` against a
    /// daemon that does not know how to pause, so the window can say so.
    pub pause: Option<Pausa>,
    /// Raises (`true`) or lowers the task in the serial queue (ADR 0149), or
    /// `None` if it cannot be done from here.
    pub cola: Option<Pausa>,
    /// It was launched by ANOTHER client of the same session. It is painted
    /// the same and can be cancelled the same — it is the same session — but
    /// the board says so: an operation nobody here asked for, and
    /// indistinguishable from one's own, is a surprise.
    pub foreign: bool,
}

/// How to pause or resume a [`HostTask`] (ADR 0147).
pub type Pausa = Arc<dyn Fn(bool) -> BoxFuture<'static, Result<(), Error>> + Send + Sync>;

/// The pause handle of a daemon task, over the SDK's handle.
fn remote_queue_move(c: norte_client::RemoteTaskCanceller) -> Pausa {
    Arc::new(move |up| {
        let c = c.clone();
        Box::pin(async move { c.mover_en_cola(up).await })
    })
}

fn remote_pause(c: norte_client::RemoteTaskCanceller) -> Pausa {
    Arc::new(move |pause| {
        let c = c.clone();
        Box::pin(async move { c.set_paused(pause).await })
    })
}

impl std::fmt::Debug for HostTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // By hand because a function is not `Debug`, and `finish_non_exhaustive`
        // SAYS so instead of implying that the task is just two fields.
        f.debug_struct("HostTask")
            .field("id", &self.id)
            .field("progress", &self.progress.borrow().state)
            .finish_non_exhaustive()
    }
}

/// What the controller needs to know how to ask for.
///
/// Object-safe on purpose (boxed futures): the host keeps an
/// `Arc<dyn HostBackend>` and a test drops its own in without generics that
/// propagate through the whole API.
pub trait HostBackend: Send + Sync + 'static {
    /// A directory's listing, as a STREAM.
    ///
    /// Paginated, not complete: a directory of half a million entries cannot
    /// travel whole before the first row is painted. The host takes the
    /// first page, paints it, and keeps draining the rest behind the scenes
    /// ([`crate::controller`] extends it with `PaneState::extend`, the same
    /// path the TUI takes).
    ///
    /// `attrs` are the attribute ids the configured columns ask for: a
    /// provider only sends what it is asked for, so asking for less leaves
    /// a column blank forever.
    /// Lists a directory, and says HOW MANY entries it skipped.
    ///
    /// The count travels with the listing and not separately because it
    /// describes THAT listing: a provider that skips entries — without
    /// permission to stat them, past a cap of its own — returns fewer rows
    /// than there are, and without saying so the screen lies by omission.
    /// `None` = the provider does not keep count, which is NOT the same as
    /// zero.
    fn list(
        &self,
        dir: VPath,
        attrs: Vec<String>,
    ) -> BoxFuture<'static, Result<(EntryStream, Option<u64>), Error>>;

    /// The capabilities of ONE LOCATION (#215): the mount answers them, not
    /// the provider, so a FAT thumb drive mounted under a case-sensitive
    /// `/home` does not inherit `/home`'s answer.
    ///
    /// What the window does with them is fold names the way the destination
    /// would fold them (#268): two marks that on an ext4 are `README.txt`
    /// and `readme.txt` are ONE name on NTFS or APFS, and queuing both lets
    /// one win non-deterministically while the other fails without
    /// explanation.
    fn capabilities(&self, path: VPath) -> BoxFuture<'static, Result<Capabilities, Error>>;

    /// Creates ONE directory. Returns the Task already queued.
    fn mkdir(&self, path: VPath) -> BoxFuture<'static, Result<HostTask, Error>>;

    /// Creates an EMPTY file, as a Task (#290).
    ///
    /// Fails if the destination exists: creating is an assertion about a
    /// free name, and a method that silently truncates is data loss with an
    /// innocent name.
    fn create_file(&self, path: VPath) -> BoxFuture<'static, Result<HostTask, Error>>;

    /// Computes the sha256 of a batch's CONTENT, as a Task (#311).
    ///
    /// The digests do NOT come back here: they do not fit in a Task's
    /// outcome nor in its progress. They are collected with
    /// [`Self::checksum_report`] when it finishes.
    fn checksum(
        &self,
        params: norte_proto::methods::FsChecksumParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>>;

    /// The digests that Task computed (#311).
    ///
    /// Only DEFINITIVE with the Task `Completed` and `pending == 0`: one
    /// from a cancelled Task is half-done, and comparing it against a
    /// checksum file would accuse files nobody ever got to read.
    fn checksum_report(
        &self,
        task: norte_proto::TaskId,
    ) -> BoxFuture<'static, Result<norte_proto::methods::FsChecksumReportResult, Error>>;

    /// What a directory is made of, child by child, as a Task (phase 4).
    ///
    /// The children do NOT come back here: a list does not fit in a Task's
    /// outcome nor in its progress. They are collected with
    /// [`Self::dir_usage_report`].
    fn dir_usage(
        &self,
        params: norte_proto::methods::FsDirUsageParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>>;

    /// The map that Task has measured so far (phase 4).
    ///
    /// It is a SNAPSHOT: partial while it runs — which is what makes asking
    /// for it useful, because a map gets painted as it goes — and
    /// definitive once the Task is terminal. Whoever lands it has to look at
    /// the state: one from a cancelled Task is half-done, and painting it as
    /// complete turns a huge directory into a small one.
    fn dir_usage_report(
        &self,
        task: norte_proto::TaskId,
    ) -> BoxFuture<'static, Result<norte_proto::methods::FsDirUsageReportResult, Error>>;

    /// Changes the POSIX PERMISSIONS of a batch, as a Task (#314).
    ///
    /// Mutates: the core records it in the journal with its reverse — the
    /// previous mode — and passes it through policy. A location without
    /// POSIX permissions responds `Unsupported` without changing anything.
    fn set_mode(
        &self,
        params: norte_proto::methods::FsSetModeParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>>;

    /// The data of ONE entry.
    ///
    /// A listing can come LAZY — the local provider returns `size` and
    /// `mtime` as `None` and whoever needs them fills them in (#52) — so
    /// without this, the size and date columns stay blank forever over
    /// `file://`, which is the default view. The TUI already probes its
    /// visible window; this is the same path for the host.
    fn stat(&self, path: VPath, attrs: Vec<String>) -> BoxFuture<'static, Result<Entry, Error>>;

    /// Reads a CHUNK of a file.
    ///
    /// Always bounded: the viewer shows a header, not the whole file (the
    /// rest is never read), and whoever calls it decides the budget.
    fn read(
        &self,
        path: VPath,
        range: Option<norte_proto::ByteRange>,
    ) -> BoxFuture<'static, Result<Vec<u8>, Error>>;

    /// The attribute catalog of a location.
    ///
    /// Without it, an `attr:` column does not know whether what it carries
    /// is a size, a date or a mode, and it gets painted as the raw number it
    /// is: the catalog is what turns `33188` into `-rw-r--r--`.
    fn attr_catalog(&self, dir: VPath) -> BoxFuture<'static, Result<AttrCatalog, Error>>;

    /// The channel of pending policy approvals: every agent op under an
    /// `ask` rule that the daemon broadcasts, and that waits for a human
    /// answer.
    fn take_approvals(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<methods::PolicyApprovalRequired>>;

    /// Answers an approval. `approve = false` denies it.
    fn policy_decide(
        &self,
        approval_id: u64,
        approve: bool,
    ) -> BoxFuture<'static, Result<(), Error>>;

    /// The UI session, and whether THIS connection owns it (ADR 0059).
    ///
    /// The core keeps it and versions it but does not read it: the document
    /// belongs to the frontends, which is why it travels as opaque JSON.
    fn session_get(&self) -> BoxFuture<'static, Result<(methods::Session, bool), Error>>;

    /// The DAEMON's log from `cursor`, at most `max` lines (#328).
    ///
    /// `cursor: None` asks for "whatever there is", which is what a panel
    /// sends when it opens, and is NOT the same as `Some(0)`: against a ring
    /// that has already wrapped, a zero would report a false `lost` on the
    /// first poll.
    ///
    /// # Errors
    /// [`Error::Unsupported`] when the other end has no log to serve. The
    /// reachable case is not an OLDER daemon — a 0.65 client never completes
    /// `initialize` against a 0.64 one — but one of the same version built
    /// without the `logging` feature. There is no version comparison
    /// anywhere: the response to the method is the only signal.
    fn log_tail(
        &self,
        cursor: Option<u64>,
        max: u32,
    ) -> BoxFuture<'static, Result<methods::LogTailResult, Error>>;

    /// Raises the level the daemon's ring keeps, and returns the one that
    /// actually ended up set (#328).
    ///
    /// The level is GLOBAL to the daemon and only EVER RISES: asking for
    /// less verbosity is not an error and lowers nothing, it answers with
    /// whatever was already set. That is why the daemon applies it and not
    /// the client — the cap that keeps a password from showing up in there
    /// lives in the process that holds the ring.
    ///
    /// # Errors
    /// [`Error::Unsupported`] same as [`Self::log_tail`]; a level outside
    /// the vocabulary is `InvalidParams`, which is a different question.
    fn log_level(&self, level: String) -> BoxFuture<'static, Result<String, Error>>;

    /// Writes the session over the revision that was read. Returns the new
    /// one.
    ///
    /// A `Conflict` means another window wrote in between: it is re-read,
    /// never overwritten.
    fn session_put(
        &self,
        version: u32,
        revision: u64,
        body: serde_json::Value,
    ) -> BoxFuture<'static, Result<u64, Error>>;

    /// Releases ownership of the session (phase 9): returns whether it WAS
    /// the owner.
    ///
    /// `false` is not an error but a fact — "it wasn't you" — and whoever is
    /// taking over needs it: without it, the terminal would launch to
    /// reclaim a session that is still busy, and the reader would be left
    /// staring at a listing that is not theirs.
    fn session_release(&self) -> BoxFuture<'static, Result<bool, Error>>;

    /// The connection events channel (lost and restored), if this
    /// connection has one and nobody has taken it yet.
    fn take_conn_events(&self) -> Option<tokio::sync::mpsc::UnboundedReceiver<ConnEvent>>;

    /// The channel of FOREIGN tasks: the ones another client of the same
    /// session launched and this one observes.
    fn take_foreign_tasks(&self) -> Option<tokio::sync::mpsc::UnboundedReceiver<HostTask>>;

    /// The `connection.degraded` notice channel (#44): a provider session
    /// that travels UNENCRYPTED.
    ///
    /// It is not about the daemon — that is [`Self::take_conn_events`] — but
    /// about the connection a provider opened underneath, and it is a
    /// SECURITY fact: until it is said, a plaintext FTP listing reads the
    /// same as an SFTP one.
    fn take_degraded(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<methods::ConnectionDegraded>>;

    /// The `connection.failed` notice channel (#322): WHY a connection
    /// could NOT be opened.
    ///
    /// Separate from [`Self::take_degraded`] because they are two different
    /// facts — a session that opened but travels badly, and one that never
    /// got to open — and mixing them makes one get painted as the other.
    /// Without this, the failure arrives as the error's CATEGORY (almost
    /// always `PermissionDenied`), which does not distinguish an empty
    /// secret from a wrong key.
    fn take_failed(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<methods::ConnectionFailed>>;

    /// The `plugin.notice` notice channel (0.69.0, ADR 0100): what a plugin
    /// `hook` wanted to tell the human about a mutation the journal already
    /// recorded, or that the daemon turned off a plugin's hooks.
    ///
    /// Separate from the two above because it speaks of something else:
    /// neither a session nor a connection, but a file that already changed.
    /// It is an ephemeral notice attributed to a third party, never a
    /// banner.
    fn take_plugin_notices(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<methods::PluginNotice>>;

    /// Deletes ONE entry: to the trash or permanently. Returns the Task
    /// already queued — the outcome arrives through its progress, not
    /// through this call.
    ///
    /// One per entry and not a batch because the wire method is that way; a
    /// deletion of several marks is several Tasks, and the board shows them
    /// all.
    fn delete(&self, path: VPath, mode: DeleteMode) -> BoxFuture<'static, Result<HostTask, Error>>;

    /// Copies ONE entry to an EXACT destination. Returns the Task already
    /// queued.
    ///
    /// `to` is the final path, not the directory: whoever calls it already
    /// composed the name. The core only invents a free name with
    /// [`norte_proto::CollisionPolicy::RenameAuto`], and with every other
    /// policy it never does — so a `to` that is a directory would copy
    /// INSIDE it without saying so, and that is not what this method
    /// promises.
    ///
    /// One per entry and not a batch, for the same reason as
    /// [`Self::delete`]: the wire method is that way, and a batch of marks
    /// is several Tasks that the board shows all of.
    fn copy(
        &self,
        from: VPath,
        to: VPath,
        on_collision: CollisionPolicy,
        queued: bool,
    ) -> BoxFuture<'static, Result<HostTask, Error>>;

    /// Packs `sources` inside a new container, as a Task (#132).
    ///
    /// The FORMAT travels explicit and comes from the name that was typed:
    /// packing into one the user did not ask for is worse than refusing, so
    /// whoever calls it resolves the name FIRST and a name without a known
    /// extension never reaches here.
    fn pack(
        &self,
        params: methods::ArchivePackParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>>;

    /// Checks a container, as a Task (#132).
    ///
    /// Mutates nothing: it reads the whole archive and answers whether it is
    /// sound. Its result, like a count's, travels in the terminal progress.
    fn test_archive(
        &self,
        params: methods::ArchiveTestParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>>;

    /// The NAMED connections the daemon has configured (#264).
    ///
    /// Asked for instead of reading `connections.toml`: reading it would
    /// drag the whole network stack into a binary that only wants to paint
    /// names, and the daemon already has it because it is the one that opens
    /// the sessions.
    ///
    /// Does not connect. Returns where it COULD go; going is navigating to
    /// that URL.
    ///
    /// The WHOLE result, including the entries the daemon could not read
    /// (#365): without them, the picker cannot say why a connection the
    /// reader knows they wrote is missing.
    fn connections(&self) -> BoxFuture<'static, Result<methods::ConnectionListResult, Error>>;

    /// Closes the SESSION of a connection, named by any of its paths (#140).
    ///
    /// The core keeps it cached by `scheme://authority`, so whoever calls it
    /// sends the place where the pane is and does not need to know how a
    /// session is keyed internally.
    ///
    /// `false` = there was none open. Not a failure, and saying "closed"
    /// when nothing was closed teaches you not to trust the message.
    fn close_connection(&self, path: VPath) -> BoxFuture<'static, Result<bool, Error>>;

    /// Delivers the secret a connection asked for (#325/#327).
    ///
    /// `conn` is the name from `connections.toml` that came in the
    /// `Error::SecretNeeded`, not something the remote server said.
    ///
    /// `secret` travels in the clear because the core needs it in the clear
    /// to authenticate; what this frontend can promise is that its copy is
    /// overwritten with zeros when dropped
    /// (`norte_frontend::secret::TypedSecret`) and that it never reaches the
    /// painting layer. ADR 0015 covers the copies beyond this point — the
    /// params, the frame, the daemon's `Value`.
    ///
    /// A core that REFUSES to store it arrives as an error and not as `Ok`:
    /// the SDK already translates that `stored: false`. Treating it as
    /// success would leave the user retrying a navigation that will never
    /// get the secret.
    fn provide_secret(&self, conn: String, secret: String)
    -> BoxFuture<'static, Result<(), Error>>;

    /// Splits a file into `part_bytes` chunks, as a Task (#132).
    fn split_file(
        &self,
        params: methods::FileSplitParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>>;

    /// Joins the chunks starting from the FIRST one, as a Task (#132).
    ///
    /// Only from the `.001`: the core searches forward, so starting from
    /// another one would join half a thing. Whoever calls it already
    /// checked this.
    fn combine_files(
        &self,
        params: methods::FileCombineParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>>;

    /// Counts what `paths` take up — bytes and entries — as a Task (#139).
    ///
    /// One of the few Tasks whose RESULT **is** its progress: it publishes
    /// nothing, mutates nothing, and what whoever launched it wants to know
    /// travels in the terminal progress. That is why it returns the Task and
    /// not a total.
    ///
    /// A real batch and not one Task per path, unlike [`Self::delete`] and
    /// [`Self::copy`]: the wire method takes a list, and counting two trees
    /// separately would force whoever asks to add them up — and add up the
    /// skipped ones too, which do not add the same way.
    fn dir_size(&self, paths: Vec<VPath>) -> BoxFuture<'static, Result<HostTask, Error>>;

    /// Asks the model for a rename plan for a DIRECTORY.
    ///
    /// Mutates NOTHING: what comes back is a proposal that has to be
    /// reviewed, checked against the core and approved. A DIRECT response
    /// and not a Task (ADR 0042): abandoning the wait cuts the dispatch off
    /// in the daemon.
    ///
    /// What comes back is from a MODEL, i.e. the least trustworthy thing in
    /// the whole system: whoever calls it validates it whole before showing
    /// it (`norte_frontend::validate_ai_plan`), and a single invalid pair
    /// brings down the whole batch — a tampered plan is never applied
    /// "as far as it's good for".
    ///
    /// `names` are the MARKED basenames (#121). Empty = the whole directory:
    /// asking for a plan over five files cannot send the provider the
    /// thousand others in the directory.
    fn ai_rename_plan(
        &self,
        dir: VPath,
        instruction: String,
        names: Vec<String>,
    ) -> BoxFuture<'static, Result<methods::AiRenamePlanResult, Error>>;

    /// The plan a `renamer` plugin PROPOSES (C3, ADR 0095): the same result
    /// as [`Self::ai_rename_plan`], from a different producer, and with the
    /// same discipline on the way back — validated whole before showing it.
    fn plugin_rename_plan(
        &self,
        plugin_id: String,
        renamer_id: String,
        dir: VPath,
        names: Vec<String>,
    ) -> BoxFuture<'static, Result<methods::AiRenamePlanResult, Error>>;

    /// The ORGANIZE plan a model proposes (phase 8): the same deal as
    /// renaming with one more freedom — the destination can carry
    /// folders — which is why its token travels WITH the plan: there is no
    /// second trip to check.
    fn ai_organize_plan(
        &self,
        dir: VPath,
        instruction: String,
        names: Vec<String>,
    ) -> BoxFuture<'static, Result<methods::AiOrganizePlanResult, Error>>;

    /// The same plan, proposed by a plugin of kind `organizer` (phase 8).
    /// Same split as the `renamer`: the plugin proposes and the core
    /// executes.
    ///
    /// **`names` is the operand, and empty means empty**, not "everything":
    /// a plugin does not list directories (rule 9), so whatever is not
    /// given to it does not exist for it, and it answers that it moves
    /// nothing.
    fn plugin_organize_plan(
        &self,
        plugin_id: String,
        organizer_id: String,
        dir: VPath,
        names: Vec<String>,
    ) -> BoxFuture<'static, Result<methods::AiOrganizePlanResult, Error>>;

    /// Applies an already-reviewed organize plan (phase 8): creates the
    /// missing folders and moves, ALL under a single `batch_id`, so it is
    /// undone as one unit.
    fn organize(
        &self,
        dir: VPath,
        moves: Vec<methods::OrganizeMove>,
        plan_hash: methods::PlanHash,
    ) -> BoxFuture<'static, Result<HostTask, Error>>;

    /// The REVIEWABLE plan for a batch of renames inside `dir`.
    ///
    /// Also mutates nothing: what is sent is INTENT — pairs of base
    /// names — and what comes back is the core's verdict (whether it is
    /// applicable, why not, how many steps are machinery) plus the
    /// `plan_hash` that has to be returned to execute EXACTLY what was
    /// shown.
    fn rename_batch_plan(
        &self,
        dir: VPath,
        pairs: Vec<methods::RenamePair>,
    ) -> BoxFuture<'static, Result<methods::FsRenameBatchPlanResult, Error>>;

    /// Executes the batch: ONE Task for all the pairs, a single undo.
    ///
    /// The SAME intent that produced the `plan_hash` is sent; the ORDER of
    /// the steps and the temporaries that break a cycle are decided by the
    /// core and never cross the wire.
    fn rename_batch(
        &self,
        dir: VPath,
        pairs: Vec<methods::RenamePair>,
        plan_hash: methods::PlanHash,
    ) -> BoxFuture<'static, Result<HostTask, Error>>;

    /// The report of an already-finished batch.
    ///
    /// The controller asks for it as soon as a `rename-batch`-class task
    /// reaches a terminal state, and shows it in the board row (and up
    /// front, if the batch left something half-done).
    ///
    /// It is the ONLY signal that a batch left the directory half-done, so
    /// it does not degrade silently: a daemon that does not know the method
    /// answers [`Error::Unsupported`], which whoever calls it distinguishes
    /// from a real failure.
    fn rename_batch_report(
        &self,
        task_id: TaskId,
    ) -> BoxFuture<'static, Result<methods::FsRenameBatchReportResult, Error>>;

    /// The report of an already-finished UNDO Task (`policy.undo_report`).
    ///
    /// Same role as [`Self::rename_batch_report`] and for the same reason:
    /// the Task's outcome says whether the undo ran, and what did NOT come
    /// back — an irreversible entry, a lock midway through the LIFO, a unit
    /// policy denied — is told only by the report.
    fn undo_report(
        &self,
        task_id: TaskId,
    ) -> BoxFuture<'static, Result<methods::PolicyUndoReportResult, Error>>;

    /// The report of an `archive.pack` (#250): what that packing kept that
    /// does not survive leaving here.
    ///
    /// The third of the same family, and the one that takes its reasoning
    /// furthest: the other two report what went WRONG, and this one reports
    /// something that went RIGHT and still has to be said — an `a\b.txt`
    /// stored, which in 7-Zip and in Explorer is a `b.txt` inside an `a`
    /// folder.
    fn archive_pack_report(
        &self,
        task_id: TaskId,
    ) -> BoxFuture<'static, Result<methods::ArchivePackReportResult, Error>>;

    /// Undoes everything an AGENT session did, whole, in reverse order.
    ///
    /// The session is an OPAQUE key: it comes from the daemon (in the
    /// approval request the agent triggered) and comes back as is. It is
    /// neither composed nor trimmed — it is painted masked, but what
    /// travels is what arrived.
    ///
    /// Returns a Task: it is a long operation with its own report
    /// (`undo_report`), and what did not come back — irreversible, denied, a
    /// LIFO that stopped partway — is said there, not in the Task's outcome.
    fn undo_session(&self, session: String) -> BoxFuture<'static, Result<HostTask, Error>>;

    /// A page of the journal going backward (`journal.list`, phase 7), for
    /// the timeline (#359). `before_seq` is the cursor of the previous page;
    /// `None` asks for the newest one.
    ///
    /// A daemon without a journal — or that does not know the method —
    /// answers `Unsupported`, and that IS said: an empty panel reads as "you
    /// have done nothing".
    fn journal_list(
        &self,
        before_seq: Option<i64>,
        limit: u32,
    ) -> BoxFuture<'static, Result<methods::JournalListResult, Error>>;

    /// Undoes the HUMAN's work after `seq` (`journal.undo_after`, phase 7).
    /// The flagged entry stays.
    ///
    /// The same undo as [`Self::undo_session`] with a different selection
    /// criterion: a Task, with its progress, its cancellation and its
    /// report.
    ///
    /// `upto_seq` is the ceiling (0.80.0): the newest thing the human saw
    /// counted. Nothing above it is undone.
    fn undo_after(
        &self,
        seq: i64,
        upto_seq: Option<i64>,
    ) -> BoxFuture<'static, Result<HostTask, Error>>;

    /// Moves ONE entry to an EXACT destination. Same rules as
    /// [`Self::copy`].
    ///
    /// A separate method and not a `bool` because they are two different
    /// verbs on the wire (`fs.copy` and `fs.move`), two different
    /// `TaskKind`s on the board, and two different journal entries. A
    /// parameter that picks between the two is a place where a copy turns
    /// into a move.
    fn move_(
        &self,
        from: VPath,
        to: VPath,
        on_collision: CollisionPolicy,
        queued: bool,
    ) -> BoxFuture<'static, Result<HostTask, Error>>;

    /// The catalog of discovered plugins, with their approved/enabled
    /// state.
    ///
    /// HELP asks for it, to know which extensions have a page and which are
    /// turned on. A failure here is not a help failure: it paints without
    /// extension pages, because the documentation is cosmetic and never
    /// brings anything down.
    fn plugin_list(&self) -> BoxFuture<'static, Result<methods::PluginListResult, Error>>;

    /// A plugin's `help.md`, on demand.
    ///
    /// `id` is a SEARCH KEY against the catalog, never a piece of a path:
    /// whoever sends it must have validated it
    /// ([`norte_proto::methods::is_valid_plugin_id`]), and the daemon
    /// resolves it against what it discovered.
    ///
    /// The markdown that comes back is NOT masked: it is third-party text
    /// and it is PARSED before being painted (`norte_help::parse_untrusted`),
    /// never dumped raw.
    fn plugin_help(
        &self,
        id: String,
    ) -> BoxFuture<'static, Result<methods::PluginHelpResult, Error>>;

    /// A plugin's `[config]` schema with its EFFECTIVE values.
    ///
    /// Both things in one trip because the wire sends them together on
    /// purpose (ADR 0037): painting settings needs the type and the value,
    /// and asking for them separately is a second round trip for nothing.
    ///
    /// An unknown id answers with ZERO keys, never an error: the same
    /// lenient criterion as `plugin.list` with an empty catalog.
    fn plugin_config(
        &self,
        id: String,
    ) -> BoxFuture<'static, Result<methods::PluginGetConfigResult, Error>>;

    /// Approves or REVOKES a plugin's capabilities.
    ///
    /// It is THE security decision of the extension system: what separates
    /// "this code is on your disk" from "this code can read your files".
    /// Whoever calls it must have asked a human — this door does not ask —
    /// and the core is the one that persists it.
    ///
    /// Revoking is not the same as turning off: turning off leaves the
    /// capabilities approved for next time, revoking withdraws them.
    /// `expected_digest` is the anchor the window SHOWED (#282): the core
    /// refuses if it no longer matches, so that what is granted is what was
    /// read.
    fn plugin_set_approval(
        &self,
        id: String,
        approved: bool,
        expected_digest: Option<String>,
    ) -> BoxFuture<'static, Result<(), Error>>;

    /// Turns an ALREADY approved plugin on or off.
    fn plugin_set_enabled(
        &self,
        id: String,
        enabled: bool,
    ) -> BoxFuture<'static, Result<(), Error>>;

    /// Uninstalls a plugin (ADR 0104): deletes its files and withdraws its
    /// consent. Returns whether it had any. Whoever calls it must have asked
    /// a human — this door does not ask, and there is no going back.
    fn plugin_uninstall(&self, id: String) -> BoxFuture<'static, Result<bool, Error>>;

    /// Sets ONE `[config.<key>]` key of a plugin.
    ///
    /// `value` ALWAYS travels as a String, in the wire's canonical encoding
    /// (`bool` → `"true"`/`"false"`, `int` → decimal). The daemon validates
    /// it against the SCHEMA before persisting it: the validation on this
    /// side is to avoid sending what is already known to be bad, never to
    /// grant what it allows.
    fn plugin_set_config(
        &self,
        id: String,
        key: String,
        value: String,
    ) -> BoxFuture<'static, Result<(), Error>>;

    /// Runs ONE plugin command and returns its output.
    ///
    /// Authorization belongs to the SERVER: `plugin.run_command` resolves
    /// the command against the catalog and requires approved + enabled on
    /// its own account. What a check on this side buys is coherence with
    /// what the reader is looking at, never the permission.
    ///
    /// The output is THIRD-PARTY text: it is masked and bounded before being
    /// painted, like anything else a plugin writes.
    fn plugin_run_command(
        &self,
        id: String,
        command: String,
        arg: String,
    ) -> BoxFuture<'static, Result<String, Error>>;

    /// The styled PREVIEW from the first `previewer` plugin that applies.
    ///
    /// `None` = none applied, which is not an error: the viewer then falls
    /// back to reading the bytes itself. A broken previewer is not one
    /// either — a plugin cannot leave a file unable to be looked at.
    ///
    /// Returns LINES OF SPANS, not HTML or bytes: the plugin describes and
    /// the host paints (ADR 0037). Each span's `role` comes from
    /// `norte-theme`'s CLOSED vocabulary, so a plugin does not choose its
    /// color, and the text is its own, i.e. NOT trustworthy: it is masked
    /// before being painted.
    fn plugin_preview_styled(
        &self,
        path: VPath,
        columns: Option<u32>,
    ) -> BoxFuture<'static, Result<Option<methods::PluginPreviewStyled>, Error>>;

    /// A file's THUMBNAIL from a plugin (ADR 0107): an already-verified
    /// image from the plugin-host, or `None` if no consented plugin matches
    /// or the one that matches did not know how. Cosmetic and fail-soft like
    /// the preview: without a thumbnail, the viewer keeps what it had.
    fn plugin_thumbnail(
        &self,
        path: VPath,
        max_edge: u32,
    ) -> BoxFuture<'static, Result<Option<methods::PluginThumbnail>, Error>>;

    /// The DECORATIONS plugins put over a batch of paths.
    ///
    /// Cosmetic and fail-soft by contract: without consented decorators,
    /// with the catalog down or with the RPC broken, the answer is "none"
    /// and the listing paints the same. A badge that does not arrive cannot
    /// bring down a screen.
    ///
    /// The batch is the VISIBLE WINDOW, not the directory: each call spins
    /// up a wasm instance per plugin (#224 measured 167 ms per page of 20
    /// over 2000 entries), so asking for them for what is not visible is
    /// paying that price for nothing.
    fn plugin_decorate(
        &self,
        paths: Vec<VPath>,
        kinds: Vec<norte_proto::EntryKind>,
    ) -> BoxFuture<'static, Result<Vec<methods::PluginDecorations>, Error>>;

    /// The values of ONE column a plugin contributes, for a batch.
    ///
    /// The "no data" shape is a vector of `None` the SIZE of `paths`, not an
    /// empty vector: the contract is positional and whoever consumes it
    /// always expects one cell per path, even when the column does not
    /// apply.
    ///
    /// Fail-soft the same as [`Self::plugin_decorate`]: a column that fails
    /// stays blank, it never turns the listing into an error.
    fn plugin_column_values(
        &self,
        plugin: String,
        column: String,
        paths: Vec<VPath>,
    ) -> BoxFuture<'static, Result<Vec<Option<String>>, Error>>;

    /// The frame a plugin paints for its panel (0.74.0, phase 3).
    ///
    /// `None` when no consented plugin paints that panel, which is the same
    /// case as an older daemon without the method: in both, the slot keeps
    /// whatever it had. Fail-soft like everything that decorates.
    fn plugin_panel_render(
        &self,
        params: methods::PluginPanelRenderParams,
    ) -> BoxFuture<'static, Result<Option<methods::PanelFrame>, Error>>;

    /// The HOST's volumes: disks, network mounts, removable media.
    ///
    /// Not a provider call, which is why it does not live in the `fs.*`
    /// family: the mount table belongs to the host, and the daemon only
    /// answers it to a human connection — an agent under scope does not need
    /// it.
    fn volumes(&self) -> BoxFuture<'static, Result<Vec<methods::Volume>, Error>>;

    /// Asks for a sync PLAN: its Task and the events channel.
    ///
    /// The plan writes NOT A BYTE: it says what it would do. What writes is
    /// `sync.apply` — which this trait does not expose yet, and that absence
    /// IS phase A's boundary — and only against the `plan_hash` this plan
    /// closed with.
    fn sync_plan(
        &self,
        params: methods::SyncPlanParams,
    ) -> BoxFuture<
        'static,
        Result<
            (
                HostTask,
                tokio::sync::mpsc::Receiver<norte_client::SyncPlanEvent>,
            ),
            Error,
        >,
    >;

    /// APPLIES an already-reviewed plan, by its `plan_hash`.
    ///
    /// The hash is a FRESHNESS token, not an approval one: it is public and
    /// deterministic, so what it guarantees is that the plan the re-plan
    /// produces NOW is what gets executed, and that a hash approved for one
    /// directory is not valid against another. Who can redeem it is decided
    /// by the core's policy.
    ///
    /// This WRITES: it is the only call on this surface that does.
    fn sync_apply(
        &self,
        plan_hash: methods::PlanHash,
    ) -> BoxFuture<'static, Result<HostTask, Error>>;

    /// The report of an already-finished sync.
    ///
    /// Same role as a rename batch's report: the Task's outcome says
    /// whether it ran, and what was NOT done — the steps that failed, what
    /// was left un-undone — is told only by the report.
    fn sync_report(
        &self,
        task_id: TaskId,
    ) -> BoxFuture<'static, Result<methods::SyncReportResult, Error>>;

    /// Compares two trees and returns its Task AND the channel of BATCHES of
    /// rows.
    ///
    /// Both together for the same reason as in [`Self::search`]: the
    /// comparison is a long task whose outcome goes through the progress and
    /// whose rows go through the channel, and keeping only one means either
    /// not being able to stop it or not seeing anything.
    ///
    /// Cancelling it is the ONLY brake: the engine emits one row per matched
    /// name across the whole tree and there is no cap — a cap would turn
    /// "are these two trees the same?" into a half answer, which is the one
    /// thing this question does not allow.
    fn compare(
        &self,
        params: methods::FsCompareParams,
    ) -> BoxFuture<
        'static,
        Result<
            (
                HostTask,
                tokio::sync::mpsc::Receiver<methods::CompareRowsBatch>,
            ),
            Error,
        >,
    >;

    /// SEMANTIC search against the index (`index.search_semantic`).
    ///
    /// A direct response, not a Task: the core embeds the query and sweeps
    /// the index, and what comes back is the whole list, best first.
    ///
    /// **Leaves the process**: the query goes to the configured AI provider.
    /// The daemon only serves it to a human connection, and requires the
    /// index to be built and embedded — with no rows it answers `NotFound`,
    /// which is an answer you have to know how to read, not just any
    /// failure.
    fn semantic_search(
        &self,
        query: String,
        k: u32,
    ) -> BoxFuture<'static, Result<Vec<methods::SemanticHit>, Error>>;

    /// Launches a search over the subtree and returns its Task AND the
    /// channel BATCHES of results arrive on.
    ///
    /// Both together because they are one thing: a search is a long task
    /// whose outcome goes through the progress and whose findings go through
    /// the channel. Keeping only one means either not being able to cancel
    /// it, or not seeing anything.
    fn search(
        &self,
        params: methods::FsSearchParams,
    ) -> BoxFuture<
        'static,
        Result<(HostTask, tokio::sync::mpsc::Receiver<methods::SearchHits>), Error>,
    >;
}

/// The real backend: the SDK.
impl HostBackend for norte_client::RemoteBackend {
    fn list(
        &self,
        dir: VPath,
        attrs: Vec<String>,
    ) -> BoxFuture<'static, Result<(EntryStream, Option<u64>), Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.list_stream(&dir, attrs).await })
    }

    fn stat(&self, path: VPath, attrs: Vec<String>) -> BoxFuture<'static, Result<Entry, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.stat(&path, attrs).await })
    }

    fn capabilities(&self, path: VPath) -> BoxFuture<'static, Result<Capabilities, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.capabilities(&path).await })
    }

    fn read(
        &self,
        path: VPath,
        range: Option<norte_proto::ByteRange>,
    ) -> BoxFuture<'static, Result<Vec<u8>, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.read(&path, range).await })
    }

    fn attr_catalog(&self, dir: VPath) -> BoxFuture<'static, Result<AttrCatalog, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.attr_catalog(&dir).await })
    }

    fn take_approvals(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<methods::PolicyApprovalRequired>> {
        norte_client::RemoteBackend::take_approvals(self)
    }

    fn policy_decide(
        &self,
        approval_id: u64,
        approve: bool,
    ) -> BoxFuture<'static, Result<(), Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.policy_decide(approval_id, approve).await })
    }

    fn checksum(
        &self,
        params: norte_proto::methods::FsChecksumParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        let backend = self.clone();
        Box::pin(async move {
            let task = backend.checksum(params).await?;
            let canceller = task.canceller();
            let pause_handle = canceller.clone();
            Ok(HostTask {
                id: task.id(),
                progress: task.progress(),
                cancel: Arc::new(move || canceller.cancel()),
                pause: Some(remote_pause(pause_handle.clone())),
                cola: Some(remote_queue_move(pause_handle)),
                foreign: false,
            })
        })
    }

    fn checksum_report(
        &self,
        task: norte_proto::TaskId,
    ) -> BoxFuture<'static, Result<norte_proto::methods::FsChecksumReportResult, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.checksum_report(task).await })
    }

    fn dir_usage(
        &self,
        params: norte_proto::methods::FsDirUsageParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        let backend = self.clone();
        Box::pin(async move {
            let task = backend.dir_usage(params).await?;
            let canceller = task.canceller();
            let pause_handle = canceller.clone();
            Ok(HostTask {
                id: task.id(),
                progress: task.progress(),
                cancel: Arc::new(move || canceller.cancel()),
                pause: Some(remote_pause(pause_handle.clone())),
                cola: Some(remote_queue_move(pause_handle)),
                foreign: false,
            })
        })
    }

    fn dir_usage_report(
        &self,
        task: norte_proto::TaskId,
    ) -> BoxFuture<'static, Result<norte_proto::methods::FsDirUsageReportResult, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.dir_usage_report(task).await })
    }

    fn set_mode(
        &self,
        params: norte_proto::methods::FsSetModeParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        let backend = self.clone();
        Box::pin(async move {
            let task = backend.set_mode(params).await?;
            let canceller = task.canceller();
            let pause_handle = canceller.clone();
            Ok(HostTask {
                id: task.id(),
                progress: task.progress(),
                cancel: Arc::new(move || canceller.cancel()),
                pause: Some(remote_pause(pause_handle.clone())),
                cola: Some(remote_queue_move(pause_handle)),
                foreign: false,
            })
        })
    }

    fn mkdir(&self, path: VPath) -> BoxFuture<'static, Result<HostTask, Error>> {
        let backend = self.clone();
        Box::pin(async move {
            let task = backend.mkdir(&path).await?;
            let canceller = task.canceller();
            let pause_handle = canceller.clone();
            Ok(HostTask {
                id: task.id(),
                progress: task.progress(),
                cancel: Arc::new(move || canceller.cancel()),
                pause: Some(remote_pause(pause_handle.clone())),
                cola: Some(remote_queue_move(pause_handle)),
                foreign: false,
            })
        })
    }

    fn create_file(&self, path: VPath) -> BoxFuture<'static, Result<HostTask, Error>> {
        let backend = self.clone();
        Box::pin(async move {
            let task = backend.create_file(&path).await?;
            let canceller = task.canceller();
            let pause_handle = canceller.clone();
            Ok(HostTask {
                id: task.id(),
                progress: task.progress(),
                cancel: Arc::new(move || canceller.cancel()),
                pause: Some(remote_pause(pause_handle.clone())),
                cola: Some(remote_queue_move(pause_handle)),
                foreign: false,
            })
        })
    }

    fn session_get(&self) -> BoxFuture<'static, Result<(methods::Session, bool), Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.session_get().await })
    }

    fn session_put(
        &self,
        version: u32,
        revision: u64,
        body: serde_json::Value,
    ) -> BoxFuture<'static, Result<u64, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.session_put(version, revision, body).await })
    }

    fn session_release(&self) -> BoxFuture<'static, Result<bool, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.session_release().await })
    }

    fn log_tail(
        &self,
        cursor: Option<u64>,
        max: u32,
    ) -> BoxFuture<'static, Result<methods::LogTailResult, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.log_tail(cursor, max).await })
    }

    fn log_level(&self, level: String) -> BoxFuture<'static, Result<String, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.log_level(&level).await })
    }

    fn take_conn_events(&self) -> Option<tokio::sync::mpsc::UnboundedReceiver<ConnEvent>> {
        norte_client::RemoteBackend::take_conn_events(self)
    }

    fn take_degraded(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<methods::ConnectionDegraded>> {
        norte_client::RemoteBackend::take_degraded(self)
    }

    fn take_failed(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<methods::ConnectionFailed>> {
        norte_client::RemoteBackend::take_failed(self)
    }

    fn take_plugin_notices(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<methods::PluginNotice>> {
        norte_client::RemoteBackend::take_plugin_notices(self)
    }

    fn take_foreign_tasks(&self) -> Option<tokio::sync::mpsc::UnboundedReceiver<HostTask>> {
        let mut source = norte_client::RemoteBackend::take_foreign_tasks(self)?;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        // A channel is not mapped in place: the bridge is a forwarding task
        // that dies with the channel that feeds it.
        tokio::spawn(async move {
            while let Some(t) = source.recv().await {
                let canceller = t.canceller();
                let pause_handle = canceller.clone();
                let task = HostTask {
                    id: t.id(),
                    progress: t.progress(),
                    cancel: Arc::new(move || canceller.cancel()),
                    pause: Some(remote_pause(pause_handle.clone())),
                    cola: Some(remote_queue_move(pause_handle)),
                    foreign: true,
                };
                if tx.send(task).is_err() {
                    return;
                }
            }
        });
        Some(rx)
    }

    fn plugin_list(&self) -> BoxFuture<'static, Result<methods::PluginListResult, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.plugins_list().await })
    }

    fn plugin_help(
        &self,
        id: String,
    ) -> BoxFuture<'static, Result<methods::PluginHelpResult, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.plugin_help(&id).await })
    }

    fn plugin_config(
        &self,
        id: String,
    ) -> BoxFuture<'static, Result<methods::PluginGetConfigResult, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.plugin_get_config(&id).await })
    }

    fn plugin_set_approval(
        &self,
        id: String,
        approved: bool,
        expected_digest: Option<String>,
    ) -> BoxFuture<'static, Result<(), Error>> {
        let backend = self.clone();
        Box::pin(async move {
            backend
                .plugins_set_approval(&id, approved, expected_digest.as_deref())
                .await
        })
    }

    fn plugin_set_enabled(
        &self,
        id: String,
        enabled: bool,
    ) -> BoxFuture<'static, Result<(), Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.plugins_set_enabled(&id, enabled).await })
    }

    fn plugin_uninstall(&self, id: String) -> BoxFuture<'static, Result<bool, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.plugins_uninstall(&id).await.map(|r| r.was_approved) })
    }

    fn plugin_set_config(
        &self,
        id: String,
        key: String,
        value: String,
    ) -> BoxFuture<'static, Result<(), Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.plugin_set_config(&id, &key, &value).await })
    }

    fn plugin_run_command(
        &self,
        id: String,
        command: String,
        arg: String,
    ) -> BoxFuture<'static, Result<String, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.plugin_run_command(&id, &command, &arg).await })
    }

    fn plugin_preview_styled(
        &self,
        path: VPath,
        columns: Option<u32>,
    ) -> BoxFuture<'static, Result<Option<methods::PluginPreviewStyled>, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.plugin_preview_styled(&path, columns).await })
    }

    fn plugin_thumbnail(
        &self,
        path: VPath,
        max_edge: u32,
    ) -> BoxFuture<'static, Result<Option<methods::PluginThumbnail>, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.plugin_thumbnail(&path, max_edge).await })
    }

    fn plugin_decorate(
        &self,
        paths: Vec<VPath>,
        kinds: Vec<norte_proto::EntryKind>,
    ) -> BoxFuture<'static, Result<Vec<methods::PluginDecorations>, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.plugin_decorate(&paths, &kinds).await })
    }

    fn semantic_search(
        &self,
        query: String,
        k: u32,
    ) -> BoxFuture<'static, Result<Vec<methods::SemanticHit>, Error>> {
        let backend = self.clone();
        // Without `root`: the whole index, same as the TUI. Bounding by the
        // pane's directory would promise a scope the index may not have —
        // it is built by roots, not by what is being looked at.
        Box::pin(async move { backend.index_search_semantic(None, &query, k).await })
    }

    fn plugin_column_values(
        &self,
        plugin: String,
        column: String,
        paths: Vec<VPath>,
    ) -> BoxFuture<'static, Result<Vec<Option<String>>, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.plugin_column_values(&plugin, &column, &paths).await })
    }

    fn plugin_panel_render(
        &self,
        params: methods::PluginPanelRenderParams,
    ) -> BoxFuture<'static, Result<Option<methods::PanelFrame>, Error>> {
        let backend = self.clone();
        // The `RemoteBackend`'s INHERENT method, which wins over this
        // trait's one by having the same name and the same signature. Its
        // neighbors tell themselves apart on their own because they take
        // references; this one does not, so if someone renames or deletes
        // the inherent one, this line starts calling itself — it compiles,
        // and blows up the actor's stack on the first call.
        Box::pin(
            async move { norte_client::RemoteBackend::plugin_panel_render(&backend, params).await },
        )
    }

    fn search(
        &self,
        params: methods::FsSearchParams,
    ) -> BoxFuture<
        'static,
        Result<(HostTask, tokio::sync::mpsc::Receiver<methods::SearchHits>), Error>,
    > {
        let backend = self.clone();
        Box::pin(async move {
            let (task, rx) = backend.search(params).await?;
            let canceller = task.canceller();
            let pause_handle = canceller.clone();
            Ok((
                HostTask {
                    id: task.id(),
                    progress: task.progress(),
                    cancel: Arc::new(move || canceller.cancel()),
                    pause: Some(remote_pause(pause_handle.clone())),
                    cola: Some(remote_queue_move(pause_handle)),
                    foreign: false,
                },
                rx,
            ))
        })
    }

    fn sync_plan(
        &self,
        params: methods::SyncPlanParams,
    ) -> BoxFuture<
        'static,
        Result<
            (
                HostTask,
                tokio::sync::mpsc::Receiver<norte_client::SyncPlanEvent>,
            ),
            Error,
        >,
    > {
        let backend = self.clone();
        Box::pin(async move {
            let (task, rx) = backend.sync_plan(params).await?;
            let canceller = task.canceller();
            let pause_handle = canceller.clone();
            Ok((
                HostTask {
                    id: task.id(),
                    progress: task.progress(),
                    cancel: Arc::new(move || canceller.cancel()),
                    pause: Some(remote_pause(pause_handle.clone())),
                    cola: Some(remote_queue_move(pause_handle)),
                    foreign: false,
                },
                rx,
            ))
        })
    }

    fn sync_apply(
        &self,
        plan_hash: methods::PlanHash,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        let backend = self.clone();
        Box::pin(async move {
            let task = backend.sync_apply(&plan_hash).await?;
            let canceller = task.canceller();
            let pause_handle = canceller.clone();
            Ok(HostTask {
                id: task.id(),
                progress: task.progress(),
                cancel: Arc::new(move || canceller.cancel()),
                pause: Some(remote_pause(pause_handle.clone())),
                cola: Some(remote_queue_move(pause_handle)),
                foreign: false,
            })
        })
    }

    fn sync_report(
        &self,
        task_id: TaskId,
    ) -> BoxFuture<'static, Result<methods::SyncReportResult, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.sync_report(task_id).await })
    }

    fn compare(
        &self,
        params: methods::FsCompareParams,
    ) -> BoxFuture<
        'static,
        Result<
            (
                HostTask,
                tokio::sync::mpsc::Receiver<methods::CompareRowsBatch>,
            ),
            Error,
        >,
    > {
        let backend = self.clone();
        Box::pin(async move {
            let (task, rx) = backend.compare(params).await?;
            let canceller = task.canceller();
            let pause_handle = canceller.clone();
            Ok((
                HostTask {
                    id: task.id(),
                    progress: task.progress(),
                    cancel: Arc::new(move || canceller.cancel()),
                    pause: Some(remote_pause(pause_handle.clone())),
                    cola: Some(remote_queue_move(pause_handle)),
                    foreign: false,
                },
                rx,
            ))
        })
    }

    fn volumes(&self) -> BoxFuture<'static, Result<Vec<methods::Volume>, Error>> {
        let backend = self.clone();
        // Without the pseudo-filesystems: `proc`, `sysfs` and friends fill
        // the list with places nobody wants to go.
        Box::pin(async move { backend.volumes(false).await })
    }

    fn delete(&self, path: VPath, mode: DeleteMode) -> BoxFuture<'static, Result<HostTask, Error>> {
        let backend = self.clone();
        Box::pin(async move {
            let task = backend.delete(&path, mode).await?;
            let canceller = task.canceller();
            let pause_handle = canceller.clone();
            Ok(HostTask {
                id: task.id(),
                progress: task.progress(),
                cancel: Arc::new(move || canceller.cancel()),
                pause: Some(remote_pause(pause_handle.clone())),
                cola: Some(remote_queue_move(pause_handle)),
                foreign: false,
            })
        })
    }

    fn copy(
        &self,
        from: VPath,
        to: VPath,
        on_collision: CollisionPolicy,
        queued: bool,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        transfer_op(self, Verb::Copy, from, to, on_collision, queued)
    }

    fn pack(
        &self,
        params: methods::ArchivePackParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        let backend = self.clone();
        Box::pin(async move {
            let task = backend.pack(params).await?;
            let canceller = task.canceller();
            let pause_handle = canceller.clone();
            Ok(HostTask {
                id: task.id(),
                progress: task.progress(),
                cancel: Arc::new(move || canceller.cancel()),
                pause: Some(remote_pause(pause_handle.clone())),
                cola: Some(remote_queue_move(pause_handle)),
                foreign: false,
            })
        })
    }

    fn test_archive(
        &self,
        params: methods::ArchiveTestParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        let backend = self.clone();
        Box::pin(async move {
            let task = backend.test_archive(params).await?;
            let canceller = task.canceller();
            let pause_handle = canceller.clone();
            Ok(HostTask {
                id: task.id(),
                progress: task.progress(),
                cancel: Arc::new(move || canceller.cancel()),
                pause: Some(remote_pause(pause_handle.clone())),
                cola: Some(remote_queue_move(pause_handle)),
                foreign: false,
            })
        })
    }

    fn connections(&self) -> BoxFuture<'static, Result<methods::ConnectionListResult, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.connections().await })
    }

    fn close_connection(&self, path: VPath) -> BoxFuture<'static, Result<bool, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.close_connection(&path).await })
    }

    fn provide_secret(
        &self,
        conn: String,
        secret: String,
    ) -> BoxFuture<'static, Result<(), Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.provide_secret(&conn, &secret).await })
    }

    fn split_file(
        &self,
        params: methods::FileSplitParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        let backend = self.clone();
        Box::pin(async move {
            let task = backend.split_file(params).await?;
            let canceller = task.canceller();
            let pause_handle = canceller.clone();
            Ok(HostTask {
                id: task.id(),
                progress: task.progress(),
                cancel: Arc::new(move || canceller.cancel()),
                pause: Some(remote_pause(pause_handle.clone())),
                cola: Some(remote_queue_move(pause_handle)),
                foreign: false,
            })
        })
    }

    fn combine_files(
        &self,
        params: methods::FileCombineParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        let backend = self.clone();
        Box::pin(async move {
            let task = backend.combine_files(params).await?;
            let canceller = task.canceller();
            let pause_handle = canceller.clone();
            Ok(HostTask {
                id: task.id(),
                progress: task.progress(),
                cancel: Arc::new(move || canceller.cancel()),
                pause: Some(remote_pause(pause_handle.clone())),
                cola: Some(remote_queue_move(pause_handle)),
                foreign: false,
            })
        })
    }

    fn dir_size(&self, paths: Vec<VPath>) -> BoxFuture<'static, Result<HostTask, Error>> {
        let backend = self.clone();
        Box::pin(async move {
            let task = backend.dir_size(methods::FsDirSizeParams { paths }).await?;
            let canceller = task.canceller();
            let pause_handle = canceller.clone();
            Ok(HostTask {
                id: task.id(),
                progress: task.progress(),
                cancel: Arc::new(move || canceller.cancel()),
                pause: Some(remote_pause(pause_handle.clone())),
                cola: Some(remote_queue_move(pause_handle)),
                foreign: false,
            })
        })
    }

    fn ai_rename_plan(
        &self,
        dir: VPath,
        instruction: String,
        names: Vec<String>,
    ) -> BoxFuture<'static, Result<methods::AiRenamePlanResult, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.ai_rename_plan(&dir, &instruction, &names).await })
    }

    fn plugin_rename_plan(
        &self,
        plugin_id: String,
        renamer_id: String,
        dir: VPath,
        names: Vec<String>,
    ) -> BoxFuture<'static, Result<methods::AiRenamePlanResult, Error>> {
        let backend = self.clone();
        Box::pin(async move {
            backend
                .plugin_rename_plan(&plugin_id, &renamer_id, &dir, &names)
                .await
        })
    }

    fn ai_organize_plan(
        &self,
        dir: VPath,
        instruction: String,
        names: Vec<String>,
    ) -> BoxFuture<'static, Result<methods::AiOrganizePlanResult, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.ai_organize_plan(&dir, &instruction, &names).await })
    }

    fn plugin_organize_plan(
        &self,
        plugin_id: String,
        organizer_id: String,
        dir: VPath,
        names: Vec<String>,
    ) -> BoxFuture<'static, Result<methods::AiOrganizePlanResult, Error>> {
        let backend = self.clone();
        Box::pin(async move {
            backend
                .plugin_organize_plan(&plugin_id, &organizer_id, &dir, &names)
                .await
        })
    }

    fn organize(
        &self,
        dir: VPath,
        moves: Vec<methods::OrganizeMove>,
        plan_hash: methods::PlanHash,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        let backend = self.clone();
        Box::pin(async move {
            let task = backend.organize(&dir, &moves, &plan_hash).await?;
            let canceller = task.canceller();
            let pause_handle = canceller.clone();
            Ok(HostTask {
                id: task.id(),
                progress: task.progress(),
                cancel: Arc::new(move || canceller.cancel()),
                pause: Some(remote_pause(pause_handle.clone())),
                cola: Some(remote_queue_move(pause_handle)),
                foreign: false,
            })
        })
    }

    fn rename_batch_plan(
        &self,
        dir: VPath,
        pairs: Vec<methods::RenamePair>,
    ) -> BoxFuture<'static, Result<methods::FsRenameBatchPlanResult, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.rename_batch_plan(&dir, &pairs).await })
    }

    fn rename_batch(
        &self,
        dir: VPath,
        pairs: Vec<methods::RenamePair>,
        plan_hash: methods::PlanHash,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        let backend = self.clone();
        Box::pin(async move {
            let task = backend.rename_batch(&dir, &pairs, &plan_hash).await?;
            let canceller = task.canceller();
            let pause_handle = canceller.clone();
            Ok(HostTask {
                id: task.id(),
                progress: task.progress(),
                cancel: Arc::new(move || canceller.cancel()),
                pause: Some(remote_pause(pause_handle.clone())),
                cola: Some(remote_queue_move(pause_handle)),
                foreign: false,
            })
        })
    }

    fn rename_batch_report(
        &self,
        task_id: TaskId,
    ) -> BoxFuture<'static, Result<methods::FsRenameBatchReportResult, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.rename_batch_report(task_id).await })
    }

    fn undo_session(&self, session: String) -> BoxFuture<'static, Result<HostTask, Error>> {
        let backend = self.clone();
        Box::pin(async move {
            let task = backend.undo_session(&session).await?;
            let canceller = task.canceller();
            let pause_handle = canceller.clone();
            Ok(HostTask {
                id: task.id(),
                progress: task.progress(),
                cancel: Arc::new(move || canceller.cancel()),
                pause: Some(remote_pause(pause_handle.clone())),
                cola: Some(remote_queue_move(pause_handle)),
                foreign: false,
            })
        })
    }

    fn undo_report(
        &self,
        task_id: TaskId,
    ) -> BoxFuture<'static, Result<methods::PolicyUndoReportResult, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.undo_report(task_id).await })
    }

    fn journal_list(
        &self,
        before_seq: Option<i64>,
        limit: u32,
    ) -> BoxFuture<'static, Result<methods::JournalListResult, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.journal_list(before_seq, limit, None).await })
    }

    fn undo_after(
        &self,
        seq: i64,
        upto_seq: Option<i64>,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        let backend = self.clone();
        Box::pin(async move {
            let task = backend.undo_after(seq, upto_seq).await?;
            let canceller = task.canceller();
            let pause_handle = canceller.clone();
            Ok(HostTask {
                id: task.id(),
                progress: task.progress(),
                cancel: Arc::new(move || canceller.cancel()),
                pause: Some(remote_pause(pause_handle.clone())),
                cola: Some(remote_queue_move(pause_handle)),
                foreign: false,
            })
        })
    }

    fn archive_pack_report(
        &self,
        task_id: TaskId,
    ) -> BoxFuture<'static, Result<methods::ArchivePackReportResult, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.archive_pack_report(task_id).await })
    }

    fn move_(
        &self,
        from: VPath,
        to: VPath,
        on_collision: CollisionPolicy,
        queued: bool,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        transfer_op(self, Verb::Move, from, to, on_collision, queued)
    }
}

/// Copy or move: the two verbs of a transfer.
///
/// An enum and not the method name as a string. The difference matters
/// because the `else` branch's destination is not a visible error: it is the
/// other operation, the one that also DELETES the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verb {
    /// `fs.copy`.
    Copy,
    /// `fs.move`.
    Move,
}

/// The SHARED body of copying and moving over the SDK.
///
/// Just one because the two calls differ only in the method name and
/// nothing else: the rest — default options, wrapping the Task, the
/// canceller — has to be identical, and two copies of the same block is the
/// place where `fs.move` ends up missing the collision policy `fs.copy` does
/// send.
fn transfer_op(
    backend: &norte_client::RemoteBackend,
    verb: Verb,
    from: VPath,
    to: VPath,
    on_collision: CollisionPolicy,
    queued: bool,
) -> BoxFuture<'static, Result<HostTask, Error>> {
    let backend = backend.clone();
    // The SDK still takes the method as a STRING, and its body is
    // `if method == FS_COPY { copy } else { move }`: anything that is not
    // exactly the copy constant turns into a move.
    // It cannot happen here because what comes in is a two-variant enum, and
    // since #270 the SDK also takes an enum: the `else` that turned any
    // unknown method into a move no longer exists.
    let method = match verb {
        Verb::Copy => norte_client::Transfer::Copy,
        Verb::Move => norte_client::Transfer::Move,
    };
    Box::pin(async move {
        let task = backend
            .transfer(
                method,
                &from,
                &to,
                norte_client::TransferOptions {
                    on_collision,
                    queued,
                    // The rest is the wire default: preserve symlinks and do
                    // not resume. Resuming is a user decision (ADR 0012) and
                    // this window has nowhere to make it yet, so it sends
                    // what the daemon understands as "not requested" instead
                    // of choosing for the user.
                    ..norte_client::TransferOptions::default()
                },
            )
            .await?;
        let canceller = task.canceller();
        let pause_handle = canceller.clone();
        Ok(HostTask {
            id: task.id(),
            progress: task.progress(),
            cancel: Arc::new(move || canceller.cancel()),
            pause: Some(remote_pause(pause_handle.clone())),
            cola: Some(remote_queue_move(pause_handle)),
            foreign: false,
        })
    })
}
