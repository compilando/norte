//! The ORGANIZE plan and its review (phase 8 of the WOW programme).
//!
//! Part of `controller`: these are methods of `Estado`, with the same
//! discipline as the rename plan next door (ADR 0086). The only writer is
//! still the actor.
//!
//! **What changes compared to its twin, and why.** Rename is reviewed as a
//! list of pairs because that is what it is; organize changes the directory's
//! SHAPE, so what is reviewed is a TREE — the same one the terminal paints,
//! computed by [`norte_frontend::organize::tree_lines`]. And there is no
//! verdict to wait for: the plan's token travels WITH the plan, so this
//! screen is born approvable instead of opening in `Pending`.

// Same imports as the parent, for the same reason as the rest of the pieces.
#[allow(clippy::wildcard_imports)]
use super::*;

/// The ORGANIZE plan under review (phase 8).
///
/// The twin of [`super::ai::RevisionIa`] without its most expensive field:
/// there is no verdict to wait for, because the plan's token came WITH the
/// plan. Everything else is identical, and on purpose — it is the same class
/// of screen and the same defense.
pub(super) struct RevisionOrganizar {
    /// The directory it was planned over.
    dir: VPath,
    /// The moves, exactly as the producer proposed them. This is what is sent
    /// to execute, and what `plan_hash` binds.
    moves: Vec<norte_proto::methods::OrganizeMove>,
    /// The already computed tree, which is what is reviewed.
    lines: Vec<norte_frontend::organize::TreeLine>,
    /// The token that has to be returned to apply it.
    plan_hash: norte_proto::methods::PlanHash,
    /// First visible line: the review is of the whole tree, by scrolling.
    first: usize,
    /// How far the reader has GOTTEN TO. Approving requires it.
    seen_until: usize,
    /// It has already been shown at least once, so the next key is an answer
    /// and not a key meant for somewhere else.
    acknowledged: bool,
    /// The epoch that requested it.
    epoch: u64,
}

impl Estado {
    /// Requests an organize plan over the focused slot's directory.
    ///
    /// `organizer` chooses the producer: `None` is the model, `Some((plugin,
    /// organizer))` is an extension of kind `organizer`. Both end up at the
    /// SAME review, because what makes the operation safe is not where the
    /// names came from.
    ///
    /// **No instruction prompt**, unlike rename: what is being asked is "look
    /// at this directory and propose a shape", and an empty text box in front
    /// would suggest there is something to type.
    pub(super) fn pedir_plan_de_organizar(
        &mut self,
        organizer: Option<(String, String)>,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // Organize MUTATES (creates folders and moves), so the read-only lock
        // is checked on REQUESTING, not only on approving: showing a plan that
        // cannot be applied is promising work.
        if self.efectos == crate::commands::Efectos::SoloLectura {
            return Self::no_muta();
        }
        self.epoca_organizar += 1;
        let epoch = self.epoca_organizar;
        let dir = self.hueco().pane.dir().clone();
        // The directory's names, at REQUEST time: the tree needs to know
        // which folder already existed, and by the time the producer answers
        // the reader may be somewhere else. Only the ones that are TEXT — the
        // tree compares them against segments of a `proposed_rel`, which
        // travels UTF-8.
        let existing = self.hueco().pane.existing_names();
        // A plugin does NOT list the directory (rule 9), so the names have to
        // be given to it by the caller: with an empty list, an organizer
        // answers — correctly — that it moves nothing. The model is the
        // opposite case: the engine lists for it, and there, empty DOES mean
        // "everything", with no cap to respect.
        let operable = self.hueco().pane.organizable_names();
        if organizer.is_some() && operable.len() > norte_proto::methods::AI_RENAME_NAMES_MAX {
            self.status.message = Some(clamp_display(norte_i18n::t_in(
                self.lang,
                "msg-organize-too-many",
            )));
            let change = ViewChange::Status(self.status.clone());
            return (self.aplicada(), vec![self.parche(vec![change])]);
        }
        self.organizar_en_vuelo = Some((epoch, dir.clone(), existing));
        let backend2 = Arc::clone(backend);
        let mailbox2 = mailbox.clone();
        tokio::spawn(async move {
            let request = match organizer {
                Some((plugin, org)) => backend2.plugin_organize_plan(plugin, org, dir, operable),
                None => backend2.ai_organize_plan(dir, String::new(), Vec::new()),
            };
            let res = (tokio::time::timeout(PLAZO_IA, request).await)
                .unwrap_or(Err(Error::ProviderUnavailable { retryable: true }));
            let _ = mailbox2
                .send(Mensaje::Fondo(Box::new(Fondo::PlanOrganizar(
                    epoch,
                    Box::new(res),
                ))))
                .await;
        });
        // And it is SAID that it is being requested: without this the gesture
        // produces nothing visible and the reader repeats it, which is what
        // exposes the race between two requests.
        self.status.message = Some(clamp_display(norte_i18n::t_in(
            self.lang,
            "msg-organize-running",
        )));
        let change = ViewChange::Status(self.status.clone());
        (self.aplicada(), vec![self.parche(vec![change])])
    }

    /// What the producer answered, checked before showing it.
    ///
    /// The belts are for INGESTION and reject IN BULK: a plan bigger than
    /// what a directory can hold gives away a hostile daemon inflating the
    /// response, and a plan without a token cannot be approved — opening it
    /// would promise a button that can do nothing.
    pub(super) fn aplicar_plan_de_organizar(
        &mut self,
        epoch: u64,
        res: Result<norte_proto::methods::AiOrganizePlanResult, Error>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // The request in flight has to be THIS one. `take_if` and not
        // `take().filter(...)`: emptying the slot before checking would let an
        // old response take the live request down with it.
        let Some((_, dir, existing)) = self.organizar_en_vuelo.take_if(|(e, _, _)| *e == epoch)
        else {
            return Vec::new();
        };
        let plan = match res {
            Ok(p) => p,
            Err(e) => {
                return self.decir_de_organizar(epoch, norte_frontend::error::error_key(&e));
            }
        };
        // The producer said WHY it does not propose (#332). The phrase
        // arrives already masked and clamped by the daemon: here it is shown,
        // not interpreted.
        if let Some(why) = plan.refused {
            self.status.message = Some(clamp_display(norte_i18n::ta_in(
                self.lang,
                "msg-rename-plan-refused",
                &[("why", &why)],
            )));
            let mut changes = vec![ViewChange::Status(self.status.clone())];
            if self
                .revision_organizar
                .take_if(|r| r.epoch == epoch)
                .is_some()
            {
                changes.push(ViewChange::Organize { organize: None });
            }
            return vec![self.parche(changes)];
        }
        if plan.moves.is_empty() {
            return self.decir_de_organizar(epoch, "msg-organize-empty");
        }
        if plan.moves.len() > norte_frontend::MAX_AI_PLAN_ENTRIES {
            return self.decir_de_organizar(epoch, "msg-organize-invalid-plan");
        }
        let Some(plan_hash) = plan.plan_hash else {
            return self.decir_de_organizar(epoch, "msg-organize-invalid-plan");
        };
        let lines = norte_frontend::organize::tree_lines(&plan.moves, &existing);
        self.revision_organizar = Some(RevisionOrganizar {
            dir,
            moves: plan.moves,
            lines,
            plan_hash,
            first: 0,
            seen_until: norte_frontend::organize::ORGANIZE_LINE_LIMIT,
            acknowledged: false,
            epoch,
        });
        self.status.message = None;
        let changes = vec![
            ViewChange::Organize {
                organize: self.vista_organizar(),
            },
            ViewChange::Status(self.status.clone()),
        ];
        vec![self.parche(changes)]
    }

    /// The review's projection, or `None` if there is none.
    ///
    /// The names are proposed by a third party over names anyone could have
    /// typed: they go through canonical sanitizing, and each line says
    /// whether what is painted differs from the real thing.
    pub(super) fn vista_organizar(&self) -> Option<crate::dto::OrganizeView> {
        use norte_frontend::organize::{ORGANIZE_LINE_LIMIT, TreeKind};

        let r = self.revision_organizar.as_ref()?;
        let line = |text: &str| {
            let (displayable, hostile) = norte_frontend::display_name(text.as_bytes());
            crate::dto::DialogLine {
                text: clamp_display(displayable),
                hostile,
            }
        };
        let total = r.lines.len();
        let last = (r.first + ORGANIZE_LINE_LIMIT).min(total);
        let window = r.lines[r.first..last]
            .iter()
            .map(|l| crate::dto::OrganizeLineView {
                depth: u32::try_from(l.depth).unwrap_or(u32::MAX),
                text: line(&l.text),
                kind: match l.kind {
                    TreeKind::NewDir => crate::dto::OrganizeLineKind::NewDir,
                    TreeKind::ExistingDir => crate::dto::OrganizeLineKind::ExistingDir,
                    TreeKind::Moved => crate::dto::OrganizeLineKind::Moved,
                },
            })
            .collect();
        // What is HIDDEN does not sneak through clean: if a line outside the
        // window paints differently from its bytes, the indicator says so.
        // Without this the flag would only exist for what is visible, and it
        // takes just putting the altered line at position twelve.
        let hidden_hostile = r.lines.iter().enumerate().any(|(i, l)| {
            (i < r.first || i >= last) && norte_frontend::display_name(l.text.as_bytes()).1
        });
        let (folders, files) = norte_frontend::organize::resumen(&r.lines);
        Some(crate::dto::OrganizeView {
            dir: {
                let (displayable, hostile) = norte_frontend::path_display(&r.dir);
                crate::dto::DialogLine {
                    text: clamp_display(displayable),
                    hostile,
                }
            },
            lines: window,
            first_visible: r.first as u64,
            total: total as u64,
            // Translated HERE and not in the renderer, for the same reason as
            // the rename plan's note: the catalogue that crosses the bridge
            // carries the strings already formatted and with no arguments.
            more_note: if total > ORGANIZE_LINE_LIMIT {
                clamp_display(norte_i18n::ta_in(
                    self.lang,
                    "modal-ai-rename-more",
                    &[("shown", &last.to_string()), ("total", &total.to_string())],
                ))
            } else {
                String::new()
            },
            hidden_hostile,
            summary: clamp_display(norte_i18n::ta_in(
                self.lang,
                "modal-organize-summary",
                &[
                    ("dirs", &folders.to_string()),
                    ("files", &files.to_string()),
                ],
            )),
            seen_all: r.seen_until >= total,
        })
    }

    /// The review's keys: walking, discarding, and approving.
    ///
    /// Same discipline as the rename review, and for the same two reasons: a
    /// chord with a modifier was meant elsewhere, and the FIRST key only
    /// acknowledges the screen — this one opens on its own, tens of seconds
    /// after the gesture that requested it, and it keeps the keyboard.
    pub(super) fn tecla_en_revision_organizar(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if k.ctrl || k.alt || k.meta {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-key-unmapped".to_owned(),
                },
                Vec::new(),
            );
        }
        let acknowledged = self
            .revision_organizar
            .as_ref()
            .is_some_and(|r| r.acknowledged);
        if !acknowledged && k.key != "Escape" && k.key != "esc" {
            if let Some(r) = self.revision_organizar.as_mut() {
                r.acknowledged = true;
            }
            self.status.message = Some(clamp_display(norte_i18n::t_in(
                self.lang,
                "host-plan-acknowledge",
            )));
            let changes = vec![
                ViewChange::Organize {
                    organize: self.vista_organizar(),
                },
                ViewChange::Status(self.status.clone()),
            ];
            return (self.aplicada(), vec![self.parche(changes)]);
        }
        let total = self
            .revision_organizar
            .as_ref()
            .map_or(0, |r| r.lines.len());
        let window = norte_frontend::organize::ORGANIZE_LINE_LIMIT;
        let cap = total.saturating_sub(window);
        let page = i64::try_from(window).unwrap_or(1);
        let move_by = |r: &mut RevisionOrganizar, delta: i64| {
            let target = i64::try_from(r.first).unwrap_or(0).saturating_add(delta);
            r.first = usize::try_from(target.max(0)).unwrap_or(0).min(cap);
            // HIGH watermark: going back up does not un-read what was read.
            r.seen_until = r.seen_until.max((r.first + window).min(total));
        };
        match k.key.as_str() {
            "ArrowDown" | "j" => {
                if let Some(r) = self.revision_organizar.as_mut() {
                    move_by(r, 1);
                }
            }
            "ArrowUp" | "k" => {
                if let Some(r) = self.revision_organizar.as_mut() {
                    move_by(r, -1);
                }
            }
            "PageDown" => {
                if let Some(r) = self.revision_organizar.as_mut() {
                    move_by(r, page);
                }
            }
            "PageUp" => {
                if let Some(r) = self.revision_organizar.as_mut() {
                    move_by(r, -page);
                }
            }
            "Escape" | "n" | "N" => return self.cerrar_revision_organizar(),
            // `Enter` does NOT approve, for the same reason as the rename
            // review: this screen opens on its own, and `Enter` is the key
            // that was being used to walk the tree while the producer was
            // thinking.
            "y" | "Y" => return self.aprobar_revision_organizar(backend, mailbox),
            _ => {
                return (
                    ActionAck::Unavailable {
                        reason_key: "host-key-unmapped".to_owned(),
                    },
                    Vec::new(),
                );
            }
        }
        let change = ViewChange::Organize {
            organize: self.vista_organizar(),
        };
        (self.aplicada(), vec![self.parche(vec![change])])
    }

    /// Walks the tree with a mouse gesture (the wheel, or a button).
    ///
    /// It exists apart from the keys for the same reason as the decide
    /// button: approving requires having reached the end, and without a way
    /// to walk with the mouse that requirement turned the screen into one a
    /// keyboardless reader could never approve. It moves the SAME watermark
    /// as the keys — reading with the wheel is reading.
    pub(super) fn recorrer_organizar(
        &mut self,
        down: bool,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let total = self
            .revision_organizar
            .as_ref()
            .map_or(0, |r| r.lines.len());
        let window = norte_frontend::organize::ORGANIZE_LINE_LIMIT;
        let cap = total.saturating_sub(window);
        let Some(r) = self.revision_organizar.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        r.first = if down {
            (r.first + 1).min(cap)
        } else {
            r.first.saturating_sub(1)
        };
        r.seen_until = r.seen_until.max((r.first + window).min(total));
        let change = ViewChange::Organize {
            organize: self.vista_organizar(),
        };
        (self.aplicada(), vec![self.parche(vec![change])])
    }

    /// Answers the review with a gesture AIMED at it (a button), which does
    /// not need the acknowledgment a key does.
    pub(super) fn decidir_revision_organizar(
        &mut self,
        approve: bool,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.revision_organizar.is_none() {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        }
        if let Some(r) = self.revision_organizar.as_mut() {
            r.acknowledged = true;
        }
        if approve {
            self.aprobar_revision_organizar(backend, mailbox)
        } else {
            self.cerrar_revision_organizar()
        }
    }

    /// Discards the plan without applying anything.
    pub(super) fn cerrar_revision_organizar(
        &mut self,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // Neither the epoch is bumped nor `organizar_en_vuelo` touched, for
        // the same reason as in the rename review: whatever is there is a
        // LATER request, and releasing it here would kill it silently.
        self.revision_organizar = None;
        let change = ViewChange::Organize { organize: None };
        (self.aplicada(), vec![self.parche(vec![change])])
    }

    /// Approves the plan: ONE Task for the whole batch, a single undo.
    ///
    /// With the `plan_hash` that came WITH the plan, so what runs is exactly
    /// what was shown — if the directory changed underneath, the core answers
    /// `PlanStale` and moves nothing.
    pub(super) fn aprobar_revision_organizar(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(r) = self.revision_organizar.as_ref() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        // The second lock, here too: approving runs N moves and creates
        // folders.
        if self.efectos == crate::commands::Efectos::SoloLectura {
            return Self::no_muta();
        }
        if r.seen_until < r.lines.len() {
            // And it says WHICH thing is missing: "you haven't read it all"
            // is fixed one way and "cannot be done" another.
            return (
                ActionAck::Unavailable {
                    reason_key: "host-plan-unseen".to_owned(),
                },
                Vec::new(),
            );
        }
        let (dir, moves, hash) = (r.dir.clone(), r.moves.clone(), r.plan_hash.clone());
        let affected = vec![dir.clone()];
        let backend2 = Arc::clone(backend);
        let mailbox2 = mailbox.clone();
        tokio::spawn(async move {
            let message = match backend2.organize(dir, moves, hash).await {
                Ok(task) => Mensaje::TaskNueva(Box::new((task, affected, None))),
                Err(e) => Mensaje::TaskFallida(Box::new(e)),
            };
            let _ = mailbox2.send(message).await;
        });
        self.cerrar_revision_organizar()
    }

    /// Says it on the status bar and opens nothing. Closes THIS epoch's
    /// review if there was one: a plan that could not be requested does not
    /// leave half a screen open, and closing whatever was there would throw
    /// away a good plan because a later request failed.
    fn decir_de_organizar(&mut self, epoch: u64, key: &str) -> Vec<BridgeEnvelope<UiUpdate>> {
        self.status.message = Some(clamp_display(norte_i18n::t_in(self.lang, key)));
        let mut changes = vec![ViewChange::Status(self.status.clone())];
        if self
            .revision_organizar
            .take_if(|r| r.epoch == epoch)
            .is_some()
        {
            changes.push(ViewChange::Organize { organize: None });
        }
        vec![self.parche(changes)]
    }
}
