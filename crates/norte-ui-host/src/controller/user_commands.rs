//! User commands in the window (ADR 0177): the decision is
//! `norte_frontend::user_commands`'; background runs are the shell's
//! (`NativeEffect::RunUserCommand`), terminal runs are the panel's.
//!
//! Part of `controller`: these are methods of `State`. The only writer is
//! still the actor.

use super::{
    ActionAck, Arc, BridgeEnvelope, Dialog, DialogChoice, DialogView, HostBackend, Message,
    ModalId, Pending, State, Typed, UiUpdate, ViewChange, clamp_display, mpsc,
};

use norte_frontend::user_commands::{self as uc, Finished, Launch, Refusal, RunIn};

impl State {
    /// `user:<id>` pressed or picked in the box: runs it, asks first
    /// (`confirm = true`), or says on the bar why not — an id commands.toml
    /// does not define included. The targets are the marks, or the cursor's
    /// entry.
    pub(super) fn run_user_command(
        &mut self,
        id: &str,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // A read-only window does not start programs.
        if self.effects == crate::commands::Effects::SoloRead {
            return Self::no_mutates();
        }
        let pane = &self.slot().pane;
        let targets = pane.marked_paths();
        match uc::request(&self.config.user_commands, id, pane.dir(), &targets) {
            Err(r) => self.refuse_user_command(&r),
            Ok(launch) if launch.confirm => self.ask_user_command(launch),
            Ok(launch) => self.launch_user_command(launch, backend, mailbox),
        }
    }

    /// Starts a launch already decided (and confirmed, if it asked).
    pub(super) fn launch_user_command(
        &mut self,
        launch: Launch,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let (result, said) = self.start_user_launch(launch, backend, mailbox);
        match result {
            Ok(()) => (self.applied(), said),
            Err(key) => (
                ActionAck::Unavailable {
                    reason_key: key.to_owned(),
                },
                said,
            ),
        }
    }

    /// [`Self::launch_user_command`] with a refusal as its key, which is what
    /// a dialog's answer (`run_pending`) reports.
    pub(super) fn start_user_launch(
        &mut self,
        launch: Launch,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (Result<(), &'static str>, Vec<BridgeEnvelope<UiUpdate>>) {
        match launch.run_in {
            RunIn::Background => {
                let label = norte_encoding::mask_terminal_hazards(&launch.label);
                if !self.native(crate::dto::NativeEffect::RunUserCommand { launch }) {
                    return (Err("host-no-desktop"), Self::without_desktop().1);
                }
                (
                    Ok(()),
                    self.say_with("user-cmd-started", &[("label", &label)]),
                )
            }
            // Resolving stats `PATH`: a handful of `stat`s, the same price
            // `pane.open` already pays on the actor for an opener's program.
            RunIn::Terminal => match uc::resolve(&launch) {
                Err(r) => {
                    let key = r.key();
                    (Err(key), self.refuse_user_command(&r).1)
                }
                Ok(program) => self.start_user_terminal(&launch, program, backend, mailbox),
            },
        }
    }

    /// Says a refusal on the bar, its arguments masked.
    fn refuse_user_command(&mut self, r: &Refusal) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let args = r.args();
        let refs: Vec<(&str, &str)> = args.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let said = self.say_with(r.key(), &refs);
        (
            ActionAck::Unavailable {
                reason_key: r.key().to_owned(),
            },
            said,
        )
    }

    /// The `confirm = true` question: what runs, on how many items.
    fn ask_user_command(&mut self, launch: Launch) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let id = ModalId(self.next_modal);
        self.next_modal += 1;
        self.dialogs.push(Dialog {
            id,
            recognized: true,
            vista: DialogView {
                id,
                title_key: "user-cmd-confirm-title".to_owned(),
                destination: None,
                subject: None,
                asker: None,
                deadline: None,
                deadline_at_ms: None,
                body: vec![crate::dto::DialogLine {
                    text: clamp_display(uc::confirm_text(&launch, self.lang)),
                    // The label is the reader's file text.
                    hostile: true,
                }],
                overflow_note: String::new(),
                overflow_hostile: false,
                choices: vec![
                    DialogChoice {
                        id: "confirm".to_owned(),
                        label_key: "dialog-confirm".to_owned(),
                        destructive: false,
                    },
                    DialogChoice {
                        id: "cancel".to_owned(),
                        label_key: "dialog-cancel".to_owned(),
                        destructive: false,
                    },
                ],
                input: None,
                input_hostile: false,
                input_secret: false,
                fields: Vec::new(),
                dest_check: crate::dto::DestCheckView::NotAsked,
            },
            typed: Typed::Text(String::new()),
            on_confirm: Some(Pending::UserCommand { launch }),
        });
        let change = ViewChange::Dialogs {
            dialogs: self.dialog_views(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// A background run ended (`UiAction::UserCommandFinished`): how, on the
    /// bar, and the panes re-listed if the command asked and it ran.
    pub(super) fn user_command_finished(
        &mut self,
        label: &str,
        finished: &Finished,
        refresh: bool,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let args = finished.args(label);
        let refs: Vec<(&str, &str)> = args.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let mut out = self.say_with(finished.key(), &refs);
        if refresh && finished.ran() {
            out.extend(self.refresh_visible(backend, mailbox).1);
        }
        (self.applied(), out)
    }
}
