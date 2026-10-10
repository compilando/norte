//! Plugin command rows for the search box, and running a command chosen
//! there.
//!
//! Part of `controller`: these are methods of `State` (ADR 0086). The only
//! writer is still the actor.

// These modules are the same `impl State` split into pieces, so they use
// the same imports as the parent. Listing them here would be a forty-line
// list per file, across 32 files, that goes out of sync the moment the
// parent imports something — `super::*` keeps it in sync on its own.
#[allow(clippy::wildcard_imports)]
use super::*;

impl State {
    /// Runs a command row chosen in the search box, built-in or plugin.
    ///
    /// The box's closing patch is the caller's, already sent: this is only
    /// what the command does.
    pub(super) fn run_command_key(
        &mut self,
        cmd: &str,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        norte_frontend::session::note_palette_recent(&mut self.palette_recent, cmd);
        // Through the SAME path as a key: the box is another door into the
        // catalogue, not a second dispatcher.
        match effect_of(cmd, 1) {
            Some(effect) => self.apply_effect(effect, backend, mailbox),
            // A PLUGIN row is not in the command catalogue and cannot be: a
            // third party contributes it at runtime.
            None if cmd.starts_with("plugin:") => self.run_from_plugin(cmd, backend, mailbox),
            // A RENAMER row (C3, ADR 0095): requests the plan and puts it
            // into the SAME review as the AI's.
            None if cmd.starts_with("renamer:") => self.run_from_renamer(cmd, backend, mailbox),
            // An ORGANIZER row (phase 8): the same dispatch, another method,
            // and the plan lands on the same reviewable tree as the model's.
            None if cmd.starts_with("organizer:") => {
                match norte_frontend::palette::parse_organizer_key(cmd) {
                    Some((id, org)) => {
                        let (id, org) = (id.to_owned(), org.to_owned());
                        self.request_organize_plan(Some((id, org)), backend, mailbox)
                    }
                    None => self.no_implemented(cmd),
                }
            }
            None => self.no_implemented(cmd),
        }
    }

    /// Requests the catalogue for the box's PLUGIN rows.
    ///
    /// It is not awaited: the box is already painted with its own commands,
    /// and the plugin ones are joined in once the daemon answers.
    /// Freezing the window until then would pay for the round trip even when
    /// there is no extension at all.
    pub(super) fn request_plugin_rows(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) {
        // In READ-ONLY they are not requested: what a plugin command does is
        // the plugin's decision, and this window is not going to launch it.
        // Same rule `palette_rows` already applies to its own commands —
        // offering what will be refused is promising something that will not
        // happen.
        if self.effects == crate::commands::Effects::SoloRead {
            return;
        }
        self.gen_palette += 1;
        let generation = self.gen_palette;
        let backend = Arc::clone(backend);
        let mailbox = mailbox.clone();
        tokio::spawn(async move {
            let res = match tokio::time::timeout(DEADLINE_PLUGINS, backend.plugin_list()).await {
                Ok(r) => r,
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            let _ = mailbox
                .send(Message::Background(Box::new(Background::PalettePlugins(
                    generation, res,
                ))))
                .await;
        });
    }

    /// The plugin rows arrived: they are joined into the open box, query and
    /// cursor kept (`replace_section`): losing what someone just typed for
    /// the sake of rows that arrive late is worse than not having them.
    ///
    /// A failure does NOT bring the box down, nor is it announced: the
    /// window's own commands are still there, which is the same criterion as
    /// the TUI's ("a dead daemon degrades the palette, it does not bring it
    /// down").
    pub(super) fn apply_plugin_rows(
        &mut self,
        generation: u64,
        res: Result<norte_proto::methods::PluginListResult, Error>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        if generation != self.gen_palette {
            return Vec::new();
        }
        let Ok(list) = res else {
            return Vec::new();
        };
        let Some(g) = self.ir_a.as_mut() else {
            return Vec::new();
        };
        // The same filter and the same cap the MANAGER applies to the
        // catalogue: a hostile daemon can announce whatever plugins it
        // wants, and here each one also contributes a row per command.
        // Without `is_valid_plugin_id`, an id with a `:` inside breaks the
        // key that `plugin_rows` builds and `parse_plugin_key` undoes, which
        // is exactly the contract the two share.
        let catalog: Vec<_> = list
            .plugins
            .iter()
            .filter(|p| norte_proto::methods::is_valid_plugin_id(&p.id))
            .take(crate::extensions::MAX_EXTENSIONS)
            .cloned()
            .collect();
        // The SHARED model decides what gets offered: only approved and
        // enabled ones — the same gate `plugin.run_command` enforces on its
        // own — in manifest order, and with the prefix that keeps a
        // third-party command from disguising itself as one of ours.
        let mut rows = norte_frontend::palette::plugin_rows_in(&catalog, self.lang);
        // And a cap on ROWS: the manifest does not bound how many commands a
        // plugin declares, so an approved one with two hundred thousand
        // turned every `ctrl+p` into a message of hundreds of megabytes.
        rows.truncate(crate::bridge::MAX_ROWS_PER_BATCH);
        if rows.is_empty() {
            return Vec::new();
        }
        g.replace_section(
            norte_frontend::goto::SECTION_PLUGINS,
            norte_frontend::goto::plugin_command_rows(rows),
            false,
        );
        self.labels_plugin = catalog
            .iter()
            .map(|p| {
                let commands = p
                    .commands
                    .iter()
                    .map(|c| (c.id.clone(), crate::extensions::third_party_text(&c.title)))
                    .collect();
                (
                    p.id.clone(),
                    (crate::extensions::third_party_text(&p.name), commands),
                )
            })
            .collect();
        let change = ViewChange::Goto {
            goto: self.vista_ir_a(),
        };
        vec![self.parche(vec![change])]
    }

    /// Runs an extension command and shows its output.
    ///
    /// The authorization is the SERVER's: `plugin.run_command` resolves the
    /// command against the catalogue and enforces approved + enabled on its
    /// own. What the row checks on this side is consistency with what the
    /// reader is currently looking at, never the permission.
    pub(super) fn run_from_plugin(
        &mut self,
        key: &str,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some((id, command)) = norte_frontend::palette::parse_plugin_key(key) else {
            return self.no_implemented(key);
        };
        if self.effects == crate::commands::Effects::SoloRead {
            // What a plugin command does is the plugin's decision: it can
            // write. A window without effects does not launch it.
            return Self::no_mutates();
        }
        self.gen_output += 1;
        let generation = self.gen_output;
        // Both labels are resolved NOW, with the catalogue the box used:
        // the response can take a while, and looking them up again on return
        // means looking them up in a catalogue that is no longer the same
        // one.
        let (label, title) = self.command_labels(id, command);
        let backend2 = Arc::clone(backend);
        let mailbox2 = mailbox.clone();
        let (id2, command2) = (id.to_owned(), command.to_owned());
        let id3 = id2.clone();
        tokio::spawn(async move {
            let res = match tokio::time::timeout(
                DEADLINE_COMMAND,
                backend2.plugin_run_command(id2, command2, String::new()),
            )
            .await
            {
                Ok(r) => r,
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            let _ = mailbox2
                .send(Message::Background(Box::new(Background::CommandOutput(
                    generation,
                    Box::new(OutputRequested {
                        id: id3,
                        plugin: label,
                        command: title,
                        res,
                    }),
                ))))
                .await;
        });
        (self.applied(), self.say("host-plugin-running"))
    }

    /// What to call things, for the output panel: the extension's name and
    /// the command's title, already masked. If the manager is not open it
    /// falls back to the id, which is the only thing this process assigns.
    pub(super) fn command_labels(
        &self,
        id: &str,
        command: &str,
    ) -> (crate::extensions::Text, crate::extensions::Text) {
        let from_catalog = self.labels_plugin.get(id);
        let name = from_catalog
            .map(|(n, _)| n.clone())
            .or_else(|| Some(self.extensions.as_ref()?.grant(id)?.name))
            // Without a known label it falls back to the id — which the core
            // DOES validate — but through the same gate as everything else:
            // it is the daemon that sends it, not this process.
            .unwrap_or_else(|| crate::extensions::third_party_text(id));
        let title = from_catalog
            .and_then(|(_, cs)| cs.get(command).cloned())
            .or_else(|| {
                self.extensions
                    .as_ref()?
                    .id_commands(id)
                    .iter()
                    .find(|c| c.id == command)
                    .map(|c| (c.title.clone(), c.hostile))
            })
            // A COMMAND's id is not painted: the manifest does not validate
            // its charset. Without a known title, the line is left without
            // one.
            .unwrap_or_default();
        (name, title)
    }
}
