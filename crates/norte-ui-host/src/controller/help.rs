//! Help: topics, context and extension pages.
//!
//! Part of `controller`: these are `State` methods, moved here without
//! touching them (ADR 0086). The only writer is still the actor.

// These modules are the same `impl State` split into pieces, so they use
// the same imports as the parent. Enumerating them here would be a
// forty-line list per file, in 32 files, that goes stale the moment the
// parent imports something — `super::*` tracks it on its own.
#[allow(clippy::wildcard_imports)]
use super::*;
use crate::dto::HelpScrollTo;

impl State {
    /// Opens help on the page for the CONTEXT the reader is in.
    ///
    /// Whoever presses `F1` while looking at a question wants the answer to
    /// THAT question, not the index. The context is a closed word
    /// (`dialog.confirm`, `viewer`, `browse`) and who claims it is said by
    /// the corpus itself on its front page, so adding a page for a new
    /// dialog does not touch this code.
    pub(super) fn open_help(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // Not over a dialog that is being TYPED into. Help keeps the
        // keyboard while it is open, so opening it over a text field turns
        // the `⌫` that fixes a typo into a step back in help, and the
        // `enter` that confirms into something else. The TUI forbids it by
        // name since H3c and for the same reasoning.
        if self.dialogs.last().is_some_and(|d| d.vista.input.is_some()) {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-help-over-input".to_owned(),
                },
                Vec::new(),
            );
        }
        let context = self.help_context();
        self.help = Some(crate::help::Help::open(
            self.lang,
            context,
            &self.effective,
            &self.effective_visor,
            self.facts(),
        ));
        // The extension catalog is requested and NOT awaited: help is
        // painted right away. The documentation is cosmetic, and a blank
        // window until the daemon answers is worse than a side panel that
        // gains rows half a second later. A failure is not reported: help is
        // painted without extension pages.
        let backend = Arc::clone(backend);
        let mailbox = mailbox.clone();
        tokio::spawn(async move {
            let res = match tokio::time::timeout(DEADLINE_PLUGINS, backend.plugin_list()).await {
                Ok(r) => r,
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            let _ = mailbox
                .send(Message::Background(Box::new(Background::HelpPlugins(res))))
                .await;
        });
        let change = ViewChange::Help {
            help: self.vista_help(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// Help's patch, and along the way the page that needs to be requested.
    ///
    /// ONE spot for both things on purpose: any gesture that changes page —
    /// an arrow, a click, a link — can land on an extension's, and that page
    /// is not in the corpus, it has to be requested. A second path that only
    /// painted would be an extension page that stays blank depending on how
    /// it is reached.
    pub(super) fn help_patch(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        self.request_plugin_page(backend, mailbox);
        let change = ViewChange::Help {
            help: self.vista_help(),
        };
        vec![self.parche(vec![change])]
    }

    /// The catalog arrived: it enters the model and, if the reader is
    /// already on an extension page, that page is requested.
    ///
    /// If help closed while this was in flight, there is nothing to do: the
    /// snapshot was of an opening that no longer exists, and keeping it for
    /// the next one would show a stale catalog.
    pub(super) fn apply_plugins_catalog(
        &mut self,
        res: Result<norte_proto::methods::PluginListResult, Error>,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Ok(list) = res else {
            return Vec::new();
        };
        let Some(a) = self.help.as_mut() else {
            return Vec::new();
        };
        let before = a.state.rows().to_vec();
        a.set_plugins(&list.plugins);
        if a.state.rows() == before {
            // A catalog that adds no row — no extension, or none with a page
            // and a valid id — does not change the screen, and a patch that
            // changes nothing forces a renderer to repaint the whole help
            // for nothing.
            return Vec::new();
        }
        self.help_patch(backend, mailbox)
    }

    /// Requests the open extension's page, if there is one and it has not
    /// already been requested in this opening of help.
    pub(super) fn request_plugin_page(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) {
        let Some(id) = self.help.as_mut().and_then(crate::help::Help::claim_page) else {
            return;
        };
        let backend = Arc::clone(backend);
        let mailbox = mailbox.clone();
        tokio::spawn(async move {
            let res = match tokio::time::timeout(DEADLINE_PLUGINS, backend.plugin_help(id.clone()))
                .await
            {
                Ok(r) => r,
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            let _ = mailbox
                .send(Message::Background(Box::new(Background::PluginPage(
                    id, res,
                ))))
                .await;
        });
    }

    /// An extension's page arrived. A failure leaves the page EMPTY with its
    /// name, which is a better answer than an error over help — and it is
    /// also what an N-1 daemon without the method sees. It is requested once
    /// per opening: closing and reopening help is the retry.
    pub(super) fn apply_plugin_page(
        &mut self,
        id: &str,
        res: Option<&norte_proto::methods::PluginHelpResult>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let a = self.help.as_mut()?;
        a.install_page(id, res?);
        let change = ViewChange::Help {
            help: self.vista_help(),
        };
        Some(self.parche(vec![change]))
    }

    /// Where the reader is, in the corpus's vocabulary.
    ///
    /// The topmost dialog wins: it is what covers the screen and what the
    /// reader is looking at. A dialog that only informs has no page of its
    /// own and falls back to the listing, which is what it was talking
    /// about.
    pub(super) fn help_context(&self) -> &'static str {
        debug_assert!(
            CONTEXTOS.contains(&self.context_computed()),
            "a context outside the declared vocabulary"
        );
        self.context_computed()
    }

    /// The computation, without the anchor.
    pub(super) fn context_computed(&self) -> &'static str {
        if let Some(d) = self.dialogs.last() {
            return match d.on_confirm {
                // A transfer shares a page with deletion: both are "the
                // question that must be answered before something changes",
                // and the corpus has ONE page about that.
                // The collision has its OWN page in the corpus: its keys are
                // different (`dialog.overwrite`, `dialog.skip`…), and sending
                // the reader to the confirm page would show them ones that do
                // not apply.
                Some(Pending::Retry { .. }) => "dialog.collision",
                // Closing falls here for the same reason: it is "answer
                // before something is lost", and what is lost is a copy left
                // halfway.
                Some(
                    Pending::Delete { .. }
                    | Pending::Transferir { .. }
                    | Pending::Release { .. }
                    // Uninstalling is a delete that asks: same page as
                    // deletion.
                    | Pending::UninstallExtension { .. }
                    // Undoing up to a point is an accepted consequence: the
                    // same page as in the TUI.
                    | Pending::UndoUntil { .. }
                    | Pending::Exit,
                ) => "dialog.confirm",
                Some(
                    Pending::Decide { .. }
                    | Pending::ApproveExtension { .. }
                    | Pending::UndoSession { .. },
                ) => "dialog.approval",
                // Search shares a page with create: both are the dialog that
                // asks you to type a name, and the corpus has ONE page about
                // that.
                // Rename shares a page with create and with search: all
                // three are the dialog that asks you to type something, and
                // the corpus has ONE page about that.
                Some(Pending::InstructionIa { .. }) => "dialog.ai-rename",
                // The batch template (#310): the same page the TUI gives its
                // prompt, the rename one.
                Some(Pending::TemplateBatch { .. }) => "dialog.rename",
                // #327: the password one has its OWN page, the same one the
                // TUI uses (`remote.md`, next to TOFU). Sending it to "type a
                // name" would be the divergence between frontends ADR 0077
                // exists to prevent, committed in the very change that adds
                // its parity test.
                Some(Pending::DeliverSecret { .. }) => "dialog.ask-secret",
                // #311: the checksums dialog is a READ-ONLY box over what is
                // under the cursor, like properties.
                Some(Pending::CopyChecksums { .. }) => "dialog.properties",
                Some(
                    Pending::CreateDirectory { .. }
                    | Pending::CreateFile { .. }
                    | Pending::Search { .. }
                    | Pending::Rename { .. }
                    // The semantic query is another dialog that asks you to
                    // type something, and the corpus has ONE page about
                    // that.
                    | Pending::QuerySemantic
                    // Marking by pattern, same thing: a dialog that asks you
                    // to type something.
                    | Pending::Patron { .. }
                    // And packing: what is typed is the container's name,
                    // which is where the format comes from.
                    | Pending::Pack { .. }
                    // Splitting asks for a SIZE, but it is the same dialog of
                    // a text field and a confirmation.
                    | Pending::Split { .. }
                    // And saving the profile asks for a NAME, which ends up
                    // being a directory: same single-field dialog (#318).
                    | Pending::SaveProfile
                    // And a text setting's value: a prefilled field and two
                    // buttons.
                    | Pending::EditSetting { .. }
                    // And permissions ask for a MODE, with the same shape
                    // (#314). The corpus documents them on the properties
                    // page, but the key CONTEXT is this one: a field and two
                    // buttons.
                    | Pending::Permissions { .. }
                    // And a favorite's name (#309): a prefilled field and two
                    // buttons, the same shape as all the ones above.
                    | Pending::SaveFavorite { .. },
                ) => "dialog.mkdir",
                None => "browse",
            };
        }
        if self.visor.is_some() {
            return "viewer";
        }
        "browse"
    }

    /// The facts help uses to dim a row, frozen on open.
    ///
    /// The two read-only ones come from the slot's capabilities, requested
    /// when each listing lands. They used to be wired to `false` with a
    /// comment saying the host did not track that — it did, since #268, and
    /// dropped everything but the fold mode, so inside a container the
    /// terminal dimmed F5/F8 and this window offered them lit. The SOURCE is
    /// the active slot and the DESTINATION is the one holding the role,
    /// which are exactly the two slots the shared table asks about.
    ///
    /// With no destination designated — three or more roleless slots, or no
    /// more than one — the answer is `false`, and the divergence from the
    /// terminal's `App::help_facts` is DELIBERATE: there, the answer comes
    /// from its own slot, which dims F5 saying "read only" when the real
    /// obstacle is that there is nowhere to copy to. A false cause shows the
    /// reader something that is not so; here the key is offered and the
    /// rejection arrives with its name (`host-no-target-designated`,
    /// `host-no-other-slot`), which is information. If anyone makes the two
    /// frontends match, let it be by moving the terminal here.
    pub(super) fn facts(&self) -> norte_frontend::availability::Facts {
        let slot = self.slot();
        // `cursor_entry`, the door for DESCRIBING: with a quick filter set
        // the raw cursor does not move and the pointed-to row is a different
        // one, so indexing `entries()` by it described an entry that is not
        // the one the reader has in front of them.
        let entry = slot.pane.cursor_entry();
        norte_frontend::availability::Facts {
            // The same as what `Activate` navigates, and through the shared
            // spot: a container and a symlink are entered the same as a
            // directory (ADR 0077). Asking here by `kind == Dir` used to dim
            // `enter` over a `.zip` that the key opens without a problem.
            enterable: entry.is_some_and(|e| norte_frontend::nav::enter_target(e).is_some()),
            // The SAME thing `request_visor` refuses, which only refuses a
            // directory: a symlink opens in the viewer without a problem,
            // and dimming F3 over one said "this does not apply" about a key
            // that works. The two spots move together.
            viewable: entry.is_some_and(|e| e.kind != EntryKind::Dir),
            rename_single: slot.pane.marks_len() <= 1,
            source_read_only: self.solo_read(self.active()),
            dest_read_only: self.slot_dest().is_ok_and(|dest| self.solo_read(dest)),
            // `degraded` in the table means the session runs UNENCRYPTED,
            // which is none of the three states this host projects
            // (connected, retrying, lost). While the connection's wire does
            // not reach this far, the honest answer is that it is unknown.
            degraded: false,
            // The host speaks through the SDK against the daemon, which is
            // the one carrying the journal (ADR 0066): what mutates here is
            // recorded and can be undone.
            journalled: true,
            // And for the same reason there is a daemon to share the session
            // with: this window has no other arm (phase 9).
            daemon: true,
            // A window is being painted, so there is a desktop where the
            // other half of the handoff can open. Asking the environment
            // here would be asking whether the thing being used exists.
            windowed: true,
        }
    }

    /// Re-freezes help's facts if it is open, and returns its patch (#262).
    ///
    /// The freezing in `open_help` guards against the READER moving, not
    /// against the world moving. Two of the facts — `enterable` and
    /// `viewable` — describe the entry under the cursor, and a copy or a
    /// delete that finish with help in front re-list the pane underneath:
    /// the reason phrase kept explaining why something did not apply to a
    /// selection that no longer exists. There was no wrong dispatch —
    /// `activate_in_help` asks again before running — but a screen that
    /// explains something false is a screen that lies.
    pub(super) fn refreeze_help(&mut self) -> Option<BridgeEnvelope<UiUpdate>> {
        if !self.refreeze_help_facts() {
            return None;
        }
        let change = ViewChange::Help {
            help: self.vista_help(),
        };
        Some(self.parche(vec![change]))
    }

    /// Re-freezes and does NOT build a patch. For paths that already send a
    /// snapshot: `parche` spends a sequence number, and dropping the
    /// envelope after spending it leaves a GAP in the sequence — exactly the
    /// condition that forces the renderer to request a full snapshot.
    ///
    /// Returns whether help was open AND its facts have CHANGED.
    pub(super) fn refreeze_help_facts(&mut self) -> bool {
        if self.help.is_none() {
            return false;
        }
        let facts = self.facts();
        self.help.as_mut().is_some_and(|a| a.refreeze(facts))
    }

    /// One part of a verdict's detail, in separate LINES (#273).
    ///
    /// The cause and the name go on different lines, which is how this
    /// surface separates them OUT of band: composing them into one let a
    /// file named `✗ 4. ya exists: other.txt` fabricate a list entry that
    /// does not exist. The name travels alone and with its mark, the only
    /// thing a third party controls.
    pub(super) fn detail_lines(
        &self,
        part: &norte_frontend::DetailPart,
    ) -> Vec<crate::dto::DialogLine> {
        use norte_frontend::DetailPart;
        let plana = |text: String| crate::dto::DialogLine {
            text: clamp_display(text),
            hostile: false,
        };
        match part {
            DetailPart::Temp { count } => vec![plana(norte_i18n::ta_in(
                self.lang,
                "modal-rename-batch-temp",
                &[("n", &count.to_string())],
            ))],
            DetailPart::Collision {
                index,
                kind_key,
                name,
                hostile,
            } => {
                let kind = norte_i18n::t_in(self.lang, kind_key);
                let cause = match index {
                    Some(n) => norte_i18n::ta_in(
                        self.lang,
                        "modal-rename-batch-collision-prefix",
                        &[("n", &n.to_string()), ("kind", &kind)],
                    ),
                    None => norte_i18n::ta_in(
                        self.lang,
                        "modal-rename-batch-collision-prefix-unindexed",
                        &[("kind", &kind)],
                    ),
                };
                vec![
                    plana(cause),
                    crate::dto::DialogLine {
                        text: clamp_display(name.clone()),
                        hostile: *hostile,
                    },
                ]
            }
            DetailPart::More {
                shown,
                total,
                hostile,
            } => vec![crate::dto::DialogLine {
                text: clamp_display(norte_i18n::ta_in(
                    self.lang,
                    "modal-rename-batch-collision-more",
                    &[("shown", &shown.to_string()), ("total", &total.to_string())],
                )),
                // The summary carries no name, but it DOES carry the mark
                // that one of the hidden ones has a hostile one: what is
                // hidden does not sneak through clean.
                hostile: *hostile,
            }],
        }
    }

    /// Help's projection.
    pub(super) fn vista_help(&self) -> Option<crate::dto::HelpView> {
        let a = self.help.as_ref()?;
        Some(a.vista(self.lang, self.effects, self.visor.is_some()))
    }

    /// The keys while help is open.
    ///
    /// FIXED on purpose, like the palette's and for the same reason: the
    /// catalog has no commands for "filter this list", "switch halves" or
    /// "follow this link". They are the ones help itself announces in its
    /// footer (`help-hint-gui`), and that string and this `match` change
    /// together.
    pub(super) fn key_in_help(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // `Ctrl+P` leaves help for the palette, which is what its footer
        // promises. BOTH changes travel in the same patch: a renderer that
        // only received the palette one would keep painting help
        // underneath.
        if k.ctrl && !k.alt && !k.meta && k.key.eq_ignore_ascii_case("p") {
            self.help = None;
            self.palette = Some(norte_frontend::palette_state::Palette::with_recent(
                self.palette_rows(),
                &self.palette_recent,
            ));
            self.request_plugin_rows(backend, mailbox);
            let changes = vec![
                ViewChange::Help { help: None },
                ViewChange::Palette {
                    palette: self.vista_palette(),
                },
            ];
            return (self.applied(), vec![self.parche(changes)]);
        }
        // WHILE FILTERING, keys are LETTERS: resolving them through the
        // keymap would turn typing "document" into open, close and filter.
        // Only `Escape`, `Backspace` and `Enter` still mean something, and
        // those three go by name because the filter is a text field.
        let filtering = self.help.as_ref().is_some_and(|a| a.state.filtering());
        let verb = if filtering { None } else { self.dialog_verb(k) };
        let Some(a) = self.help.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        match (verb.as_deref(), k.key.as_str()) {
            (Some("dialog.cancel"), _) | (None, "Escape" | "esc") => {
                // While filtering, `esc` stops filtering and does not close:
                // closing all of help for abandoning a search would lose the
                // page that was being read.
                if a.state.filtering() {
                    a.state.end_filter();
                } else {
                    self.help = None;
                }
            }
            (Some("dialog.pane"), _) => a.state.toggle_focus(),
            // The keys that SCROLL, resolved by the reader's keymap — before,
            // the renderer handled `PageDown`, `Home`, `[`… as fixed keys,
            // and rebinding them changed the terminal and not this window.
            //
            // On the SIDE the model applies them: the page walks through the
            // topics and shows one. In the BODY, no: the body crosses the
            // whole bridge and the one that scrolls it is the DOM, which is
            // the one that knows what it measures; moving it here would
            // create a SECOND truth about where help is scrolled to (#267).
            // So the host only ASKS — "one page down" — and the renderer
            // measures and scrolls (`HelpView::scroll`, bridge 76). The
            // arrows scroll the body only if there is nothing executable:
            // with actions, they pick one, same as in the terminal.
            (Some(verb @ ("dialog.down" | "dialog.up")), _)
                if a.state.focus() == norte_frontend::help::Focus::Body
                    && a.state.actions().is_empty() =>
            {
                a.scroll(if verb == "dialog.down" {
                    HelpScrollTo::LineDown
                } else {
                    HelpScrollTo::LineUp
                });
            }
            (Some("dialog.down"), _) => a.state.down(),
            (Some("dialog.up"), _) => a.state.up(),
            (
                Some(
                    verb @ ("dialog.page-down" | "dialog.page-up" | "dialog.top" | "dialog.bottom"),
                ),
                _,
            ) => {
                if a.state.focus() == norte_frontend::help::Focus::Topics {
                    match verb {
                        "dialog.page-down" => a.state.page_down(HELP_PAGE),
                        "dialog.page-up" => a.state.page_up(HELP_PAGE),
                        "dialog.top" => a.state.top(),
                        _ => a.state.bottom(),
                    }
                } else {
                    a.scroll(match verb {
                        "dialog.page-down" => HelpScrollTo::PageDown,
                        "dialog.page-up" => HelpScrollTo::PageUp,
                        "dialog.top" => HelpScrollTo::Top,
                        _ => HelpScrollTo::Bottom,
                    });
                }
            }
            // Sections belong to the BODY no matter who has focus.
            (Some("dialog.section-prev"), _) => a.scroll(HelpScrollTo::SectionPrev),
            (Some("dialog.section-next"), _) => a.scroll(HelpScrollTo::SectionNext),
            (Some("dialog.back"), _) | (None, "Backspace" | "backspace") => {
                if a.state.filtering() {
                    a.state.backspace();
                } else if !a.state.back() {
                    // At the root, "back" is close: the reader has nowhere
                    // to go back to, and a key that does nothing reads as a
                    // frozen window.
                    self.help = None;
                }
            }
            (Some("dialog.confirm"), _) | (None, "Enter" | "enter") => {
                return self.enter_in_help(backend, mailbox);
            }
            (Some("dialog.filter"), _) if !a.state.filtering() => a.state.start_filter(),
            (_, other) => {
                // A TEXT key is a code point, not a key name (`ArrowLeft` is
                // not typed), and it only counts with the filter open:
                // typing "d" while reading a page cannot start filtering on
                // its own.
                let mut chars = other.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) if a.state.filtering() && !k.ctrl && !k.alt && !k.meta => {
                        a.state.push_char(c);
                    }
                    _ => return (self.applied(), Vec::new()),
                }
            }
        }
        (self.applied(), self.help_patch(backend, mailbox))
    }

    /// `enter` over help: open the chosen page, follow a link, or run a
    /// command.
    pub(super) fn enter_in_help(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(a) = self.help.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        if a.state.focus() == norte_frontend::help::Focus::Topics {
            a.state.open_selected();
            return (self.applied(), self.help_patch(backend, mailbox));
        }
        let i = a.state.action_cursor();
        self.activate_in_help(u32::try_from(i).unwrap_or(u32::MAX), backend, mailbox)
    }

    /// Acts on body row `i`, whether from `enter` or from a click.
    ///
    /// The check for whether it CAN lives here, in the host, and not in
    /// whoever paints: the renderer does not attach a listener to a dimmed
    /// row, but the keyboard does not go through there, so delegating it let
    /// `enter` run a dimmed row.
    pub(super) fn activate_in_help(
        &mut self,
        i: u32,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let (lang, effects, hay_visor) = (self.lang, self.effects, self.visor.is_some());
        let Some(a) = self.help.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        let i = i as usize;
        let verdict = a.action_executable(i, lang, effects, hay_visor);
        // Pointing at the row is the half of the click that does NOT
        // execute, and it is done the same way: after following a link, the
        // next arrow moves from where the reader pointed.
        a.point_at(i);
        match verdict {
            Ok(action) => self.act_in_help(Some(action), backend, mailbox),
            Err(key) if key.is_empty() => {
                // A row that no longer exists: the renderer was one frame
                // behind, and that is not an error.
                (Self::stale(StaleAction::Modal), Vec::new())
            }
            Err(key) => {
                // Dimmed: the reason is stated and the page STAYS open.
                // Closing help to refuse would take away the page with the
                // explanation from the reader.
                self.status.message = Some(clamp_display(norte_i18n::t_in(self.lang, &key)));
                let changes = vec![
                    ViewChange::Status(self.status.clone()),
                    ViewChange::Help {
                        help: self.vista_help(),
                    },
                ];
                (
                    ActionAck::Unavailable { reason_key: key },
                    vec![self.parche(changes)],
                )
            }
        }
    }

    /// What a body action does, whether from `enter` or from a click.
    pub(super) fn act_in_help(
        &mut self,
        action: Option<norte_frontend::help::Action>,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match action {
            Some(norte_frontend::help::Action::Open(id)) => {
                if let Some(a) = self.help.as_mut() {
                    a.state.open(&id);
                }
                (self.applied(), self.help_patch(backend, mailbox))
            }
            Some(norte_frontend::help::Action::Run(cmd)) => {
                // Running closes help: the command acts on the listing help
                // was covering. The close travels FIRST and in its own
                // patch, so the ack the effect returns still points at the
                // update that reflects it.
                self.help = None;
                let close = self.parche(vec![ViewChange::Help { help: None }]);
                let Some(effect) = effect_of(&cmd, 1) else {
                    let (ack, mut rest) = self.no_implemented(&cmd);
                    let mut sends = vec![close];
                    sends.append(&mut rest);
                    return (ack, sends);
                };
                let (ack, mut rest) = self.apply_effect(effect, backend, mailbox);
                let mut sends = vec![close];
                sends.append(&mut rest);
                (ack, sends)
            }
            None => (self.applied(), Vec::new()),
        }
    }

    /// A click on a row in help's side panel: SHOWS whatever there is, the
    /// same thing the arrow does.
    pub(super) fn choose_page(
        &mut self,
        row: u32,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(a) = self.help.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        a.state.click_row(row as usize);
        (self.applied(), self.help_patch(backend, mailbox))
    }
}
