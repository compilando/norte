//! Session undo (M3-2): runs the persisted `Reversal` of each journal entry
//! in LIFO order and appends a compensating entry (append-only, the chain
//! stays whole).
//!
//! **No-clobber (strict).** `RenameBack`/`RestoreTrash` require the
//! destination to be FREE before acting (they never overwrite). Undoing a
//! `Created` is the subtle case, and it has two halves. The entry records the
//! IDENTITY of what it created (ADR 0152), so a node that got REPLACED is
//! detected and the undo is refused — what's there is not its. What identity
//! does not see is an EDIT: the file is the same inode with different
//! content, and that does not produce a new `Created`. That is why it is also
//! undone ONLY via TRASH (recoverable), and without the `TRASH` capability the
//! entry is skipped with its own counter in the [`UndoReport`], never a
//! permanent `remove` (#65).
//!
//! **Three units, three contracts.** [`undo_units`] groups by `batch_id` for
//! the three of them, and [`revert_unit`] hands out:
//!
//! - a LONE mutation → [`revert_entry`];
//! - a `fs.rename_batch` batch → [`revert_batch`], all of it or nothing (half
//!   a permutation undone is not any state);
//! - a `sync.apply` batch → [`revert_sync_batch`], whatever it can and with
//!   names for what it cannot (half a sync undone IS a state: the tree from
//!   before with some files already returned).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use norte_proto::{CapabilityFlags, ConflictKind, Error, TaskId, TaskKind, VPath};
use norte_vfs::Provider;
use tokio_util::sync::CancellationToken;

use crate::journal::{Actor, JournalEntry, NewEntry, Reversal, SqliteJournal};
use crate::progress::ProgressReporter;
use crate::rename::exec::{BatchJournal, BatchReport, PlannedStep};
use crate::rename::plan::{NameCaps, name_key};

/// Cap on denied units the report LISTS (#171). The rest only count.
///
/// The same number and the same reason as `sync`'s caps: a list without a
/// cap travels over the wire and stays in client memory, and with a policy
/// that denies by default that's as many rows as the session has units.
pub use norte_proto::methods::UNDO_MAX_DENIED_REPORTED;

/// Projects an [`UndoReport`] to what travels (`policy.undo_report`, #71).
/// Used by the `Backend`'s two arms: the daemon to answer over the socket and
/// the embedded one to read it without one.
pub(crate) fn report_to_proto(r: UndoReport) -> norte_proto::methods::PolicyUndoReportResult {
    use norte_proto::methods;
    methods::PolicyUndoReportResult {
        undone: r.undone,
        skipped_irreversible: r.skipped_irreversible,
        skipped_created_no_trash: r.skipped_created_no_trash,
        skipped_not_ours: r.skipped_not_ours,
        blocked: r
            .blocked
            .map(|(seq, error)| methods::UndoBlocked { seq, error }),
        // 0.36.0: undoing a BATCH can end up halfway, and that is not a
        // `blocked` — `blocked` says "I stopped and the tree is consistent".
        // Without these two fields, the human whose undo left a directory
        // half-renamed saw exactly the same thing as one that went fine.
        batch_stuck: r.batch_stuck.as_ref().map(crate::rename::stuck_to_proto),
        compensations_lost: r.compensations_lost,
        // #171: what the policy denied unit by unit. Goes separate from
        // `blocked` because it says the opposite of it — the undo did NOT
        // stop.
        denied: r
            .denied
            .into_iter()
            .map(|(seq, error)| methods::UndoBlocked { seq, error })
            .collect(),
        denied_total: r.denied_total,
    }
}

/// Result of a [`crate::Engine::undo_session`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UndoReport {
    /// Entries reverted successfully (compensation appended). Counts
    /// ENTRIES, not units: a `fs.rename_batch` batch contributes all of its
    /// own at once, because it is reverted whole or not at all, and a
    /// `sync.apply` one contributes the ones that truly came back. The
    /// Task's progress, by contrast, advances by units — to the human a batch
    /// is ONE undo step.
    pub undone: u64,
    /// `Irreversible` entries found and skipped (there's nothing to
    /// overwrite).
    pub skipped_irreversible: u64,
    /// `Created` reversals skipped because the provider does NOT have
    /// `TRASH` (#65): undoing them would be a PERMANENT delete of "whatever
    /// lives at that path today" — without a `node_id` in the `Created`, it
    /// may be human work done after the creation. The node stays; whoever
    /// wants it deleted asks for it explicitly (`fs.delete`).
    pub skipped_created_no_trash: u64,
    /// `Created` reversals skipped because what is at that path is NOT the
    /// node that entry created (#369/#371, ADR 0152). The node stays and the
    /// session continues.
    pub skipped_not_ours: u64,
    /// First step blocked (drift/conflict): original `seq` + reason. The
    /// session stops there (strict).
    ///
    /// **This is not the same as [`Self::denied`]**, and confusing the two
    /// would be reading the report backwards: this one says "I stopped here
    /// and the tree stayed consistent"; that one says "this unit was not
    /// touched and the undo continued with the rest".
    pub blocked: Option<(i64, Error)>,
    /// Units the POLICY denied, with the `seq` of their first entry and the
    /// reason (#171). The undo does NOT stop: it blocks that unit and
    /// continues.
    ///
    /// Capped at [`UNDO_MAX_DENIED_REPORTED`]; [`Self::denied_total`] counts
    /// all of them. Undoing half a million entries under a policy that
    /// denies by default would fill the client's memory with the list,
    /// which is the same failure as #196 at the other end.
    pub denied: Vec<(i64, Error)>,
    /// How many units the policy denied in total, capped or not.
    pub denied_total: u64,
    /// **Undoing a batch got stuck halfway.** The executor could not return
    /// some undo step it had already applied, so the directory did NOT go
    /// back to how it was: here is the concrete step, with names.
    ///
    /// When this is `Some`, the Task ends `Failed` and not `Completed`: an
    /// undo that says "done" promises a restored tree, and this one isn't.
    /// Look at it even if the Task failed — the error only tells the cause.
    pub batch_stuck: Option<crate::rename::StuckStep>,
    /// Reversals from a batch undo that were APPLIED but whose compensation
    /// could not be written. Each one leaves an entry that keeps looking
    /// pending even though its effect already came back: a later undo will
    /// find it and block there. It is the only signal of that.
    pub compensations_lost: u64,
    /// **What did NOT come back**, by path and in wire bytes (rule 1),
    /// capped at [`UNDO_MAX_UNREVERTED_PATHS`].
    ///
    /// It's filled by undoing a SYNC batch (`sync.apply`), which reverts
    /// what it can instead of refusing the whole thing: without this list, a
    /// `skipped_irreversible: 3` over a batch of ten thousand steps is a
    /// number with nowhere to look. A path lands here for one of three
    /// reasons, and the counters are what tell them apart: the entry was
    /// `irreversible` ([`Self::skipped_irreversible`]), it was a `created`
    /// at a destination without trash ([`Self::skipped_created_no_trash`],
    /// #65), or its reversal ran into drift ([`Self::blocked`], which names
    /// the first one).
    ///
    /// **The counters do NOT split it in three.** `skipped_irreversible` and
    /// `skipped_created_no_trash` are exact, but of the entries with drift
    /// only the first stays in `blocked` and there is no counter for the
    /// rest: a list of five with `skipped_irreversible: 1` means "one
    /// irreversible and FOUR that did not come back for some other reason",
    /// not four identifiable blocks. And it is capped, so adding up does not
    /// work either past [`UNDO_MAX_UNREVERTED_PATHS`].
    ///
    /// The rename path does not use it — that batch comes back whole or is
    /// not touched, so there is no "what did not come back".
    ///
    /// **Today it does not leave the process**: `PolicyUndoReportResult`
    /// does not carry this field, so a remote client sees the counters and
    /// not the paths. Putting it on the wire is a `norte-proto` change with
    /// its goldens, and needs the authority redacted first the way
    /// [`crate::engine::span_path`](crate::Engine) does — a wire `VPath` can
    /// carry userinfo (rule 10).
    pub unreverted_paths: Vec<Vec<u8>>,
}

/// How many paths fit in [`UndoReport::unreverted_paths`].
///
/// The same criterion as the wire's lists (`SYNC_MAX_FAILURES_REPORTED` and
/// friends): a sample that fits on a screen, with the counter next to it
/// telling the whole truth. A sync batch can have half a million steps and
/// this report lives in the daemon's memory.
pub const UNDO_MAX_UNREVERTED_PATHS: usize = 64;

/// Rebuilds a `VPath` from the `to_wire` bytes stored in the journal.
fn wire(bytes: &[u8]) -> Result<VPath, Error> {
    let s = std::str::from_utf8(bytes).map_err(|_| Error::InvalidPath)?;
    VPath::parse(s).map_err(|_| Error::InvalidPath)
}

/// `true` if `p` does NOT exist (is free) in `provider`.
///
/// Shared with the batch executor's rollback (`crate::rename::exec`): the
/// no-clobber primitive has to be ONE, or the two copies stop treating a
/// `stat` that fails for another reason the same way.
pub(crate) async fn is_free(provider: &dyn Provider, p: &VPath) -> Result<bool, Error> {
    match provider.stat(p).await {
        Err(Error::NotFound) => Ok(true),
        Ok(_) => Ok(false),
        Err(e) => Err(e),
    }
}

const OCCUPIED: Error = Error::Conflict {
    conflict: ConflictKind::Exists,
};

/// Are `a` and `b` the SAME node? (#274)
///
/// Without identity — a provider that doesn't give one — it answers `false`:
/// what this decides is whether you can rename over what occupies the
/// origin, and when in doubt, nothing is touched.
async fn same_node(provider: &dyn Provider, a: &VPath, b: &VPath) -> Result<bool, Error> {
    let a_id = provider.node_id(a, norte_vfs::FollowLinks::No).await?;
    let b_id = provider.node_id(b, norte_vfs::FollowLinks::No).await?;
    Ok(matches!((a_id, b_id), (Some(x), Some(y)) if x == y))
}

/// Renames `from` to `dest` by way of an intermediate name, for when the two
/// paths are the same node and the provider's rename cannot overwrite
/// (#274).
///
/// The twin of `ops::spelling_rename`, for the undo path. It lives
/// apart and shares no code with it because there is no `TaskCtx` here, no
/// observer, no retries: undoing already runs inside its own task, and what
/// the journal emits is the compensation above.
async fn rename_via_detour(
    provider: &dyn Provider,
    from: &VPath,
    dest: &VPath,
) -> Result<(), Error> {
    for n in 0..1000u32 {
        let mut name = crate::rename::naming::TEMP_PREFIX.to_vec();
        name.extend_from_slice(format!("case-undo-{n}").as_bytes());
        let seg = norte_proto::Segment::new(name).map_err(|_| Error::InvalidPath)?;
        let temp = from.with_file_name(seg).ok_or(Error::InvalidPath)?;
        if !is_free(provider, &temp).await? {
            continue;
        }
        provider.rename(from, &temp).await?;
        if let Err(e) = provider.rename(&temp, dest).await {
            // The way back, or the file is left with the detour's name and
            // the reader has nowhere to look for it.
            if let Err(revert) = provider.rename(&temp, from).await {
                tracing::error!(
                    error = %e,
                    revert = %revert,
                    left_at = %crate::engine::span_path(&temp),
                    "undoing a case change could not finish or go back"
                );
            }
            return Err(e);
        }
        return Ok(());
    }
    Err(OCCUPIED)
}

/// The POSIX permissions `p` has NOW, or `None` if they cannot be read
/// (#314).
///
/// `None` is not a failure of the caller: there are providers that don't
/// publish `posix.mode`, and then the honest thing is to record the mutation
/// as irreversible and say so, instead of saving a made-up mode that a later
/// undo would apply as if it were the one from before.
///
/// The twelve permission bits, without the node-class ones: it's the only
/// thing `set_mode` accepts, and returning the whole `st_mode` would make
/// the reversal try to change what class the node is.
pub(crate) async fn modo_actual(provider: &dyn Provider, p: &VPath) -> Option<u32> {
    let req = norte_vfs::AttrRequest::sanitized(vec!["posix.mode".to_owned()]);
    let opt = norte_vfs::ListOptions { attrs: req };
    let entry = provider.stat_with(p, &opt).await.ok()?;
    match entry.attrs.get("posix.mode")? {
        norte_proto::AttrValue::Uint(m) => u32::try_from(*m)
            .ok()
            .map(|m| m & norte_proto::methods::MODE_PERMISSION_BITS),
        _ => None,
    }
}

/// Does `dir` have any children?
///
/// Looks ONLY at the first item of the listing: the question is "is it
/// empty?", and a directory with a hundred thousand entries answers it with
/// the first one. It never materializes the listing, so it has no cap to
/// blow.
async fn has_children(provider: &dyn Provider, dir: &VPath) -> Result<bool, Error> {
    use futures::StreamExt as _;
    let mut listing = provider.list(dir).await?;
    match listing.next().await {
        Some(item) => {
            item?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Result of trying to revert ONE undo unit (a lone entry or a whole batch).
pub(crate) enum Reverted {
    /// Reverted and compensated.
    Done,
    /// `Irreversible`: skipped.
    SkippedIrreversible,
    /// `Created` reversal on a provider without `TRASH`: skipped, the node
    /// stays (#65). No compensation (there was no effect); a later undo will
    /// find it again — honest.
    SkippedNoTrash,
    /// `Created` reversal whose node is NOT the one the entry created
    /// (#371): skipped, the node stays, and the session CONTINUES.
    ///
    /// It does not block, and that is a decision. The module blocks on any
    /// drift because a drift is a state nobody explains; this one does have
    /// an explanation — the reader put it there — and it's the most common
    /// drift there is: any editor that saves atomically changes the inode,
    /// so editing ONE file from a copy would leave the other one thousand
    /// nine hundred ninety-nine without their undo. "I left it where it was
    /// and I'm telling you" describes that better than "I stopped".
    SkippedNotOurs,
    /// Blocked by drift/conflict. `seq` is the SPECIFIC entry that could not
    /// be reverted — in a batch, the one for the step that got stuck, not
    /// the one for the whole batch: it's the one the human has to go look
    /// at.
    Blocked {
        /// The blocked entry.
        seq: i64,
        /// Why.
        error: Error,
    },
    /// The unit ALREADY counted itself in the [`UndoReport`] and the session
    /// continues.
    ///
    /// It's what [`revert_sync_batch`] returns, which reverts part of a
    /// batch and skips the rest: `Done` would make the caller add ALL of the
    /// unit's entries to `undone`, and `SkippedIrreversible` would add one
    /// for a batch that maybe reverted nine thousand. Whoever splits between
    /// the counters is whoever knows what happened to each entry.
    Accounted,
    /// Undoing a BATCH got applied halfway and could not be unwound either:
    /// the tree did NOT come back. Unlike `Blocked`, this FAILS the Task —
    /// saying `Completed` about a directory reverted halfway is the one lie
    /// this module cannot allow itself. The detail (which step, with which
    /// names) goes in [`UndoReport::batch_stuck`].
    Stuck {
        /// The entry whose step was left applied.
        seq: i64,
        /// Why it could not come back.
        error: Error,
    },
}

impl Reverted {
    /// Shortcut for the blocked case, built in eleven places.
    fn blocked(seq: i64, error: Error) -> Self {
        Self::Blocked { seq, error }
    }
}

/// Splits the LIFO entries into undo UNITS: a lone entry, or ALL the ones
/// that share a `batch_id`.
///
/// **A batch is ONE unit**, and it has to reach the task as a single item.
/// What is done with that unit depends on who wrote it: a rename batch is
/// reverted whole or not touched at all (half a permutation undone is the
/// state this whole feature exists to prevent), and a sync one reverts what
/// it can ([`revert_sync_batch`]). The grouping is the same for both, and
/// that's why it lives here and not in either of them.
///
/// The grouping is GLOBAL, not by contiguous entries. The scheduler runs up
/// to four tasks per provider and the undo is queued with its own key, so
/// another mutation by the SAME actor can land between two entries of the
/// batch; grouping by contiguity would split that batch into two units and
/// the first one would leave it halfway. The unit is anchored where its
/// first member appeared (the larger `seq`), so that the LIFO order between
/// units is preserved and the members left below get MOVED UP.
///
/// **What that moving up costs.** A strict LIFO by entries can always be
/// undone; grouping cannot. If the interleaved mutation occupies a name the
/// batch needs to come back, [`feasible`] sees it and the unit ends up
/// blocked — and, undo being strict, the session stops there and never
/// reaches the interleaved one that would unstick it. Example: `seq 1`
/// (batch) `a → tmp`, `seq 2` (lone) `x → a`, `seq 3` (batch) `tmp → b`.
/// Undoing the batch asks for `a` to be free and the old `x` occupies it; by
/// entries (3, 2, 1) it would have worked. Accepted knowingly: an honest
/// block is preferable to half a permutation undone, which is the state
/// this function exists to prevent. It's rare in practice.
///
/// No risk of crossing actors: the list comes from
/// [`crate::journal::Journal::revertible_for`], which already filters by
/// actor, so a `batch_id` shared by two actors (impossible today: the batch
/// is written by a single task with a single actor) wouldn't join them
/// either.
pub(crate) fn undo_units(entries: Vec<JournalEntry>) -> Vec<Vec<JournalEntry>> {
    let mut units: Vec<Vec<JournalEntry>> = Vec::new();
    let mut at: HashMap<i64, usize> = HashMap::new();
    for e in entries {
        match e.batch_id {
            Some(b) => {
                if let Some(&i) = at.get(&b) {
                    units[i].push(e);
                } else {
                    at.insert(b, units.len());
                    units.push(vec![e]);
                }
            }
            None => units.push(vec![e]),
        }
    }
    units
}

/// Runs `entry`'s reversal on `provider` (verified, strict) and, if it
/// succeeds, appends the compensation with `actor` and `undoes_seq =
/// entry.seq`.
///
/// `batch` is the batch of the COMPENSATION, not of the reverted entry:
/// `Some(id)` when this reversal is part of undoing a batch — the sync one,
/// [`revert_sync_batch`] — and `None` for a lone mutation. It's a GROUPING
/// label, so the audit and any reading of the journal see a batch's undo as
/// a batch; it does not make it undoable (`revertible_for` filters
/// `undoes_seq IS NULL`, so a compensation is never reverted).
///
/// # Errors
/// Only for a failure PERSISTING the compensation in the journal (rule 4) —
/// and by then the EFFECT already happened: the node moved and the journal
/// does not know it. An FS conflict/drift is NOT an error: `Reverted::Blocked`
/// is returned.
// Linear dispatch by `Reversal` (4 branches): clearer together than split apart.
#[expect(
    clippy::too_many_lines,
    reason = "Linear dispatch by `Reversal` (4 branches): clearer together than split apart"
)]
pub(crate) async fn revert_entry(
    provider: &dyn Provider,
    journal: &SqliteJournal,
    entry: &JournalEntry,
    actor: &Actor,
    batch: Option<i64>,
    cancel: &CancellationToken,
) -> Result<Reverted, Error> {
    let path = wire(&entry.path)?;
    match entry.reversal.as_str() {
        "irreversible" => Ok(Reverted::SkippedIrreversible),

        // Undo of a Created: remove the created node (if it still exists,
        // and if it is the SAME one — see below).
        //
        // The undo ALWAYS goes via TRASH (recoverable), and identity does
        // not change that: a content EDIT does not produce a new `Created`
        // nor change the inode, so the file the agent created and the user
        // edited afterward is still indistinguishable by identity from one
        // nobody touched. What the trash buys is that the user's work stays
        // recoverable instead of destroyed. Without the `TRASH` capability
        // (sftp/object with `logical_trash` OFF — the common remote case)
        // it does NOT fall back to a permanent `remove`: the entry is
        // SKIPPED and the node stays (#65).
        "delete" => {
            // The stat goes FIRST: drift (the node is no longer there)
            // ALWAYS blocks — classifying it as skip-because-no-trash would
            // swallow the divergence signal that strict mode values.
            let node = match provider.stat(&path).await {
                Ok(node) => node,
                // It's no longer there: unexpected state → block (does not
                // fake success).
                Err(e) => return Ok(Reverted::blocked(entry.seq, e)),
            };
            // And is it what this entry created? (#369, ADR 0152)
            //
            // Replacing the node DOES change the inode, and that's where
            // this bites: a copy that failed because its destination folder
            // got deleted leaves entries naming paths that today have
            // SOMETHING ELSE inside — the good copy, the one the user redid
            // after reading the failure. Without this check, undoing that
            // batch takes it to the trash: a delete caused by an operation
            // that never happened.
            //
            // It can only make the undo refuse MORE: without a saved
            // fingerprint (old entries, providers without identity), without
            // a fingerprint that parses, or without an identity to compare
            // now, it proceeds as always.
            if let Some(expected) = entry
                .reversal_ref
                .as_deref()
                .and_then(crate::journal::footprint_to_node)
            {
                match provider.node_id(&path, norte_vfs::FollowLinks::No).await {
                    Ok(Some(current)) if current != expected => {
                        tracing::info!(
                            seq = entry.seq,
                            "what's at that path is not what this entry created: leaving it",
                        );
                        return Ok(Reverted::SkippedNotOurs);
                    }
                    Ok(_) => {}
                    Err(e) => return Ok(Reverted::blocked(entry.seq, e)),
                }
            }
            // A created DIRECTORY is only undone EMPTY. The trash takes the
            // whole subtree with it, so a directory with content the
            // journal doesn't explain — a file the user put inside
            // afterward, or a sibling from the same batch whose reversal
            // could not come back — would vanish from sight without
            // anything naming it. Descending `seq` order leaves it empty by
            // construction when everything goes well; this turns that
            // assumption into a check.
            if node.kind == norte_proto::EntryKind::Dir {
                match has_children(provider, &path).await {
                    Ok(true) => {
                        tracing::info!(
                            seq = entry.seq,
                            "the created directory has content this undo didn't put there: leaving it",
                        );
                        return Ok(Reverted::blocked(entry.seq, OCCUPIED));
                    }
                    Ok(false) => {}
                    Err(e) => return Ok(Reverted::blocked(entry.seq, e)),
                }
            }
            if !provider
                .capabilities()
                .flags
                .contains(CapabilityFlags::TRASH)
            {
                return Ok(Reverted::SkippedNoTrash);
            }
            // Id of the compensating trash (#99): the undone event's `seq`
            // (unique) works as a counter; WITHIN one `undo_session`
            // `now_ms` is computed once per reversal, so `trash_retrying`'s
            // retry converges. (Between SEPARATE undo invocations `now_ms`
            // differs: only `seq` is stable — capped, without loss.) It's
            // routed via `trash_retrying` so a transient-after-effect
            // doesn't lose `reversal_ref` on the undo path too.
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
            let comp_trash_id = norte_vfs::trash::TrashId::new(now_ms, entry.seq.unsigned_abs());
            let (comp_op, comp_reversal, comp_ref) =
                match crate::ops::trash_retrying(provider, &path, &comp_trash_id, cancel).await {
                    Ok(dest) => ("trashed", Reversal::RestoreTrash, dest),
                    // Cancellation is NOT a block: the Task has to end
                    // `Cancelled` (rule 3), not `Completed` with a reason.
                    // The token is the Task's, so the retry ladder doesn't
                    // keep running after a cut.
                    Err(Error::Cancelled) => return Err(Error::Cancelled),
                    Err(e) => return Ok(Reverted::blocked(entry.seq, e)),
                };
            let comp_ref_bytes = comp_ref.as_ref().map(|d| d.to_wire().into_bytes());
            if let Err(e) = journal
                .journal()
                .record_entry(&NewEntry {
                    op: comp_op,
                    path: &entry.path,
                    path_to: None,
                    reversal: comp_reversal,
                    reversal_ref: comp_ref_bytes.as_deref(),
                    actor,
                    undoes_seq: Some(entry.seq),
                    batch_id: batch,
                })
                .await
            {
                // The node is ALREADY in the trash and the journal doesn't
                // know it: the log is the only thing left to answer "where
                // did my file go?". It goes with both paths, redacted (rule
                // 10), the way `sync::exec` does when burying.
                tracing::error!(
                    seq = entry.seq,
                    error = %e,
                    buried = %crate::engine::span_path(&path),
                    dest = comp_ref.as_ref().map(crate::engine::span_path).unwrap_or_default(),
                    "reversal applied without compensating: the node is in the trash and the journal doesn't record it",
                );
                return Err(Error::from(e));
            }
            Ok(Reverted::Done)
        }

        // Undo of a Renamed: return the node at `path` (destination) to
        // `path_to` (origin). The origin must be FREE.
        //
        // TOCTOU is_free→rename: on local it's closed by
        // `renameat2(NOREPLACE)`; on sftp (posix-rename clobbering) and
        // object (copy+delete) the window exists — remote-provider debt,
        // narrow window.
        "rename_back" => {
            let Some(from_bytes) = entry.path_to.as_deref() else {
                return Ok(Reverted::blocked(entry.seq, Error::InvalidPath));
            };
            let from = wire(from_bytes)?;
            // Undoing a SPELLING change (#274) cannot ask for the origin to
            // be free: on the volume that folds — the only one where that
            // rename happens — `stat("Foo.txt")` finds the `foo.txt` we
            // just created, so `is_free` always says "occupied" and the
            // undo would block guaranteed. And a `Blocked` strangles the
            // LIFO: it strands everything earlier in the session (#128).
            //
            // What breaks the tie is identity: if what occupies the origin
            // is the SAME node we're returning, there's nothing to respect
            // there — it IS it. Then it's renamed by the detour, the same
            // way it was done on the way out, because the provider's rename
            // can't overwrite either.
            let same_node_occupies = match is_free(provider, &from).await {
                Ok(true) => false,
                Ok(false) => match same_node(provider, &path, &from).await {
                    Ok(true) => true,
                    Ok(false) => return Ok(Reverted::blocked(entry.seq, OCCUPIED)),
                    Err(e) => return Ok(Reverted::blocked(entry.seq, e)),
                },
                Err(e) => return Ok(Reverted::blocked(entry.seq, e)),
            };
            let result = if same_node_occupies {
                rename_via_detour(provider, &path, &from).await
            } else {
                provider.rename(&path, &from).await
            };
            if let Err(e) = result {
                return Ok(Reverted::blocked(entry.seq, e));
            }
            // Compensation: renamed inverse (destination=origin,
            // origin=destination).
            journal
                .journal()
                .record_entry(&NewEntry {
                    op: "renamed",
                    path: from_bytes,
                    path_to: Some(&entry.path),
                    reversal: Reversal::RenameBack,
                    reversal_ref: None,
                    actor,
                    undoes_seq: Some(entry.seq),
                    batch_id: batch,
                })
                .await
                .map_err(Error::from)?;
            Ok(Reverted::Done)
        }

        // Undo of a ModeChanged (#314): return the permissions it had.
        //
        // The previous mode comes in `reversal_ref`, in decimal ASCII (see
        // `Reversal::SetModeBack`). Without it — or unreadable — the entry
        // would be classified `Irreversible` and wouldn't reach here; if it
        // reaches here anyway that's a corrupt journal, and then it BLOCKS
        // instead of making up a mode. There's no "is it free" check to
        // make: this doesn't create or move anything, it only returns
        // twelve bits to whatever is at that path.
        //
        // "Whatever is there", not "the node that changed": the journal
        // doesn't store the node's identity — the same gap the `delete` arm
        // reasons about above — so if that got deleted and someone created
        // something else with that name, this reversal gives it the
        // previous one's permissions. The ceiling on the damage is lower
        // than a delete's, but it's better not to fake a guarantee that
        // isn't checked.
        "set_mode_back" => {
            let Some(previous) = entry
                .reversal_ref
                .as_deref()
                .and_then(|b| std::str::from_utf8(b).ok())
                .and_then(|s| s.parse::<u32>().ok())
            else {
                return Ok(Reverted::blocked(entry.seq, Error::InvalidPath));
            };
            // A corrupt mode in the journal is not passed to the provider:
            // node-class bits aren't a permission, and the trait says to
            // reject them instead of trimming them.
            if previous & !norte_proto::methods::MODE_PERMISSION_BITS != 0 {
                return Ok(Reverted::blocked(entry.seq, Error::InvalidPath));
            }
            // The mode as it is NOW, so the compensation tells the truth
            // about what it undid. If it can't be read, it's NOT made up:
            // the compensation stays irreversible, the same as the original
            // mutation does when it couldn't read its own. Falling back to
            // `path_to` — the mode that was REQUESTED — would promise a
            // redo toward a value nobody ever observed, and `chmod(2)` may
            // have changed it along the way (clears setgid silently).
            let current = modo_actual(provider, &path).await;
            if let Err(e) = provider.set_mode(&path, previous).await {
                return Ok(Reverted::blocked(entry.seq, e));
            }
            journal
                .journal()
                .record_entry(&NewEntry {
                    op: "mode_changed",
                    path: &entry.path,
                    path_to: Some(previous.to_string().as_bytes()),
                    reversal: if current.is_some() {
                        Reversal::SetModeBack
                    } else {
                        Reversal::Irreversible
                    },
                    reversal_ref: current.map(|m| m.to_string().into_bytes()).as_deref(),
                    actor,
                    undoes_seq: Some(entry.seq),
                    batch_id: batch,
                })
                .await
                .map_err(Error::from)?;
            Ok(Reverted::Done)
        }

        // Undo of a Trashed: restore to the original. The original must be
        // FREE.
        "restore_trash" => {
            match is_free(provider, &path).await {
                Ok(true) => {}
                Ok(false) => return Ok(Reverted::blocked(entry.seq, OCCUPIED)),
                Err(e) => return Ok(Reverted::blocked(entry.seq, e)),
            }
            let res = match entry.reversal_ref.as_deref() {
                // Trash that NAMED its destination (logical, or
                // `norte-vfs-local`'s freedesktop one): restored from that
                // exact path, without guessing. `restore_from` and not
                // `rename` because a trash can have metadata next to the
                // payload that travels with it (freedesktop's
                // `.trashinfo`); the trait's default IS the rename, so the
                // logic doesn't change behavior.
                Some(dest_bytes) => {
                    let dest = wire(dest_bytes)?;
                    provider.restore_from(&dest, &path).await
                }
                // Native trash: restore by original path.
                None => provider.restore_trashed(&path).await,
            };
            if let Err(e) = res {
                return Ok(Reverted::blocked(entry.seq, e));
            }
            // Compensation: the node reappeared at `path` (a creation).
            journal
                .journal()
                .record_entry(&NewEntry {
                    op: "created",
                    path: &entry.path,
                    path_to: None,
                    reversal: Reversal::Delete,
                    reversal_ref: None,
                    actor,
                    undoes_seq: Some(entry.seq),
                    batch_id: batch,
                })
                .await
                .map_err(Error::from)?;
            Ok(Reverted::Done)
        }

        // Unknown reversal tag (journal from a newer core): treat as an
        // honest block, don't guess.
        _ => Ok(Reverted::blocked(entry.seq, Error::Unsupported)),
    }
}

/// The inverse step of a batch entry, with the `seq` it compensates.
struct Inverse {
    /// Base name the node has NOW (the entry's `path`).
    from: Vec<u8>,
    /// Base name it goes back to (the entry's `path_to`).
    to: Vec<u8>,
    /// The entry this step reverts.
    seq: i64,
}

/// The inverse chain of a batch: for each entry, from where the node is NOW
/// to where it was. In the same LIFO order it arrives in, which is exactly
/// the order it has to be applied in.
///
/// Also checks the batch's two structural preconditions along the way: it's
/// all `rename_back` and it all lives in ONE directory (that's how
/// [`crate::Engine::rename_batch`] writes it). A journal that says otherwise
/// is corrupt or comes from a core we don't know: it blocks, it doesn't
/// guess — `Err` is always a `Reverted::Blocked` with the culprit `seq`,
/// never an error that kills the whole session.
fn inverse_chain(unit: &[JournalEntry], dir: &VPath) -> Result<Vec<Inverse>, Reverted> {
    let mut steps: Vec<Inverse> = Vec::with_capacity(unit.len());
    for e in unit {
        let bad = || Reverted::blocked(e.seq, Error::InvalidPath);
        if e.reversal.as_str() != Reversal::RenameBack.as_str() {
            tracing::error!(
                seq = e.seq,
                "batch entry with a reversal that isn't a rename"
            );
            return Err(Reverted::blocked(e.seq, Error::Unsupported));
        }
        let now = wire(&e.path).map_err(|_| bad())?;
        let before = wire(e.path_to.as_deref().ok_or_else(bad)?).map_err(|_| bad())?;
        if now.parent().as_ref() != Some(dir) || before.parent().as_ref() != Some(dir) {
            tracing::error!(seq = e.seq, "batch entry outside the batch's directory");
            return Err(bad());
        }
        let (Some(from), Some(to)) = (now.file_name(), before.file_name()) else {
            return Err(bad());
        };
        steps.push(Inverse {
            from: from.as_bytes().to_vec(),
            to: to.as_bytes().to_vec(),
            seq: e.seq,
        });
    }
    Ok(steps)
}

/// Can the WHOLE inverse chain be applied against the current listing?
///
/// Pure simulation over the directory's comparison keys ([`name_key`]): a
/// step needs its origin present and its destination free, and each step
/// frees and occupies names for the next one. It's what makes the batch
/// all-or-nothing: the answer comes BEFORE touching the provider, so one
/// irreversible member leaves the whole batch untouched instead of halfway.
///
/// A rename that only changes CASE (`Foo → foo` in a directory that
/// doesn't tell them apart) has origin and destination with the same key:
/// it neither frees nor occupies, and its "occupied" destination is itself.
///
/// **Answers ALL-OR-NOTHING, not no-clobber.** It's a snapshot of the
/// listing, and between the snapshot and the first `rename` there's room
/// for another task (the undo is queued with its own key, so the scheduler
/// doesn't serialize it against an `fs.*` on the same directory). What
/// keeps it from overwriting is the provider's `rename`: atomic on local
/// (`renameat2(NOREPLACE)`), check-then-act on sftp and object — the same
/// window the batch's OUTBOUND leg already has, which also plans over a
/// listing and then renames bare. Closing it is a job for both halves
/// together, not this one alone.
///
/// **Blocking at two levels, not one (#128).** A directory that does NOT
/// normalize (ext4) can have `café` NFC and `café` NFD as two different
/// files; both share [`name_key`] but not the bytes. A batch that moved the
/// NFD one, whose undo wants to restore it, would run into the NFC twin and
/// read as "destination occupied" — a file the rename would never touch,
/// blocking the reversion of another one, and with strict LIFO that took
/// down the undo of everything earlier in the session too.
///
/// That's why this function tracks TWO collections: `occupied_bytes` (the
/// EXACT names present) decides the block, and `occupied_keys` (the
/// [`name_key`] keys present, with their count — two files can share a key)
/// decides whether a step's ORIGIN is still there, the same criterion as
/// before. A twin that only shares a key no longer blocks the destination;
/// an occupant that shares the EXACT bytes still blocks the same as ever.
///
/// This is safe — and not merely optimistic — because what really keeps it
/// from overwriting is the provider's `rename`, not this simulation, and
/// all three providers compare exact bytes at that point: local with
/// `renameat2(RENAME_NOREPLACE)` (atomic, compares the directory entry as
/// is), sftp with a prior `stat` on the remote path as is (`provider.rs`,
/// `rename`: `if self.exists(&to_r)`), and object with `ensure_absent` on
/// the exact key (`provider.rs`, `rename`) — none of the three does a
/// stat/lookup aware of NFC/NFD folding. Relaxing the block to exact bytes,
/// then, doesn't open a window the real `rename` would have let through: if
/// the bytes match something real, the provider rejects it just the same
/// (atomic on local, with the same TOCTOU window as always on sftp/object —
/// this function never promised to close it, see above).
///
/// A folding by UPPERCASE/lowercase is different: on a directory that truly
/// doesn't tell case apart, two names that only differ in case can't be two
/// separate entries — they're the same file, and the filesystem itself
/// already resolves the lookup by folding case before it reaches `rename`.
/// A real listing of such a directory can never bring back two different
/// bytes under the same key by that route; the case that does happen for
/// real, and the only one this change relaxes, is NFC/NFD in a directory
/// that doesn't normalize.
fn feasible(steps: &[Inverse], listing: &[Vec<u8>], caps: NameCaps) -> Result<(), (usize, Error)> {
    let mut occupied_bytes: HashSet<Vec<u8>> = listing.iter().cloned().collect();
    let mut occupied_keys: HashMap<Vec<u8>, u32> = HashMap::new();
    for n in listing {
        *occupied_keys
            .entry(name_key(n, caps).into_owned())
            .or_insert(0) += 1;
    }
    for (i, s) in steps.iter().enumerate() {
        let fk = name_key(&s.from, caps).into_owned();
        if !occupied_keys.contains_key(&fk) {
            return Err((i, Error::NotFound));
        }
        if s.from != s.to {
            // The block is by EXACT bytes, not by key: a twin that only
            // shares `name_key` is not the file this step would touch (see
            // the function's rustdoc).
            if occupied_bytes.contains(&s.to) {
                return Err((i, OCCUPIED));
            }
            // The step both frees AND occupies at BOTH levels: leaving one
            // out of sync would make the NEXT step of this same simulation
            // see an origin that's no longer there, or a free destination
            // that's actually still occupied.
            occupied_bytes.remove(&s.from);
            occupied_bytes.insert(s.to.clone());
            let tk = name_key(&s.to, caps).into_owned();
            if fk != tk {
                if let Some(count) = occupied_keys.get_mut(&fk) {
                    // `occupied_keys.contains_key(&fk)` was already checked
                    // above in this SAME iteration, and nothing in between
                    // touches it: the count can't be 0 here.
                    // `saturating_sub` only keeps a future break of that
                    // invariant from becoming a silent underflow; the
                    // `debug_assert` is what would make it LOUD in tests.
                    debug_assert!(*count > 0, "key {fk:?} counted as zero");
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        occupied_keys.remove(&fk);
                    }
                }
                *occupied_keys.entry(tk).or_insert(0) += 1;
            }
        }
    }
    Ok(())
}

/// Reverts a BATCH (`fs.rename_batch`) as ONE unit: whole, or nothing.
///
/// `unit` is the revertible entries sharing a `batch_id`, in LIFO order. The
/// reversal is the inverse chain of its steps — each entry says where the
/// node went from and to, so undoing them in LIFO returns the directory
/// exactly to where it was, temporaries included — and it runs through the
/// SAME executor as the outbound leg ([`crate::rename::exec::run`]): a
/// failure halfway through the undo unwinds what the undo had applied,
/// instead of leaving the batch half-reverted.
///
/// **All-or-nothing.** The chain is simulated first against the CURRENT
/// listing ([`feasible`]); if a single step doesn't fit, none is applied
/// and the unit comes back `Blocked` with the `seq` of the step in the way.
/// It's the property this function exists for: half a permutation undone is
/// worse than none.
///
/// Each step carries `undoes: Some(seq)` from the entry it reverts, so no
/// compensation looks like a new revertible mutation, and all of them go
/// under a FRESH `batch_id`: a batch's undo is itself a batch.
///
/// What the executor could not unwind (`stuck`) and the compensations lost
/// along the way get dumped into `report`: they're batch detail the
/// session's [`UndoReport`] had nowhere to count, and staying quiet about
/// them would leave the caller believing the tree came back.
///
/// # Errors
/// ONLY [`Error::Cancelled`], when the token is cancelled between steps and
/// the executor already unwound its own (rule 3). Any other failure —
/// conflict, provider, journal, listing — comes back as `Blocked`/`Stuck`,
/// so the report says WHERE instead of dying with a bare error and a whole
/// session left untried.
///
/// # Panics
/// Only for poisoning of a report `Mutex` (another thread panicked while
/// holding it), same criterion as the rest of the core.
#[tracing::instrument(
    skip_all,
    fields(task_id = %task_id, batch = unit.len(), seq = unit.first().map_or(0, |e| e.seq))
)]
pub(crate) async fn revert_batch(
    provider: &dyn Provider,
    journal: &Arc<SqliteJournal>,
    unit: &[JournalEntry],
    actor: &Actor,
    cancel: &CancellationToken,
    task_id: TaskId,
    report: &Mutex<UndoReport>,
) -> Result<Reverted, Error> {
    let Some(first) = unit.first() else {
        // Impossible: `undo_units` never produces an empty unit.
        return Ok(Reverted::Done);
    };
    // The unit comes from `revertible_for`, which filters by actor, and
    // from `undo_units`, which groups by `batch_id`. If that ever stopped
    // being true, this executor would revert HALF a batch believing it
    // whole — which is exactly what can't happen. Asserted where it's
    // consumed.
    debug_assert!(
        unit.iter().all(|e| e.batch_id == first.batch_id
            && e.actor_kind == first.actor_kind
            && e.actor_id == first.actor_id),
        "an undo unit is ONE batch of ONE actor",
    );
    let Some(dir) = wire(&first.path).ok().and_then(|p| p.parent()) else {
        return Ok(Reverted::blocked(first.seq, Error::InvalidPath));
    };
    let steps = match inverse_chain(unit, &dir) {
        Ok(steps) => steps,
        Err(blocked) => return Ok(blocked),
    };

    // The DIRECTORY is asked, not the provider (ADR 0054): the batch was
    // planned with this directory's folding, and undoing it with another
    // one is what makes `feasible` declare viable an inverse step the
    // filesystem is going to collapse. A failure here blocks the unit and
    // says so, like the listing failure below — it never kills the undo
    // session.
    let caps = match provider.capabilities_at(&dir).await {
        Ok(caps) => caps,
        Err(error) => return Ok(Reverted::blocked(first.seq, error)),
    };
    if caps.flags.contains(CapabilityFlags::READ_ONLY) {
        return Ok(Reverted::blocked(first.seq, Error::Unsupported));
    }
    let name_caps = NameCaps::from_capabilities(caps);
    // A listing failure does NOT kill the session: it blocks this unit and
    // the report says which. Dying here would return an empty
    // `UndoReport`, with no `seq` or reason, and with everything older in
    // the session left untried — including the silly case of a directory
    // that's since grown past the plannable cap.
    let listing = match crate::engine::list_base_names(provider, &dir).await {
        Ok(names) => names,
        Err(error) => return Ok(Reverted::blocked(first.seq, error)),
    };
    if let Err((i, error)) = feasible(&steps, &listing, name_caps) {
        // Step i doesn't fit → NOTHING is applied. The `seq` is that of
        // THAT step.
        tracing::info!(
            seq = steps[i].seq,
            error = %error,
            "the batch can't be undone whole: leaving it intact",
        );
        return Ok(Reverted::blocked(steps[i].seq, error));
    }

    let planned: Vec<PlannedStep> = steps
        .iter()
        .enumerate()
        .map(|(i, s)| {
            Ok(PlannedStep {
                from: dir.join(seg(&s.from)?),
                to: dir.join(seg(&s.to)?),
                // The undo's "pair" is the entry it reverts: that way the
                // executor's report's `failed_pair` indexes back into
                // `steps` and out comes a `seq`. Saturating the index would
                // break that correspondence, so it's rejected instead of
                // aliased (the wire's pair cap makes it unreachable, but
                // the coupling is still stated).
                pair_index: u32::try_from(i).map_err(|_| Error::LimitExceeded {
                    limit: Error::LIMIT_ENTRIES.into(),
                })?,
                undoes: Some(s.seq),
            })
        })
        .collect::<Result<_, Error>>()?;

    let batch_id = match journal.journal().alloc_batch().await {
        Ok(id) => id,
        Err(e) => return Ok(Reverted::blocked(first.seq, Error::from(e))),
    };
    let recorder = BatchJournal {
        journal: Arc::clone(journal),
        actor: actor.clone(),
        batch_id,
    };
    // The executor publishes progress step by step and the undo counts it
    // by units: giving it the task's reporter would let it overwrite its
    // `entries_total`. This emitter exists only to satisfy the signature;
    // its snapshots reach nobody (the receiver is dropped right here).
    let (progress, _rx) = ProgressReporter::new(task_id, TaskKind::Undo);
    let batch_report = Mutex::new(BatchReport::default());
    let outcome = crate::rename::exec::run(
        provider,
        &recorder,
        &planned,
        cancel,
        &progress,
        &batch_report,
    )
    .await;

    let snapshot = batch_report.lock().expect("batch report lock").clone();
    interpret(outcome, &snapshot, &steps, first.seq, report)
}

/// Translates what the executor did into a [`Reverted`], dumping into
/// `report` the detail the session's [`UndoReport`] had nowhere to count.
///
/// Any `seq` won't do: the executor's report talks in `pair_index`, which
/// gets indexed back into `steps` here to name the specific ENTRY.
///
/// # Panics
/// Only for poisoning of the report's `Mutex`.
fn interpret(
    outcome: Result<(), Error>,
    snapshot: &BatchReport,
    steps: &[Inverse],
    fallback_seq: i64,
    report: &Mutex<UndoReport>,
) -> Result<Reverted, Error> {
    let seq_of = |i: u32| {
        steps
            .get(i as usize)
            .map_or(fallback_seq, |s: &Inverse| s.seq)
    };
    if snapshot.compensations_lost > 0 {
        tracing::error!(
            lost = snapshot.compensations_lost,
            "undo reversals applied without compensating: the session will block here",
        );
        report.lock().expect("undo report lock").compensations_lost += snapshot.compensations_lost;
    }
    if let Some(s) = snapshot.stuck.clone() {
        tracing::error!(
            pair_index = s.pair_index,
            still_applied = s.still_applied,
            "undoing the batch got stuck halfway: the tree did NOT fully come back",
        );
        let (seq, error) = (seq_of(s.pair_index), s.error.clone());
        report.lock().expect("undo report lock").batch_stuck = Some(s);
        // The tree did NOT come back: this FAILS the task. `Blocked` would
        // leave it `Completed`, promising a restored tree — and under
        // cancellation that's exactly the lie the executor avoids by
        // returning an error other than `Cancelled` when its rollback gets
        // stuck.
        return Ok(Reverted::Stuck { seq, error });
    }
    match outcome {
        Ok(()) => Ok(Reverted::Done),
        // Cancellation: the executor already unwound what it had applied,
        // and the task has to end `Cancelled` like any other (rule 3).
        Err(Error::Cancelled) => Err(Error::Cancelled),
        // Anything else is a CLEAN block: without `stuck`, the executor
        // returned the tree exactly to how it was, so strict LIFO stops
        // here with the `seq` of the step that failed.
        Err(error) => Ok(Reverted::blocked(
            snapshot.failed_pair.map_or(fallback_seq, seq_of),
            error,
        )),
    }
}

/// Is this unit a SYNC batch (`sync.apply`) and not a rename one
/// (`fs.rename_batch`)?
///
/// The journal has no column saying which method wrote a batch — and this
/// task doesn't change its schema — so the distinction comes from the
/// SHAPE of the entries, which is what really decides what undo can apply:
///
/// - a rename batch is ALL `renamed`/`rename_back` (that's how
///   [`crate::rename::exec::run`] writes it, and its undo returns the
///   whole permutation or none of it);
/// - a sync batch is the THREE shapes from `crate::sync::exec`'s table, and
///   only those: `created` (reversal `delete` or `irreversible`), `trashed`
///   (`restore_trash`) and `removed` (`irreversible`).
///
/// **The check is POSITIVE, and that's where its safety value is.** "Not a
/// rename batch" is not "is a sync one": with the negative criterion,
/// rewriting a rename batch's `reversal` column to `delete` would move it
/// from the path that REFUSES it ([`revert_batch`] blocks everything that
/// isn't `rename_back`) to one that trashes each entry's `path` — which in
/// a rename is the DESTINATION. Requiring `op` too means two columns have
/// to be rewritten, and a future shape this core doesn't know falls on the
/// refusing side instead of the acting one.
///
/// A MIXED unit isn't a sync one either, for the same reason: it falls into
/// [`revert_batch`], which blocks it without guessing.
fn is_sync_unit(unit: &[JournalEntry]) -> bool {
    !unit.is_empty()
        && unit.iter().all(|e| {
            e.batch_id.is_some()
                && matches!(e.op.as_str(), "created" | "trashed" | "removed")
                && matches!(
                    e.reversal.as_str(),
                    "delete" | "restore_trash" | "irreversible"
                )
        })
}

/// The `op` used to journal an ORGANIZE move (phase 8).
///
/// It's not `renamed`, and the difference is one of correctness, not
/// cosmetics: a `renamed` batch lives in ONE directory and gets undone by
/// `revert_batch`, which builds the inverse chain from the first entry's
/// parent. An organize batch moves into SUBDIRECTORIES — there's no common
/// parent — and also brings the `created` entries for the folders it made.
/// With the same `op`, that batch would fall into the rename executor and
/// get undone against a directory that isn't its own.
///
/// It's painted as-is in the timeline and the audit, which is the honest
/// thing: organizing is what happened.
pub const OP_ORGANIZED: &str = "organized";

/// Is this unit an ORGANIZE batch (phase 8)?
///
/// It's enough for ONE entry to carry [`OP_ORGANIZED`]: no one else writes
/// that `op`. It also checks that the whole batch has the expected shape —
/// move or create, with their reversals — so a tampered `batch_id` can't
/// drag an entry of another kind down this path.
fn is_organize_unit(unit: &[JournalEntry]) -> bool {
    !unit.is_empty()
        && unit.iter().any(|e| e.op == OP_ORGANIZED)
        && unit.iter().all(|e| {
            e.batch_id.is_some()
                && matches!(e.op.as_str(), OP_ORGANIZED | "created")
                && matches!(
                    e.reversal.as_str(),
                    "rename_back" | "delete" | "irreversible"
                )
        })
}

/// Is this unit a PERMISSIONS batch (#315)?
///
/// A recursive `fs.set_mode` groups its n nodes under a batch so an audit
/// can read ONE action where the human did one. But it's not a rename batch
/// — there's no permutation to undo whole nor temporaries to cross — nor a
/// sync one: each entry carries its own previous mode and is undone on its
/// own.
fn is_mode_unit(unit: &[JournalEntry]) -> bool {
    !unit.is_empty()
        && unit
            .iter()
            .all(|e| e.batch_id.is_some() && e.op == "mode_changed")
}

/// Undoes a permissions batch: entry by entry, in LIFO, and whatever can't
/// be done is SAID (#315).
///
/// Unlike a rename batch, here "whole or nothing" would be worse: the modes
/// are independent — none depends on another having already come back —
/// and a tree of a hundred thousand files where just one can't be touched
/// would come back whole minus that one, which is exactly what the reader
/// wants. A node's `Blocked` stops the SESSION the same as in any other
/// unit; what it doesn't do is throw away the ones that already came back.
async fn revert_mode_batch(
    provider: &dyn Provider,
    journal: &Arc<SqliteJournal>,
    unit: &[JournalEntry],
    actor: &Actor,
    cancel: &CancellationToken,
    report: &Mutex<UndoReport>,
) -> Result<Reverted, Error> {
    // ALL the batch's entries have to live on the same provider, and that's
    // checked before touching anything. Without this, a `batch_id` with
    // paths on two hosts — which `fs.set_mode` accepts, because it resolves
    // a provider PER PATH — would have the caller resolve ONE provider from
    // the first entry and run the rest against it: the policy evaluated one
    // machine and the effect lands on another. Same guard `revert_sync_batch`
    // has, and for the same reason.
    let Some(first) = unit.first() else {
        return Ok(Reverted::Accounted);
    };
    // And NOT `one_provider`, even though it's the same question: that one
    // also looks at `reversal_ref` as if it were a path, and in a
    // `mode_changed` that field is the PREVIOUS mode in decimal ASCII
    // (`journal.rs`). Passing it through `wire` would block every
    // permissions batch with an `InvalidPath` that means nothing.
    let origin = match wire(&first.path) {
        Ok(p) => p,
        Err(e) => return Ok(Reverted::blocked(first.seq, e)),
    };
    for e in unit {
        match wire(&e.path) {
            Ok(p) if p.scheme() == origin.scheme() && p.authority() == origin.authority() => {}
            Ok(_) => {
                // A batch with paths on two machines: the caller resolves
                // ONE provider from the first entry, so running would apply
                // the rest against another host. The policy evaluated one
                // machine and the effect would land on another.
                return Ok(Reverted::blocked(e.seq, Error::InvalidPath));
            }
            Err(err) => return Ok(Reverted::blocked(e.seq, err)),
        }
    }
    // The COMPENSATION's batch is its own: the entries this undo writes are
    // another action, and mixing them with the original batch would make a
    // second undo believe they're part of it.
    let batch = journal.journal().alloc_batch().await.ok();
    // From the LEAF to the root: the outbound walk went top-down, so
    // undoing the directory first could leave it without its execute bit
    // while its children are still to be reverted — and then they'd never
    // be reached. The order isn't inherited from the query's `ORDER BY`:
    // it's set here, like `revert_sync_batch` does.
    let mut ordered: Vec<&JournalEntry> = unit.iter().collect();
    ordered.sort_by_key(|e| std::cmp::Reverse(e.seq));
    let mut done: u64 = 0;
    let mut blocked_unit: Option<Reverted> = None;
    for entry in ordered {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        match revert_entry(provider, journal, entry, actor, batch, cancel).await? {
            Reverted::Done => done += 1,
            // **NOT whole-or-nothing**, unlike a rename batch: the modes
            // are independent — none depends on another having already come
            // back — so a tree of a hundred thousand files where one can't
            // be touched comes back whole minus that one, which is what the
            // reader wants. Half a permutation undone WOULD be an invalid
            // state; half a permissions reversion isn't.
            other => {
                note_unreverted(report, &entry.path);
                if blocked_unit.is_none() {
                    blocked_unit = Some(other);
                }
            }
        }
    }
    // THIS function keeps the count, not the caller, which would add up
    // the whole unit's members: here each node may or may not come back.
    report.lock().expect("healthy report lock").undone += done;
    Ok(blocked_unit.unwrap_or(Reverted::Accounted))
}

/// The entries from `seqs` that are ALREADY undone according to the journal
/// as of NOW (#358): what was chosen when an undo was requested might have
/// been undone by someone else meanwhile.
///
/// Asked ONCE, for the whole plan, as soon as the Task gets its turn
/// (`Engine::undo_in_progress`): from then on only it writes undo
/// compensations, so the answer doesn't go stale while it runs. Asking per
/// unit would cost a journal scan for each one.
///
/// # Errors
/// The journal's, as [`Error`].
pub(crate) async fn undone(journal: &SqliteJournal, seqs: &[i64]) -> Result<HashSet<i64>, Error> {
    Ok(journal
        .journal()
        .undone_among(seqs)
        .await
        .map_err(Error::from)?
        .into_iter()
        .collect())
}

/// What state a unit chosen for undo is in NOW (#358).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Validity {
    /// None of it is undone: revert it exactly as chosen.
    Whole(Vec<JournalEntry>),
    /// Already fully undone: nothing left to do with it.
    Undone,
    /// Part of it already came back, in a unit that knows how to revert in
    /// pieces (sync, organize, permissions): continue with the REST, which
    /// is exactly what a new undo would choose.
    InPart(Vec<JournalEntry>),
    /// Part of it already came back, in a unit that is all-or-nothing (a
    /// rename batch): it can neither continue nor be skipped. Continuing
    /// would revert half the batch; skipping it would let the LIFO cross a
    /// half-done unit and revert what's under it. The undo STOPS, like any
    /// other block. The `seq` is that of its first entry.
    Stop(i64),
}

/// Classifies `unit` against what's already been undone (see [`Validity`]).
///
/// Why "skip if something is undone" isn't enough: an undo that stopped
/// halfway through a sync unit (a block, a cancellation) leaves that unit
/// halfway, and the undo waiting behind it, by skipping it, would continue
/// with the OLDER units — below something half-returned, which is exactly
/// what strict LIFO exists to prevent.
pub(crate) fn validity(unit: Vec<JournalEntry>, already_undone: &HashSet<i64>) -> Validity {
    let how_many = unit
        .iter()
        .filter(|e| already_undone.contains(&e.seq))
        .count();
    if how_many == 0 {
        return Validity::Whole(unit);
    }
    if how_many == unit.len() {
        return Validity::Undone;
    }
    if is_sync_unit(&unit) || is_organize_unit(&unit) || is_mode_unit(&unit) {
        return Validity::InPart(
            unit.into_iter()
                .filter(|e| !already_undone.contains(&e.seq))
                .collect(),
        );
    }
    Validity::Stop(unit.first().map_or(0, |e| e.seq))
}

/// Reverts ONE undo unit, whatever kind it is.
///
/// The only place that decides which undo a unit gets, and it lives here —
/// next to the three functions it dispatches to — and not in
/// [`crate::Engine::undo_session_for`], which only needs to know what to
/// count. It's decided by the SHAPE of the entries ([`is_sync_unit`]), not
/// the unit's size: a one-step sync batch also needs the path that knows
/// how to skip what's irreversible.
///
/// # Errors
/// Those of [`revert_entry`], [`revert_batch`] and [`revert_sync_batch`]:
/// cancellation (rule 3) and failure persisting a compensation (rule 4).
pub(crate) async fn revert_unit(
    provider: &dyn Provider,
    journal: &Arc<SqliteJournal>,
    unit: &[JournalEntry],
    actor: &Actor,
    cancel: &CancellationToken,
    task_id: TaskId,
    report: &Mutex<UndoReport>,
) -> Result<Reverted, Error> {
    if is_sync_unit(unit) {
        return revert_sync_batch(provider, journal, unit, actor, cancel, task_id, report).await;
    }
    // An ORGANIZE batch (phase 8) has the shape of a sync one and not that
    // of a rename batch: its entries don't share a directory — that's the
    // whole point of organizing — and they come mixed with the `created`
    // entries for the new folders. So the SAME executor undoes it, since it
    // already knows how: it checks everything is on one provider, orders in
    // strict LIFO by `seq` and compensates each entry by its reversal.
    //
    // The order comes for free and is what's needed: folders get created
    // BEFORE moving (smaller seq), so in LIFO the files come back first and
    // the folders get deleted afterward, already empty.
    if is_organize_unit(unit) {
        return revert_sync_batch(provider, journal, unit, actor, cancel, task_id, report).await;
    }
    if is_mode_unit(unit) {
        return revert_mode_batch(provider, journal, unit, actor, cancel, report).await;
    }
    match unit.split_first() {
        // A unit of one: the usual path, untouched.
        Some((entry, [])) => revert_entry(provider, journal, entry, actor, None, cancel).await,
        // A unit of several: a rename batch, whole or nothing.
        _ => revert_batch(provider, journal, unit, actor, cancel, task_id, report).await,
    }
}

/// All the unit's entries live on the SAME provider (scheme and authority),
/// or the unit isn't touched.
///
/// `Err` is always a [`Reverted::Blocked`] with the culprit `seq`, never an
/// error that kills the session: same criterion as [`inverse_chain`].
///
/// **`reversal_ref` isn't always a path**, and that's why `reversal` is
/// checked before reading it: in a `delete` it carries the created node's
/// identity (ADR 0152, `<volume>:<index>`), which doesn't parse as a
/// `VPath` and would block the whole unit with an `InvalidPath`. Today it
/// can't reach here — a `created` from the observer doesn't carry a
/// `batch_id`, and these units require one — but the day a batch brings
/// one, this won't swallow it anymore. Same care [`revert_mode_batch`] had
/// to take with `set_mode_back`.
fn one_provider(unit: &[JournalEntry], first: &JournalEntry) -> Result<(), Reverted> {
    let origin = wire(&first.path).map_err(|e| Reverted::blocked(first.seq, e))?;
    for e in unit {
        let ref_is_path = e.reversal != "delete";
        let refs = [
            Some(e.path.as_slice()),
            e.reversal_ref.as_deref().filter(|_| ref_is_path),
        ];
        for bytes in refs.into_iter().flatten() {
            let path = wire(bytes).map_err(|err| Reverted::blocked(e.seq, err))?;
            if path.scheme() != origin.scheme() || path.authority() != origin.authority() {
                tracing::error!(
                    seq = e.seq,
                    "batch entry pointing at a different provider than the rest of the batch",
                );
                return Err(Reverted::blocked(e.seq, Error::InvalidPath));
            }
        }
    }
    Ok(())
}

/// The paths whose restoration this undo CANNOT get right, and that's why
/// they aren't touched from either side.
///
/// An overwrite with trash leaves `trashed(P)` + `created(P)`. With a
/// LOGICAL trash the `trashed` saves the exact payload in `reversal_ref`
/// and restoring it is an unambiguous rename. With the system's NATIVE
/// trash there's no handle: `restore_trashed(P)` picks the most recent item
/// among the ones whose ORIGINAL path is `P` (`norte-vfs-local`,
/// `restore_trashed`) — and by the time the undo gets there, the most
/// recent one is the one it just buried itself when undoing the `created`.
/// It would restore the NEW file and leave the user's original inside the
/// trash, counting it as a success.
///
/// Truly undoing that needs `trash()` to return the item's identifier,
/// which is `norte-vfs` debt (noted in `norte-vfs-local`). Until then the
/// pair is left UNTOUCHED — the synced file stays where it is and the
/// original stays in the trash, where the user takes it out by hand — and
/// the report gives the path. Burying the new one and not restoring the
/// old one would leave the path EMPTY, which is worse than not touching
/// anything.
fn ambiguous_restores(unit: &[JournalEntry]) -> HashSet<&[u8]> {
    let buried: HashSet<&[u8]> = unit
        .iter()
        .filter(|e| {
            e.reversal.as_str() == Reversal::RestoreTrash.as_str() && e.reversal_ref.is_none()
        })
        .map(|e| e.path.as_slice())
        .collect();
    unit.iter()
        .filter(|e| {
            e.reversal.as_str() == Reversal::Delete.as_str() && buried.contains(e.path.as_slice())
        })
        .map(|e| e.path.as_slice())
        .collect()
}

/// Notes a path that did NOT come back, capped.
///
/// The counter it belongs to was already kept by the caller: this is the
/// named sample, not the count.
///
/// # Panics
/// INVARIANT: the report's `Mutex` only gets poisoned if another thread
/// panicked while holding it, which is unrecoverable — same criterion as
/// the rest of the core's locks.
fn note_unreverted(report: &Mutex<UndoReport>, path: &[u8]) {
    let mut report = report.lock().expect("undo report lock");
    if report.unreverted_paths.len() < UNDO_MAX_UNREVERTED_PATHS {
        report.unreverted_paths.push(path.to_vec());
    }
}

/// Reverts a SYNC batch (`sync.apply`): whatever it can, in DESCENDING
/// `seq` order, naming what it can't.
///
/// **It is not [`revert_batch`], and the difference is the whole
/// contract.** Half a permutation undone is not any state, so a rename
/// batch comes back whole or is not touched. Half a sync undone IS a
/// state: it's the tree from before with some of the files already
/// returned. So here an irreversible entry doesn't refuse the batch — that
/// would make it all-or-nothing, and would leave 9,999 reversible steps
/// hostage to one that isn't — instead it's skipped, counted and named in
/// [`UndoReport::unreverted_paths`].
///
/// # Why the order is descending `seq`
/// It's the order [`revertible_for`](crate::journal::Journal::revertible_for)
/// returns, and it's enforced here again so the property lives in the
/// function that depends on it. What that order resolves, with the shapes
/// [`crate::sync::exec`] emits:
///
/// - **The pair of an `Overwrite` with LOGICAL trash** (`trashed` then
///   `created`): deletes what was created BEFORE restoring what was
///   buried, which is the only sequence where `restore_trash` finds its
///   path free. Over a NATIVE trash a free path isn't enough and the pair
///   isn't touched: see [`ambiguous_restores`].
/// - **A `CreateDir` and the copies inside it**: the walk is pre-order, so
///   the directory is journaled BEFORE its children and, the other way
///   around, it gets emptied before being sent to the trash. That it ends
///   up empty is NOT assumed: a created directory's reversal checks it has
///   no children and blocks if it does, because the trash would take
///   whatever was inside it too.
///
/// The rest of the shapes (`Copy`, `DeleteTree`, the irreversible ones)
/// touch one path each and aren't ordered against each other.
///
/// # What this undo CANNOT do
/// - An `irreversible` entry has no reversal to run: it's an `Overwrite` or
///   a `DeleteTree` onto a destination without trash, and what was there is
///   no longer anywhere. Counted in [`UndoReport::skipped_irreversible`].
/// - A `created` on a provider WITHOUT trash is skipped too (#65): its
///   reversal is a delete, and permanently deleting "whatever lives at that
///   path today" could destroy later human work — ADR 0152's identity check
///   rules out that it's ANOTHER node, not that it's the same one with
///   different content. In other words: **on a destination without trash,
///   a `Copy` doesn't get undone either**, even though the plan shows it
///   with a `Delete` reversal. Counted in
///   [`UndoReport::skipped_created_no_trash`].
/// - An overwrite whose trash gives no recoverable destination: the pair is
///   left untouched, with its path named ([`ambiguous_restores`]).
/// - Drift (the path changed under the undo) blocks THAT entry, the first
///   one is noted in [`UndoReport::blocked`] and the session stops there —
///   but the batch's other entries are tried anyway, which is the rule
///   above. Continuing is safe because each reversal checks its own thing
///   before acting, and where that check wasn't enough it's been added: a
///   created directory isn't buried if it has children, and an ambiguous
///   overwrite isn't touched. What it does NOT promise is independence
///   between an overwrite's two halves: if the first buries what was
///   created and the second fails to restore what was buried, the path is
///   left EMPTY — fully recoverable from the trash, fully named in the
///   report, but empty.
///
/// # Errors
/// ONLY [`Error::Cancelled`] (rule 3, the token is checked between
/// entries) and the JOURNAL's error when a compensation could not be
/// persisted after its effect already happened (rule 4). The latter adds
/// to [`UndoReport::compensations_lost`] and fails the Task: with the
/// journal failing, continuing to touch the tree means writing mutations
/// nobody records.
///
/// # Panics
/// Only for poisoning of the report's `Mutex`, same criterion as the rest
/// of the core.
#[tracing::instrument(
    skip_all,
    fields(task_id = %task_id, batch = unit.len(), seq = unit.first().map_or(0, |e| e.seq))
)]
pub(crate) async fn revert_sync_batch(
    provider: &dyn Provider,
    journal: &Arc<SqliteJournal>,
    unit: &[JournalEntry],
    actor: &Actor,
    cancel: &CancellationToken,
    task_id: TaskId,
    report: &Mutex<UndoReport>,
) -> Result<Reverted, Error> {
    let Some(first) = unit.first() else {
        // Impossible: `undo_units` never produces an empty unit.
        return Ok(Reverted::Accounted);
    };
    // The unit comes from `revertible_for` (filters by actor) and from
    // `undo_units` (groups by `batch_id`). Asserted where it's consumed,
    // like in `revert_batch`.
    debug_assert!(
        unit.iter().all(|e| e.batch_id == first.batch_id
            && e.actor_kind == first.actor_kind
            && e.actor_id == first.actor_id),
        "an undo unit is ONE batch of ONE actor",
    );
    // ALL entries have to live on ONE provider, and it's really checked.
    // The caller resolves ONE (from the first path) and uses it for all of
    // them; `revert_batch` got this for free — `inverse_chain` requires all
    // of them to hang off the same directory, and a `VPath`'s parent
    // carries scheme and authority — but here there's no common directory.
    // Without this, a tampered `batch_id` runs one host's path against
    // another's provider: the policy evaluated one machine and the effect
    // lands on another. Along the way, this pass is what makes an `Err`
    // from `revert_entry` mean ONLY "the compensation couldn't be
    // persisted": the `wire()` calls that return `Err(InvalidPath)` there
    // can no longer fail.
    if let Err(refused) = one_provider(unit, first) {
        return Ok(refused);
    }
    // The order correctness needs is imposed HERE (see the rustdoc), not
    // inherited from whoever listed the entries' `ORDER BY`.
    let mut ordered: Vec<&JournalEntry> = unit.iter().collect();
    ordered.sort_by_key(|e| std::cmp::Reverse(e.seq));
    let ambiguous = ambiguous_restores(unit);

    // Undoing a batch READS as a batch: a FRESH `batch_id` for all the
    // compensations, each with its `undoes_seq`. An id requested and unused
    // (a whole batch irreversible) is just dropped — these are grouping
    // labels, not an auditable counter.
    let batch_id = match journal.journal().alloc_batch().await {
        Ok(id) => id,
        Err(e) => return Ok(Reverted::blocked(first.seq, Error::from(e))),
    };

    let mut blocked: Option<(i64, Error)> = None;
    for entry in ordered {
        // Rule 3: between entries. What's compensated stays compensated
        // and the rest of the batch stays revertible on the next undo.
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        // The pair a NATIVE trash doesn't know how to undo: don't even
        // touch it.
        if ambiguous.contains(entry.path.as_slice()) {
            if entry.reversal.as_str() == Reversal::RestoreTrash.as_str() {
                tracing::error!(
                    seq = entry.seq,
                    "overwrite over a trash without a recoverable destination: the undo can't \
                     tell the buried file apart from the one it would itself bury, so it \
                     touches neither — the original stays in the trash",
                );
                // Named ONCE per pair: the `trashed` entry is the one for
                // the file the user wants back.
                note_unreverted(report, &entry.path);
            }
            if blocked.is_none() {
                blocked = Some((entry.seq, Error::Unsupported));
            }
            continue;
        }
        match revert_entry(provider, journal, entry, actor, Some(batch_id), cancel).await {
            Ok(Reverted::Done) => {
                report.lock().expect("undo report lock").undone += 1;
            }
            Ok(Reverted::SkippedIrreversible) => {
                report
                    .lock()
                    .expect("undo report lock")
                    .skipped_irreversible += 1;
                note_unreverted(report, &entry.path);
            }
            Ok(Reverted::SkippedNoTrash) => {
                report
                    .lock()
                    .expect("undo report lock")
                    .skipped_created_no_trash += 1;
                note_unreverted(report, &entry.path);
            }
            Ok(Reverted::SkippedNotOurs) => {
                report.lock().expect("undo report lock").skipped_not_ours += 1;
                note_unreverted(report, &entry.path);
            }
            // Drift on ONE entry. The first one (larger `seq`, i.e. the
            // most recent mutation) is named and it continues: the others
            // don't depend on it, and each reversal checks its own thing
            // before acting.
            Ok(Reverted::Blocked { seq, error }) => {
                tracing::info!(seq, error = %error, "a sync batch entry didn't come back");
                note_unreverted(report, &entry.path);
                if blocked.is_none() {
                    blocked = Some((seq, error));
                }
            }
            // `revert_entry` doesn't produce this (it's the rename
            // executor's). If it ever did, a half-returned tree is NOT
            // treated as a skip: it's propagated as-is and the Task fails.
            Ok(stuck @ Reverted::Stuck { .. }) => return Ok(stuck),
            Ok(Reverted::Accounted) => {
                debug_assert!(false, "`revert_entry` doesn't account for itself");
            }
            // Rule 4: the effect happened and its compensation did NOT stay
            // durable. The entry keeps looking pending and a later undo
            // will block there; it's the only signal.
            Err(e) => {
                tracing::error!(
                    seq = entry.seq,
                    error = %e,
                    "reversal applied without compensating in a sync batch: the journal doesn't have it",
                );
                report.lock().expect("undo report lock").compensations_lost += 1;
                return Err(e);
            }
        }
    }

    match blocked {
        // Strict at the SESSION level: the batch did what it could and the
        // LIFO stops here, because what came before a drift can no longer
        // be promised.
        Some((seq, error)) => Ok(Reverted::blocked(seq, error)),
        None => Ok(Reverted::Accounted),
    }
}

/// A base name as a `Segment`, or [`Error::InvalidPath`].
///
/// The names come from a journal `VPath`, so they were already legal
/// segments; this checks it again at the one point that rebuilds them
/// (rule 6: the guarantee is checked, not assumed).
fn seg(b: &[u8]) -> Result<norte_proto::Segment, Error> {
    norte_proto::Segment::new(b.to_vec()).map_err(|_| Error::InvalidPath)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(seq: i64, batch: Option<i64>) -> JournalEntry {
        JournalEntry {
            seq,
            ts_ms: 0,
            entry_hash: Vec::new(),
            actor_kind: "user".to_owned(),
            actor_id: None,
            op: "renamed".to_owned(),
            path: Vec::new(),
            path_to: None,
            reversal: "rename_back".to_owned(),
            reversal_ref: None,
            undoes_seq: None,
            batch_id: batch,
        }
    }

    fn shape(units: &[Vec<JournalEntry>]) -> Vec<Vec<i64>> {
        units
            .iter()
            .map(|u| u.iter().map(|e| e.seq).collect())
            .collect()
    }

    /// Lone mutations: one unit each, in the same LIFO order.
    #[test]
    fn lone_entries_stay_one_unit_each() {
        let units = undo_units(vec![entry(3, None), entry(2, None), entry(1, None)]);
        assert_eq!(shape(&units), vec![vec![3], vec![2], vec![1]]);
    }

    /// A contiguous batch arrives whole, and in LIFO order inside.
    #[test]
    fn a_contiguous_batch_is_one_unit() {
        let units = undo_units(vec![
            entry(4, None),
            entry(3, Some(7)),
            entry(2, Some(7)),
            entry(1, Some(7)),
        ]);
        assert_eq!(shape(&units), vec![vec![4], vec![3, 2, 1]]);
    }

    /// THE property: a mutation from another task slipped IN THE MIDDLE of
    /// the batch doesn't split it. With grouping by contiguity this would
    /// give `[[3], [2], [1]]` and the first unit would leave the
    /// permutation halfway.
    #[test]
    fn an_interleaved_entry_does_not_split_the_batch() {
        let units = undo_units(vec![entry(3, Some(7)), entry(2, None), entry(1, Some(7))]);
        assert_eq!(shape(&units), vec![vec![3, 1], vec![2]]);
    }

    /// Two different interleaved batches stay apart: each one whole, each
    /// its own.
    #[test]
    fn two_interleaved_batches_stay_apart() {
        let units = undo_units(vec![
            entry(4, Some(8)),
            entry(3, Some(9)),
            entry(2, Some(8)),
            entry(1, Some(9)),
        ]);
        assert_eq!(shape(&units), vec![vec![4, 2], vec![3, 1]]);
    }

    /// An entry with the shape `sync.apply` writes: no `path_to`, with a
    /// batch, and one of its three reversals.
    fn sync_entry(seq: i64, batch: Option<i64>, op: &str, reversal: &str) -> JournalEntry {
        JournalEntry {
            op: op.to_owned(),
            path: format!("mem:///d/{seq}").into_bytes(),
            reversal: reversal.to_owned(),
            ..entry(seq, batch)
        }
    }

    /// A rename batch does NOT go through the sync path: its undo is
    /// all-or-nothing and that property can't be lost to a dispatch.
    #[test]
    fn a_rename_batch_is_not_a_sync_unit() {
        let unit = vec![entry(2, Some(7)), entry(1, Some(7))];
        assert!(!is_sync_unit(&unit));
    }

    /// A sync batch of ONE entry is still a batch: the dispatch looks at
    /// the shape, not the size.
    #[test]
    fn a_sync_batch_of_one_is_still_a_sync_unit() {
        let unit = vec![sync_entry(1, Some(7), "created", "delete")];
        assert!(is_sync_unit(&unit));
    }

    /// The three shapes the sync executor emits, together.
    #[test]
    fn the_three_shapes_of_a_sync_batch_are_a_sync_unit() {
        let unit = vec![
            sync_entry(3, Some(7), "removed", "irreversible"),
            sync_entry(2, Some(7), "created", "delete"),
            sync_entry(1, Some(7), "trashed", "restore_trash"),
        ];
        assert!(is_sync_unit(&unit));
    }

    /// #358: a unit's currency against what another undo already returned.
    /// Nothing → whole; everything → undone; part of a unit that reverts in
    /// pieces → the REST; part of an all-or-nothing batch → stops.
    #[test]
    fn unit_validity_against_what_is_already_undone() {
        let sync = || {
            vec![
                sync_entry(3, Some(7), "created", "delete"),
                sync_entry(2, Some(7), "created", "delete"),
            ]
        };
        let batch = || vec![entry(5, Some(9)), entry(4, Some(9))];
        let none = HashSet::new();
        assert!(matches!(validity(sync(), &none), Validity::Whole(u) if u.len() == 2));
        assert_eq!(validity(sync(), &HashSet::from([3, 2])), Validity::Undone);
        match validity(sync(), &HashSet::from([3])) {
            Validity::InPart(rest) => {
                assert_eq!(rest.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![2]);
            }
            other => panic!("a sync unit halfway continues with the rest: {other:?}"),
        }
        assert_eq!(
            validity(batch(), &HashSet::from([4])),
            Validity::Stop(5),
            "a rename batch halfway STOPS the LIFO: it's neither continued nor skipped"
        );
        assert_eq!(validity(batch(), &HashSet::from([4, 5])), Validity::Undone);
    }

    /// A MIXED unit is not a sync one: whoever slipped a `created` into a
    /// rename batch would otherwise get the permissive undo for a
    /// permutation — and half a permutation undone is exactly what can't
    /// happen. Falls into `revert_batch`, which blocks it.
    #[test]
    fn a_mixed_unit_is_not_a_sync_unit() {
        let unit = vec![
            sync_entry(2, Some(7), "created", "delete"),
            entry(1, Some(7)),
        ];
        assert!(!is_sync_unit(&unit));
    }

    /// A lone mutation (an `fs.copy`) carries no batch and follows the
    /// usual path, with its compensation carrying no `batch_id`.
    #[test]
    fn a_lone_entry_without_a_batch_is_not_a_sync_unit() {
        let unit = vec![sync_entry(1, None, "created", "delete")];
        assert!(!is_sync_unit(&unit));
    }

    /// Rewriting ONLY the `reversal` column of a rename batch isn't enough
    /// to buy it the permissive undo: the `op` still says `renamed`, and
    /// with the negative criterion ("there's no `rename_back` at all") that
    /// unit would have gone on to trash the DESTINATION of each rename.
    #[test]
    fn a_rename_batch_with_a_rewritten_reversal_is_still_not_a_sync_unit() {
        let unit = vec![
            sync_entry(2, Some(7), "renamed", "delete"),
            sync_entry(1, Some(7), "renamed", "delete"),
        ];
        assert!(!is_sync_unit(&unit));
    }

    /// The pair of an overwrite over NATIVE trash (without `reversal_ref`)
    /// isn't touched from either of its two sides.
    #[test]
    fn an_overwrite_pair_without_a_trash_reference_is_ambiguous() {
        let unit = vec![
            JournalEntry {
                path: b"mem:///d/a.txt".to_vec(),
                ..sync_entry(2, Some(7), "created", "delete")
            },
            JournalEntry {
                path: b"mem:///d/a.txt".to_vec(),
                ..sync_entry(1, Some(7), "trashed", "restore_trash")
            },
        ];
        let ambiguous = ambiguous_restores(&unit);
        assert_eq!(ambiguous.len(), 1);
        assert!(ambiguous.contains(b"mem:///d/a.txt".as_slice()));
    }

    /// With LOGICAL trash the `trashed` knows where to get the file from,
    /// so there's no ambiguity and the pair is undone whole.
    #[test]
    fn an_overwrite_pair_with_a_trash_reference_is_not_ambiguous() {
        let unit = vec![
            JournalEntry {
                path: b"mem:///d/a.txt".to_vec(),
                ..sync_entry(2, Some(7), "created", "delete")
            },
            JournalEntry {
                path: b"mem:///d/a.txt".to_vec(),
                reversal_ref: Some(b"mem:///d/.norte-trash/1/a.txt".to_vec()),
                ..sync_entry(1, Some(7), "trashed", "restore_trash")
            },
        ];
        assert!(ambiguous_restores(&unit).is_empty());
    }

    /// A `trashed` without `reversal_ref` that NOBODY re-creates (a
    /// `DeleteTree`) is restored without ambiguity: the undo doesn't bury
    /// anything at that path, so the trash's most recent item is still its
    /// own.
    #[test]
    fn a_lone_native_trash_entry_is_not_ambiguous() {
        let unit = vec![sync_entry(1, Some(7), "trashed", "restore_trash")];
        assert!(ambiguous_restores(&unit).is_empty());
    }

    /// An entry pointing at ANOTHER provider blocks the unit before
    /// touching anything: the caller resolves a single provider for all of
    /// them.
    #[test]
    fn a_unit_that_spans_two_providers_is_refused() {
        let first = JournalEntry {
            path: b"sftp://a/x".to_vec(),
            ..sync_entry(2, Some(7), "created", "delete")
        };
        let other = JournalEntry {
            path: b"sftp://b/x".to_vec(),
            ..sync_entry(1, Some(7), "created", "delete")
        };
        let unit = vec![first.clone(), other];
        let refused = one_provider(&unit, &first).expect_err("blocked");
        assert!(matches!(
            refused,
            Reverted::Blocked {
                seq: 1,
                error: Error::InvalidPath
            }
        ));
    }

    /// And `reversal_ref` counts the same: it's the path `restore_trash`
    /// EXECUTES on the unit's provider.
    #[test]
    fn a_trash_reference_on_another_provider_is_refused() {
        let first = JournalEntry {
            path: b"mem:///d/a".to_vec(),
            reversal_ref: Some(b"sftp://b/trash/a".to_vec()),
            ..sync_entry(1, Some(7), "trashed", "restore_trash")
        };
        let unit = vec![first.clone()];
        assert!(one_provider(&unit, &first).is_err());
    }

    /// Not even an empty unit, which `undo_units` doesn't produce anyway.
    #[test]
    fn an_empty_unit_is_not_a_sync_unit() {
        assert!(!is_sync_unit(&[]));
    }

    /// The path list has a cap; the COUNTER doesn't. A batch of half a
    /// million irreversible steps can't take down the daemon's memory.
    #[test]
    fn the_unreverted_path_list_is_capped() {
        let report = Mutex::new(UndoReport::default());
        for i in 0..(UNDO_MAX_UNREVERTED_PATHS * 3) {
            note_unreverted(&report, format!("mem:///d/{i}").as_bytes());
        }
        let r = report.lock().expect("lock");
        assert_eq!(r.unreverted_paths.len(), UNDO_MAX_UNREVERTED_PATHS);
        assert_eq!(r.unreverted_paths[0], b"mem:///d/0", "trims from the TAIL");
    }

    const SENSITIVE: NameCaps = NameCaps {
        fold: norte_encoding::FoldMode::None,
    };

    fn inv(from: &[u8], to: &[u8], seq: i64) -> Inverse {
        Inverse {
            from: from.to_vec(),
            to: to.to_vec(),
            seq,
        }
    }

    /// The inverse chain of a permutation (with its temporary) fits whole.
    #[test]
    fn the_inverse_chain_of_a_permutation_is_feasible() {
        let steps = vec![
            inv(b"b", b".norte-rename-0", 3),
            inv(b"a", b"b", 2),
            inv(b".norte-rename-0", b"a", 1),
        ];
        assert!(feasible(&steps, &[b"a".to_vec(), b"b".to_vec()], SENSITIVE).is_ok());
    }

    /// An occupant at the destination of ONE step invalidates the WHOLE
    /// chain, and says which: it's what makes the undo all-or-nothing.
    #[test]
    fn an_occupied_destination_kills_the_whole_chain() {
        let steps = vec![inv(b"y", b"b", 2), inv(b"x", b"a", 1)];
        let listing = [b"x".to_vec(), b"y".to_vec(), b"a".to_vec()];
        let (i, e) = feasible(&steps, &listing, SENSITIVE).expect_err("blocked");
        assert_eq!(i, 1, "the second step is the one that doesn't fit");
        assert_eq!(e, OCCUPIED);
    }

    /// An origin that's no longer there (someone deleted or moved the
    /// file) also blocks the whole batch.
    #[test]
    fn a_vanished_source_kills_the_whole_chain() {
        let steps = vec![inv(b"y", b"b", 2)];
        let (i, e) = feasible(&steps, &[b"x".to_vec()], SENSITIVE).expect_err("blocked");
        assert_eq!(i, 0);
        assert_eq!(e, Error::NotFound);
    }

    /// Undoing a case-only rename in a directory that doesn't tell case
    /// apart: the "occupied" destination is the origin itself, and that's
    /// not a conflict.
    #[test]
    fn undoing_a_case_only_rename_is_not_a_conflict() {
        let insensitive = NameCaps {
            fold: norte_encoding::FoldMode::Simple,
        };
        let steps = vec![inv(b"foo", b"Foo", 1)];
        assert!(feasible(&steps, &[b"foo".to_vec()], insensitive).is_ok());
    }
}
