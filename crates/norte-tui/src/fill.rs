//! The channel a paginated listing arrives through, one per SLOT.
//!
//! A dir with 100k entries is not listed all at once: the provider hands it
//! out gradually and a drainer coalesces batches of [`FILL_BATCH`] — or
//! partial ones every [`FILL_INTERVAL`], so a slow remote listing is seen to
//! progress — and sends them over this channel. [`apply_fill_msg`] is who
//! applies them.
//!
//! One `Fill` per slot and not a global one: with a single slot, any `cd`
//! that starts a new listing dropped whoever was draining — and
//! `pane.mirror` is ONE keystroke with no change of focus, so the pane left
//! half-listed under a permanent "loading…" is exactly the one being looked
//! at.
//!
//! Used to live in the `ntc` binary's root, a crate DIFFERENT from this lib,
//! spread across four places: the two constants above, the two types in the
//! middle, and `spawn_fill` ten thousand lines away.

use futures::StreamExt as _;
use norte_core::backend::EntryStream;
use norte_frontend::layout::{BySlot, SlotId};
use norte_i18n::t;
use norte_proto::Entry;

use crate::app::App;
use crate::probes::Probed;

/// Batch the drainer coalesces before sending (avoids a re-sort per entry;
/// the full re-sort is done by [`crate::app::Pane::extend_listing`]). A dir
/// of 100k is ~24 batches ⇒ ~24 growing re-sorts during the fill; the
/// incremental merge (persisted keys) is the optimization deferred to an
/// issue.
pub const FILL_BATCH: usize = 4096;
/// The drainer flushes a PARTIAL batch every so often (besides when it
/// fills up): on a slow remote listing (pages per RTT) the user sees
/// progress and the "loading… (n)" counter advances instead of jumping by
/// 4096 at a time.
pub const FILL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// Message from a paginated listing's drainer to the run loop.
pub enum FillMsg {
    /// One more batch of entries for the pane.
    Batch(Vec<Entry>),
    /// The listing was cut off midway (a provider/daemon error): not
    /// silent (the UI warns and clears the `loading` state).
    Failed,
}

/// A listing being FILLED in the background: the drainer's channel, nothing
/// else. Dropping it drops the `rx` → the drainer dies on its next send →
/// releases the stream → cooperative cancellation (rule 3).
///
/// It does NOT carry its pane. The run loop keeps `[Option<Fill>; 2]`, one
/// slot per pane, so the array index IS the pane — the same shape as
/// `decorate_fetch`. A `pane` field beside it would be a second source of the
/// same fact, and two sources of one fact drift; a `pane.swap` would then have
/// to keep them in step by hand instead of just swapping the two slots.
///
/// One slot per pane is also the whole point rather than a tidiness: with a
/// single global slot, any cd that starts a new paginated listing dropped
/// whoever was draining — and `pane.mirror` makes that ONE keystroke with no
/// change of focus, so the pane left half-listed under a permanent
/// «cargando…» is the one the reader is looking at.
pub struct Fill {
    /// The batches the drainer is coalescing. Dropping the `Fill` closes the
    /// channel, which is how a half-done listing gets cancelled (rule 3).
    pub rx: tokio::sync::mpsc::Receiver<FillMsg>,
}

/// Core of the post-refresh ritual (#117 review, #118): releases the
/// paginated drainer ONLY if its pane was really re-listed (releasing it
/// blindly after a half-done Esc would leave the pane hanging in `loading`
/// forever, #78) and invalidates probe #52's dedup (a new listing re-lazifies
/// the entries). SINGLE body for `after_panes_refresh` (run loop) and the
/// `Cd::Refreshed` arm of `apply_cd` (Ctrl+R via `dispatch`).
pub fn release_refreshed_fill(
    panes: &crate::panel::PaneSlots,
    refreshed: &[bool],
    fill: &mut BySlot<Fill>,
    last_probed: &mut Probed,
) {
    if !refreshed.iter().any(|r| *r) {
        return;
    }
    for (pane, _) in refreshed.iter().enumerate().filter(|(_, r)| **r) {
        fill.remove(panes.slot_of(pane));
    }
    last_probed.clear();
}

/// Applies a pagination drainer's message (ADR 0017) to the pane. If the pane
/// switched to virtual search mode (Alt+F7 over a dir still paginating,
/// review MAJOR T6), the fill became STALE — `begin_search` emptied the
/// entries — and its drainer would feed the REAL listing as if they were hits
/// (the search's own root sneaking in among the results): the fill is
/// released and the batch is DISCARDED. A belt symmetric to `drain_search`'s
/// drain-guard; the suspender is releasing the fill in `launch_search`.
pub fn apply_fill_msg(app: &mut App, fill: &mut BySlot<Fill>, slot: SlotId, msg: Option<FillMsg>) {
    // The batch goes to ITS slot, not to a position. If that slot no longer
    // exists — the pane was closed, the tab was closed — the batch is
    // DROPPED: applying it to whoever now occupies that position would paint
    // another directory's entries into a listing, and nothing would say so.
    let Some(pane) = app.panes.browser_mut(slot) else {
        fill.remove(slot);
        return;
    };
    if pane.virtual_search {
        fill.remove(slot);
        return;
    }
    match msg {
        Some(FillMsg::Batch(batch)) => pane.extend_listing(batch),
        Some(FillMsg::Failed) => {
            pane.finish_listing();
            app.message = Some(t("msg-list-incomplete"));
            fill.remove(slot);
        }
        None => {
            pane.finish_listing();
            fill.remove(slot);
        }
    }
}

/// Starts the drainer for the REST of the listing: sends coalesced batches to
/// the run loop, which applies them with [`crate::app::Pane::extend_listing`].
/// Dropping the `rx` (a new cd for the SAME PANE) kills the drainer on its
/// next send → releases the stream (rule 3). No `pane`: who receives it is
/// decided by the slot the run loop files it under (see [`Fill`]).
#[must_use]
pub fn spawn_fill(mut stream: EntryStream) -> Fill {
    // Bounded at 1: the drainer does not run more than one batch ahead of the
    // run loop (backpressure); the memory peak is one batch, not the whole
    // dir.
    let (tx, rx) = tokio::sync::mpsc::channel::<FillMsg>(1);
    tokio::spawn(async move {
        let mut batch = Vec::with_capacity(FILL_BATCH);
        let mut flush = tokio::time::interval(FILL_INTERVAL);
        flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        flush.tick().await; // consumes the interval's immediate tick
        loop {
            tokio::select! {
                item = stream.next() => match item {
                    Some(Ok(e)) => {
                        batch.push(e);
                        if batch.len() >= FILL_BATCH
                            && tx
                                .send(FillMsg::Batch(std::mem::take(&mut batch)))
                                .await
                                .is_err()
                        {
                            return; // the run loop dropped the rx (new cd)
                        }
                    }
                    Some(Err(_)) => {
                        let _ = tx.send(FillMsg::Failed).await;
                        return;
                    }
                    None => {
                        if !batch.is_empty() {
                            let _ = tx.send(FillMsg::Batch(batch)).await;
                        }
                        return; // done: drop(tx) closes the channel → finish_listing
                    }
                },
                _ = flush.tick() => {
                    // Flushes a PARTIAL batch (progress on slow streams).
                    if !batch.is_empty()
                        && tx
                            .send(FillMsg::Batch(std::mem::take(&mut batch)))
                            .await
                            .is_err()
                    {
                        return;
                    }
                }
            }
        }
    });
    Fill { rx }
}

#[cfg(test)]
mod search_fill_tests {
    use super::{Fill, FillMsg, apply_fill_msg};
    use crate::app::{App, Pane};
    use norte_proto::{Entry, EntryKind, Segment, VPath};

    fn vp(w: &str) -> VPath {
        VPath::parse(w).expect("test wire")
    }

    fn file(dir: &VPath, name: &str) -> Entry {
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir.join(Segment::new(name.as_bytes().to_vec()).unwrap()),
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: None,
        }
    }

    /// review MAJOR T6: a large dir still PAGINATING (live fill) + `Alt+F7`
    /// on that pane → `begin_search` marks it virtual and empties it; a LATER
    /// batch from the REAL listing's drainer must never enter the virtual
    /// pane (it would sneak in as a hit — the search's own root among the
    /// results).
    #[test]
    fn fill_does_not_contaminate_the_virtual_pane() {
        let root = vp("file:///d");
        let mut app = App::new(
            Pane::new(root.clone(), vec![]),
            Pane::new(root.clone(), vec![]),
        );
        // Live paginated fill for pane 0 (dir still loading).
        let (_tx, rx) = tokio::sync::mpsc::channel::<FillMsg>(1);
        let mut fill: norte_frontend::layout::BySlot<Fill> = norte_frontend::layout::BySlot::new();
        fill.insert(crate::panel::SLOT_LEFT, Fill { rx });
        // Alt+F7 on pane 0: switches to virtual and empties it.
        app.panes[0].begin_search(root.clone());
        // A batch from the REAL listing's drainer arrives.
        apply_fill_msg(
            &mut app,
            &mut fill,
            crate::panel::SLOT_LEFT,
            Some(FillMsg::Batch(vec![
                file(&root, "real1"),
                file(&root, "real2"),
            ])),
        );
        assert!(
            app.panes[0].entries().is_empty(),
            "the real listing does NOT enter the virtual pane"
        );
        assert!(
            fill.get(crate::panel::SLOT_LEFT).is_none(),
            "the stale fill is released"
        );
        assert!(app.panes[0].virtual_search, "the pane stays in search mode");
    }

    /// A batch that arrives for a slot that NO LONGER EXISTS is dropped.
    ///
    /// This is the bug the P6 refactor pays for. With the fill filed by
    /// POSITION, a closed pane's batch used to be applied to whoever occupied
    /// that position when it arrived: the reader watched a listing grow with
    /// another directory's entries, with nothing saying so and no green suite
    /// catching it, because the listing kept arriving — just at the wrong
    /// place.
    #[test]
    fn a_batch_for_a_closed_slot_is_dropped() {
        let root = vp("mem:///d");
        let mut app = App::new(
            Pane::new(root.clone(), vec![file(&root, "a")]),
            Pane::new(root.clone(), vec![]),
        );
        let before = app.panes[0].entries().len();
        let ghost = norte_frontend::layout::SlotId(9_999);
        let (_tx, rx) = tokio::sync::mpsc::channel::<FillMsg>(1);
        let mut fill: norte_frontend::layout::BySlot<Fill> = norte_frontend::layout::BySlot::new();
        fill.insert(ghost, Fill { rx });

        apply_fill_msg(
            &mut app,
            &mut fill,
            ghost,
            Some(FillMsg::Batch(vec![file(&root, "from-elsewhere")])),
        );

        assert_eq!(
            app.panes[0].entries().len(),
            before,
            "the visible listing receives no entries from a closed pane"
        );
        assert!(
            fill.get(ghost).is_none(),
            "and the ghost slot is released instead of left draining"
        );
    }
}
