//! The dialog stack: typing into one and answering it.
//!
//! Part of `controller`: these are `State` methods, moved here without
//! touching them (ADR 0086). The only writer is still the actor.

// These modules are the same `impl State` split into pieces, so they use
// the same imports as the parent. Enumerating them here would be a
// forty-line list per file, in 32 files, that goes stale the moment the
// parent imports something — `super::*` tracks it on its own.
#[allow(clippy::wildcard_imports)]
use super::*;

impl State {
    /// Types into a dialog's field.
    ///
    /// The renderer sends the WHOLE text after the edit, not a delta: the
    /// caret is its own, and rebuilding it in Rust would mean keeping two
    /// ideas of where the cursor is.
    pub(super) fn write_in_dialog(
        &mut self,
        id: ModalId,
        text: &str,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(dialog) = self.dialogs.iter_mut().find(|d| d.id == id) else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        if dialog.vista.input.is_none() {
            // A decision dialog has nowhere to type, and accepting text
            // nobody is going to read would be worse than saying so.
            return (Self::stale(StaleAction::Modal), Vec::new());
        }
        if !matches!(dialog.typed, Typed::Text(_)) {
            // A PASSWORD field is not typed into here (#327): the host does
            // not store what is typed, and the password crosses over once,
            // with the answer. A renderer that sends it anyway is pushing
            // secret material down a file-name path, so it is discarded
            // BEFORE touching it — without storing it, without projecting
            // it, and without answering with a phrase that talks about
            // "names".
            return (Self::stale(StaleAction::Modal), Vec::new());
        }
        if text.len() > MAX_NAME {
            // Neither trimmed nor accepted halfway: a name is not a screen
            // string, and trimming it is inventing another one.
            return (
                ActionAck::Unavailable {
                    reason_key: "host-name-too-long".to_owned(),
                },
                Vec::new(),
            );
        }
        let Typed::Text(raw) = &mut dialog.typed else {
            // Impossible: filtered out by the guard above, before looking at
            // anything.
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        text.clone_into(raw);
        // What gets PAINTED is something else: masked (a `U+202E` in the
        // name you are about to be asked to approve shows) and clamped.
        let (pintable, hostile) = norte_frontend::display_name(text.as_bytes());
        dialog.vista.input = Some(clamp_display(pintable));
        dialog.vista.input_hostile = hostile;
        let change = ViewChange::Dialogs {
            dialogs: self.dialog_views(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// Refuses to confirm a form that cannot be launched yet, without
    /// closing it.
    ///
    /// `None` = go ahead (or it is not a form). Lives outside
    /// `run_pending` on purpose: there, the dialog has already been
    /// taken off the stack, so "do not close" is not an option, and a
    /// mistyped `1 gigabyte` used to sweep away all twelve controls while the
    /// notice pointed at a field that no longer existed — advice that cannot
    /// be followed. It is the same spot, and the same reason, as the guard
    /// for the empty secret.
    fn rejects_form_invalid(
        &mut self,
        pos: usize,
    ) -> Option<(ActionAck, Vec<BridgeEnvelope<UiUpdate>>)> {
        let (key, field) = match &self.dialogs[pos].typed {
            Typed::Form(form) => {
                if let Some(field) = form.field_unreadable() {
                    ("search-bad-field", Some(field))
                } else if form.has_criteria() {
                    return None;
                } else {
                    // With no criteria at all it is not a search, it is a
                    // recursive listing under another name.
                    ("search-empty", None)
                }
            }
            Typed::Text(_) | Typed::Secret => return None,
        };
        // Focus goes to the guilty field and the projection is rebuilt, so
        // the window shows it pointed at instead of just saying so.
        if let (Some(field), Typed::Form(form)) = (field, &mut self.dialogs[pos].typed) {
            form.field = field;
        }
        if let Typed::Form(form) = &self.dialogs[pos].typed {
            let fields = super::search::search_fields(form);
            self.dialogs[pos].vista.fields = fields;
        }
        self.status.message = Some(clamp_display(norte_i18n::t_in(self.lang, key)));
        let changes = vec![
            ViewChange::Status(self.status.clone()),
            ViewChange::Dialogs {
                dialogs: self.dialog_views(),
            },
        ];
        Some((
            ActionAck::Unavailable {
                reason_key: key.to_owned(),
            },
            vec![self.parche(changes)],
        ))
    }

    /// Touches a FORM-dialog field (bridge 91).
    ///
    /// Three guards, and each one covers something different: a dialog that
    /// is not a form has no fields to touch; an id that is not from this
    /// form is not invented — the fields are decided by the host, not by
    /// whoever paints — and a text longer than a name is neither trimmed nor
    /// accepted halfway, same as in [`Self::write_in_dialog`].
    ///
    /// A toggle and a cycle arrive WITHOUT a value: the renderer says they
    /// were touched and which state they go to is decided here. Sending the
    /// destination would let two quick presses step on each other, the
    /// second one born from an earlier snapshot.
    pub(super) fn touch_dialog_field(
        &mut self,
        id: ModalId,
        field: &str,
        valor: &crate::action::DialogFieldValue,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        use crate::action::DialogFieldValue as Valor;
        use norte_frontend::search::{self as search, SearchField};

        let Some(dialog) = self.dialogs.iter_mut().find(|d| d.id == id) else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        let Typed::Form(form) = &mut dialog.typed else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        if let Valor::Text { text } = valor
            && text.len() > MAX_NAME
        {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-name-too-long".to_owned(),
                },
                Vec::new(),
            );
        }
        // A `U+FFFD` is not typed: it is put there by this host's PROJECTION
        // when masking, and it comes back when the renderer re-seeds the
        // field with what it painted. Accepting it would turn what is
        // painted into what is typed — the search pattern would end up
        // carrying the replacement instead of the name — and nothing would
        // say so. It is the same belt `segment_typed` puts on the
        // single-field dialog; the other one (the renderer not re-seeding)
        // lives in `dialogs.ts`.
        if let Valor::Text { text } = valor
            && text.contains('\u{FFFD}')
        {
            return (Self::stale(StaleAction::Modal), Vec::new());
        }
        let conocido = match (SearchField::por_id(field), valor) {
            (Some(f), Valor::Text { text }) => {
                form.set_text(f, text.clone());
                true
            }
            (None, Valor::Toggled) => match field {
                search::ID_REGEX => {
                    form.toggle_regex();
                    true
                }
                search::ID_CASE => {
                    form.toggle_case();
                    true
                }
                search::ID_WHOLE_WORD => {
                    form.toggle_whole_word();
                    true
                }
                search::ID_RECURSIVE => {
                    form.toggle_recursive();
                    true
                }
                _ => false,
            },
            (None, Valor::Cycled) if field == search::ID_KINDS => {
                form.cycle_kinds();
                true
            }
            // Everything else is a control answering for something that is
            // not its own — a "touched" text field, a toggle with text — or
            // an id this form does not have. In both cases, nothing to
            // apply.
            _ => false,
        };
        if !conocido {
            // Not a stale modal — it is open and it is the same one — it is
            // a renderer naming a control this form does not have. Saying
            // "resync" would hide that bug of its own.
            return (
                ActionAck::Unavailable {
                    reason_key: "host-unknown-field".to_owned(),
                },
                Vec::new(),
            );
        }
        dialog.vista.fields = super::search::search_fields(form);
        let change = ViewChange::Dialogs {
            dialogs: self.dialog_views(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// The reader wants to close: it either asks, or closes.
    ///
    /// The decision of WHETHER to ask is the shared one
    /// (`settings::quit_needs_confirm`), the same one the terminal uses;
    /// what each frontend computes on its own is what counts as "work
    /// remaining". Here that means some task ALIVE on the board: what would
    /// be lost on close is a copy left halfway, not a mark.
    ///
    /// Closing never used to ask. `quit_needs_confirm`'s rustdoc already
    /// named a window's `confirm_quit_should_open` that did not exist.
    pub(super) fn request_exit(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let has_work = self.tasks.values().any(|t| !Self::terminal(t.vista.state));
        if !norte_frontend::settings::quit_needs_confirm(
            self.config.common.ui_confirm_quit,
            has_work,
        ) {
            self.nativo(crate::dto::NativeEffect::CloseWindow);
            return (self.applied(), Vec::new());
        }
        let id = ModalId(self.next_modal);
        self.next_modal += 1;
        // The body SAYS how much is running when there is any: a plain
        // "are you sure?" is not a question that can be answered.
        let body = if has_work {
            let n = self
                .tasks
                .values()
                .filter(|t| !Self::terminal(t.vista.state))
                .count();
            vec![crate::dto::DialogLine {
                text: clamp_display(norte_i18n::ta_in(
                    self.lang,
                    "modal-quit-pending",
                    &[("n", &n.to_string())],
                )),
                hostile: false,
            }]
        } else {
            Vec::new()
        };
        self.dialogs.push(Dialog {
            id,
            recognized: true,
            vista: DialogView {
                id,
                title_key: "modal-quit-title".to_owned(),
                destination: None,
                subject: None,
                asker: None,
                deadline: None,
                deadline_at_ms: None,
                body,
                overflow_note: String::new(),
                overflow_hostile: false,
                choices: vec![
                    DialogChoice {
                        id: "confirm".to_owned(),
                        label_key: "dialog-quit".to_owned(),
                        // Closing with work running LOSES that work: the
                        // button says so with its shape, like the delete
                        // one.
                        destructive: has_work,
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
            on_confirm: Some(Pending::Exit),
        });
        let change = ViewChange::Dialogs {
            dialogs: self.dialog_views(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// Answers a dialog.
    ///
    /// An id that is not the open dialog's — because it was already
    /// answered, because the renderer was slow — does nothing and says so:
    /// confirming twice does NOT delete twice.
    /// What a dialog's AFFIRMATIVE answer sets in motion.
    ///
    /// Separate from [`Self::responder_dialog`], which keeps what is the
    /// same for all of them: that the id matches the open dialog, that the
    /// answer is among the ones offered, the read-only lock, and the close.
    /// Only what each pending action does lives here.
    #[expect(
        clippy::too_many_lines,
        reason = "exhaustive dispatcher: one arm per pending action, no logic inside"
    )]
    pub(super) fn run_pending(
        &mut self,
        dialog: Dialog,
        secret: Option<&str>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Message>,
    ) -> (Option<&'static str>, Vec<BridgeEnvelope<UiUpdate>>) {
        let mut outputs = Vec::new();
        // The reason the answer did NOT do anything, if there was one:
        // it travels to the ack instead of staying only in the status bar.
        let mut rehusado: Option<&'static str> = None;
        match dialog.on_confirm {
            Some(Pending::Delete { paths, permanent }) => {
                Self::launch_deleted(paths, permanent, backend, buzon);
            }
            Some(Pending::InstructionIa { dir }) => {
                let instruction = dialog.typed.text().to_owned();
                outputs.extend(self.launch_plan_ia(dir, instruction, backend, buzon));
            }
            Some(Pending::TemplateBatch { dir, names }) => {
                let template = dialog.typed.text().to_owned();
                outputs.extend(self.launch_template_plan(dir, &names, &template, backend, buzon));
            }
            Some(Pending::Exit) => {
                // It was already asked and the answer was yes: the host
                // dumps the session and destroys the window.
                self.nativo(crate::dto::NativeEffect::CloseWindow);
            }
            Some(Pending::QuerySemantic) => {
                let query = dialog.typed.text().to_owned();
                outputs.extend(self.launch_semantic(query, backend, buzon));
            }
            Some(Pending::DeliverSecret { conn, slot, dir }) => {
                // The empty field does not reach here: `responder_dialog`
                // leaves confirm INERT while there is nothing, and that is
                // where it has to be — at this point the dialog has already
                // left the stack, and "do not close" is no longer an option.
                // What IS checked is the SHAPE: without it, a wiring bug
                // would deliver a normal field's text as if it were a
                // password.
                let (Typed::Secret, Some(secret)) = (&dialog.typed, secret) else {
                    rehusado = Some("host-secret-empty");
                    return (rehusado, outputs);
                };
                // It is wrapped the MOMENT it arrives: from here on, the
                // host's copy is overwritten with zeros when the task ends,
                // instead of sitting in the heap until someone reuses the
                // block.
                let mut secret_safe = norte_frontend::secret::TypedSecret::default();
                secret_safe.set(secret);
                Self::launch_secret(conn, secret_safe, slot, dir, backend, buzon);
            }
            Some(Pending::Rename { from, siembra }) => {
                let (motivo, parts) =
                    self.confirm_rename(&from, &siembra, dialog.typed.text(), backend, buzon);
                rehusado = motivo;
                outputs.extend(parts);
            }
            Some(Pending::Transferir {
                source,
                source_dir,
                paths,
                dest,
                mover,
            }) => {
                self.send_batch(&paths, &source_dir, &dest, mover, backend, buzon);
                // The marks are CONSUMED by the send, not by the outcome
                // (same criterion as the TUI and as mc): a selection
                // half-consumed would mean different things depending on
                // which of the N tasks finished.
                //
                // And the SOURCE slot's, not whichever has focus now:
                // `FocusSlot` is not blocked while a dialog is open, so a
                // click on the other pane between the question and the
                // answer used to clear the wrong pane's marks and leave
                // intact the ones that had just been sent — and the reader
                // would press F5 again over the same thing.
                if let Some(h) = self.slots.get_mut(&source) {
                    h.pane.clear_marks();
                }
                outputs.push(self.parche_rows());
            }
            Some(Pending::Release { paths, dest }) => {
                // `source_dir` is the DESTINATION on purpose: it is only
                // used to note which directories go out of date when
                // something is moved, and here nothing is ever moved.
                // Wherever it came from belongs to another process and this
                // window does not list it.
                //
                // And the marks are NOT touched: this pane's were set by
                // the reader for something else, and what is being copied
                // did not come from there.
                self.send_batch(&paths, &dest, &dest, false, backend, buzon);
                outputs.push(self.parche_rows());
            }
            Some(Pending::Search { root }) => {
                use norte_frontend::search::SearchField;
                // The form is COPIED before touching `self`: launching the
                // search needs the whole state, and the dialog has it on
                // loan.
                // The form is MOVED: the dialog arrives by value and its
                // `on_confirm` was already consumed above, so there is
                // nothing left to clone.
                let Typed::Form(form) = dialog.typed else {
                    // A search dialog with no form is a wiring bug, not the
                    // reader's. The guard exists because the type demands
                    // it: there is nothing to launch, and nothing to tell
                    // whoever is in front either.
                    return (None, outputs);
                };
                let form = *form;
                // Already validated: `responder_dialog` checks it BEFORE
                // taking the dialog off the stack, which is where it can
                // still be left unclosed.
                //
                // The clock is read HERE and told to the mapping: "changed
                // seven days ago" is counted from this instant.
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0_i64, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
                // What the results header shows as the query. A filters-only
                // search has no pattern to show, and leaving it blank used
                // to paint a mute header.
                let label = if !form.text(SearchField::Name).is_empty() {
                    form.text(SearchField::Name).to_owned()
                } else if !form.text(SearchField::Content).is_empty() {
                    form.text(SearchField::Content).to_owned()
                } else {
                    norte_i18n::t_in(self.lang, "search-query-filters-only")
                };
                let params = norte_frontend::search::params(&form, root, now_ms, Self::MAX_RESULTS);
                outputs.extend(self.launch_search(params, label, backend, buzon));
            }
            // The two that create an EMPTY node from a typed name. Together
            // because they are the same shape — validate the segment,
            // enqueue, mark the directory to refresh — and this `match` is
            // a dispatcher already brushing its limit.
            Some(p @ (Pending::CreateDirectory { .. } | Pending::CreateFile { .. })) => {
                let (dir, file) = match p {
                    Pending::CreateDirectory { dir } => (dir, false),
                    Pending::CreateFile { dir } => (dir, true),
                    _ => unreachable!("the pattern above only leaves those two"),
                };
                let (motivo, parts) = if file {
                    self.create_file(&dir, dialog.typed.text(), backend, buzon)
                } else {
                    self.create_directory(&dir, dialog.typed.text(), backend, buzon)
                };
                rehusado = motivo;
                outputs.extend(parts);
            }
            // #309: the favorite. The destination was captured by the dialog
            // on open, it is not re-read here.
            Some(Pending::SaveFavorite { dest }) => {
                let (motivo, parts) = self.save_favorite(&dest, dialog.typed.text(), buzon);
                rehusado = motivo;
                outputs.extend(parts);
            }
            // #318: the profile. Unlike the favorite, what is saved is read
            // NOW: it is the screen's state, not an answer the dialog
            // captured on open.
            Some(Pending::SaveProfile) => {
                let (motivo, parts) = self.save_profile(dialog.typed.text(), buzon);
                rehusado = motivo;
                outputs.extend(parts);
            }
            // A settings text entry's value: it is validated by the shared
            // editor and, if valid, written.
            Some(Pending::EditSetting { id }) => {
                let (motivo, parts) = self.confirm_setting_value(id, dialog.typed.text(), buzon);
                rehusado = motivo;
                outputs.extend(parts);
            }
            // The two that build files from what was typed, together: this
            // `match` is a dispatcher and already brushes its limit.
            Some(p @ (Pending::Split { .. } | Pending::Pack { .. })) => {
                let (motivo, parts) = self.run_from_file(p, dialog.typed.text(), backend, buzon);
                rehusado = motivo;
                outputs.extend(parts);
            }
            // #311: copy the checksum list. The bytes were assembled when
            // the dialog opened, with coreutils escaping: what is painted
            // is sanitized, and copying THAT would give a `SHA256SUMS` that
            // does not verify the files it names.
            Some(Pending::CopyChecksums { bytes }) => {
                // How many LINES it carries: it is the number the message
                // shows, and the payload always ends in a newline.
                let count = bytes.split(|b| *b == b'\n').count().saturating_sub(1);
                if self.nativo(crate::dto::NativeEffect::CopyBytes { bytes, count }) {
                    outputs.extend(self.say("msg-checksum-copied"));
                } else {
                    // Nobody is listening on the native channel: there is no
                    // clipboard to copy to, and saying so is better than a
                    // button that does nothing.
                    rehusado = Some("host-no-desktop");
                }
            }
            Some(Pending::Permissions { targets }) => {
                let typed = dialog.typed.text().to_owned();
                let (motivo, parts) = self.change_permissions(targets, &typed, backend, buzon);
                rehusado = motivo;
                outputs.extend(parts);
            }
            Some(Pending::Patron { mark }) => {
                let patron = dialog.typed.text().to_owned();
                let (motivo, parts) = self.apply_patron(mark, &patron);
                rehusado = motivo;
                outputs.extend(parts);
            }
            Some(Pending::UndoSession {
                session: the_session,
            }) => {
                let (motivo, parts) = self.undo_session(&the_session, backend, buzon);
                rehusado = motivo;
                outputs.extend(parts);
            }
            Some(Pending::UndoUntil { seq, techo }) => {
                outputs.extend(self.undo_until(seq, techo, backend, buzon));
            }
            Some(Pending::ApproveExtension {
                id,
                capabilities,
                digest,
            }) => {
                let (motivo, parts) = self.grant(&id, &capabilities, digest, backend, buzon);
                rehusado = motivo;
                outputs.extend(parts);
            }
            Some(Pending::UninstallExtension { id }) => {
                outputs.extend(self.gobernar(&id, Governance::Uninstall, backend, buzon));
            }
            Some(Pending::Decide {
                approval_id,
                session,
            }) => {
                // And it notes WHO was told yes from here: the agents
                // panel's row distinguishes "asked N times" from "M were
                // approved", which are not the same when a different window
                // answered, when it was denied, or when it timed out.
                if let Some(the_session) = &session {
                    self.agencia.sessions.approved(the_session);
                    if self.agencia.panel {
                        let change = ViewChange::Agents {
                            agents: self.vista_agents(),
                        };
                        outputs.push(self.parche(vec![change]));
                    }
                }
                // Only `approve` approves. Any other answer — and closing the
                // dialog — DENIES: a security decision has no default answer
                // that says "yes".
                //
                // And if the yes does NOT arrive, it is reported. A
                // `policy.decide` that fails — the daemon went down between
                // the question and the answer — leaves the operation denied
                // by silence while this window assumes it authorized it: "I
                // said it" and "it arrived" are not the same thing on a
                // security surface. Denying is the opposite: if that one
                // does not arrive, the outcome is the same one that was
                // requested.
                launch_approval(approval_id, backend, buzon);
            }
            // A collision is not answered with "confirm": each output IS a
            // policy, and the one that translates them is `responder_dialog`,
            // which knows which one was pressed. Reaching here would be an
            // answer this dialog never offered, and those are not
            // interpreted.
            Some(Pending::Retry { .. }) | None => {}
        }
        (rehusado, outputs)
    }

    /// A TYPED name, as a `Segment`, or the key for the reason it is not
    /// valid.
    ///
    /// The replacement-character guard lives here and not only in rename
    /// because they share the RETURN path: `write_in_dialog` projects
    /// `clamp_display(display_name(text))` on every keystroke, and the
    /// renderer re-seeds the field with that projection if it had to rebuild
    /// the node. Without the guard, creating a directory would write to disk
    /// the U+FFFD the screen had put there.
    pub(super) fn segment_typed(name: &str) -> Result<norte_proto::Segment, &'static str> {
        if name.contains('\u{FFFD}') {
            return Err("msg-transfer-name-fffd");
        }
        norte_proto::Segment::new(name.as_bytes().to_vec()).map_err(|_| "err-bad-name")
    }

    pub(super) fn responder_dialog(
        &mut self,
        id: ModalId,
        choice: &str,
        secret: Option<&str>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(pos) = self.dialogs.iter().position(|d| d.id == id) else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        // An answer the dialog did not offer is not interpreted: there are
        // no implicit answers on a decision surface.
        if !self.dialogs[pos]
            .vista
            .choices
            .iter()
            .any(|c| c.id == choice)
        {
            return (Self::stale(StaleAction::Modal), Vec::new());
        }
        // The FIRST answer to a dialog that opened ON ITS OWN does not
        // answer it: it only acknowledges it. Lives HERE and not on the key
        // path because the mouse is this surface's primary input: the
        // dialog paints in the same spot as the previous one and with the
        // same first option, so a click already in flight over "Confirm"
        // used to land on the "Approve" of an agent approval that had just
        // arrived.
        //
        // Answers that DENY are exempt for the same reason as `Escape`:
        // shrugging off something you did not ask for has to work on the
        // first try, and denying is the safe outcome.
        if !self.dialogs[pos].recognized && choice != "deny" && choice != "cancel" {
            self.dialogs[pos].recognized = true;
            self.status.message = Some(clamp_display(norte_i18n::t_in(
                self.lang,
                "host-dialog-acknowledge",
            )));
            return (
                self.applied(),
                vec![self.parche(vec![ViewChange::Status(self.status.clone())])],
            );
        }
        if let Some(rejection) = self.rejects_for_read_only(pos) {
            return rejection;
        }
        // Confirming with an EMPTY password field is inert: it does not
        // deliver and does not close (#327). Delivering the empty string
        // reproduces #320 — an empty secret makes the connection
        // authenticate with the ambient string, i.e. with an identity nobody
        // asked for — and closing the dialog would turn a finger getting
        // ahead of itself into an abandoned navigation.
        //
        // Lives HERE, before the `remove`, and not inside `run_pending`:
        // there, the dialog has already left the stack and "do not close" is
        // no longer an option. It is the same spot the TUI chose
        // (`ALLOW_ASK_SECRET`).
        //
        // What JUST arrived is judged, not a stored state: the host keeps
        // none, so the field the reader sees empty is exactly the one being
        // evaluated. With a buffer in the host the two could differ — a
        // dialog stacked on top discards the field's node, and coming back
        // it paints empty over a buffer that was not — and then "confirming
        // over an empty field does nothing" would stop being true exactly
        // where it had been promised.
        if choice == "confirm" && matches!(self.dialogs[pos].typed, Typed::Secret) {
            if secret.is_none_or(str::is_empty) {
                return (
                    ActionAck::Unavailable {
                        reason_key: "host-secret-empty".to_owned(),
                    },
                    Vec::new(),
                );
            }
            // And one that does not fit is REJECTED, not trimmed. Trimming
            // was worse than the limit: delivering the first 256 characters
            // of a longer passphrase fails authentication without saying
            // why, and the reader has no way to suspect it — the field is
            // masked.
            if secret.is_some_and(|s| s.chars().count() > SECRET_MAX_CHARS) {
                return (
                    ActionAck::Unavailable {
                        reason_key: "host-secret-too-long".to_owned(),
                    },
                    Vec::new(),
                );
            }
        }
        // A FORM is validated here, before the `remove`, and for the same
        // reason as the secret above: inside `run_pending` the dialog
        // has already left the stack and "do not close" stops being an
        // option.
        if choice == "confirm"
            && let Some(rehuso) = self.rejects_form_invalid(pos)
        {
            return rehuso;
        }
        let dialog = self.dialogs.remove(pos);
        let mut outputs = Vec::new();
        // `confirm` is the affirmative answer for normal dialogs; `approve`,
        // for an approval. Different names on purpose: on a security
        // surface, "confirm" and "approve" should never be able to get
        // confused in a renderer.
        let mut rehusado = None;
        if choice == "confirm" || choice == "approve" {
            let (motivo, parts) = self.run_pending(dialog, secret, backend, buzon);
            rehusado = motivo;
            outputs.extend(parts);
        } else if let Some(Pending::Decide { approval_id, .. }) = dialog.on_confirm {
            // Deny explicitly, and also on close: leaving the agent waiting
            // on an answer that never arrives is worse than telling it no.
            let backend = Arc::clone(backend);
            tokio::spawn(async move {
                let _ = backend.policy_decide(approval_id, false).await;
            });
        } else if let Some(Pending::Retry { con }) = &dialog.on_confirm {
            // A collision's four outputs are not "confirm" (#274): each one
            // IS a different policy, and which one was pressed is the whole
            // answer. `cancel` translates to none of them and so nothing is
            // relaunched — the failed task stays as it was.
            let enqueue = self.enqueue;
            if let Some(policy) = collision_policy(choice) {
                Self::launch_retry(con.clone(), policy, enqueue, backend, buzon);
            }
        }
        let change = ViewChange::Dialogs {
            dialogs: self.dialog_views(),
        };
        outputs.push(self.parche(vec![change]));
        // A rejection is ACKNOWLEDGED as such. Answering `Applied` for a
        // name that was never written tells the renderer the operation
        // succeeded, and the same surface already answered `Unavailable`
        // when the rejection was for having several marks: two answers for
        // the same thing.
        match rehusado {
            Some(reason_key) => (
                ActionAck::Unavailable {
                    reason_key: reason_key.to_owned(),
                },
                outputs,
            ),
            None => (self.applied(), outputs),
        }
    }

    /// The most items a dialog's body shows.
    ///
    /// The body cannot grow with the selection — a batch of a thousand files
    /// does not fit in a question — so it is capped. That it was capped is
    /// said by [`Self::truncation_note`]: a list silently trimmed describes
    /// an operation smaller than the one about to run, and this is the last
    /// screen where saying no is still possible.
    pub(super) const MAX_LINES_DIALOG: usize = 16;

    /// The phrase saying the body shows less than there is. Empty if it
    /// shows all of them.
    pub(super) fn truncation_note(&self, shown: usize, total: usize) -> String {
        if shown >= total {
            return String::new();
        }
        clamp_display(norte_i18n::ta_in(
            self.lang,
            "dialog-body-truncated",
            &[("shown", &shown.to_string()), ("total", &total.to_string())],
        ))
    }

    /// A path as a dialog LINE: masked, clamped, and saying whether what is
    /// painted differs from the real thing.
    ///
    /// A single function because the five dialogs that show paths — create,
    /// search, delete, transfer and approve — have to say it the same way,
    /// and the spot where one of them forgets the `bool` is exactly where
    /// someone approves something else.
    pub(super) fn path_line(p: &VPath) -> crate::dto::DialogLine {
        let (text, hostile) = norte_frontend::path_display(p);
        // TRIMMING also alters what is painted, and it happens AFTER
        // `path_display`'s verdict: a clean, long UTF-8 path — twelve
        // 255-byte segments are enough — used to be painted with a trailing
        // `…` and declared faithful. An ellipsis is a legal character in a
        // name, so a reader cannot tell "that is its name" from "this got
        // cut", and in a batch's report that name is the only actionable
        // thing there is: it is about to be typed by hand.
        let cropped = text.len() > crate::bridge::MAX_STRING_BYTES;
        crate::dto::DialogLine {
            text: clamp_display(text),
            hostile: hostile || cropped,
        }
    }

    /// Like [`Self::path_line`], but with a specific REINTERPRETATION:
    /// the one in force when the operation this is asking about was
    /// launched.
    ///
    /// Two things that were done wrong where a collision is asked about, and
    /// both make something else get approved:
    ///
    /// - it used to mask over `display_lossy()`, which had ALREADY put in
    ///   the U+FFFDs. `display_name` then received flawless UTF-8 and
    ///   declared the path FAITHFUL, so the hostile badge did not show — on
    ///   the one screen where overwriting a file is approved;
    /// - `pane.names-encoding` was not applied, so on a cp866 pane the
    ///   terminal asked about `Папка` and the window about `??????`.
    ///   Approving a name that is not the one you have been looking at is
    ///   not approving.
    ///
    /// The encoding arrives as a PARAMETER and is not read from the active
    /// slot: the collision appears asynchronously, on top of whatever the
    /// reader is doing, and a slot change or an encoding cycle fits between
    /// the send and the question. Whoever launches captures it
    /// (`Retry::enc`).
    ///
    /// And the WHOLE path, not the name: "notas.txt" does not say WHICH
    /// notas.txt, and with two panes and a batch that is exactly the
    /// question.
    pub(super) fn line_with_encoding(
        p: &VPath,
        enc: Option<norte_encoding::NameEncoding>,
    ) -> crate::dto::DialogLine {
        let (text, hostile) = norte_frontend::path_display_with(p, enc);
        // TRIMMING also alters what is painted, and an ellipsis is a legal
        // character in a name: same reasoning as `path_line`.
        let cropped = text.len() > crate::bridge::MAX_STRING_BYTES;
        crate::dto::DialogLine {
            text: clamp_display(text),
            hostile: hostile || cropped,
        }
    }

    pub(super) fn dialog_views(&self) -> Vec<DialogView> {
        self.dialogs.iter().map(|d| d.vista.clone()).collect()
    }
}
