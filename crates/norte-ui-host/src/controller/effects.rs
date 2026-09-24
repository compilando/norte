//! Resolving an `Effect` from the shared catalogue.
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
    /// Runs what a command asks over the focused slot.
    ///
    /// It is the SAME path the renderer's direct actions take (a click, a
    /// drag): a key and a gesture that mean the same thing doing the same
    /// thing cannot depend on someone remembering to keep them in sync.
    ///
    /// **It is a DISPATCHER, and that is why it grows one line per new
    /// gesture.** What the lint measures here says nothing about its
    /// complexity: each arm is a name and a call, and the exhaustive `match`
    /// is exactly what makes adding an `Effect` without handling it a
    /// compile error. Splitting the arms into functions to get under the
    /// threshold hides that split in a second place without improving
    /// anything — it has already been done three times, and all three times
    /// the next gesture brushed against it again. The groups that DO mean
    /// something — what opens, what lays out, what acts on entries — are
    /// grouped; the rest stays here in plain view.
    #[expect(
        clippy::too_many_lines,
        reason = "exhaustive dispatcher: one arm per gesture, no logic inside"
    )]
    pub(super) fn apply_effect(
        &mut self,
        effect: Effect,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // Focus can be on a panel that is NOT a listing and that DOES take
        // keys — today, the process one. Then "down" is going down THROUGH
        // IT: until now the `active` role painted it focused while the
        // arrows moved the listing next to it, which is half a function and
        // the half that is not visible.
        //
        // It is decided by EFFECT and not by key, so `j`, `↓` and `g g` work
        // the same: the keymap says which command it is, and the surface
        // with focus says what it means there.
        // Cancel is decided BEFORE anything: with the process panel focused
        // and the board empty, `effect_on_focused_pane` answers "applied"
        // to any effect, and that would turn "there is nothing to stop" into
        // silence.
        if matches!(effect, Effect::CancelTask) {
            return self.cancel_by_command();
        }
        if matches!(effect, Effect::PauseTask) {
            return self.pause_by_command(mailbox);
        }
        if matches!(effect, Effect::RetryTask) {
            return self.retry_by_command(backend, mailbox);
        }
        if matches!(effect, Effect::ToggleCola) {
            return self.toggle_cola();
        }
        if let Effect::MoverEnCola { up } = effect {
            return self.move_in_queue_by_command(up, mailbox);
        }
        // Walking and discarding the board, for the same reason and before
        // focus: they are BOARD commands, not the panel that paints it, and
        // with the process panel closed they still have to mean the same
        // thing.
        if let Effect::TaskVecina { back: going_back } = effect {
            return self.move_on_board(going_back);
        }
        if matches!(effect, Effect::DiscardTask) {
            return self.discard_task();
        }
        if let Some(outcome) = self.effect_on_focused_pane(effect) {
            return outcome;
        }
        // `Enter` on the timeline (#359) is "come back here": it asks with
        // the count before undoing anything.
        if self.the_line_has_focus() && matches!(effect, Effect::Enter) {
            return self.ask_undo_until();
        }
        if self.places_have_focus() && matches!(effect, Effect::Enter | Effect::Mark) {
            // Entering and collapsing are handled by the side bar, and the
            // `cd` that comes out goes to the LISTING through the same path
            // as any other: that is what makes having it open not change
            // where operations go.
            return self.activate_place_at_cursor(backend, mailbox);
        }
        let slot = self.active();
        match effect {
            Effect::Cursor(_)
            | Effect::Page(_)
            | Effect::Extremo { .. }
            | Effect::Enter
            | Effect::Up
            | Effect::Trail { .. }
            | Effect::Mark
            | Effect::MarkAll
            | Effect::InvertMarks
            | Effect::MarkExtension { .. }
            | Effect::MarkClass { .. }
            | Effect::RestoreMarks
            | Effect::MarkSubiendo
            | Effect::MarkPage { .. }
            | Effect::MarkToEdge { .. }
            | Effect::UnmarkAll => self.listing_effect(effect, slot, backend, mailbox),
            Effect::Focus {
                back: going_back,
                solo_listings,
            } => self.mover_focus(going_back, solo_listings, backend, mailbox),
            Effect::Dest => self.designar_dest(),
            Effect::JumpBack => self.jump_to_point(backend, mailbox),
            Effect::PinJump => self.set_jump_point(),
            // Handled above, before the focused panel. The arm exists
            // because the `match` is exhaustive on purpose: a new effect
            // with no place has to be a compile error.
            // The BOARD's three are handled before getting here: they do not
            // depend on which panel has focus.
            Effect::CancelTask | Effect::TaskVecina { .. } | Effect::DiscardTask => {
                self.cancel_by_command()
            }
            Effect::PauseTask => self.pause_by_command(mailbox),
            Effect::RetryTask => self.retry_by_command(backend, mailbox),
            Effect::ToggleCola => self.toggle_cola(),
            Effect::MoverEnCola { up } => self.move_in_queue_by_command(up, mailbox),
            Effect::Size(_)
            | Effect::Equalize
            | Effect::Rotate
            | Effect::Layouts
            | Effect::Split { .. }
            | Effect::CloseSlot
            | Effect::ToggleSlot { .. }
            | Effect::TabNew
            | Effect::CloseTab
            | Effect::CycleTab { .. }
            | Effect::MoverTab { .. }
            | Effect::IrAPestana { .. } => {
                self.layout_effect(effect, backend, mailbox)
            }
            Effect::Sort(col) => self.sort_by_column(slot, col.into()),
            Effect::Refresh => self.refresh_visible(backend, mailbox),
            Effect::ToggleHidden => self.toggle_hidden(),
            Effect::CycleEncoding => self.cycle_encoding(),
            Effect::Mirror | Effect::MirrorObjetivo | Effect::Bring | Effect::Swap => {
                self.pane_gesture(effect, backend, mailbox)
            }
            // Apart from the group above: those NAVIGATE, and this one only
            // flips a switch.
            Effect::MirrorPermanent => self.toggle_mirror_permanent(),
            Effect::SideVolumes { right } => {
                self.open_side_volumes(right, backend, mailbox)
            }
            Effect::Columns => self.open_columns(),
            Effect::Search => self.request_search(),
            Effect::SearchFast => self.search_fast(),
            Effect::CreateDirectory
            | Effect::CreateFile
            | Effect::Delete { .. }
            | Effect::Transferir { .. }
            | Effect::Rename
            | Effect::RenameIa
            // Phase 8: organize creates folders and moves, so a read-only
            // window does not request it either.
            | Effect::Organize
            | Effect::RenameBatch
            // #314: changing permissions writes, so a read-only window does
            // not do it either.
            | Effect::Permissions
            | Effect::SearchSemantic
            | Effect::Sync
            // The two that LAUNCH a process: what that process does with the
            // files is not this window's decision.
            | Effect::OpenExternal
            | Effect::EditExternal
            | Effect::CompareFiles
            | Effect::Terminal
            // The terminal PANEL (#362), and with more reason than
            // `Terminal`: that one launches an outside emulator, and this one
            // runs a shell INSIDE the window. In one that promises not to
            // write, this would be the widest possible back door — anything
            // at all is typed in there.
            //
            // It goes in THIS arm and not the layout one, where it used to be:
            // that one runs unconditionally, so the guard was never reached.
            // And the keymap's filter is not enough, because the panel bar's
            // button, the menu entry and the status bar's buttons call
            // `effect_of` without going through it.
            | Effect::OpenTerminal
            // Phase 9: the handoff writes the session, releases it and closes
            // the window. None of the three are done by a read-only one.
            | Effect::Handoff
                if self.effects == crate::commands::Effects::SoloRead =>
            {
                Self::no_mutates()
            }
            // And outside read-only, the panel opens. It goes here and not
            // with the layout because from there the guard above is not
            // reached.
            Effect::OpenTerminal => self.open_terminal_panel(backend, mailbox),
            // Copying the path touches nothing and goes in both modes:
            // putting text on the clipboard is as read-only as reading a
            // name.
            Effect::CopyPath => self.copy_paths(),
            Effect::Checksums { verify } => self.launch_checksums(verify, backend, mailbox),
            Effect::MarkPatron { mark } => self.request_patron(mark),
            Effect::OpenExternal => self.open_external(),
            Effect::EditExternal => self.edit_external(),
            Effect::CompareFiles => self.compare_files(),
            Effect::Terminal => self.open_terminal(),
            Effect::Handoff => self.request_handoff(backend, mailbox),
            Effect::Compare => self.request_comparison(backend, mailbox),
            Effect::Disconnect => self.disconnect(backend, mailbox),
            Effect::DirectorySize
            | Effect::Pack
            | Effect::Unpack
            | Effect::CheckArchive
            | Effect::SplitFile
            | Effect::Join => self.effect_over_entries(effect, backend, mailbox),
            // Like comparing: it needs the backend because it goes out to ask
            // as soon as it opens, and the panel is born saying it is
            // planning.
            Effect::Sync => self.request_sync(backend, mailbox),
            Effect::Palette
            | Effect::IrA
            | Effect::Help
            | Effect::Settings
            | Effect::Extensions
            | Effect::Agents
            | Effect::Theme
            | Effect::Menu
            | Effect::Exit
            | Effect::ProfileChoose
            | Effect::ProfileSaveAs
            | Effect::ProfileVecino { .. }
            | Effect::Volumes
            | Effect::Connections
            // History and the hotlist are two other selectors: they go with
            // the rest of what OPENS, and not each with its own arm — this
            // `match` dispatches, and grows one arm per new gesture.
            | Effect::History
            | Effect::Hotlist
            | Effect::Popular
            | Effect::SideHistory { .. }
            | Effect::View => self.effect_that_opens(effect, backend, mailbox),
            Effect::CreateDirectory
            | Effect::CreateFile
            | Effect::Delete { .. }
            | Effect::Transferir { .. }
            | Effect::Rename
            | Effect::RenameIa
            // Phase 8: organize creates folders and moves, so a read-only
            // window does not request it either.
            | Effect::Organize
            | Effect::RenameBatch
            | Effect::Permissions
            | Effect::SearchSemantic => self.effect_that_mutates(effect, backend, mailbox),
        }
    }

    /// The effects that move the CURSOR or the listing: walking, entering,
    /// going up, going back and marking. None of this writes.
    pub(super) fn listing_effect(
        &mut self,
        effect: Effect,
        slot: u32,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match effect {
            Effect::Cursor(delta) => self.apply(
                &UiAction::MoveCursor {
                    slot_id: slot,
                    delta,
                },
                backend,
                mailbox,
            ),
            Effect::Page(pages) => {
                let rows = i64::from(self.slot().visible.max(1));
                self.apply(
                    &UiAction::MoveCursor {
                        slot_id: slot,
                        delta: pages.saturating_mul(rows),
                    },
                    backend,
                    mailbox,
                )
            }
            Effect::Extremo { al_final } => {
                if al_final {
                    self.slot_mut().pane.end();
                } else {
                    self.slot_mut().pane.home();
                }
                (self.applied(), vec![self.parche_cursor()])
            }
            Effect::Enter => {
                // A key acts on what is under the cursor RIGHT NOW, so the
                // generation is this very instant's.
                let key = RowKey(self.slot().pane.cursor() as u64);
                let generation = self.slot().pane.listing_epoch();
                self.navigation(
                    &UiAction::Activate {
                        slot_id: slot,
                        key,
                        generation,
                    },
                    backend,
                    mailbox,
                )
            }
            Effect::Up => self.navigation(&UiAction::Parent { slot_id: slot }, backend, mailbox),
            Effect::Trail { back: going_back } => self.navigation(
                &UiAction::History {
                    slot_id: slot,
                    back: going_back,
                },
                backend,
                mailbox,
            ),
            Effect::Mark => {
                let key = RowKey(self.slot().pane.cursor() as u64);
                let generation = self.slot().pane.listing_epoch();
                self.mark(slot, key, generation)
            }
            Effect::UnmarkAll => {
                self.slot_mut().pane.clear_marks();
                (self.applied(), vec![self.parche_rows()])
            }
            Effect::MarkAll => {
                self.slot_mut().pane.mark_all();
                (self.applied(), vec![self.parche_rows()])
            }
            Effect::InvertMarks => {
                self.slot_mut().pane.invert_marks();
                (self.applied(), vec![self.parche_rows()])
            }
            // #313: the rule for what "the same extension" is, what counts as
            // a file, and what gets restored lives in `PaneState`, so nothing
            // is decided here — it is the same model as the terminal's.
            Effect::MarkExtension { mark } => {
                self.slot_mut().pane.mark_same_extension(mark);
                (self.applied(), vec![self.parche_rows()])
            }
            Effect::MarkClass { dirs } => {
                self.slot_mut().pane.mark_kind(dirs);
                (self.applied(), vec![self.parche_rows()])
            }
            Effect::RestoreMarks => {
                self.slot_mut().pane.restore_previous_marks();
                (self.applied(), vec![self.parche_rows()])
            }
            // Marking WHILE MOVING: the whole rule — what it advances to,
            // what decides whether the span gets marked or unmarked, and that
            // both edge ones clear the other side — lives in `PaneState`,
            // same as in the terminal. The window repaints rows AND cursor
            // because these DO move it (except the edge ones, which on
            // purpose do not).
            Effect::MarkSubiendo => {
                self.slot_mut().pane.toggle_mark_and_retreat();
                (self.applied(), vec![self.parche_rows()])
            }
            Effect::MarkPage { down } => {
                let n = self.slot().pane.page_step();
                self.slot_mut().pane.toggle_mark_page(n, down);
                (self.applied(), vec![self.parche_rows()])
            }
            Effect::MarkToEdge { up } => {
                if up {
                    self.slot_mut().pane.mark_to_top();
                } else {
                    self.slot_mut().pane.mark_to_bottom();
                }
                (self.applied(), vec![self.parche_rows()])
            }
            // The rest do not get here: the `match` above dispatches them.
            _ => Self::no_mutates(),
        }
    }

    /// Sends a NATIVE effect to the hosting process, if anyone is there.
    ///
    /// `false` = nobody is listening. It is not a host error: a frontend that
    /// does not know how to do these things does not subscribe, and then the
    /// honest thing is to tell whoever pressed the key that it does not
    /// happen here, instead of acknowledging something that is not going to
    /// occur.
    pub(super) fn nativo(&self, effect: crate::dto::NativeEffect) -> bool {
        self.desktop
            .nativos
            .as_ref()
            .is_some_and(|tx| tx.send(effect).is_ok())
    }

    /// The paths of what is MARKED — or of the selected one, if there are no
    /// marks — to the clipboard.
    ///
    /// Marked first and the cursor as a fallback: it is the same rule as copy
    /// and move, and having two answers to "what does this act on" depending
    /// on the command is what makes a gesture apply to something else.
    ///
    /// In BYTES and in native form when there is one: what gets pasted has to
    /// open the same file, and a lossy-decoded path opens a different one.
    pub(super) fn copy_paths(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let slot = self.slot();
        // `marked_paths` already falls back to the cursor when there are no
        // marks: it is the same rule as copy and move, and having two
        // answers to "what does this act on" depending on the command is
        // what applies a gesture to something else.
        let paths: Vec<VPath> = slot.pane.marked_paths();
        if paths.is_empty() {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-nothing-selected".to_owned(),
                },
                Vec::new(),
            );
        }
        let count = paths.len();
        let bytes = norte_frontend::shell::clipboard_bytes(&paths);
        if !self.nativo(crate::dto::NativeEffect::CopyBytes { bytes, count }) {
            return Self::without_desktop();
        }
        let outgoing = self.say_with("msg-paths-copied", &[("n", &count.to_string())]);
        (self.applied(), outgoing)
    }

    /// Opens what is selected with the application the desktop chooses.
    pub(super) fn open_external(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(path) = self.slot().pane.selected().map(|e| e.path.clone()) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-nothing-selected".to_owned(),
                },
                Vec::new(),
            );
        };
        // Only what is on THIS disk: `xdg-open` cannot be handed an
        // `sftp://`, and pretending otherwise would open something else — or
        // nothing — without saying so.
        if !norte_frontend::shell::is_local(&path) {
            let outgoing = self.say("host-not-local");
            return (
                ActionAck::Unavailable {
                    reason_key: "host-not-local".to_owned(),
                },
                outgoing,
            );
        }
        // `openers.toml` rules, and the desktop is the LAST resort (#28). The
        // table used to be read only by the terminal, so "PDFs with zathura"
        // held in `ntc` and not in the window: a whole documented feature
        // honored by a single surface.
        if let Some(effect) = self.program_declared(&path) {
            if !self.nativo(effect) {
                return Self::without_desktop();
            }
            return (self.applied(), self.say("msg-opening-external"));
        }
        if !self.nativo(crate::dto::NativeEffect::OpenPath { path }) {
            return Self::without_desktop();
        }
        (self.applied(), self.say("msg-opening-external"))
    }

    /// The program `openers.toml` declares for this file, already ready to
    /// run. `None` if there is no rule for its mimetype, or if the binary is
    /// not there.
    ///
    /// The mimetype is guessed from the NAME with the same function as the
    /// terminal's (`openers::guess_mime`): who opens what cannot depend on
    /// which surface asks for it.
    fn program_declared(&self, path: &VPath) -> Option<crate::dto::NativeEffect> {
        let native_path = norte_vfs::native::vpath_to_native(path).ok()?;
        let mime = norte_frontend::openers::guess_mime(
            path.file_name()
                .map_or(&[][..], norte_proto::Segment::as_bytes),
        );
        let opener = self.config.openers.resolve(mime)?;
        // `%d` is the PANEL's directory, not the file's: the child opens
        // where the reader is looking (#144).
        let dir = norte_vfs::native::vpath_to_native(self.slot().pane.dir()).unwrap_or_else(|_| {
            native_path
                .parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_default()
        });
        let argv = Self::argv_resolved(opener.argv(&[&native_path], &dir))?;
        Some(crate::dto::NativeEffect::RunProgram {
            title_key: "program-output-open".to_owned(),
            argv,
            cwd: Some(path_bytes(&dir)),
            detached: opener.detached(),
        })
    }

    /// An already-interpolated argv, with its program resolved to an ABSOLUTE
    /// path and in bytes, ready for `NativeEffect::RunProgram`.
    ///
    /// It is resolved before giving it a `cwd` (ADR 0082): a bare name with
    /// `current_dir` set would be looked up in the directory currently being
    /// viewed. A binary that is not there returns `None`, and the caller
    /// decides.
    ///
    /// Returns the argv and not the whole effect on purpose: `title_key`
    /// stays as a LITERAL in each caller, which is what `catalogo_del_host`'s
    /// sweep can follow. A key hidden behind a parameter is a key that will
    /// paint as its own identifier the day it is missing.
    fn argv_resolved(mut argv: Vec<std::ffi::OsString>) -> Option<Vec<Vec<u8>>> {
        use std::os::unix::ffi::OsStrExt as _;
        let program = argv
            .first()
            .and_then(|p| norte_frontend::openers::resolve_program(p))?;
        argv[0] = program.into_os_string();
        Some(argv.iter().map(|a| a.as_bytes().to_vec()).collect())
    }

    /// `pane.edit`: the editor `[ui] editor` names, and if there is none,
    /// open.
    ///
    /// **`$EDITOR` is not used, and that is still deliberate**: it is a
    /// terminal editor and this window has nowhere to put one (#290). What
    /// was not deliberate was also ignoring `[ui] editor`, which names an
    /// explicit program and can perfectly well be graphical — its sibling key
    /// `[ui] diff` IS honored by this window, with this same machinery.
    pub(super) fn edit_external(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let configured = self
            .config
            .common
            .ui_editor
            .as_ref()
            .filter(|c| !c.is_empty())
            .cloned();
        let Some(template) = configured else {
            return self.open_external();
        };
        let Some(path) = self.slot().pane.selected().map(|e| e.path.clone()) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-nothing-selected".to_owned(),
                },
                Vec::new(),
            );
        };
        let Ok(native_path) = norte_vfs::native::vpath_to_native(&path) else {
            // A local editor cannot open an `sftp://`, same as `xdg-open`: it
            // is said, instead of launching blindly.
            let outgoing = self.say("host-not-local");
            return (
                ActionAck::Unavailable {
                    reason_key: "host-not-local".to_owned(),
                },
                outgoing,
            );
        };
        let dir = norte_vfs::native::vpath_to_native(self.slot().pane.dir()).unwrap_or_else(|_| {
            native_path
                .parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_default()
        });
        let template = norte_frontend::openers::expand_argv(&template, &[&native_path], &dir);
        let Some(argv) = Self::argv_resolved(template) else {
            let outgoing = self.say("host-program-missing");
            return (
                ActionAck::Unavailable {
                    reason_key: "host-program-missing".to_owned(),
                },
                outgoing,
            );
        };
        let effect = crate::dto::NativeEffect::RunProgram {
            title_key: "program-output-edit".to_owned(),
            argv,
            cwd: Some(path_bytes(&dir)),
            detached: self.config.common.ui_editor_detached.unwrap_or(false),
        };
        if !self.nativo(effect) {
            return Self::without_desktop();
        }
        (self.applied(), self.say("msg-opening-external"))
    }

    /// Opens a terminal sitting in the active panel's directory.
    pub(super) fn open_terminal(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let dir = self.slot().pane.dir().clone();
        if !norte_frontend::shell::is_local(&dir) {
            // A terminal sits in a filesystem directory: over an `sftp://`
            // there is nowhere to sit it, and opening it in `$HOME` without
            // saying anything would be opening it somewhere else.
            let outgoing = self.say("host-not-local");
            return (
                ActionAck::Unavailable {
                    reason_key: "host-not-local".to_owned(),
                },
                outgoing,
            );
        }
        if !self.nativo(crate::dto::NativeEffect::OpenTerminal { dir }) {
            return Self::without_desktop();
        }
        (self.applied(), self.say("msg-opening-terminal"))
    }

    /// Compares TWO files (#312) with `[ui] diff`'s program.
    ///
    /// WHICH two is decided by `norte_frontend::diffpair` — what is marked,
    /// or this one against the one across from it — and WHICH program is
    /// decided by the same configuration as the terminal's, with the same
    /// interpolation (`openers::expand_argv`) and the same default value.
    /// What changes is how it runs: the terminal suspends and waits for a
    /// key; here it is run by the host, detached if `[ui] diff_detached`
    /// says the comparator opens a window, and waited for and its output
    /// captured if not.
    pub(super) fn compare_files(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        use std::os::unix::ffi::OsStrExt as _;
        let here = self.slot();
        let marked: Vec<&norte_proto::Entry> = here.pane.marked_entries();
        let there = self
            .roles
            .get(norte_frontend::layout::RoleId::Target)
            .and_then(|SlotId(s)| self.slots.get(&s))
            .and_then(|h| h.pane.selected());
        let pair = norte_frontend::diffpair::pair(&marked, here.pane.selected(), there);
        let (a, b) = match pair {
            Ok(p) => p,
            Err(e) => {
                let key = e.message_key();
                let outgoing = self.say(key);
                return (
                    ActionAck::Unavailable {
                        reason_key: key.to_owned(),
                    },
                    outgoing,
                );
            }
        };
        let dir = self.slot().pane.dir().clone();
        let (Ok(native_a), Ok(native_b), Ok(native_dir)) = (
            norte_vfs::native::vpath_to_native(&a),
            norte_vfs::native::vpath_to_native(&b),
            norte_vfs::native::vpath_to_native(&dir),
        ) else {
            let outgoing = self.say("host-not-local");
            return (
                ActionAck::Unavailable {
                    reason_key: "host-not-local".to_owned(),
                },
                outgoing,
            );
        };
        let configured = self
            .config
            .common
            .ui_diff
            .as_ref()
            .filter(|c| !c.is_empty())
            .cloned();
        let detached = configured.is_some() && self.config.common.ui_diff_detached.unwrap_or(false);
        let template = configured.unwrap_or_else(|| {
            norte_frontend::diffpair::DEFAULT_ARGV
                .iter()
                .map(|s| (*s).to_owned())
                .collect()
        });
        let mut argv =
            norte_frontend::openers::expand_argv(&template, &[&native_a, &native_b], &native_dir);
        // The program is resolved to an absolute path BEFORE giving it a
        // `cwd` (ADR 0082): a bare name with `current_dir` set would be
        // looked up in the directory currently being viewed.
        let Some(program) = argv
            .first()
            .and_then(|p| norte_frontend::openers::resolve_program(p))
        else {
            let outgoing = self.say("host-program-missing");
            return (
                ActionAck::Unavailable {
                    reason_key: "host-program-missing".to_owned(),
                },
                outgoing,
            );
        };
        argv[0] = program.into_os_string();
        let effect = crate::dto::NativeEffect::RunProgram {
            title_key: "program-output-compare".to_owned(),
            argv: argv.iter().map(|a| a.as_bytes().to_vec()).collect(),
            cwd: Some(native_dir.as_os_str().as_bytes().to_vec()),
            detached,
        };
        if !self.nativo(effect) {
            return Self::without_desktop();
        }
        (self.applied(), self.say("msg-opening-external"))
    }

    /// What a program that ran while being waited for printed (#312): masked
    /// line by line, clamped, and shown.
    pub(super) fn program_finished(
        &mut self,
        title_key: &str,
        command: &str,
        output: &[u8],
        truncated: bool,
        failed: bool,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // Same caps as an extension's output: it is the same class of text,
        // from a different program.
        const MAX_LINES: usize = 2000;
        let text = String::from_utf8_lossy(output);
        let mut lines = Vec::new();
        let mut hostile = false;
        for line in text.lines().take(MAX_LINES) {
            let (displayable, flagged) = norte_frontend::display_name(line.as_bytes());
            hostile |= flagged;
            lines.push(clamp_display(displayable));
        }
        let was_truncated = truncated || text.lines().nth(MAX_LINES).is_some();
        let (cmd, cmd_hostile) = norte_frontend::display_name(command.as_bytes());
        self.desktop.program = Some(crate::dto::ProgramOutputView {
            // The key comes BACK from whoever is hosting: it is checked
            // against the ones this host emits, and whatever is not
            // recognized falls back to the generic one — an outside key does
            // not paint as its own identifier.
            title_key: match title_key {
                "program-output-compare" => "program-output-compare".to_owned(),
                _ => "program-output-title".to_owned(),
            },
            command: crate::dto::MaskedTextView {
                text: clamp_display(cmd),
                hostile: cmd_hostile,
            },
            lines,
            text_hostile: hostile,
            truncated: was_truncated,
            failed,
        });
        let change = ViewChange::ProgramOutput {
            output: self.desktop.program.clone(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// Closes a program's output panel.
    pub(super) fn close_program_output(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.desktop.program = None;
        (
            self.applied(),
            vec![self.parche(vec![ViewChange::ProgramOutput { output: None }])],
        )
    }

    /// Nobody listens to native effects: it is SAID.
    pub(super) fn without_desktop() -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        (
            ActionAck::Unavailable {
                reason_key: "host-no-desktop".to_owned(),
            },
            Vec::new(),
        )
    }

    /// The effects that open a SCREEN over the listing and touch nothing.
    pub(super) fn effect_that_opens(
        &mut self,
        effect: Effect,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match effect {
            Effect::Palette => self.open_palette(backend, mailbox),
            Effect::IrA => self.open_go_to(backend, mailbox),
            Effect::Help => self.open_help(backend, mailbox),
            Effect::Settings => self.open_settings(),
            Effect::Extensions => self.open_extensions(backend, mailbox),
            Effect::Agents => self.open_agents(),
            Effect::Theme => self.open_theme(),
            Effect::Menu => self.open_menu(),
            Effect::Exit => self.request_exit(),
            Effect::ProfileChoose => self.request_profiles(None, mailbox),
            Effect::ProfileSaveAs => self.request_save_profile(),
            Effect::ProfileVecino { back: going_back } => {
                self.request_profiles(Some(!going_back), mailbox)
            }
            Effect::Volumes => self.open_volumes(backend, mailbox),
            Effect::Connections => self.open_connections(backend, mailbox),
            Effect::History => self.open_history(),
            Effect::Hotlist => self.open_hotlist(),
            Effect::Popular => self.open_popular(),
            Effect::SideHistory { right } => self.open_side_history(right),
            Effect::View => self.request_visor(backend, mailbox),
            // The rest do not get here: the `match` above dispatches them.
            _ => Self::no_mutates(),
        }
    }

    /// The effects that WRITE. None mutates here: all five open the question
    /// the mutation goes through, which is the only gate.
    pub(super) fn effect_that_mutates(
        &mut self,
        effect: Effect,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match effect {
            Effect::CreateDirectory => self.request_mkdir(),
            Effect::CreateFile => self.request_file_new(),
            Effect::Delete { permanent } => self.request_deleted(permanent),
            // With the backend because, like comparing, it goes out to ask as
            // soon as it opens: the dialog is born with no destination
            // warnings and they arrive afterwards.
            Effect::Transferir { mover } => self.request_transfer(mover, backend, mailbox),
            Effect::Rename => self.request_rename(),
            Effect::RenameIa => self.request_instruction_ia(),
            Effect::Organize => self.request_organize_plan(None, backend, mailbox),
            Effect::RenameBatch => self.request_batch_template(None),
            Effect::Permissions => self.request_permissions(),
            Effect::SearchSemantic => self.request_query_semantic(),
            // The rest do not get here: the `match` above dispatches them.
            _ => Self::no_mutates(),
        }
    }
}

/// A native path in BYTES, which is how it crosses the bridge.
///
/// A free function so as not to repeat the Unix trait's `use` inside every
/// method: a `use` mid-function is what clippy calls
/// `items_after_statements`.
fn path_bytes(p: &std::path::Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt as _;
    p.as_os_str().as_bytes().to_vec()
}
