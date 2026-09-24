//! The journal's timeline, in the window (phase 7, #359).
//!
//! The MODEL — what a row is, that a batch is one, which `seq` the cut sends,
//! and how much it is going to take — is the shared one
//! (`norte_frontend::timeline`), the same one the TUI uses. What is here is
//! the wiring: requesting the pages, walking the rows, and asking before
//! undoing.
//!
//! The mold is the disk map's: one state per slot, one live request with its
//! token, and a response that arrives with a different token is discarded.

#[allow(clippy::wildcard_imports)]
use super::*;

/// The kind that occupies a timeline slot.
pub(super) const KIND: &str = "timeline";

/// How many rows are requested per page: the same as the TUI's. Well below
/// the protocol's cap, because this is a screen that is read and whatever
/// does not fit is requested on reaching the bottom.
const POR_PAGINA: u32 = 50;

/// What a timeline slot has, and what it is requesting.
#[derive(Default)]
pub(super) struct EstadoLinea {
    /// The rows and the cursor. The SHARED state.
    model: norte_frontend::timeline::Timeline,
    /// The request in flight: its token and where it was requested from.
    in_flight: Option<(RequestToken, Option<i64>)>,
    /// The first page has already been requested (whatever it answered).
    /// Without this, a journal that cannot be read would be retried after
    /// every actor message.
    requested: bool,
    /// Why there is no history to show, already translated. Only the FIRST
    /// page sets it: without it there is nothing to paint and the reason goes
    /// in its place.
    reason: Option<String>,
    /// The failure of a LATER page, already translated. It goes in the
    /// footer — the rows that did arrive are still there, so the "empty" slot
    /// is not painted — and it does not stop pagination forever: it is
    /// retried on scrolling down again.
    page_error: Option<String>,
    /// The cursor has moved since that failure: it can be requested again.
    retry: bool,
    /// The daemon stopped advancing — an empty page, or a cursor that does
    /// not go back. Treated as the end: an honest one never does this, and
    /// one that did would trigger a request after every actor message.
    exhausted: bool,
    /// After a reload, which row to return to: its `seq`. Without this, every
    /// Task that finished would send the cursor back to the top while someone
    /// is looking at it.
    return_to: Option<i64>,
}

impl Estado {
    /// Requests whatever the placed timelines are missing: the first page
    /// when their slot appears, and the next one when the cursor reaches the
    /// last loaded row.
    ///
    /// Called after EVERY actor message, so the first thing is to bail out
    /// cheaply: with no timeline slot at all there is nothing to walk.
    ///
    /// A new slot starts from scratch, and that is why closing and reopening
    /// the panel REREADS the history: anything could have happened in
    /// between — it is normal to do things with the panel closed — and one
    /// that shows what was there a while ago is worse than an empty one.
    pub(super) fn sondear_lineas(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) {
        let slots: Vec<u32> = self
            .reparto
            .placements
            .iter()
            .filter(|(slot, _)| kind_de(&self.arbol, *slot).is_some_and(|k| k.as_str() == KIND))
            .map(|(SlotId(id), _)| *id)
            .collect();
        if slots.is_empty() && self.lineas.is_empty() {
            return;
        }
        // A `SlotId` gets reused: without pruning, a new slot would inherit
        // another one's history.
        self.lineas.retain(|id, _| slots.contains(id));
        for id in slots {
            let state = self.lineas.entry(id).or_default();
            if state.in_flight.is_some() || state.reason.is_some() {
                continue;
            }
            let from = if state.requested {
                // Reaching the bottom requests the next page, and it is the
                // only moment more is requested: loading the whole journal on
                // opening would bring months of history to show ten rows.
                let bottom =
                    !state.model.is_empty() && state.model.cursor() + 1 >= state.model.len();
                // And no further than what the bridge lets cross: a loaded
                // row that is not sent is a cursor over something invisible.
                let fits = state.model.len() < crate::bridge::MAX_ROWS_PER_BATCH;
                let allowed = state.page_error.is_none() || state.retry;
                match state.model.next_before_seq() {
                    Some(s) if bottom && fits && allowed && !state.exhausted => Some(s),
                    _ => continue,
                }
            } else {
                None
            };
            self.token += 1;
            let token = RequestToken(self.token);
            if let Some(e) = self.lineas.get_mut(&id) {
                e.in_flight = Some((token, from));
            }
            let backend = Arc::clone(backend);
            let mailbox = mailbox.clone();
            tokio::spawn(async move {
                let res = match tokio::time::timeout(
                    PLAZO_PLUGINS,
                    backend.journal_list(from, POR_PAGINA),
                )
                .await
                {
                    Ok(r) => r,
                    Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
                };
                let _ = mailbox
                    .send(Mensaje::Fondo(Box::new(Fondo::PaginaDeLinea(
                        id, token, from, res,
                    ))))
                    .await;
            });
        }
    }

    /// Lands a page: it is used if the token is that of THAT slot's last
    /// request, and discarded otherwise.
    pub(super) fn aterrizar_pagina(
        &mut self,
        slot: u32,
        token: RequestToken,
        from: Option<i64>,
        res: Result<norte_proto::methods::JournalListResult, Error>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let lang = self.lang;
        let state = self.lineas.get_mut(&slot)?;
        if state.in_flight.map(|(t, _)| t) != Some(token) {
            return None;
        }
        state.in_flight = None;
        state.requested = true;
        match (res, from) {
            (Ok(page), None) => {
                state.model =
                    norte_frontend::timeline::Timeline::new(&page.rows, page.next_before_seq);
                // A reload returns to the row the cursor had, if it is still
                // there.
                if let Some(seq) = state.return_to.take()
                    && let Some(i) = state.model.rows().iter().position(|r| r.seq == seq)
                {
                    state.model.set_cursor(i);
                }
            }
            (Ok(page), Some(d)) => {
                if page.rows.is_empty() || page.next_before_seq.is_some_and(|n| n >= d) {
                    state.exhausted = true;
                }
                state.model.extend(&page.rows, page.next_before_seq);
                state.page_error = None;
            }
            // A daemon with no journal, or one that does not know the method:
            // here there is no history to show, and it is SAID.
            (Err(Error::Unsupported), None) => {
                state.reason = Some(norte_i18n::t_in(lang, "timeline-unavailable"));
            }
            (Err(e), None) => {
                state.reason = Some(clamp_display(norte_frontend::error::error_category_in(
                    lang, &e,
                )));
            }
            (Err(e), Some(_)) => {
                state.page_error = Some(clamp_display(norte_frontend::error::error_category_in(
                    lang, &e,
                )));
                state.retry = false;
            }
        }
        let snap = self.snapshot();
        Some(self.sobre(UiUpdate::Snapshot(Box::new(snap))))
    }

    /// Requests the first page again for the open timelines, keeping the
    /// cursor's row.
    ///
    /// Called when a Task finishes: with the panel OPEN — which is normal for
    /// a side slot — whatever was just done, or undone, has to show up. The
    /// ceiling (`upto_seq`) already keeps an undo from going past what was
    /// counted; this is for what was counted to be what is current.
    pub(super) fn recargar_lineas(&mut self) {
        for state in self.lineas.values_mut() {
            if !state.requested || state.in_flight.is_some() {
                continue;
            }
            state.return_to = state.model.selected().map(|r| r.seq);
            state.requested = false;
            state.exhausted = false;
            state.page_error = None;
            state.reason = None;
        }
    }

    /// The timeline slot with focus, if one has it.
    fn linea_enfocada(&self) -> Option<u32> {
        let SlotId(id) = self.roles.get(RoleId::Active)?;
        kind_de(&self.arbol, SlotId(id))
            .is_some_and(|k| k.as_str() == KIND)
            .then_some(id)
    }

    /// Whether the timeline has focus (and therefore `Enter` belongs to it).
    pub(super) fn linea_tiene_el_foco(&self) -> bool {
        self.linea_enfocada().is_some()
    }

    /// Movement, with the timeline focused. Only the THREE movement effects
    /// are its own; everything else — the tab key that exits it, above all —
    /// goes its own way.
    pub(super) fn efecto_en_linea(
        &mut self,
        efecto: Efecto,
    ) -> Option<(ActionAck, Vec<BridgeEnvelope<UiUpdate>>)> {
        if !matches!(
            efecto,
            Efecto::Cursor(_) | Efecto::Pagina(_) | Efecto::Extremo { .. }
        ) {
            return None;
        }
        let id = self.linea_enfocada()?;
        let state = self.lineas.get_mut(&id)?;
        // Up to the last row that CROSSES the bridge: beyond that, the cursor
        // would point at a row the renderer does not have.
        let rows = state.model.len().min(crate::bridge::MAX_ROWS_PER_BATCH);
        // Moving is what authorizes requesting a failed page again.
        state.retry = true;
        if rows == 0 {
            return Some((self.aplicada(), Vec::new()));
        }
        let total = i64::try_from(rows).unwrap_or(i64::MAX);
        let current = i64::try_from(state.model.cursor()).unwrap_or(0);
        let target = match efecto {
            Efecto::Cursor(n) => current.saturating_add(n.clamp(-total, total)),
            // A page is ten rows, like the TUI's list when it does not know
            // how tall it is: jumping further than there is means nothing.
            Efecto::Pagina(n) => current.saturating_add(n.clamp(-total, total).saturating_mul(10)),
            Efecto::Extremo { al_final: false } => 0,
            Efecto::Extremo { al_final: true } => total - 1,
            _ => return None,
        };
        state
            .model
            .set_cursor(usize::try_from(target.max(0)).unwrap_or(0));
        let snap = self.snapshot();
        Some((
            self.aplicada(),
            vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))],
        ))
    }

    /// `Enter` on the timeline: asks before undoing up to the cursor's row,
    /// with the COUNT.
    ///
    /// A cut that takes nothing back does NOT open a dialog: asking "are you
    /// sure?" about something that is not going to happen teaches saying yes
    /// without reading.
    pub(super) fn preguntar_deshacer_hasta(
        &mut self,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(id) = self.linea_enfocada() else {
            return (self.aplicada(), Vec::new());
        };
        let Some(state) = self.lineas.get(&id) else {
            return (self.aplicada(), Vec::new());
        };
        let (Some(seq), summary) = (state.model.corte(), state.model.resumen()) else {
            return (self.aplicada(), Vec::new());
        };
        // The ceiling freezes NOW, with the count that is about to be shown:
        // the undo does not go past what this question counted.
        let ceiling = state.model.techo();
        if summary.no_hace_nada() {
            return (self.aplicada(), self.decir("timeline-undo-nothing"));
        }
        if self.efectos == crate::commands::Efectos::SoloLectura {
            return Self::no_muta();
        }
        let line = |text: String| crate::dto::DialogLine {
            text: clamp_display(text),
            hostile: false,
        };
        // The three numbers on separate lines because they mean different
        // things and are not added together. What is skipped and what is
        // foreign only if there is any.
        let mut body = vec![
            line(norte_i18n::t_in(self.lang, "timeline-undo-body")),
            line(norte_i18n::ta_in(
                self.lang,
                "timeline-undo-count",
                &[("n", &summary.a_deshacer.to_string())],
            )),
        ];
        if summary.irreversibles > 0 {
            body.push(line(norte_i18n::ta_in(
                self.lang,
                "timeline-undo-skipped",
                &[("n", &summary.irreversibles.to_string())],
            )));
        }
        if summary.ajenas > 0 {
            body.push(line(norte_i18n::ta_in(
                self.lang,
                "timeline-undo-foreign",
                &[("n", &summary.ajenas.to_string())],
            )));
        }
        let modal = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let view = DialogView {
            id: modal,
            title_key: "timeline-undo-title".to_owned(),
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
                    label_key: "dialog-confirm".to_owned(),
                    // Undo WRITES: it moves files back and deletes what was
                    // created.
                    destructive: true,
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
        };
        self.dialogos.push(Dialogo {
            id: modal,
            vista: view,
            tecleado: Tecleado::Texto(String::new()),
            reconocido: true,
            al_confirmar: Some(Pendiente::DeshacerHasta {
                seq,
                techo: ceiling,
            }),
        });
        let change = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![change])])
    }

    /// The human read the count and said yes: it runs as an undo Task, with
    /// the usual progress and cancellation. What happened is told by its
    /// report, which this window already shows (`informe_de_undo`).
    pub(super) fn deshacer_hasta(
        &mut self,
        seq: i64,
        techo: Option<i64>,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // With no known scope, like a session's: what is on screen is
        // relisted.
        let visible = self.dirs_visibles();
        let backend = Arc::clone(backend);
        let mailbox = mailbox.clone();
        tokio::spawn(async move {
            match backend.undo_after(seq, techo).await {
                Ok(task) => {
                    let _ = mailbox
                        .send(Mensaje::TaskNueva(Box::new((task, visible, None))))
                        .await;
                }
                Err(e) => {
                    let _ = mailbox.send(Mensaje::TaskFallida(Box::new(e))).await;
                }
            }
        });
        self.decir("msg-timeline-undo-running")
    }

    /// A timeline's projection.
    pub(super) fn vista_de_linea(&self, id: u32) -> crate::dto::TimelineSlotView {
        let state = self.lineas.get(&id);
        let rows: Vec<crate::dto::TimelineRowView> = state
            .map(|e| {
                e.model
                    .rows()
                    .iter()
                    .take(crate::bridge::MAX_ROWS_PER_BATCH)
                    .map(|f| {
                        let mut tail = Vec::new();
                        if f.members > 1 {
                            tail.push(norte_i18n::ta_in(
                                self.lang,
                                "timeline-batch",
                                &[("n", &f.members.to_string())],
                            ));
                        }
                        if !f.reversible {
                            tail.push(norte_i18n::t_in(self.lang, "timeline-irreversible"));
                        }
                        crate::dto::TimelineRowView {
                            time: norte_frontend::format::hora_utc(f.ts_ms),
                            actor: clamp_display(f.actor_kind.clone()),
                            op: clamp_display(f.op.clone()),
                            path: clamp_display(norte_frontend::timeline::path_label(&f.path)),
                            hostile: f.hostile,
                            tail: tail.join(" · "),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let empty = match state {
            Some(e) if e.reason.is_some() => e.reason.clone().unwrap_or_default(),
            // "Nothing has been done yet" only when it HAS BEEN CHECKED.
            Some(e) if e.model.cargada() => norte_i18n::t_in(self.lang, "timeline-empty"),
            _ => norte_i18n::t_in(self.lang, "timeline-loading"),
        };
        let read_only = self.efectos == crate::commands::Efectos::SoloLectura;
        let footer = state
            .filter(|e| !e.model.is_empty())
            .map(|e| {
                let summary = e.model.resumen();
                // A page that did not arrive is said here: the rows that did
                // arrive occupy the slot, so the reason has nowhere else to
                // be seen.
                if let Some(error) = &e.page_error {
                    error.clone()
                } else if read_only {
                    // A footer that promises to undo in a window that is not
                    // going to do it teaches you not to trust the footer.
                    norte_i18n::t_in(self.lang, "host-read-only")
                } else if summary.no_hace_nada() {
                    norte_i18n::t_in(self.lang, "timeline-undo-nothing")
                } else {
                    norte_i18n::ta_in(
                        self.lang,
                        "timeline-undo-count",
                        &[("n", &summary.a_deshacer.to_string())],
                    )
                }
            })
            .unwrap_or_default();
        crate::dto::TimelineSlotView {
            slot_id: id,
            title: norte_i18n::t_in(self.lang, "timeline-title"),
            cursor: state
                .filter(|e| !e.model.is_empty())
                .map(|e| e.model.cursor() as u64),
            rows,
            empty,
            footer,
        }
    }
}
