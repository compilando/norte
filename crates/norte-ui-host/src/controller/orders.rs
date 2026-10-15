//! The `:` box in the window (ADR 0185): the live `sel`, the marks coming
//! back on Esc, and an order going through the doors F5, F6, F7 and F8
//! already use — the same steps as the terminal, from
//! `norte_frontend::orders`.
//!
//! Part of `controller`: these are methods of `State`.

#[allow(clippy::wildcard_imports)]
use super::*;

use norte_frontend::goto::Mode;
use norte_frontend::orders::{self, Context, Step};

/// Where an order that went to measure was asked from.
#[derive(Debug)]
pub(super) struct OrderTicket {
    /// The box's generation (`gen_ir_a`) when it ran.
    generation: u64,
    /// The active slot.
    slot: u32,
    /// Its folder.
    dir: VPath,
    /// The marks from before the box, to start the selection from.
    before: Vec<VPath>,
}

/// An order's measurements landing: where it was asked, the order, and
/// what each stat said.
pub(super) type OrderMeasured = (
    OrderTicket,
    norte_orders::Order,
    Vec<(VPath, Option<u64>, Option<i64>)>,
);

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

impl State {
    /// After every edit of the box. In `:` mode, previews the line's `sel`
    /// over the active slot from the marks it had when `:` was typed;
    /// leaving `:` mode puts them back. Returns the rows' patch when the
    /// marks may have moved.
    pub(super) fn order_after_edit(&mut self) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Some(g) = self.ir_a.as_ref() else {
            return Vec::new();
        };
        let in_order = g.mode() == Mode::Order;
        if !in_order && self.order_marks_before.is_none() {
            return Vec::new();
        }
        let query = g.query().to_owned();
        let before = match self.order_marks_before.take() {
            Some(b) => b,
            None => orders::marks_before(&self.slot().pane),
        };
        let lang = self.lang;
        self.order_preview =
            orders::preview(&mut self.slot_mut().pane, &query, &before, now_ms(), lang);
        if in_order {
            self.order_marks_before = Some(before);
        } else {
            self.order_preview.clear();
        }
        vec![self.parche_rows()]
    }

    /// The box closed without running: the marks from before come back.
    pub(super) fn order_cancel(&mut self) -> Vec<BridgeEnvelope<UiUpdate>> {
        self.order_preview.clear();
        let Some(before) = self.order_marks_before.take() else {
            return Vec::new();
        };
        orders::restore(&mut self.slot_mut().pane, &before);
        vec![self.parche_rows()]
    }

    /// Runs an order: the selection here, the last stage through the door
    /// its key uses.
    pub(super) fn run_order(
        &mut self,
        order: &norte_orders::Order,
        measured: bool,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // The marks from before the box: every path that does not act gives
        // them back, and the selection starts from them, so `mark.restore`
        // afterwards returns to them too.
        let before = match self.order_marks_before.take() {
            Some(b) => b,
            None => orders::marks_before(&self.slot().pane),
        };
        self.order_preview.clear();
        orders::restore(&mut self.slot_mut().pane, &before);
        let mut out = vec![self.parche_rows()];
        let here = self.slot().pane.dir().clone();
        let other = self.directory_dest().ok();
        let cx = Context {
            here: &here,
            other: other.as_ref(),
            listing: self.slot().pane.real_entries(),
            enc: self.slot().pane.name_encoding(),
        };
        let steps = match orders::steps(order, &cx) {
            Ok(s) => s,
            Err(e) => {
                let text = orders::order_error_text(&e, self.lang);
                out.extend(self.say_text(text));
                return (self.applied(), out);
            }
        };
        if self.refused_in_read_only(steps.last()) {
            return Self::no_mutates();
        }
        // The lazy listing (#52) measured only the painted rows: what a `sel`
        // needs and is missing is statted first, and the order runs again
        // when the answers are in (`order_measured`).
        let missing: Vec<VPath> = steps
            .iter()
            .filter_map(|s| match s {
                Step::Select(q) => Some(orders::unmeasured(&self.slot().pane, q)),
                _ => None,
            })
            .flatten()
            .collect();
        if !missing.is_empty() && !measured {
            let ticket = OrderTicket {
                generation: self.gen_ir_a,
                slot: self.active(),
                dir: here,
                before,
            };
            Self::measure_then_run(ticket, order.clone(), missing, backend, mailbox);
            out.extend(self.say("msg-order-measuring"));
            return (self.applied(), out);
        }
        let mut matched = None;
        for step in &steps {
            if let Step::Select(q) = step {
                let Ok(n) = self.slot_mut().pane.select_query(q, now_ms()) else {
                    orders::restore(&mut self.slot_mut().pane, &before);
                    out.push(self.parche_rows());
                    out.extend(self.say("err-bad-pattern"));
                    return (self.applied(), out);
                };
                matched = Some(n);
                out.push(self.parche_rows());
            }
        }
        let Some(last) = orders::runnable(&steps, matched).last() else {
            orders::restore(&mut self.slot_mut().pane, &before);
            out.push(self.parche_rows());
            out.extend(self.say("msg-order-nothing-matched"));
            return (self.applied(), out);
        };
        let (ack, mut rest) = match last {
            Step::Select(_) => (self.applied(), Vec::new()),
            Step::Copy { to } => self.confirm_transfer(to, false, backend, mailbox),
            Step::Move { to } => self.confirm_transfer(to, true, backend, mailbox),
            Step::Trash => self.request_deleted(false),
            // Resolved against `here`, which in a results tab is only the
            // search's root (C6): refused like F7 there.
            Step::Mkdir { .. } if self.slot().pane.is_results() => {
                let key = "host-results-not-a-location";
                (
                    ActionAck::Unavailable {
                        reason_key: key.to_owned(),
                    },
                    self.say(key),
                )
            }
            Step::Mkdir { path } => {
                Self::order_mkdir(path, backend, mailbox);
                (self.applied(), Vec::new())
            }
            Step::Cd { to } => (
                self.applied(),
                self.navigate(to, Trail::Record, backend, mailbox),
            ),
            Step::Command(id) => self.run_command_key(id, backend, mailbox),
            // The timeline panel IS the log.
            Step::Log(_) => self.run_command_key("layout.timeline", backend, mailbox),
            Step::Undo(n) => {
                Self::order_undo(self.gen_ir_a, *n, backend, mailbox);
                (self.applied(), Vec::new())
            }
        };
        out.append(&mut rest);
        (ack, out)
    }

    /// Whether a read-only window must refuse the order's acting stage. The
    /// keymap, palette and menus filter by the same list; an order must not
    /// be the way around them.
    fn refused_in_read_only(&self, last: Option<&Step>) -> bool {
        if self.effects != crate::commands::Effects::SoloRead {
            return false;
        }
        match last {
            Some(Step::Copy { .. } | Step::Move { .. } | Step::Trash | Step::Mkdir { .. }) => true,
            Some(Step::Command(id)) => {
                !crate::commands::all_with(self.effects).contains(&id.as_str())
            }
            _ => false,
        }
    }

    /// Stats the rows a `sel` needs, then sends the order back. The answer
    /// travels WITH where it was asked (`ticket`): if the slot, its folder or
    /// the box's generation changed by then, [`Self::order_measured`] drops
    /// it instead of running it somewhere the reader never previewed.
    fn measure_then_run(
        ticket: OrderTicket,
        order: norte_orders::Order,
        missing: Vec<VPath>,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) {
        let backend = Arc::clone(backend);
        let mailbox = mailbox.clone();
        tokio::spawn(async move {
            let mut stats = Vec::with_capacity(missing.len());
            for path in missing {
                if let Ok(st) = backend.stat(path.clone(), Vec::new()).await {
                    stats.push((path, st.size, st.mtime_ms));
                }
            }
            let _ = mailbox
                .send(Message::Background(Box::new(Background::OrderMeasured(
                    Box::new((ticket, order, stats)),
                ))))
                .await;
        });
    }

    /// The stats an order waited for: into the listing, then the order runs
    /// — once; a row whose stat failed stays unknown and does not match.
    pub(super) fn order_measured(
        &mut self,
        landed: OrderMeasured,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let (ticket, order, stats) = landed;
        let moved = ticket.generation != self.gen_ir_a
            || self.ir_a.is_some()
            || ticket.slot != self.active()
            || self.slot().pane.dir() != &ticket.dir;
        if moved {
            return self.say("msg-order-stale");
        }
        for (path, size, mtime) in stats {
            self.slot_mut().pane.hydrate(&path, size, mtime);
        }
        self.order_marks_before = Some(ticket.before);
        let (ack, mut out) = self.run_order(&order, true, backend, mailbox);
        // A refusal on the second pass is said, not dropped with the ack.
        if let ActionAck::Unavailable { reason_key } = ack {
            out.extend(self.say(&reason_key));
        }
        out
    }

    /// `mkdir` from an order: the path is already resolved, so it goes
    /// straight to the task F7's dialog would start.
    fn order_mkdir(path: &VPath, backend: &Arc<dyn HostBackend>, mailbox: &mpsc::Sender<Message>) {
        let dir = path.parent().unwrap_or_else(|| path.clone());
        let backend = Arc::clone(backend);
        let mailbox = mailbox.clone();
        let path = path.clone();
        tokio::spawn(async move {
            let message = match backend.mkdir(path).await {
                Ok(task) => Message::TaskNew(Box::new((task, vec![dir], None))),
                Err(e) => Message::TaskFailed(Box::new(e)),
            };
            let _ = mailbox.send(message).await;
        });
    }

    /// `undo N`: asks for the reader's newest page; the answer opens the
    /// timeline's own confirmation ([`Self::order_undo_landed`]).
    fn order_undo(
        generation: u64,
        n: u32,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) {
        let backend = Arc::clone(backend);
        let mailbox = mailbox.clone();
        tokio::spawn(async move {
            let res = backend
                .journal_list_mine(norte_proto::methods::JOURNAL_LIST_MAX_PAGE)
                .await;
            let _ = mailbox
                .send(Message::Background(Box::new(Background::OrderUndo(
                    generation, n, res,
                ))))
                .await;
        });
    }

    /// The page for `undo N` arrived: cut it and ask, with the ceiling frozen
    /// at the newest row counted — unless the reader opened the box again
    /// meanwhile, which a late dialog would land on.
    pub(super) fn order_undo_landed(
        &mut self,
        generation: u64,
        n: u32,
        res: Result<norte_proto::methods::JournalListResult, Error>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        if generation != self.gen_ir_a || self.ir_a.is_some() {
            return self.say("msg-order-stale");
        }
        let page = match res {
            Ok(p) => p,
            Err(e) => {
                let text = norte_i18n::ta_in(
                    self.lang,
                    "msg-order-journal-failed",
                    &[("err", &e.to_string())],
                );
                return self.say_text(clamp_display(text));
            }
        };
        match orders::undo_cut(&page.rows, page.next_before_seq, n) {
            Ok(cut) => self.ask_undo_cut(cut.seq, cut.summary, cut.techo).1,
            Err(orders::UndoRefusal::Nothing) => self.say("timeline-undo-nothing"),
            Err(orders::UndoRefusal::TooFar) => self.say("msg-order-undo-too-far"),
        }
    }
}
