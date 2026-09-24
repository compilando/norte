//! The rename plan a model proposes, and its review.
//!
//! Part of `controller`: these are `State` methods, moved here without
//! touching them (ADR 0086). The only writer is still the actor.

// These modules are the same `impl State` split into pieces, so they use
// the same imports as the parent. Enumerating them here would be a
// forty-line list per file, in 32 files, that goes stale the moment the
// parent imports something — `super::*` tracks it on its own.
#[allow(clippy::wildcard_imports)]
use super::*;

/// The rename plan a model proposed, while it is under review.
///
/// Stores the already-validated PAIRS and not the text the daemon answered
/// with: validation is a fail-loud belt (`norte_frontend::validate_ai_plan`)
/// and a single pair that is not a legal `Segment` brings down the whole
/// batch, so whatever survives to here is already applicable byte for byte.
pub(super) struct RevisionIa {
    /// The directory the plan was made over.
    dir: VPath,
    /// What the model proposed, exactly as it answered.
    entries: Vec<norte_proto::methods::AiRenameEntry>,
    /// The same pairs in the shape the core asks for. It is what is sent to
    /// request the verdict AND what is sent to execute: both trips carry the
    /// SAME intent, which is what makes the `plan_hash` mean something.
    pairs: Vec<norte_proto::methods::RenamePair>,
    /// The core's verdict. Born `Pending` — the review opens and gets filled
    /// in — because checking it against the directory is another trip.
    plan: norte_frontend::BatchPlan,
    /// First visible pair: the review covers the whole plan, by scroll.
    first: usize,
    /// How far the reader has GOTTEN. Approving requires it: the review is
    /// the entire defense against a plan written from names controlled by
    /// whoever writes to the directory, and with five pairs visible out of
    /// two hundred fifty-six that defense covered 2%.
    seen_until: usize,
    /// This review has already been SHOWN at least once, so the next key is
    /// an answer and not a key meant for somewhere else.
    ///
    /// The screen opens ON ITS OWN, completely, tens of seconds after the
    /// gesture that requested it, and it keeps the keyboard. Without this,
    /// the `y` from someone typing `yes.txt` into the quick filter would
    /// approve the whole directory's rename.
    reconocida: bool,
    /// The epoch that requested it.
    epoch: u64,
}

impl State {
    /// Opens the instruction prompt for a rename plan.
    ///
    /// What is typed is NOT a name: it is what is being asked of a model.
    /// Nothing mutates here, and that is why the prompt does not carry the
    /// byte discipline the rename one does — the text is for the daemon, not
    /// for the disk.
    pub(super) fn request_instruction_ia(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let dir = self.slot().pane.dir().clone();
        self.request_ai_instruction_about(dir)
    }

    /// Like [`Self::request_instruction_ia`], but over a GIVEN directory.
    ///
    /// Exists to reopen the field after an empty instruction: there, the
    /// operand is already in hand, and deriving it again from the active
    /// slot would reopen over a different place if something moved it
    /// underneath.
    pub(super) fn request_ai_instruction_about(
        &mut self,
        dir: VPath,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let id = ModalId(self.next_modal);
        self.next_modal += 1;
        let vista = DialogView {
            id,
            title_key: "modal-ai-rename".to_owned(),
            destination: None,
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body: vec![Self::path_line(&dir)],
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
            input: Some(String::new()),
            input_hostile: false,
            input_secret: false,
            fields: Vec::new(),
            dest_check: crate::dto::DestCheckView::NotAsked,
        };
        self.dialogs.push(Dialog {
            id,
            vista: vista.clone(),
            typed: Typed::Text(String::new()),
            recognized: true,
            on_confirm: Some(Pending::InstructionIa { dir }),
        });
        let change = ViewChange::Dialogs {
            dialogs: self.dialog_views(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// Opens the batch rename TEMPLATE prompt (#310), prefilled with
    /// `[N].[E]` — the name exactly as it is — or with what was typed if it
    /// reopens after a diagnostic.
    ///
    /// Prefilled with identity and not blank, like the TUI: that way the
    /// first thing seen is the shape a template has. The names it acts on
    /// are fixed HERE — what is marked, or the cursor's — the same operand
    /// as any other operation; only the ones that are text, because a plan
    /// pair travels UTF-8 by protocol.
    pub(super) fn request_batch_template(
        &mut self,
        siembra: Option<String>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let dir = self.slot().pane.dir().clone();
        let names: Vec<String> = self
            .slot()
            .pane
            .marked_paths()
            .iter()
            .filter_map(|p| p.file_name())
            .filter_map(|s| String::from_utf8(s.as_bytes().to_vec()).ok())
            .collect();
        if names.is_empty() {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-rename-batch-nothing".to_owned(),
                },
                Vec::new(),
            );
        }
        let texto = siembra.unwrap_or_else(|| "[N].[E]".to_owned());
        let id = ModalId(self.next_modal);
        self.next_modal += 1;
        let vista = DialogView {
            id,
            title_key: "modal-rename-batch".to_owned(),
            destination: None,
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body: vec![
                Self::path_line(&dir),
                crate::dto::DialogLine {
                    text: clamp_display(norte_i18n::t_in(self.lang, "modal-rename-batch-hint")),
                    hostile: false,
                },
            ],
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
            input: Some(texto.clone()),
            input_hostile: false,
            input_secret: false,
            fields: Vec::new(),
            dest_check: crate::dto::DestCheckView::NotAsked,
        };
        self.dialogs.push(Dialog {
            id,
            vista: vista.clone(),
            typed: Typed::Text(texto),
            recognized: true,
            on_confirm: Some(Pending::TemplateBatch { dir, names }),
        });
        let change = ViewChange::Dialogs {
            dialogs: self.dialog_views(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// Generates the template's plan and puts it into the SAME review as the
    /// AI's (#310): what makes the operation safe is not where the names
    /// came from.
    ///
    /// An invalid template is explained in the status bar and the prompt
    /// reopens with what was typed, instead of discarding the text: the TUI
    /// leaves it open with the diagnostic underneath, and this is the same
    /// thing with dialogs that close on confirm.
    pub(super) fn launch_template_plan(
        &mut self,
        dir: VPath,
        names: &[String],
        template: &str,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Message>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let texto = template.trim().to_owned();
        if let Err(e) = norte_frontend::rename_pattern::check(&texto, names) {
            let key = norte_frontend::rename_pattern::error_key(e);
            self.status.message = Some(clamp_display(norte_i18n::t_in(self.lang, key)));
            let mut outputs = vec![self.parche(vec![ViewChange::Status(self.status.clone())])];
            let (_, reabierto) = self.request_batch_template(Some(texto));
            outputs.extend(reabierto);
            return outputs;
        }
        // Pairs that do NOT change are discarded, like in the TUI: an
        // identity plan renames nothing, and confirming without touching
        // the template is harmless.
        let entries: Vec<norte_proto::methods::AiRenameEntry> =
            norte_frontend::rename_pattern::plan(&texto, names, 1)
                .into_iter()
                .filter(|(from, to)| from != to)
                .map(|(from, to)| norte_proto::methods::AiRenameEntry { from, to })
                .collect();
        if entries.is_empty() {
            self.status.message = Some(clamp_display(norte_i18n::t_in(
                self.lang,
                "msg-rename-batch-no-changes",
            )));
            return vec![self.parche(vec![ViewChange::Status(self.status.clone())])];
        }
        // AGAINST the directory it was planned over, with the same belt as
        // the model's plan: a `from` that is not there does not get in.
        let del_dir: Vec<Vec<u8>> = self
            .slot()
            .pane
            .entries()
            .iter()
            .filter_map(|e| e.path.file_name().map(|s| s.as_bytes().to_vec()))
            .collect();
        self.epoch_ia += 1;
        let epoch = self.epoch_ia;
        let Some(parejas) = norte_frontend::rename_pairs_in(&entries, Some(&del_dir)) else {
            return self.ai_say(epoch, "msg-ai-rename-invalid-plan");
        };
        self.open_revision(epoch, dir, entries, parejas, backend, buzon)
    }

    /// A RENAMER row from the palette (C3, ADR 0095): asks the plugin for
    /// the plan over what is marked — or pointed at — and the answer comes
    /// in through `Background::PlanIa`, the same path as the AI's plan: same
    /// review, same core verdict, same `plan_hash`. What makes the
    /// operation safe is not who proposed the names.
    pub(super) fn run_from_renamer(
        &mut self,
        key: &str,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some((id, renamer)) = norte_frontend::palette::parse_renamer_key(key) else {
            return self.no_implementado(key);
        };
        if self.effects == crate::commands::Effects::SoloRead {
            // The plan ends in a rename: a window with no effects does not
            // request it.
            return Self::no_mutates();
        }
        let names: Vec<String> = self
            .slot()
            .pane
            .marked_paths()
            .iter()
            .filter_map(|p| p.file_name())
            .filter_map(|s| String::from_utf8(s.as_bytes().to_vec()).ok())
            .collect();
        if names.is_empty() {
            let said = self.say("msg-rename-batch-nothing");
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-rename-batch-nothing".to_owned(),
                },
                said,
            );
        }
        let dir = self.slot().pane.dir().clone();
        self.epoch_ia += 1;
        let epoch = self.epoch_ia;
        let del_dir: Vec<Vec<u8>> = self
            .slot()
            .pane
            .entries()
            .iter()
            .filter_map(|e| e.path.file_name().map(|s| s.as_bytes().to_vec()))
            .collect();
        self.ai_in_flight = Some((epoch, dir.clone(), del_dir));
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        let (id, renamer) = (id.to_owned(), renamer.to_owned());
        tokio::spawn(async move {
            let res = (tokio::time::timeout(
                DEADLINE_IA,
                backend.plugin_rename_plan(id, renamer, dir, names),
            )
            .await)
                .unwrap_or(Err(Error::ProviderUnavailable { retryable: true }));
            let _ = buzon
                .send(Message::Background(Box::new(Background::PlanIa(
                    epoch,
                    Box::new(res),
                ))))
                .await;
        });
        (self.applied(), self.say("host-plan-asking"))
    }

    /// Asks the model for the plan. The answer comes back to the actor.
    ///
    /// A new epoch per request: between asking for it and it arriving, the
    /// reader may have discarded the review or requested another one, and an
    /// old plan does not open over the current one.
    pub(super) fn launch_plan_ia(
        &mut self,
        dir: VPath,
        instruction: String,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Message>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        if instruction.trim().is_empty() {
            self.status.message = Some(clamp_display(norte_i18n::t_in(
                self.lang,
                "modal-ai-rename-empty-instruction",
            )));
            // And the field COMES BACK. The terminal leaves the modal open
            // with the error underneath; here the dialog had already left
            // the stack, so a message asking you to type an instruction on a
            // screen with nowhere to type it was not a refusal: it was a
            // dead end. The field was empty, so nothing is lost by rebuilding
            // it — and this is exactly what its twin case, the semantic
            // query, was already doing three files over.
            let (_, mut outside) = self.request_ai_instruction_about(dir);
            let change = ViewChange::Status(self.status.clone());
            outside.push(self.parche(vec![change]));
            return outside;
        }
        self.epoch_ia += 1;
        let epoch = self.epoch_ia;
        // The names of the directory being PLANNED over, saved with the
        // request: #275's belt requires every `from` to exist where it is
        // about to be applied, and by the time the model answers the reader
        // may be somewhere else. Asking the pane then would validate the
        // plan against a directory that is not its own.
        let names: Vec<Vec<u8>> = self
            .slot()
            .pane
            .entries()
            .iter()
            .filter_map(|e| e.path.file_name().map(|s| s.as_bytes().to_vec()))
            .collect();
        self.ai_in_flight = Some((epoch, dir.clone(), names));
        // What is MARKED, if there are marks (#121): a plan over five files
        // cannot send the provider the thousand in the directory. Names
        // that are not UTF-8 stay out — the wire carries them as text and
        // the engine rejects them fail-loud before sending anything — so
        // marking them and asking for a plan means asking about the rest,
        // not about the whole directory.
        // `marked_entries` and NOT `marked_paths`: the latter falls back to
        // the cursor when there are no marks, and here that would turn
        // "nothing marked" — which means the whole directory — into "this
        // one lone file".
        let marked: Vec<String> = self
            .slot()
            .pane
            .marked_entries()
            .iter()
            .filter_map(|e| e.path.file_name())
            .filter_map(|s| String::from_utf8(s.as_bytes().to_vec()).ok())
            .collect();
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let res = (tokio::time::timeout(
                DEADLINE_IA,
                backend.ai_rename_plan(dir, instruction, marked),
            )
            .await)
                .unwrap_or(Err(Error::ProviderUnavailable { retryable: true }));
            let _ = buzon
                .send(Message::Background(Box::new(Background::PlanIa(
                    epoch,
                    Box::new(res),
                ))))
                .await;
        });
        // And it IS SAID that it is being requested. Without this the key
        // produced nothing visible, so the reader pressed it again — which
        // is exactly what exposed the race between the two requests.
        self.status.message = Some(clamp_display(norte_i18n::t_in(
            self.lang,
            "host-plan-asking",
        )));
        let change = ViewChange::Status(self.status.clone());
        vec![self.parche(vec![change])]
    }

    /// What the model answered, reviewed before showing it.
    ///
    /// Two belts, and both are about INGESTION — not presentation — so they
    /// reject the WHOLE BLOCK and do not even open the review:
    ///
    /// - a plan with more pairs than a directory can have gives away a
    ///   hostile daemon inflating the answer;
    /// - a pair that is not a legal `Segment` gives away a broken or
    ///   tampered one, and applying "whatever is valid" from a tampered plan
    ///   is exactly what must not be done.
    pub(super) fn apply_plan_ia(
        &mut self,
        epoch: u64,
        res: Result<norte_proto::methods::AiRenamePlanResult, Error>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Message>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // The IN-FLIGHT request has to be this one. A plan from a different
        // epoch is one the reader abandoned, and opening it is the
        // application moving on its own.
        // `take_if` and NOT `take().filter(...)`: `take` empties the slot
        // BEFORE the filter even looks, so an OLD answer used to sweep away
        // the LIVE request. The sequence was normal — ask, see nothing, ask
        // again — and both ended up unopened, saying nothing, and
        // indistinguishable from a dead daemon.
        let Some((_, dir, names)) = self.ai_in_flight.take_if(|(e, _, _)| *e == epoch) else {
            return Vec::new();
        };
        let plan = match res {
            Ok(p) => p,
            Err(e) => return self.ai_say(epoch, norte_frontend::error::error_key(&e)),
        };
        // The producer said WHY it does not propose (#332): a renamer that
        // refused. The phrase arrives already masked and clamped by the
        // daemon, and here it is shown, not interpreted.
        if let Some(why) = plan.refused {
            self.status.message = Some(clamp_display(norte_i18n::ta_in(
                self.lang,
                "msg-rename-plan-refused",
                &[("why", &why)],
            )));
            let mut changes = vec![ViewChange::Status(self.status.clone())];
            if self.revision_ia.take_if(|r| r.epoch == epoch).is_some() {
                changes.push(ViewChange::AiRename { ai_rename: None });
            }
            return vec![self.parche(changes)];
        }
        if plan.entries.is_empty() {
            return self.ai_say(epoch, "msg-ai-rename-empty");
        }
        if plan.entries.len() > norte_frontend::MAX_AI_PLAN_ENTRIES {
            return self.ai_say(epoch, "msg-ai-rename-invalid-plan");
        }
        // AGAINST the directory it was PLANNED over (#275), not against
        // what the pane shows now: a tampered plan cannot rename something
        // that was not there, and the reader may have gone somewhere else
        // while the model was thinking.
        let Some(parejas) = norte_frontend::rename_pairs_in(&plan.entries, Some(&names)) else {
            return self.ai_say(epoch, "msg-ai-rename-invalid-plan");
        };
        self.open_revision(epoch, dir, plan.entries, parejas, backend, buzon)
    }

    /// Opens the review of a plan — the model's or a template's (#310) — and
    /// asks the core for the verdict IN THE SAME trip: the review needs the
    /// `plan_hash` for approving to do anything, and a plan left waiting for
    /// someone to ask for it later would have nobody to. It is spawned
    /// because against a huge directory it is a whole `fs.list`, and
    /// awaiting it here would freeze the actor.
    fn open_revision(
        &mut self,
        epoch: u64,
        dir: VPath,
        entries: Vec<norte_proto::methods::AiRenameEntry>,
        parejas: Vec<norte_proto::methods::RenamePair>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Message>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let b = Arc::clone(backend);
        let buz = buzon.clone();
        let d = dir.clone();
        let p = parejas.clone();
        tokio::spawn(async move {
            let res = b.rename_batch_plan(d, p).await;
            let _ = buz
                .send(Message::Background(Box::new(Background::BatchPlan(
                    epoch,
                    Box::new(res),
                ))))
                .await;
        });
        self.revision_ia = Some(RevisionIa {
            dir,
            entries,
            pairs: parejas,
            plan: norte_frontend::BatchPlan::Pending,
            first: 0,
            seen_until: norte_frontend::AI_RENAME_PAIR_LIMIT,
            reconocida: false,
            epoch,
        });
        let change = ViewChange::AiRename {
            ai_rename: self.vista_ia(),
        };
        vec![self.parche(vec![change])]
    }

    /// The core's verdict on the plan under review.
    pub(super) fn apply_batch_plan(
        &mut self,
        epoch: u64,
        res: Result<norte_proto::methods::FsRenameBatchPlanResult, Error>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Some(r) = self.revision_ia.as_mut().filter(|r| r.epoch == epoch) else {
            return Vec::new();
        };
        let failure = res.as_ref().err().map(norte_frontend::error::error_key);
        r.plan = match res {
            Ok(p) => norte_frontend::BatchPlan::Ready(Box::new(p)),
            // `Failed` is not "not applicable": it is "there is no plan",
            // i.e. there is no approved `plan_hash` to send. The specific
            // reason goes to the status bar; here all that is known is that
            // approving cannot do anything.
            Err(_) => norte_frontend::BatchPlan::Failed,
        };
        let mut changes = vec![ViewChange::AiRename {
            ai_rename: self.vista_ia(),
        }];
        if let Some(key) = failure {
            self.status.message = Some(clamp_display(norte_i18n::t_in(self.lang, key)));
            changes.push(ViewChange::Status(self.status.clone()));
        }
        vec![self.parche(changes)]
    }

    /// The review's projection, or `None` if there is none.
    ///
    /// The names are proposed by a MODEL over names anyone could have
    /// written: both go through the canonical sanitizing and each one says
    /// whether what is painted differs from the real thing. And they travel
    /// WHOLE and separately, never concatenated with an arrow — same reason
    /// as a transfer's destination.
    pub(super) fn vista_ia(&self) -> Option<crate::dto::AiRenameView> {
        let r = self.revision_ia.as_ref()?;
        let line = |texto: &str| {
            let (pintable, hostil) = norte_frontend::display_name(texto.as_bytes());
            crate::dto::DialogLine {
                text: clamp_display(pintable),
                hostile: hostil,
            }
        };
        let pairs = r
            .entries
            .iter()
            .skip(r.first)
            .take(norte_frontend::AI_RENAME_PAIR_LIMIT)
            .map(|e| crate::dto::AiRenamePairView {
                from: line(&e.from),
                to: line(&e.to),
            })
            .collect();
        let total = r.entries.len();
        let until = (r.first + norte_frontend::AI_RENAME_PAIR_LIMIT).min(total);
        Some(crate::dto::AiRenameView {
            dir: Self::path_line(&r.dir),
            pairs,
            first_visible: r.first as u64,
            total: total as u64,
            more_note: if until >= total {
                String::new()
            } else {
                clamp_display(norte_i18n::ta_in(
                    self.lang,
                    "modal-ai-rename-more",
                    &[("shown", &until.to_string()), ("total", &total.to_string())],
                ))
            },
            // What is NOT visible is also said: a line's mark only exists
            // for that line, and the altered pair could be at position
            // twelve.
            hidden_hostile: r
                .entries
                .iter()
                .enumerate()
                .filter(|(i, _)| *i < r.first || *i >= until)
                .any(|(_, e)| {
                    norte_frontend::display_name(e.from.as_bytes()).1
                        || norte_frontend::display_name(e.to.as_bytes()).1
                }),
            // Translated HERE: a renderer does not translate, and of the
            // whole body this is the line that cannot be lost.
            status: clamp_display(norte_i18n::t_in(self.lang, r.plan.status_key())),
            // The detail comes ENTIRELY from the shared layer, marks
            // included: every surface paints names an attacker controls,
            // and one that derives it on its own is where the sanitizing
            // gets lost.
            detail: r
                .plan
                .detail_parts(r.pairs.len(), self.lang)
                .into_iter()
                .flat_map(|part| self.detail_lines(&part))
                .collect(),
            // Approving requires BOTH things: that the core accepts it and
            // that the reader has reached the end. The core cannot know the
            // second and the reader cannot know the first. The rule lives in
            // the SHARED crate since it was noticed the terminal only asked
            // for the first: a signature over something not read is not a
            // signature, and with two hundred renames the ones that matter
            // can be at row one hundred eighty.
            confirmable: norte_frontend::approval_ready(r.plan.confirmable(), r.seen_until, total),
            real_steps_note: if r.plan.ready().is_none() {
                String::new()
            } else {
                clamp_display(norte_i18n::ta_in(
                    self.lang,
                    "modal-ai-rename-real-steps",
                    &[("n", &r.plan.real_steps().to_string())],
                ))
            },
            seen_all: r.seen_until >= total,
        })
    }

    /// The keys while the review is open.
    ///
    /// FIXED, like the palette's and help's, and for the same reason: the
    /// catalog has no commands for "scroll through this plan" or "approve
    /// it". They are the ones the screen itself announces in its footer.
    pub(super) fn key_in_ai_review(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // A chord WITH a modifier is not an answer to this screen: it is a
        // key meant for somewhere else. `key_in_quick` refuses them for
        // the same reason, and here it matters more — `ctrl+y` used to
        // approve a batch.
        if k.ctrl || k.alt || k.meta {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-key-unmapped".to_owned(),
                },
                Vec::new(),
            );
        }
        // The FIRST key only acknowledges the screen. It opens on its own,
        // tens of seconds after the gesture that requested it, and it keeps
        // the keyboard: without this step, the key the reader meant to send
        // somewhere else answered a question they did not yet know they had
        // in front of them. `Escape` is the exception and needs no
        // acknowledgment: discarding is safe in both states, and whoever
        // does not want this has to be able to shrug it off on the first
        // try.
        let reconocida = self.revision_ia.as_ref().is_some_and(|r| r.reconocida);
        if !reconocida && k.key != "Escape" && k.key != "esc" {
            if let Some(r) = self.revision_ia.as_mut() {
                r.reconocida = true;
            }
            self.status.message = Some(clamp_display(norte_i18n::t_in(
                self.lang,
                "host-plan-acknowledge",
            )));
            let changes = vec![
                ViewChange::AiRename {
                    ai_rename: self.vista_ia(),
                },
                ViewChange::Status(self.status.clone()),
            ];
            return (self.applied(), vec![self.parche(changes)]);
        }
        let total = self.revision_ia.as_ref().map_or(0, |r| r.entries.len());
        let window = norte_frontend::AI_RENAME_PAIR_LIMIT;
        let cap = total.saturating_sub(window);
        let page = i64::try_from(window).unwrap_or(1);
        let mover = |r: &mut RevisionIa, delta: i64| {
            let dest = i64::try_from(r.first).unwrap_or(0).saturating_add(delta);
            r.first = usize::try_from(dest.max(0)).unwrap_or(0).min(cap);
            // The watermark only GOES UP: scrolling back does not undo what
            // has already been read.
            r.seen_until = r.seen_until.max((r.first + window).min(total));
        };
        match k.key.as_str() {
            "ArrowDown" | "j" => {
                if let Some(r) = self.revision_ia.as_mut() {
                    mover(r, 1);
                }
            }
            "ArrowUp" | "k" => {
                if let Some(r) = self.revision_ia.as_mut() {
                    mover(r, -1);
                }
            }
            "PageDown" => {
                if let Some(r) = self.revision_ia.as_mut() {
                    mover(r, page);
                }
            }
            "PageUp" => {
                if let Some(r) = self.revision_ia.as_mut() {
                    mover(r, -page);
                }
            }
            "Escape" | "n" | "N" => return self.close_revision_ia(),
            // `Enter` does NOT approve, and this deliberately breaks parity
            // with the TUI. There, the plan is opened by a reader's key and
            // the next key is an answer; here the screen opens on its own
            // tens of seconds later, and `Enter` is exactly the key being
            // used to walk the tree while the model was thinking. Two
            // `Enter`s in a row entering nested directories are normal;
            // the second one approving a batch rename is not. What is left
            // is `y` — which acknowledgment protects — and the button, a
            // gesture that cannot be confused with anything else.
            "y" | "Y" => return self.approve_revision_ia(backend, buzon),
            _ => {
                return (
                    ActionAck::Unavailable {
                        reason_key: "host-key-unmapped".to_owned(),
                    },
                    Vec::new(),
                );
            }
        }
        let change = ViewChange::AiRename {
            ai_rename: self.vista_ia(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// Answers the review with a gesture AIMED at it (a button).
    ///
    /// It does not need the acknowledgment a key does: a click on this
    /// screen's button cannot be a gesture meant for somewhere else.
    pub(super) fn decide_revision_ia(
        &mut self,
        approve: bool,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.revision_ia.is_none() {
            return (Self::stale(StaleAction::Modal), Vec::new());
        }
        if let Some(r) = self.revision_ia.as_mut() {
            r.reconocida = true;
        }
        if approve {
            self.approve_revision_ia(backend, buzon)
        } else {
            self.close_revision_ia()
        }
    }

    /// Discards the plan without applying anything.
    pub(super) fn close_revision_ia(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // The epoch is not bumped, and `ai_in_flight` is not touched. Both
        // look like caution and one of them was a bug:
        //
        // - The epoch is not needed. A late verdict no longer finds a
        //   review to update, and a NEW request bumps the epoch itself.
        // - `ai_in_flight` CANNOT be this review's request: `apply_plan_ia`
        //   took it when opening it. If there is something there it is a
        //   LATER request, and clearing it here would silently kill it —
        //   discarding a plan being read is not abandoning the one that was
        //   just requested.
        self.revision_ia = None;
        let change = ViewChange::AiRename { ai_rename: None };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// Approves the plan: ONE Task for the whole batch, a single undo.
    ///
    /// Only if the CORE marked it applicable, and with the `plan_hash` it
    /// itself returned: what runs is exactly what was shown. A plan with no
    /// verdict, or with one that says no, is not approved and is reported.
    pub(super) fn approve_revision_ia(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(r) = self.revision_ia.as_ref() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        // The second lock, here too. `rejects_for_read_only` looks at
        // DIALOGS, and this is a screen of its own: today it is unreachable
        // in read-only because the two doors that open it are closed, but
        // that is exactly the condition that stops holding the day someone
        // adds a third one. Approving a plan executes N moves.
        if self.effects == crate::commands::Effects::SoloRead {
            return Self::no_mutates();
        }
        if r.seen_until < r.entries.len() {
            // And WHICH of the two things is missing is stated: "the core
            // does not accept it" and "you haven't read it all yet" are
            // fixed in different ways.
            return (
                ActionAck::Unavailable {
                    reason_key: "host-plan-unseen".to_owned(),
                },
                Vec::new(),
            );
        }
        let Some(plan) = r.plan.ready().filter(|p| p.executable) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-plan-not-applicable".to_owned(),
                },
                Vec::new(),
            );
        };
        let (dir, parejas, hash) = (r.dir.clone(), r.pairs.clone(), plan.plan_hash.clone());
        let afectados = vec![dir.clone()];
        let backend2 = Arc::clone(backend);
        let buzon2 = buzon.clone();
        tokio::spawn(async move {
            let message = match backend2.rename_batch(dir, parejas, hash).await {
                Ok(task) => Message::TaskNew(Box::new((task, afectados, None))),
                Err(e) => Message::TaskFailed(Box::new(e)),
            };
            let _ = buzon2.send(message).await;
        });
        self.close_revision_ia()
    }

    /// States it in the status bar and opens nothing. Closes the review if
    /// there was one: a plan that could not be requested does not leave half
    /// a screen open.
    pub(super) fn ai_say(&mut self, epoch: u64, key: &str) -> Vec<BridgeEnvelope<UiUpdate>> {
        self.status.message = Some(clamp_display(norte_i18n::t_in(self.lang, key)));
        let mut changes = vec![ViewChange::Status(self.status.clone())];
        // Only THIS epoch's review is closed. Closing whichever one there
        // was would drop a good plan, already with a verdict and about to
        // be approved, because ANOTHER, later request had failed.
        if self.revision_ia.take_if(|r| r.epoch == epoch).is_some() {
            changes.push(ViewChange::AiRename { ai_rename: None });
        }
        vec![self.parche(changes)]
    }

    /// Opens the entry under the cursor's name, to edit it. Does NOT rename.
    ///
    /// With SEVERAL marks it refuses, and that is NOT what the TUI does: the
    /// TUI renames the cursor's and ignores the marks. The shared table
    /// documents the asymmetry in `Facts::rename_single` and lets each
    /// frontend answer; this host already answered "one only" in `facts()`,
    /// so dimming the row and renaming anyway would have been help lying
    /// about the key.
    pub(super) fn request_rename(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let slot = self.slot();
        if slot.pane.marks_len() > 1 {
            return (
                ActionAck::Unavailable {
                    reason_key: norte_frontend::availability::reason_key(
                        norte_help::Reason::WrongTarget,
                    )
                    .to_owned(),
                },
                Vec::new(),
            );
        }
        let Some(from) = slot.pane.selected().map(|e| e.path.clone()) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-nothing-selected".to_owned(),
                },
                Vec::new(),
            );
        };
        let Some(name) = from.file_name() else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-cannot-transfer-root".to_owned(),
                },
                Vec::new(),
            );
        };
        // The seed is what the ROW paints, with the canonical sanitizing AND
        // with whatever reinterpretation the pane has set: editing produces
        // the text that is seen, and since #57 the row can be transcoded.
        // Seeding without it left `CAF<FFFD>.TXT` under a row that said
        // `CAFÉ.TXT`. For a name still not representable that carries a
        // U+FFFD, and that residue is exactly what the confirmation's guard
        // does not let through.
        let (pintable, hostil) =
            norte_frontend::display_name_with(name.as_bytes(), slot.pane.name_encoding());
        let siembra = clamp_display(pintable.clone());
        if siembra != pintable {
            // Trimming tacks an ellipsis onto the end, and `…` is a LEGAL
            // character in a name: neither masked nor flagged. Editing that
            // field and confirming would write the trim to disk as part of
            // the name, with nothing to say so — and the U+FFFD guard does
            // not see it, because the trim happens AFTER `display_name` has
            // given its verdict. Opening it is refused, the only honest
            // option: the name does not fit, so it cannot be edited here.
            return (
                ActionAck::Unavailable {
                    reason_key: "host-name-not-editable".to_owned(),
                },
                Vec::new(),
            );
        }
        let id = ModalId(self.next_modal);
        self.next_modal += 1;
        let vista = DialogView {
            id,
            title_key: "modal-rename-title".to_owned(),
            // A rename goes nowhere: it stays where it is.
            destination: None,
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body: vec![Self::path_line(&from)],
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
            input: Some(siembra.clone()),
            input_hostile: hostil,
            input_secret: false,
            fields: Vec::new(),
            dest_check: crate::dto::DestCheckView::NotAsked,
        };
        self.dialogs.push(Dialog {
            id,
            vista: vista.clone(),
            // The raw value starts the SAME as the seed: that is what makes
            // it possible to recognize "has not touched it" without
            // carrying a separate flag.
            typed: Typed::Text(siembra.clone()),
            recognized: true,
            on_confirm: Some(Pending::Rename { from, siembra }),
        });
        let change = ViewChange::Dialogs {
            dialogs: self.dialog_views(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// The bytes a confirmed rename is about to write, or the key for the
    /// reason there are none.
    ///
    /// Three rules, and all three are about rule 1:
    ///
    /// - **Untouched**, the ORIGINAL BYTES are rebuilt — and then the
    ///   destination is the source, so the result is always "same name,
    ///   same place". This branch renames NOTHING, and it exists so the seed
    ///   cannot turn into the operand: the screen projection is not
    ///   reversible for a name that is not UTF-8.
    ///
    ///   The consequence has to be stated because it is not obvious: a name
    ///   that is not valid UTF-8 **cannot be renamed from this window**.
    ///   Untouched gives "same name"; touched carries the U+FFFD the screen
    ///   put there and there is no way to type around it. It is fail-closed
    ///   and deliberate — the alternative would be writing real mojibake —
    ///   but it is a limitation, not a protection that actually works.
    /// - **Touched and carrying a U+FFFD**, it is refused: that character
    ///   was put there by the screen, and confirming it would write real
    ///   mojibake. The guard does not tell residue from intent apart, so it
    ///   also refuses a TYPED U+FFFD — a deliberate asymmetry with creating
    ///   a directory, which has no seed to inherit residue from.
    /// - **The same name in the same place** is not an operation.
    pub(super) fn bytes_del_rename(
        from: &VPath,
        siembra: &str,
        written: &str,
    ) -> Result<VPath, &'static str> {
        let bytes = if written == siembra {
            from.file_name()
                .map(|n| n.as_bytes().to_vec())
                .unwrap_or_default()
        } else {
            if written.contains('\u{FFFD}') {
                return Err("msg-transfer-name-fffd");
            }
            written.as_bytes().to_vec()
        };
        let seg = norte_proto::Segment::new(bytes).map_err(|_| "err-bad-name")?;
        let dest = from.parent().ok_or("host-cannot-transfer-root")?.join(seg);
        if dest == *from {
            return Err("msg-transfer-name-same");
        }
        Ok(dest)
    }
}
