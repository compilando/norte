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
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // What the preview marked stays marked: it is what the reader saw.
        self.order_marks_before = None;
        self.order_preview.clear();
        let here = self.slot().pane.dir().clone();
        let other = self.directory_dest().ok();
        let cx = Context {
            here: &here,
            other: other.as_ref(),
        };
        let steps = match orders::steps(order, &cx) {
            Ok(s) => s,
            Err(e) => {
                let text = orders::order_error_text(&e, self.lang);
                return (self.applied(), self.say_text(text));
            }
        };
        let mut matched = None;
        let mut out = Vec::new();
        for step in &steps {
            if let Step::Select(q) = step {
                match self.slot_mut().pane.select_query(q, now_ms()) {
                    Ok(n) => matched = Some(n),
                    Err(_) => return (self.applied(), self.say("err-bad-pattern")),
                }
                out.push(self.parche_rows());
            }
        }
        let Some(last) = orders::runnable(&steps, matched).last() else {
            out.extend(self.say("msg-order-nothing-matched"));
            return (self.applied(), out);
        };
        let (ack, mut rest) = match last {
            Step::Select(_) => (self.applied(), Vec::new()),
            Step::Copy { to } => self.confirm_transfer(to, false, backend, mailbox),
            Step::Move { to } => self.confirm_transfer(to, true, backend, mailbox),
            Step::Trash => self.request_deleted(false),
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
                Self::order_undo(*n, backend, mailbox);
                (self.applied(), Vec::new())
            }
        };
        out.append(&mut rest);
        (ack, out)
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
    fn order_undo(n: u32, backend: &Arc<dyn HostBackend>, mailbox: &mpsc::Sender<Message>) {
        let backend = Arc::clone(backend);
        let mailbox = mailbox.clone();
        tokio::spawn(async move {
            let res = backend
                .journal_list_mine(norte_proto::methods::JOURNAL_LIST_MAX_PAGE)
                .await;
            let _ = mailbox
                .send(Message::Background(Box::new(Background::OrderUndo(n, res))))
                .await;
        });
    }

    /// The page for `undo N` arrived: cut it and ask, with the ceiling frozen
    /// at the newest row counted.
    pub(super) fn order_undo_landed(
        &mut self,
        n: u32,
        res: Result<norte_proto::methods::JournalListResult, Error>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Ok(page) = res else {
            return self.say("timeline-undo-nothing");
        };
        match orders::undo_cut(&page.rows, page.next_before_seq, n) {
            Ok(cut) => self.ask_undo_cut(cut.seq, cut.summary, cut.techo).1,
            Err(orders::UndoRefusal::Nothing) => self.say("timeline-undo-nothing"),
            Err(orders::UndoRefusal::TooFar) => self.say("msg-order-undo-too-far"),
        }
    }
}
