//! The `fs.compare` Task (C6): wraps the `norte-compare` engine in the
//! core's task framework and converts its row stream into capped, coalesced
//! BATCHES ([`CompareRowsBatch`]).
//!
//! Nothing about the comparison is decided here: the verdicts, the
//! confidences and the errors-as-rows belong to the engine. What this
//! module contributes is what the engine deliberately doesn't know — the
//! Task's cancellation, its progress, and that a million rows can't become
//! a million frames.
//!
//! **Hard rule 4 does NOT apply**: comparing mutates nothing, writes no
//! byte and has no possible undo, so there is no journal entry to create.
//! Stated here so a later review doesn't ask for one that wouldn't mean
//! anything.
//!
//! The coalescing is the same as `fs.search`'s (`search.rs`), on purpose:
//! two different batch contracts for two identical feeds would be two
//! things to keep in sync by hand.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use norte_compare::{CompareError, CompareOptions};
use norte_proto::methods::{COMPARE_ROWS_MAX_BATCH, CompareRow, CompareRowsBatch};
use norte_proto::{Error, TaskId, VPath};
use norte_vfs::Provider;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Time-based flush for the row batch: the same interval (and the same
/// reason) as `fs.search`'s — the panel drips live even if the batch
/// doesn't fill up.
const FLUSH_INTERVAL: Duration = Duration::from_millis(100);

/// Batch under construction.
struct Batch {
    task_id: TaskId,
    rows: Vec<CompareRow>,
}

impl Batch {
    fn new(task_id: TaskId) -> Self {
        Self {
            task_id,
            rows: Vec::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    fn len(&self) -> usize {
        self.rows.len()
    }

    /// Extracts the accumulated batch, leaving the buffer empty.
    fn take(&mut self) -> CompareRowsBatch {
        CompareRowsBatch {
            task_id: self.task_id,
            rows: std::mem::take(&mut self.rows),
        }
    }
}

/// Outcome of a [`flush`] (mirrors `search.rs`'s).
enum FlushOutcome {
    /// Sent (or nothing to send): the comparison continues. Carries HOW MANY
    /// rows went out, which is what progress counts — a batch that never
    /// got sent cannot show up in `entries_done`.
    Continue(u64),
    /// The receiver died (the owner left): ends cleanly.
    ReceiverGone,
    /// Cancelled while `send` was blocked on backpressure: ends as
    /// `Cancelled` (hard rule 3).
    Cancelled,
}

/// How the PAIR of sides is matched, computed by WHOEVER KNOWS BOTH ROOTS —
/// `norte_compare::compare` no longer computes it alone (#153, ADR 0051):
/// it used to do it internally, on the first listing, against
/// `Provider::capabilities()` with no path — the same `LocalProvider` serving
/// `/home` (ext4) and `/mnt/usb` (exFAT) answered the SAME response for
/// both, and the second mount's folding collisions got lost silently.
///
/// Since ADR 0054 EACH ROOT is asked (`Provider::capabilities_at`), which is
/// what closes #153: the answer is no longer the provider's but the
/// directory's, and on `file://` it comes from a read-only staircase that
/// also knows how to recognize an ext4/f2fs in `+F` (#145). The prior `stat`
/// that forced lazy probing is no longer needed: `capabilities_at` IS an
/// async operation and probes on its own.
///
/// A root that doesn't exist makes its `capabilities_at` fail here, and then
/// what's answered is what the provider DECLARES instead of propagating the
/// error: the root still fails its own `list` inside the engine, with its
/// error row, which is where the user has to see it. Failing here would
/// turn an error row into a comparison that never starts.
pub(crate) async fn probed_sides(
    left: &dyn Provider,
    left_root: &VPath,
    right: &dyn Provider,
    right_root: &VPath,
) -> norte_compare::Sides {
    let (left_caps, right_caps) = tokio::join!(
        left.capabilities_at(left_root),
        right.capabilities_at(right_root)
    );
    norte_compare::Sides::from_capabilities(
        degraded(left_caps, left, left_root),
        degraded(right_caps, right, right_root),
    )
}

/// A root's capabilities, or whatever the provider declares if it couldn't
/// answer — **saying so**.
///
/// The degradation matters, and that's why it doesn't go in `trace!`: a
/// Linux default is `CASE_SENSITIVE`, i.e. "fold nothing", so a root that
/// stops answering partway through turns a comparison into one that fails
/// to report case collisions. ADR 0054 says that where the guarantee isn't
/// there, it must be SAID, and a TRACE line doesn't say it: it's invisible
/// in production.
pub(crate) fn degraded(
    result: Result<norte_proto::Capabilities, Error>,
    provider: &dyn Provider,
    root: &VPath,
) -> norte_proto::Capabilities {
    match result {
        Ok(caps) => caps,
        Err(e) => {
            tracing::warn!(
                error = %e,
                root = %root.display_lossy(),
                "could not probe this root's capabilities: using what the provider declares"
            );
            provider.capabilities()
        }
    }
}

/// Sends the pending batch (if any). A `send` blocked on backpressure (full
/// channel + slow receiver) does NOT ignore cancellation: it's raced with a
/// `select` against the token.
async fn flush(
    tx: &mpsc::Sender<CompareRowsBatch>,
    batch: &mut Batch,
    cancel: &CancellationToken,
) -> FlushOutcome {
    if batch.is_empty() {
        return FlushOutcome::Continue(0);
    }
    let lot = batch.take();
    let rows = u64::try_from(lot.rows.len()).unwrap_or(u64::MAX);
    tokio::select! {
        biased;
        () = cancel.cancelled() => FlushOutcome::Cancelled,
        r = tx.send(lot) => match r {
            Ok(()) => FlushOutcome::Continue(rows),
            Err(_) => FlushOutcome::ReceiverGone,
        },
    }
}

/// The Task's body: drains [`norte_compare::compare`]'s stream and emits
/// batches over `tx`. Pure read: no journal, no mutations.
///
/// - **The providers come in by value** and the stream is built RIGHT HERE
///   INSIDE: `CompareStream<'a>` borrows BOTH providers, so the borrow has
///   to be born inside the `async` that consumes it, not outside.
/// - **Cancellation** (hard rule 3): the engine checks the token per
///   directory and per hashed chunk, and `flush` checks it too while
///   waiting for room in the channel. The stream's ONLY `Err` is
///   [`CompareError::Cancelled`] and it means exactly that: the Task ends
///   `Cancelled`, not `Failed`. Every REAL failure —an unreadable
///   directory, an oversized one, a read that breaks mid-hash— is a ROW.
/// - **Progress**: `entries_done` counts SENT rows, and it's incremented
///   when the `flush` is confirmed, not when the row is accumulated into
///   the batch. This is contract (C1): with no `max_hits` to count against,
///   it's the only signal a client has for detecting that it missed a
///   `compare.rows` notification, so counting rows that stayed in a
///   discarded batch (due to cancellation, or because the receiver
///   vanished) would make it report a loss that never happened.
///   `bytes_done` stays at zero — with the hash rung off, not a byte is
///   read, and a byte bar that forever paints zero lies more than not
///   existing. `current` isn't touched either: it would carry a `VPath`
///   from the compared tree into a broadcast every connected human sees,
///   and this Task's gate is per ROOT.
/// - **Coalescing**: rows accumulate up to [`COMPARE_ROWS_MAX_BATCH`] or
///   get drained every [`FLUSH_INTERVAL`], whichever happens first.
///
/// # Errors
/// [`Error::Cancelled`] if cancelled. Nothing else: the comparison has no
/// other premature ending, and there's nothing to clean up because it wrote
/// nothing.
pub async fn run_compare(
    left: Arc<dyn Provider>,
    left_root: VPath,
    right: Arc<dyn Provider>,
    right_root: VPath,
    opts: CompareOptions,
    tx: mpsc::Sender<CompareRowsBatch>,
    ctx: &crate::scheduler::TaskCtx,
) -> Result<(), Error> {
    let task_id = ctx.progress.snapshot().task_id;
    let mut batch = Batch::new(task_id);
    let mut last_flush = Instant::now();

    let sides = probed_sides(left.as_ref(), &left_root, right.as_ref(), &right_root).await;
    // What this ACTOR cannot walk (#209): the daemon's read gate looks at
    // BOTH roots, so comparing `$HOME` against something else is legitimate
    // and used to drag the daemon's state directory along with it —
    // `journal.db`, the sync spools and, with the hash rung on, an equality
    // oracle over their bytes. It's the half #165 left open, and it comes
    // from the SAME place as `fs.search`'s walk exclusions.
    let excluded = crate::policy::walk_exclusions(&ctx.actor);
    let mut stream = norte_compare::compare(
        left.as_ref(),
        &left_root,
        right.as_ref(),
        &right_root,
        opts,
        sides,
        excluded,
        ctx.cancel.clone(),
    );

    while let Some(item) = stream.next().await {
        match item {
            Ok(row) => batch.rows.push(row),
            // The stream's ONLY error. It's emitted once and the stream
            // ends; whatever accumulated is discarded (the receiver no
            // longer needs it: the comparison never got to answer).
            Err(CompareError::Cancelled) => return Err(Error::Cancelled),
            // `CompareError` is `#[non_exhaustive]`: a future variant is NOT
            // a cancellation and cannot be treated as one — a Task that says
            // `Cancelled` when it actually failed lies about what happened,
            // and a sync plan reading those rows would believe it.
            Err(other) => {
                tracing::error!(error = %other, "fs.compare: unexpected end from the engine");
                return Err(Error::Internal { panic: false });
            }
        }
        if batch.len() >= COMPARE_ROWS_MAX_BATCH || last_flush.elapsed() >= FLUSH_INTERVAL {
            match flush(&tx, &mut batch, &ctx.cancel).await {
                FlushOutcome::Continue(rows) => {
                    ctx.progress.update(|p| p.entries_done += rows);
                    last_flush = Instant::now();
                }
                // The receiver vanished partway through: the comparison did
                // NOT finish, and saying `Completed` would make whoever
                // compares received rows against `entries_done` accept a
                // half-finished answer as good. `Cancelled` is what really
                // happened (whoever asked for it), and there's nothing to
                // clean up.
                FlushOutcome::ReceiverGone | FlushOutcome::Cancelled => {
                    return Err(Error::Cancelled);
                }
            }
        }
    }

    match flush(&tx, &mut batch, &ctx.cancel).await {
        FlushOutcome::Continue(rows) => {
            ctx.progress.update(|p| p.entries_done += rows);
            Ok(())
        }
        FlushOutcome::ReceiverGone | FlushOutcome::Cancelled => Err(Error::Cancelled),
    }
}
