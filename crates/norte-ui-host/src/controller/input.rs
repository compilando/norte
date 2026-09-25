//! The keys: which verb resolves each chord, and on which screen.
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
    /// The key, when there is an open INPUT CONTEXT that keeps it.
    ///
    /// `None` = there was none (or the one there was did not want it) and the
    /// key goes its normal way: the listing's resolver.
    ///
    /// The ORDER is who covers whom. The viewer is a whole other screen; help
    /// covers the listing and the palette can be opened from it, so it goes
    /// first; the palette is a free text editor; and the incremental search
    /// only keeps TEXT keys.
    // TODO(translation): review — this paragraph describes the general
    /// input-context precedence order, but the item right after it is
    /// `key_in_dialog`'s own doc, about dialog keys specifically; it looks
    /// like a stale fragment left by an earlier edit.
    /// A dialog's keys: answering it or cancelling it, and nothing else.
    ///
    /// TEXT does not go through here. It is typed in the renderer's field and
    /// arrives via `dialog_input`, which is what lets the approved bytes be
    /// what was typed and not a reconstruction from loose keys.
    ///
    /// `Enter` chooses the first NON-destructive answer, so on an agent's
    /// approval dialog it chooses `deny`: approving a mutation nobody asked
    /// for cannot be what happens from leaving a finger on Enter.
    ///
    /// A key that is neither of the two is EATEN just the same: a modal that
    /// lets through a key it does not understand is not a modal.
    pub(super) fn key_in_dialog(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(d) = self.dialogs.last() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        let id = d.id;
        let typing = d.vista.input.is_some();
        // TWO REGIMES, the same pair as the TUI's and the rest of this host's
        // fields. With a field open the keys are LETTERS: resolving them
        // through the keymap would turn typing a file name into answering the
        // question, because there is no `dialog.*` verb for "type a letter".
        // Without a field, the key goes through the SHARED resolver, which is
        // what makes a preset that rebinds `dialog.confirm` change this
        // window and not just the TUI.
        let verb = if typing {
            match k.key.as_str() {
                "Enter" | "enter" => Some("dialog.confirm"),
                "Escape" | "esc" => Some("dialog.cancel"),
                _ => None,
            }
        } else {
            let Ok(chord) = k.to_chord() else {
                return (self.applied(), Vec::new());
            };
            match self.resolver_dialog.push(chord) {
                Resolution::Run { command, .. } => match command.as_str() {
                    "dialog.confirm" => Some("dialog.confirm"),
                    "dialog.cancel" => Some("dialog.cancel"),
                    "dialog.approve" => Some("dialog.approve"),
                    "dialog.deny" => Some("dialog.deny"),
                    // The four outcomes of a collision (#287). Each one names
                    // ITS answer: `dialog.confirm` over a collision chooses
                    // none, because "confirm" does not say which of the
                    // four, and the one chosen by elimination is the
                    // destructive one.
                    "dialog.overwrite" => Some("dialog.overwrite"),
                    "dialog.skip" => Some("dialog.skip"),
                    "dialog.rename" => Some("dialog.rename"),
                    "dialog.newer" => Some("dialog.newer"),
                    _ => None,
                },
                _ => None,
            }
        };
        let Some(d) = self.dialogs.last() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        // The VERB chooses among the answers THIS dialog offers: one it does
        // not offer is not interpreted — there are no implicit answers on a
        // decision surface — and that is why `dialog.confirm` over an
        // approval does not approve: an approval's affirmative is named
        // `approve` on purpose, so a renderer cannot mix them up.
        let chosen = match verb {
            Some("dialog.confirm") => d.vista.choices.iter().find(|c| c.id == "confirm"),
            Some("dialog.approve") => d.vista.choices.iter().find(|c| c.id == "approve"),
            Some("dialog.deny") => d.vista.choices.iter().find(|c| c.id == "deny"),
            // The collision's, each by its own name. A dialog that does not
            // offer them ignores them, which is the usual rule: there are no
            // implicit answers here.
            Some("dialog.overwrite") => d.vista.choices.iter().find(|c| c.id == "overwrite"),
            Some("dialog.skip") => d.vista.choices.iter().find(|c| c.id == "skip"),
            Some("dialog.rename") => d.vista.choices.iter().find(|c| c.id == "rename"),
            Some("dialog.newer") => d.vista.choices.iter().find(|c| c.id == "newer"),
            Some("dialog.cancel") => d
                .vista
                .choices
                .iter()
                .find(|c| c.id == "cancel" || c.id == "deny")
                // Closing is ALWAYS possible: if the dialog offers neither
                // cancel nor deny, the answer is the last non-destructive
                // one.
                .or_else(|| d.vista.choices.iter().rfind(|c| !c.destructive)),
            _ => None,
        }
        .map(|c| c.id.clone());
        let Some(choice) = chosen else {
            return (self.applied(), Vec::new());
        };
        // No secret, and it is not an oversight (#327): a KEY cannot carry a
        // password. Over a dialog that asks for one, this path confirms with
        // `None`, i.e. inertly, and the only door that delivers it is the
        // renderer's — the button and the field's own Enter — which does have
        // the value. That is what is wanted: the host does not store what was
        // typed, so a chord cannot deliver something the host does not have.
        self.responder_dialog(id, &choice, None, backend, mailbox)
    }

    /// A key's `dialog.*` verb, through the SHARED resolver (#287).
    ///
    /// It is the only door: this window's modal surfaces used to handle fixed
    /// keys, so a preset that rebound `dialog.up` changed the TUI and not the
    /// window — exactly the drift the common catalogue exists to not have.
    ///
    /// `None` when the key does not form a chord, is not bound, or opens a
    /// sequence still unresolved. In all three cases the surface does
    /// nothing, which is what it used to do with a key it did not
    /// understand.
    pub(super) fn dialog_verb(&mut self, k: &crate::keys::KeyInput) -> Option<String> {
        let chord = k.to_chord().ok()?;
        match self.resolver_dialog.push(chord) {
            Resolution::Run { command, .. } => Some(command),
            _ => None,
        }
    }

    /// The chord bound to a `dialog.*` verb, already painted. Empty if none.
    ///
    /// For modal surfaces' FOOTERS: they are painted with what the keymap
    /// says, not with a translated literal, because the literal stops being
    /// true the moment someone rebinds the key.
    pub(super) fn dialog_chord(&self, command: &str) -> String {
        self.resolver_dialog
            .effective()
            .bindings()
            .into_iter()
            .find(|(_, c)| *c == command)
            .map(|(seq, _)| norte_frontend::keymap::paint_chord(&seq))
            .unwrap_or_default()
    }

    /// Is there a screen in front that would keep a key before the listing?
    /// The SAME ones [`Self::key_of_an_overlay`] handles, except the menu,
    /// which belongs to whoever asks.
    ///
    /// It is a second list on purpose and not a detour through that function:
    /// that one HANDLES the key (cancels a viewer read in flight, abandons a
    /// plan), and asking cannot have effects. Whoever adds an overlay there
    /// adds it here too;
    /// `alt_alone_does_not_open_the_menu_over_a_dialog` pins the case that
    /// matters.
    pub(super) fn something_keeps_the_keys(&self) -> bool {
        !self.dialogs.is_empty()
            || self.wizard.is_some()
            || self.desktop.output.is_some()
            || self.desktop.program.is_some()
            || self.help.is_some()
            || self.sync.is_some()
            || self.comparison.is_some()
            || self.revision_ia.is_some()
            || self.search.is_some()
            || self.selector_layout.is_some()
            || self.selector_columns.is_some()
            || self.selector.is_some()
            || self.selector_profile.is_some()
            || self.theme_chosen.is_some()
            || self.extensions.is_some()
            || self.agency.panel
            || self.settings.is_some()
            || self.visor.is_some()
            || self.palette.is_some()
            || self.ir_a.is_some()
    }

    pub(super) fn key_of_an_overlay(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> Option<(ActionAck, Vec<BridgeEnvelope<UiUpdate>>)> {
        // The DIALOG comes before everything else: it is the only truly modal
        // surface — a question that must be answered before continuing —
        // and keys it did not trap used to fall through to the listing
        // UNDERNEATH. With a name prompt open, `Backspace` navigated to the
        // parent while typing and `Enter` entered the directory under the
        // cursor instead of confirming; with a delete confirmation open,
        // `Enter` navigated the screen the question was covering.
        if !self.dialogs.is_empty() {
            return Some(self.key_in_dialog(k, backend, mailbox));
        }
        // The first-run wizard (spec 2026-09-10): behind the dialog, which is
        // a security question, and in front of everything else — it is
        // asking which keys to have, so its own are fixed.
        if self.wizard.is_some() {
            return Some(self.key_in_wizard(k, backend, mailbox));
        }
        // An extension command's OUTPUT keeps ALL the keys while it is up: it
        // paints full screen, so a modal that let through a key it does not
        // understand is not a modal. `Enter` and `Escape` close it — both,
        // because closing a read-only panel with `Enter` is the reflex; the
        // rest mean nothing here and do not fall through to what is
        // underneath, where a delete confirmation could be waiting for a yes
        // the reader cannot see. The PLUGIN chooses the moment, deciding when
        // its command answers.
        if self.desktop.output.is_some() {
            if matches!(k.key.as_str(), "Escape" | "esc" | "Enter" | "enter") {
                return Some(self.close_output());
            }
            return Some((self.applied(), Vec::new()));
        }
        // A program's output (#312), for the same reason and with the same
        // keys: it is read and closed.
        if self.desktop.program.is_some() {
            if matches!(k.key.as_str(), "Escape" | "esc" | "Enter" | "enter") {
                return Some(self.close_program_output());
            }
            return Some((self.applied(), Vec::new()));
        }
        // HELP goes first, even before the viewer, and not out of taste: it
        // opens ON TOP of whatever was there — also on top of the viewer,
        // which is where the viewer's help page is requested from — and
        // whoever is on top keeps the keys. The other way around, `F1` in the
        // viewer opened a help screen that received not a single key and that
        // none could close.
        if self.help.is_some() {
            return Some(self.key_in_help(k, backend, mailbox));
        }
        // The REVIEW of a rename plan goes before the rest of the overlays
        // and only after the dialog and help: it is a screen read in full
        // before approving a mutation, and a key that slipped through to the
        // listing underneath would move the cursor under a plan that is still
        // waiting for a yes.
        // The sync panel, same as the diff one: while it is open it keeps the
        // keys.
        if self.sync.is_some() {
            return Some(self.key_in_sync(k, backend, mailbox));
        }
        // The diff panel, when open, keeps the keys: it is a whole screen,
        // and an arrow that slipped through it would move the listing
        // underneath.
        if self.comparison.is_some() {
            return Some(self.key_in_comparison(k, backend, mailbox));
        }
        if self.revision_ia.is_some() {
            return Some(self.key_in_ai_review(k, backend, mailbox));
        }
        // Phase 8: the organize tree keeps the keys for the same reason as
        // the review next door — it is a whole screen and it is approved with
        // them.
        if self.revision_organize.is_some() {
            return Some(self.key_in_organize_review(k, backend, mailbox));
        }
        if self.search.is_some() {
            return Some(self.key_in_search(k, backend, mailbox));
        }
        if self.selector_layout.is_some() {
            return Some(self.key_in_layouts(k, backend, mailbox));
        }
        if self.selector_columns.is_some() {
            return Some(self.key_in_columns(k, backend, mailbox));
        }
        if self.selector.is_some() {
            return Some(self.key_in_selector(k, backend, mailbox));
        }
        if self.selector_profile.is_some() {
            return Some(self.key_in_profiles(k, backend, mailbox));
        }
        if self.theme_chosen.is_some() {
            return Some(self.key_in_theme(k, mailbox));
        }
        if self.extensions.is_some() {
            return Some(self.key_in_extensions(k, backend, mailbox));
        }
        if self.agency.panel {
            return Some(self.key_in_agents(k, backend, mailbox));
        }
        if self.settings.is_some() {
            return Some(self.key_in_settings(k, mailbox));
        }
        if self.visor.is_some() {
            return Some(self.key_in_viewer(k, backend, mailbox));
        }
        // Any LISTING key cancels a viewer read in flight. The user pressed
        // F3, got tired of waiting and moved on to something else: opening
        // the viewer half a second later is opening a window nobody asked
        // for anymore — and switching their keyboard's map with no gesture of
        // theirs. (A second F3 requests its own read and keeps the new
        // token.)
        self.viewer_in_flight = None;
        // The open menu keeps the keys, same as the palette: an arrow that
        // slipped through it would move the listing underneath.
        if self.menu.is_some() {
            return Some(self.key_in_menu(k, backend, mailbox));
        }
        if self.palette.is_some() {
            return Some(self.key_in_palette(k, backend, mailbox));
        }
        // "Go to" (#357), for the same reason as the palette: it is a free
        // text editor, and a letter that slipped through it would act on the
        // listing.
        if self.ir_a.is_some() {
            return Some(self.key_in_goto(k, backend, mailbox));
        }
        if self.slot().pane.quick().is_some() {
            return self.key_in_quick(k);
        }
        // And, when NOBODY else wanted it, `Escape` abandons a rename plan
        // still thinking. It goes LAST, which is the only position where
        // "nobody wanted it" is true: above it, it ate the `Escape` that
        // closes the palette and the one that cancels the quick filter — one
        // key doing two things badly at once — and it skipped the viewer's
        // in-flight cut.
        //
        // And only `Escape`, not any key like the viewer: the model really
        // does take a while, and continuing to navigate while it thinks is
        // normal. What cannot happen is the plan opening on the screen half a
        // minute after its owner has moved on to something else.
        if self.ai_in_flight.is_some() && (k.key == "Escape" || k.key == "esc") {
            self.epoch_ia += 1;
            self.ai_in_flight = None;
            self.status.message = Some(clamp_display(norte_i18n::t_in(
                self.lang,
                "host-plan-abandoned",
            )));
            let change = ViewChange::Status(self.status.clone());
            return Some((self.applied(), vec![self.parche(vec![change])]));
        }
        None
    }

    /// A key with the splash screen up, or `None` if it is not.
    ///
    /// `1`..`9` OPENS the row carrying that number — it is what the screen
    /// promises in its footer, and without this the rows were painted
    /// numbered and the number did nothing; any other key removes it and
    /// means nothing else. Typing onto the listing behind it would be acting
    /// on something the reader is not looking at. The terminal resolves the
    /// same thing in `norte-tui/src/splash.rs`.
    fn key_in_splash(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> Option<(ActionAck, Vec<BridgeEnvelope<UiUpdate>>)> {
        self.splash.as_ref()?;
        // A digit WITH a modifier is not the shortcut: `ctrl+1` is a keymap
        // chord on any other screen, and here it would be a coincidence.
        let digit = (k.key.chars().count() == 1 && !k.ctrl && !k.alt && !k.meta)
            .then(|| k.key.chars().next())
            .flatten()
            .and_then(|c| c.to_digit(10))
            .filter(|n| *n >= 1);
        if let Some(n) = digit {
            let number = u8::try_from(n).unwrap_or(0);
            return Some(self.activate_splash_row(number, backend, mailbox));
        }
        Some((self.applied(), self.close_splash()))
    }

    /// A key: the SHARED keymap resolves it and the host only executes.
    ///
    /// The four outcomes are the resolver's, and none stays silent: a command
    /// runs, a half-finished prefix or count are PAINTED (what is not seen
    /// cannot be cancelled), a key bound to something that cannot be done
    /// here says so, and a key with no binding is discarded leaving the state
    /// clean.
    pub(super) fn key(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // The splash screen is removed by WHATEVER KEY, and that key does
        // nothing else (ADR 0115). It goes first of all because whatever is
        // in front rules: typing the key onto the listing behind it would be
        // acting on something the reader is not looking at. The terminal does
        // the same (`norte-tui/src/splash.rs`).
        if let Some(outcome) = self.key_in_splash(k, backend, mailbox) {
            return outcome;
        }
        if let Some(outcome) = self.key_of_an_overlay(k, backend, mailbox) {
            return outcome;
        }
        // The DOCKED viewer with focus keeps the viewer's keys (#291), as in
        // the TUI: it is the same viewer somewhere else. Whatever its keymap
        // does not bind — the tab key, a global shortcut — goes its normal
        // way.
        if let Some(outcome) = self.key_in_preview(k) {
            return outcome;
        }
        // The TERMINAL panel with focus keeps the BYTES (#362), and this arm
        // does not look like the ones above: the others translate keys into
        // commands, and here everything is handed to a shell — arrows, tab,
        // F5, ctrl+c — because inside a shell that is what they mean.
        //
        // With ONE exception, which is the door: the lone chord that opened
        // the panel takes you out. The byte table is SHARED with the
        // terminal.
        if let Some(outcome) = self.key_in_terminal(k, backend, mailbox) {
            return outcome;
        }
        let Ok(chord) = k.to_chord() else {
            // A key the adapter does not understand is not guessed at.
            return (
                ActionAck::Unavailable {
                    reason_key: "host-key-unmapped".to_owned(),
                },
                Vec::new(),
            );
        };
        // Read BEFORE the push: a miss that breaks a half-typed sequence is
        // not a letter typed onto the listing.
        let idle = self.resolver.pending().is_empty() && self.resolver.count().is_none();
        match self.resolver.push(chord) {
            Resolution::Run { command, count } => {
                let times = count.times();
                let Some(effect) = effect_of(&command, times) else {
                    // In the catalogue, bound, and this host does not do it.
                    // Said with the SAME phrase as the TUI.
                    let phrase = norte_frontend::keymap::unavailable_message_in(
                        &command,
                        Availability::NotHere,
                        self.lang,
                    );
                    self.status.message = Some(clamp_display(phrase));
                    self.status.pending = None;
                    let change = ViewChange::Status(self.status.clone());
                    return (
                        ActionAck::Unavailable {
                            reason_key: "cmd-not-here".to_owned(),
                        },
                        vec![self.parche(vec![change])],
                    );
                };
                self.status.pending = None;
                // The sequence closed: the continuations panel describes keys
                // that are no longer live, and its own contract says it is
                // dropped as soon as the resolver's state changes.
                self.whichkey = None;
                self.apply_effect(effect, backend, mailbox)
            }
            Resolution::Pending(_) | Resolution::Counting(_) => {
                self.status.pending = Some(PendingView {
                    chords: self
                        .resolver
                        .pending()
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(" "),
                    count: self.resolver.count(),
                });
                // The panel is built HERE, on the transition, and not while
                // projecting: `build` costs several strings and one or two
                // Fluent formats per row.
                self.whichkey = Some(norte_frontend::whichkey::WhichKeyRows::build(
                    &self.effective,
                    self.resolver.pending(),
                    self.resolver.count(),
                    self.lang,
                ));
                let changes = vec![
                    ViewChange::Status(self.status.clone()),
                    ViewChange::WhichKey {
                        whichkey: self.vista_whichkey(),
                    },
                ];
                (self.applied(), vec![self.parche(changes)])
            }
            Resolution::Unavailable { command, why } => {
                let phrase =
                    norte_frontend::keymap::unavailable_message_in(&command, why, self.lang);
                self.status.message = Some(clamp_display(phrase));
                self.status.pending = None;
                self.whichkey = None;
                let change = ViewChange::Status(self.status.clone());
                (
                    ActionAck::Unavailable {
                        reason_key: match why {
                            Availability::Here => "cmd-here",
                            Availability::NotBuilt { .. } => "cmd-not-built",
                            Availability::NotHere => "cmd-not-here",
                        }
                        .to_owned(),
                    },
                    vec![self.parche(vec![change])],
                )
            }
            Resolution::Reset => self.key_missed(idle, chord),
        }
    }

    /// A key no binding took. `idle` = nothing was half-typed before it.
    fn key_missed(
        &mut self,
        idle: bool,
        chord: norte_frontend::keymap::Chord,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // `type_to_search` (ADR 0155): a letter no binding took opens a
        // quick search that jumps to the names starting with it — the
        // terminal does the same. Only with a LISTING focused: `slot_mut`
        // falls back to the last listing, and a side panel's miss would open
        // a search in a pane the reader is not in.
        let listing_focused = self
            .roles
            .get(RoleId::Active)
            .is_some_and(|SlotId(id)| self.slots.contains_key(&id));
        if listing_focused && let Some(c) = self.resolver.effective().typed_search_char(idle, chord)
        {
            self.slot_mut().pane.type_to_search(c);
            if self.slot().pane.quick().is_some() {
                return (self.applied(), vec![self.parche_rows()]);
            }
        }
        let had_pending = self.status.pending.take().is_some();
        let had_panel = self.whichkey.take().is_some();
        if had_pending || had_panel {
            let changes = vec![
                ViewChange::Status(self.status.clone()),
                ViewChange::WhichKey { whichkey: None },
            ];
            return (self.applied(), vec![self.parche(changes)]);
        }
        (self.applied(), Vec::new())
    }

    /// The keys while the palette is open.
    ///
    /// Fixed on purpose: `esc` closes, `enter` runs what is selected, the
    /// arrows move and everything else types. It is the same thing the TUI
    /// does, and for the same reason — the catalogue has no commands for
    /// this.
    // TODO(translation): review — this paragraph documents the palette's
    /// keys, but the item right after it is `key_in_menu`'s doc, about the
    /// menu's keys; it looks like a stale fragment left by an earlier edit.
    /// Open menu keys.
    ///
    /// FIXED, like the palette's and for the same reason: there are no
    /// `dialog.*` verbs for "next menu item", so they cannot come from the
    /// keymap either. The arrows walk, `Enter` runs and `Escape` closes; any
    /// other is discarded instead of falling through to the listing
    /// underneath, which would be acted on for a screen the reader is not
    /// looking at.
    pub(super) fn key_in_menu(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(menu_state) = self.menu.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        match k.key.as_str() {
            "Escape" | "esc" => self.forget_menu(),
            "ArrowLeft" | "left" => menu_state.cycle_menu(-1),
            "ArrowRight" | "right" => menu_state.cycle_menu(1),
            "ArrowUp" | "up" => menu_state.cycle_item(-1),
            "ArrowDown" | "down" => menu_state.cycle_item(1),
            "Enter" | "enter" => {
                let chosen = menu_state.selected();
                return self.run_from_menu(chosen, backend, mailbox);
            }
            _ => return (self.applied(), Vec::new()),
        }
        let change = ViewChange::Menu {
            menu: self.vista_menu(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// Closes the menu and runs what was chosen.
    ///
    /// The close travels in its OWN patch and BEFORE the effect, for the same
    /// reason as the palette: the command can open another screen, and doing
    /// it behind the menu would leave it eating the keys of the one that just
    /// opened.
    pub(super) fn run_from_menu(
        &mut self,
        chosen: Option<&'static str>,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.forget_menu();
        let closing = self.parche(vec![ViewChange::Menu {
            menu: self.vista_menu(),
        }]);
        let Some(cmd) = chosen else {
            return (self.applied(), vec![closing]);
        };
        // Through the SAME path as a key: a menu is another door into the
        // catalogue, not a second dispatcher.
        let (ack, mut rest) = match effect_of(cmd, 1) {
            Some(effect) => self.apply_effect(effect, backend, mailbox),
            None => self.no_implemented(cmd),
        };
        let mut outgoing = vec![closing];
        outgoing.append(&mut rest);
        (ack, outgoing)
    }

    pub(super) fn key_in_palette(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(palette) = self.palette.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        let height = 10;
        match k.key.as_str() {
            "Escape" | "esc" => {
                self.palette = None;
            }
            "Enter" | "enter" => {
                let chosen = palette.selected();
                self.palette = None;
                if let Some(cmd) = chosen {
                    norte_frontend::session::note_palette_recent(&mut self.palette_recent, &cmd);
                    // The close travels in its OWN patch and before the
                    // effect. Without it, a renderer that applies patches —
                    // which is what the reference one does — received the
                    // command's change and none of the palette's, and kept
                    // painting it over the listing until the next snapshot.
                    let closing = self.parche(vec![ViewChange::Palette { palette: None }]);
                    // It runs through the SAME path as a key: the palette is
                    // another door into the catalogue, not a second
                    // dispatcher.
                    let (ack, mut rest) = match effect_of(&cmd, 1) {
                        Some(effect) => self.apply_effect(effect, backend, mailbox),
                        // A PLUGIN row is not in the command catalogue and
                        // cannot be: a third party contributes it at
                        // runtime.
                        None if cmd.starts_with("plugin:") => {
                            self.run_from_plugin(&cmd, backend, mailbox)
                        }
                        // A RENAMER row (C3, ADR 0095): requests the plan and
                        // puts it into the SAME review as the AI's.
                        None if cmd.starts_with("renamer:") => {
                            self.run_from_renamer(&cmd, backend, mailbox)
                        }
                        // An ORGANIZER row (phase 8): the same dispatch,
                        // another method, and the plan lands on the same
                        // reviewable tree as the model's.
                        None if cmd.starts_with("organizer:") => {
                            match norte_frontend::palette::parse_organizer_key(&cmd) {
                                Some((id, org)) => {
                                    let (id, org) = (id.to_owned(), org.to_owned());
                                    self.request_organize_plan(Some((id, org)), backend, mailbox)
                                }
                                None => self.no_implemented(&cmd),
                            }
                        }
                        None => self.no_implemented(&cmd),
                    };
                    let mut outgoing = vec![closing];
                    outgoing.append(&mut rest);
                    return (ack, outgoing);
                }
            }
            "ArrowDown" | "down" => palette.down(),
            "ArrowUp" | "up" => palette.up(),
            "PageDown" | "pgdn" => palette.page_down(height),
            "PageUp" | "pgup" => palette.page_up(height),
            "Backspace" | "backspace" => palette.backspace(),
            other => {
                // A TEXT key is a code point, not a UTF-16 unit nor a key
                // name: `ArrowLeft` is not typed.
                let mut chars = other.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) if !k.ctrl && !k.alt && !k.meta => palette.push_char(c),
                    _ => return (self.applied(), Vec::new()),
                }
            }
        }
        let change = ViewChange::Palette {
            palette: self.vista_palette(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// A catalogue command this host does not execute, said with the same
    /// phrase as the TUI.
    pub(super) fn no_implemented(
        &mut self,
        cmd: &str,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let phrase =
            norte_frontend::keymap::unavailable_message_in(cmd, Availability::NotHere, self.lang);
        self.status.message = Some(clamp_display(phrase));
        let change = ViewChange::Status(self.status.clone());
        (
            ActionAck::Unavailable {
                reason_key: "cmd-not-here".to_owned(),
            },
            vec![self.parche(vec![change])],
        )
    }
}
