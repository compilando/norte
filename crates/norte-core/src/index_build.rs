//! Indexing walk (M4, ADR 0034): traverses a subtree via
//! [`Provider::list`] and collects [`IndexEntry`] for
//! [`norte_index::Index::build`]. BFS model from [`crate::search::run_walk`]:
//! confined to the root (defense in depth), cancellation check (rule 3),
//! and does NOT descend symlinks (name candidate, not followed → no cycles).

use std::collections::VecDeque;
use std::sync::Arc;

use futures::StreamExt;
use norte_index::IndexEntry;
use norte_proto::{EntryKind, Error, VPath};
use norte_vfs::Provider;

use crate::scheduler::TaskCtx;

/// Cap on indexed entries per build (same class as the `list`/`search`
/// limits). A larger tree fails with `LimitExceeded`; what was sent before
/// stays indexed, and the build does not sweep.
const MAX_INDEX_ENTRIES: usize = 5_000_000;

/// Walks `root` and sends every entry (dirs included) as an [`IndexEntry`]
/// to `out`, which `Index::build_stream` consumes as they come (#408) —
/// the tree is never held whole. Cancelable: a tripped token cuts it short
/// with [`Error::Cancelled`]. If whoever reads `out` is gone, it stops:
/// that side has its own error to report. On ANY early stop — cancel,
/// limit, provider error — what was already sent stays indexed and nothing
/// is swept: the index is a superset of the tree, never a wrong prune.
pub(crate) async fn walk_for_index(
    provider: Arc<dyn Provider>,
    root: VPath,
    ctx: &TaskCtx,
    out: &tokio::sync::mpsc::Sender<IndexEntry>,
) -> Result<(), Error> {
    let confine = root.clone();
    let mut queue: VecDeque<VPath> = VecDeque::new();
    queue.push_back(root);
    let mut sent: usize = 0;

    while let Some(dir) = queue.pop_front() {
        if ctx.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        // Unreadable directory: skipped (counts as examined) and continues.
        let Ok(mut stream) = provider.list(&dir).await else {
            ctx.progress.update(|p| p.entries_done += 1);
            continue;
        };
        while let Some(item) = stream.next().await {
            if ctx.cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let Ok(entry) = item else {
                ctx.progress.update(|p| p.entries_done += 1);
                continue;
            };
            // Belt-and-suspenders: an entry outside the root is COMPLETELY
            // ignored (the scope is a core invariant, not the provider's
            // correctness) — same criterion as `search::run_walk`.
            if !crate::policy::is_under(&confine, &entry.path) {
                ctx.progress.update(|p| p.entries_done += 1);
                continue;
            }
            ctx.progress.update(|p| {
                p.entries_done += 1;
                p.current = Some(entry.path.clone());
            });
            // Descent: dirs yes; symlinks NO (not followed → no cycles).
            if entry.kind == EntryKind::Dir {
                queue.push_back(entry.path.clone());
            }
            sent += 1;
            if sent > MAX_INDEX_ENTRIES {
                return Err(Error::LimitExceeded {
                    limit: Error::LIMIT_ENTRIES.to_owned(),
                });
            }
            let item = IndexEntry {
                path: entry.path,
                kind: entry.kind,
                size: entry.size,
                mtime_ms: entry.mtime_ms,
            };
            if out.send(item).await.is_err() {
                return Ok(());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use norte_proto::{TaskId, VPath};
    use norte_testkit::MemProvider;
    use norte_vfs::Provider as _;
    use tokio_util::sync::CancellationToken;

    use super::walk_for_index;

    /// #408: a walk whose reader is gone — a failed build — stops instead
    /// of parking forever on a full channel, however big the tree.
    #[tokio::test]
    async fn a_walk_whose_reader_is_gone_stops() {
        let mem = Arc::new(MemProvider::new());
        let root = VPath::parse("mem:///r").expect("wire");
        mem.mkdir(&root).await.expect("mkdir");
        for i in 0..40 {
            let dir = VPath::parse(&format!("mem:///r/d{i}")).expect("wire");
            mem.mkdir(&dir).await.expect("mkdir");
        }
        let (reporter, _rx) =
            crate::progress::ProgressReporter::new(TaskId::new(1), norte_proto::TaskKind::Index);
        let ctx = crate::scheduler::TaskCtx {
            pause: crate::scheduler::PauseGate::default(),
            cancel: CancellationToken::new(),
            progress: Arc::new(reporter),
            actor: crate::journal::Actor::User,
        };
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        drop(rx);
        let walked = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            walk_for_index(mem, root, &ctx, &tx),
        )
        .await
        .expect("the walk did not hang");
        assert!(walked.is_ok(), "the reader's side reports the error");
    }
}
