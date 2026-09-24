//! The one-line prompts `App` opens over the listing: mark by pattern,
//! rename, pack, split, create directory, a transfer's destination, command
//! line, AI rename and semantic search. All follow the same shape: `open_*`,
//! `*_push`, `*_pop`, `cancel_*`, `*_confirm`, `*_submitted` and
//! `*_set_error`.

use super::modal::{Modal, PromptKind, TextPrompt, TransferKind};
use super::{AI_RENAME_PAIR_LIMIT, App, SEMANTIC_HIT_LIMIT, format_by_name, parse_size};
use norte_i18n::{t, ta};
use norte_proto::{EntryKind, VPath};

impl App {
    /// Types into prompt `kind`, if it's the one open. No-op if it isn't:
    /// each dispatch table calls its own, and a key must not write into
    /// another modal's field.
    pub(crate) fn prompt_push(&mut self, kind: PromptKind, c: char) {
        if let Some(prompt) = self.open_prompt(kind) {
            prompt.push(c);
        }
    }

    /// Erases backward in prompt `kind`. No-op if it isn't the open one.
    pub(crate) fn prompt_pop(&mut self, kind: PromptKind) {
        if let Some(prompt) = self.open_prompt(kind) {
            prompt.pop();
        }
    }

    /// Leaves a failed submit's diagnostic and KEEPS what's typed: the user
    /// corrects it and retries.
    fn prompt_set_error(&mut self, kind: PromptKind, msg: String) {
        if let Some(prompt) = self.open_prompt(kind) {
            prompt.set_error(msg);
        }
    }

    /// Closes prompt `kind` after QUEUING what it requested, and opens the
    /// next pending one. The discipline is the same across all ten:
    /// `*_confirm` validates and does NOT close; this closes, and only when
    /// the submit went through.
    fn prompt_submitted(&mut self, kind: PromptKind) {
        if self.modal.as_ref().and_then(Modal::prompt_kind) == Some(kind) {
            self.modal = None;
            self.open_next_pending();
        }
    }

    /// Esc over a text prompt: closes without writing anything and opens the
    /// next pending one.
    ///
    /// Only FREE-TEXT modals close through here. A DECISION one — collision,
    /// approval, TOFU — has to deny through `on_dialog_key`, and reaching
    /// here with one open is a routing bug: asserted in debug and ignored in
    /// release, another one's decision is never closed blindly.
    pub(crate) fn cancel_prompt(&mut self, kind: PromptKind) {
        if self.modal.as_ref().and_then(Modal::prompt_kind) != Some(kind) {
            debug_assert!(
                false,
                "only free-text modals close without a decision; \
                 a DECISION modal must deny through on_dialog_key"
            );
            return;
        }
        self.modal = None;
        self.open_next_pending();
    }

    /// Prompt `kind`'s text field, if it's exactly the one that's open.
    fn open_prompt(&mut self, kind: PromptKind) -> Option<TextPrompt<'_>> {
        let modal = self.modal.as_mut()?;
        (modal.prompt_kind() == Some(kind)).then(|| modal.text_prompt())?
    }
}

impl App {
    /// Opens the mark-by-pattern modal (#103).
    pub fn open_mark_pattern(&mut self, mark: bool) {
        self.modal = Some(Modal::MarkPattern {
            mark,
            pattern: String::new(),
            error: None,
        });
    }

    /// Adds a character to the current pattern. No-op without the pattern
    /// modal.
    pub fn mark_pattern_push(&mut self, c: char) {
        self.prompt_push(PromptKind::MarkPattern, c);
    }

    /// Erases the pattern's last character. No-op without the pattern
    /// modal.
    pub fn mark_pattern_pop(&mut self) {
        self.prompt_pop(PromptKind::MarkPattern);
    }

    /// Applies the pattern: closes the modal and returns how many marks it
    /// changed. An invalid pattern LEAVES the modal open with the
    /// diagnostic — the user keeps what's typed to fix it.
    ///
    /// # Errors
    /// If the glob doesn't compile.
    pub fn mark_pattern_confirm(&mut self) -> Result<usize, norte_frontend::PatternError> {
        let Some(Modal::MarkPattern { mark, pattern, .. }) = &self.modal else {
            return Ok(0);
        };
        let (mark, pattern) = (*mark, pattern.clone());
        match self.focused_mut().mark_glob(&pattern, mark) {
            Ok(changed) => {
                self.modal = None;
                // Same discipline as ANY other modal close (`on_dialog_key`,
                // `cancel_mark_pattern`): never leave an approval/collision
                // queued waiting for the next key.
                self.open_next_pending();
                Ok(changed)
            }
            Err(e) => {
                let msg = e.to_string();
                if let Some(Modal::MarkPattern { error, .. }) = &mut self.modal {
                    *error = Some(msg);
                }
                Err(e)
            }
        }
    }

    /// Opens the in-place rename (shift+F6, #105): Move with the destination
    /// in `from`'s own PARENT — not the pane's dir, which in the search's
    /// VIRTUAL pane is the walk's root and would rename by moving the hit
    /// elsewhere. Always over the cursor (marks don't do bulk renaming —
    /// that would be a batch-rename, a different feature). No-op over a
    /// root.
    pub fn open_rename(&mut self) {
        let Some(from) = self.focused().selected().map(|e| e.path.clone()) else {
            return;
        };
        let Some(to_dir) = from.parent() else {
            return;
        };
        self.open_transfer_name_with(TransferKind::Move, self.focus, from, to_dir, false);
    }

    /// The editable-name modal. `from_pane` is the SOURCE pane and isn't
    /// assumed to be the focused one: a drop is born in the pane where the
    /// button went down, and that's where the name reinterpretation (#57)
    /// the field gets seeded with comes from.
    pub(super) fn open_transfer_name_with(
        &mut self,
        kind: TransferKind,
        from_pane: usize,
        from: VPath,
        to_dir: VPath,
        from_marks: bool,
    ) {
        let original = from
            .file_name()
            .map_or(Vec::new(), |n| n.as_bytes().to_vec());
        let enc = self.panes[from_pane].name_encoding();
        // Prefill = what the pane PAINTS (#98/M1): under reinterpretation, a
        // non-UTF8 name gets decoded (#57) instead of going through lossy —
        // editing produces the text that's SEEN; untouched, the original
        // bytes still win.
        let name = match (enc, std::str::from_utf8(&original)) {
            (_, Ok(s)) => s.to_owned(),
            (Some(e), Err(_)) => norte_encoding::decode_name(&original, e),
            (None, Err(_)) => String::from_utf8_lossy(&original).into_owned(),
        };
        // The destination's two lines are requested THE SAME WAY as with
        // several items (#343): splitting by item count left unwarned
        // exactly the most common case, copying a single file. They're born
        // empty and the event loop's return header fills them in, which is
        // where asking is possible.
        //
        // The total is THIS file's, with the same all-or-nothing rule as
        // several items': if the listing doesn't carry its size, there's no
        // number to show and the space warning stays quiet.
        self.pending_dest_check = Some(crate::app::DestCheck {
            to: to_dir.clone(),
            total: norte_frontend::space::total_to_write(
                self.panes[from_pane].entries(),
                std::slice::from_ref(&from),
            ),
        });
        self.modal = Some(Modal::TransferName {
            kind,
            from,
            to_dir,
            name,
            original,
            touched: false,
            from_marks,
            enc,
            error: None,
            space: None,
            confine: None,
        });
    }

    /// Adds a character to the current name (#105). Sets `touched`: from the
    /// first edit, the name is the TEXT. No-op without the modal.
    pub fn transfer_name_push(&mut self, c: char) {
        self.prompt_push(PromptKind::TransferName, c);
    }

    /// Erases the last character (#105). Sets `touched` ONLY if it erased
    /// something (review MINOR-5: an empty pop must not narrow the original
    /// bytes' path).
    pub fn transfer_name_pop(&mut self) {
        self.prompt_pop(PromptKind::TransferName);
    }

    /// Cancels without transferring — same guarded contract as
    /// [`Self::cancel_mkdir`].
    pub fn cancel_transfer_name(&mut self) {
        self.cancel_prompt(PromptKind::TransferName);
    }

    /// Validates and returns `(kind, from, dest)` WITHOUT closing the modal
    /// (same discipline as [`Self::mkdir_confirm`]: it closes the submit
    /// that got queued, via [`Self::transfer_name_submitted`]). Rules:
    /// untouched → the ORIGINAL bytes (rule 1); touched → the text's bytes,
    /// and a text that still contains U+FFFD (leftover from a hostile
    /// name's lossy prefill) gets REJECTED — confirming it would write real
    /// mojibake to disk. The guard doesn't tell leftover from intent: even a
    /// U+FFFD TYPED on purpose gets rejected (deliberate asymmetry with
    /// mkdir, which has no lossy prefill to inherit leftovers from). Under
    /// reinterpretation (#57), a TOUCHED name writes the decoded text's
    /// UTF-8 bytes — it transcodes on purpose: "see the name right and fix
    /// it" is the use case, and the untouched one stays byte-exact.
    /// `dest == from` also gets rejected (no-op; in rename, "same name").
    /// The name goes through [`norte_proto::Segment`] (not empty, not `/`,
    /// not NUL, not `.`/`..`).
    pub fn transfer_name_confirm(&mut self) -> Option<(TransferKind, VPath, VPath)> {
        let Some(Modal::TransferName {
            kind,
            from,
            to_dir,
            name,
            original,
            touched,
            ..
        }) = &self.modal
        else {
            return None;
        };
        let bytes = if *touched {
            if name.contains('\u{FFFD}') {
                let msg = norte_i18n::t("msg-transfer-name-fffd");
                self.transfer_name_set_error(msg);
                return None;
            }
            name.as_bytes().to_vec()
        } else {
            original.clone()
        };
        let (kind, from, to_dir) = (*kind, from.clone(), to_dir.clone());
        match norte_proto::Segment::new(bytes) {
            Ok(seg) => {
                let dest = to_dir.join(seg);
                if dest == from {
                    self.transfer_name_set_error(norte_i18n::t("msg-transfer-name-same"));
                    return None;
                }
                Some((kind, from, dest))
            }
            Err(e) => {
                self.transfer_name_set_error(e.to_string());
                None
            }
        }
    }

    /// Closes the modal after a submit that DID get queued (#105) and, if
    /// the source was the MARK, CONSUMES it (review MAJOR-1 — same doctrine
    /// as the batch: the selection gets consumed on SUBMIT). Esc and
    /// failures never consume.
    pub fn transfer_name_submitted(&mut self) {
        if let Some(Modal::TransferName { from_marks, .. }) = &self.modal {
            // What's specific to this submit, and why it isn't the plain
            // generic close: the batch gets consumed on SUBMIT.
            if *from_marks {
                self.focused_mut().clear_marks();
            }
        }
        self.prompt_submitted(PromptKind::TransferName);
    }

    /// Leaves a failed attempt's diagnostic (#105): the typed text survives
    /// to correct it.
    pub fn transfer_name_set_error(&mut self, msg: String) {
        self.prompt_set_error(PromptKind::TransferName, msg);
    }

    /// Opens the pack dialog (#132), or leaves the reason if it can't.
    ///
    /// The default name comes from what's about to be packed: with a single
    /// mark or the cursor over one, that entry's; with several, the
    /// directory's. It's what the managers these keys come from do, and it
    /// saves typing the normal case.
    ///
    /// It doesn't open over a read-only panel: the archive gets written
    /// THERE, and asking for the name only to fail afterward is making
    /// someone type for nothing.
    pub fn open_pack(&mut self) {
        if self.pane_read_only(self.focus()) {
            self.message = Some(t("msg-pack-read-only"));
            return;
        }
        let marked = self.focused().marked_paths();
        if marked.is_empty() {
            self.message = Some(t("msg-pack-nothing"));
            return;
        }
        let base = if marked.len() == 1 {
            marked[0].file_name().map(|s| s.as_bytes().to_vec())
        } else {
            self.focused()
                .dir()
                .file_name()
                .map(|s| s.as_bytes().to_vec())
        };
        let base = base.unwrap_or_else(|| b"archive".to_vec());
        // The suggestion comes from the source's bytes, with the active
        // REINTERPRETATION if there is one (#57): with "view names as
        // cp866" set, the pane paints `Папка` and the dialog used to suggest
        // `?????.zip` — the dialog contradicting the panel it was opened
        // from. What can't be read stays as `U+FFFD` and
        // [`Self::pack_confirm`] REFUSES to confirm it, same as the rename
        // prompt: a name with the replacement character inside is nobody's
        // name.
        let suggested = match self.focused().name_encoding() {
            Some(enc) => format!("{}.zip", norte_encoding::decode_name(&base, enc)),
            None => format!("{}.zip", String::from_utf8_lossy(&base)),
        };
        self.modal = Some(Modal::Pack {
            name: suggested,
            error: None,
        });
    }

    /// Adds a character to the archive's name. No-op without its modal.
    pub fn pack_push(&mut self, c: char) {
        self.prompt_push(PromptKind::Pack, c);
    }

    /// Erases the last character. No-op without its modal.
    pub fn pack_pop(&mut self) {
        self.prompt_pop(PromptKind::Pack);
    }

    /// Cancels the pack dialog without writing anything.
    pub fn cancel_pack(&mut self) {
        self.cancel_prompt(PromptKind::Pack);
    }

    /// The `archive.pack` params the dialog describes, or `None` if the
    /// name isn't valid.
    ///
    /// The FORMAT comes from the typed name and travels explicit; a name
    /// with no known extension gets refused here instead of packing into a
    /// format the user didn't ask for.
    #[must_use]
    pub fn pack_confirm(&mut self) -> Option<norte_proto::methods::ArchivePackParams> {
        let Some(Modal::Pack { name, .. }) = &self.modal else {
            return None;
        };
        let name = name.clone();
        // The replacement character must not reach a file name: it's what's
        // left of bytes that couldn't be read, and two different names
        // produce the SAME `U+FFFD` — the second archive would collide with
        // the first's. Same criterion, and same key, as the rename prompt.
        if name.contains('\u{FFFD}') {
            self.pack_set_error(t("msg-transfer-name-fffd"));
            return None;
        }
        let Some(format) = format_by_name(name.as_bytes()) else {
            self.pack_set_error(t("msg-pack-unknown-format"));
            return None;
        };
        let Ok(seg) = norte_proto::Segment::new(name.into_bytes()) else {
            self.pack_set_error(t("msg-pack-bad-name"));
            return None;
        };
        let dir = self.focused().dir().clone();
        let dest = dir.join(seg);
        Some(norte_proto::methods::ArchivePackParams {
            sources: self.focused().marked_paths(),
            dest,
            format,
            level: None,
            // The base is the panel's directory: the saved names are the
            // ones seen on screen, which is what whoever unpacks it later
            // expects.
            base: dir,
        })
    }

    /// The dialog closed because the task got queued.
    pub fn pack_submitted(&mut self) {
        self.prompt_submitted(PromptKind::Pack);
    }

    /// Leaves the diagnostic and keeps what's typed.
    pub fn pack_set_error(&mut self, msg: String) {
        self.prompt_set_error(PromptKind::Pack, msg);
    }

    /// The panel a split's chunks go to: the next VISIBLE one, or the same
    /// one if there's no other (#132).
    ///
    /// By visible position and not by slot id: `slot_ids()` includes tabs
    /// that aren't on screen, so chunks could land in a background tab's
    /// directory — four gigabytes in a place the reader isn't looking at
    /// and that the dialog doesn't name.
    #[must_use]
    pub fn split_dest_pane(&self) -> usize {
        // By visible POSITION, the same notion of "pane" focus,
        // `pane_read_only` and the rest of the TUI use. With a single
        // panel the destination is itself — which is what F5 does when
        // there's nowhere else to point at — not `None`.
        let n = self.panes.len();
        if n <= 1 {
            return self.focus();
        }
        (self.focus() + 1) % n
    }

    /// Opens the split-a-file dialog (#132).
    pub fn open_split(&mut self) {
        // The read-only check is on the DESTINATION, not the source:
        // splitting reads the focused panel and writes to the other. With
        // the gate backward it refused splitting a file that was in a
        // read-only place — inside an archive, in an SFTP export — and it
        // accepted splitting INTO one, which then failed with a raw error.
        let dest = self.split_dest_pane();
        if self.pane_read_only(dest) {
            self.message = Some(t("msg-pack-read-only"));
            return;
        }
        if self
            .focused()
            .selected()
            .is_none_or(|e| e.kind != EntryKind::File)
        {
            self.message = Some(t("msg-split-needs-file"));
            return;
        }
        self.modal = Some(Modal::Split {
            size: "10M".to_owned(),
            error: None,
        });
    }

    /// Adds a character to the size. No-op without its modal.
    pub fn split_push(&mut self, c: char) {
        self.prompt_push(PromptKind::Split, c);
    }

    /// Erases the last character. No-op without its modal.
    pub fn split_pop(&mut self) {
        self.prompt_pop(PromptKind::Split);
    }

    /// Cancels the split dialog.
    pub fn cancel_split(&mut self) {
        self.cancel_prompt(PromptKind::Split);
    }

    /// The `file.split` params the dialog describes, or `None` if the size
    /// isn't valid.
    #[must_use]
    pub fn split_confirm(&mut self) -> Option<norte_proto::methods::FileSplitParams> {
        let Some(Modal::Split { size, .. }) = &self.modal else {
            return None;
        };
        let Some(bytes) = parse_size(size) else {
            self.split_set_error(t("msg-split-bad-size"));
            return None;
        };
        let path = self.focused().selected().map(|e| e.path.clone())?;
        // Chunks go to the OTHER visible panel if there is one, and to the
        // same one if not: it's what copy does, and for the same reason —
        // splitting a one-gigabyte file into the place it's already in
        // usually doesn't fit. **No `?` over the lookup**: with a single
        // panel there was no "other", the whole function returned `None`,
        // and Enter did absolutely nothing — no task, no error, no closing
        // the dialog.
        let dest_dir = self.panes[self.split_dest_pane()].dir().clone();
        Some(norte_proto::methods::FileSplitParams {
            path,
            part_bytes: bytes,
            dest_dir,
        })
    }

    /// Opens the PERMISSIONS dialog (#314) over `targets`, with the field
    /// pre-filled with `mode` if the cursor's entry's could be read.
    ///
    /// Pre-filling isn't a decoration: typing `755` over an empty field is
    /// easy, and stripping the execute bit off a file that already had it —
    /// because you couldn't see which one it was — is the kind of mistake
    /// this dialog has to make hard.
    pub fn open_chmod(&mut self, targets: Vec<VPath>, mode: Option<u32>) {
        if targets.is_empty() {
            return;
        }
        self.modal = Some(Modal::Chmod {
            mode: mode
                .map(norte_frontend::chmod::format_mode)
                .unwrap_or_default(),
            targets,
            error: None,
        });
    }

    /// What has to be sent on confirming the permissions dialog: the paths
    /// and the mode already parsed. `None` if what's typed isn't a mode —
    /// the dialog stays open with its diagnostic.
    pub fn chmod_confirm(&mut self) -> Option<norte_proto::methods::FsSetModeParams> {
        let Some(Modal::Chmod { mode, targets, .. }) = &self.modal else {
            return None;
        };
        let (text, paths) = (mode.clone(), targets.clone());
        // The field accepts `chmod`'s form (#315): `755`, `-R 755` or
        // `-R 644,755` — files and directories — instead of a separate key
        // inside a field where every key is text.
        match norte_frontend::chmod::parse_request(&text) {
            Ok(req) => Some(norte_proto::methods::FsSetModeParams {
                paths,
                mode: req.mode,
                recursive: req.recursive,
                dir_mode: req.dir_mode,
            }),
            Err(e) => {
                self.chmod_set_error(norte_i18n::t(e.message_key()));
                None
            }
        }
    }

    /// The dialog closed because the task got queued.
    pub fn chmod_submitted(&mut self) {
        self.prompt_submitted(PromptKind::Chmod);
    }

    /// Leaves the diagnostic and keeps what's typed.
    pub fn chmod_set_error(&mut self, msg: String) {
        self.prompt_set_error(PromptKind::Chmod, msg);
    }

    /// The dialog closed because the task got queued.
    pub fn split_submitted(&mut self) {
        self.prompt_submitted(PromptKind::Split);
    }

    /// Leaves the diagnostic and keeps what's typed.
    pub fn split_set_error(&mut self, msg: String) {
        self.prompt_set_error(PromptKind::Split, msg);
    }

    /// Opens the create-directory modal (F7, #104).
    pub fn open_mkdir(&mut self) {
        self.modal = Some(Modal::Mkdir {
            name: String::new(),
            error: None,
        });
    }

    /// Adds a character to the current name. No-op without the mkdir modal.
    /// Capped in `chars` like the pattern (#103): an accidental paste
    /// doesn't overflow the modal; the REAL name limit is set by the
    /// provider.
    pub fn mkdir_push(&mut self, c: char) {
        self.prompt_push(PromptKind::Mkdir, c);
    }

    /// Erases the name's last character. No-op without the mkdir modal.
    pub fn mkdir_pop(&mut self) {
        self.prompt_pop(PromptKind::Mkdir);
    }

    /// Cancels `Modal::Mkdir` without creating anything — THIS free-text
    /// modal's Esc (same contract and guard as
    /// [`Self::cancel_mark_pattern`]: a DECISION modal never closes through
    /// here).
    pub fn cancel_mkdir(&mut self) {
        self.cancel_prompt(PromptKind::Mkdir);
    }

    /// Validates the name and returns the full DESTINATION (focused pane's
    /// dir + name as [`norte_proto::Segment`] — the validation is
    /// `VPath`'s: not empty, not `/`, not NUL, not `.`/`..`). Does NOT close
    /// the modal (#104 review MINOR-1): the caller closes it with
    /// [`Self::mkdir_submitted`] ONLY after queuing the task — a submit
    /// that fails (policy, connection) leaves the diagnostic with
    /// [`Self::mkdir_set_error`] and the user KEEPS what's typed. An
    /// invalid name leaves its diagnostic right here and returns `None`.
    pub fn mkdir_confirm(&mut self) -> Option<VPath> {
        let Some(Modal::Mkdir { name, .. }) = &self.modal else {
            return None;
        };
        match norte_proto::Segment::new(name.as_bytes().to_vec()) {
            Ok(seg) => Some(self.focused().dir().join(seg)),
            Err(e) => {
                let msg = e.to_string();
                self.mkdir_set_error(msg);
                None
            }
        }
    }

    /// Closes the modal after a submit that DID get queued (#104): same
    /// closing discipline as the rest (never leave a pending one waiting).
    pub fn mkdir_submitted(&mut self) {
        self.prompt_submitted(PromptKind::Mkdir);
    }

    /// Leaves a failed submit's diagnostic in the modal (#104): the typed
    /// name survives to correct it and retry.
    pub fn mkdir_set_error(&mut self, msg: String) {
        self.prompt_set_error(PromptKind::Mkdir, msg);
    }

    /// The program the loop must launch this turn, if any.
    ///
    /// **Returns `None` once the user has already asked to quit**, and the
    /// intent is dropped: `on_tick` can ARM a `PendingShell` (the editor
    /// from `pane.edit-new`, when creation finishes) and right after, within
    /// the same tick, swallow the `Ctrl+C` the `refresh_panes` right behind
    /// it polls. The loop drains what's pending BEFORE looking at `quit`, so
    /// without this guard a `Ctrl+C` during creation didn't quit norte: it
    /// opened the editor, and only quit once it was closed. Nobody pressing
    /// `Ctrl+C` is asking for an editor to open.
    pub fn take_pending_shell(&mut self) -> Option<super::PendingShell> {
        let pending = self.pending_shell.take();
        if self.quit { None } else { pending }
    }

    /// Opens the create-empty-file modal (Shift+F4, #290).
    ///
    /// Asks for a name because the DAEMON creates the file (`fs.create`) and
    /// not the editor: this way creation goes through policy and the
    /// journal, with its undo, like any other mutation (hard rule 4).
    /// Before, the editor launched with an empty buffer and the file
    /// appeared on save, outside norte entirely.
    pub fn open_edit_new(&mut self) {
        self.modal = Some(Modal::EditNew {
            dir: self.focused().dir().clone(),
            name: String::new(),
            error: None,
        });
    }

    /// Opens the "save the workspace as a profile" modal (#306).
    ///
    /// Pre-filled with the ACTIVE profile if there is one: the normal thing
    /// is to start from the one you have set, so "save as" over the same
    /// name is saving over it — which is what any program does. With no
    /// profile, empty: there's no default name that isn't a made-up one.
    pub fn open_profile_save_as(&mut self) {
        let name = self
            .active_profile
            .as_ref()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.modal = Some(Modal::ProfileSaveAs { name, error: None });
    }

    /// Closes the profile modal after a save that DID write.
    pub fn prompt_submitted_profile_save(&mut self) {
        self.prompt_submitted(PromptKind::ProfileSaveAs);
    }

    /// Leaves the diagnostic under the field; the name survives.
    pub fn prompt_error_profile_save(&mut self, msg: String) {
        self.prompt_set_error(PromptKind::ProfileSaveAs, msg);
    }

    /// Validates the name and returns the full DESTINATION. Same contract as
    /// [`Self::mkdir_confirm`], NOT closing the modal included: it's closed
    /// by [`Self::edit_new_submitted`] once the task is queued.
    ///
    /// The directory comes from the MODAL, not from the focused pane: it
    /// got bound on opening it (see [`Modal::EditNew`]).
    ///
    /// The locality guard gets repeated over that bound directory. With
    /// `dir` in the modal it's DEFENSIVE — dispatch already did it and
    /// nobody can change that value — and it stays because what would make
    /// it necessary is exactly the change someone will see as a
    /// simplification: reading `focused().dir()` again here. Then a pane
    /// that moved to `sftp://` between opening and confirming would create a
    /// file no editor on this machine can open afterward.
    pub fn edit_new_confirm(&mut self) -> Option<VPath> {
        let Some(Modal::EditNew { dir, name, .. }) = &self.modal else {
            return None;
        };
        if norte_vfs_local::vpath_to_native(dir).is_err() {
            // The SAME message as the shell and the editor: names the
            // sanitized location instead of just saying "no".
            let msg = crate::gestures::shell_remote_message(self);
            self.edit_new_set_error(msg);
            return None;
        }
        let destination = match norte_proto::Segment::new(name.as_bytes().to_vec()) {
            Ok(seg) => dir.join(seg),
            Err(e) => {
                let msg = e.to_string();
                self.edit_new_set_error(msg);
                return None;
            }
        };
        Some(destination)
    }

    /// Closes the modal after a submit that DID get queued.
    pub fn edit_new_submitted(&mut self) {
        self.prompt_submitted(PromptKind::EditNew);
    }

    /// Leaves a failed submit's diagnostic; the name survives.
    pub fn edit_new_set_error(&mut self, msg: String) {
        self.prompt_set_error(PromptKind::EditNew, msg);
    }

    /// Opens the transfer-destination prompt ([`Modal::TransferDest`]).
    ///
    /// Pre-filled with the focused panel's address, in wire form: it's the
    /// one [`Self::transfer_dest_confirm`] knows how to read back, and
    /// editing its tail is shorter than typing it whole. No-op if there's
    /// nothing to transfer: never a dialog over an empty batch.
    pub fn open_transfer_dest(&mut self, kind: TransferKind) {
        if self.focused().marked_paths().is_empty() {
            return;
        }
        self.modal = Some(Modal::TransferDest {
            kind,
            input: self.focused().dir().to_wire(),
            error: None,
        });
    }

    /// Adds a character to the current destination. No-op without its
    /// modal. Same cap as the rest of the free-text prompts.
    pub fn transfer_dest_push(&mut self, c: char) {
        self.prompt_push(PromptKind::TransferDest, c);
    }

    /// Erases the destination's last CHARACTER, percent escape included.
    /// No-op without its modal.
    ///
    /// `String::pop` used to erase one character of the TEXT, and the text
    /// is wire form: backspacing over `%C3%A9` left `%C3%A`, which no
    /// longer parses (`BadEscape`) — one keypress didn't erase a letter of
    /// the name, it corrupted an escape (#246 M3).
    pub fn transfer_dest_pop(&mut self) {
        self.prompt_pop(PromptKind::TransferDest);
    }

    /// Cancels the destination prompt without transferring anything.
    pub fn cancel_transfer_dest(&mut self) {
        self.cancel_prompt(PromptKind::TransferDest);
    }

    /// Reads the typed destination and OPENS the transfer through the usual
    /// door ([`Self::open_transfer_to_dir`]).
    ///
    /// An address that doesn't parse leaves its diagnostic in the modal
    /// itself and keeps what's typed, like the rest of the prompts. Returns
    /// `true` if it moved on to the next modal.
    pub fn transfer_dest_confirm(&mut self) -> bool {
        let Some(Modal::TransferDest { kind, input, .. }) = &self.modal else {
            return false;
        };
        let (kind, input) = (*kind, input.clone());
        // Only the WIRE form gets read: a text that looks like a local path
        // (`/home/…`) isn't a norte address, and guessing a scheme for it is
        // how a copy ends up in another backend than the reader thought.
        match VPath::parse(&input) {
            Ok(dir) if dir == *self.focused().dir() => {
                // The prompt is PRE-FILLED with the source directory, so an
                // unedited `Enter` asked to copy every mark onto itself:
                // `ops::copy_task` rejects it, but one at a time, and the
                // reader ended up with N failed tasks instead of one line in
                // the dialog itself (#244 m6).
                if let Some(Modal::TransferDest { error, .. }) = &mut self.modal {
                    *error = Some(t("msg-transfer-dest-same"));
                }
                false
            }
            Ok(dir) => {
                self.open_transfer_to_dir(kind, self.focus(), dir, None);
                true
            }
            Err(e) => {
                if let Some(Modal::TransferDest { error, .. }) = &mut self.modal {
                    *error = Some(ta("msg-transfer-dest-invalid", &[("err", &e.to_string())]));
                }
                false
            }
        }
    }

    /// Opens the `pane.command-line` prompt (#135).
    pub fn open_command_line(&mut self) {
        self.modal = Some(Modal::CommandLine {
            command: String::new(),
            error: None,
        });
    }

    /// Adds a character to the command line. No-op without its modal. Same
    /// cap in `chars` as the rest of the free-text prompts. Reaching the cap
    /// LEAVES A DIAGNOSTIC, unlike the rest of the free-text prompts (S4
    /// review, M4). A truncated directory name fails to create and it
    /// shows; a truncated command line RUNS — `rm -rf /old-project` trimmed
    /// to `rm -rf /project` is a different order, not journalled and not
    /// undoable. Staying quiet about the truncation here is letting Enter
    /// get pressed blind.
    pub fn command_line_push(&mut self, c: char) {
        self.prompt_push(PromptKind::CommandLine, c);
    }

    /// Erases the line's last character. No-op without its modal.
    pub fn command_line_pop(&mut self) {
        self.prompt_pop(PromptKind::CommandLine);
    }

    /// Cancels `Modal::CommandLine` without running anything (same contract
    /// and guard as [`Self::cancel_mkdir`]: a DECISION modal never closes
    /// through here).
    pub fn cancel_command_line(&mut self) {
        self.cancel_prompt(PromptKind::CommandLine);
    }

    /// Validates and returns the line; does NOT close the modal — the
    /// caller closes with [`Self::command_line_submitted`] after leaving
    /// the suspension pending (same discipline as
    /// [`Self::ai_rename_confirm`]).
    ///
    /// The line is returned AS IS, without `trim`: the trimmed version is
    /// only used to decide if it's empty. A command starting with a space
    /// is a real bash/zsh convention (`HISTCONTROL=ignorespace`), and
    /// trimming it would silently change what the user wrote.
    pub fn command_line_confirm(&mut self) -> Option<String> {
        if let Some(Modal::CommandLine { command, error }) = &mut self.modal {
            if command.trim().is_empty() {
                *error = Some(t("modal-command-line-empty"));
                return None;
            }
            return Some(command.clone());
        }
        None
    }

    /// Closes the prompt after leaving the suspension queued (same closing
    /// discipline as [`Self::ai_rename_submitted`]).
    pub fn command_line_submitted(&mut self) {
        self.prompt_submitted(PromptKind::CommandLine);
    }

    /// Opens the batch rename TEMPLATE prompt (#310), pre-filled with
    /// `[N].[E]` — the name exactly as it is.
    ///
    /// Pre-filled with the identity and not blank: this way the first thing
    /// seen is what a template's shape looks like, and editing it is
    /// shorter than writing it whole. An identity plan renames nothing
    /// (pairs that don't change get dropped), so confirming without
    /// touching anything is harmless.
    pub fn open_rename_batch(&mut self) {
        self.modal = Some(Modal::RenameBatchPattern {
            pattern: "[N].[E]".to_owned(),
            error: None,
        });
    }

    /// Validates the template against the names it's going to touch and
    /// returns it; does NOT close the modal — the caller closes with
    /// [`Self::rename_batch_submitted`] after spawning the plan request,
    /// same discipline as [`Self::ai_rename_confirm`].
    ///
    /// A template that doesn't work leaves its diagnostic right here, under
    /// the field, and returns `None`: it gets explained with the human
    /// still there and before asking the core anything.
    pub fn rename_batch_confirm(&mut self) -> Option<String> {
        let names = self.rename_batch_names();
        if let Some(Modal::RenameBatchPattern { pattern, error }) = &mut self.modal {
            let text = pattern.trim().to_owned();
            return match norte_frontend::rename_pattern::check(&text, &names) {
                Ok(()) => Some(text),
                Err(e) => {
                    *error = Some(t(norte_frontend::rename_pattern::error_key(e)));
                    None
                }
            };
        }
        None
    }

    /// The names the batch acts on: the MARKED ones, and if there are none
    /// the cursor's — the same operand as any other operation
    /// (`marked_paths`), so there's no new rule to learn.
    ///
    /// Only the ones that are text: a plan's pair travels UTF-8 by
    /// protocol, so a name that isn't can't enter a batch (not through the
    /// AI path either). They're filtered out here and the caller says so,
    /// instead of sending them and the plan coming back invalid without
    /// explaining which one was the odd one out.
    #[must_use]
    pub fn rename_batch_names(&self) -> Vec<String> {
        self.focused()
            .marked_paths()
            .iter()
            .filter_map(|p| {
                p.file_name()
                    .and_then(|s| std::str::from_utf8(s.as_bytes()).ok())
                    .map(std::borrow::ToOwned::to_owned)
            })
            .collect()
    }

    /// Closes the template prompt after spawning the plan request.
    pub fn rename_batch_submitted(&mut self) {
        self.prompt_submitted(PromptKind::RenameBatch);
    }

    /// Adds a character to the template. No-op without its modal.
    pub fn rename_batch_push(&mut self, c: char) {
        self.prompt_push(PromptKind::RenameBatch, c);
    }

    /// Erases the template's last character. No-op without its modal.
    pub fn rename_batch_pop(&mut self) {
        self.prompt_pop(PromptKind::RenameBatch);
    }

    /// Cancels the template prompt without launching anything.
    pub fn cancel_rename_batch(&mut self) {
        self.cancel_prompt(PromptKind::RenameBatch);
    }

    /// Opens the AI rename instruction prompt (M4-IA).
    pub fn open_ai_rename(&mut self) {
        self.modal = Some(Modal::AiRenameInstruction {
            instruction: String::new(),
            error: None,
        });
    }

    /// Adds a character to the current instruction. No-op without its
    /// modal. Capped in `chars` like the pattern (#103): an accidental
    /// paste doesn't overflow the modal; the REAL limit (4 KiB) is set by
    /// the daemon.
    pub fn ai_rename_push(&mut self, c: char) {
        self.prompt_push(PromptKind::AiRename, c);
    }

    /// Erases the instruction's last character. No-op without its modal.
    pub fn ai_rename_pop(&mut self) {
        self.prompt_pop(PromptKind::AiRename);
    }

    /// Cancels `Modal::AiRenameInstruction` without launching anything —
    /// THIS free-text modal's Esc (same contract and guard as
    /// [`Self::cancel_mkdir`]: a DECISION modal never closes through here).
    pub fn cancel_ai_rename(&mut self) {
        self.cancel_prompt(PromptKind::AiRename);
    }

    /// Validates and returns the instruction; does NOT close the modal —
    /// the caller closes with [`Self::ai_rename_submitted`] after SPAWNING
    /// the request (audit INFO-7: the spawn itself doesn't fail; the
    /// model's failures arrive ASYNC and go out via the status bar,
    /// `msg-ai-rename-failed`, not through the modal). An empty instruction
    /// leaves its diagnostic right here and returns `None`.
    pub fn ai_rename_confirm(&mut self) -> Option<String> {
        if let Some(Modal::AiRenameInstruction { instruction, error }) = &mut self.modal {
            let text = instruction.trim();
            if text.is_empty() {
                *error = Some(t("modal-ai-rename-empty-instruction"));
                return None;
            }
            return Some(text.to_owned());
        }
        None
    }

    /// Closes the prompt after a launch that DID go out (M4-IA): same
    /// closing discipline as [`Self::mkdir_submitted`] (never leave a
    /// pending one waiting).
    pub fn ai_rename_submitted(&mut self) {
        self.prompt_submitted(PromptKind::AiRename);
    }

    /// Leaves a diagnostic under the field with the text KEPT. Audit
    /// INFO-7: in the real flow it only covers SYNCHRONOUS diagnostics
    /// prior to the spawn (today, the empty instruction is marked by
    /// [`Self::ai_rename_confirm`] itself); a model failure arrives ASYNC
    /// with the prompt already closed and goes to the status bar, never
    /// through here.
    pub fn ai_rename_set_error(&mut self, msg: String) {
        self.prompt_set_error(PromptKind::AiRename, msg);
    }

    /// Scrolls the AI plan's window (audit MAJOR-3): `down` advances one
    /// pair, otherwise it goes back; clamped to `[0, len - window]`. No-op
    /// without its modal. Scrolling NEVER confirms nor cancels —
    /// `dialog_action` returns `None` for `dialog.up`/`dialog.down` in this
    /// modal (outside its decision allowlist) and the run loop routes those
    /// commands here.
    pub fn ai_plan_scroll(&mut self, down: bool) {
        if let Some(Modal::AiRenamePlan {
            entries,
            offset,
            seen,
            ..
        }) = &mut self.modal
        {
            let max = entries.len().saturating_sub(AI_RENAME_PAIR_LIMIT);
            *offset = if down {
                (*offset + 1).min(max)
            } else {
                offset.saturating_sub(1)
            };
            // HIGH watermark: scrolling back up doesn't un-read what was
            // already read, and without this approving would depend on
            // where you stopped instead of how far you'd gotten.
            *seen = (*seen).max((*offset + AI_RENAME_PAIR_LIMIT).min(entries.len()));
        }
    }

    /// Scrolls the organize tree's window (phase 8), with the same clamp
    /// and the same high watermark as the AI plan: approving depends on how
    /// far you've GOTTEN, not on where you stopped. No-op without its
    /// modal.
    pub fn organize_plan_scroll(&mut self, down: bool) {
        if let Some(Modal::OrganizePlan {
            lines,
            offset,
            seen,
            ..
        }) = &mut self.modal
        {
            let max = lines
                .len()
                .saturating_sub(norte_frontend::organize::ORGANIZE_LINE_LIMIT);
            *offset = if down {
                (*offset + 1).min(max)
            } else {
                offset.saturating_sub(1)
            };
            *seen = (*seen)
                .max((*offset + norte_frontend::organize::ORGANIZE_LINE_LIMIT).min(lines.len()));
        }
    }

    /// Scrolls the checksums modal's window (#311), with the same clamp as
    /// the AI plan's and for the same reason: the whole list is scrollable,
    /// and the verdict that matters — the one that doesn't match — can be
    /// on any row. No-op without its modal.
    pub fn checksums_scroll(&mut self, down: bool) {
        if let Some(Modal::Checksums { rows, offset, .. }) = &mut self.modal {
            let max = rows.len().saturating_sub(AI_RENAME_PAIR_LIMIT);
            *offset = if down {
                (*offset + 1).min(max)
            } else {
                offset.saturating_sub(1)
            };
        }
    }

    /// Puts the BATCH plan (§17) into the AI plan modal that was waiting
    /// for it. Returns `false` if there wasn't one — the human already
    /// closed the modal, or the plan is HELD behind another modal and the
    /// run loop fills it in — so the caller knows to look for it in its
    /// stash.
    ///
    /// Only fills in a modal in [`norte_frontend::BatchPlan::Pending`]: an
    /// answer never overwrites an already-resolved plan.
    pub fn settle_ai_batch_plan(&mut self, resolved: &norte_frontend::BatchPlan) -> bool {
        if let Some(Modal::AiRenamePlan { plan, .. }) = &mut self.modal
            && *plan == norte_frontend::BatchPlan::Pending
        {
            *plan = resolved.clone();
            return true;
        }
        false
    }

    /// Opens the semantic search query prompt (M4-IA-2).
    pub fn open_semantic_search(&mut self) {
        self.modal = Some(Modal::SemanticQuery {
            query: String::new(),
            error: None,
        });
    }

    /// Adds a character to the current query. No-op without its modal. Same
    /// cap in `chars` as the AI instruction: an accidental paste doesn't
    /// overflow the modal; the REAL limit is set by the daemon.
    pub fn semantic_push(&mut self, c: char) {
        self.prompt_push(PromptKind::Semantic, c);
    }

    /// Erases the query's last character. No-op without its modal.
    pub fn semantic_pop(&mut self) {
        self.prompt_pop(PromptKind::Semantic);
    }

    /// Cancels `Modal::SemanticQuery` without launching anything — THIS
    /// free-text modal's Esc (same contract and guard as
    /// [`Self::cancel_ai_rename`]: a DECISION modal never closes through
    /// here).
    pub fn cancel_semantic(&mut self) {
        self.cancel_prompt(PromptKind::Semantic);
    }

    /// Validates and returns the query; does NOT close the modal — the
    /// caller closes with [`Self::semantic_submitted`] after SPAWNING the
    /// request (same contract as [`Self::ai_rename_confirm`]: model
    /// failures arrive ASYNC and go out via the status bar,
    /// `msg-semantic-failed`, not through the modal). An empty query leaves
    /// its diagnostic right here and returns `None`.
    pub fn semantic_confirm(&mut self) -> Option<String> {
        if let Some(Modal::SemanticQuery { query, error }) = &mut self.modal {
            let text = query.trim();
            if text.is_empty() {
                *error = Some(t("modal-semantic-empty-query"));
                return None;
            }
            return Some(text.to_owned());
        }
        None
    }

    /// Closes the prompt after a launch that DID go out (M4-IA-2): same
    /// closing discipline as [`Self::ai_rename_submitted`] (never leave a
    /// pending one waiting).
    pub fn semantic_submitted(&mut self) {
        self.prompt_submitted(PromptKind::Semantic);
    }

    /// Leaves a diagnostic under the field with the text KEPT. Like
    /// [`Self::ai_rename_set_error`]: only covers SYNCHRONOUS diagnostics
    /// prior to the spawn (today, the empty query is marked by
    /// [`Self::semantic_confirm`] itself); a model failure arrives ASYNC
    /// with the prompt already closed and goes to the status bar, never
    /// through here.
    pub fn semantic_set_error(&mut self, msg: String) {
        self.prompt_set_error(PromptKind::Semantic, msg);
    }

    /// Moves the hits cursor (`down` = true moves it down); the window
    /// follows the cursor, clamped on both ends. No-op without its modal.
    /// Scrolling NEVER confirms nor cancels — `dialog_action` returns
    /// `None` for `dialog.up`/`dialog.down` in this modal (outside its
    /// decision allowlist) and the run loop routes those commands here
    /// ([`Self::ai_plan_scroll`]'s mold).
    pub fn semantic_cursor(&mut self, down: bool) {
        if let Some(Modal::SemanticHits {
            hits,
            offset,
            cursor,
        }) = &mut self.modal
        {
            if hits.is_empty() {
                return;
            }
            *cursor = if down {
                (*cursor + 1).min(hits.len() - 1)
            } else {
                cursor.saturating_sub(1)
            };
            if *cursor < *offset {
                *offset = *cursor;
            }
            if *cursor >= *offset + SEMANTIC_HIT_LIMIT {
                *offset = *cursor + 1 - SEMANTIC_HIT_LIMIT;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::app::pane::Pane;
    use crate::app::testutil::*;
    use norte_proto::{Entry, EntryKind, VPath};

    /// #132: the pack dialog's suggestion can come out lossy — the source's
    /// name isn't always UTF-8 — and confirming it as is would create a
    /// file with the replacement character inside.
    ///
    /// Two different names that can't be read give the SAME suggestion, so
    /// the second archive would collide with the first's file. It's the
    /// same rejection, and the same key, as the rename prompt.
    #[test]
    fn empaquetar_rehusa_un_nombre_con_el_caracter_de_reemplazo() {
        let mut app = app_dos_panes();
        app.modal = Some(Modal::Pack {
            name: "caf\u{FFFD}.zip".to_owned(),
            error: None,
        });
        assert!(app.pack_confirm().is_none(), "doesn't pack with that");
        let Some(Modal::Pack { error, .. }) = &app.modal else {
            panic!("the dialog stays open to fix it");
        };
        assert_eq!(error.as_deref(), Some(t("msg-transfer-name-fffd").as_str()));
    }

    /// And an extension norte doesn't know how to WRITE gets stated in the
    /// dialog, instead of packing a zip named like a rar.
    #[test]
    fn empaquetar_rehusa_una_extension_que_no_se_escribe() {
        let mut app = app_dos_panes();
        app.modal = Some(Modal::Pack {
            name: "stuff.rar".to_owned(),
            error: None,
        });
        assert!(app.pack_confirm().is_none());
        let Some(Modal::Pack { error, .. }) = &app.modal else {
            panic!("stays open");
        };
        assert!(error.is_some(), "and says why");
    }

    /// #105: F5 for ONE item opens the editable name prefixed with the
    /// ORIGINAL name. Untouched, confirm uses the RAW bytes (rule 1: a
    /// non-UTF8 name copied unedited never goes through lossy).
    #[test]
    fn transfer_name_sin_editar_conserva_los_bytes_originales() {
        let dir = VPath::parse("mem:///").unwrap();
        let hostile = dir
            .clone()
            .join(norte_proto::Segment::new(b"report\xFF\xFE.dat".to_vec()).unwrap());
        let mut app = App::new(
            Pane::new(
                dir.clone(),
                vec![Entry {
                    attrs: std::collections::BTreeMap::new(),
                    path: hostile.clone(),
                    kind: EntryKind::File,
                    size: None,
                    mtime_ms: None,
                }],
            ),
            Pane::new(VPath::parse("mem:///dst").unwrap(), Vec::new()),
        );
        app.open_transfer(TransferKind::Copy, 0, 1, None);
        let (kind, from, dest) = app.transfer_name_confirm().expect("valid");
        assert_eq!(kind, TransferKind::Copy);
        assert_eq!(from, hostile);
        assert_eq!(
            dest,
            VPath::parse("mem:///dst")
                .unwrap()
                .join(norte_proto::Segment::new(b"report\xFF\xFE.dat".to_vec()).unwrap()),
            "raw bytes to the destination, never the lossy form"
        );
    }

    /// #105: editing replaces the name with the TYPED text; and a text that
    /// still contains U+FFFD (leftover from a hostile name's lossy prefill)
    /// gets REJECTED — confirming it would write mojibake to disk.
    #[test]
    fn transfer_name_editado_usa_el_texto_y_rechaza_fffd() {
        let dir = VPath::parse("mem:///").unwrap();
        let hostile = dir
            .clone()
            .join(norte_proto::Segment::new(b"x\xFF.dat".to_vec()).unwrap());
        let mut app = App::new(
            Pane::new(
                dir.clone(),
                vec![Entry {
                    attrs: std::collections::BTreeMap::new(),
                    path: hostile,
                    kind: EntryKind::File,
                    size: None,
                    mtime_ms: None,
                }],
            ),
            Pane::new(VPath::parse("mem:///dst").unwrap(), Vec::new()),
        );
        app.open_transfer(TransferKind::Copy, 0, 1, None);
        // Touch the field (erases the lossy prefill's last char): the text
        // still carries the prefill's U+FFFD → rejected with a diagnostic.
        app.transfer_name_pop();
        assert!(app.transfer_name_confirm().is_none());
        assert!(matches!(
            &app.modal,
            Some(Modal::TransferName { error: Some(_), .. })
        ));
        // Rewritten clean: valid, and it's the text's bytes.
        while matches!(&app.modal, Some(Modal::TransferName { name, .. }) if !name.is_empty()) {
            app.transfer_name_pop();
        }
        for c in "clean.dat".chars() {
            app.transfer_name_push(c);
        }
        let (_, _, dest) = app.transfer_name_confirm().expect("clean");
        assert_eq!(dest, VPath::parse("mem:///dst/clean.dat").unwrap());
    }

    /// #105: shift+F6 — in-place rename: destination = SAME dir; confirming
    /// without changing the name is an error (no-op), and a new name builds
    /// the destination in the same dir.
    #[test]
    fn rename_construye_en_el_mismo_dir_y_rechaza_el_mismo_nombre() {
        let mut app = app_with_entries(&["a.txt"]);
        app.open_rename();
        assert!(
            app.transfer_name_confirm().is_none(),
            "same name = no-op, never a submit"
        );
        assert!(matches!(
            &app.modal,
            Some(Modal::TransferName { error: Some(_), .. })
        ));
        app.transfer_name_push('2'); // "a.txt2"
        let (kind, from, dest) = app.transfer_name_confirm().expect("new name");
        assert_eq!(kind, TransferKind::Move);
        assert_eq!(from, VPath::parse("mem:///a.txt").unwrap());
        assert_eq!(dest, VPath::parse("mem:///a.txt2").unwrap());
    }

    /// #105 (rule 1, canonical corpus): renaming a hostile name to a clean
    /// one keeps `from` BYTE-EXACT for every corpus name — the source never
    /// goes through text, only the new name is typed.
    #[test]
    fn rename_de_cada_nombre_hostil_del_corpus_conserva_el_from() {
        let dir = VPath::parse("mem:///").unwrap();
        for (i, hostile) in norte_testkit::corpus::hostile_names().iter().enumerate() {
            let from = dir
                .clone()
                .join(norte_proto::Segment::new(hostile.bytes.clone()).unwrap());
            let mut app = App::new(
                Pane::new(
                    dir.clone(),
                    vec![Entry {
                        attrs: std::collections::BTreeMap::new(),
                        path: from.clone(),
                        kind: EntryKind::File,
                        size: None,
                        mtime_ms: None,
                    }],
                ),
                Pane::new(dir.clone(), Vec::new()),
            );
            app.open_rename();
            while matches!(&app.modal, Some(Modal::TransferName { name, .. }) if !name.is_empty()) {
                app.transfer_name_pop();
            }
            for c in "clean".chars() {
                app.transfer_name_push(c);
            }
            let (_, got_from, dest) = app
                .transfer_name_confirm()
                .unwrap_or_else(|| panic!("corpus[{i}] {}", hostile.id));
            assert_eq!(got_from, from, "corpus[{i}]: byte-exact from");
            assert_eq!(dest, VPath::parse("mem:///clean").unwrap());
        }
    }

    /// #104: F7's modal validates with `VPath`'s rules and returns the full
    /// destination; invalid = diagnostic in the modal, never a submit.
    #[test]
    fn el_modal_mkdir_valida_y_construye_el_destino() {
        let mut app = app_with_entries(&["a"]);
        app.open_mkdir();
        for c in "docs".chars() {
            app.mkdir_push(c);
        }
        let target = app.mkdir_confirm().expect("valid name");
        assert_eq!(target, VPath::parse("mem:///docs").unwrap());
        assert!(
            app.modal.is_some(),
            "confirming does NOT close: the queued submit closes it (MINOR-1)"
        );
        // A failed submit leaves the diagnostic and keeps the name…
        app.mkdir_set_error("policy".into());
        assert!(matches!(
            &app.modal,
            Some(Modal::Mkdir { error: Some(_), name }) if name == "docs"
        ));
        // …and the one that got queued, closes.
        app.mkdir_submitted();
        assert!(app.modal.is_none(), "submitted closes the modal");

        // Empty: error, modal open.
        app.open_mkdir();
        assert!(app.mkdir_confirm().is_none());
        assert!(
            matches!(&app.modal, Some(Modal::Mkdir { error: Some(_), .. })),
            "the diagnostic stays in the modal"
        );

        // `..` is DotSegment: never a destination.
        app.mkdir_push('.');
        app.mkdir_push('.');
        assert!(app.mkdir_confirm().is_none());
        assert!(matches!(
            &app.modal,
            Some(Modal::Mkdir { error: Some(_), .. })
        ));

        // Embedded `/`: InvalidByte.
        app.cancel_mkdir();
        app.open_mkdir();
        for c in "a/b".chars() {
            app.mkdir_push(c);
        }
        assert!(app.mkdir_confirm().is_none());

        // Cancelling closes with nothing.
        app.cancel_mkdir();
        assert!(app.modal.is_none());
    }

    /// #290: `pane.edit-new` ASKS for a name, because the daemon creates the
    /// file and not the editor. Same contract as F7's — validates with
    /// `VPath`'s rules, doesn't close on confirming, keeps what's typed
    /// after a failed submit — over the other kind of node.
    /// The pane is `file://` on purpose: creating a file to edit it gets
    /// refused where there's no native way to, and `mem://` doesn't have
    /// one.
    fn app_local_para_crear() -> App {
        let d = VPath::parse("file:///tmp").expect("wire");
        App::new(
            super::super::Pane::new(d.clone(), Vec::new()),
            super::super::Pane::new(d, Vec::new()),
        )
    }

    #[test]
    fn el_modal_de_fichero_nuevo_valida_y_construye_el_destino() {
        let mut app = app_local_para_crear();
        app.open_edit_new();
        for c in "notes.txt".chars() {
            app.prompt_push(PromptKind::EditNew, c);
        }
        let target = app.edit_new_confirm().expect("valid name");
        assert_eq!(target, VPath::parse("file:///tmp/notes.txt").unwrap());
        assert!(
            app.modal.is_some(),
            "confirming does NOT close: the submit closes it"
        );

        // A failed submit — policy, journal — keeps the name.
        app.edit_new_set_error("policy".into());
        assert!(matches!(
            &app.modal,
            Some(Modal::EditNew { error: Some(_), name, .. }) if name == "notes.txt"
        ));
        app.edit_new_submitted();
        assert!(app.modal.is_none(), "submitted closes the modal");

        // And names that are never a destination still aren't here.
        for bad in ["", "..", "a/b"] {
            app.open_edit_new();
            for c in bad.chars() {
                app.prompt_push(PromptKind::EditNew, c);
            }
            assert!(app.edit_new_confirm().is_none(), "{bad} isn't a name");
            assert!(
                matches!(&app.modal, Some(Modal::EditNew { error: Some(_), .. })),
                "{bad}: the diagnostic stays in the modal"
            );
            app.cancel_prompt(PromptKind::EditNew);
        }
    }

    /// The directory gets BOUND on opening the modal, like the window: if
    /// the pane goes elsewhere between opening it and confirming it, the
    /// file gets created where the reader was looking when they typed the
    /// name.
    #[test]
    fn el_fichero_nuevo_se_crea_donde_se_abrio_el_dialogo() {
        let mut app = app_local_para_crear();
        app.open_edit_new();
        for c in "notes.txt".chars() {
            app.prompt_push(PromptKind::EditNew, c);
        }
        // The pane moves UNDER the modal.
        app.panes[0].begin_listing(
            VPath::parse("file:///other").unwrap(),
            Vec::new(),
            false,
            None,
        );
        assert_eq!(
            app.edit_new_confirm(),
            Some(VPath::parse("file:///tmp/notes.txt").unwrap()),
            "the destination comes from the modal, not from the pane now"
        );
    }

    /// A `Ctrl+C` that arrives while the editor is being armed does NOT
    /// open the editor: the loop drains what's pending before looking at
    /// `quit`, so without this guard quitting norte went through an editing
    /// session nobody asked for first.
    #[test]
    fn pedir_salir_tira_el_programa_pendiente() {
        let mut app = app_with_entries(&["a"]);
        app.pending_shell = Some(crate::app::PendingShell {
            argv: vec![std::ffi::OsString::from("vi")],
            cwd: None,
            wait_for_key: false,
            check_regular: None,
        });
        assert!(
            app.take_pending_shell().is_some(),
            "without quitting, it launches"
        );

        app.pending_shell = Some(crate::app::PendingShell {
            argv: vec![std::ffi::OsString::from("vi")],
            cwd: None,
            wait_for_key: false,
            check_regular: None,
        });
        app.quit = true;
        assert!(
            app.take_pending_shell().is_none(),
            "on quitting, it doesn't"
        );
        assert!(
            app.pending_shell.is_none(),
            "and the intent is dropped: it doesn't reappear next turn"
        );
    }

    /// #103 T9: the pattern modal marks/unmarks and reports how many marks
    /// it changed — happy path (valid glob, real matches).
    #[test]
    fn the_pattern_modal_marks_and_reports_how_many() {
        let mut app = app_with_entries(&["a.rs", "b.rs", "c.txt"]);
        app.open_mark_pattern(true);
        assert!(matches!(
            app.modal,
            Some(Modal::MarkPattern { mark: true, .. })
        ));
        app.mark_pattern_push('*');
        app.mark_pattern_push('.');
        app.mark_pattern_push('r');
        app.mark_pattern_push('s');
        let changed = app.mark_pattern_confirm().expect("valid glob");
        assert_eq!(changed, 2);
        assert!(app.modal.is_none());
        assert_eq!(app.focused().marks_len(), 2);
    }

    /// An invalid pattern (a glob that doesn't compile) leaves the modal
    /// OPEN with the diagnostic — the user keeps what's typed to fix it —
    /// and marks nothing.
    #[test]
    fn an_invalid_pattern_keeps_the_modal_open_and_marks_nothing() {
        let mut app = app_with_entries(&["a.rs"]);
        app.open_mark_pattern(true);
        app.mark_pattern_push('[');
        assert!(app.mark_pattern_confirm().is_err());
        assert!(app.modal.is_some(), "the user keeps their text to fix it");
        assert_eq!(app.focused().marks_len(), 0);
    }
}
