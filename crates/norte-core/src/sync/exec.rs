//! The executor: an approved plan turns into writes, into ONE undoable
//! journal unit, and into a report saying what happened with each step.
//!
//! `crates/norte-core/src/rename/exec.rs` is the sibling and worth reading
//! first: a Task for many steps, a `StepJournal` with a batch sharing a
//! `batch_id`, a report behind a `Mutex` and cancellation checked BETWEEN
//! steps. What's different here —and it's the only thing that matters— is
//! that **there is no rollback**.
//!
//! # Half a sync is a real state; half a permutation isn't
//!
//! And **half a delete is one too**: a `DeleteTree` cut in half writes its
//! entry for whatever it managed to remove (#186). The condition that used
//! to skip it on `Err(Cancelled)` excluded exactly the case where the entry
//! matters — `remove_tree` checks the token BETWEEN entries, so by the time
//! it returns `Cancelled` an arbitrary number of nodes have already fallen,
//! and against a trash-less destination they've fallen for good.
//!
//! The rename batch unwinds entirely on the first failure because a
//! half-renamed directory is neither the before nor the after. A
//! half-synced tree IS something: it's the tree from before with forty
//! thousand files already updated. Automatically unwinding it would take
//! away from the user work that went well just to put it back where it was,
//! and on top of that every unwound step is another write that can fail.
//! So **a step that fails is a row of the report and the Task continues**,
//! and what was applied stays —journalled under its batch, i.e. undoable by
//! hand by whoever wants to undo it—.
//!
//! # Revalidation is the only thing between the TTL and a lost file
//! Between a human approving a plan and it being applied, up to
//! [`SYNC_PLAN_TTL_MS`](norte_proto::methods::SYNC_PLAN_TTL_MS)
//! milliseconds pass. Before EVERY destructive step —an `Overwrite`, a
//! `DeleteTree`— a `stat` is done and compared against what the comparison
//! saw at the time ([`DestWitness`]); if it doesn't match, the step is NOT
//! executed and comes out in the report as [`SyncFailureCause::Conflict`].
//! Without that, approving a plan would be signing a blank check against
//! the destination tree for ten minutes.
//!
//! # Hard rule 4: a plan with no journal doesn't apply
//! `sync.apply` REQUIRES a journal
//! ([`Engine::sync_apply_as`](crate::Engine::sync_apply_as) answers
//! `Unsupported` without one). The rename batch tolerates not having one
//! because a rename is undone by looking at the directory; here it
//! overwrites and buries, and every step of the plan carries a promised
//! [`StepReversal`] only the journal can fulfill.
//!
//! Since #167 the embedded transport DOES have the state directory's
//! journal —and since #177 it opens it right here, when `sync.apply` asks
//! for it—, so this `Unsupported` stopped being the common case: it's left
//! for the engine that really can't have one (another process holding the
//! lock, or an embedder that built `Engine::new()` by hand).
//!
//! # How each class is journalled (the spec's normative table)
//!
//! | step | entries | reversal |
//! | --- | --- | --- |
//! | `CreateDir`, `Copy` | `created` | `Delete` |
//! | `Overwrite` with trash | `trashed` + `created` | `RestoreTrash` + `Delete` |
//! | `Overwrite` with no trash | `created` | `Irreversible` |
//! | `DeleteTree` with trash | `trashed` (ONE, for the whole tree) | `RestoreTrash` |
//! | `DeleteTree` with no trash | `removed` (ONE, for whatever got removed) | `Irreversible` |
//! | `Skip` | none | — |
//!
//! **"No trash" here means "no reversal", and those are two different
//! things.** A destination with trash that does NOT NAME what it buries
//! (`Provider::trash_restorable` at `false`) produces a plan whose steps
//! are all `Irreversible`, so it falls into the table's "no trash" rows —
//! but the delete STILL goes to the trash (`destroy_leaf`/`destroy_tree`
//! check `delete_mode`, which comes from whether the destination has
//! trash). What's lost is the undo, not the user's trash.
//!
//! Undo walks `seq` in descending order, so within an `Overwrite`'s pair it
//! deletes what was created BEFORE restoring what was buried. The correct
//! order comes from the mechanism that already existed, not from care
//! taken here.
//!
//! **The trash-less `Overwrite` carries no entry of its own for the
//! delete**, and that's deliberate: the one entry says `created` with
//! reversal `Irreversible`, which is the whole truth —"this path has new
//! content and what was there before cannot be recovered"—. With two
//! entries (an irreversible `removed` + an undoable `created`) the batch's
//! undo would delete the new file without being able to restore the old
//! one, and would leave the path EMPTY where the user had something: worse
//! than the state it came to fix.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::{Stream, StreamExt as _};
use norte_proto::methods::RelPath;
use norte_proto::methods::{
    DestTrash, SYNC_MAX_FAILURES_REPORTED, StepReversal, SyncFailure, SyncFailureCause,
    SyncReportResult, SyncStep, SyncStepKind,
};
use norte_proto::{ConflictKind, Entry, EntryKind, Error, VPath};
use norte_sync::DestWitness;
use norte_vfs::{Provider, SymlinkKind};

use crate::journal::{Actor, NewEntry, Reversal, SqliteJournal};
use crate::observer::NoopObserver;
use crate::scheduler::TaskCtx;
use crate::sync::spool::SpoolStep;

/// The two roots and their providers, resolved once.
///
/// The roots come from the SPOOL HEADER, the only place they're kept:
/// `sync.apply` carries a hash and nothing else.
pub(crate) struct SyncTargets {
    /// SOURCE root's provider.
    pub source: Arc<dyn Provider>,
    /// DESTINATION root's provider. Can be the same object.
    pub dest: Arc<dyn Provider>,
    /// Where it's read from.
    pub source_root: VPath,
    /// Where it's written to.
    ///
    /// **No PATH comes out of here**: each one is composed by joining this
    /// root with a [`RelPath`], whose segments cannot be `..` or `.` or
    /// contain `/` or NUL — [`norte_proto::Segment`] prevents that at
    /// construction, and deserialization prevents it again after decoding,
    /// so a `%2E%2E` doesn't sneak through either.
    ///
    /// **That the BYTES stay inside is guaranteed by
    /// [`Self::dest_confined`]**, not by this composition: resolution is
    /// done by the filesystem, and a symlink placed at an INTERMEDIATE
    /// component between approving and applying would redirect the write
    /// outside the tree with the daemon's credentials (#164). Destructive
    /// steps dodge it by a side effect —`stat` is an `lstat`, so
    /// revalidation sees a `Symlink` where the witness said `Dir` and
    /// answers conflict; and `walk` doesn't descend symlinks—; `Copy` and
    /// `CreateDir` defend with the confined root, when the destination
    /// knows how to give one.
    pub dest_root: VPath,
    /// The destination root OPENED, when this destination knows how to
    /// confine itself (ADR 0054).
    ///
    /// Opened ONCE per Task, here, and from then on every step that creates
    /// something addresses relative segments against it instead of a path:
    /// with no path to recompose there's no window between checking and
    /// writing.
    ///
    /// `None` = this destination doesn't know how (`file://` on Windows,
    /// SFTP, a bucket). Then it's written by path, as always, and it's said
    /// in the log: refusing would leave unsynced the destinations that
    /// can't give that defense, which is a much higher price than the risk
    /// it avoids.
    pub dest_confined: Option<Box<dyn norte_vfs::ConfinedRoot>>,
    /// The policy gate, consulted step by step over the REAL path.
    ///
    /// The root gate `sync.apply` asks for before starting resolves an
    /// agent's scope boundary (it's per root, and `is_under` is
    /// transitive), but it does **not** resolve a `deny` rule in
    /// `policy.toml` over a path INSIDE the tree: those rules match by
    /// containment of the queried path, so querying only the root doesn't
    /// see them. Without this, a `Mirror` plan with an empty source would
    /// delete a subtree `fs.delete` refuses — i.e., a looser delete
    /// authorization than the one that already exists.
    pub policy: Arc<dyn crate::policy::PolicyGate>,
    /// What kind of delete is really going to happen, so it can be asked
    /// about as what it is. Comes from the plan's `dest_has_trash`, which is
    /// the same thing each step's reversal comes from.
    pub delete_mode: norte_proto::DeleteMode,
}

impl SyncTargets {
    /// Opens the confined destination root, if this destination knows how
    /// to give one.
    ///
    /// Done INSIDE the Task, not at construction: there's a `task_id` here
    /// to say in the log which operation is being talked about when the
    /// destination doesn't know how to confine and has to degrade.
    /// # Errors
    /// That of `open_root` when the destination declared it knew how to
    /// confine and couldn't. It does NOT degrade: see
    /// [`crate::ops::open_dest_root`], where the reasoning for why that
    /// would be the key that reopens #164 lives.
    pub(crate) async fn with_dest_confined(mut self, task_id: u64) -> Result<Self, Error> {
        self.dest_confined =
            crate::ops::open_dest_root(self.dest.as_ref(), &self.dest_root, task_id).await?;
        Ok(self)
    }

    /// Is the destination root still the path syncing was requested
    /// against? (#368)
    ///
    /// The same hole ADR 0151 closed in a tree copy, and it matters MORE
    /// here: a sync is precisely the operation left running against a
    /// destination nobody is watching. The root is opened once per Task and
    /// the descriptor survives a `rename`, so deleting the destination
    /// folder mid-sync used to leave the files in the trash and the report
    /// saying it went fine.
    ///
    /// With no confined root there's nothing to check and the answer is
    /// yes: same treatment as `copy_tree`, and refusing here would break
    /// every destination that doesn't know how to confine itself.
    async fn dest_still_standing(
        &self,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<(), Error> {
        let Some(root) = self.dest_confined.as_deref() else {
            return Ok(());
        };
        crate::ops::dest_still_there_or_fails(self.dest.as_ref(), root, &self.dest_root, cancel)
            .await
    }

    /// A step's destination: the real path, plus the relative one under the
    /// confined root if there is one.
    ///
    /// `to` comes from [`dest_path`], which already composed it by joining
    /// the step's relative to [`Self::dest_root`], so the relative is
    /// recovered from there and comes out the SAME one — and if it didn't
    /// fall inside (it can't, but the type doesn't know that), the step
    /// degrades to the by-path route instead of writing somewhere the root
    /// doesn't cover.
    fn dest_at(&self, to: &VPath) -> crate::ops::Dest<'_> {
        match self
            .dest_confined
            .as_deref()
            .zip(crate::ops::rel_under(&self.dest_root, to))
        {
            Some((root, rel)) => {
                crate::ops::Dest::under(self.dest.as_ref(), Some(root), rel, to.clone())
            }
            None => crate::ops::Dest::plain(self.dest.as_ref(), to.clone()),
        }
    }

    /// Does the policy authorize THIS step over THIS path?
    ///
    /// A `Deny` is a row of the report ([`SyncFailureCause::Denied`]), not
    /// the Task's ending: it's exactly what a failing step means.
    ///
    /// An `Ask` is also refused, and not asked. A modal per step in a
    /// half-million-step plan isn't an interface, and the root gate already
    /// asked once for the whole batch; refusing is the safe side of the
    /// trade-off.
    fn allows(
        &self,
        op: crate::policy::PolicyOp,
        path: &VPath,
        actor: &Actor,
    ) -> Result<(), Error> {
        use crate::policy::{Decision, DenyReason};
        match self.policy.evaluate(actor, op, &[path]) {
            Decision::Allow => Ok(()),
            Decision::Deny(reason) => Err(Error::PolicyDenied {
                rule: reason.rule_id().to_owned(),
            }),
            Decision::Ask => Err(Error::PolicyDenied {
                rule: DenyReason::NotApproved.rule_id().to_owned(),
            }),
        }
    }
}

/// How ONE effect of this batch is recorded.
///
/// Three methods and not a `Mutation`: [`crate::observer::Mutation`] carries
/// `batch_id` only in the rename, and what makes these writes ONE undoable
/// unit is precisely that they all share theirs.
#[async_trait]
pub(crate) trait StepJournal: Send + Sync {
    /// A node this batch CREATED. `reversal` is
    /// [`Reversal::Delete`] except for a trash-less overwrite, which is
    /// [`Reversal::Irreversible`] (see the module's table).
    ///
    /// # Errors
    /// The journal's error. Hard rule 4: an effect whose entry doesn't land
    /// durably leaves the tree outside the journal, so the caller stops the
    /// Task instead of continuing to produce more.
    async fn created(&self, path: &VPath, reversal: Reversal) -> Result<(), Error>;

    /// A node this batch BURIED in the trash. `dest` is a logical trash's
    /// recoverable path, and it's what undo needs to know WHERE to fetch it
    /// from.
    ///
    /// # Errors
    /// The journal's error, as in [`StepJournal::created`].
    async fn trashed(&self, path: &VPath, dest: Option<&VPath>) -> Result<(), Error>;

    /// A node this batch PERMANENTLY deleted (a `DeleteTree` over a
    /// trash-less destination). Always [`Reversal::Irreversible`].
    ///
    /// # Errors
    /// The journal's error, as in [`StepJournal::created`].
    async fn removed(&self, path: &VPath) -> Result<(), Error>;
}

/// Records to the journal under ONE `batch_id`.
pub(crate) struct BatchJournal {
    /// The journal, which is also the engine's observer.
    pub journal: Arc<SqliteJournal>,
    /// Who caused it.
    pub actor: Actor,
    /// The id every entry of this application shares: what makes them ONE
    /// undoable unit ([`crate::journal::Journal::alloc_batch`]).
    pub batch_id: i64,
}

impl BatchJournal {
    /// The insert, with the batch set.
    async fn record(
        &self,
        op: &str,
        path: &VPath,
        reversal: Reversal,
        reversal_ref: Option<&VPath>,
    ) -> Result<(), Error> {
        let path = path.to_wire().into_bytes();
        let reference = reversal_ref.map(|p| p.to_wire().into_bytes());
        self.journal
            .journal()
            .record_entry(&NewEntry {
                op,
                path: &path,
                path_to: None,
                reversal,
                reversal_ref: reference.as_deref(),
                actor: &self.actor,
                undoes_seq: None,
                batch_id: Some(self.batch_id),
            })
            .await
            .map_err(|e| {
                tracing::error!(error = %e, op, "sync.apply: failed to write the journal");
                Error::from(e)
            })?;
        Ok(())
    }
}

#[async_trait]
impl StepJournal for BatchJournal {
    async fn created(&self, path: &VPath, reversal: Reversal) -> Result<(), Error> {
        self.record("created", path, reversal, None).await
    }

    async fn trashed(&self, path: &VPath, dest: Option<&VPath>) -> Result<(), Error> {
        self.record("trashed", path, Reversal::RestoreTrash, dest)
            .await
    }

    async fn removed(&self, path: &VPath) -> Result<(), Error> {
        self.record("removed", path, Reversal::Irreversible, None)
            .await
    }
}

/// A freshly opened report, with its batch already set.
///
/// The destination's trash goes in HERE, on opening it (#170), and not on
/// closing: it comes from the options of the plan being applied —the same
/// ones the `sync.plan_done`'s [`DestTrash`] came from—, so the report can't
/// end up saying something different from what was approved.
#[must_use]
pub(crate) fn new_report(batch_id: i64, dest_trash: DestTrash) -> SyncReportResult {
    SyncReportResult {
        done: 0,
        failed: 0,
        skipped: 0,
        bytes: 0,
        failures: Vec::new(),
        batch_id: Some(batch_id),
        dest_trash,
    }
}

/// What happened to ONE step.
#[derive(Debug)]
enum Applied {
    /// It ran, moving these bytes.
    Wrote(u64),
    /// It touched nothing: the plan already carried it as
    /// [`SyncStepKind::Skip`].
    Skipped,
}

/// What got buried and did NOT end up recorded, with everything needed to
/// find it by hand.
///
/// Exists because #160 happened LIVE and all that was left of it was a log
/// line: `sync.apply` buried the destination, the journal row never
/// arrived, and the trash path —the only place the file lives now— was
/// stuck in a `tracing` field the caller can't read. Rule 6 asks for a
/// typed error, and this is the data that error has to carry.
#[derive(Debug)]
pub(crate) struct Unrecorded {
    /// The path that got buried: what the user believes is still there.
    ///
    /// Two paths lead here and both leave the same gap: the `trashed` that
    /// couldn't be written and couldn't be compensated either (#160), and
    /// the `created` that fails AFTER a `trashed` that DID land (#206) —
    /// there the new file is in place, the old one is in the trash, and the
    /// batch cannot be undone because it's missing half of its pair.
    pub buried: VPath,
    /// Where it ended up, if the destination's trash NAMES what it takes.
    /// `None` with `DestTrash::Opaque` (macOS, Windows), and then there's
    /// nowhere to point anyone to.
    pub at: Option<VPath>,
    /// The journal error that started all this.
    pub source: Error,
}

/// Why a plan application stopped.
///
/// Two variants and not a bare [`Error`] because the two states the tree
/// can be left in are different and the caller has to be able to tell them
/// apart: with [`Self::Stopped`] what applied is journalled and the step
/// that failed left no trace; with [`Self::Unrecorded`] there's a file that
/// moved that the journal doesn't know about.
#[derive(Debug)]
pub(crate) enum ApplyError {
    /// The usual: cancellation, the journal, a spool that stopped reading.
    /// What applied up to here is recorded.
    Stopped(Error),
    /// Hard rule 4 broken, and it couldn't be compensated (#160). Carries
    /// the buried path and its trash destination so whoever receives it can
    /// SAY so — which is what a `tracing::error!` doesn't allow.
    Unrecorded(Box<Unrecorded>),
}

impl ApplyError {
    /// The shape the wire understands, AFTER stating what doesn't fit in
    /// it.
    ///
    /// [`Self::Unrecorded`]'s detail has no category in the taxonomy —there
    /// is no "your file is in the trash and nobody pointed to it"— so it's
    /// lost in the conversion, and that's why this function writes it to
    /// the operator's log before losing it. **And that's why there's no
    /// `From`**: a `?` in some future caller would silently convert it and
    /// leave the state unstated again, which is #160 all over again.
    pub(crate) fn into_wire(self) -> Error {
        match self {
            Self::Stopped(e) => e,
            Self::Unrecorded(u) => {
                // The one line that names both paths. `bury` no longer
                // writes it for this case: two `error!`s for the same fact
                // is noise, and this is the one that counts, which comes
                // out when the Task dies.
                tracing::error!(
                    error = %u.source,
                    buried = %crate::engine::span_path(&u.buried),
                    at = u.at.as_ref().map(crate::engine::span_path),
                    fix = if u.at.is_some() {
                        "fetch it by hand from the `at` path"
                    } else {
                        "this trash doesn't say where it took it (macOS/Windows): look for it in \
                         the system trash"
                    },
                    "sync.apply: STOPPED — the destination was buried, its journal entry did NOT \
                     land and returning it didn't work either",
                );
                u.source
            }
        }
    }
}

/// Why a step stopped, and whether that also stops the Task.
#[derive(Debug)]
enum StepError {
    /// The step did not happen. A row of the report; the Task continues.
    Failed(Error),
    /// Nothing can continue: a cancellation, or the journal.
    Fatal(Error),
    /// Nothing can continue AND the tree isn't what it was: something got
    /// buried, its row didn't arrive, and returning it didn't work either.
    /// Separate from [`Self::Fatal`] because the state is different —there
    /// the destination stayed intact, here it didn't— and because only this
    /// one carries where to look.
    ///
    /// `Box`ed because two `VPath`s and an `Error` make this variant four
    /// times the size of the other two, and `StepError` is the `Err` of a
    /// function called once per step of a half-million-step plan: the cold
    /// path shouldn't make the usual one pay for the size.
    Unrecoverable(Box<Unrecorded>),
}

impl StepError {
    /// A PROVIDER's error: a row of the report, unless it's the
    /// cancellation —which isn't a step failure, it's the Task's ending—.
    fn from_provider(e: Error) -> Self {
        if matches!(e, Error::Cancelled) {
            StepError::Fatal(e)
        } else {
            StepError::Failed(e)
        }
    }
}

/// The cause that goes into the report.
///
/// [`SyncFailureCause::IllegalName`] earns its own: a name's legality under
/// the DESTINATION root isn't validated at planning time (see the variant's
/// rustdoc), so it's the failure family that surfaces here as a matter of
/// course, the one the user can fix on their own, and the one that will
/// repeat identically on every attempt until they fix it. A generic `Io`
/// wouldn't tell them any of that.
fn cause_of(e: &Error) -> SyncFailureCause {
    match e {
        Error::PermissionDenied | Error::PolicyDenied { .. } => SyncFailureCause::Denied,
        Error::InvalidPath => SyncFailureCause::IllegalName,
        // "No longer as the plan saw it": the destination changed, the
        // source disappeared, or something occupies the spot.
        Error::NotFound | Error::Conflict { .. } => SyncFailureCause::Conflict,
        _ => SyncFailureCause::Io,
    }
}

/// `rel`'s absolute path under `root`.
///
/// It cannot escape `root`: a [`norte_proto::Segment`] cannot be `..` or
/// `.` or carry `/` or NUL, and [`RelPath`] validates them at construction
/// and at deserialization. It's the property that guarantees no step
/// writes outside the destination root, and that's why it's composed this
/// way and never by concatenating strings.
fn under(root: &VPath, rel: &RelPath) -> VPath {
    let mut path = root.clone();
    for segment in rel.segments() {
        path = path.join(segment.clone());
    }
    path
}

/// Where this step falls IN THE DESTINATION.
///
/// `dest_root + dest_rel.unwrap_or(rel)`, which is the normative rule of
/// [`SyncStep::dest_rel`](norte_proto::methods::SyncStep::dest_rel): it
/// writes over the file that EXISTS, not the one the source spells. A
/// source's NFC `café` against a destination's NFD `café` are the same
/// pair, and pasting the source's spelling over ext4 would create a SECOND
/// file next to the one meant to be overwritten — with a `RestoreTrash`
/// promise over something nobody buried.
fn dest_path(targets: &SyncTargets, record: &SpoolStep) -> VPath {
    let rel = record.step.dest_rel.as_ref().unwrap_or(&record.step.rel);
    under(&targets.dest_root, rel)
}

/// `stat` with retries, distinguishing "not there" from "couldn't look".
async fn stat(
    provider: &dyn Provider,
    path: &VPath,
    ctx: &TaskCtx,
) -> Result<Option<Entry>, Error> {
    use futures::FutureExt as _;
    match crate::ops::with_retry(&ctx.cancel, || provider.stat(path).boxed()).await {
        Ok(entry) => Ok(Some(entry)),
        Err(Error::NotFound) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Is the destination still what the plan saw?
///
/// The spec's `stat`, and the only thing standing between the plan's TTL
/// and a lost file. Compared against the [`DestWitness`] the comparison
/// noted:
///
/// - **Not there** → conflict. The step was approved over something that
///   no longer exists.
/// - **Changed kind** → conflict, always.
/// - **Changed size or date** → conflict. Only what BOTH snapshots carry is
///   compared: a provider that lists with no size (`file://` is one)
///   leaves the witness half-filled, and declaring a conflict over that
///   would reject the whole plan on the most common filesystem.
///
/// # A `DeleteTree` also looks at HOW MANY things are inside (#176)
/// A directory's `stat` only moves when its DIRECT children change, so a
/// subtree that gained a hundred files two levels down between approving
/// and applying used to revalidate clean and get deleted whole: the step
/// with the largest blast radius, with the loosest check.
///
/// Now a tree delete's witness carries its first level's COUNT and it's
/// counted again here. Set by the core's wiring at planning time, not the
/// transducer —which is pure and has no provider—, and that's why it costs
/// one listing per destructive step at planning and another at applying,
/// over a step that was going to list it whole anyway.
///
/// **What still goes unseen**: a change in a GRANDCHILD. A file added three
/// levels down moves neither the directory's `stat` nor its first level's
/// count. Closing that needs a revalidation walk, which is exactly the cost
/// the "an orphan is ONE journal entry" design avoids. The approval phrase
/// says so across the four delete variants: a tree is re-checked from
/// outside, not leaf by leaf.
///
/// And with a witness carrying no size or date this stays at "still exists
/// and is still the same kind". That's less than the spec promises and
/// it's what the plan can know: an `Overwrite`'s pair DOES come hydrated
/// (the cascade needs size and date to decide), so the case that really
/// matters is covered. And a second-resolution date leaves a one-second
/// window where a same-size change goes unnoticed.
async fn revalidate(
    provider: &dyn Provider,
    path: &VPath,
    witness: Option<DestWitness>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    // No snapshot, no destroying. The transducer ALWAYS sets it on the two
    // classes calling here, so a missing one isn't a sparse provider: it's
    // a spool file this binary didn't write the way it writes them. And
    // since the witness does NOT enter the `plan_hash` —it's where the
    // conclusion came from, not the conclusion—, deleting it is exactly the
    // edit the digest doesn't see; requiring it is what would make it
    // useless.
    let Some(before) = witness else {
        tracing::error!("sync.apply: a destructive step with no destination witness");
        return Err(Error::Conflict {
            conflict: ConflictKind::Unknown,
        });
    };
    let Some(now) = stat(provider, path, ctx).await? else {
        return Err(Error::NotFound);
    };
    if now.kind != before.kind {
        return Err(Error::Conflict {
            conflict: ConflictKind::TypeMismatch,
        });
    }
    let size_moved = matches!((before.size, now.size), (Some(a), Some(b)) if a != b);
    let mtime_moved = matches!((before.mtime_ms, now.mtime_ms), (Some(a), Some(b)) if a != b);
    if size_moved || mtime_moved {
        return Err(Error::Conflict {
            conflict: ConflictKind::Exists,
        });
    }
    // And the first level's count, when the plan noted it (#176): a
    // directory's `stat` only moves when its DIRECT children change, so
    // without this a subtree that gained files between approving and
    // applying would revalidate clean. Only compared if BOTH snapshots
    // carry it, same as size and date — a `None` is "not known", never
    // "zero".
    if let Some(before_count) = before.entries {
        let now_count = count_first_level(provider, path, ctx).await;
        if now_count != Some(before_count) {
            return Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            });
        }
    }
    Ok(())
}

/// How many entries `path`'s first level has, or `None` if it couldn't be
/// counted cheaply (#176).
///
/// Same cap as at planning: past it the plan didn't note anything either,
/// so the two snapshots agree on not knowing.
async fn count_first_level(provider: &dyn Provider, path: &VPath, ctx: &TaskCtx) -> Option<u64> {
    use futures::StreamExt as _;

    let mut stream = provider.list(path).await.ok()?;
    let mut n = 0_u64;
    loop {
        if ctx.cancel.is_cancelled() {
            return None;
        }
        match stream.next().await {
            Some(Ok(_)) => {
                n += 1;
                if n > crate::sync::COUNT_MAX {
                    return None;
                }
            }
            Some(Err(_)) => return None,
            None => return Some(n),
        }
    }
}

/// Copies ONE leaf from the source to the destination and returns the bytes
/// it moved.
///
/// The destination has to be FREE: a provider's `write` is create-new, so
/// an occupied destination comes out as `Conflict` instead of clobbering
/// anything by surprise. Whoever overwrites has already emptied it
/// beforehand, deliberately and with its journal entry.
///
/// **The journal entry is NOT set by `ops`**, and that's why it's given an
/// observer that does nothing: `Mutation::Created` carries no `batch_id`,
/// so the copy would end up outside the batch and undo wouldn't see it.
/// The caller sets it, with the batch and with the reversal that step class
/// gets.
async fn copy_leaf(
    targets: &SyncTargets,
    from: &VPath,
    to: &VPath,
    entry: &Entry,
    ctx: &TaskCtx,
) -> Result<u64, Error> {
    use norte_proto::{CollisionPolicy, ResumePolicy, SymlinkPolicy, VerifyPolicy};

    if entry.kind == EntryKind::Symlink {
        let target = crate::ops::with_retry(&ctx.cancel, || {
            use futures::FutureExt as _;
            targets.source.read_link(from).boxed()
        })
        .await?;
        // `Unknown`: the kind is resolved by the destination provider
        // against its own tree, same as in a normal copy (issue #18).
        crate::ops::symlink_retrying(
            &targets.dest_at(to),
            &target,
            SymlinkKind::Unknown,
            &ctx.cancel,
        )
        .await?;
        return Ok(0);
    }
    let opts = crate::TransferOptions {
        // Never read through this path —`copy_file_retrying` only looks at
        // `resume` and `verify`—, and it's explicit so it's stated: the
        // executor is the one resolving collisions, revalidating before
        // destroying. What makes an occupied destination come out as a
        // conflict is that a provider's `write` is create-new.
        on_collision: CollisionPolicy::Fail,
        symlinks: SymlinkPolicy::Preserve,
        // Cancelling leaves the destination CLEAN, which is M1's contract
        // and what this branch chooses: a `.norte-partial` per interrupted
        // file stays in the destination tree, and the next comparison
        // would see it as an orphan —which under `Mirror` is a
        // `DeleteTree`—. Resuming a large sync is an improvement that can
        // be added later; leaving garbage the mode itself sweeps away, no.
        resume: ResumePolicy::Off,
        verify: VerifyPolicy::default(),
        // A sync isn't queued: it's ONE plan, and its steps already go in
        // the order the plan decided.
        queued: false,
    };
    let before = ctx.progress.snapshot().bytes_done;
    let observer: Arc<dyn crate::observer::MutationObserver> = Arc::new(NoopObserver);
    crate::ops::copy_file_retrying(
        targets.source.as_ref(),
        &targets.dest_at(to),
        from,
        entry.size,
        opts,
        &observer,
        ctx,
    )
    .await?;
    Ok(ctx.progress.snapshot().bytes_done.saturating_sub(before))
}

/// A copying step's source, looked at BEFORE touching the destination.
///
/// **The order is half of an `Overwrite`'s safety.** Looking at the source
/// AFTER burying the destination, a source that disappeared between
/// approving and applying —or changed into a directory— leaves the
/// destination path EMPTY: whatever was there buried and nothing to
/// replace it with, with a report saying "conflict" over a destruction
/// that already happened. Looking at it first, the step fails without
/// having touched anything.
async fn source_leaf(
    targets: &SyncTargets,
    record: &SpoolStep,
    ctx: &TaskCtx,
) -> Result<(VPath, Entry), StepError> {
    let from = under(&targets.source_root, &record.step.rel);
    let entry = stat(targets.source.as_ref(), &from, ctx)
        .await
        .map_err(StepError::from_provider)?
        .ok_or(StepError::Failed(Error::NotFound))?;
    if entry.kind == EntryKind::Dir {
        // The plan named a LEAF. A directory here means the source changed
        // shape, and copying it recursively would push into the batch a
        // whole subtree nobody approved.
        return Err(StepError::Failed(Error::Conflict {
            conflict: ConflictKind::TypeMismatch,
        }));
    }
    Ok((from, entry))
}

/// Copies an already-checked leaf over a FREE destination, and journals it
/// with `reversal`.
async fn place(
    targets: &SyncTargets,
    from: &VPath,
    entry: &Entry,
    recorder: &dyn StepJournal,
    to: &VPath,
    reversal: Reversal,
    ctx: &TaskCtx,
) -> Result<Applied, StepError> {
    let bytes = copy_leaf(targets, from, to, entry, ctx)
        .await
        .map_err(StepError::from_provider)?;
    recorder
        .created(to, reversal)
        .await
        .map_err(StepError::Fatal)?;
    Ok(Applied::Wrote(bytes))
}

/// Deletes `path` and everything hanging off it, PERMANENTLY.
///
/// The entries inside are NOT journalled one by one: the step is ONE —"this
/// tree is no longer there"— and so is its entry, just like the trash
/// buries the tree in one piece.
async fn remove_tree(
    targets: &SyncTargets,
    path: &VPath,
    ctx: &TaskCtx,
) -> (u64, Result<(), Error>) {
    let provider = targets.dest.as_ref();
    // The `stat` isn't decorative and is the same one `ops::delete_task`
    // does: the walk starts with a `list`, and a `list` over a file is
    // `Conflict{TypeMismatch}`. A `DeleteTree` names the orphan entry,
    // which most of the time is a FILE — without this, `Mirror` against a
    // trash-less destination (a bucket, an SFTP) would answer "conflict"
    // for every extra file and delete none.
    // `removed_count` counts what's NO LONGER THERE, and is returned no
    // matter what — it's the only thing distinguishing "the tree was never
    // touched" from "the tree is half-destroyed", and whether to journal
    // hangs off that distinction (#186).
    let mut removed_count = 0u64;
    let entry = match stat(provider, path, ctx).await {
        Ok(Some(e)) => e,
        Ok(None) => return (removed_count, Err(Error::NotFound)),
        Err(e) => return (removed_count, Err(e)),
    };
    if entry.kind == EntryKind::Dir {
        // The walk emits every parent before its children, so traversing it
        // backward IS post-order and every directory reaches its `remove`
        // already empty.
        let entries = match crate::ops::walk(provider, path, &ctx.cancel).await {
            Ok(e) => e,
            Err(e) => return (removed_count, Err(e)),
        };
        for entry in entries.iter().rev() {
            if ctx.cancel.is_cancelled() {
                return (removed_count, Err(Error::Cancelled));
            }
            // Every node via the DESCRIPTOR when there's a root (#296), and
            // stating its kind: post-order reaches already-empty
            // directories and leaves, and `unlinkat` needs to know which of
            // the two it's deleting.
            match targets
                .dest_at(&entry.path)
                .remove_kind(entry.kind == EntryKind::Dir, &ctx.cancel)
                .await
            {
                Ok(()) => removed_count += 1,
                // A failure AFTER a transient leaves the node in doubt: the
                // `remove` may have reached the bucket and lost the
                // response. Counts as removed, because the error of
                // counting it is one extra row and the error of not
                // counting it is an object deleted forever with no row at
                // all (#186, security review MAJOR-1).
                Err((e, crate::ops::Ambiguity::MaybeApplied)) => {
                    return (removed_count + 1, Err(e));
                }
                Err((e, crate::ops::Ambiguity::NotApplied)) => return (removed_count, Err(e)),
            }
        }
    }
    // And the tree's root, the same way and stating its kind: the `stat`
    // above already said whether it's a directory or a lone leaf.
    match targets
        .dest_at(path)
        .remove_kind(entry.kind == EntryKind::Dir, &ctx.cancel)
        .await
    {
        Ok(()) => (removed_count + 1, Ok(())),
        Err((e, crate::ops::Ambiguity::MaybeApplied)) => (removed_count + 1, Err(e)),
        Err((e, crate::ops::Ambiguity::NotApplied)) => (removed_count, Err(e)),
    }
}

/// A deterministic trash id for ONE step, stable across every retry (#99).
///
/// **Per VICTIM and not per Task**, which is where this parts ways with
/// `ops::delete_task`: there a Task buries exactly one thing and the
/// `task_id` is enough as a counter. Here a Task buries hundreds, and a
/// LOGICAL trash names its folder with the id
/// (`.norte-trash/<ms>-<counter>/`) — with the counter fixed, two steps
/// buried in the same millisecond would collide, and the second would come
/// out as `Conflict{Exists}`, i.e. as a destination drift that never
/// happened. `SyncStep::id` is monotonic within ONE plan, which is exactly
/// the scope needed.
fn trash_id(step_id: u64) -> norte_vfs::trash::TrashId {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    norte_vfs::trash::TrashId::new(now_ms, step_id)
}

/// Destroys a LEAF however the destination allows: to the trash if it has
/// one, forever if not.
///
/// **That UNDO can't undo the step isn't a reason to deny the human their
/// trash.** A destination whose trash doesn't NAME what it buries leaves
/// the plan with no reversal —the journal ends up with no `reversal_ref`
/// and guessing would restore the wrong file— but the trash is still
/// there, and what's buried is fetched by hand from it. Permanently
/// deleting what could have been buried would be over-destroying over a
/// bookkeeping problem.
///
/// Doesn't journal: the caller writes the entry that's its job (a single
/// one, `Irreversible`).
async fn destroy_leaf(
    targets: &SyncTargets,
    to: &VPath,
    step_id: u64,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    if targets.delete_mode == norte_proto::DeleteMode::Trash {
        crate::ops::trash_retrying(targets.dest.as_ref(), to, &trash_id(step_id), &ctx.cancel)
            .await?;
        return Ok(());
    }
    // By DESCRIPTOR when the root is available (#296): we already had it in
    // hand and the delete still resolved the path. It's much better
    // covered than a copy —`revalidate` requires the witness's kind, size
    // and mtime to still match before destroying anything— but covered
    // isn't confined.
    targets
        .dest_at(to)
        .remove_kind(false, &ctx.cancel)
        .await
        .map_err(|(e, _)| e)
}

/// The same for a TREE: the trash takes it in one piece (which is why it
/// doesn't even need the walk), and with no trash it's walked in
/// post-order.
async fn destroy_tree(
    targets: &SyncTargets,
    to: &VPath,
    step_id: u64,
    ctx: &TaskCtx,
) -> (u64, Result<(), Error>) {
    if targets.delete_mode == norte_proto::DeleteMode::Trash {
        // The trash takes the tree in one piece: either zero nodes or
        // "this tree", which for the journal's purposes is ONE entry, same
        // as before.
        return match crate::ops::trash_retrying_amb(
            targets.dest.as_ref(),
            to,
            &trash_id(step_id),
            &ctx.cancel,
        )
        .await
        {
            Ok(_) => (1, Ok(())),
            // Same as above: a `trash` in doubt counts as taken.
            Err((e, crate::ops::Ambiguity::MaybeApplied)) => (1, Err(e)),
            Err((e, crate::ops::Ambiguity::NotApplied)) => (0, Err(e)),
        };
    }
    remove_tree(targets, to, ctx).await
}

/// Buries `to` in the trash and journals it.
///
/// # The row that doesn't arrive
/// If the journal fails AFTER the burial, the effect happened and its
/// record didn't (hard rule 4 broken live — #160). It's compensated:
/// `restore_from` returns what was buried to its path and the step dies
/// with the destination intact. Two limits, and both go to the log:
///
/// - `restore_from` can itself fail, and then the log line is all that's
///   left;
/// - with a trash that does NOT NAME what it takes (`DestTrash::Opaque`:
///   macOS, Windows) there's nowhere to point and no compensation is
///   possible.
///
/// `restore_from` inherits [`Provider::rename`]'s no-replace contract: a
/// provider that honors it to the letter fails with `Conflict` instead of
/// clobbering something that reached `to` in the window between the burial
/// and the compensation. `norte-vfs-sftp` documents that window as TOCTOU
/// (its `rename` checks then renames, it isn't atomic) — the same risk
/// [`crate::undo`] already runs when restoring a `Trashed` with
/// `reversal_ref`, of which this is just another caller.
///
/// **And when the compensation does NOT arrive**, the step comes out as
/// [`StepError::Unrecoverable`] and not as a plain `Fatal`: they're two
/// different tree states —one intact, the other with a file that moved
/// that the journal doesn't know about— and only the second can say where
/// to look. The buried path and its trash destination travel INSIDE the
/// error (rule 6), which is what #160 didn't have: they lived in `tracing`
/// fields, so "where's my file?" was only answered by whoever happened to
/// be reading the daemon's log at that instant.
///
/// **The other half of an `Overwrite`'s pair isn't compensated either, but
/// it's already SAID (#206).** A `created` that fails after a `trashed`
/// that DID land leaves a batch whose undo blocks: it goes down by `seq`
/// and would have to delete a file it has no row for before it can dig up
/// the other one. Returning it would require deleting the just-placed copy
/// AND digging up the old one — two more mutations on the path where the
/// journal already proved it doesn't work, and neither would end up
/// recorded either. So it isn't compensated: it comes out as
/// [`StepError::Unrecoverable`] with the buried path and its spot in the
/// trash inside, which is the difference between "it failed" and "your
/// file is here".
async fn bury(
    targets: &SyncTargets,
    recorder: &dyn StepJournal,
    to: &VPath,
    step_id: u64,
    ctx: &TaskCtx,
) -> Result<Option<VPath>, StepError> {
    let buried =
        crate::ops::trash_retrying(targets.dest.as_ref(), to, &trash_id(step_id), &ctx.cancel)
            .await
            .map_err(StepError::from_provider)?;
    if let Err(e) = recorder.trashed(to, buried.as_ref()).await {
        // Hard rule 4 in reverse: the effect happened and its row didn't.
        // The only thing that leaves the tree as it was is UNDOING it here,
        // and since task 11b it can be done: `trash()` returns the exact
        // path of what was buried and `restore_from` returns it to its
        // spot.
        //
        // Only when the trash NAMES what it took. With `DestTrash::Opaque`
        // (macOS, Windows) there's nothing to point to and the log line is
        // still the whole answer.
        let returned = match buried.as_ref() {
            Some(there) => targets.dest.restore_from(there, to).await,
            None => Err(Error::Unsupported),
        };
        // `reversal_ref` is the ONLY clue to where the file ended up, and
        // it just failed to land in the journal. It's written to the
        // operator's log before dying: without this, "where's my file?" is
        // answered by nobody. It ALSO states whether the compensation
        // landed — a `restore_from` that also fails leaves the file
        // buried, and the operator needs to know that in the same line.
        // The difference an `error!` alone couldn't make. If the
        // compensation landed, the tree is back to what it was and this is
        // a journal failure like any other: stated here and done. If not,
        // the file is somewhere else and nothing records it — that's a
        // STATE, not a failure, and comes out TYPED with the buried path
        // and its destination inside, so the caller can say so (and so a
        // test can check it, which is what #160 didn't have). That case is
        // NOT logged here: `ApplyError::into_wire` does it, once, when the
        // Task dies.
        if returned.is_ok() {
            tracing::error!(
                error = %e,
                buried = %crate::engine::span_path(to),
                at = buried.as_ref().map(crate::engine::span_path),
                "sync.apply: the destination was buried, its journal entry did NOT land, and it \
                 was returned to its spot",
            );
            return Err(StepError::Fatal(e));
        }
        return Err(StepError::Unrecoverable(Box::new(Unrecorded {
            buried: to.clone(),
            at: buried,
            source: e,
        })));
    }
    // Where it ended up, for whoever has to say so if the OTHER half of the
    // pair fails (#206).
    Ok(buried)
}

/// A reversal this core doesn't emit for this step class.
///
/// Unreachable: the transducer derives the reversal from the class and the
/// trash, and the spool refuses on reading a step whose shape doesn't hold
/// up. If it ever arrived, NOT destroying is the only answer.
fn unexpected_reversal(kind: SyncStepKind, reversal: Option<StepReversal>) -> StepError {
    tracing::error!(
        ?kind,
        ?reversal,
        "sync.apply: a step with a reversal this core does not emit"
    );
    StepError::Failed(Error::Conflict {
        conflict: ConflictKind::Unknown,
    })
}

/// `CreateDir`: creates the directory, or fails if something already
/// occupies the spot.
async fn create_dir(
    targets: &SyncTargets,
    recorder: &dyn StepJournal,
    to: &VPath,
    ctx: &TaskCtx,
) -> Result<Applied, StepError> {
    // Pre-stat: it's `mkdir_retrying`'s contract (without it it can't tell
    // its own ghost directory apart from someone else's after a transient
    // failure), and it doubles as this class's revalidation. Something
    // already there is NOT adopted: claiming someone else's directory as
    // ours would make undo send it to the trash with its contents.
    let pre = stat(targets.dest.as_ref(), to, ctx)
        .await
        .map_err(StepError::from_provider)?;
    if pre.is_some() {
        return Err(StepError::Failed(Error::Conflict {
            conflict: ConflictKind::Exists,
        }));
    }
    crate::ops::mkdir_retrying(&targets.dest_at(to), &ctx.cancel)
        .await
        .map_err(StepError::from_provider)?;
    recorder
        .created(to, Reversal::Delete)
        .await
        .map_err(StepError::Fatal)?;
    Ok(Applied::Wrote(0))
}

/// `Overwrite`: revalidate, look at the source, empty the destination and
/// copy.
///
/// That order, and no other: revalidating before destroying is what
/// protects against the TTL, and looking at the source before destroying is
/// what prevents leaving the path empty when the source is no longer there
/// (see [`source_leaf`]).
async fn overwrite(
    targets: &SyncTargets,
    record: &SpoolStep,
    recorder: &dyn StepJournal,
    to: &VPath,
    ctx: &TaskCtx,
) -> Result<Applied, StepError> {
    revalidate(targets.dest.as_ref(), to, record.dest, ctx)
        .await
        .map_err(StepError::from_provider)?;
    let (from, entry) = source_leaf(targets, record, ctx).await?;
    match record.step.reversal {
        Some(StepReversal::RestoreTrash) => {
            let at = bury(targets, recorder, to, record.step.id, ctx).await?;
            // By hand and not via `place` (#206): once `trashed` lands
            // written, a `created` that fails leaves a BATCH whose undo
            // blocks — undo goes down by `seq` and would have to delete a
            // file it has no row for before it can dig up the other one.
            // Compensating it would require two more mutations exactly on
            // the path where the journal already proved it doesn't work,
            // so it isn't compensated: it's STATED, with the buried path
            // and its spot in the trash inside the error, which is what
            // turns "couldn't be undone" into "this is here".
            let bytes = copy_leaf(targets, &from, to, &entry, ctx)
                .await
                .map_err(StepError::from_provider)?;
            if let Err(source) = recorder.created(to, Reversal::Delete).await {
                return Err(StepError::Unrecoverable(Box::new(Unrecorded {
                    buried: to.clone(),
                    at,
                    source,
                })));
            }
            Ok(Applied::Wrote(bytes))
        }
        Some(StepReversal::Irreversible) => {
            destroy_leaf(targets, to, record.step.id, ctx)
                .await
                .map_err(StepError::from_provider)?;
            // ONE entry, irreversible: see the module's note on why the
            // delete carries no entry of its own WHEN THE COPY LANDS.
            let placed = place(
                targets,
                &from,
                &entry,
                recorder,
                to,
                Reversal::Irreversible,
                ctx,
            )
            .await;
            if placed.is_err() {
                // And why it DOES carry one when the copy doesn't land: what
                // was there before is gone, the new one wasn't written, and
                // without this entry the destruction would have ended up
                // entirely outside the journal (hard rule 4 asks for an
                // entry or an explicit `Irreversible` classification; this
                // is both). The report names it, but the report belongs to
                // the Task and leaves with it.
                //
                // That entry failing is irreparable for the same reason as
                // in `delete_tree`: the delete was permanent. Comes out
                // typed.
                if let Err(e) = recorder.removed(to).await {
                    return Err(StepError::Unrecoverable(Box::new(Unrecorded {
                        buried: to.clone(),
                        at: None,
                        source: e,
                    })));
                }
            }
            placed
        }
        other => Err(unexpected_reversal(record.step.kind, other)),
    }
}

/// `DeleteTree`: revalidate and remove the tree in one piece, to the trash
/// or forever.
async fn delete_tree(
    targets: &SyncTargets,
    record: &SpoolStep,
    recorder: &dyn StepJournal,
    to: &VPath,
    ctx: &TaskCtx,
) -> Result<Applied, StepError> {
    revalidate(targets.dest.as_ref(), to, record.dest, ctx)
        .await
        .map_err(StepError::from_provider)?;
    match record.step.reversal {
        Some(StepReversal::RestoreTrash) => {
            bury(targets, recorder, to, record.step.id, ctx).await?;
            Ok(Applied::Wrote(0))
        }
        Some(StepReversal::Irreversible) => {
            let (removed_count, removed) = destroy_tree(targets, to, record.step.id, ctx).await;
            if removed_count > 0 && removed.is_err() {
                // Neither the journal nor the report can say "halfway":
                // the row says `removed <root>` and the report never gets a
                // row. The only way left to tell "no longer there" apart
                // from "dented" is stating it here.
                tracing::warn!(
                    removed_count,
                    path = %crate::engine::span_path(to),
                    "sync.apply: the tree was left HALF-deleted; its journal row states the \
                     root, not how much fell",
                );
            }
            // The entry is written even if the delete was left halfway:
            // "this tree is no longer whole" is an irreversible mutation
            // whether `remove` reached the end or died at file 40,000, and
            // without it the destroyed part is left outside the journal
            // (hard rule 4).
            //
            // **Cancellation is NOT the exception, and believing so was the
            // bug (#186).** `remove_tree` checks the token BETWEEN entries,
            // so by the time it returns `Cancelled` an arbitrary number of
            // nodes have already fallen — for a `Mirror` over a trash-less
            // destination, permanently. Skipping the row there left a
            // half-deleted subtree with no journal entry (no undo) and no
            // report row (the loop exits with `Err` on the spot), i.e. no
            // trace at all; and on top of that three frontends assert in
            // prose that what applies gets journalled.
            //
            // What decides is `removed_count`, not the error's class: zero
            // nodes means "the tree was never touched" —the revalidation
            // that already exited above, a `walk` that failed, a
            // cancellation before the first delete— and that one really
            // doesn't deserve an entry.
            if removed_count > 0 {
                // And if THIS row doesn't land either, it's the same kind
                // of breakdown as `bury` but worse: what was deleted is
                // permanent and no compensation is possible, so the error
                // has to carry at least the path. `at: None` because
                // there's no trash to point to — that's what `Irreversible`
                // means here.
                if let Err(e) = recorder.removed(to).await {
                    return Err(StepError::Unrecoverable(Box::new(Unrecorded {
                        buried: to.clone(),
                        at: None,
                        source: e,
                    })));
                }
            }
            removed.map_err(StepError::from_provider)?;
            Ok(Applied::Wrote(0))
        }
        other => Err(unexpected_reversal(record.step.kind, other)),
    }
}

/// The policy, asked about the REAL path of a step about to act.
///
/// It's asked about what the step DOES: creating a directory is `mkdir`,
/// copying is `copy`, and an overwrite is both —it destroys what was there
/// and writes over it— so it goes through both gates.
fn gate_step(
    targets: &SyncTargets,
    kind: SyncStepKind,
    to: &VPath,
    actor: &Actor,
) -> Result<(), StepError> {
    use crate::policy::PolicyOp;
    let del = PolicyOp::Delete {
        mode: targets.delete_mode,
    };
    let ops: &[PolicyOp] = match kind {
        SyncStepKind::CreateDir => &[PolicyOp::Mkdir],
        SyncStepKind::Copy => &[PolicyOp::Copy],
        SyncStepKind::Overwrite => &[del, PolicyOp::Copy],
        SyncStepKind::DeleteTree => &[del],
        // A `Skip` doesn't act and an unknown class never gets to act.
        _ => &[],
    };
    for op in ops {
        targets.allows(*op, to, actor).map_err(StepError::Failed)?;
    }
    Ok(())
}

/// Executes ONE step.
async fn execute(
    targets: &SyncTargets,
    record: &SpoolStep,
    recorder: &dyn StepJournal,
    ctx: &TaskCtx,
) -> Result<Applied, StepError> {
    let to = dest_path(targets, record);
    if record.step.kind != SyncStepKind::Skip {
        // The destination root is NOT a step. The transducer already
        // prevents this (`SyncError::RootIsNotAStep`) and `rel` enters the
        // `plan_hash`, so this isn't reachable today; the invariant is
        // checked HERE because this is where a failure means "the whole
        // destination tree got deleted" and the guard lives in another
        // crate.
        if to == targets.dest_root {
            tracing::error!("sync.apply: an acting step names the destination root");
            return Err(StepError::Failed(Error::InvalidPath));
        }
        gate_step(targets, record.step.kind, &to, &ctx.actor)?;
    }
    match record.step.kind {
        SyncStepKind::Skip => Ok(Applied::Skipped),
        SyncStepKind::CreateDir => create_dir(targets, recorder, &to, ctx).await,
        SyncStepKind::Copy => {
            let (from, entry) = source_leaf(targets, record, ctx).await?;
            place(targets, &from, &entry, recorder, &to, Reversal::Delete, ctx).await
        }
        SyncStepKind::Overwrite => overwrite(targets, record, recorder, &to, ctx).await,
        SyncStepKind::DeleteTree => delete_tree(targets, record, recorder, &to, ctx).await,
        // The spool refuses on reading a step of unknown class, so this
        // isn't reachable from a file we wrote ourselves. `SyncStepKind` is
        // `#[non_exhaustive]`, so the wildcard is mandatory, and falling on
        // the side of NOT touching anything is the only safe choice: a
        // class this binary can't name, it can't undo either.
        _ => Err(StepError::Failed(Error::Unsupported)),
    }
}

/// Records a failed step. The LIST has a cap; the COUNTER doesn't.
fn record_failure(report: &Mutex<SyncReportResult>, step: &SyncStep, cause: SyncFailureCause) {
    // INVARIANT: the `Mutex` only gets poisoned if another thread panicked
    // holding it, which is unrecoverable — the same criterion as the rest
    // of the core's locks.
    let mut report = report.lock().expect("sync report lock");
    report.failed = report.failed.saturating_add(1);
    if report.failures.len() < SYNC_MAX_FAILURES_REPORTED {
        report.failures.push(SyncFailure {
            rel: step.rel.clone(),
            // The step's CLASS, which the core has right in front of it and
            // used to drop until 0.41.0 (#195). It's what tells which root
            // `rel` hangs off of —a `DeleteTree` always talks about the
            // destination— without whoever reads the report, who has no
            // plan, having to deduce it from `dest_rel`'s presence.
            kind: step.kind,
            // The DESTINATION's spelling travels with the failure: without
            // it, `IllegalName`'s star case —a name that blows past
            // `NAME_MAX` when recomposed in NFD— would be shown with the
            // source's spelling, which is the short, legal one.
            dest_rel: step.dest_rel.clone(),
            cause,
        });
    }
}

/// Executes the plan: one step after another, IN THE ORDER IT ARRIVES.
///
/// The walk is pre-order, so a `CreateDir` always precedes every copy
/// inside it: **nothing gets sorted**.
///
/// Cancellation is checked BETWEEN steps (hard rule 3), and it's also seen
/// by the `ops` inside a step —a GiB copy doesn't wait to finish—. What
/// applied stays journalled under its batch; it is NOT unwound (see the
/// module's note).
///
/// **What the report does NOT distinguish.** A `Conflict` row can be
/// "nothing was touched" (revalidation caught it in time) or "the
/// destination was buried and the copy didn't land", and the action that
/// falls to the user isn't the same: check the trash, or re-plan.
/// Distinguishing them costs one more wire cause, a closed
/// daemon→client vocabulary; today the daemon's log says it.
///
/// `steps` can fail HALFWAY, with steps already executed: a truncated or
/// edited spool. It isn't the same as a step that fails —there's no way to
/// know what came next, so there's nothing to record as a row— and the
/// answer is stopping the Task. The batch is left closed and undoable at
/// that point, which is what matters.
///
/// # Errors
/// [`Error::Cancelled`], the journal's error, or whatever the step stream
/// brought. A step that fails does NOT come out here: it comes out in
/// `report`.
///
/// # Panics
/// Only if the report's `Mutex` is poisoned (another thread panicked
/// holding it), which is the same thing the rest of the core does with its
/// locks.
#[tracing::instrument(
    skip_all,
    fields(
        task_id = ctx.progress.snapshot().task_id.get(),
        dest = %crate::engine::span_path(&targets.dest_root),
    )
)]
pub(crate) async fn run<S>(
    targets: SyncTargets,
    recorder: &dyn StepJournal,
    steps: S,
    ctx: &TaskCtx,
    report: &Mutex<SyncReportResult>,
) -> Result<(), ApplyError>
where
    S: Stream<Item = Result<SpoolStep, Error>>,
{
    if ctx.cancel.is_cancelled() {
        return Err(ApplyError::Stopped(Error::Cancelled));
    }
    // The destination root is opened HERE, once for the whole Task (#164):
    // that's why `targets` comes in by value, and not borrowed like
    // everything else. Not being able to open a destination that said it
    // knew how to confine stops the WHOLE Task, and before touching
    // anything — it isn't a report row, it's that the defense the human
    // saw advertised isn't there.
    let targets = &targets
        .with_dest_confined(ctx.progress.snapshot().task_id.get())
        .await
        .map_err(ApplyError::Stopped)?;
    // And that the opened root is the path that was requested, before
    // touching anything (#368): between resolving it and opening it there's
    // the usual window.
    targets
        .dest_still_standing(&ctx.cancel)
        .await
        .map_err(ApplyError::Stopped)?;
    let mut steps = std::pin::pin!(steps);
    let mut done_count: usize = 0;
    let mut last_check = std::time::Instant::now();
    loop {
        if ctx.cancel.is_cancelled() {
            return Err(ApplyError::Stopped(Error::Cancelled));
        }
        let Some(next) = steps.next().await else {
            // And one last time BEFORE saying it went well, whatever the
            // `stat` costs: the destination going away with the last step
            // in flight is exactly what the periodic check skips, and it's
            // the one moment where lying closes everything out.
            //
            // Unless no step was ever executed, in which case the one
            // above just ran with nothing in between. Same `i > 0`
            // `copy_tree` had to add: an empty plan paid the check twice
            // and exposed itself once more to a false positive, having
            // written nothing to protect.
            if done_count == 0 {
                return Ok(());
            }
            return targets
                .dest_still_standing(&ctx.cancel)
                .await
                .map_err(ApplyError::Stopped);
        };
        // Same cadence as the copy, and for the same reason: every 32
        // steps, or on the first step to start after 5 seconds have
        // passed. The second trigger is what makes a plan of four huge
        // files get checked BETWEEN them and not just at the start and
        // end.
        //
        // Which isn't a five-second ceiling: this is checked once per
        // STEP, so a fifty-gigabyte file copies whole before anyone looks
        // again. What neither form allows is saying it went well, which is
        // what the final check is for.
        if done_count > 0
            && (done_count.is_multiple_of(crate::ops::CHECK_ROOT_EACH)
                || last_check.elapsed().as_secs() >= crate::ops::CHECK_ROOT_EVERY_SECONDS)
        {
            targets
                .dest_still_standing(&ctx.cancel)
                .await
                .map_err(ApplyError::Stopped)?;
            last_check = std::time::Instant::now();
        }
        done_count = done_count.saturating_add(1);
        let record = match next {
            Ok(record) => record,
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "sync.apply: the plan stopped being readable mid-execution"
                );
                return Err(ApplyError::Stopped(e));
            }
        };
        match execute(targets, &record, recorder, ctx).await {
            Ok(Applied::Wrote(bytes)) => {
                let mut report = report.lock().expect("sync report lock");
                report.done = report.done.saturating_add(1);
                report.bytes = report.bytes.saturating_add(bytes);
            }
            Ok(Applied::Skipped) => {
                let mut report = report.lock().expect("sync report lock");
                report.skipped = report.skipped.saturating_add(1);
            }
            Err(StepError::Failed(e)) => {
                let cause = cause_of(&e);
                tracing::debug!(?cause, "sync.apply: a step did not happen");
                record_failure(report, &record.step, cause);
            }
            // Cancellation and the journal stop the Task. The journal,
            // because continuing would produce more effects outside it
            // (hard rule 4); cancellation, because that's what was asked
            // for.
            Err(StepError::Fatal(e)) => return Err(ApplyError::Stopped(e)),
            // And this one stops for the same reason, but carrying WHERE TO
            // LOOK: the destination is buried, its row didn't arrive and
            // returning it failed.
            //
            // With a REPORT ROW, which is the half #160 asked for by name
            // ("report the step as failed rather than leaving the task in
            // an unstated state"). Without it the report comes out `done:
            // 0, failed: 0` when the breakdown is on the first step, every
            // frontend paints zeros and the human reads "nothing happened"
            // over a file that's in the trash. The daemon's log doesn't
            // reach anyone who isn't watching it.
            Err(StepError::Unrecoverable(u)) => {
                record_failure(report, &record.step, SyncFailureCause::Io);
                return Err(ApplyError::Unrecorded(u));
            }
        }
        ctx.progress.update(|p| p.entries_done += 1);
    }
}

/// The path a step reads from the SOURCE, for the gate and for the tests.
#[cfg(test)]
fn source_path(targets: &SyncTargets, record: &SpoolStep) -> VPath {
    under(&targets.source_root, &record.step.rel)
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use norte_proto::methods::{
        CompareConfidence, CompareCriterion, StepReversal, SyncStep, SyncStepKind,
    };
    use norte_proto::{CapabilityFlags, TaskKind};
    use norte_testkit::MemProvider;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::progress::ProgressReporter;

    fn vp(wire: &str) -> VPath {
        VPath::parse(wire).expect("wire")
    }

    fn rel(wire: &str) -> RelPath {
        RelPath::parse_wire(wire).expect("rel")
    }

    fn ctx(cancel: CancellationToken) -> TaskCtx {
        TaskCtx {
            pause: crate::scheduler::PauseGate::default(),
            cancel,
            progress: Arc::new(
                ProgressReporter::new(norte_proto::TaskId::new(1), TaskKind::Sync).0,
            ),
            actor: Actor::User,
        }
    }

    async fn write(mem: &MemProvider, wire: &str, content: &[u8]) {
        let mut sink = mem.write(&vp(wire)).await.expect("write");
        sink.write(Bytes::copy_from_slice(content))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");
    }

    async fn read(mem: &MemProvider, wire: &str) -> Vec<u8> {
        let mut stream = mem.read(&vp(wire), None).await.expect("read");
        let mut out = Vec::new();
        while let Some(chunk) = stream.next().await {
            out.extend_from_slice(&chunk.expect("chunk"));
        }
        out
    }

    /// A recorder that only notes what it's asked: the real journal tests
    /// live in the integration file, with `SQLite` behind them.
    /// A recorded entry: op, path, reversal and trash reference.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Recorded {
        op: &'static str,
        path: String,
        reversal: Reversal,
        reversal_ref: Option<String>,
    }

    #[derive(Default)]
    struct Recorder {
        entries: Mutex<Vec<Recorded>>,
    }

    #[async_trait]
    impl StepJournal for Recorder {
        async fn created(&self, path: &VPath, reversal: Reversal) -> Result<(), Error> {
            self.entries.lock().expect("lock").push(Recorded {
                op: "created",
                path: path.to_wire(),
                reversal,
                reversal_ref: None,
            });
            Ok(())
        }
        async fn trashed(&self, path: &VPath, dest: Option<&VPath>) -> Result<(), Error> {
            self.entries.lock().expect("lock").push(Recorded {
                op: "trashed",
                path: path.to_wire(),
                reversal: Reversal::RestoreTrash,
                reversal_ref: dest.map(VPath::to_wire),
            });
            Ok(())
        }
        async fn removed(&self, path: &VPath) -> Result<(), Error> {
            self.entries.lock().expect("lock").push(Recorded {
                op: "removed",
                path: path.to_wire(),
                reversal: Reversal::Irreversible,
                reversal_ref: None,
            });
            Ok(())
        }
    }

    /// A recorder that fails EXACTLY on `trashed`, which is #160's weapon:
    /// the trash took the file and the row didn't arrive.
    #[derive(Default)]
    struct TrashedFails;

    #[async_trait]
    impl StepJournal for TrashedFails {
        async fn created(&self, _path: &VPath, _reversal: Reversal) -> Result<(), Error> {
            Ok(())
        }
        async fn trashed(&self, _path: &VPath, _dest: Option<&VPath>) -> Result<(), Error> {
            Err(Error::Io { retryable: false })
        }
        async fn removed(&self, _path: &VPath) -> Result<(), Error> {
            Ok(())
        }
    }

    /// A recorder that fails EXACTLY on `created`, which is #206's weapon:
    /// the `trashed` landed written and its pair didn't.
    #[derive(Default)]
    struct CreatedFails;

    #[async_trait]
    impl StepJournal for CreatedFails {
        async fn created(&self, _path: &VPath, _reversal: Reversal) -> Result<(), Error> {
            Err(Error::Io { retryable: false })
        }
        async fn trashed(&self, _path: &VPath, _dest: Option<&VPath>) -> Result<(), Error> {
            Ok(())
        }
        async fn removed(&self, _path: &VPath) -> Result<(), Error> {
            Ok(())
        }
    }

    fn step(kind: SyncStepKind, rel_wire: &str, reversal: Option<StepReversal>) -> SyncStep {
        SyncStep {
            id: 1,
            kind,
            rel: rel(rel_wire),
            dest_rel: None,
            size: None,
            criterion: CompareCriterion::Presence,
            confidence: CompareConfidence::Certain,
            reversal,
            reason: reversal
                .filter(|r| *r == StepReversal::Irreversible)
                .map(|_| norte_proto::methods::SyncReason::NoTrashOnTarget),
        }
    }

    /// An `Overwrite` step with trash over `name`, with the witness
    /// revalidation requires taken from the REAL destination.
    async fn overwrite_step(mem: &Arc<MemProvider>, name: &str) -> SpoolStep {
        let entry = mem
            .stat(&vp(&format!("mem:///d/{name}")))
            .await
            .expect("destination stat");
        SpoolStep {
            step: step(
                SyncStepKind::Overwrite,
                name,
                Some(StepReversal::RestoreTrash),
            ),
            dest: Some(DestWitness::of(&entry)),
        }
    }

    fn targets(mem: &Arc<MemProvider>) -> SyncTargets {
        SyncTargets {
            source: Arc::clone(mem) as Arc<dyn Provider>,
            dest: Arc::clone(mem) as Arc<dyn Provider>,
            source_root: vp("mem:///s"),
            dest_root: vp("mem:///d"),
            policy: Arc::new(crate::policy::AllowAll),
            delete_mode: norte_proto::DeleteMode::Trash,
            dest_confined: None,
        }
    }

    /// `dest_rel` rules over `rel`: it writes over the file that EXISTS,
    /// not the one the source spells. It's the half of #152 that is
    /// closed, and without it an `Overwrite` of an NFC `café` against an
    /// NFD `café` would create a SECOND file on ext4 next to the one meant
    /// to be overwritten.
    #[tokio::test]
    async fn the_destination_is_named_by_dest_rel_when_present() {
        let mem = Arc::new(MemProvider::new());
        let t = targets(&mem);
        let mut s = step(
            SyncStepKind::Copy,
            "caf%C3%A9.txt",
            Some(StepReversal::Delete),
        );
        s.dest_rel = Some(rel("cafe%CC%81.txt"));
        let record = SpoolStep {
            step: s,
            dest: None,
        };
        assert_eq!(dest_path(&t, &record).to_wire(), "mem:///d/cafe\u{301}.txt");
        assert_eq!(source_path(&t, &record).to_wire(), "mem:///s/caf\u{e9}.txt");
    }

    /// #176: a `DeleteTree` also looks at HOW MANY things are inside.
    ///
    /// A directory's `stat` only moves when its DIRECT children change —and
    /// in `MemProvider` not even that—, so without the count a subtree that
    /// gained files between approving and applying used to revalidate clean
    /// and get deleted whole: the step with the largest blast radius, with
    /// the loosest check.
    #[tokio::test]
    async fn a_tree_delete_counts_its_first_level() {
        let mem = Arc::new(MemProvider::new());
        let c = ctx(CancellationToken::new());
        mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
        mem.mkdir(&vp("mem:///d/sub")).await.expect("mkdir");
        write(&mem, "mem:///d/sub/a.txt", b"a").await;

        let entry = mem.stat(&vp("mem:///d/sub")).await.expect("stat");
        let snapshot = DestWitness::of(&entry).with_entries(Some(1));
        // As planned: one entry inside.
        revalidate(mem.as_ref(), &vp("mem:///d/sub"), Some(snapshot), &c)
            .await
            .expect("nothing changed");

        // Someone drops something in while the human decides.
        write(&mem, "mem:///d/sub/b.txt", b"b").await;
        let err = revalidate(mem.as_ref(), &vp("mem:///d/sub"), Some(snapshot), &c)
            .await
            .expect_err("the tree is no longer what was approved");
        assert_eq!(cause_of(&err), SyncFailureCause::Conflict);

        // And a witness with NO count can't declare a conflict over this:
        // it's "not known", never "zero" (same criterion as size).
        let no_count = DestWitness::of(&entry);
        revalidate(mem.as_ref(), &vp("mem:///d/sub"), Some(no_count), &c)
            .await
            .expect("with no count, there's nothing to compare");
    }

    /// Revalidation looks at what BOTH snapshots carry. A size that moved
    /// is a conflict; a witness with no size cannot be one (`file://` lists
    /// that way by default and would reject the whole plan).
    #[tokio::test]
    async fn revalidation_only_compares_what_both_snapshots_carry() {
        let mem = Arc::new(MemProvider::new());
        mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
        write(&mem, "mem:///d/a.txt", b"12345").await;
        let c = ctx(CancellationToken::new());
        let entry = mem.stat(&vp("mem:///d/a.txt")).await.expect("stat");

        let same = DestWitness::of(&entry);
        revalidate(mem.as_ref(), &vp("mem:///d/a.txt"), Some(same), &c)
            .await
            .expect("has not changed");

        let other_size = DestWitness {
            size: Some(99),
            ..same
        };
        let err = revalidate(mem.as_ref(), &vp("mem:///d/a.txt"), Some(other_size), &c)
            .await
            .expect_err("changed size");
        assert_eq!(cause_of(&err), SyncFailureCause::Conflict);

        let no_measurements = DestWitness {
            kind: entry.kind,
            size: None,
            mtime_ms: None,
            entries: None,
        };
        revalidate(
            mem.as_ref(),
            &vp("mem:///d/a.txt"),
            Some(no_measurements),
            &c,
        )
        .await
        .expect("a provider that doesn't measure produces no conflicts");

        let other_kind = DestWitness {
            kind: EntryKind::Dir,
            ..same
        };
        let err = revalidate(mem.as_ref(), &vp("mem:///d/a.txt"), Some(other_kind), &c)
            .await
            .expect_err("changed kind");
        assert_eq!(cause_of(&err), SyncFailureCause::Conflict);
    }

    /// The report's taxonomy, and in particular the cause that exists
    /// because a name's legality under the destination is NOT validated at
    /// planning time.
    #[test]
    fn each_error_falls_into_its_own_cause() {
        assert_eq!(
            cause_of(&Error::InvalidPath),
            SyncFailureCause::IllegalName,
            "a name the destination rejects has its own name"
        );
        assert_eq!(cause_of(&Error::PermissionDenied), SyncFailureCause::Denied);
        assert_eq!(
            cause_of(&Error::PolicyDenied {
                rule: "out-of-scope".to_owned()
            }),
            SyncFailureCause::Denied,
            "\"you can't\" and \"that can't be named\" lead to different actions"
        );
        assert_eq!(cause_of(&Error::NotFound), SyncFailureCause::Conflict);
        assert_eq!(
            cause_of(&Error::Io { retryable: true }),
            SyncFailureCause::Io
        );
    }

    /// A destructive step with NO witness is refused. The witness doesn't
    /// enter the `plan_hash`, so deleting it from the spool is exactly the
    /// edit the digest doesn't see; requiring it is what makes it have no
    /// effect.
    #[tokio::test]
    async fn a_destructive_step_with_no_witness_destroys_nothing() {
        let mem = Arc::new(MemProvider::new());
        mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
        write(&mem, "mem:///d/a.txt", b"12345").await;
        let c = ctx(CancellationToken::new());
        let err = revalidate(mem.as_ref(), &vp("mem:///d/a.txt"), None, &c)
            .await
            .expect_err("with no snapshot, nothing is destroyed");
        assert_eq!(cause_of(&err), SyncFailureCause::Conflict);
    }

    /// And a destination that's no longer there is also a conflict, not a
    /// bare error: it's what keeps a `DeleteTree` whose tree someone
    /// already deleted from counting as a breakdown.
    #[tokio::test]
    async fn a_destination_that_disappeared_is_a_conflict() {
        let mem = Arc::new(MemProvider::new());
        let c = ctx(CancellationToken::new());
        let entry = Entry {
            path: vp("mem:///d/not-there"),
            kind: EntryKind::File,
            size: None,
            mtime_ms: None,
            attrs: std::collections::BTreeMap::default(),
        };
        let err = revalidate(
            mem.as_ref(),
            &vp("mem:///d/not-there"),
            Some(DestWitness::of(&entry)),
            &c,
        )
        .await
        .expect_err("not there");
        assert_eq!(cause_of(&err), SyncFailureCause::Conflict);
    }

    /// A trash-less `Overwrite` leaves ONE irreversible `created` entry, and
    /// not a `removed`+`created` pair: with the pair, the batch's undo
    /// would delete the new file without being able to restore the old
    /// one and would leave the path empty.
    #[tokio::test]
    async fn a_trash_less_overwrite_is_one_irreversible_entry() {
        // `MemProvider` with no `TRASH`, which is what it declares by default.
        let mem = Arc::new(MemProvider::with_flags(CapabilityFlags::CASE_SENSITIVE));
        mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
        mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
        write(&mem, "mem:///s/a.txt", b"new").await;
        write(&mem, "mem:///d/a.txt", b"old").await;
        let entry = mem.stat(&vp("mem:///d/a.txt")).await.expect("stat");

        // With no trash on the destination, the delete is PERMANENT: it's
        // what `engine` derives from the capabilities and what the gate
        // authorized.
        let t = SyncTargets {
            delete_mode: norte_proto::DeleteMode::Permanent,
            ..targets(&mem)
        };
        let recorder = Recorder::default();
        let record = SpoolStep {
            step: step(
                SyncStepKind::Overwrite,
                "a.txt",
                Some(StepReversal::Irreversible),
            ),
            dest: Some(DestWitness::of(&entry)),
        };
        let report = Mutex::new(new_report(7, DestTrash::Restorable));
        run(
            t,
            &recorder,
            futures::stream::iter(vec![Ok(record)]),
            &ctx(CancellationToken::new()),
            &report,
        )
        .await
        .expect("the application finishes");

        assert_eq!(read(&mem, "mem:///d/a.txt").await, b"new");
        let entries = recorder.entries.lock().expect("lock").clone();
        assert_eq!(entries.len(), 1, "a single entry: {entries:?}");
        assert_eq!(entries[0].op, "created");
        assert_eq!(entries[0].reversal, Reversal::Irreversible);
        assert_eq!(report.lock().expect("lock").done, 1);
    }

    /// **Irreversible does NOT mean "permanently deleted".** A destination
    /// whose trash does NOT NAME what it buries produces irreversible
    /// steps —undo can't get it right— but the trash still exists, and
    /// what was overwritten has to end up inside it: the human fetches it
    /// by hand. Deleting it forever would be over-destroying over a
    /// bookkeeping problem.
    #[tokio::test]
    async fn an_irreversible_step_with_trash_buries_instead_of_deleting() {
        let mem = Arc::new(
            MemProvider::with_flags(CapabilityFlags::CASE_SENSITIVE | CapabilityFlags::TRASH)
                .with_logical_trash(),
        );
        mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
        mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
        write(&mem, "mem:///s/a.txt", b"new").await;
        write(&mem, "mem:///d/a.txt", b"old").await;
        let entry = mem.stat(&vp("mem:///d/a.txt")).await.expect("stat");

        let t = targets(&mem);
        let recorder = Recorder::default();
        let record = SpoolStep {
            step: step(
                SyncStepKind::Overwrite,
                "a.txt",
                Some(StepReversal::Irreversible),
            ),
            dest: Some(DestWitness::of(&entry)),
        };
        let report = Mutex::new(new_report(7, DestTrash::Restorable));
        run(
            t,
            &recorder,
            futures::stream::iter(vec![Ok(record)]),
            &ctx(CancellationToken::new()),
            &report,
        )
        .await
        .expect("the application finishes");

        assert_eq!(read(&mem, "mem:///d/a.txt").await, b"new");
        // There's still ONE irreversible entry (the journal promises no undo)…
        let entries = recorder.entries.lock().expect("lock").clone();
        assert_eq!(entries.len(), 1, "a single entry: {entries:?}");
        assert_eq!(entries[0].reversal, Reversal::Irreversible);
        // …and yet the old one is in the trash, not annihilated.
        assert!(
            mem.stat(&vp("mem:///.norte-trash")).await.is_ok(),
            "what was overwritten was buried instead of deleted forever"
        );
    }

    /// A stream that breaks HALFWAY isn't a step that fails: what already
    /// ran stays (and journalled), what came after is unknown, and the
    /// Task stops.
    #[tokio::test]
    async fn a_plan_that_stops_being_readable_halfway_stops_the_task_with_no_row() {
        let mem = Arc::new(MemProvider::new());
        mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
        mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
        write(&mem, "mem:///s/a.txt", b"x").await;
        let t = targets(&mem);
        let recorder = Recorder::default();
        let report = Mutex::new(new_report(7, DestTrash::Restorable));
        let stream = futures::stream::iter(vec![
            Ok(SpoolStep {
                step: step(SyncStepKind::Copy, "a.txt", Some(StepReversal::Delete)),
                dest: None,
            }),
            Err(Error::Io { retryable: false }),
        ]);
        let err = run(
            t,
            &recorder,
            stream,
            &ctx(CancellationToken::new()),
            &report,
        )
        .await
        .expect_err("the plan stopped being readable");
        assert!(
            matches!(err, ApplyError::Stopped(Error::Io { .. })),
            "{err:?}"
        );

        let (done, failed) = {
            let report = report.lock().expect("lock");
            (report.done, report.failed)
        };
        assert_eq!(done, 1, "what applied stays");
        assert_eq!(failed, 0, "no step to attribute it to");
        assert_eq!(read(&mem, "mem:///d/a.txt").await, b"x");
        assert_eq!(recorder.entries.lock().expect("lock").len(), 1);
    }

    /// #160: if the journal row does NOT arrive AFTER the destination was
    /// buried, the file has moved and gone unrecorded — hard rule 4 broken
    /// live. The compensation is fetching it back from the trash: the step
    /// fails, the Task stops, and the destination is left with its
    /// original bytes.
    #[tokio::test]
    async fn a_journal_that_fails_after_burying_returns_the_file_to_its_spot() {
        let mem = Arc::new(MemProvider::new().with_logical_trash());
        mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
        mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
        write(&mem, "mem:///d/a.txt", b"old").await;
        let t = targets(&mem);

        let err = super::bury(
            &t,
            &TrashedFails,
            &vp("mem:///d/a.txt"),
            1,
            &ctx(CancellationToken::new()),
        )
        .await
        .expect_err("the journal failed");
        assert!(
            matches!(err, StepError::Fatal(Error::Io { .. })),
            "the journal failure stops the Task: {err:?}"
        );

        assert_eq!(
            read(&mem, "mem:///d/a.txt").await,
            b"old",
            "the destination came back from the trash with its bytes"
        );
    }

    /// A `MemProvider` that CANCELS as soon as it has deleted something.
    ///
    /// This is what makes #186's test deterministic: `remove_tree` checks
    /// the token BETWEEN entries, so "cancelled halfway" only reproduces if
    /// the token falls between two `remove`s. With a task that cancels on a
    /// clock, that's a race, and a race in the suite is a test that goes
    /// red one day and says nothing.
    struct CancelsOnDelete {
        inner: Arc<MemProvider>,
        cancel: CancellationToken,
    }

    #[async_trait]
    impl Provider for CancelsOnDelete {
        fn scheme(&self) -> &str {
            self.inner.scheme()
        }
        fn capabilities(&self) -> norte_proto::Capabilities {
            self.inner.capabilities()
        }
        async fn stat(&self, p: &VPath) -> Result<Entry, Error> {
            self.inner.stat(p).await
        }
        async fn list(&self, p: &VPath) -> Result<norte_vfs::EntryStream, Error> {
            self.inner.list(p).await
        }
        async fn read(
            &self,
            p: &VPath,
            range: Option<norte_proto::ByteRange>,
        ) -> Result<norte_vfs::ByteStream, Error> {
            self.inner.read(p, range).await
        }
        async fn write(&self, p: &VPath) -> Result<Box<dyn norte_vfs::ByteSink>, Error> {
            self.inner.write(p).await
        }
        async fn mkdir(&self, p: &VPath) -> Result<(), Error> {
            self.inner.mkdir(p).await
        }
        async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), Error> {
            self.inner.rename(from, to).await
        }
        /// Really deletes, and THEN cancels: on return, the tree is already
        /// half-done and the token is already dropped.
        async fn remove(&self, p: &VPath) -> Result<(), Error> {
            self.inner.remove(p).await?;
            self.cancel.cancel();
            Ok(())
        }
    }

    /// A `MemProvider` whose `remove` APPLIES the effect and answers a
    /// transient failure, cancelling along the way.
    ///
    /// This is a remote's "timeout after commit" (issue #17) caught by a
    /// cancellation: the object is no longer in the bucket, the response
    /// got lost, and the user —who's been staring at the stall for a
    /// while— presses `Ctrl+K`. Without this there's no deterministic way
    /// to reach that corner.
    struct DeletesAndLies {
        inner: Arc<MemProvider>,
        cancel: CancellationToken,
    }

    #[async_trait]
    impl Provider for DeletesAndLies {
        fn scheme(&self) -> &str {
            self.inner.scheme()
        }
        fn capabilities(&self) -> norte_proto::Capabilities {
            self.inner.capabilities()
        }
        async fn stat(&self, p: &VPath) -> Result<Entry, Error> {
            self.inner.stat(p).await
        }
        async fn list(&self, p: &VPath) -> Result<norte_vfs::EntryStream, Error> {
            self.inner.list(p).await
        }
        async fn read(
            &self,
            p: &VPath,
            range: Option<norte_proto::ByteRange>,
        ) -> Result<norte_vfs::ByteStream, Error> {
            self.inner.read(p, range).await
        }
        async fn write(&self, p: &VPath) -> Result<Box<dyn norte_vfs::ByteSink>, Error> {
            self.inner.write(p).await
        }
        async fn mkdir(&self, p: &VPath) -> Result<(), Error> {
            self.inner.mkdir(p).await
        }
        async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), Error> {
            self.inner.rename(from, to).await
        }
        async fn remove(&self, p: &VPath) -> Result<(), Error> {
            self.inner.remove(p).await?;
            self.cancel.cancel();
            Err(Error::Io { retryable: true })
        }
    }

    /// **#186 through the other door, the one the fix's first version left
    /// open.** A `remove` that applies and dies on a transient counts as
    /// removed.
    ///
    /// The object is no longer in the bucket and the response got lost; if
    /// that's the FIRST node of the post-order, counting it as "not
    /// removed" leaves the tree dented, deleted forever, and with no
    /// journal row — which is exactly the hole, reached with no cancel
    /// between entries. The whole tree resolves the doubt toward "we did
    /// it": that choice's error is one extra row, the opposite one's is an
    /// effect with no row.
    #[tokio::test]
    async fn an_ambiguous_delete_counts_as_deleted() {
        let mem = Arc::new(MemProvider::with_flags(CapabilityFlags::CASE_SENSITIVE));
        mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
        mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
        mem.mkdir(&vp("mem:///d/sub")).await.expect("mkdir");
        write(&mem, "mem:///d/sub/a.txt", b"one").await;
        let entry = mem.stat(&vp("mem:///d/sub")).await.expect("stat");

        let cancel = CancellationToken::new();
        let dest = Arc::new(DeletesAndLies {
            inner: Arc::clone(&mem),
            cancel: cancel.clone(),
        });
        let t = SyncTargets {
            dest: dest as Arc<dyn Provider>,
            delete_mode: norte_proto::DeleteMode::Permanent,
            ..targets(&mem)
        };
        let recorder = Recorder::default();
        let err = delete_tree(
            &t,
            &SpoolStep {
                step: step(
                    SyncStepKind::DeleteTree,
                    "sub",
                    Some(StepReversal::Irreversible),
                ),
                dest: Some(DestWitness::of(&entry)),
            },
            &recorder,
            &vp("mem:///d/sub"),
            &ctx(cancel),
        )
        .await
        .expect_err("remove died on a transient");
        assert!(
            matches!(err, StepError::Failed(Error::Io { .. })),
            "{err:?}"
        );

        assert!(
            mem.stat(&vp("mem:///d/sub/a.txt")).await.is_err(),
            "the effect DID apply, even though the response got lost"
        );
        assert_eq!(
            recorder.entries.lock().expect("lock").len(),
            1,
            "and therefore there's a row: an effect in doubt resolves toward \"it happened\""
        );
    }

    /// **#186.** An irreversible `DeleteTree` cancelled HALFWAY writes the
    /// row for what it DID delete.
    ///
    /// `remove_tree` checks the cancellation token BETWEEN entries, so by
    /// the time it returns `Cancelled` an arbitrary number of nodes have
    /// already fallen — and for a `Mirror` against a trash-less destination
    /// (a bucket, an SFTP, a FAT thumb drive), permanently. The earlier
    /// condition skipped the row in exactly that case, leaving a
    /// half-deleted subtree with: no journal entry (no undo), no report row
    /// (the loop exits with `Err` on the spot) and nothing to tell the
    /// user. All three frontends assert in prose that what applies gets
    /// journalled.
    #[tokio::test]
    async fn a_tree_delete_cancelled_halfway_leaves_its_row() {
        // No trash on the destination: the delete is PERMANENT, which is
        // the case where the row is all that's left.
        let mem = Arc::new(MemProvider::with_flags(CapabilityFlags::CASE_SENSITIVE));
        mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
        mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
        mem.mkdir(&vp("mem:///d/sub")).await.expect("mkdir");
        write(&mem, "mem:///d/sub/a.txt", b"one").await;
        write(&mem, "mem:///d/sub/b.txt", b"two").await;
        let entry = mem.stat(&vp("mem:///d/sub")).await.expect("stat");

        let cancel = CancellationToken::new();
        let dest = Arc::new(CancelsOnDelete {
            inner: Arc::clone(&mem),
            cancel: cancel.clone(),
        });
        let t = SyncTargets {
            dest: dest as Arc<dyn Provider>,
            delete_mode: norte_proto::DeleteMode::Permanent,
            ..targets(&mem)
        };
        let recorder = Recorder::default();
        let record = SpoolStep {
            step: step(
                SyncStepKind::DeleteTree,
                "sub",
                Some(StepReversal::Irreversible),
            ),
            dest: Some(DestWitness::of(&entry)),
        };
        let report = Mutex::new(new_report(7, DestTrash::Restorable));
        let err = run(
            t,
            &recorder,
            futures::stream::iter(vec![Ok(record)]),
            &ctx(cancel),
            &report,
        )
        .await
        .expect_err("cancelled halfway through the tree");
        assert!(
            matches!(err, ApplyError::Stopped(Error::Cancelled)),
            "{err:?}"
        );

        // Something fell, and forever.
        assert!(
            mem.stat(&vp("mem:///d/sub/b.txt")).await.is_err(),
            "the post-order's first child was deleted"
        );
        assert!(
            mem.stat(&vp("mem:///d/sub")).await.is_ok(),
            "and the tree did NOT fall entirely: this is half a destruction"
        );

        // And that's in the journal, which is all #186 asks for.
        assert_eq!(
            recorder.entries.lock().expect("lock").as_slice(),
            [Recorded {
                op: "removed",
                path: "mem:///d/sub".to_owned(),
                reversal: Reversal::Irreversible,
                reversal_ref: None,
            }],
            "a half-destroyed tree is a real state, and hard rule 4 does not exempt it"
        );
    }

    /// And its twin, the one that stops the fix from overshooting: a
    /// cancellation arriving BEFORE the first delete leaves no row.
    ///
    /// Zero nodes removed means "the tree was never touched", and noting it
    /// would say something was destroyed that's still whole — an
    /// irreversible `removed` over an intact directory is worse than not
    /// noting anything.
    #[tokio::test]
    async fn a_delete_cancelled_before_touching_anything_leaves_no_row() {
        let mem = Arc::new(MemProvider::with_flags(CapabilityFlags::CASE_SENSITIVE));
        mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
        mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
        mem.mkdir(&vp("mem:///d/sub")).await.expect("mkdir");
        write(&mem, "mem:///d/sub/a.txt", b"one").await;
        let entry = mem.stat(&vp("mem:///d/sub")).await.expect("stat");

        let t = SyncTargets {
            delete_mode: norte_proto::DeleteMode::Permanent,
            ..targets(&mem)
        };
        let recorder = Recorder::default();
        let cancel = CancellationToken::new();
        cancel.cancel();
        // Via `delete_tree` and not `destroy_tree`: the recorder only
        // reaches the first one, so asserting this over the second would be
        // an assertion that couldn't fail.
        let err = delete_tree(
            &t,
            &SpoolStep {
                step: step(
                    SyncStepKind::DeleteTree,
                    "sub",
                    Some(StepReversal::Irreversible),
                ),
                dest: Some(DestWitness::of(&entry)),
            },
            &recorder,
            &vp("mem:///d/sub"),
            &ctx(cancel),
        )
        .await
        .expect_err("cancelled");
        assert!(matches!(err, StepError::Fatal(Error::Cancelled)), "{err:?}");
        assert!(
            mem.stat(&vp("mem:///d/sub/a.txt")).await.is_ok(),
            "the tree is still whole"
        );
        assert!(
            recorder.entries.lock().expect("lock").is_empty(),
            "and therefore nothing to note: an irreversible `removed` over an intact \
             directory would be worse than silence"
        );
    }

    /// **#160, and the reason [`Unrecorded`] exists.** When the row doesn't
    /// arrive AND the compensation doesn't either, the error that bubbles
    /// up NAMES what was buried and its trash destination — and stops the
    /// Task before the next step.
    ///
    /// Before, this was a bare `tracing::error!` and a `Fatal(Io)`: the
    /// caller couldn't tell "the destination stayed intact" apart from
    /// "your file is in the trash and nobody pointed to it", and "where's
    /// my file?" went unanswered by anyone not reading the daemon's log at
    /// that moment. This is what was seen live at the end of task 13 of the
    /// sync plan.
    #[tokio::test]
    async fn a_journal_that_fails_with_no_way_to_return_names_the_buried() {
        // OPAQUE trash: `trash()` doesn't say where it took the file, so
        // `restore_from` has nothing to point to and compensation is
        // impossible.
        let mem = Arc::new(MemProvider::new());
        mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
        mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
        write(&mem, "mem:///d/a.txt", b"old").await;
        write(&mem, "mem:///d/b.txt", b"also old").await;
        write(&mem, "mem:///s/a.txt", b"new").await;
        write(&mem, "mem:///s/b.txt", b"new too").await;
        let t = targets(&mem);

        // TWO steps, to be able to check the second never gets to run.
        let steps = vec![
            Ok(overwrite_step(&mem, "a.txt").await),
            Ok(overwrite_step(&mem, "b.txt").await),
        ];
        let report = Mutex::new(new_report(7, DestTrash::Restorable));
        let err = run(
            t,
            &TrashedFails,
            futures::stream::iter(steps),
            &ctx(CancellationToken::new()),
            &report,
        )
        .await
        .expect_err("the journal failed after burying");

        let ApplyError::Unrecorded(u) = err else {
            panic!("the state has to come out typed, not in a log line: {err:?}");
        };
        assert_eq!(u.buried, vp("mem:///d/a.txt"), "names what was buried");
        assert!(
            matches!(u.source, Error::Io { .. }),
            "and the error that caused it: {:?}",
            u.source
        );
        assert_eq!(
            read(&mem, "mem:///d/b.txt").await,
            b"also old",
            "and the Task stopped: the next step never got to run"
        );
    }

    /// #206: a `created` that fails AFTER a `trashed` that DID land leaves a
    /// BATCH whose undo blocks, and that gets STATED with the path and its
    /// spot in the trash inside the error.
    ///
    /// It isn't compensated, and that's the decision: returning it would
    /// require deleting the just-placed copy AND digging up the old one,
    /// i.e. two more mutations on the path where the journal already
    /// proved it doesn't work — and neither would end up recorded either.
    /// What CAN be done is not leaving the operator searching: it comes
    /// out as `Unrecoverable`, not as a plain `Fatal`, which is the
    /// difference between "it failed" and "your file is HERE".
    #[tokio::test]
    async fn a_created_that_fails_after_burying_says_where_everything_ended_up() {
        let mem = Arc::new(MemProvider::new());
        mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
        mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
        write(&mem, "mem:///s/a.txt", b"new").await;
        write(&mem, "mem:///d/a.txt", b"old").await;
        let t = targets(&mem);
        let record = SpoolStep {
            step: step(
                SyncStepKind::Overwrite,
                "a.txt",
                Some(StepReversal::RestoreTrash),
            ),
            dest: Some(DestWitness::of(
                &mem.stat(&vp("mem:///d/a.txt")).await.expect("stat"),
            )),
        };

        let err = super::overwrite(
            &t,
            &record,
            &CreatedFails,
            &vp("mem:///d/a.txt"),
            &ctx(CancellationToken::new()),
        )
        .await
        .expect_err("the `created` row never arrived");
        let StepError::Unrecoverable(u) = err else {
            panic!(
                "the batch was left unable to be undone: that's a STATE, not a failure: {err:?}"
            );
        };
        assert_eq!(
            u.buried,
            vp("mem:///d/a.txt"),
            "the error names WHAT was left unable to be undone"
        );
        assert_eq!(
            u.at, None,
            "and where it went, if the trash names it: this one doesn't (same `None` as \
             #160's opaque arm)"
        );
        // And the tree ended up as it ended up: the new one placed, the old one buried.
        assert_eq!(read(&mem, "mem:///d/a.txt").await, b"new");
    }

    /// #160, the other arm: a "vanish" trash (macOS/Windows, `Opaque`) does
    /// not name what it took — `buried` arrives `None` and there's nothing
    /// for `restore_from` to point to. Compensation isn't possible, so the
    /// step comes out as [`StepError::Unrecoverable`] and not as a plain
    /// `Fatal`: the tree is NOT what it was, and what was buried travels in
    /// the error so someone can say so. With no trash destination to
    /// offer, though — that's exactly what this trash doesn't know.
    #[tokio::test]
    async fn an_opaque_trash_does_not_compensate_but_still_fails_loud() {
        let mem = Arc::new(MemProvider::new());
        mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
        mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
        write(&mem, "mem:///d/a.txt", b"old").await;
        let t = targets(&mem);

        let err = super::bury(
            &t,
            &TrashedFails,
            &vp("mem:///d/a.txt"),
            1,
            &ctx(CancellationToken::new()),
        )
        .await
        .expect_err("the journal failed");
        let StepError::Unrecoverable(u) = err else {
            panic!(
                "with no compensation possible the state is DIFFERENT, and comes out typed: {err:?}"
            );
        };
        assert_eq!(u.buried, vp("mem:///d/a.txt"));
        assert!(
            u.at.is_none(),
            "an opaque trash does not say where it took it"
        );

        assert!(
            mem.stat(&vp("mem:///d/a.txt")).await.is_err(),
            "with no `reversal_ref` there's no compensation possible: the file is still \
             out of place, and that's what the log says, not a `stat` that finds it \
             again"
        );
    }
}
