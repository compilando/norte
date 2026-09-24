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

/// Cap on materialized entries (anti-DoS; same class as the `list`/`search`
/// limits). A larger tree fails with `LimitExceeded` instead of exhausting RAM.
const MAX_INDEX_ENTRIES: usize = 5_000_000;

/// Walks `root` and returns every entry (dirs included) as an
/// [`IndexEntry`]. Cancelable: a tripped token cuts it short with
/// [`Error::Cancelled`] (the later `build` does not run, so the previous
/// index is left intact).
pub(crate) async fn walk_for_index(
    provider: Arc<dyn Provider>,
    root: VPath,
    ctx: &TaskCtx,
) -> Result<Vec<IndexEntry>, Error> {
    let confine = root.clone();
    let mut queue: VecDeque<VPath> = VecDeque::new();
    queue.push_back(root);
    let mut out: Vec<IndexEntry> = Vec::new();

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
            out.push(IndexEntry {
                path: entry.path,
                kind: entry.kind,
                size: entry.size,
                mtime_ms: entry.mtime_ms,
            });
            if out.len() > MAX_INDEX_ENTRIES {
                return Err(Error::LimitExceeded {
                    limit: Error::LIMIT_ENTRIES.to_owned(),
                });
            }
        }
    }
    Ok(out)
}
