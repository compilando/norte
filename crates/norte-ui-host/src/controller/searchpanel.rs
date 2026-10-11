//! The search panel in the window (spec 2026-10-09): the slot's `kind`, its
//! actions, its pause and its runs.
//!
//! The MODEL — the form, the pause, the grouping, what a row does — is
//! [`norte_frontend::search_panel`], the terminal's. What lives here is what
//! only this host knows: the pause's tic (C5: the host has no tick, so each
//! edit schedules one), the run in flight with its forwarder, the ONE row
//! the panel's runs keep on the board (C4: the previous run's row goes when
//! the next one is live), and the projection to `SlotView::Search`.
//!
//! Part of `controller`: methods of `State`, like its neighbors. The only
//! writer is still the actor.

// These modules are the same `impl State` split into pieces, so they use
// the same imports as the parent (see `search.rs`).
#[allow(clippy::wildcard_imports)]
use super::*;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use norte_frontend::search::{ID_KINDS, SearchField};
use norte_frontend::search_panel::{
    Activation, DEBOUNCE, Due, FolderNode, KEY_FAILED, PanelStatus, Row, SearchPanel,
};
use norte_frontend::search_status::Outcome;

use crate::action::DialogFieldValue;
use crate::dto::{SearchPanelRowView, SearchPanelSlotView};

/// The slot's `kind`, exactly as the shared registry declares it.
pub(super) const KIND: &str = "search";

/// Rows the view carries around the cursor until the renderer says which
/// ones it has in view.
const WINDOW_DEFAULT: u32 = 200;
/// Rows the view carries at most, whatever the renderer asks.
const WINDOW_MAX: u32 = 500;

/// The panel's state: the shared model and the run this host has in
/// flight.
pub(super) struct PanelState {
    model: SearchPanel,
    run: Option<PanelRunHost>,
    /// The rows the renderer has in view, `(first, count)`; `None` = around
    /// the cursor.
    window: Option<(u64, u32)>,
    /// The board row of the run the live one replaced: it goes when the
    /// next run is live, so the board keeps ONE row for the panel (C4).
    previous_row: Option<u64>,
}

impl PanelState {
    fn new(root: VPath) -> Self {
        Self {
            model: SearchPanel::new(root),
            run: None,
            window: None,
            previous_row: None,
        }
    }
}

/// The run in flight, as this host knows it.
struct PanelRunHost {
    /// This run's identity across the WHOLE host, which its messages carry
    /// back. The model's epoch starts over with every panel, so a closed
    /// panel's run and a reopened one's first would both be epoch 1, and a
    /// late message of the old walk would land on the new one.
    run: u64,
    /// The model's epoch for this run, for the calls into the model.
    epoch: u64,
    /// The daemon's Task, once `SearchPanelLive` brings it. Zero until then.
    task: norte_proto::TaskId,
    /// Shared with the forwarder, which cancels the Task itself when the
    /// run was let go before its id reached the actor (Task 1's shape).
    abandoned: Arc<AtomicBool>,
    /// The walk ended: nothing left to cancel, only a row to forget.
    ended: bool,
}

/// The next host-wide run identity (see [`PanelRunHost::run`]).
fn next_run_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// The clock the pause is measured with: tokio's, so a test on a stopped
/// clock drives the pause too.
fn now() -> std::time::Instant {
    tokio::time::Instant::now().into_std()
}

/// The instant "changed in the last N days" counts from.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// The status sentence of a run. The counts pick their own id (`-one` or
/// plural) HERE: Fluent selects no plural on a string argument.
fn run_text(lang: norte_i18n::Lang, key: &str, files: usize, folders: usize) -> String {
    let count = |one: &str, many: &str, n: usize| {
        if n == 1 {
            norte_i18n::t_in(lang, one)
        } else {
            norte_i18n::ta_in(lang, many, &[("n", &n.to_string())])
        }
    };
    norte_i18n::ta_in(
        lang,
        key,
        &[
            ("n", &files.to_string()),
            (
                "files",
                &count("search-panel-files-one", "search-panel-files", files),
            ),
            (
                "folders",
                &count("search-panel-folders-one", "search-panel-folders", folders),
            ),
        ],
    )
}

/// The panel's status sentence, translated.
fn status_text(lang: norte_i18n::Lang, model: &SearchPanel) -> String {
    match model.status() {
        PanelStatus::Idle => norte_i18n::t_in(lang, "search-panel-idle"),
        PanelStatus::Unreadable(field) => norte_i18n::ta_in(
            lang,
            "search-panel-unreadable",
            &[("field", &norte_i18n::t_in(lang, field.key()))],
        ),
        PanelStatus::Run { key, error, .. } if key == KEY_FAILED => norte_i18n::ta_in(
            lang,
            KEY_FAILED,
            &[("error", error.as_deref().unwrap_or_default())],
        ),
        PanelStatus::Run {
            key,
            files,
            folders,
            ..
        } => run_text(lang, key, files, folders),
    }
}

/// A folder's label: relative to the root (`.` for the root itself), or
/// the whole path when it is not under it.
fn folder_label(dir: &VPath, root: &VPath) -> (String, bool) {
    let under: Vec<&[u8]> = dir.segments().collect();
    let base: Vec<&[u8]> = root.segments().collect();
    if dir.scheme() != root.scheme()
        || dir.authority() != root.authority()
        || !under.starts_with(&base)
    {
        return norte_frontend::path_display(dir);
    }
    if under.len() == base.len() {
        return (".".to_owned(), false);
    }
    let mut hostile = false;
    let parts: Vec<String> = under[base.len()..]
        .iter()
        .map(|s| {
            let (text, h) = norte_frontend::display_name(s);
            hostile |= h;
            text
        })
        .collect();
    (parts.join("/"), hostile)
}

/// Row `i` of `rows`, as it crosses.
fn row_view(
    lang: norte_i18n::Lang,
    tree: &[FolderNode],
    root: &VPath,
    rows: &[Row],
    i: usize,
) -> SearchPanelRowView {
    let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
    match rows[i] {
        Row::Folder { folder } => {
            let node = &tree[folder];
            let (path, hostile) = folder_label(&node.dir, root);
            SearchPanelRowView::Folder {
                path: clamp_display(path),
                hostile,
                count: count(node.files.len()),
                // Unfolded = its first file follows it: a folder exists
                // because a file was found in it.
                open: matches!(rows.get(i + 1), Some(Row::File { folder: f, .. }) if *f == folder),
            }
        }
        Row::File { folder, file } => {
            let f = &tree[folder].files[file];
            let (name, hostile) = f
                .entry
                .path
                .file_name()
                .map_or((String::new(), false), |s| {
                    norte_frontend::display_name(s.as_bytes())
                });
            SearchPanelRowView::File {
                name: clamp_display(name),
                hostile,
                count: count(f.matches.len()).saturating_add(f.more),
            }
        }
        Row::Line { folder, file, line } => {
            let m = &tree[folder].files[file].matches[line];
            // Masked char for char, so the ranges still fall on the same
            // chars.
            let preview = clamp_display(norte_encoding::mask_terminal_hazards(&m.preview));
            // A clamp cuts the text (and ends it in `…`): a range past what
            // is left marks nothing, and one across the cut stops at it.
            let kept = preview.chars().count().saturating_sub(usize::from(
                preview.len() < m.preview.len() && preview.ends_with('…'),
            ));
            let ranges = m
                .ranges
                .iter()
                .filter_map(|&(start, end)| {
                    let limit = u32::try_from(kept).unwrap_or(u32::MAX);
                    (start < limit).then(|| (start, end.min(limit)))
                })
                .collect();
            SearchPanelRowView::Line {
                line: m.line,
                preview,
                ranges,
            }
        }
        Row::More { folder, file } => SearchPanelRowView::More {
            label: norte_i18n::ta_in(
                lang,
                "search-panel-more",
                &[("n", &tree[folder].files[file].more.to_string())],
            ),
        },
    }
}

/// The rows a view carries: the renderer's window, or one around the
/// cursor until it says; never more than [`WINDOW_MAX`].
///
/// The renderer's window is honoured even when the cursor is outside it:
/// the wheel scrolled away from the cursor, and dragging the window back
/// to it sent rows the renderer does not have in view, so it painted
/// blanks. A keyboard move forgets the window (`window = None`), and the
/// renderer scrolls to the cursor and says again.
fn window_of(window: Option<(u64, u32)>, cursor: u64, total: u64) -> (u64, u64) {
    let count = u64::from(
        window
            .map_or(WINDOW_DEFAULT, |(_, c)| c)
            .clamp(1, WINDOW_MAX),
    );
    let first = window.map_or_else(|| cursor.saturating_sub(count / 2), |(f, _)| f);
    let first = first.min(total.saturating_sub(count));
    (first, first.saturating_add(count).min(total))
}

impl State {
    /// The panel's view for slot `slot`.
    pub(super) fn search_panel_view(&self, slot: u32) -> SearchPanelSlotView {
        // Every path that places the slot gives it a state; a placeholder
        // keeps a view honest (idle, empty) if one ever slipped through.
        let placeholder;
        let state = if let Some((_, s)) = &self.search_panel {
            s
        } else {
            placeholder = PanelState::new(self.slot().pane.dir().clone());
            &placeholder
        };
        let model = &state.model;
        let (root, root_hostile) = norte_frontend::path_display(model.root());
        let rows = model.rows();
        let total = rows.len() as u64;
        let cursor = (!rows.is_empty()).then(|| model.cursor() as u64);
        let (first, end) = window_of(state.window, cursor.unwrap_or(0), total);
        let unreadable = match model.status() {
            PanelStatus::Unreadable(f) => Some(f.id().to_owned()),
            _ => None,
        };
        SearchPanelSlotView {
            slot_id: slot,
            title: norte_i18n::t_in(self.lang, "search-panel-title"),
            root: clamp_display(root),
            root_hostile,
            fields: super::search::search_fields(model.form()),
            filters_open: model.filters_open(),
            filters_set: u32::try_from(model.filters_set()).unwrap_or(u32::MAX),
            unreadable,
            status: clamp_display(status_text(self.lang, model)),
            running: model.is_running(),
            dimmed: model.is_dimmed(),
            first_row: first,
            total_rows: total,
            rows: (first..end)
                .map(|i| {
                    row_view(
                        self.lang,
                        model.tree(),
                        model.root(),
                        &rows,
                        usize::try_from(i).unwrap_or(usize::MAX),
                    )
                })
                .collect(),
            cursor,
        }
    }

    /// The panel ALONE, as a `slot` patch (#401): what it shows changes on
    /// its own — a batch, a tic — and the rest of the screen did not.
    fn repaint_search_panel(&mut self) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Some((slot, _)) = &self.search_panel else {
            return Vec::new();
        };
        let view = self.search_panel_view(*slot);
        vec![self.parche(vec![ViewChange::Slot {
            slot: Box::new(SlotView::Search(Box::new(view))),
        }])]
    }

    /// The board after a row left it.
    fn repaint_board(&mut self, mailbox: &mpsc::Sender<Message>) -> BridgeEnvelope<UiUpdate> {
        self.annotate_strip(mailbox);
        self.parche(vec![ViewChange::Tasks {
            tasks: self.vistas_de_tasks(),
            cursor: self.board_cursor(),
        }])
    }

    /// A panel slot that is placed gets its state, rooted at the focused
    /// listing's folder; one that left takes its run and its board rows
    /// with it. Called by every change of the tree that can place or remove
    /// it, and at startup for a restored one.
    pub(super) fn reconcile_search_panel(&mut self, mailbox: &mpsc::Sender<Message>) {
        match (self.slot_of_kind(KIND), self.search_panel.is_some()) {
            (Some(SlotId(id)), true) => {
                if let Some((slot, _)) = self.search_panel.as_mut() {
                    *slot = id;
                }
            }
            (Some(SlotId(id)), false) => {
                let root = self.slot().pane.dir().clone();
                self.search_panel = Some((id, PanelState::new(root)));
            }
            (None, true) => {
                // Review Focus 5: closing cancels what runs, and its row
                // goes with the panel that owned it.
                self.abandon_panel_run();
                if let Some((_, state)) = self.search_panel.take()
                    && let Some(row) = state.previous_row
                {
                    self.tasks.remove(&row);
                }
                self.annotate_strip(mailbox);
            }
            (None, false) => {}
        }
    }

    /// Lets go of the run in flight, if any (Task 1's shape): the forwarder
    /// stops, and the Task is cancelled now if its id is known — before
    /// that, the forwarder cancels it as soon as it lands. Its board row is
    /// remembered, to go when the next run is live.
    fn abandon_panel_run(&mut self) {
        let Some(run) = self.search_panel.as_mut().and_then(|(_, s)| s.run.take()) else {
            return;
        };
        run.abandoned.store(true, Ordering::SeqCst);
        let id = run.task.get();
        if id == 0 {
            return;
        }
        if !run.ended {
            let _ = self.cancel(id);
        }
        if let Some((_, s)) = self.search_panel.as_mut() {
            // Two runs replaced before the next one went live: the older
            // row cannot wait for a run that will never come.
            if let Some(older) = s.previous_row.replace(id) {
                self.tasks.remove(&older);
            }
        }
    }

    /// Schedules a tic for when the pause the model just started is over
    /// (C5). The deadline is taken NOW, not when the task first runs.
    fn schedule_search_panel_tic(mailbox: &mpsc::Sender<Message>) {
        let at = tokio::time::Instant::now() + DEBOUNCE;
        let mailbox = mailbox.clone();
        tokio::spawn(async move {
            tokio::time::sleep_until(at).await;
            let _ = mailbox.send(Message::SearchPanelTic).await;
        });
    }

    /// A tic: if the pause is over, the model says what to do, and it is
    /// done — cancel-then-launch, so two walks never race over the tree.
    pub(super) fn search_panel_tick(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Some((_, state)) = self.search_panel.as_mut() else {
            return Vec::new();
        };
        let Some(due) = state.model.due(now(), now_ms()) else {
            return Vec::new();
        };
        match due {
            Due::Launch { epoch, params } => {
                self.abandon_panel_run();
                self.launch_panel_run(epoch, *params, backend, mailbox);
            }
            Due::Clear => self.abandon_panel_run(),
            // Only dimmed: what runs keeps landing on what is shown.
            Due::Unreadable(_) => {}
        }
        self.repaint_search_panel()
    }

    /// Launches the run `epoch` and hooks up its forwarder, the one
    /// `launch_search` uses: the Task to the board, its id, its batches and
    /// how it ended, all through the mailbox. Its messages carry the run's
    /// host-wide identity, and none is sent once the run was let go.
    fn launch_panel_run(
        &mut self,
        epoch: u64,
        params: norte_proto::methods::FsSearchParams,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) {
        let run = next_run_id();
        let abandoned = Arc::new(AtomicBool::new(false));
        let abandoned2 = Arc::clone(&abandoned);
        let backend = Arc::clone(backend);
        let mailbox = mailbox.clone();
        let background = |b: Background| Message::Background(Box::new(b));
        tokio::spawn(async move {
            let (task, mut rx) = match backend.search(params).await {
                Ok(pair) => pair,
                Err(e) => {
                    if !abandoned2.load(Ordering::SeqCst) {
                        let _ = mailbox
                            .send(background(Background::SearchPanelBroken(run, Box::new(e))))
                            .await;
                    }
                    return;
                }
            };
            let cancel = Arc::clone(&task.cancel);
            // Let go while the daemon was accepting it: nobody else knows
            // the id, so this cancels, and the run never reaches the board.
            if abandoned2.load(Ordering::SeqCst) {
                cancel();
                return;
            }
            let id = task.id;
            let progress = task.progress.clone();
            let _ = mailbox
                .send(Message::TaskNew(Box::new((task, Vec::new(), None))))
                .await;
            // Named HERE and not with the first batch: there may be none.
            let _ = mailbox
                .send(background(Background::SearchPanelLive(run, id)))
                .await;
            while let Some(batch) = rx.recv().await {
                if abandoned2.load(Ordering::SeqCst) {
                    cancel();
                    return;
                }
                if mailbox
                    .send(background(Background::SearchPanelHits(
                        run,
                        Box::new(batch),
                    )))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            // A run let go says nothing more: the walk it ended is not the
            // one the panel is waiting for.
            if abandoned2.load(Ordering::SeqCst) {
                return;
            }
            let state = progress.borrow().state.clone();
            let _ = mailbox
                .send(background(Background::SearchPanelEnded(
                    run,
                    Box::new(state),
                )))
                .await;
        });
        if let Some((_, s)) = self.search_panel.as_mut() {
            s.run = Some(PanelRunHost {
                run,
                epoch,
                task: norte_proto::TaskId::new(0),
                abandoned,
                ended: false,
            });
        }
    }

    /// The model's epoch of the run `run`, if it is the one in flight.
    fn panel_run_epoch(&self, run: u64) -> Option<u64> {
        self.search_panel
            .as_ref()
            .and_then(|(_, s)| s.run.as_ref())
            .filter(|r| r.run == run)
            .map(|r| r.epoch)
    }

    /// The run `run` is live with Task `id`. The current one takes its id
    /// and the row of the run it replaced leaves the board; a replaced one
    /// that slipped onto the board between the forwarder's check and the
    /// abandon is cancelled and taken off it.
    pub(super) fn search_panel_live(
        &mut self,
        run: u64,
        id: norte_proto::TaskId,
        mailbox: &mpsc::Sender<Message>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let gone = if self.panel_run_epoch(run).is_some() {
            let Some((_, s)) = self.search_panel.as_mut() else {
                return Vec::new();
            };
            if let Some(run) = s.run.as_mut() {
                run.task = id;
            }
            s.previous_row.take()
        } else {
            let _ = self.cancel(id.get());
            Some(id.get())
        };
        let Some(row) = gone else {
            return Vec::new();
        };
        if self.tasks.remove(&row).is_none() {
            return Vec::new();
        }
        vec![self.repaint_board(mailbox)]
    }

    /// A batch of the run `run`; one of another run is dropped here, and a
    /// stale epoch by the model.
    pub(super) fn search_panel_hits(
        &mut self,
        run: u64,
        batch: &norte_proto::methods::SearchHits,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Some(epoch) = self.panel_run_epoch(run) else {
            return Vec::new();
        };
        let Some((_, s)) = self.search_panel.as_mut() else {
            return Vec::new();
        };
        if !s.model.apply_hits(epoch, batch) {
            return Vec::new();
        }
        self.repaint_search_panel()
    }

    /// The run `run` never got queued: the panel says why.
    pub(super) fn search_panel_broken(
        &mut self,
        run: u64,
        e: &Error,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let category = clamp_display(norte_frontend::error::error_category_in(self.lang, e));
        self.search_panel_finished(run, Outcome::Failed(category))
    }

    /// The run `run` sent its last batch, and its Task stood at `state`.
    /// A channel that closed before the terminal state was published reads
    /// as done: the walker sends no more.
    pub(super) fn search_panel_ended(
        &mut self,
        run: u64,
        state: &norte_proto::TaskState,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let lang = self.lang;
        let outcome = norte_frontend::search_status::outcome_of(state, |e| {
            clamp_display(norte_frontend::error::error_category_in(lang, e))
        })
        .unwrap_or(Outcome::Done);
        self.search_panel_finished(run, outcome)
    }

    fn search_panel_finished(
        &mut self,
        run: u64,
        outcome: Outcome,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Some(epoch) = self.panel_run_epoch(run) else {
            return Vec::new();
        };
        let Some((_, s)) = self.search_panel.as_mut() else {
            return Vec::new();
        };
        s.model.finished(epoch, outcome);
        if let Some(run) = s.run.as_mut() {
            run.ended = true;
        }
        self.repaint_search_panel()
    }

    /// The panel's actions. Each names its slot: one that is not the
    /// panel's is a race with an earlier layout.
    pub(super) fn search_panel_action(
        &mut self,
        action: &UiAction,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let slot_id = match action {
            UiAction::SearchPanelEdit { slot_id, .. }
            | UiAction::SearchPanelGo { slot_id }
            | UiAction::SearchPanelReroot { slot_id }
            | UiAction::SearchPanelFilters { slot_id }
            | UiAction::SearchPanelActivate { slot_id, .. }
            | UiAction::SearchPanelFold { slot_id, .. }
            | UiAction::SearchPanelVisibleRange { slot_id, .. } => *slot_id,
            _ => return (Self::stale(StaleAction::Modal), Vec::new()),
        };
        if self
            .search_panel
            .as_ref()
            .is_none_or(|(s, _)| *s != slot_id)
        {
            return (Self::stale(StaleAction::Generation), Vec::new());
        }
        match action {
            UiAction::SearchPanelEdit { field, value, .. } => {
                self.search_panel_edit(field, value, mailbox)
            }
            UiAction::SearchPanelGo { .. } => {
                if let Some((_, s)) = self.search_panel.as_mut() {
                    s.model.force(now());
                }
                // Enter does not wait: the tic runs now.
                (self.applied(), self.search_panel_tick(backend, mailbox))
            }
            UiAction::SearchPanelReroot { .. } => {
                let root = self.slot().pane.dir().clone();
                // What runs under the old root stops now, not at the tic.
                self.abandon_panel_run();
                if let Some((_, s)) = self.search_panel.as_mut() {
                    s.model.set_root(root, now());
                }
                Self::schedule_search_panel_tic(mailbox);
                (self.applied(), self.repaint_search_panel())
            }
            UiAction::SearchPanelFilters { .. } => {
                if let Some((_, s)) = self.search_panel.as_mut() {
                    s.model.toggle_filters();
                }
                (self.applied(), self.repaint_search_panel())
            }
            UiAction::SearchPanelActivate { row, .. } => {
                let Some(i) = self.search_panel_row(*row) else {
                    return (Self::stale(StaleAction::Generation), Vec::new());
                };
                let activation = self.search_panel.as_mut().and_then(|(_, s)| {
                    s.model.set_cursor(i);
                    s.model.activate()
                });
                self.search_panel_activation(activation, backend, mailbox)
            }
            UiAction::SearchPanelFold { row, open, .. } => {
                let Some(i) = self.search_panel_row(*row) else {
                    return (Self::stale(StaleAction::Generation), Vec::new());
                };
                if let Some((_, s)) = self.search_panel.as_mut() {
                    s.model.set_cursor(i);
                    s.model.fold(*open);
                }
                (self.applied(), self.repaint_search_panel())
            }
            UiAction::SearchPanelVisibleRange { first, count, .. } => {
                if let Some((_, s)) = self.search_panel.as_mut() {
                    s.window = Some((*first, (*count).min(WINDOW_MAX)));
                }
                (self.applied(), self.repaint_search_panel())
            }
            _ => (Self::stale(StaleAction::Modal), Vec::new()),
        }
    }

    /// Row `row` of the panel, if the tree has it: one past the end means
    /// the tree changed under the click, and it is not clamped.
    fn search_panel_row(&self, row: u64) -> Option<usize> {
        let (_, s) = self.search_panel.as_ref()?;
        let i = usize::try_from(row).ok()?;
        (i < s.model.rows().len()).then_some(i)
    }

    /// A field touched: the same ids and guards as the dialog's form, then
    /// the pause starts over.
    fn search_panel_edit(
        &mut self,
        field: &str,
        value: &DialogFieldValue,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if let DialogFieldValue::Text { text } = value {
            if text.len() > MAX_NAME {
                return (
                    ActionAck::Unavailable {
                        reason_key: "host-name-too-long".to_owned(),
                    },
                    Vec::new(),
                );
            }
            // A `U+FFFD` is put there by this host's masking, not typed: the
            // renderer re-seeded the input with what it painted.
            if text.contains('\u{FFFD}') {
                return (Self::stale(StaleAction::Modal), Vec::new());
            }
        }
        let Some((_, s)) = self.search_panel.as_mut() else {
            return (Self::stale(StaleAction::Generation), Vec::new());
        };
        let known = match (SearchField::por_id(field), value) {
            (Some(f), DialogFieldValue::Text { text }) => {
                s.model.edit(f, text.clone(), now());
                true
            }
            (None, DialogFieldValue::Toggled) if field != ID_KINDS => s.model.toggle(field, now()),
            (None, DialogFieldValue::Cycled) if field == ID_KINDS => s.model.toggle(field, now()),
            _ => false,
        };
        if !known {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-unknown-field".to_owned(),
                },
                Vec::new(),
            );
        }
        Self::schedule_search_panel_tic(mailbox);
        (self.applied(), self.repaint_search_panel())
    }

    /// Does what activating a row asked: a fold repaints, a file is gone to
    /// in the listing (as a search result is), a line opens in the viewer
    /// at it.
    fn search_panel_activation(
        &mut self,
        activation: Option<Activation>,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match activation {
            None | Some(Activation::Toggled) => (self.applied(), self.repaint_search_panel()),
            Some(Activation::GoTo { dir, entry }) => {
                // The keys go back to the listing, which lands on the entry
                // byte for byte once its folder is listed.
                let listing = self.active();
                self.roles.set(RoleId::Active, SlotId(listing));
                self.reconciles_roles();
                self.slot_mut().pane.set_pending_focus(entry);
                let mut outgoing = self.repaint_search_panel();
                outgoing.push(self.parche(vec![ViewChange::Layout(self.layout())]));
                outgoing.extend(self.navigate(&dir, Trail::Record, backend, mailbox));
                (self.applied(), outgoing)
            }
            Some(Activation::View { path, target }) => {
                let (ack, mut outgoing) =
                    self.request_viewer_of(path, Some(target), backend, mailbox);
                outgoing.splice(0..0, self.repaint_search_panel());
                (ack, outgoing)
            }
        }
    }

    /// Is the panel the slot with the keyboard?
    pub(super) fn search_panel_has_focus(&self) -> bool {
        self.search_panel
            .as_ref()
            .is_some_and(|(slot, _)| self.roles.get(RoleId::Active) == Some(SlotId(*slot)))
    }

    /// A command while the panel has the keyboard: the cursor's moves. What
    /// is not its own goes its normal way.
    pub(super) fn search_panel_effect(
        &mut self,
        effect: Effect,
    ) -> Option<(ActionAck, Vec<BridgeEnvelope<UiUpdate>>)> {
        /// Rows a page moves, like the terminal's panel.
        const PAGE: usize = 10;
        let (_, s) = self.search_panel.as_mut()?;
        let steps = |n: i64| usize::try_from(n.unsigned_abs()).unwrap_or(usize::MAX);
        match effect {
            Effect::Cursor(n) if n < 0 => s.model.up(steps(n)),
            Effect::Cursor(n) => s.model.down(steps(n)),
            Effect::Page(n) if n < 0 => s.model.up(steps(n).saturating_mul(PAGE)),
            Effect::Page(n) => s.model.down(steps(n).saturating_mul(PAGE)),
            Effect::End { al_final: false } => s.model.home(),
            Effect::End { al_final: true } => s.model.end(),
            _ => return None,
        }
        // The view goes back around the cursor (see `window_of`).
        s.window = None;
        Some((self.applied(), self.repaint_search_panel()))
    }

    /// Enter with the panel focused: activates the cursor's row.
    pub(super) fn search_panel_enter(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let activation = self.search_panel.as_mut().and_then(|(_, s)| {
            s.window = None;
            s.model.activate()
        });
        self.search_panel_activation(activation, backend, mailbox)
    }

    /// ←/→ with the panel focused fold and unfold: no keymap gives the
    /// arrows a command over a list, so they are matched here, before it.
    pub(super) fn key_in_search_panel(
        &mut self,
        k: &crate::keys::KeyInput,
    ) -> Option<(ActionAck, Vec<BridgeEnvelope<UiUpdate>>)> {
        if k.ctrl || k.alt || k.meta || k.shift || !self.search_panel_has_focus() {
            return None;
        }
        let open = match k.key.as_str() {
            "ArrowRight" | "right" => true,
            "ArrowLeft" | "left" => false,
            _ => return None,
        };
        let (_, s) = self.search_panel.as_mut()?;
        s.model.fold(open);
        s.window = None;
        Some((self.applied(), self.repaint_search_panel()))
    }
}

#[cfg(test)]
mod tests {
    use super::window_of;

    /// With no window from the renderer the view goes around the cursor;
    /// the renderer's window is honoured even with the cursor outside it
    /// (a wheel scrolled away from it); never more than 500 rows.
    #[test]
    fn the_window_is_the_renderers_and_has_a_cap() {
        // Around the cursor by default.
        assert_eq!(window_of(None, 1000, 5000), (900, 1100));
        // Near the top and the end, clamped.
        assert_eq!(window_of(None, 3, 5000), (0, 200));
        assert_eq!(window_of(None, 4999, 5000), (4800, 5000));
        // The renderer's window, capped.
        assert_eq!(window_of(Some((40, 9999)), 40, 5000), (40, 540));
        // A cursor outside it does NOT drag it back: the rows scrolled to
        // are the ones sent, or the renderer paints blanks.
        assert_eq!(window_of(Some((40, 30)), 100, 5000), (40, 70));
        assert_eq!(window_of(Some((40, 30)), 10, 5000), (40, 70));
        assert_eq!(window_of(Some((3000, 60)), 0, 5000), (3000, 3060));
        // A window past a tree that shrank is pulled back inside it.
        assert_eq!(window_of(Some((4990, 60)), 0, 5000), (4940, 5000));
        // Fewer rows than the window.
        assert_eq!(window_of(None, 2, 3), (0, 3));
        assert_eq!(window_of(None, 0, 0), (0, 0));
    }
}
