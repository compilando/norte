//! Directory synchronization: the retained plan and its execution (spec
//! `docs/superpowers/specs/2026-08-11-directory-sync-design.md`, ADR 0049).
//!
//! The PLANNER doesn't live here: it's `norte-sync`, a pure transducer over
//! `norte-compare`'s rows that never touches a provider. What lives here is
//! everything a daemon needs so that plan can be **approved** and
//! **executed**:
//!
//! - [`spool`] — the approved plan, retained in a file bound to the
//!   connection that produced it. It's what makes `sync.apply` carry
//!   nothing more than a hash, and what runs be, by the wire's SHAPE, what
//!   a human saw.
//! - `run_sync_plan` (private) — the `sync.plan` Task: feeds
//!   `norte_compare::compare`'s stream through the transducer, TEEs each
//!   element to the spool and to the batch traveling to the client, and
//!   closes with a [`SyncPlanDone`].
//!
//! - `exec` (private) — the EXECUTOR: revalidates before destroying,
//!   writes, and records each effect in ONE undoable journal unit. It's
//!   what turns an approved plan into changes on the destination tree.
//!
//! **Hard rule 4 does NOT apply to `sync.plan`**: planning doesn't write a
//! byte to either tree and has no possible undo. What writes is
//! `sync.apply`, which IS a journal batch. Stated here so a later review
//! doesn't ask for an entry that would mean nothing.

pub(crate) mod exec;
pub mod spool;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use norte_proto::methods::{
    DestTrash, RelPath, SYNC_STEPS_MAX_BATCH, SyncCompareOptions, SyncPlanDone, SyncStep,
    SyncStepKind, SyncStepsBatch,
};
use norte_proto::{Error, TaskId};
use norte_sync::{PlanItem, SyncError, SyncOptions};
use norte_vfs::Provider;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

pub use spool::{
    PlanOutcome, SPOOL_DIR_NAME, SPOOL_FORMAT, Spool, SpoolError, SpoolHeader, SpoolReader,
    SpoolStep, SpoolSummary, SpoolWriter, SweepReport,
};

/// Time-based flush for the step batch: the same interval (and the same
/// reason) as `fs.compare`'s and `fs.search`'s — the dialog drips live even
/// if the batch doesn't fill up.
const FLUSH_INTERVAL: Duration = Duration::from_millis(100);

/// What a `sync.plan` keeps emitting.
///
/// Defined by the SDK ([`norte_client::SyncPlanEvent`], ADR 0066) and
/// re-exported here. Its two variants ARE wire types, and the embedded and
/// remote plans emit exactly the same thing: two definitions would be two
/// places to add a variant.
pub use norte_client::SyncPlanEvent;

/// Filter for [`SyncPlanParams::include`](norte_proto::methods::SyncPlanParams::include).
///
/// # Filters the transducer's OUTPUT, never its input
/// Filtering the ROWS reopens #152: the spelling the destination gives a
/// folder travels in the folder's row, which is `Same` and produces no
/// step at all, so a plan that only saw the selected rows would compose
/// destination paths with the SOURCE's spelling. This is stated as a rule
/// in `norte_sync::plan`'s rustdoc, and here is where it's honored: the
/// transducer sees the whole tree and this trims what comes out.
///
/// # A selected ancestor drags its subtree along
/// Selecting a folder in the diff panel means syncing it, and a descended
/// orphan produces a `CreateDir` plus one step per descendant: with exact
/// equality the folder would be created empty. So membership is by segment
/// PREFIX, never by string prefix (`café` cannot drag `cafétière` along)
/// — the comparison is done on the wire form, which separates segments by
/// `/` and percent-encodes everything else, so a `/` inside one can only be
/// a separator.
///
/// # BLOCKERS are not filtered
/// A blocker isn't a step: it says why the plan can't be executed, and some
/// have the whole tree as their scope (`DestReadOnly` hangs off the root,
/// which no selection names). Trimming them by selection would turn a
/// read-only destination into an executable plan, so all of them pass
/// through and `executable` keeps talking about the full comparison.
/// # A `CreateDir` a chosen step needs stays
/// The drag is top-down, and the plan needs it the other way too. A source
/// orphan produces, in pre-order, a `CreateDir new` and then a `Copy
/// new/a.txt`; the panel lets the file's ROW be selected. With only the
/// downward drag, the `CreateDir` would fall out and leave an `executable`
/// plan whose only copy goes to a directory that doesn't exist — and on top
/// of that it breaks the rule `SyncStepsBatch::steps` publishes ("a
/// `CreateDir` precedes every copy inside it"). So a `CreateDir` whose
/// `rel` is a STRICT ancestor of something selected stays. Only that class:
/// a `DeleteTree` on an ancestor would take down exactly the subtree that
/// was asked to be synced.
#[derive(Debug, Clone)]
struct IncludeFilter {
    /// The root was in the list: everything gets in and there's nothing to
    /// look at.
    everything: bool,
    /// The requested paths in wire form (lossless: percent-encoding over
    /// the raw bytes, no `to_str` and no folding anything — hard rule 1).
    wire: HashSet<String>,
    /// The STRICT ancestors of what was requested, for the `CreateDir`s.
    ancestors: HashSet<String>,
}

impl IncludeFilter {
    /// Builds the filter. An EMPTY list isn't "everything": it's a
    /// selection of zero paths, and produces a plan with no steps. Whoever
    /// doesn't want to filter sends the field absent.
    fn new(list: &[RelPath]) -> Self {
        let wire: HashSet<String> = list.iter().map(RelPath::to_wire).collect();
        // The ancestor closure is ≤ (paths × depth), i.e. bounded by
        // `SYNC_MAX_INCLUDE`: paid once and leaves `covers` at one hash
        // lookup per step.
        let mut ancestors = HashSet::new();
        for path in &wire {
            for (i, _) in path.match_indices('/') {
                ancestors.insert(path[..i].to_owned());
            }
        }
        Self {
            everything: list.iter().any(RelPath::is_root),
            wire,
            ancestors,
        }
    }

    /// Does `rel` fall within the selection, by itself or via an ancestor?
    fn covers(&self, rel: &RelPath) -> bool {
        if self.everything {
            return true;
        }
        let wire = rel.to_wire();
        if self.wire.contains(&wire) {
            return true;
        }
        // Every `/` in the wire form closes exactly one ancestor, and there's
        // nowhere else it can appear.
        wire.match_indices('/')
            .any(|(i, _)| self.wire.contains(&wire[..i]))
    }

    /// Is `rel` a strict ancestor of something selected? (See the type's
    /// note: it only decides about a `CreateDir`.)
    fn is_needed_ancestor(&self, rel: &RelPath) -> bool {
        self.ancestors.contains(&rel.to_wire())
    }

    /// Does this step survive the selection?
    fn keeps(&self, step: &SyncStep) -> bool {
        self.covers(&step.rel)
            || (step.kind == SyncStepKind::CreateDir && self.is_needed_ancestor(&step.rel))
    }
}

/// Step batch under construction.
struct Batch {
    task_id: TaskId,
    steps: Vec<SyncStep>,
}

impl Batch {
    fn new(task_id: TaskId) -> Self {
        Self {
            task_id,
            steps: Vec::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    fn len(&self) -> usize {
        self.steps.len()
    }

    /// Extracts the accumulated batch, leaving the buffer empty.
    fn take(&mut self) -> SyncStepsBatch {
        SyncStepsBatch {
            task_id: self.task_id,
            steps: std::mem::take(&mut self.steps),
        }
    }
}

/// Outcome of a [`flush`] (mirrors `compare.rs`'s).
enum FlushOutcome {
    /// Sent (or nothing to send): the plan continues. Carries HOW MANY
    /// steps went out, which is what progress counts.
    Continue(u64),
    /// The receiver died (the owner left): ends cleanly.
    ReceiverGone,
    /// Cancelled while `send` was blocked on backpressure.
    Cancelled,
}

/// Sends the pending batch (if any). A `send` blocked on backpressure does
/// NOT ignore cancellation: it's raced with a `select` against the token
/// (hard rule 3).
async fn flush(
    tx: &mpsc::Sender<SyncPlanEvent>,
    batch: &mut Batch,
    cancel: &CancellationToken,
) -> FlushOutcome {
    if batch.is_empty() {
        return FlushOutcome::Continue(0);
    }
    let lot = batch.take();
    let steps = u64::try_from(lot.steps.len()).unwrap_or(u64::MAX);
    tokio::select! {
        biased;
        () = cancel.cancelled() => FlushOutcome::Cancelled,
        r = tx.send(SyncPlanEvent::Steps(lot)) => match r {
            Ok(()) => FlushOutcome::Continue(steps),
            Err(_) => FlushOutcome::ReceiverGone,
        },
    }
}

/// Everything the `sync.plan` Task needs and cannot derive.
///
/// It's a struct and not seven arguments because seven arguments are seven
/// places to get the order wrong between two `Arc<dyn Provider>`s the
/// compiler can't tell apart.
pub(crate) struct SyncPlanJob {
    /// SOURCE root's provider.
    pub source: Arc<dyn Provider>,
    /// DESTINATION root's provider. Can be the same object.
    pub dest: Arc<dyn Provider>,
    /// The two roots, the mode, `on_unknown` and the destination's
    /// capabilities.
    pub opts: SyncOptions,
    /// What it's compared with. Goes into the hash and the spool's header:
    /// a plan made with `hash` on isn't the same as one made with sizes
    /// only.
    pub compare: SyncCompareOptions,
    /// The caller's selection, already validated against
    /// [`SYNC_MAX_INCLUDE`](norte_proto::methods::SYNC_MAX_INCLUDE).
    pub include: Option<Vec<RelPath>>,
    /// THE daemon's spool (cloned, never built a second time).
    pub spool: Spool,
    /// The connection that owns the plan. It's half the key that will let
    /// it be opened later.
    pub conn_id: u64,
}

/// Body of the `sync.plan` Task: compares, transduces, retains and emits.
///
/// The flow is `norte_compare::compare` → `norte_sync::plan` → (spool,
/// batch). Every element surviving `include` is pushed to the
/// [`SpoolWriter`](spool::SpoolWriter) **and** to the batch in the same
/// iteration: the writer hashes, counts and writes in the same call, so
/// there's no way to retain one sequence and show another (the `plan_hash`
/// summarizes EXACTLY what the human sees).
///
/// - **`descend_orphans` is set by the caller on the SOURCE side** (done by
///   [`Engine::sync_plan_as`](crate::Engine::sync_plan_as)), not the
///   client: whoever approves needs how many files and how many bytes, and
///   the executor needs one step per file. A DESTINATION orphan is one
///   whole `DeleteTree`, and descending it buys listings that change not a
///   single step.
/// - **Cancellation** (hard rule 3): the token is the SAME for the walk and
///   for the transducer —`norte_sync::plan`'s rustdoc requires it—, and
///   `flush` checks it again while waiting for room in the channel. A
///   cancelled plan closes with [`PlanOutcome::Interrupted`], which deletes
///   the `.part`: **it leaves nothing approvable**.
/// - **Only the stream's `None` arm closes with [`PlanOutcome::Ended`]**.
///   The partial digest of a plan cut in half is indistinguishable from a
///   shorter complete one's, so closing it would produce a valid
///   `plan_hash` for a plan that claims to sync a tree that was only a
///   third walked.
/// - **A batch that doesn't get delivered also interrupts.** If the owner
///   left, the plan we'd retain would be one nobody ever saw whole; and
///   dropping the channel is also what stops the walk.
/// - **Progress**: `entries_done` counts STEPS emitted and is incremented
///   when the `flush` is confirmed, never when accumulated into the batch
///   — it's the signal a client uses to detect a lost `sync.steps`, and
///   counting steps that stayed in a discarded batch would make it report
///   a loss that never happened. To be precise: "confirmed" means the
///   batch ENTERED the Task's channel, not that the frame reached the
///   client. The two numbers only diverge when the daemon's pump can't
///   deliver, and that path ends the plan with no `sync.plan_done`, so
///   nobody can approve over an inflated count. `bytes_done` stays at
///   zero: planning doesn't write. `current` isn't touched either (it
///   would carry a `VPath` to a broadcast every connected human sees, and
///   this Task's gate is per ROOT).
///
/// `Sides` + `norte_compare::compare`'s stream, in a separate function so
/// [`run_sync_plan`] fits under the gate's line limit (#153, ADR 0051) —
/// see `crate::compare::probed_sides`'s rustdoc for why `Sides` is computed
/// HERE and not inside the comparison engine.
async fn compared_rows<'a>(
    source: &'a dyn Provider,
    source_root: &'a norte_proto::VPath,
    dest: &'a dyn Provider,
    dest_root: &'a norte_proto::VPath,
    opts: norte_compare::CompareOptions,
    excluded: Vec<norte_proto::VPath>,
    cancel: CancellationToken,
) -> norte_compare::CompareStream<'a> {
    let sides = crate::compare::probed_sides(source, source_root, dest, dest_root).await;
    norte_compare::compare(
        source,
        source_root,
        dest,
        dest_root,
        opts,
        sides,
        excluded,
        cancel,
    )
}

/// Cap on entries counted from a `DeleteTree`'s first level (#176).
///
/// Above it, the witness is left WITHOUT a count: counting a million-entry
/// directory while planning costs the whole listing, and the count exists
/// to be cheap. A witness with no count relaxes nothing — revalidation only
/// compares what the two snapshots carry, same as with size and date.
const COUNT_MAX: u64 = 4096;

/// Completes a `DeleteTree`'s witness with its first level's count.
///
/// Any other kind of step comes back unchanged: only deleting a tree
/// revalidates a DIRECTORY, and only there does the count say anything.
async fn count_if_deletes_tree(
    item: PlanItem,
    dest: &dyn Provider,
    dest_root: &norte_proto::VPath,
    cancel: &CancellationToken,
) -> PlanItem {
    use futures::StreamExt as _;

    let PlanItem::Step {
        step,
        dest: snapshot,
    } = item
    else {
        return item;
    };
    let (Some(snapshot), SyncStepKind::DeleteTree) = (snapshot, step.kind) else {
        return PlanItem::Step {
            step,
            dest: snapshot,
        };
    };
    let mut path = dest_root.clone();
    for segment in step.dest_rel.as_ref().unwrap_or(&step.rel).segments() {
        path = path.join(segment.clone());
    }
    let counted = match dest.list(&path).await {
        Ok(mut stream) => {
            let mut n = 0_u64;
            loop {
                if cancel.is_cancelled() {
                    break None;
                }
                match stream.next().await {
                    Some(Ok(_)) => {
                        n += 1;
                        if n > COUNT_MAX {
                            break None; // too many: counting stops being cheap
                        }
                    }
                    // An unreadable entry leaves the count with NO answer: a
                    // number that skipped something is worse than no number.
                    Some(Err(_)) => break None,
                    None => break Some(n),
                }
            }
        }
        Err(_) => None,
    };
    PlanItem::Step {
        step,
        dest: Some(snapshot.with_entries(counted)),
    }
}

/// Turns into a BLOCKER a step whose destination name that provider cannot
/// have (#163).
///
/// Only looks at steps that CREATE a name there: a delete names something
/// that already exists, so its legality is proven by its existence.
///
/// Blocks instead of skipping for the same reason as `TypeMismatchDir`:
/// whoever asked for a mirror asked for the destination to end up like the
/// source, and a name that cannot exist there is a structural divergence no
/// later report fixes.
fn block_if_the_name_does_not_fit(item: PlanItem, dest: &dyn Provider) -> PlanItem {
    use norte_proto::methods::{SyncBlocker, SyncBlockerKind};

    let PlanItem::Step {
        step,
        dest: snapshot,
    } = item
    else {
        return item;
    };
    let creates = matches!(
        step.kind,
        SyncStepKind::Copy | SyncStepKind::Overwrite | SyncStepKind::CreateDir
    );
    let rel = step.dest_rel.as_ref().unwrap_or(&step.rel);
    if !creates
        || rel
            .segments()
            .iter()
            .all(|s| dest.name_is_legal(s.as_bytes()))
    {
        return PlanItem::Step {
            step,
            dest: snapshot,
        };
    }
    PlanItem::Blocker(SyncBlocker {
        rel: rel.clone(),
        kind: SyncBlockerKind::IllegalDestName,
        // The side is ALWAYS the destination: it's its filesystem refusing
        // the name, not the source that wrote it wrong.
        side: Some(norte_proto::methods::Side::Right),
    })
}

/// # Errors
/// [`Error::Cancelled`] if it was cancelled or the owner stopped
/// receiving; [`Error::Io`] if the spool couldn't be written;
/// [`Error::Internal`] if the transducer ended in a way this wiring cannot
/// produce.
#[tracing::instrument(skip_all, fields(conn_id = job.conn_id))]
pub(crate) async fn run_sync_plan(
    job: SyncPlanJob,
    tx: mpsc::Sender<SyncPlanEvent>,
    ctx: &crate::scheduler::TaskCtx,
) -> Result<(), Error> {
    let SyncPlanJob {
        source,
        dest,
        opts,
        compare,
        include,
        spool,
        conn_id,
    } = job;
    let task_id = ctx.progress.snapshot().task_id;
    let include = include.as_deref().map(IncludeFilter::new);
    // From the SAME pair of booleans the transducer uses to decide each
    // step's `reversal`, translated by the wire's type: if the summary
    // derived it on its own, the plan and its dialog could say different
    // things about the same destination.
    let dest_trash = DestTrash::of(opts.dest_has_trash, opts.dest_trash_restorable);

    let mut writer = spool
        .create(conn_id, &opts, &compare)
        .await
        .map_err(|e| spool_error(&e))?;
    let mut batch = Batch::new(task_id);
    let mut last_flush = Instant::now();

    // The stream is built RIGHT HERE: `compare` borrows BOTH providers, so
    // the borrow has to be born inside the `async` that consumes it.
    let rows = compared_rows(
        source.as_ref(),
        &opts.source_root,
        dest.as_ref(),
        &opts.dest_root,
        compare_options(&compare, &opts),
        // Same as in `fs.compare` (#209): a sync plan READS both trees just
        // like a comparison, so an agent cannot inventory the daemon's
        // state directory through here.
        crate::policy::walk_exclusions(&ctx.actor),
        ctx.cancel.clone(),
    )
    .await;
    // Pinned on the stack: the transducer's stream is not `Unpin` (its
    // `Unfold` holds the `async` that produces it), and it's polled here
    // from a loop.
    let mut items = std::pin::pin!(norte_sync::plan(rows, opts.clone(), ctx.cancel.clone()));

    // `Ok(())` = the stream reached `None`. Anything else interrupts, and
    // the interruption CANNOT go out the same door as the ending (see the
    // note on the `PlanOutcome` type).
    let ended: Result<(), Error> = loop {
        // The time tick goes in a `select!` and not hanging off a step's
        // arrival, which is what `fs.compare` does. There it doesn't
        // matter —every pair is a row—; here the transducer emits NOTHING
        // for a `Same` row, so a plan that produces three steps and then
        // walks two hundred thousand identical files would leave those
        // three in the buffer for the whole walk. The stream is
        // `FusedStream` precisely so it can go in a `select!` (task 3's
        // note).
        let next = tokio::select! {
            biased;
            item = items.next() => item,
            () = tokio::time::sleep_until(last_flush + FLUSH_INTERVAL) => {
                match flush(&tx, &mut batch, &ctx.cancel).await {
                    FlushOutcome::Continue(steps) => {
                        ctx.progress.update(|p| p.entries_done += steps);
                        last_flush = Instant::now();
                        continue;
                    }
                    FlushOutcome::ReceiverGone | FlushOutcome::Cancelled => {
                        break Err(Error::Cancelled);
                    }
                }
            }
        };
        let Some(item) = next else {
            break Ok(());
        };
        let item = match item {
            Ok(item) => item,
            Err(SyncError::Cancelled) => break Err(Error::Cancelled),
            // `SyncError` is `#[non_exhaustive]` and the other variants are
            // WIRING failures (a mode this binary doesn't plan, a source
            // that names no side, a row outside its root). This caller
            // builds the options itself, so none of them is reachable from
            // the wire; saying `Cancelled` for them would lie about what
            // happened.
            // The CLASS is logged, not the `Display`: `OutsideRoot` and
            // `RootIsNotAStep` format paths, and the paths arriving that
            // way are exactly the ones a provider chose to return outside
            // the root it was asked to list — i.e., an attacker. The rest
            // of this module redacts, and `read_gate` sets the criterion:
            // never the path in the trace.
            Err(other) => {
                tracing::error!(
                    class = sync_error_class(&other),
                    "sync.plan: unexpected end from the transducer"
                );
                break Err(Error::Internal { panic: false });
            }
        };
        if let (PlanItem::Step { step, .. }, Some(filter)) = (&item, include.as_ref())
            && !filter.keeps(step)
        {
            continue;
        }
        // A `DeleteTree`'s witness is completed with its first level's
        // COUNT (#176). This goes here and not in the transducer because
        // the transducer is pure and has no provider — and it goes at
        // PLANNING and not at applying because what's compared is "what
        // there was when the human decided" against "what's there now".
        //
        // It's one listing per destructive step, and it's the step with
        // the largest blast radius of all: a directory's `stat` only
        // changes when its DIRECT children change, so without this a
        // subtree that gained a hundred files between approving and
        // applying would revalidate clean and get deleted whole.
        let item = count_if_deletes_tree(item, dest.as_ref(), &opts.dest_root, &ctx.cancel).await;
        // And a name the DESTINATION cannot have blocks the plan instead of
        // being discovered at execution time (#163). Decided by the
        // destination's provider, which is the one that knows its rules;
        // here it's only asked, and asking costs no I/O.
        let item = block_if_the_name_does_not_fit(item, dest.as_ref());
        // Hashes, counts and writes in the SAME call: the batch below
        // carries exactly the same thing.
        if let Err(e) = writer.push(&item).await {
            tracing::error!(error = %e, "sync.plan: the spool did not accept an element");
            break Err(spool_error(&e));
        }
        if let PlanItem::Step { step, .. } = item {
            batch.steps.push(step);
            if batch.len() >= SYNC_STEPS_MAX_BATCH {
                match flush(&tx, &mut batch, &ctx.cancel).await {
                    FlushOutcome::Continue(steps) => {
                        ctx.progress.update(|p| p.entries_done += steps);
                        last_flush = Instant::now();
                    }
                    // The owner left: retaining a plan nobody saw whole is
                    // exactly what the approval dialog exists to prevent.
                    // And dropping the channel is what stops the walk.
                    FlushOutcome::ReceiverGone | FlushOutcome::Cancelled => {
                        break Err(Error::Cancelled);
                    }
                }
            }
        }
    };
    if let Err(e) = ended {
        // NOT `Ended`: deletes the `.part` and there's no plan to apply.
        let _ = writer.finish(PlanOutcome::Interrupted).await;
        return Err(e);
    }
    let closing = Closing {
        conn_id,
        task_id,
        dest_trash,
    };
    close_plan(writer, &spool, closing, &mut batch, &tx, ctx).await
}

/// What identifies the plan being closed, and the only part of it
/// [`close_plan`] cannot read off the spool's summary.
#[derive(Debug, Clone, Copy)]
struct Closing {
    /// The owning connection (half the key of the retained plan).
    conn_id: u64,
    /// The Task that produced it.
    task_id: TaskId,
    /// What trash the destination has, i.e. what undo could return if this
    /// plan gets applied. Comes from the options and not the summary
    /// because the spool doesn't count it: it isn't a counter, it's a
    /// property of the destination.
    dest_trash: DestTrash,
}

/// Closes a plan that reached the end of its stream: last batch, the
/// spool's terminator and `sync.plan_done`.
///
/// This is what decides whether the plan stays RETAINED, so the three ways
/// it should not are together here:
///
/// 1. **The last batch doesn't get delivered.** A plan nobody saw whole
///    isn't approved, which is what the dialog exists to prevent.
/// 2. **The channel is closed.** `flush` returns `Continue(0)` without
///    touching it when the batch is empty, so a plan over two identical
///    trees —zero steps— would never find out through that path.
/// 3. **`finish` says no.** The authoritative one: the connection was torn
///    down while the plan was closing, or someone is applying an identical
///    plan.
///
/// And a fourth, with the plan already retained: if the notice doesn't
/// arrive, it's withdrawn. The plan has to be closed BEFORE it's sent —the
/// other way around would leave the client with a hash that can't be
/// opened yet—, so the only way to avoid leaving a plan nobody will apply
/// or collect is to undo it.
async fn close_plan(
    writer: SpoolWriter,
    spool: &Spool,
    closing: Closing,
    batch: &mut Batch,
    tx: &mpsc::Sender<SyncPlanEvent>,
    ctx: &crate::scheduler::TaskCtx,
) -> Result<(), Error> {
    let Closing {
        conn_id,
        task_id,
        dest_trash,
    } = closing;
    match flush(tx, batch, &ctx.cancel).await {
        FlushOutcome::Continue(steps) => ctx.progress.update(|p| p.entries_done += steps),
        FlushOutcome::ReceiverGone | FlushOutcome::Cancelled => {
            let _ = writer.finish(PlanOutcome::Interrupted).await;
            return Err(Error::Cancelled);
        }
    }
    if tx.is_closed() {
        let _ = writer.finish(PlanOutcome::Interrupted).await;
        return Err(Error::Cancelled);
    }
    let summary = match writer.finish(PlanOutcome::Ended).await {
        Ok(summary) => summary,
        Err(SpoolError::Interrupted) => return Err(Error::Cancelled),
        Err(e) => return Err(spool_error(&e)),
    };
    // BUILT from the summary, not recomputed: `executable` and the
    // counters are derived in one place ([`SpoolWriter::finish`]), and
    // whoever derives them on their own will sooner or later derive them
    // differently.
    let plan_hash = summary.plan_hash.clone();
    let done = SyncPlanDone {
        task_id,
        plan_hash: summary.plan_hash,
        counts: summary.counts,
        blockers: summary.blockers,
        blockers_total: summary.blockers_total,
        executable: summary.executable,
        dest_trash,
    };
    // The owner could recompute the digest on their own —`PlanHasher`
    // carries no key and received every batch—, so "nobody knows the hash"
    // isn't why this is safe: the reason is that the plan stops being
    // retained, and that `sync.apply` still requires write scope.
    if tx.send(SyncPlanEvent::Done(done)).await.is_err() {
        tracing::debug!(
            conn = conn_id,
            "sync.plan_done with no owner: withdrawing the plan"
        );
        let _ = spool.remove(conn_id, &plan_hash).await;
        return Err(Error::Cancelled);
    }
    Ok(())
}

/// The CLASS of a transducer failure, for the trace.
///
/// Exists so the error isn't formatted: `OutsideRoot` and `RootIsNotAStep`
/// carry paths in their `Display`, and the paths arriving that way are the
/// ones a provider chose to return outside the root it was asked to list.
/// A closed vocabulary says the same thing for diagnosis and doesn't write
/// into an operator's log something outside their control (rule 10, same
/// criterion as `read_gate`).
fn sync_error_class(e: &SyncError) -> &'static str {
    match e {
        SyncError::Cancelled => "cancelled",
        SyncError::SourceSideUnknown => "source-side-unknown",
        SyncError::OutsideRoot { .. } => "outside-root",
        SyncError::RootIsNotAStep { .. } => "root-is-not-a-step",
        SyncError::ModeNotPlanned(_) => "mode-not-planned",
        SyncError::Compare(_) => "compare",
        // `SyncError` is `#[non_exhaustive]`.
        _ => "unknown",
    }
}

/// Translates the spool's failure to the wire's taxonomy.
///
/// Only [`SpoolError::Io`] is a real I/O failure; the rest, on the WRITE
/// path, can only be an already-closed writer or a record over the cap — a
/// failure of this core, not of the disk.
fn spool_error(e: &SpoolError) -> Error {
    match e {
        SpoolError::Io(_) => Error::Io { retryable: false },
        _ => Error::Internal { panic: false },
    }
}

/// The comparison ENGINE's options from the wire's.
///
/// `follow_symlinks` and `descend_orphans` don't belong to the caller in
/// `sync.plan` (rejected upstream with `-32602`): they're set here, and
/// `descend_orphans` on the SOURCE side.
fn compare_options(wire: &SyncCompareOptions, opts: &SyncOptions) -> norte_compare::CompareOptions {
    norte_compare::CompareOptions {
        criteria: wire.criteria,
        max_depth: wire.max_depth,
        mtime_tolerance_ms: wire.mtime_tolerance_ms,
        follow_symlinks: false,
        descend_orphans: Some(opts.source_side),
    }
}

#[cfg(test)]
mod tests {
    use norte_proto::methods::{RelPath, SyncStepKind};

    use super::IncludeFilter;

    fn rel(wire: &str) -> RelPath {
        RelPath::parse_wire(wire).expect("rel")
    }

    /// A minimal step: only `kind` and `rel` matter for the filter.
    fn step(kind: norte_proto::methods::SyncStepKind, wire: &str) -> super::SyncStep {
        super::SyncStep {
            id: 1,
            kind,
            rel: rel(wire),
            dest_rel: None,
            size: None,
            criterion: norte_proto::methods::CompareCriterion::Presence,
            confidence: norte_proto::methods::CompareConfidence::Certain,
            reversal: Some(norte_proto::methods::StepReversal::Delete),
            reason: None,
        }
    }

    /// #163: a name the DESTINATION cannot have blocks the plan, instead of
    /// being discovered at execution time.
    ///
    /// Decided by the destination's provider —which knows its rules—, and
    /// here the WIRING is tested with one that refuses on purpose: the real
    /// Win32 rules are set by `norte-vfs-local` and can't be run on this
    /// machine, but a "no" from it turning into a block can.
    #[test]
    fn a_name_the_destination_rejects_blocks_the_plan() {
        use norte_proto::Error;
        use norte_proto::methods::{SyncBlockerKind, SyncStepKind};
        use norte_vfs::Provider;

        /// A Windows-style destination: no colons allowed.
        struct NoColons;

        #[async_trait::async_trait]
        impl Provider for NoColons {
            #[expect(
                clippy::unnecessary_literal_bound,
                reason = "the trait's signature is `-> &str`"
            )]
            fn scheme(&self) -> &str {
                "mem"
            }
            fn capabilities(&self) -> norte_proto::Capabilities {
                norte_proto::Capabilities {
                    flags: norte_proto::CapabilityFlags::empty(),
                    max_path: None,
                }
            }
            fn name_is_legal(&self, name: &[u8]) -> bool {
                !name.contains(&b':')
            }
            async fn stat(&self, _p: &norte_proto::VPath) -> Result<norte_proto::Entry, Error> {
                Err(Error::NotFound)
            }
            async fn list(&self, _p: &norte_proto::VPath) -> Result<norte_vfs::EntryStream, Error> {
                Err(Error::NotFound)
            }
            async fn read(
                &self,
                _p: &norte_proto::VPath,
                _r: Option<norte_proto::ByteRange>,
            ) -> Result<norte_vfs::ByteStream, Error> {
                Err(Error::NotFound)
            }
            async fn write(
                &self,
                _p: &norte_proto::VPath,
            ) -> Result<Box<dyn norte_vfs::ByteSink>, Error> {
                Err(Error::Unsupported)
            }
            async fn mkdir(&self, _p: &norte_proto::VPath) -> Result<(), Error> {
                Err(Error::Unsupported)
            }
            async fn remove(&self, _p: &norte_proto::VPath) -> Result<(), Error> {
                Err(Error::Unsupported)
            }
            async fn rename(
                &self,
                _a: &norte_proto::VPath,
                _b: &norte_proto::VPath,
            ) -> Result<(), Error> {
                Err(Error::Unsupported)
            }
        }

        let copy = |wire: &str| super::PlanItem::Step {
            step: step(SyncStepKind::Copy, wire),
            dest: None,
        };

        // Legal: comes out as a step, untouched.
        assert!(matches!(
            super::block_if_the_name_does_not_fit(copy("report.txt"), &NoColons),
            super::PlanItem::Step { .. }
        ));

        // Illegal there: a block, with the DESTINATION side.
        let blocked = super::block_if_the_name_does_not_fit(copy("f%3Aads"), &NoColons);
        let super::PlanItem::Blocker(b) = blocked else {
            panic!("a name the destination rejects has to block")
        };
        assert_eq!(b.kind, SyncBlockerKind::IllegalDestName);
        assert_eq!(b.side, Some(norte_proto::methods::Side::Right));
        assert_eq!(b.rel, rel("f%3Aads"));

        // And a DELETE isn't checked: it names something that already
        // exists there, so its legality is proven by its existence.
        let deleted = super::PlanItem::Step {
            step: step(SyncStepKind::DeleteTree, "f%3Aads"),
            dest: None,
        };
        assert!(matches!(
            super::block_if_the_name_does_not_fit(deleted, &NoColons),
            super::PlanItem::Step { .. }
        ));
    }

    #[test]
    fn a_selection_drags_its_subtree_and_not_its_neighbor() {
        let f = IncludeFilter::new(&[rel("caf%C3%A9")]);
        assert!(f.covers(&rel("caf%C3%A9")), "the folder itself");
        assert!(f.covers(&rel("caf%C3%A9/x.txt")), "what's inside");
        // By STRING prefix, `cafétière` would hang off `café` (hard rule 1).
        assert!(!f.covers(&rel("caf%C3%A9ti%C3%A8re")));
        assert!(!f.covers(&rel("other")));
    }

    #[test]
    fn the_root_in_the_list_includes_everything_and_an_empty_list_nothing() {
        let everything = IncludeFilter::new(&[RelPath::default()]);
        assert!(everything.covers(&rel("a/b/c")));
        let nothing = IncludeFilter::new(&[]);
        assert!(!nothing.covers(&rel("a")));
        assert!(!nothing.covers(&RelPath::default()));
    }

    #[test]
    fn membership_is_by_bytes_with_no_folding_or_normalizing() {
        // NFC in the list, NFD in the step: two different names.
        let f = IncludeFilter::new(&[rel("caf%C3%A9")]);
        assert!(!f.covers(&rel("cafe%CC%81")));
        // And case doesn't fold either.
        let g = IncludeFilter::new(&[rel("README")]);
        assert!(!g.covers(&rel("readme")));
    }

    #[test]
    fn selecting_a_file_keeps_the_createdir_it_needs() {
        // The panel lets the file's ROW be chosen; without its `CreateDir`
        // the plan would copy into a directory that doesn't exist.
        let f = IncludeFilter::new(&[rel("new/a.txt")]);
        assert!(f.keeps(&step(SyncStepKind::CreateDir, "new")));
        assert!(f.keeps(&step(SyncStepKind::Copy, "new/a.txt")));
        // But ONLY that class: deleting the ancestor would take down
        // exactly what was asked to be synced.
        assert!(!f.keeps(&step(SyncStepKind::DeleteTree, "new")));
        // And the ancestor has to be an ancestor of SOMETHING chosen.
        assert!(!f.keeps(&step(SyncStepKind::CreateDir, "other")));
    }

    #[test]
    fn a_non_utf8_name_gets_in_by_its_wire_form() {
        let f = IncludeFilter::new(&[rel("report%FF%FE.dat")]);
        assert!(f.covers(&rel("report%FF%FE.dat")));
        assert!(!f.covers(&rel("report%FE%FF.dat")));
    }
}
