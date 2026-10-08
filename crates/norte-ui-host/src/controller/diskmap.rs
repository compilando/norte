//! The disk map, in the window (phase 4).
//!
//! The map's state is the SHARED one (`norte_frontend::diskmap`), the same
//! the terminal uses: which directory is described, what was measured, and
//! which child is chosen. And the layout into rectangles is shared too
//! (`norte_frontend::treemap::squarify`). What is here is the WIRING:
//! requesting the measurement, landing it, and resolving a click.
//!
//! The mold is the plugin panel's, on purpose: one state per slot, one live
//! request with its token, and a response that arrives with another token is
//! discarded. What changes is what is requested — a measurement, not a
//! frame — and that here what is kept between repaints is what was MEASURED,
//! which costs minutes.
//!
//! # Why the HOST lays out and not the renderer
//! A treemap computed twice is two different treemaps the moment someone
//! touches a rounding, and then the rectangle that is painted and the one
//! that resolves a click stop being the same one — i.e. you press one and the
//! one next to it opens. Same rule as the plugin panel (ADR 0077), and here
//! with more reason: what is on the other side of a click is a file.
//!
//! # The map does NOT follow the cursor
//! Its signature is the DIRECTORY, not the row. That is why it is in
//! `NO_SIGUEN`, and why moving the cursor does not re-measure: probing per
//! cursor would turn going down a `$HOME` into a storm of minutes-long
//! measurements.

use std::sync::Arc;

use norte_frontend::layout::SlotId;
use norte_proto::VPath;
use tokio::sync::mpsc;

use super::{Message, RequestToken, State, kind_de};
use crate::backend::HostBackend;
use crate::bridge::{BridgeEnvelope, clamp_display};
use crate::dto::UiUpdate;

/// The kind that occupies a disk-map slot.
pub(super) const KIND: &str = "disk-map";

/// What a map slot has NOW and what it is requesting.
#[derive(Default)]
pub(super) struct StateMap {
    /// What was measured, with its directory and its selection. The SHARED
    /// state.
    pub(super) map: norte_frontend::diskmap::DiskMap,
    /// The directory of the last measurement requested — or attempted and
    /// failed.
    ///
    /// Both things in one field because they answer the same question: does
    /// this need requesting? Without noting the failed attempt, a directory
    /// that cannot be measured would be retried after every actor message.
    pub(super) requested: Option<VPath>,
    /// The request in flight, with its token.
    pub(super) in_flight: Option<(RequestToken, VPath)>,
    /// How to stop the measurement in flight: another folder or a closed
    /// panel cancels it instead of waiting minutes for an answer nobody
    /// will look at (2026-10-08).
    pub(super) stop: Option<Arc<Stop>>,
}

/// The cancel handle of a measurement, which only exists once the daemon
/// launched it. A stop requested BEFORE that is remembered, and the task
/// is cancelled the moment it appears.
#[derive(Default)]
pub(super) struct Stop {
    cancel: std::sync::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    stopped: std::sync::atomic::AtomicBool,
}

impl Stop {
    /// Cancels the measurement, now or as soon as it is launched.
    pub(super) fn stop(&self) {
        self.stopped
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if let Ok(guard) = self.cancel.lock()
            && let Some(cancel) = guard.as_ref()
        {
            cancel();
        }
    }

    /// The launched task's cancel; cancels at once if a stop came first.
    fn launched(&self, cancel: &Arc<dyn Fn() + Send + Sync>) {
        if let Ok(mut guard) = self.cancel.lock() {
            *guard = Some(Arc::clone(cancel));
        }
        if self.stopped.load(std::sync::atomic::Ordering::SeqCst) {
            cancel();
        }
    }
}

/// Measures `dir` for map `slot`, sending what it counts as it goes and the
/// report at the end, both under `token`.
async fn measure(
    backend: Arc<dyn HostBackend>,
    mailbox: mpsc::Sender<Message>,
    (id, token): (u32, RequestToken),
    dir: VPath,
    stop: Arc<Stop>,
) {
    let params = norte_proto::methods::FsDirUsageParams {
        path: dir,
        // One level: that is what a map paints, and it is the only thing the
        // server serves today. Asking for more is REJECTED (ADR 0117).
        depth: 1,
    };
    // The deadline governs the LAUNCH, not the measurement: `fs.dir_usage`
    // returns the Task as soon as it is queued, and measuring a `$HOME` can
    // take minutes. A deadline on the measurement would kill it exactly on
    // the trees for which it exists.
    let launched =
        match tokio::time::timeout(super::DEADLINE_PLUGINS, backend.dir_usage(params)).await {
            Ok(r) => r,
            Err(_) => Err(norte_proto::Error::ProviderUnavailable { retryable: true }),
        };
    let task = match launched {
        Ok(t) => t,
        Err(e) => {
            let _ = mailbox
                .send(Message::MapContent(Box::new((id, token, Err(e)))))
                .await;
            return;
        }
    };
    let task_id = task.id;
    stop.launched(&task.cancel);
    let mut prog = task.progress;
    // The report is only DEFINITIVE once the Task is terminal. Requesting it
    // earlier would give half a map without saying it is half, and half a
    // map reads as a small directory.
    //
    // Meanwhile what it has COUNTED goes out, at most four times a second: a
    // measurement that only says "measuring" for minutes reads as stuck.
    let mut said = std::time::Instant::now();
    while !prog.borrow().state.is_terminal() {
        if prog.changed().await.is_err() {
            break;
        }
        if said.elapsed() >= std::time::Duration::from_millis(250) {
            said = std::time::Instant::now();
            let (entries, bytes) = {
                let p = prog.borrow();
                (p.entries_done, p.bytes_done)
            };
            let _ = mailbox
                .send(Message::MapProgress(id, token, entries, bytes))
                .await;
        }
    }
    let state_now = prog.borrow().state.clone();
    let res = if state_now.is_terminal() {
        backend
            .dir_usage_report(task_id)
            .await
            .map(|report| (state_now, report))
    } else {
        // The channel died without reaching terminal: the daemon went down.
        Err(norte_proto::Error::ProviderUnavailable { retryable: true })
    };
    let _ = mailbox
        .send(Message::MapContent(Box::new((id, token, res))))
        .await;
}

impl State {
    /// Which directory the map in slot `slot` should be describing.
    ///
    /// The link is resolved with the shared engine, same as the preview, the
    /// viewer, and the plugin panel: a followed slot that dies degrades to
    /// the `active` role. It also returns the SLOT, because the click
    /// navigates THAT listing, not the map's.
    fn followed_by_map(&self, slot: SlotId) -> Option<(u32, VPath)> {
        let mut diags = Vec::new();
        let followed =
            norte_frontend::layout::resolve_follow(&self.tree, slot, &self.roles, &mut diags)
                .or_else(|| self.roles.get(norte_frontend::layout::RoleId::Active))
                // Only a LISTING has a directory: with the keyboard in the
                // tree the active role named the tree, and the map kept the
                // previous folder (2026-10-08). Then the remembered listing.
                .filter(|SlotId(id)| self.slots.contains_key(id))
                .unwrap_or(SlotId(self.active()));
        let SlotId(id) = followed;
        let slot_state = self.slots.get(&id)?;
        Some((id, slot_state.pane.dir().clone()))
    }

    /// `dir` was read again: the maps that measured it forget they did, so
    /// the next `probe_maps` measures it anew. One still measuring keeps
    /// going, and is measured once more when it lands (`land_map`): its
    /// numbers are from before the change.
    pub(super) fn maps_measured_before(&mut self, dir: &VPath) {
        for state in self.maps.values_mut() {
            if state.in_flight.as_ref().is_some_and(|(_, d)| d == dir) {
                state.map.changed_meanwhile();
            } else if state.in_flight.is_none() && state.requested.as_ref() == Some(dir) {
                state.requested = None;
            }
        }
    }

    /// Requests the measurement for placed maps whose directory changed.
    ///
    /// It is called after EVERY actor message, like its neighbours, so the
    /// first thing is to bail out cheaply when there is nothing to do:
    /// walking the tree to discover there is no map at all is paid on every
    /// keystroke of every session that does not use it.
    pub(super) fn probe_maps(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let slots: Vec<SlotId> = self
            .split
            .placements
            .iter()
            .filter(|(slot, _)| kind_de(&self.tree, *slot).is_some_and(|k| k.as_str() == KIND))
            .map(|(slot, _)| *slot)
            .collect();
        if slots.is_empty() && self.maps.is_empty() {
            return Vec::new();
        }
        // A slot that no longer exists keeps nothing: a `SlotId` gets reused,
        // and without pruning, a new slot's map would inherit the previous
        // one's measurement — another directory's sizes, under this title.
        let alive: Vec<u32> = slots.iter().map(|SlotId(id)| *id).collect();
        // And a CLOSED map stops measuring: minutes of walking a tree for
        // a panel that is gone (2026-10-08).
        self.maps.retain(|id, state| {
            let keep = alive.contains(id);
            if !keep && let Some(stop) = state.stop.take() {
                stop.stop();
            }
            keep
        });

        // The maps aimed now: each one is PUBLISHED with its directory and
        // "measuring". Without that the window kept the view it opened with
        // — no title, nothing — for the minutes a `$HOME` takes to measure
        // (2026-10-08).
        let mut aimed: Vec<u32> = Vec::new();
        for slot in slots {
            let SlotId(id) = slot;
            let Some((_, dir)) = self.followed_by_map(slot) else {
                continue;
            };
            let state = self.maps.entry(id).or_default();
            // Measuring ANOTHER folder: that measurement is cancelled and
            // this one starts now. It waited for the old one to finish — a
            // `$HOME` takes minutes — while the map said "measuring" under
            // the new title (2026-10-08).
            if let Some((_, measuring)) = state.in_flight.as_ref()
                && *measuring != dir
            {
                if let Some(stop) = state.stop.take() {
                    stop.stop();
                }
                state.in_flight = None;
            }
            if state.requested.as_ref() == Some(&dir) || state.in_flight.is_some() {
                continue;
            }
            // Pointing at it FORGETS what was measured: the previous
            // directory's map under the new one's title is the wrong answer
            // for exactly the while the measurement lasts, which is when
            // someone is looking at it.
            if state.map.dir() != Some(&dir) {
                state.map.aim(dir.clone());
            }
            self.token += 1;
            let token = RequestToken(self.token);
            let stop = Arc::new(Stop::default());
            let state = self.maps.entry(id).or_default();
            state.in_flight = Some((token, dir.clone()));
            state.stop = Some(Arc::clone(&stop));
            aimed.push(id);
            tokio::spawn(measure(
                Arc::clone(backend),
                mailbox.clone(),
                (id, token),
                dir,
                stop,
            ));
        }
        if aimed.is_empty() {
            return Vec::new();
        }
        let changes = aimed
            .into_iter()
            .map(|id| crate::dto::ViewChange::Slot {
                slot: Box::new(crate::dto::SlotView::DiskMap(Box::new(self.map_view(id)))),
            })
            .collect();
        vec![self.parche(changes)]
    }

    /// What a running measurement has counted so far, if it is still THAT
    /// slot's live one: the map's note, and its slot alone republished.
    pub(super) fn map_progress(
        &mut self,
        slot: u32,
        token: RequestToken,
        entries: u64,
        bytes: u64,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let state = self.maps.get_mut(&slot)?;
        if state.in_flight.as_ref().map(|(t, _)| *t) != Some(token) {
            return None;
        }
        state.map.progress(entries, bytes);
        let change = crate::dto::ViewChange::Slot {
            slot: Box::new(crate::dto::SlotView::DiskMap(Box::new(self.map_view(slot)))),
        };
        Some(self.parche(vec![change]))
    }

    /// Lands a measurement: it is shown if the token is that of THAT slot's
    /// last request, and discarded otherwise.
    ///
    /// **And the DIRECTORY is checked in addition to the token.** Measuring
    /// takes a while, and in that time the panel may be pointing elsewhere: a
    /// report landed without checking would paint one directory's sizes
    /// under another one's title.
    pub(super) fn land_map(
        &mut self,
        slot: u32,
        token: RequestToken,
        res: Result<
            (
                norte_proto::TaskState,
                norte_proto::methods::FsDirUsageReportResult,
            ),
            norte_proto::Error,
        >,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let state = self.maps.get_mut(&slot)?;
        if state.in_flight.as_ref().map(|(t, _)| *t) != Some(token) {
            return None;
        }
        let (_, dir) = state.in_flight.take()?;
        state.stop = None;
        // The attempt is recorded no matter what: without this, a directory
        // that cannot be measured would be retried after every actor
        // message.
        state.requested = Some(dir.clone());
        if state.map.dir() != Some(&dir) {
            return None; // arrived late: the panel is already somewhere else
        }
        let (task_state, report) = match res {
            Ok(pair) => pair,
            Err(e) => {
                // The reason ends up in the panel's TITLE, so it goes
                // translated to the session's language and clamped, like
                // search's.
                state
                    .map
                    .failure(clamp_display(norte_frontend::error::error_category_in(
                        self.lang, &e,
                    )));
                let snap = self.snapshot();
                return Some(self.over(UiUpdate::Snapshot(Box::new(snap))));
            }
        };
        let complete = task_state == norte_proto::TaskState::Completed;
        state.map.land(report, complete);
        // The directory changed while this was measured: what landed is
        // from before, and `probe_maps` measures it once more.
        if state.map.take_changed() {
            state.requested = None;
        }
        let snap = self.snapshot();
        Some(self.over(UiUpdate::Snapshot(Box::new(snap))))
    }

    /// A click on a rectangle: enters that child.
    ///
    /// It is resolved against the SAME layout that was painted —
    /// `map_view` uses the size inside the border and so does this — so
    /// the rectangle that is seen and the one that answers are the same one
    /// by construction.
    ///
    /// **Without `zone_can`**: that filter exists because in a plugin panel
    /// the label and the command are chosen by a third party and nothing
    /// binds them together. Here `squarify` sets them, so filtering them
    /// would be guarding against oneself.
    pub(super) fn click_on_map(
        &mut self,
        slot: u32,
        row: u16,
        col: u16,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (crate::bridge::ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // A HIDDEN slot keeps its map, so its zones would keep resolving even
        // though nobody sees them. The renderer does not paint what is
        // hidden, so a click there does not come from a person.
        if self.hidden(slot) {
            return (Self::stale(crate::StaleAction::Generation), Vec::new());
        }
        let Some((cols, rows)) = self
            .split
            .placements
            .iter()
            .find(|(SlotId(s), _)| *s == slot)
            .map(|(_, r)| (r.width.saturating_sub(2), r.height.saturating_sub(2)))
        else {
            return (self.applied(), Vec::new());
        };
        let chosen = self.maps.get(&slot).and_then(|e| {
            let frame = norte_frontend::treemap::squarify(&e.map.report().children, cols, rows);
            let arg = frame.hit_at(row, col)?.arg.clone()?;
            let seg = norte_proto::Segment::parse_wire(&arg).ok()?;
            // Only a DIRECTORY opens: the map shows both kinds, and
            // "entering" a file is not navigating.
            let child = e.map.report().children.iter().find(|c| c.name == seg)?;
            (child.kind == norte_proto::EntryKind::Dir).then_some(seg)
        });
        let Some(seg) = chosen else {
            // A cell with no rectangle, or a file: nothing happens, and it is
            // not a reader error.
            return (self.applied(), Vec::new());
        };
        // Pointing ALSO selects: keyboard and mouse leave the map in the same
        // place, which is what makes clicking and then using the arrows
        // continue from where you were.
        if let Some(e) = self.maps.get_mut(&slot) {
            e.map.choose(&seg);
        }
        let Some((target_slot, dir)) = self.followed_by_map(SlotId(slot)) else {
            return (self.applied(), Vec::new());
        };
        let target = dir.join(seg);
        // Navigate the FOLLOWED listing, not the map: the map points, and the
        // `cd` goes the same way as any other (ADR 0077). `Record` because
        // this is a move the reader asked for: it enters the trail and prunes
        // forward.
        let updates = self.navigate_slot(
            target_slot,
            &target,
            norte_frontend::nav::Trail::Record,
            backend,
            mailbox,
        );
        (self.applied(), updates)
    }

    /// Projects a slot's disk map into what the renderer paints.
    ///
    /// The frame is laid out with the size INSIDE the border, same as a
    /// plugin panel's signature: whoever describes the content does not know
    /// where its slot landed, so the one who paints does the math — and here
    /// the host paints and resolves, so the two computations are the same
    /// one.
    ///
    /// Without a placed slot there is no size, and then there is no map: an
    /// empty one is sent with its title, like a panel whose first frame has
    /// not arrived yet.
    pub(super) fn map_view(&self, id: u32) -> crate::dto::DiskMapSlotView {
        let state = self.maps.get(&id);
        let (title, title_hostile) = state.and_then(|e| e.map.dir_label()).unwrap_or_default();

        let cells = self
            .split
            .placements
            .iter()
            .find(|(SlotId(s), _)| *s == id)
            .map(|(_, r)| (r.width.saturating_sub(2), r.height.saturating_sub(2)));

        let (tiles, grid) = match (state, cells) {
            (Some(e), Some((cols, rows))) => (
                norte_frontend::treemap::tiles(&e.map.report().children, cols, rows)
                    .into_iter()
                    .map(|t| crate::dto::DiskTileView {
                        col: t.x,
                        row: t.y,
                        width: t.w,
                        height: t.h,
                        name: clamp_display(t.name),
                        hostile: t.masked,
                        size: t.size,
                        percent: t.percent,
                        class: t.class.as_str().to_owned(),
                        shade: t.shade,
                    })
                    .collect(),
                [cols, rows],
            ),
            _ => (Vec::new(), [0, 0]),
        };
        let (lines, hits) = match (state, cells) {
            (Some(e), Some((cols, rows))) => {
                let frame = norte_frontend::treemap::squarify(&e.map.report().children, cols, rows);
                let lines = frame
                    .lines
                    .iter()
                    .map(|line| line.iter().map(super::views::span_view).collect())
                    .collect();
                let hits = frame
                    .hits
                    .iter()
                    .map(|h| crate::dto::HitView {
                        row: h.row,
                        col: h.col,
                        width: h.width,
                    })
                    .collect();
                (lines, hits)
            }
            _ => (Vec::new(), Vec::new()),
        };

        crate::dto::DiskMapSlotView {
            slot_id: id,
            title: clamp_display(title),
            title_hostile,
            lines,
            hits,
            measuring: state.is_some_and(|e| e.in_flight.is_some()),
            // "measuring · 12345 items · 3.0 GiB" while it runs (bridge 103).
            activity: state
                .filter(|e| e.in_flight.is_some())
                .map(|e| e.map.activity(self.lang))
                .unwrap_or_default(),
            // In THIS session's language, like the rest of the window.
            empty: if state.is_some_and(|e| e.in_flight.is_none() && e.map.nothing_to_draw()) {
                norte_i18n::t_in(self.lang, "disk-map-empty")
            } else {
                String::new()
            },
            tiles,
            grid,
        }
    }
}
