//! Results tabs in the window (spec 2026-10-10): a tab of the listing whose
//! entries are a search's hits. The decisions live in the rustdoc of
//! [`norte_frontend::results`]; here is what only this host has: the run that
//! feeds a tab, and how it reaches the tab.
//!
//! A run is the search panel's forwarder (`searchpanel::spawn_search_run`)
//! with its HOST-WIDE run id. "Open as listing" MOVES a live panel run here
//! (C9) without touching the forwarder: its messages keep their id, and the
//! handlers ask this map first (`results_slot_of_run`). A run handed over
//! keeps `panel_epoch`, so the panel watches along while it still shows that
//! query.
//!
//! Part of `controller`: methods of `State`, like its neighbors.

#[allow(clippy::wildcard_imports)]
use super::*;

use std::sync::atomic::{AtomicBool, Ordering};

use norte_frontend::results::ResultSet;
use norte_frontend::search_status::Outcome;

/// The run feeding a results tab.
pub(super) struct ResultsRun {
    /// The host-wide id its forwarder's messages carry.
    pub(super) run: u64,
    /// The panel model's epoch, when the run was handed over by the panel.
    pub(super) panel_epoch: Option<u64>,
    /// Zero until `SearchRunLive` names it.
    pub(super) task: norte_proto::TaskId,
    /// Shared with the forwarder, which cancels the Task itself when the
    /// run is let go before its id reached the actor.
    pub(super) abandoned: Arc<AtomicBool>,
}

impl State {
    /// The results tab the run `run` feeds, if any.
    pub(super) fn results_slot_of_run(&self, run: u64) -> Option<u32> {
        self.results_runs
            .iter()
            .find(|(_, r)| r.run == run)
            .map(|(slot, _)| *slot)
    }

    /// A new tab next to the active listing showing `set` over `entries`,
    /// focused; its id and what to publish.
    pub(super) fn open_results_slot(
        &mut self,
        set: ResultSet,
        entries: Vec<Entry>,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (u32, Vec<BridgeEnvelope<UiUpdate>>) {
        use norte_frontend::layout::KindId;
        let next_to = self.active();
        let id = self.new_slot();
        let root = set.root().clone();
        let mut slot = Slot::empty(
            root.clone(),
            self.config.common.ui_show_hidden.unwrap_or(true),
            self.columns.sort_for(root.scheme()),
            self.config.common.ui_parent_entry.unwrap_or(true),
        );
        slot.pane.begin_results(set, entries);
        // At rest: it has its rows, and `wake_visible` must not list the
        // root into it.
        slot.state = SlotState::Ready;
        // Before the tree: `set_tree` keeps a slot it finds, and seeds an
        // empty one at the active folder otherwise.
        self.slots.insert(id, slot);
        let updated = self
            .tree
            .add_tab(SlotId(next_to), &Node::slot(SlotId(id), KindId::browser()));
        let (_, outgoing) = self.apply_layout_with(updated, Some(SlotId(id)), backend, mailbox);
        (id, outgoing)
    }

    /// The rows, the header and the tab strip (the title counts) of the
    /// results tab `slot`.
    fn repaint_results(&mut self, slot: u32) -> Vec<BridgeEnvelope<UiUpdate>> {
        if !self.slots.contains_key(&slot) {
            return Vec::new();
        }
        let rows = self.patch_rows_of(slot);
        vec![rows, self.parche(vec![ViewChange::Layout(self.layout())])]
    }

    /// A batch for the results tab `slot`. One that no longer shows results
    /// (closed, or navigated to a folder before the abandon) lets the run go:
    /// a late batch never lands in a folder listing.
    pub(super) fn results_hits(
        &mut self,
        slot: u32,
        batch: &norte_proto::methods::SearchHits,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let epoch = self.results_runs.get(&slot).and_then(|r| r.panel_epoch);
        let Some(h) = self.slots.get_mut(&slot).filter(|h| h.pane.is_results()) else {
            self.abandon_results(slot);
            return self.repaint_search_panel();
        };
        if let Some(set) = h.pane.results_mut() {
            set.note_hits(batch);
        }
        h.pane.extend(batch.entries.clone());
        let mut outgoing = self.repaint_results(slot);
        if let Some(e) = epoch {
            outgoing.extend(self.panel_sees_hits(e, batch));
        }
        outgoing
    }

    /// The run feeding `slot` ended with `outcome`.
    pub(super) fn results_finished(
        &mut self,
        slot: u32,
        outcome: Outcome,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let epoch = self.results_runs.remove(&slot).and_then(|r| r.panel_epoch);
        if let Some(set) = self.slots.get_mut(&slot).and_then(|h| h.pane.results_mut()) {
            set.set_outcome(outcome.clone());
        }
        let mut outgoing = self.repaint_results(slot);
        if let Some(e) = epoch {
            outgoing.extend(self.panel_sees_end(e, outcome));
        }
        outgoing
    }

    /// Cancels and forgets `slot`'s run, if any (closing, navigating away,
    /// re-running). Before its Task id is known, the forwarder cancels it
    /// through `abandoned` as soon as it lands. A tab still showing results
    /// reads as cancelled, not as searching forever, and so does a panel
    /// still watching the run: the forwarder says nothing more once let go.
    /// The panel's next paint shows it; callers that publish nothing else
    /// repaint it themselves.
    pub(super) fn abandon_results(&mut self, slot: u32) {
        let Some(run) = self.results_runs.remove(&slot) else {
            return;
        };
        run.abandoned.store(true, Ordering::SeqCst);
        if run.task.get() != 0 {
            let _ = self.cancel(run.task.get());
        }
        if let Some(e) = run.panel_epoch {
            self.panel_sees_cancel(e);
        }
        if let Some(set) = self
            .slots
            .get_mut(&slot)
            .and_then(|h| h.pane.results_mut())
            .filter(|s| s.is_running())
        {
            set.set_outcome(Outcome::Cancelled);
        }
    }
}
