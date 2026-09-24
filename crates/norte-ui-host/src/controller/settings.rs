//! The settings screen: showing them, cycling them, requesting a value, and
//! writing it.
//!
//! Part of `controller`: these are methods of `State`, moved here without
//! touching them (ADR 0086). The only writer is still the actor.

// These modules are the same `impl State` split into pieces, so they use
// the same imports as the parent. Listing them here would be a forty-line
// list per file, across 32 files, that goes out of sync the moment the
// parent imports something — `super::*` keeps it in sync on its own.
#[allow(clippy::wildcard_imports)]
use super::*;

/// What comes back from writing a setting, OUTSIDE the actor.
///
/// The write and the reread go together in the same `spawn_blocking`: what
/// the bar says at the end — "saved" and what did not apply — needs both, and
/// two trips through the mailbox would be two intermediate states nobody
/// wants to see.
pub(super) struct SettingWritten {
    /// The entry's name, already translated, for the message.
    pub(super) name: String,
    /// The new value as text, for the message.
    pub(super) valor: String,
    /// `Err` is the key for why it was NOT written. `Ok(None)` is that it was
    /// written but the reread failed: the file is fine — `persist_set` just
    /// wrote it —, so "saved" is said and the window keeps the configuration
    /// it had until it restarts.
    pub(super) result: Result<Option<norte_frontend::config::FrontendConfig>, &'static str>,
}

/// An F11 key is NO LONGER set (or could not be removed), and the
/// configuration reread with it gone.
///
/// It carries the `id` because the message depends on what the row says AFTER
/// rereading: removing the key from your layer does not return the factory
/// value if the profile or the project set the same one.
pub(super) struct SettingRestablecido {
    /// The entry's name, already translated, for the message.
    pub(super) name: String,
    /// Its catalogue id (`ui.theme`), to find the row again.
    pub(super) id: &'static str,
    /// `Err` is the key for why it was not removed.
    pub(super) result: Result<Option<norte_frontend::config::FrontendConfig>, &'static str>,
}

impl State {
    /// Opens settings.
    ///
    /// The rows are built HERE and frozen, like the palette's and for the
    /// same reason: `build_rows` resolves each entry's effective value and
    /// formats two Fluent strings per row. The configuration is the one the
    /// window has SET, which after a profile change or a written setting is
    /// no longer the startup one.
    pub(super) fn open_settings(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.settings = Some(crate::settings::Settings::open(
            &self.config,
            &self.paths,
            self.lang,
        ));
        let change = ViewChange::Settings {
            settings: self.vista_settings(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// A click on a settings row: only moves the cursor.
    pub(super) fn choose_setting(
        &mut self,
        row: u32,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(settings) = self.settings.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        settings.point_at(row as usize);
        let change = ViewChange::Settings {
            settings: self.vista_settings(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// A double click on a row: selects it and activates it, which is what
    /// Enter does. The SAME path and no other: the mouse is a second door
    /// into the same machine, not a second machine.
    pub(super) fn activate_setting_by_mouse(
        &mut self,
        row: u32,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(settings) = self.settings.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        settings.point_at(row as usize);
        self.activate_setting(mailbox)
    }

    /// Settings' projection.
    ///
    /// The theme and preset lists are resolved HERE and live, like on
    /// activating a row: the effective theme changes live, and a dropdown
    /// with the list from two reloads ago offers what is no longer there.
    pub(super) fn vista_settings(&self) -> Option<crate::dto::SettingsView> {
        let themes = norte_frontend::theme::theme_names(&self.config.user_themes);
        Some(self.settings.as_ref()?.vista(
            self.lang,
            &themes,
            norte_frontend::keymap::presets::NAMES,
        ))
    }

    /// What has been typed in the search box.
    ///
    /// With a dialog in front, NO: the same guard as `activate_setting`, and for
    /// a worse reason. The value prompt stays open with the live screen
    /// behind it, and filtering underneath it changes which rows there are —
    /// whatever the dialog confirms is looked up by id, so it no longer
    /// writes to a different one, but the screen changing under a modal is
    /// what makes nobody understand where the value ended up.
    pub(super) fn search_setting(
        &mut self,
        text: &str,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if !self.dialogs.is_empty() {
            return (Self::stale(StaleAction::Modal), Vec::new());
        }
        let Some(settings) = self.settings.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        settings.query(text);
        let change = ViewChange::Settings {
            settings: self.vista_settings(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// A click on the index: the cursor to that section's first row.
    pub(super) fn jump_to_section(
        &mut self,
        section: &str,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if !self.dialogs.is_empty() {
            return (Self::stale(StaleAction::Modal), Vec::new());
        }
        let Some(settings) = self.settings.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        settings.skip(section);
        let change = ViewChange::Settings {
            settings: self.vista_settings(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// A window control set a value: it is validated with the shared editor
    /// and, if valid, written.
    ///
    /// Same path as cycling with Enter from here on — write on the background
    /// thread, reread, snapshot — because it is the same operation: the only
    /// difference is who chose the value.
    pub(super) fn set_setting(
        &mut self,
        id: &str,
        value: &str,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if !self.dialogs.is_empty() {
            return (Self::stale(StaleAction::Modal), Vec::new());
        }
        let themes = norte_frontend::theme::theme_names(&self.config.user_themes);
        let Some(settings) = self.settings.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        match settings.set(id, value, &themes, norte_frontend::keymap::presets::NAMES) {
            Ok(write) => {
                let change = ViewChange::Settings {
                    settings: self.vista_settings(),
                };
                let mut outgoing = vec![self.parche(vec![change])];
                let (reason, parts) = self.write_setting(write, mailbox);
                outgoing.extend(parts);
                match reason {
                    Some(key) => (
                        ActionAck::Unavailable {
                            reason_key: key.to_owned(),
                        },
                        outgoing,
                    ),
                    None => (self.applied(), outgoing),
                }
            }
            // The rejection is said WITH its numbers, like the keyboard's: a
            // key alone does not say between what and what.
            Err(e) => {
                let (key, text) = match e {
                    norte_frontend::settings::SettingsEditError::NotAnInt => (
                        "msg-settings-invalid-int",
                        norte_i18n::t_in(self.lang, "msg-settings-invalid-int"),
                    ),
                    norte_frontend::settings::SettingsEditError::OutOfRange { min, max } => (
                        "msg-settings-invalid-range",
                        norte_i18n::ta_in(
                            self.lang,
                            "msg-settings-invalid-range",
                            &[("min", &min.to_string()), ("max", &max.to_string())],
                        ),
                    ),
                    norte_frontend::settings::SettingsEditError::Invalid { key } => {
                        (key, norte_i18n::t_in(self.lang, key))
                    }
                };
                self.status.message = Some(clamp_display(text));
                let changes = vec![
                    ViewChange::Settings {
                        settings: self.vista_settings(),
                    },
                    ViewChange::Status(self.status.clone()),
                ];
                let outgoing = vec![self.parche(changes)];
                (
                    ActionAck::Unavailable {
                        reason_key: key.to_owned(),
                    },
                    outgoing,
                )
            }
        }
    }

    /// Resets a row: removes its key from the writing layer.
    ///
    /// Goes through the SAME path as writing — background thread, mailbox,
    /// reread and snapshot — because it is the same class of operation: I/O
    /// with a cross-process lock behind it. The only difference is what gets
    /// said at the end, and that is decided by LOOKING at the reread row:
    /// removing the key from your layer does not return the factory value if
    /// the profile or the project sets it.
    pub(super) fn reset_setting(
        &mut self,
        row: u32,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if !self.dialogs.is_empty() {
            return (Self::stale(StaleAction::Modal), Vec::new());
        }
        let Some(settings) = self.settings.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        settings.point_at(row as usize);
        let Some(reset) = settings.reset(row as usize) else {
            // Nothing to remove: no notice and no write. A row already at its
            // factory value has no key anywhere.
            let change = ViewChange::Settings {
                settings: self.vista_settings(),
            };
            return (self.applied(), vec![self.parche(vec![change])]);
        };
        let Some(dir) = self.write_dir() else {
            return (self.applied(), self.say("host-no-config-dir"));
        };
        let layers = self.layers_actuales();
        let mailbox = mailbox.clone();
        let norte_frontend::settings::PendingReset {
            section,
            key,
            id,
            name,
        } = reset;
        tokio::task::spawn_blocking(move || {
            let result = match norte_config::persist_unset(&dir, section, &key) {
                // The error does NOT travel: it can carry the file's path
                // (#73).
                Err(e) => Err(io_key(&e)),
                Ok(_) => Ok(norte_frontend::config::load(&layers).ok()),
            };
            let done = SettingRestablecido { name, id, result };
            let _ = mailbox.blocking_send(Message::Background(Box::new(
                Background::SettingRestablecido(Box::new(done)),
            )));
        });
        (self.applied(), Vec::new())
    }

    /// The key is no longer there (or could not be removed): what was reread
    /// is applied and what really happened is said.
    pub(super) fn setting_restablecido(
        &mut self,
        done: SettingRestablecido,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let SettingRestablecido {
            name: nombre,
            id,
            result: resultado,
        } = done;
        let cfg = match resultado {
            Err(key) => return self.say(key),
            Ok(cfg) => cfg,
        };
        if let Some(cfg) = cfg {
            self.apply_config(cfg, backend, mailbox);
        }
        if let Some(settings) = self.settings.as_mut() {
            settings.refresh(&self.config, self.lang);
        }
        // The reread point is the answer: if the row is still modified,
        // another layer sets it and the value has not gone back to the
        // factory one.
        let still_set = self
            .settings
            .as_ref()
            .is_some_and(|a| a.follows_modificada(id));
        let key = if still_set {
            "settings-still-set-elsewhere"
        } else {
            "settings-reset-done"
        };
        self.status.message = Some(clamp_display(norte_i18n::ta_in(
            self.lang,
            key,
            &[("name", &nombre)],
        )));
        let snap = self.snapshot();
        vec![self.over(UiUpdate::Snapshot(Box::new(snap)))]
    }

    /// The keys while settings is open.
    ///
    /// FIXED, like the palette's and help's: the catalogue has no commands
    /// for "go down this list". `enter` activates the row: cycles whatever
    /// cycles, and asks in a dialog for whatever is typed.
    pub(super) fn key_in_settings(
        &mut self,
        k: &crate::keys::KeyInput,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        /// How many rows a page moves.
        const PAGE: i64 = 10;
        if self.settings.is_none() {
            return (Self::stale(StaleAction::Modal), Vec::new());
        }
        let verb = match k.key.as_str() {
            "Home" | "home" | "End" | "end" => None,
            _ => self.dialog_verb(k),
        };
        if verb.as_deref() == Some("dialog.confirm") {
            return self.activate_setting(mailbox);
        }
        let Some(settings) = self.settings.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        match (verb.as_deref(), k.key.as_str()) {
            (Some("dialog.cancel"), _) => self.settings = None,
            (Some("dialog.down"), _) => settings.mover(1),
            (Some("dialog.up"), _) => settings.mover(-1),
            (Some("dialog.page-down"), _) => settings.mover(PAGE),
            (Some("dialog.page-up"), _) => settings.mover(-PAGE),
            (_, "Home" | "home") => settings.mover(i64::MIN / 2),
            (_, "End" | "end") => settings.mover(i64::MAX / 2),
            // Switches sides, like in help. By the key and not by a catalogue
            // verb: on this screen `dialog.pane` means nothing, and the index
            // is not a panel.
            (_, "Tab" | "tab") => settings.change_side(),
            _ => return (self.applied(), Vec::new()),
        }
        let change = ViewChange::Settings {
            settings: self.vista_settings(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// Activates the cursor's row: whatever cycles is written right away;
    /// whatever is typed is requested in a dialog.
    ///
    /// The theme and preset lists are resolved HERE and live, like in the
    /// terminal: the effective theme may have changed live.
    fn activate_setting(
        &mut self,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // With a dialog in front nothing activates: the keyboard already
        // orders it this way (the dialog gets the key first), and the mouse
        // has to do the same or a double click with the value prompt open
        // would write — or stack a second prompt — behind it.
        if !self.dialogs.is_empty() {
            return (Self::stale(StaleAction::Modal), Vec::new());
        }
        let Some(settings) = self.settings.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        let themes = norte_frontend::theme::theme_names(&self.config.user_themes);
        match settings.activate(&themes, norte_frontend::keymap::presets::NAMES) {
            crate::settings::Activacion::Nothing => (self.applied(), Vec::new()),
            crate::settings::Activacion::Write(write) => {
                let change = ViewChange::Settings {
                    settings: self.vista_settings(),
                };
                let mut outgoing = vec![self.parche(vec![change])];
                let (reason, parts) = self.write_setting(*write, mailbox);
                outgoing.extend(parts);
                match reason {
                    Some(reason_key) => (
                        ActionAck::Unavailable {
                            reason_key: reason_key.to_owned(),
                        },
                        outgoing,
                    ),
                    None => (self.applied(), outgoing),
                }
            }
            crate::settings::Activacion::RequestText {
                name: nombre,
                actual,
                id,
            } => self.request_setting_value(&nombre, actual, id),
        }
    }

    /// Requests a text entry's value: the field dialog, with the current
    /// value inside, and the entry's name as the body.
    ///
    /// This is the window's way of doing what the terminal does by typing in
    /// the row: its field is native and the text comes back whole on
    /// confirming.
    fn request_setting_value(
        &mut self,
        name: &str,
        current: String,
        setting_id: &'static str,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let id = ModalId(self.next_modal);
        self.next_modal += 1;
        // The current value comes from a `norte.toml` that could be the
        // project's: it is painted masked, and what is edited is the real
        // one.
        let (displayable, hostile) = norte_frontend::display_name(current.as_bytes());
        let view = DialogView {
            id,
            title_key: "modal-setting-edit".to_owned(),
            destination: None,
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body: vec![crate::dto::DialogLine {
                text: clamp_display(name.to_owned()),
                hostile: false,
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
            input: Some(clamp_display(displayable)),
            input_hostile: hostile,
            input_secret: false,
            fields: Vec::new(),
            dest_check: crate::dto::DestCheckView::NotAsked,
        };
        self.dialogs.push(Dialog {
            id,
            vista: view,
            typed: Typed::Text(current),
            recognized: true,
            on_confirm: Some(Pending::EditSetting { id: setting_id }),
        });
        let change = ViewChange::Dialogs {
            dialogs: self.dialog_views(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// The dialog brought the value for row `row`: it is validated with the
    /// shared editor and, if valid, written.
    ///
    /// A rejection is said on the bar WITH its numbers — "between 8 and 32" —
    /// and goes back to the acknowledgment by its key, without writing
    /// anything.
    pub(super) fn confirm_setting_value(
        &mut self,
        id: &'static str,
        text: &str,
        mailbox: &mpsc::Sender<Message>,
    ) -> (Option<&'static str>, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(settings) = self.settings.as_mut() else {
            // Settings closed with the dialog in front: there is no row to go
            // back to, and writing "blind" would be writing something else.
            return (
                Some("host-settings-closed"),
                self.say("host-settings-closed"),
            );
        };
        match settings.confirm_text(id, text) {
            Ok(write) => {
                let change = ViewChange::Settings {
                    settings: self.vista_settings(),
                };
                let mut outgoing = vec![self.parche(vec![change])];
                let (reason, parts) = self.write_setting(write, mailbox);
                outgoing.extend(parts);
                (reason, outgoing)
            }
            Err(e) => {
                // With the numbers: the key alone does not say between what
                // and what. And with the HOST's language, which is the bar's.
                let (key, text) = match e {
                    norte_frontend::settings::SettingsEditError::NotAnInt => (
                        "msg-settings-invalid-int",
                        norte_i18n::t_in(self.lang, "msg-settings-invalid-int"),
                    ),
                    norte_frontend::settings::SettingsEditError::OutOfRange { min, max } => (
                        "msg-settings-invalid-range",
                        norte_i18n::ta_in(
                            self.lang,
                            "msg-settings-invalid-range",
                            &[("min", &min.to_string()), ("max", &max.to_string())],
                        ),
                    ),
                    norte_frontend::settings::SettingsEditError::Invalid { key } => {
                        (key, norte_i18n::t_in(self.lang, key))
                    }
                };
                self.status.message = Some(clamp_display(text));
                let patch = self.parche(vec![ViewChange::Status(self.status.clone())]);
                (Some(key), vec![patch])
            }
        }
    }

    /// Writes `write` to the layer this window writes and rereads the
    /// configuration, OUTSIDE the actor.
    ///
    /// The actor is the state's only writer and this is I/O with a
    /// cross-process lock behind it (`persist_set` blocks while another norte
    /// writes): doing it here would freeze the whole window. It comes back
    /// through the mailbox like the theme and the favorites.
    ///
    /// The layer is the active PROFILE's if there is one, and the user's if
    /// not ([`Self::write_dir`]): a setting written below one the
    /// profile also sets ends up covered — saved and with no effect (ADR
    /// 0079).
    pub(super) fn write_setting(
        &mut self,
        write: norte_frontend::settings::PendingWrite,
        mailbox: &mpsc::Sender<Message>,
    ) -> (Option<&'static str>, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(dir) = self.write_dir() else {
            return (Some("host-no-config-dir"), self.say("host-no-config-dir"));
        };
        let layers = self.layers_actuales();
        let mailbox = mailbox.clone();
        let norte_frontend::settings::PendingWrite {
            section,
            key,
            value,
            name,
            display,
        } = write;
        tokio::task::spawn_blocking(move || {
            let result = match norte_config::persist_set(&dir, section, &key, value) {
                // The error does NOT travel: it can carry the file's path,
                // and what the bar says comes from the catalogue (#73). The
                // category is enough to know what happened.
                Err(e) => Err(io_key(&e)),
                Ok(_) => Ok(norte_frontend::config::load(&layers).ok()),
            };
            let done = SettingWritten {
                name,
                valor: display,
                result,
            };
            let _ = mailbox.blocking_send(Message::Background(Box::new(
                Background::SettingWritten(Box::new(done)),
            )));
        });
        (None, Vec::new())
    }

    /// The setting is now (or is not) in the file: it is said, and the reread
    /// configuration is applied through the same path as a profile change.
    ///
    /// If settings is still open, its rows are rebuilt over what was reread:
    /// the row cycled optimistically on activating it, and this leaves it
    /// saying what the file says. It ends in a SNAPSHOT and not a patch
    /// because the theme and the keymap move the whole screen.
    pub(super) fn setting_written(
        &mut self,
        done: SettingWritten,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let SettingWritten {
            name: nombre,
            valor,
            result: resultado,
        } = done;
        let cfg = match resultado {
            Err(key) => {
                // The optimistic row was LYING: it is rebuilt over what is
                // actually set.
                if let Some(settings) = self.settings.as_mut() {
                    settings.refresh(&self.config, self.lang);
                }
                let mut outgoing = self.say(key);
                let change = ViewChange::Settings {
                    settings: self.vista_settings(),
                };
                outgoing.push(self.parche(vec![change]));
                return outgoing;
            }
            Ok(None) => {
                tracing::warn!("the setting was written but the configuration could not be reread");
                None
            }
            Ok(Some(cfg)) => Some(cfg),
        };
        let unapplied = cfg.map_or_else(Vec::new, |cfg| self.apply_config(cfg, backend, mailbox));
        // Whatever the active profile's file brings that is not understood is
        // still there after rereading: a trace is left, like on a profile
        // change. Without the bar message, which here is taken by "saved" and
        // the profile already said it when it was set.
        for warning in &self.config.common.profile_warnings {
            tracing::warn!(motivo = %warning, "profile line ignored");
        }
        if let Some(settings) = self.settings.as_mut() {
            settings.refresh(&self.config, self.lang);
        }
        self.status.message = Some(clamp_display(if unapplied.is_empty() {
            norte_i18n::ta_in(
                self.lang,
                "msg-settings-saved",
                &[("name", &nombre), ("value", &valor)],
            )
        } else {
            norte_i18n::ta_in(
                self.lang,
                "msg-settings-saved-restart",
                &[("name", &nombre), ("value", &valor)],
            )
        }));
        let snap = self.snapshot();
        vec![self.over(UiUpdate::Snapshot(Box::new(snap)))]
    }
}
