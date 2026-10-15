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

use norte_frontend::results::{MAX_HITS, ResultSet};
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
    /// C6: why `effect` is refused in the active listing, when that is a
    /// results tab: what writes at, opens at or sends the pane's location,
    /// which for a results tab is only the search's root.
    pub(super) fn refuses_in_results(&self, effect: &Effect) -> Option<&'static str> {
        if !self.slot().pane.is_results() {
            return None;
        }
        matches!(
            effect,
            Effect::CreateDirectory
                | Effect::CreateFile
                | Effect::Organize
                | Effect::RenameIa
                | Effect::RenameBatch
                | Effect::Pack
                | Effect::Join
                | Effect::Sync
                | Effect::Compare
                | Effect::Terminal
                | Effect::OpenTerminal
                | Effect::CommandLine
                | Effect::TerminalNew { .. }
                | Effect::Mirror
                | Effect::MirrorTarget
                | Effect::Swap
                | Effect::Send { .. }
                | Effect::SearchSemantic
        )
        .then_some("host-results-not-a-location")
    }

    /// C6: the refusal for what would write at, or run in, the active
    /// listing's `dir()` when it is a results tab, from a door that is not
    /// an `Effect` (a plugin row of the palette, the command bar).
    pub(super) fn refuse_in_results_tab(
        &mut self,
    ) -> Option<(ActionAck, Vec<BridgeEnvelope<UiUpdate>>)> {
        self.refuse_results_as("host-results-not-a-location")
    }

    /// The same refusal with `key`: `results-not-a-location` when the
    /// results tab is a DESTINATION (C5, a desktop drop).
    pub(super) fn refuse_results_as(
        &mut self,
        key: &'static str,
    ) -> Option<(ActionAck, Vec<BridgeEnvelope<UiUpdate>>)> {
        if !self.slot().pane.is_results() {
            return None;
        }
        Some((
            ActionAck::Unavailable {
                reason_key: key.to_owned(),
            },
            self.say(key),
        ))
    }

    /// C11: a move, delete or rename of `paths` was just submitted; every
    /// results tab showing one of them checks it once the mutation ends.
    pub(super) fn expect_gone(&mut self, paths: &[VPath]) {
        for h in self.slots.values_mut() {
            let shown: Vec<VPath> = paths
                .iter()
                .filter(|p| h.pane.real_entries().iter().any(|e| &e.path == *p))
                .cloned()
                .collect();
            if let Some(set) = h.pane.results_mut()
                && !shown.is_empty()
            {
                set.expect(shown);
            }
        }
    }

    /// (Re)runs `slot`'s query into it (refresh, session restore): it starts
    /// over empty under a fresh set capped at [`MAX_HITS`], and any run it
    /// had is let go first.
    pub(super) fn launch_results(
        &mut self,
        slot: u32,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) {
        self.abandon_results(slot);
        let Some(h) = self.slots.get_mut(&slot) else {
            return;
        };
        let Some(search) = h.pane.results().map(|r| r.search().clone()) else {
            return;
        };
        h.pane
            .begin_results(ResultSet::new(search.clone(), MAX_HITS), Vec::new());
        h.state = SlotState::Ready;
        let run = super::searchpanel::next_run_id();
        let abandoned = Arc::new(AtomicBool::new(false));
        super::searchpanel::spawn_search_run(
            run,
            search.params(super::searchpanel::now_ms()),
            true,
            Arc::clone(&abandoned),
            backend,
            mailbox,
        );
        self.results_runs.insert(
            slot,
            ResultsRun {
                run,
                panel_epoch: None,
                task: norte_proto::TaskId::new(0),
                abandoned,
            },
        );
    }

    /// Re-runs every VISIBLE results tab (`pane.refresh`, C2); what to
    /// publish.
    pub(super) fn rerun_visible_results(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let slots: Vec<u32> = self
            .slots
            .iter()
            .filter(|(id, h)| h.pane.is_results() && !self.hidden(**id))
            .map(|(id, _)| *id)
            .collect();
        let mut outgoing = Vec::new();
        for slot in slots {
            self.launch_results(slot, backend, mailbox);
            outgoing.extend(self.repaint_results(slot));
        }
        outgoing
    }

    /// C11: after the last live mutation over `affected` ended, every
    /// results tab expecting paths in those folders stats them in one
    /// spawned batch.
    ///
    /// A path that still answers STAYS expected: its own task may not have
    /// reached the actor yet (deletes are submitted in parallel, a batch's
    /// moves in series), and settling it here would leave a gone hit in the
    /// tab. Only a gone answer, or a re-run, ends an expectation; one whose
    /// mutation failed costs a stat on the folder's next mutation.
    pub(super) fn recheck_results(
        &mut self,
        affected: &[VPath],
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) {
        for (slot, h) in &self.slots {
            let Some(set) = h.pane.results() else {
                continue;
            };
            let paths: Vec<VPath> = set
                .expected()
                .into_iter()
                .filter(|p| p.parent().is_some_and(|d| affected.contains(&d)))
                .collect();
            if paths.is_empty() {
                continue;
            }
            let (slot, backend, mailbox) = (*slot, Arc::clone(backend), mailbox.clone());
            tokio::spawn(async move {
                let mut gone = Vec::new();
                for path in paths {
                    if matches!(
                        backend.stat(path.clone(), Vec::new()).await,
                        Err(Error::NotFound)
                    ) {
                        gone.push(path);
                    }
                }
                if gone.is_empty() {
                    return;
                }
                let _ = mailbox
                    .send(Message::Background(Box::new(Background::ResultsGone(
                        slot, gone,
                    ))))
                    .await;
            });
        }
    }

    /// The hits [`Self::recheck_results`] found gone leave the tab. A path
    /// no longer in the tab (a re-run replaced the entries since) is
    /// forgotten for nothing.
    pub(super) fn results_gone(
        &mut self,
        slot: u32,
        gone: &[VPath],
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Some(h) = self.slots.get_mut(&slot).filter(|h| h.pane.is_results()) else {
            return Vec::new();
        };
        let left = h.pane.forget(gone);
        if let Some(set) = h.pane.results_mut() {
            for path in gone {
                set.settle(path);
            }
        }
        if left == 0 {
            return Vec::new();
        }
        self.repaint_results(slot)
    }

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
