//! The window's menu.
//!
//! Part of `controller`: these are methods of `State`, moved here without
//! touching them (ADR 0086). The only writer is still the actor.

// These modules are the same `impl State` split into pieces, so they use
// the same imports as the parent. Listing them here would be a forty-line
// list per file, across 32 files, that goes out of sync the moment the
// parent imports something — `super::*` keeps it in sync on its own.
#[allow(clippy::wildcard_imports)]
use super::*;

impl State {
    /// Closes the open menu, noting where it was.
    ///
    /// ONE door: the menu closes from four places — the key, `Escape`,
    /// choosing an entry, and clicking outside — and whichever one forgot to
    /// note it would be the one that makes the next opening start from the
    /// first item for no apparent reason.
    pub(super) fn olvidar_menu(&mut self) {
        if let Some(m) = &self.menu {
            self.menu_last = m.menu();
        }
        self.menu = None;
    }

    /// Opens the menu bar wherever it was, or closes it if it was already
    /// open.
    ///
    /// The same key opens and closes, as in the TUI: `alt+m` is "the menu",
    /// and pressing it twice cannot leave two dropdowns open nor require
    /// `Esc`.
    pub(super) fn open_menu(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.menu.is_some() {
            self.olvidar_menu();
        } else {
            self.menu = Some(norte_frontend::menu::MenuState::reopen_at(self.menu_last));
        }
        let change = ViewChange::Menu {
            menu: self.vista_menu(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// Answers with the WHOLE screen.
    ///
    /// Two actions trigger it — a `Resync` and a resize — and both for the
    /// same reason: what changes does not fit in a patch, because everything
    /// changes. A single copy so the two do not diverge.
    pub(super) fn responds_with_snapshot(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let snap = self.snapshot();
        (
            self.applied(),
            vec![self.over(UiUpdate::Snapshot(Box::new(snap)))],
        )
    }

    /// A click on a bar title: opens that menu, or collapses the one that
    /// was open if it was the same one.
    ///
    /// An index outside the bar is rejected as stale and closes nothing: it
    /// is a race with an earlier catalogue, not an order.
    pub(super) fn expand_menu(&mut self, menu: u32) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let i = menu as usize;
        if i >= norte_frontend::menu::MENUS.len() {
            return (Self::stale(StaleAction::Generation), Vec::new());
        }
        let same = self.menu.as_ref().is_some_and(|m| m.menu() == i);
        if same {
            self.olvidar_menu();
        } else {
            self.menu = Some(norte_frontend::menu::MenuState::reopen_at(i));
        }
        let change = ViewChange::Menu {
            menu: self.vista_menu(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// The mouse hovering over an entry: moves the cursor and nothing else.
    pub(super) fn apuntar_en_menu(
        &mut self,
        row: u32,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(m) = self.menu.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        m.point_at(row as usize);
        let change = ViewChange::Menu {
            menu: self.vista_menu(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// A click on an entry: runs it.
    ///
    /// It is resolved against the menu the HOST has open, not against
    /// whatever the renderer says: a row that no longer exists — the menu
    /// changed between painting and the click — runs nothing.
    pub(super) fn activate_from_menu(
        &mut self,
        row: u32,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(m) = self.menu.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        m.point_at(row as usize);
        if m.item() != row as usize {
            return (Self::stale(StaleAction::Generation), Vec::new());
        }
        let chosen = m.selected();
        self.run_from_menu(chosen, backend, mailbox)
    }

    /// A click on the panel bar (#324): the `button` on the bar that this
    /// host sent, through the SAME dispatch as its shortcut. Two paths to
    /// open the same panel diverge the moment one of them grows a detail —
    /// the lesson from ADR 0077 applied inside a single frontend, same as in
    /// the TUI.
    pub(super) fn click_pane_bar(
        &mut self,
        button: u32,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let buttons = self.pane_buttons();
        let Some(button_def) = buttons.get(button as usize) else {
            // The bar the renderer painted is no longer this one: a plugin
            // contributed a kind, or was removed. Let it request a snapshot.
            return (Self::stale(StaleAction::Generation), Vec::new());
        };
        let command = button_def.command.clone();
        match crate::commands::effect_of(&command, 1) {
            Some(effect) => self.apply_effect(effect, backend, mailbox),
            // A contributed kind whose `layout.<kind>` is not in the
            // catalogue: the button exists to SHOW the panel, and saying it
            // cannot be opened from here is better than a mute click.
            None => self.no_implementado(&command),
        }
    }

    /// A click on a status bar item (ADR 0132): its command, through the
    /// same dispatch as its shortcut and as the panel bar.
    ///
    /// It is looked up by id in the CURRENT list: an item that is no longer
    /// there (the tasks finished, the list changed) is a normal race, and the
    /// renderer requests a snapshot.
    pub(super) fn click_status_item(
        &mut self,
        id: &str,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(command) = self
            .state_items()
            .into_iter()
            .find(|v| v.id == id)
            .and_then(|v| v.command)
        else {
            return (Self::stale(StaleAction::Generation), Vec::new());
        };
        match crate::commands::effect_of(command, 1) {
            Some(effect) => self.apply_effect(effect, backend, mailbox),
            None => self.no_implementado(command),
        }
    }

    /// A click on a layout button (ADR 0133): its order, through its
    /// shortcut's dispatch. An id not in the shared table is a renderer from
    /// another version: let it request a snapshot.
    pub(super) fn click_layout_button(
        &mut self,
        id: &str,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(button_def) = norte_frontend::layoutbar::by_id(id) else {
            return (Self::stale(StaleAction::Generation), Vec::new());
        };
        match crate::commands::effect_of(button_def.command, 1) {
            Some(effect) => self.apply_effect(effect, backend, mailbox),
            None => self.no_implementado(button_def.command),
        }
    }

    /// A tab bar button (ADR 0133): first it selects the tab — the clicked
    /// group gets focus — and then it runs the order through its shortcut's
    /// dispatch. A tab that is no longer there is a normal race.
    pub(super) fn tab_button(
        &mut self,
        slot_id: u32,
        verb: crate::action::TabVerb,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let (ack, mut outputs) = self.choose_tab(slot_id, backend, mailbox);
        if matches!(ack, ActionAck::Stale { .. }) {
            return (ack, outputs);
        }
        let command = match verb {
            crate::action::TabVerb::New => "pane.tab-new",
            crate::action::TabVerb::Close => "pane.tab-close",
        };
        let (ack, more) = match crate::commands::effect_of(command, 1) {
            Some(effect) => self.apply_effect(effect, backend, mailbox),
            None => self.no_implementado(command),
        };
        outputs.extend(more);
        (ack, outputs)
    }

    /// A click OUTSIDE the dropdown closes it without running anything.
    pub(super) fn close_menu(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.menu.is_none() {
            return (self.applied(), Vec::new());
        }
        self.olvidar_menu();
        let change = ViewChange::Menu {
            menu: self.vista_menu(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// Alt pressed and released alone (bridge 68): collapses the open menu or
    /// opens it as `app.menu`.
    ///
    /// With a screen that keeps keys to itself it does nothing, which is what
    /// the `app.menu` key would do there anyway: the dialog or the help
    /// screen eats it. Opening the menu on top of a pending question would
    /// leave two surfaces fighting over the keyboard.
    pub(super) fn toggle_menu(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.menu.is_some() {
            return self.close_menu();
        }
        if self.something_keeps_the_keys() {
            return (self.applied(), Vec::new());
        }
        self.open_menu()
    }
}
