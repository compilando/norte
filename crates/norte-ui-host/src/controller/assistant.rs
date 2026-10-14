//! The assistant panel in the window (ADR 0186): the same conversation model
//! as the terminal (`norte_frontend::assistant`), a turn asked of the backend
//! in the background, and Apply through the order line's executor.
//!
//! Part of `controller`: these are methods of `State`.

#[allow(clippy::wildcard_imports)]
use super::*;

use norte_frontend::assistant::Turn;

/// The panel's kind id.
pub(super) const KIND: &str = "assistant";

impl State {
    /// The panel's projection.
    pub(super) fn assistant_view(&self, id: u32) -> crate::dto::AssistantSlotView {
        let here = self.slot().pane.dir().clone();
        let turns = self
            .assistant
            .turns()
            .iter()
            .map(|t| {
                let (kind, text, note, written_for) = match t {
                    Turn::Reader(s) => ("reader", s.clone(), String::new(), String::new()),
                    Turn::Order {
                        text,
                        note,
                        written_for,
                    } => {
                        let w = if *written_for == here {
                            String::new()
                        } else {
                            let (dir, _) = norte_frontend::path_display(written_for);
                            norte_i18n::ta_in(self.lang, "assistant-written-for", &[("dir", &dir)])
                        };
                        ("order", text.clone(), note.clone(), w)
                    }
                    Turn::Answer(s) => ("answer", s.clone(), String::new(), String::new()),
                    Turn::Failed(s) => ("failed", s.clone(), String::new(), String::new()),
                };
                crate::dto::AssistantTurnView {
                    kind: kind.to_owned(),
                    text: clamp_display(norte_encoding::mask_terminal_hazards(&text)),
                    note: clamp_display(norte_encoding::mask_terminal_hazards(&note)),
                    written_for: clamp_display(written_for),
                }
            })
            .collect();
        crate::dto::AssistantSlotView {
            slot_id: id,
            turns,
            pending: self.assistant.pending(),
            empty: norte_i18n::t_in(self.lang, "assistant-empty"),
        }
    }

    /// The panel changed: its slot travels alone.
    pub(super) fn repaint_assistant(&mut self) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Some(id) = self.slot_of_kind(KIND) else {
            return Vec::new();
        };
        let view = self.assistant_view(id.0);
        vec![self.parche(vec![ViewChange::Slot {
            slot: Box::new(crate::dto::SlotView::Assistant(Box::new(view))),
        }])]
    }

    /// Sends what was typed, if nothing is in flight.
    pub(super) fn assistant_submit(
        &mut self,
        text: &str,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let here = self.slot().pane.dir().clone();
        let other = self.directory_dest().ok();
        // `ask`, not the input buffer: the window's field is the renderer's,
        // so parked text would be invisible and glued onto the next message.
        let Some(params) = self.assistant.ask(text, &here, other.as_ref()) else {
            return self.say("assistant-busy");
        };
        let backend = Arc::clone(backend);
        let mailbox = mailbox.clone();
        tokio::spawn(async move {
            let res = backend.ai_chat(params).await;
            let _ = mailbox
                .send(Message::Background(Box::new(Background::AssistantTurn(
                    res,
                ))))
                .await;
        });
        self.repaint_assistant()
    }

    /// The turn arrived (or why not). The conversation outlives the panel,
    /// so a turn that lands with it closed is kept for when it reopens.
    pub(super) fn assistant_landed(
        &mut self,
        res: Result<norte_proto::methods::AiChatTurn, Error>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let lang = self.lang;
        self.assistant
            .land(res.map_err(|e| norte_frontend::assistant::failure_text(&e, lang)));
        self.repaint_assistant()
    }

    /// The panel's actions. Each names its slot: one that is not the panel's
    /// is a race with an earlier layout.
    pub(super) fn assistant_action(
        &mut self,
        action: &UiAction,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let slot_id = match action {
            UiAction::AssistantSubmit { slot_id, .. }
            | UiAction::AssistantApply { slot_id, .. } => *slot_id,
            _ => return (Self::stale(StaleAction::Modal), Vec::new()),
        };
        if self.slot_of_kind(KIND).is_none_or(|s| s.0 != slot_id) {
            return (Self::stale(StaleAction::Generation), Vec::new());
        }
        match action {
            UiAction::AssistantSubmit { text, .. } => (
                self.applied(),
                self.assistant_submit(text, backend, mailbox),
            ),
            UiAction::AssistantApply { turn, .. } => {
                let order = self
                    .assistant
                    .order_at(usize::try_from(*turn).unwrap_or(usize::MAX))
                    .and_then(|t| norte_orders::parse(t).ok());
                match order {
                    // The order line's executor: preview, read-only refusal
                    // and the usual confirmation, exactly as if typed.
                    Some(order) => self.run_order(&order, false, backend, mailbox),
                    None => (Self::stale(StaleAction::Generation), Vec::new()),
                }
            }
            _ => (Self::stale(StaleAction::Modal), Vec::new()),
        }
    }

    /// `@` in the box: the panel opens (if it was not) and the text goes as
    /// the reader's first message.
    pub(super) fn ask_assistant(
        &mut self,
        text: &str,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let (ack, mut out) = self.show_assistant(backend, mailbox);
        out.append(&mut self.assistant_submit(text, backend, mailbox));
        (ack, out)
    }

    /// Opens the panel if it is not there, brings it to the front if it is
    /// behind another tab, and gives it the keyboard — what the terminal's
    /// `open_assistant` does.
    pub(super) fn show_assistant(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let (ack, mut out) = match self.slot_of_kind(KIND) {
            None => self.open_slot_of_kind(KIND, backend, mailbox),
            Some(id)
                if self
                    .tree
                    .tabs_of(id)
                    .is_some_and(|(t, a)| t.get(a) != Some(&id)) =>
            {
                self.choose_tab(id.0, backend, mailbox)
            }
            Some(_) => (self.applied(), Vec::new()),
        };
        if let Some(id) = self.slot_of_kind(KIND) {
            self.roles.set(RoleId::Active, id);
            self.reconciles_roles();
            out.push(self.parche(vec![ViewChange::Layout(self.layout())]));
            // Painted after the layout, so the renderer focuses its input.
            out.extend(self.repaint_assistant());
        }
        (ack, out)
    }
}
