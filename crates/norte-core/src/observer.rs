//! The journal's seam (M3): EVERY mutation the core executes goes through a
//! [`MutationObserver`] BEFORE being considered complete (hard rule 4). In
//! M0 the observer is a no-op; the journal will plug in here without
//! touching the engine.

use std::sync::Arc;

use async_trait::async_trait;
use norte_proto::{Error, VPath};

use crate::journal::Actor;

/// An observable VFS mutation.
#[derive(Debug)]
pub enum Mutation<'a> {
    /// Node created (a committed file or a dir).
    Created {
        /// Where it was created.
        path: &'a VPath,
        /// WHAT was created: the node's identity, if the backend can give
        /// it (#369, ADR 0152).
        ///
        /// The reverse of a `created` is a delete, and without this it
        /// deletes whatever is at that path NOW — which need not be what
        /// was created. With it, undo compares and refuses when it doesn't
        /// match.
        ///
        /// `None` = could not be determined (a provider with no stable
        /// identity, or a `stat` that failed). Then undo does what it
        /// always does: this can only make it refuse MORE, never less.
        node: Option<norte_vfs::NodeId>,
    },
    /// Node PERMANENTLY deleted (irreversible).
    Removed(&'a VPath),
    /// Node moved to the trash (RECOVERABLE — M3's undo restores it;
    /// a different regime from `Removed`, ADR 0009).
    Trashed {
        /// Original path (victim).
        path: &'a VPath,
        /// Recoverable destination in a LOGICAL trash (`.norte-trash/<id>`,
        /// phase 9) → `reversal_ref`. `None` if it's the OS's NATIVE trash
        /// or a "vanish" (no stable path; the handle is resolved in M3-2's
        /// undo).
        dest: Option<&'a VPath>,
    },
    /// Node renamed within a provider.
    Renamed {
        /// Original path.
        from: &'a VPath,
        /// New path.
        to: &'a VPath,
        /// The batch this rename belongs to (`fs.rename_batch`): the tag
        /// grouping n journal entries so they can be undone together.
        /// `None` for a standalone rename — which is everything outside
        /// the batch executor.
        ///
        /// The batch lives in THIS variant and not in the task's context
        /// because the batch executor only ever emits renames.
        ///
        /// EXECUTOR OBLIGATION: each step calls `Provider::rename`
        /// DIRECTLY. If it instead went through the move path with a
        /// collision policy, an overwrite would emit a [`Mutation::Removed`]
        /// —classified `Irreversible`— that would fall OUTSIDE the group: a
        /// permanent delete inside an operation the wire advertises as one
        /// undoable unit, one a batch undo would not even see to block on.
        /// The planner already rejects the whole plan on any collision, so
        /// the executor never has a reason to overwrite anything.
        batch: Option<i64>,
    },
    /// POSIX permissions changed (#314).
    ModeChanged {
        /// The node whose mode changed.
        path: &'a VPath,
        /// The mode it HAD, read before writing the new one. It's the
        /// entire reverse: without it there's no undo to offer.
        ///
        /// `None` when it could not be read —a provider that doesn't
        /// publish `posix.mode`, or a `stat` that failed—, and then the
        /// entry is classified `Irreversible` with that reason (rule 4).
        /// Promising an undo that would restore a made-up mode is worse
        /// than offering none.
        from: Option<u32>,
        /// The mode that was set.
        to: u32,
        /// The batch this change belongs to (#315): the tag grouping the n
        /// nodes of ONE recursive `fs.set_mode`. `None` for a standalone
        /// change — which is everything with no recursion.
        ///
        /// Exists for the same reason as in [`Self::Renamed`]: without it,
        /// a chmod over a tree of a hundred thousand files leaves a
        /// hundred thousand entries nobody can regroup, and an audit
        /// reading them would see a hundred thousand actions where the
        /// human did one. Undo works the same either way —it's LIFO and
        /// every entry carries its reverse—; what the batch buys is being
        /// able to SAY they were one.
        batch: Option<i64>,
    },
}

impl<'a> Mutation<'a> {
    /// A `created` whose node identity is unknown (ADR 0152).
    ///
    /// This is the right call for whoever creates something and cannot
    /// cheaply ask WHAT it created —and the honest one: undo will do what
    /// it always does—. Whoever can ask builds the variant with its `node`,
    /// which is what the copy does.
    #[must_use]
    pub fn creado(path: &'a VPath) -> Self {
        Self::Created { path, node: None }
    }
}

/// Mutation receiver. M3 implements it as the journal (with undo); until
/// then, an internal no-op observer.
#[async_trait]
pub trait MutationObserver: Send + Sync {
    /// Notifies a mutation that has already been applied successfully.
    /// Async and fallible: the journal awaits the insert before the op is
    /// considered complete, and its failure PROPAGATES (rule 4 — the op
    /// fails if its entry did not land durably).
    ///
    /// # Errors
    /// The sink's error (e.g. a journal write failure).
    async fn on_mutation(&self, mutation: &Mutation<'_>, actor: &Actor) -> Result<(), Error>;

    /// Is it worth finding out the identity of what's being created (ADR 0152)?
    ///
    /// Knowing it costs a `stat` per created node, and against a remote
    /// destination that's a network round trip. Whoever isn't going to
    /// store the mutation isn't going to use the identity either, so it can
    /// say no and skip the whole thing.
    ///
    /// This is a cost HINT, not a guarantee: answering `true` does not
    /// oblige the caller to fetch it —it can fail and `None` can still
    /// arrive—, and answering `false` only promises it won't be missed.
    /// That's why the default is `true`: an observer that stores but does
    /// not answer this loses the protection silently, which is the
    /// expensive direction for the mistake to go.
    fn wants_identity(&self) -> bool {
        true
    }

    /// The observer that ONE Task will use for ALL its mutations, decided
    /// **once, before the first effect**.
    ///
    /// `None` —the default case— means "I serve myself": an observer with
    /// no window to lose (the daemon's journal, already open; a no-op) has
    /// nothing to pin.
    ///
    /// # Why this exists: a half-logged operation is worse than none (#205)
    ///
    /// The EMBEDDED journal can be lost and recovered mid-session (#179).
    /// If asked PER MUTATION, a long operation —a `copy_tree` that starts
    /// with the file busy and outlasts the retry backoff— starts logging
    /// partway through: the first k entries with no row, the following n-k
    /// with one, within ONE Task and ONE actor. And then `undo_session`
    /// unwinds the logged tail and leaves the head that isn't: half the
    /// copy undone, with nothing to tell the user which half — because
    /// there are no rows to name it.
    ///
    /// "Nothing got logged" is fixed by hand; "half got logged" is a trap.
    /// By pinning the verdict at the start of the Task, an operation lands
    /// either entirely inside the journal or entirely outside it, which is
    /// how it was before the window learned to reopen.
    ///
    /// **The pinned handle is held for the entire Task**, and that's the
    /// other half of the contract: while it lives,
    /// [`crate::embedded::LazyJournal::release`] cannot let go of the file.
    /// What that protects is the **pinned → last row** window, and it's
    /// worth not confusing it with the one from the gate to the pin: the
    /// gate resolves and RELEASES its `Arc`, and the Task can wait in the
    /// scheduler's queue for an indefinite while before pinning. An
    /// idleness timer firing there closes the window without breaking
    /// anything —there are no rows to lose and pinning reopens it— but it
    /// can leave the whole operation unlogged if a third party wins the
    /// reopen. Whoever writes that timer also has to look at Tasks that
    /// are dispatched but not yet started.
    ///
    /// # Errors
    /// [`Error::JournalUnavailable`] if the journal exists and CANNOT BE
    /// OPENED (#178). Same fail-closed as the engine's gate, repeated here
    /// because the gate looks BEFORE enqueuing and this looks when actually
    /// starting: up to thirty seconds of queueing fit between the two, and
    /// in that gap a file can go from sound to corrupt. Not propagating it
    /// would leave the Task silently mutating an entire tree, which is
    /// exactly what #178 refuses — and here no effect has happened yet, so
    /// refusing is free.
    async fn pin_for_task(&self) -> Result<Option<Arc<dyn MutationObserver>>, Error> {
        Ok(None)
    }
}

/// THIS Task's observer, pinned once and for all (see
/// [`MutationObserver::pin_for_task`]).
///
/// Called at the START of the body of every Task that mutates, before any
/// effect. That the decision is made here and not in every `on_mutation` is
/// what makes the Task land either entirely inside or entirely outside the
/// journal.
/// # Errors
/// Those of [`MutationObserver::pin_for_task`]: the journal exists and
/// cannot be opened. This runs BEFORE the first effect, so the Task dies
/// without having touched anything.
pub(crate) async fn pin_for_task(
    observer: Arc<dyn MutationObserver>,
) -> Result<Arc<dyn MutationObserver>, Error> {
    Ok(match observer.pin_for_task().await? {
        Some(pinned) => pinned,
        None => observer,
    })
}

/// Observer that does nothing (M0 / tests with no journal).
pub(crate) struct NoopObserver;

#[async_trait]
impl MutationObserver for NoopObserver {
    async fn on_mutation(&self, _mutation: &Mutation<'_>, _actor: &Actor) -> Result<(), Error> {
        Ok(())
    }

    fn wants_identity(&self) -> bool {
        false
    }
}
