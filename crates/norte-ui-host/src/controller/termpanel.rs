//! The terminal panel in the window (#362, bridge 95).
//!
//! Two halves. The SHELL and the grid belong to `norte-term`, the same ones
//! the terminal uses: the same pty, the same reader thread and the same
//! emulation, so the two frontends show the same thing by construction and
//! not because someone compares two emulators. And the TRANSLATION of that
//! grid into what crosses the bridge, which is the only thing of its own
//! here.
//!
//! # What is NOT done in the translation, and it is the decision
//!
//! **An indexed color is not resolved.** The shell says "color 4"; which blue
//! that is, is decided by the palette of whoever paints it. If it were
//! resolved here to an `#rrggbb`, the panel would stop obeying the reader's
//! theme and there would be no way to fix it from the theme. That is why
//! [`TerminalColorView`] keeps the two cases distinct.
//!
//! **Nothing is masked.** What comes out of the grid can no longer carry a
//! control byte: the parser eats the escapes and drops the C0 codes that do
//! not move the cursor. Masking of hostile names exists because a name
//! arrives raw; this does not arrive raw, it arrives parsed.

use std::sync::Arc;

use norte_frontend::shell_profiles::ShellProfile;
use norte_frontend::terminals::{AnsiColor, InstanceId, TerminalIcon, TerminalShell, Terminals};
use norte_term::{ColorTerm, Screen, Style};
use tokio::sync::mpsc;

use crate::backend::HostBackend;
use crate::bridge::BridgeEnvelope;
use crate::dto::{
    TerminalColorView, TerminalInstanceView, TerminalSlotView, TerminalSpanView, UiUpdate,
    ViewChange,
};

use super::{ActionAck, Message, State};

/// `norte-term`'s shell as the shared model's [`TerminalShell`]: a newtype
/// because neither the trait nor the type is this crate's.
pub(super) struct Pty(norte_term::pty::Shell);

impl TerminalShell for Pty {
    fn pump(&mut self) -> bool {
        self.0.pump()
    }
    fn resize(&mut self, size: (u16, u16)) {
        self.0.resize(size);
    }
    fn take_title(&mut self) -> Option<String> {
        self.0.take_title()
    }
    fn exit_code(&mut self) -> Option<i32> {
        self.0.exit_code()
    }
}

/// Cells the instance list takes on the right, when it shows.
const LIST_COLS: u16 = 18;

/// The panel's instances, as `State` holds them.
pub(super) type PanelShells = Terminals<Pty>;

/// The panel's kind, which is also the suffix of its command.
pub(super) const KIND: &str = "terminal";

/// The command that opens the panel and the one that takes it out: it is the
/// SAME one.
pub(super) const COMMAND: &str = "layout.terminal";

/// The commands whose lone chord still reaches norte from inside the panel:
/// the shared list, the TUI's too (ADR 0077).
use norte_frontend::terminals::PASS_THROUGH;

impl State {
    /// Opens the terminal panel, or brings it to the front.
    ///
    /// **It never closes it**, unlike `toggle_slot`: inside there is a
    /// shell of the reader's with whatever it had half-done, and closing it
    /// kills it. Closing is `layout.close-slot`, named for what it does.
    pub(super) fn open_terminal_panel(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if let Some(id) = self.slot_of_kind(KIND) {
            // Behind another tab: it is brought to the front.
            let behind = self
                .tree
                .tabs_of(id)
                .is_some_and(|(t, a)| t.get(a) != Some(&id));
            if behind {
                // `choose_tab` also starts a missing shell: bringing it
                // forward is the gesture.
                return self.choose_tab(id.0, backend, mailbox);
            }
            // Already in front, and here is the DOOR, in both directions. The
            // key is one, so it has to take you in and bring you back:
            //
            // - with focus elsewhere, it is given to it (and without this,
            //   opening with the key never focused the panel:
            //   `open_slot_of_kind` leaves focus on the remembered listing,
            //   so it was only entered with the mouse or with the ring);
            // - with focus INSIDE, it is returned to the listing. Without
            //   this the documented exit did nothing and the panel was a
            //   mousetrap: in there every key belongs to the shell, including
            //   the ring's.
            //
            // What it does NOT do, and that is the difference with
            // `toggle_slot`: close. There is a shell of the reader's in
            // there.
            let inside = self
                .roles
                .get(norte_frontend::layout::RoleId::Active)
                .is_some_and(|a| a == id);
            //
            // Inside a panel with no LIVE shell, though, the key starts one
            // and stays: leaving a panel that says "no shell" left the reader
            // to guess that the same key, pressed again, was the way to get
            // one (2026-10-09).
            let live = self.terminals.iter().any(|i| i.exited.is_none());
            let target = if inside && live {
                norte_frontend::layout::SlotId(self.active())
            } else {
                id
            };
            self.roles
                .set(norte_frontend::layout::RoleId::Active, target);
            self.reconciles_roles();
            // And if the slot exists WITHOUT a shell, it is started here. It
            // is the case of a restored session — the tree is saved, the
            // shell is not — and of a startup that failed: without this the
            // panel stayed dead for the rest of the window's life, because
            // this branch returned before checking.
            if !live {
                self.start_si_missing(mailbox);
            }
            let snap = self.snapshot();
            let envelope = self.over(UiUpdate::Snapshot(Box::new(snap)));
            return (self.applied(), vec![envelope]);
        }
        // A shell sits in a filesystem directory: over an `sftp://` there is
        // nowhere to sit it, and opening it in `$HOME` without saying
        // anything would be opening it somewhere else. It is the same gate as
        // `app.terminal`, with the same reason and the same phrase.
        let dir = self.slot().pane.dir().clone();
        if !norte_frontend::shell::is_local(&dir) {
            let outgoing = self.say("host-not-local");
            return (
                ActionAck::Unavailable {
                    reason_key: "host-not-local".to_owned(),
                },
                outgoing,
            );
        }
        // A new slot: any shells still listed belonged to a slot that went
        // away without closing them (a layout switch), and are not this
        // panel's to show.
        bury(self.terminals.drain());
        let (ack, mut updates) = self.open_slot_of_kind(KIND, backend, mailbox);
        // Focus goes to the panel: opening it and not being able to type
        // inside without hunting for the mouse is not opening it.
        // `open_slot_of_kind` leaves it on the remembered listing, so it is
        // set here.
        if let Some(id) = self.slot_of_kind(KIND) {
            self.roles.set(norte_frontend::layout::RoleId::Active, id);
            self.reconciles_roles();
        }
        updates.extend(self.start_si_missing(mailbox));
        // The focus moved AFTER `open_slot_of_kind` sent its frame, and the
        // active role only travels in the layout: without a frame here the
        // window kept the listing painted as active while the keys went to
        // the shell. Opening is rare; the whole frame is the simple answer.
        let snap = self.snapshot();
        updates.push(self.over(UiUpdate::Snapshot(Box::new(snap))));
        (ack, updates)
    }

    /// Starts the shell if the slot exists and does not have one, and
    /// republishes.
    ///
    /// Called from both paths — opening the slot and returning to it —
    /// because the second one is the restored-session path: the tree is
    /// saved and the shell is not, so on starting the window there is a slot
    /// with no shell.
    pub(super) fn start_si_missing(
        &mut self,
        mailbox: &mpsc::Sender<Message>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // "Missing" = no LIVE shell: a panel of failed ones only would be a
        // still picture forever.
        if self.terminals.iter().any(|i| i.exited.is_none()) || self.slot_of_kind(KIND).is_none() {
            return Vec::new();
        }
        let profile = self.config.shell_profiles.default_profile().clone();
        match self.spawn_instance(&profile, mailbox) {
            Ok(()) => self.republicar_terminal(),
            Err(outgoing) => outgoing,
        }
    }

    /// `terminal.new` and the `+`: another shell, from a shell profile
    /// (`None` = the default one), in front.
    pub(super) fn new_terminal(
        &mut self,
        profile: Option<&str>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.effects == crate::commands::Effects::SoloRead {
            return Self::no_mutates();
        }
        if self.slot_of_kind(KIND).is_none() {
            return not_here();
        }
        // Same gate and phrase as opening the panel: a shell over an
        // `sftp://` would sit somewhere else without saying so.
        if !norte_frontend::shell::is_local(self.slot().pane.dir()) {
            let outgoing = self.say("host-not-local");
            return (
                ActionAck::Unavailable {
                    reason_key: "host-not-local".to_owned(),
                },
                outgoing,
            );
        }
        let profiles = &self.config.shell_profiles;
        let Some(profile) = profile
            .map_or(Some(profiles.default_profile()), |n| profiles.get(n))
            .cloned()
        else {
            // A stale menu after `terminal.toml` changed: the name is the
            // problem, not the panel.
            return rejected();
        };
        match self.spawn_instance(&profile, mailbox) {
            Ok(()) => (self.applied(), self.republicar_terminal()),
            Err(outgoing) => (
                ActionAck::Unavailable {
                    reason_key: "host-shell-failed".to_owned(),
                },
                outgoing,
            ),
        }
    }

    /// Starts `profile`'s shell in the pane's directory and puts it in
    /// front. On failure nothing is added, and the reader is TOLD: an empty
    /// panel with no reason is what makes a panel distrusted.
    fn spawn_instance(
        &mut self,
        profile: &ShellProfile,
        mailbox: &mpsc::Sender<Message>,
    ) -> Result<(), Vec<BridgeEnvelope<UiUpdate>>> {
        // The read-only rule for this panel, in ONE place: a read-only window
        // never STARTS a program — whether by `+`, by key or by opening the
        // panel. With no shell started there is nothing for close to gate.
        if self.effects == crate::commands::Effects::SoloRead {
            return Err(Self::no_mutates().1);
        }
        let dir = self.slot().pane.dir().clone();
        match start(&dir, profile) {
            Ok(shell) => {
                // Not journalled: a shell the reader opens is the reader
                // acting with their own permissions (ADR 0084). Logged
                // because starting one is the most privileged thing a
                // frontend does. The shell profile's NAME only: its args may
                // carry a token.
                tracing::info!(
                    shell_profile = %profile.name,
                    "the window opened a shell in a terminal panel \
                     (does not go to the journal: no actor and no undo)"
                );
                self.terminals.push(
                    profile.name.clone(),
                    profile.icon,
                    profile.color,
                    Pty(shell),
                );
                self.restart_terminal_tick(mailbox);
                Ok(())
            }
            Err(e) => {
                tracing::warn!(error = %e, "could not open the panel's shell");
                Err(self.say("host-shell-failed"))
            }
        }
    }

    /// `terminal.next` / `terminal.prev`.
    pub(super) fn step_terminal(
        &mut self,
        forward: bool,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.slot_of_kind(KIND).is_none() || self.terminals.is_empty() {
            return not_here();
        }
        if forward {
            self.terminals.next();
        } else {
            self.terminals.prev();
        }
        (self.applied(), self.republicar_terminal())
    }

    /// `terminal.new-profile`: the shell profiles, in a picker.
    pub(super) fn request_shell_profile(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(slot) = self.slot_of_kind(KIND) else {
            return not_here();
        };
        let names = self.panel_de_terminal(slot.0).profiles;
        self.open_terminal_picker(crate::pickers::Selector::shell_profiles(slot.0, &names))
    }

    /// `terminal.decorate`: icons and colours for the one in front.
    pub(super) fn request_terminal_decorate(
        &mut self,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(slot) = self.slot_of_kind(KIND) else {
            return not_here();
        };
        if self.terminals.active_id().is_none() {
            return not_here();
        }
        self.open_terminal_picker(crate::pickers::Selector::terminal_decorations(
            slot.0, self.lang,
        ))
    }

    fn open_terminal_picker(
        &mut self,
        s: crate::pickers::Selector,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.selector = Some(s);
        self.gen_selector += 1;
        let change = ViewChange::Picker {
            picker: self.vista_selector(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// A terminal picker's row was chosen.
    pub(super) fn apply_terminal_choice(
        &mut self,
        choice: crate::pickers::TerminalChoice,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        use crate::pickers::TerminalChoice;
        let front = self.terminals.active();
        let (icon, color) = (front.and_then(|i| i.icon), front.and_then(|i| i.color));
        let Some(id) = self.terminals.active_id() else {
            if let TerminalChoice::Profile(n) = choice {
                return self.new_terminal(Some(&n), mailbox);
            }
            return not_here();
        };
        // One attribute at a time: choosing a colour keeps the icon.
        match choice {
            TerminalChoice::Profile(n) => self.new_terminal(Some(&n), mailbox),
            TerminalChoice::Icon(i) => {
                self.terminals.decorate(id, i, color);
                (self.applied(), self.republicar_terminal())
            }
            TerminalChoice::Color(c) => {
                self.terminals.decorate(id, icon, c);
                (self.applied(), self.republicar_terminal())
            }
        }
    }

    /// `terminal.rename`: a text field prefilled with the current name.
    pub(super) fn request_terminal_rename(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(front) = self.terminals.active() else {
            return not_here();
        };
        let id = front.id.0;
        let seed = front.name.clone().unwrap_or_default();
        let modal = super::ModalId(self.next_modal);
        self.next_modal += 1;
        let vista = crate::dto::DialogView {
            id: modal,
            title_key: "terminal-rename-prompt".to_owned(),
            destination: None,
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body: Vec::new(),
            overflow_note: String::new(),
            overflow_hostile: false,
            choices: vec![
                crate::dto::DialogChoice {
                    id: "confirm".to_owned(),
                    label_key: "dialog-confirm".to_owned(),
                    destructive: false,
                },
                crate::dto::DialogChoice {
                    id: "cancel".to_owned(),
                    label_key: "dialog-cancel".to_owned(),
                    destructive: false,
                },
            ],
            input: Some(crate::bridge::clamp_display(seed.clone())),
            input_hostile: false,
            input_secret: false,
            fields: Vec::new(),
            dest_check: crate::dto::DestCheckView::NotAsked,
        };
        self.dialogs.push(super::Dialog {
            id: modal,
            vista,
            typed: super::Typed::Text(seed),
            recognized: true,
            on_confirm: Some(super::Pending::RenameTerminal { id }),
        });
        let change = ViewChange::Dialogs {
            dialogs: self.dialog_views(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// A click on an instance's entry.
    pub(super) fn select_terminal(
        &mut self,
        id: u32,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if !self.terminals.select(InstanceId(id)) {
            return not_here();
        }
        (self.applied(), self.republicar_terminal())
    }

    /// Closes an instance, killing its shell; `None` = the one in front.
    pub(super) fn close_terminal(
        &mut self,
        id: Option<u32>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(id) = id.map(InstanceId).or(self.terminals.active_id()) else {
            return not_here();
        };
        let Some(gone) = self.terminals.close(id) else {
            return not_here();
        };
        bury(vec![gone]);
        (self.applied(), self.republicar_terminal())
    }

    /// Names an instance; blank clears the name.
    pub(super) fn rename_terminal(
        &mut self,
        id: u32,
        name: &str,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let id = InstanceId(id);
        if self.terminals.display_title(id).is_none() {
            return not_here();
        }
        self.terminals.rename(id, name);
        (self.applied(), self.republicar_terminal())
    }

    /// Icon and colour, VALIDATED: the renderer's word is not trusted.
    pub(super) fn decorate_terminal(
        &mut self,
        id: u32,
        icon: Option<&str>,
        color: Option<u8>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let id = InstanceId(id);
        if self.terminals.display_title(id).is_none() {
            return not_here();
        }
        let icon = match icon.map(TerminalIcon::parse) {
            Some(None) => return rejected(),
            other => other.flatten(),
        };
        let color = match color.map(AnsiColor::new) {
            Some(None) => return rejected(),
            other => other.flatten(),
        };
        self.terminals.decorate(id, icon, color);
        (self.applied(), self.republicar_terminal())
    }

    /// Schedules the panel's next tick, if there is still a panel.
    ///
    /// It rearms only while the slot stays in the tree and turns off when it
    /// closes — same mechanism as the log panel's polling, and for the same
    /// reason: a 30 Hz timer that outlived the panel would keep waking the
    /// actor to paint nothing.
    fn probe_terminal(&self, mailbox: &mpsc::Sender<Message>) {
        if self.slot_of_kind(KIND).is_none() {
            return;
        }
        let (mailbox, epoch) = (mailbox.clone(), self.terminal_epoch);
        tokio::spawn(async move {
            tokio::time::sleep(super::TERMINAL_TIC).await;
            let _ = mailbox.send(Message::TerminalTic(epoch)).await;
        });
    }

    /// Flushes what the shell wrote and republishes if something changed.
    ///
    /// A tick with no bytes produces no patch: a quiet shell does not wake
    /// the renderer thirty times a second.
    pub(super) fn terminal_tic(
        &mut self,
        epoch: u64,
        mailbox: &mpsc::Sender<Message>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        if epoch != self.terminal_epoch {
            // From an earlier opening: let it die without rearming.
            return Vec::new();
        }
        // If the slot is no longer there, the shell goes with it and the tick
        // does not rearm.
        if self.slot_of_kind(KIND).is_none() {
            self.release_terminal();
            return Vec::new();
        }
        // The slot's size goes to EVERY shell before pumping: the pty has to
        // know it or a full-screen program paints for a width that is not its
        // own. From the LAYOUT, minus the frame the renderer paints; `resize`
        // does nothing if it did not change. Every shell, not only the one in
        // front, so switching does not reflow — and every one PUMPED, or the
        // pty's tail-only buffer corrupts the screens behind.
        let size = self.terminal_size();
        let report = self.terminals.tick(size);
        // Rearmed only while some shell is alive. With none — all gone, or
        // all exited showing their last screen — a 30 Hz timer would spin at
        // rest over a still picture; the next instance restarts it.
        if self.terminals.iter().any(|i| i.exited.is_none()) {
            self.probe_terminal(mailbox);
        } else {
            self.terminal_epoch += 1;
        }
        // A quiet tick produces no patch: a quiet shell does not wake the
        // renderer thirty times a second.
        if report.active_output || report.list_changed {
            return self.republicar_terminal();
        }
        Vec::new()
    }

    /// Starts the tick afresh: the epoch bumps so a chain already in flight
    /// dies instead of ticking twice as fast.
    fn restart_terminal_tick(&mut self, mailbox: &mpsc::Sender<Message>) {
        self.terminal_epoch += 1;
        self.probe_terminal(mailbox);
    }

    /// Releases every shell and stops the pump. Called by the slot's closing.
    pub(super) fn release_terminal(&mut self) {
        bury(self.terminals.drain());
        // And the epoch bumps: the tick in flight is left to die without
        // rearming.
        self.terminal_epoch += 1;
    }

    /// The keys when the terminal panel has focus: ALL to the shell, except
    /// the one that takes it out.
    ///
    /// `None` = this panel does not want them, and the key goes its normal
    /// way.
    ///
    /// It does not look like `key_in_preview` and it should not: that one
    /// resolves against a keymap, and here there is no keymap that counts —
    /// inside a shell, `tab`, the arrows and `ctrl+c` mean whatever the shell
    /// says. The only thing norte keeps is the lone chord that opened the
    /// panel; if the preset binds it to a sequence there is no door, and then
    /// the panel does NOT take the keys, which is better than a panel you
    /// cannot leave.
    pub(super) fn key_in_terminal(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> Option<(ActionAck, Vec<BridgeEnvelope<UiUpdate>>)> {
        let exit_chord = self.exit_chord()?;
        let focus = self.roles.get(norte_frontend::layout::RoleId::Active)?;
        if super::kind_de(&self.tree, focus).is_none_or(|k| k.as_str() != KIND) {
            return None;
        }
        let chord = k.to_chord().ok()?;
        if chord == exit_chord {
            // The SAME path that opened it: the key is one, so the way back
            // has to be the same code.
            return Some(self.open_terminal_panel(backend, mailbox));
        }
        // The panel keys are not the shell's: without this `alt+o` reached
        // the shell as `ESC o` and the ring could not leave the panel.
        if PASS_THROUGH
            .iter()
            .any(|c| self.effective.lone_chord(c).as_ref() == Some(&chord))
        {
            return None;
        }
        let bytes = norte_frontend::subshell::chord_a_bytes(chord)?;
        self.terminal_write(&bytes);
        Some((self.applied(), Vec::new()))
    }

    /// The terminal slot's size in cells, without the frame.
    ///
    /// `None` if the slot is not placed — behind a tab, or does not fit: then
    /// the pty is not touched, because the last good size is better than a
    /// made-up one.
    fn terminal_size(&self) -> Option<(u16, u16)> {
        let id = self.slot_of_kind(KIND)?;
        let (_, r) = self.split.placements.iter().find(|(s, _)| *s == id)?;
        // The list on the right is cells the shell does not get.
        let width = r.width.saturating_sub(2).saturating_sub(self.list_cols());
        Some((width, r.height.saturating_sub(2)))
    }

    /// The list's width: VS Code shows it only with two or more shells.
    fn list_cols(&self) -> u16 {
        if self.terminals.len() >= 2 {
            LIST_COLS
        } else {
            0
        }
    }

    /// The LONE chord that runs `layout.terminal`, if the preset gives one.
    ///
    /// The rule that it has to be lone lives in `Effective::lone_chord` and is
    /// shared by the two places that hand the whole keyboard over to another
    /// program: the terminal's subshell and this panel. A two-key sequence
    /// would force stealing the shell's first key exactly where the reader is
    /// typing it.
    fn exit_chord(&self) -> Option<norte_frontend::keymap::Chord> {
        self.effective.lone_chord(COMMAND)
    }

    /// Sends bytes to the shell in front, if it is alive. An exited one is a
    /// still picture: its keys go nowhere.
    pub(super) fn terminal_write(&mut self, bytes: &[u8]) {
        if let Some(i) = self.terminals.active_mut()
            && i.exited.is_none()
        {
            i.shell.0.write(bytes);
        }
    }

    /// Only the panel (#401): the shell's output touches nothing else, and
    /// the whole frame at up to 30 Hz rebuilt every slot. Without a placed
    /// slot there is nothing to send.
    fn republicar_terminal(&mut self) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Some(slot) = self.slot_of_kind(KIND) else {
            return Vec::new();
        };
        let terminal = Box::new(self.panel_de_terminal(slot.0));
        vec![self.parche(vec![ViewChange::Terminal { terminal }])]
    }

    /// The panel's view for the snapshot, if the slot exists.
    pub(super) fn panel_de_terminal(&self, slot: u32) -> TerminalSlotView {
        let front = self.terminals.active();
        let mut view = build_view(slot, front.map(|i| i.shell.0.screen()));
        if let Some(i) = front
            && i.exited.is_some()
        {
            // A still picture has no cursor to blink.
            view.cursor = None;
            view.exited = i.exited;
        }
        view.instances = self
            .terminals
            .iter()
            .map(|i| TerminalInstanceView {
                id: i.id.0,
                title: self
                    .terminals
                    .display_title(i.id)
                    .unwrap_or_default()
                    .to_owned(),
                name: i.name.clone(),
                icon: i.icon.map(|c| c.as_str().to_owned()),
                color: i.color.map(AnsiColor::index),
                exited: i.exited,
                unseen: i.unseen,
            })
            .collect();
        view.active = self.terminals.active_id().map(|id| id.0);
        view.list_cols = self.list_cols();
        let profiles = &self.config.shell_profiles;
        let default = &profiles.default_profile().name;
        view.profiles = std::iter::once(default.clone())
            .chain(
                profiles
                    .iter()
                    .filter(|p| &p.name != default)
                    .map(|p| p.name.clone()),
            )
            .collect();
        view
    }
}

/// Kills and reaps shells OFF the actor: each `Shell`'s `Drop` is a kill
/// and a blocking wait, and a shell that ignores the hang-up makes that
/// wait long — the whole window would freeze for it, once per shell.
///
/// A clean exit removed by the tick does not come through here: its child
/// was already reaped by `exit_code`, so its `Drop` does not block.
fn bury(gone: Vec<norte_frontend::terminals::Instance<Pty>>) {
    if gone.is_empty() {
        return;
    }
    tokio::task::spawn_blocking(move || drop(gone));
}

/// The "nothing to act on" refusal the instance actions share.
fn not_here() -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
    (
        ActionAck::Unavailable {
            reason_key: "cmd-not-here".to_owned(),
        },
        Vec::new(),
    )
}

/// A value from the renderer outside what the host accepts.
fn rejected() -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
    (
        ActionAck::Unavailable {
            reason_key: "host-value-rejected".to_owned(),
        },
        Vec::new(),
    )
}

/// Starts a shell profile's shell with what norte decides: the directory
/// and the environment.
///
/// These live here and not in `norte-term` because the `NORTE_LEVEL`
/// contract is norte's rule, not an emulator's.
fn start(
    dir: &norte_proto::VPath,
    profile: &ShellProfile,
) -> std::io::Result<norte_term::pty::Shell> {
    // Absolute by construction (`terminal.toml` refuses anything else), and
    // checked again here because the spawn is where a relative one would be
    // looked up from the directory being browsed (#302).
    if !profile.program.is_absolute() {
        return Err(std::io::Error::other("the shell program is not absolute"));
    }
    let native = norte_vfs::native::vpath_to_native(dir)
        .map_err(|_| std::io::Error::other("the directory is not a native path"))?;
    norte_term::pty::Shell::open(
        &norte_term::pty::Startup {
            program: &profile.program,
            args: &profile.args,
            dir: &native,
            // The real size is set by the renderer when it says which slot it
            // got; this is the startup one.
            tam: (80, 24),
            env: &[(
                norte_frontend::shell::LEVEL_VAR.into(),
                norte_frontend::shell::next_norte_level().into(),
            )],
        },
        norte_frontend::subshell::terminal_reply,
    )
}

/// A shell's grid, turned into the bridge's view.
///
/// Without a grid — there is no shell — the view SAYS so: a blank panel and a
/// panel with no shell look the same and are not the same thing.
#[must_use]
fn build_view(slot_id: u32, screen: Option<&Screen>) -> TerminalSlotView {
    let Some(p) = screen else {
        return TerminalSlotView {
            slot_id,
            rows: Vec::new(),
            cursor: None,
            no_shell: true,
            instances: Vec::new(),
            active: None,
            exited: None,
            profiles: Vec::new(),
            list_cols: 0,
        };
    };
    let (_, height) = p.size();
    TerminalSlotView {
        slot_id,
        rows: (0..height)
            .map(|f| {
                p.row_tramos(f)
                    .into_iter()
                    .map(|(text, style)| span_view(text, style))
                    .collect()
            })
            .collect(),
        cursor: cursor_at(p),
        no_shell: false,
        instances: Vec::new(),
        active: None,
        exited: None,
        profiles: Vec::new(),
        list_cols: 0,
    }
}

/// Where the cursor goes, or `None` if the shell hid it.
///
/// Any full-screen program hides it while it paints, and then painting it
/// would be making up where it is.
fn cursor_at(p: &Screen) -> Option<(u16, u16)> {
    if !p.cursor_visible() {
        return None;
    }
    let (row, col) = p.cursor();
    let (width, height) = p.size();
    // The column can be worth as much as the width — the "pending wrap"
    // state — and there the cursor is painted in the last cell, which is
    // where a real terminal leaves it.
    (row < height).then(|| (row, col.min(width.saturating_sub(1))))
}

fn span_view(text: String, e: Style) -> TerminalSpanView {
    TerminalSpanView {
        text,
        fg: color_view(e.fg),
        bg: color_view(e.bg),
        bold: e.bold,
        dim: e.tenue,
        italic: e.italic,
        underline: e.underlined,
        reverse: e.inverse,
        strike: e.strikethrough,
    }
}

fn color_view(c: ColorTerm) -> Option<TerminalColorView> {
    match c {
        ColorTerm::Default => None,
        ColorTerm::Indexed(index) => Some(TerminalColorView::Indexed { index }),
        ColorTerm::Rgb(r, g, b) => Some(TerminalColorView::Rgb {
            hex: format!("#{r:02x}{g:02x}{b:02x}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An index crosses the bridge UNRESOLVED, and an RGB as hex.
    ///
    /// This is the module's decision, and it is pinned with a test because
    /// the day someone "improves" this by resolving the index against the
    /// theme, the panel will stop obeying the reader's theme and nothing else
    /// will turn red.
    #[test]
    fn an_index_stays_an_index_and_an_rgb_is_hex() {
        let mut p = Screen::new(12, 1);
        p.alimentar(b"\x1b[31ma\x1b[38;2;1;2;3mb");
        let v = build_view(7, Some(&p));
        let row = &v.rows[0];
        assert_eq!(
            row[0].fg,
            Some(TerminalColorView::Indexed { index: 1 }),
            "the shell's color 1 is not resolved here"
        );
        assert_eq!(
            row[1].fg,
            Some(TerminalColorView::Rgb {
                hex: "#010203".to_owned()
            })
        );
    }

    /// Without a shell, the view SAYS so instead of sending an empty grid.
    #[test]
    fn no_shell_says_so() {
        let v = build_view(3, None);
        assert!(v.no_shell);
        assert!(v.rows.is_empty());
        assert_eq!(v.cursor, None);
    }

    /// The cursor does not cross if the shell hid it.
    #[test]
    fn a_hidden_cursor_does_not_cross() {
        let mut p = Screen::new(10, 3);
        p.alimentar(b"hola");
        assert_eq!(build_view(1, Some(&p)).cursor, Some((0, 4)));
        p.alimentar(b"\x1b[?25l");
        assert_eq!(build_view(1, Some(&p)).cursor, None);
    }

    /// ALL the grid's rows go across, including the empty ones: a terminal
    /// does not scroll like a list, it repaints, and a renderer that only got
    /// the written ones would have to guess the height.
    #[test]
    fn all_rows_go_across() {
        let mut p = Screen::new(6, 4);
        p.alimentar(b"una");
        let v = build_view(1, Some(&p));
        assert_eq!(v.rows.len(), 4);
    }
}
