//! The command bar in the window (ADR 0181, bridge 113): the same model as
//! the terminal's (`norte_frontend::command_bar`), keys routed here while it
//! has the keyboard, and each order run as a new terminal-panel entry.

#[allow(clippy::wildcard_imports)]
use super::*;

use norte_frontend::shell::Shell;

/// The reader's login shell, as the quoting knows it (`None` = POSIX sh).
fn shell_kind() -> Option<Shell> {
    norte_frontend::shell::login_shell()
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(Shell::parse)
}

impl State {
    /// Is the bar on screen: pinned by `[ui] command_bar`, or called up.
    pub(super) fn command_bar_visible(&self) -> bool {
        self.config.common.ui_chrome.command_bar() || self.command_bar_focus
    }

    /// The bar's projection, `None` when hidden.
    pub(super) fn vista_command_bar(&self) -> Option<crate::dto::CommandBarView> {
        if !self.command_bar_visible() {
            return None;
        }
        let dir = self.slot().pane.dir();
        let (prompt, _) = norte_frontend::path_display(dir);
        let reason = norte_frontend::availability::verdict("app.terminal", &self.facts())
            .reason()
            .map_or_else(String::new, |r| {
                norte_i18n::t_in(self.lang, norte_frontend::availability::reason_key(r))
            });
        Some(crate::dto::CommandBarView {
            prompt: clamp_display(prompt),
            text: clamp_display(norte_encoding::mask_terminal_hazards(
                self.command_bar.text(),
            )),
            cursor: u32::try_from(self.command_bar.cursor()).unwrap_or(u32::MAX),
            focused: self.command_bar_focus,
            reason: clamp_display(reason),
        })
    }

    fn command_bar_patch(&mut self) -> Vec<BridgeEnvelope<UiUpdate>> {
        let change = ViewChange::CommandBar {
            command_bar: self.vista_command_bar(),
        };
        vec![self.parche(vec![change])]
    }

    /// `pane.command-line`: the bar takes the keyboard, showing it if
    /// hidden.
    pub(super) fn focus_command_bar(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.command_bar_focus = true;
        (self.applied(), self.command_bar_patch())
    }

    /// A paste into the focused bar: its first line only.
    pub(super) fn paste_in_command_bar(
        &mut self,
        text: &str,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if !self.command_bar_focus {
            return (self.applied(), Vec::new());
        }
        let first = text.lines().next().unwrap_or_default();
        self.command_bar.insert(first);
        (self.applied(), self.command_bar_patch())
    }

    /// A key while the bar has the keyboard: fixed keys, like the search
    /// box's.
    pub(super) fn key_in_command_bar(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match k.key.as_str() {
            "Escape" | "esc" => self.command_bar_focus = false,
            "Enter" | "enter" if k.alt => {
                if let Some(name) = self
                    .slot()
                    .pane
                    .cursor_entry()
                    .and_then(|e| e.path.file_name().map(|n| n.as_bytes().to_vec()))
                {
                    let word = norte_frontend::command_bar::quote_for(shell_kind(), &name);
                    self.command_bar.insert(&word);
                }
            }
            "Enter" | "enter" => return self.submit_command_bar(backend, mailbox),
            "Backspace" | "backspace" => self.command_bar.backspace(),
            "Delete" | "delete" | "del" => self.command_bar.delete(),
            "ArrowLeft" | "left" => self.command_bar.left(),
            "ArrowRight" | "right" => self.command_bar.right(),
            "Home" | "home" => self.command_bar.home(),
            "End" | "end" => self.command_bar.end(),
            "ArrowUp" | "up" => self.command_bar.history_prev(),
            "ArrowDown" | "down" => self.command_bar.history_next(),
            "Tab" | "tab" => {
                let names: Vec<Vec<u8>> = self
                    .slot()
                    .pane
                    .entries()
                    .iter()
                    .filter_map(|e| e.path.file_name().map(|n| n.as_bytes().to_vec()))
                    .collect();
                let refs: Vec<&[u8]> = names.iter().map(Vec::as_slice).collect();
                self.command_bar.complete(&refs, shell_kind());
            }
            other if !k.ctrl && !k.alt && !k.meta && other.chars().count() == 1 => {
                self.command_bar.insert(other);
            }
            _ => {}
        }
        (self.applied(), self.command_bar_patch())
    }

    /// `Enter`: refuses off disk with the shared table's reason; otherwise
    /// the order goes to the history (saved off the actor) and runs as a
    /// new terminal-panel entry, kept after a clean exit.
    fn submit_command_bar(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if let Some(r) =
            norte_frontend::availability::verdict("app.terminal", &self.facts()).reason()
        {
            let key = norte_frontend::availability::reason_key(r);
            let said = self.say(key);
            return (
                ActionAck::Unavailable {
                    reason_key: key.to_owned(),
                },
                said,
            );
        }
        let Some(cwd) = norte_frontend::shell::shell_dir(self.slot().pane.dir()) else {
            return (self.applied(), Vec::new());
        };
        let Some(order) = self.command_bar.take() else {
            return (self.applied(), self.command_bar_patch());
        };
        if let Some(path) = self.command_history_path.clone() {
            let history = self.command_bar.history().to_vec();
            let _ = std::thread::Builder::new()
                .name("command bar history".into())
                .spawn(move || norte_config::command_history::save(&path, &history));
        }
        let shell = norte_frontend::shell::login_shell();
        let argv = norte_frontend::command_bar::argv(&shell, &order);
        let launch = norte_frontend::user_commands::Launch {
            id: String::new(),
            label: order,
            program: argv.first().cloned().unwrap_or_default(),
            args: argv.into_iter().skip(1).collect(),
            cwd,
            run_in: norte_frontend::user_commands::RunIn::Terminal,
            refresh: false,
            confirm: false,
            count: 0,
        };
        let mut out = self.command_bar_patch();
        let (res, said) = self.start_user_terminal(&launch, shell, backend, mailbox);
        out.extend(said);
        match res {
            Ok(()) => {
                self.keep_active_terminal();
                (self.applied(), out)
            }
            Err(key) => (
                ActionAck::Unavailable {
                    reason_key: key.to_owned(),
                },
                out,
            ),
        }
    }
}
