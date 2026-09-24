//! Searching: the quick filter, normal search and semantic search.
//!
//! Part of `controller`: these are methods of `Estado`, moved here without
//! touching them (ADR 0086). The only writer is still the actor.

// These modules are the same `impl Estado` split into pieces, so they use
// the same imports as the parent. Listing them here would be a forty-line
// list per file, across 32 files, that goes out of sync the moment the
// parent imports something — `super::*` keeps it in sync on its own.
#[allow(clippy::wildcard_imports)]
use super::*;

/// How a search ended. The type and the precedence of its phrases belong to
/// the shared crate: here they used to be written separately and had already
/// diverged.
use norte_frontend::search_status::Outcome as Desenlace;

/// The search form's FIELDS, exactly as they cross the bridge (91).
///
/// The values are masked and clamped HERE, like everything that crosses:
/// what is typed ends up painted in a label, and a `U+202E` pasted from
/// somewhere else cannot reorder the line. What was really typed stays in
/// `Tecleado::Formulario`, unclamped — it is the same separation as a single
/// field dialog's `input`/`Tecleado::Texto`.
///
/// The toggles' labels are OWN keys (`search-toggle-*`) and not the
/// terminal's: those carry the key's name inside plus a `{ $on }` with the
/// state, which here is structural — the renderer paints a checkbox, not a
/// sentence.
pub(super) fn campos_de_busqueda(
    form: &norte_frontend::search::SearchForm,
) -> Vec<crate::dto::DialogFieldView> {
    use crate::dto::{DialogFieldKind, DialogFieldView};
    use norte_frontend::search::{self as busqueda, SearchField};

    let text_field = |f: SearchField| {
        let (displayable, hostile) = norte_frontend::display_name(form.texto(f).as_bytes());
        DialogFieldView {
            id: f.id().to_owned(),
            label_key: f.clave().to_owned(),
            value: clamp_display(displayable),
            hostile,
            kind: DialogFieldKind::Text,
        }
    };
    let toggle_field = |id: &str, key: &str, on: bool| DialogFieldView {
        id: id.to_owned(),
        label_key: key.to_owned(),
        value: String::new(),
        hostile: false,
        kind: DialogFieldKind::Toggle { on },
    };

    let mut fields: Vec<DialogFieldView> = SearchField::ORDEN.into_iter().map(text_field).collect();
    fields.push(toggle_field(
        busqueda::ID_REGEX,
        "search-toggle-regex",
        form.regex,
    ));
    fields.push(toggle_field(
        busqueda::ID_CASE,
        "search-toggle-case",
        form.case,
    ));
    fields.push(toggle_field(
        busqueda::ID_WHOLE_WORD,
        "search-toggle-whole-word",
        form.whole_word,
    ));
    fields.push(toggle_field(
        busqueda::ID_RECURSIVE,
        "search-toggle-recursive",
        form.recursive,
    ));
    fields.push(DialogFieldView {
        id: busqueda::ID_KINDS.to_owned(),
        label_key: "search-toggle-kinds".to_owned(),
        value: String::new(),
        hostile: false,
        kind: DialogFieldKind::Cycle {
            value_key: form.kinds.clave().to_owned(),
        },
    });
    fields
}

/// A live search and what it has found so far.
pub(super) struct Busqueda {
    /// Which of this window's searches it is.
    ///
    /// The identity CANNOT be the Task: the daemon brings the id and it
    /// arrives late, so until then there would be nothing to tell a batch
    /// apart from the previous search's. The epoch is known at LAUNCH time,
    /// which is when it is needed.
    pub(super) epoca: u64,
    /// The daemon's Task, once it is known. Zero while it is not.
    pub(super) task: norte_proto::TaskId,
    /// The view closed and whatever is left of this search is unneeded.
    ///
    /// It is shared with its forwarder, which is the one that can cancel
    /// before the id reaches the actor: `esc` right after launching is the
    /// window where nobody else has anyone to cancel.
    abandoned: Arc<std::sync::atomic::AtomicBool>,
    /// What was searched for, so it can be said.
    query: String,
    /// Where it was searched.
    root: VPath,
    /// What was found, in the order it arrived.
    hits: Vec<Hit>,
    /// This search is SEMANTIC: it asked by meaning against the index, not
    /// by name against the tree.
    semantic: bool,
    /// Where the cursor is.
    cursor: usize,
    /// How it ended, or that it is still running.
    ///
    /// A `bool` only said whether it was still alive, and then EVERY outcome
    /// painted "N hits" — i.e. a search that failed on the second directory
    /// and another that walked the whole tree read the same. That is not an
    /// interface imprecision: it is a false claim about the disk, and
    /// whoever reads it stops searching.
    ///
    /// The type belongs to the SHARED crate, and with it the phrases'
    /// precedence: the two frontends used to decide it separately and
    /// already disagreed on the pair "cancelled right at the cap" (ADR
    /// 0077).
    pub(super) desenlace: Desenlace,
    /// The cap that was requested: reaching it means there is more.
    cap: u32,
}

/// A search hit, wherever it came from.
#[derive(Clone)]
struct Hit {
    /// Where it is.
    path: VPath,
    /// What it is, if known. `None` on a SEMANTIC hit: the index returns
    /// paths and similarities, not kinds, and saying "file" because it
    /// usually is one is making up the answer.
    kind: Option<EntryKind>,
    /// How similar it is to what was asked, in `[-1, 1]`. `None` on a
    /// name search: there are no degrees there, it either matches or it does
    /// not.
    score: Option<f64>,
}

impl Estado {
    /// Cap on ONE search's results.
    ///
    /// Bounds the message and the host's memory: a big tree with a loose
    /// pattern returns everything there is. Reaching it is NOT a failure —
    /// the Task completes — and it is SAID, because "100 results" and "the
    /// first 100 of who knows how many" are two different answers.
    pub(super) const MAX_RESULTADOS: u32 = 2000;

    /// Opens the search prompt. What is typed is the pattern.
    /// Opens a GLOB prompt to mark — or unmark — by pattern.
    ///
    /// A prompt and not a key: the operand is a pattern that is typed, and
    /// that already has a shape in this host. What gets marked is decided by
    /// the SHARED model (`mark_glob`), which folds the name before matching
    /// and knows that a `*` over masked names cannot mean "everything that
    /// paints oddly".
    pub(super) fn pedir_patron(
        &mut self,
        marcar: bool,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        self.dialogos.push(Dialogo {
            id,
            reconocido: true,
            vista: DialogView {
                id,
                title_key: if marcar {
                    "modal-mark-pattern-title"
                } else {
                    "modal-unmark-pattern-title"
                }
                .to_owned(),
                destination: None,
                subject: None,
                asker: None,
                deadline: None,
                deadline_at_ms: None,
                body: Vec::new(),
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
            },
            tecleado: Tecleado::Texto(String::new()),
            al_confirmar: Some(Pendiente::Patron { marcar }),
        });
        let change = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![change])])
    }

    /// Applies the typed pattern.
    pub(super) fn aplicar_patron(
        &mut self,
        marcar: bool,
        pattern: &str,
    ) -> (Option<&'static str>, Vec<BridgeEnvelope<UiUpdate>>) {
        if pattern.is_empty() {
            // An empty glob matches nothing, and saying so is better than
            // doing nothing: whoever pressed the key believes they marked
            // something.
            return (Some("err-empty-pattern"), self.decir("err-empty-pattern"));
        }
        match self.hueco_mut().pane.mark_glob(pattern, marcar) {
            Ok(n) => {
                // The TUI's key, which already existed and says "N marks
                // changed": it serves both directions, and a second
                // definition of the same key is silently dropped by Fluent —
                // the trap this repo has already run into twice.
                let mut outgoing =
                    self.decir_con("msg-marked-by-pattern", &[("n", &n.to_string())]);
                outgoing.push(self.parche_filas());
                (None, outgoing)
            }
            // A glob that does not compile is SAID: it is what the reader
            // just typed, and staying quiet leaves a key that did nothing.
            Err(_) => (Some("err-bad-pattern"), self.decir("err-bad-pattern")),
        }
    }

    pub(super) fn pedir_busqueda(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let form = norte_frontend::search::SearchForm::new();
        let root = self.hueco().pane.dir().clone();
        let where_line = Self::linea_de_ruta(&root);
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        self.dialogos.push(Dialogo {
            id,
            reconocido: true,
            vista: DialogView {
                id,
                title_key: "modal-search-title".to_owned(),
                destination: None,
                subject: None,
                asker: None,
                deadline: None,
                deadline_at_ms: None,
                body: vec![where_line],
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
                // A form has no "the" field: it has all of them in `fields`.
                // `input` stays `None` so a renderer does not also paint a
                // loose, unlabeled box.
                input: None,
                input_hostile: false,
                input_secret: false,
                fields: campos_de_busqueda(&form),
                dest_check: crate::dto::DestCheckView::NotAsked,
            },
            tecleado: Tecleado::Formulario(Box::new(form)),
            al_confirmar: Some(Pendiente::Buscar { root }),
        });
        let change = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![change])])
    }

    /// Launches the search and hooks up the channel its batches arrive
    /// through.
    ///
    /// The parameters arrive ALREADY built, from the shared mapping
    /// (`norte_frontend::search::params`): the terminal asks the same
    /// search, and two mappings diverge silently. `etiqueta` is what the
    /// results view shows as the query — the name pattern, or the content
    /// one if that one is empty — and is not used to search.
    pub(super) fn lanzar_busqueda(
        &mut self,
        params: norte_proto::methods::FsSearchParams,
        etiqueta: String,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // The root is copied BEFORE: `params` moves into the backend, and the
        // results view needs it to say where the search happened.
        let root = params.root.clone();
        let backend = Arc::clone(backend);
        let mailbox2 = mailbox.clone();
        self.epoca_busqueda += 1;
        let epoch = self.epoca_busqueda;
        let abandoned = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let abandoned2 = Arc::clone(&abandoned);
        // The search is launched and ANSWERS through the mailbox, like
        // everything else: the actor keeps handling keys while the daemon
        // walks the tree.
        tokio::spawn(async move {
            let (task, mut rx) = match backend.search(params).await {
                Ok(pair) => pair,
                Err(e) => {
                    // Through BOTH paths: the bar says it once and the
                    // search view stops claiming it is still searching.
                    // Without the second, it stayed at "searching…" forever
                    // over something that never got to exist.
                    let _ = mailbox2
                        .send(Mensaje::Fondo(Box::new(Fondo::BusquedaRota(
                            epoch,
                            Box::new(e.clone()),
                        ))))
                        .await;
                    let _ = mailbox2.send(Mensaje::TaskFallida(Box::new(e))).await;
                    return;
                }
            };
            let id = task.id;
            let cancel = Arc::clone(&task.cancel);
            let _ = mailbox2
                .send(Mensaje::TaskNueva(Box::new((task, Vec::new(), None))))
                .await;
            // Named HERE and not with the first batch: there may be no first
            // batch — the core does not send empty batches — and then the
            // search was left with no name, unable to finish and unable to
            // be cancelled.
            let _ = mailbox2
                .send(Mensaje::Fondo(Box::new(Fondo::BusquedaViva(epoch, id))))
                .await;
            // The view may have closed while the daemon was accepting the
            // Task: in that window the actor has nobody to cancel, so
            // whoever does have someone cancels.
            if abandoned2.load(std::sync::atomic::Ordering::SeqCst) {
                cancel();
                return;
            }
            // The pump lives as long as the channel: when the daemon closes
            // it, the search finished and the progress already said so on
            // its own.
            while let Some(batch) = rx.recv().await {
                if abandoned2.load(std::sync::atomic::Ordering::SeqCst) {
                    cancel();
                    return;
                }
                if mailbox2
                    .send(Mensaje::Fondo(Box::new(Fondo::Resultados(
                        epoch,
                        Box::new(batch),
                    ))))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        });
        // The view opens RIGHT AWAY, empty and saying it is running: waiting
        // for the first batch is a window that does not react to a key that
        // did do something.
        self.busqueda = Some(Busqueda {
            semantic: false,
            epoca: epoch,
            // Not known yet: `Fondo::BusquedaViva` brings it. Zero is never
            // a real Task.
            task: norte_proto::TaskId::new(0),
            abandoned,
            query: etiqueta,
            root,
            hits: Vec::new(),
            cursor: 0,
            desenlace: Desenlace::Running,
            cap: Self::MAX_RESULTADOS,
        });
        let change = ViewChange::Search {
            search: self.vista_busqueda(),
        };
        vec![self.parche(vec![change])]
    }

    /// A batch of results.
    ///
    /// Matched by EPOCH, known at launch. The Task's id was not enough: until
    /// it arrived, `b.task` was zero and the first batch to show up named the
    /// search — including a late one from the PREVIOUS search, whose
    /// forwarder is still alive — so one pattern's hits filled the list
    /// labelled with another's.
    pub(super) fn aplicar_resultados(
        &mut self,
        epoch: u64,
        lote: &norte_proto::methods::SearchHits,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let b = self.busqueda.as_mut()?;
        if b.epoca != epoch {
            return None;
        }
        let room = usize::try_from(b.cap).unwrap_or(usize::MAX);
        for e in &lote.entries {
            if b.hits.len() >= room {
                break;
            }
            b.hits.push(Hit {
                path: e.path.clone(),
                kind: Some(e.kind),
                score: None,
            });
        }
        let change = ViewChange::Search {
            search: self.vista_busqueda(),
        };
        Some(self.parche(vec![change]))
    }

    /// The search's projection.
    pub(super) fn vista_busqueda(&self) -> Option<crate::dto::SearchView> {
        let b = self.busqueda.as_ref()?;
        let (where_text, root_hostile) = norte_frontend::path_display(&b.root);
        Some(crate::dto::SearchView {
            semantic: b.semantic,
            query: clamp_display(norte_frontend::display_name(b.query.as_bytes()).0),
            root: clamp_display(where_text),
            root_hostile,
            rows: b
                .hits
                .iter()
                .map(|e| {
                    let name = e
                        .path
                        .file_name()
                        .map_or_else(Vec::new, |s| s.as_bytes().to_vec());
                    let (displayable, hostile) = norte_frontend::display_name(&name);
                    let (parent, parent_hostile) = e.path.parent().map_or_else(
                        || (String::new(), false),
                        |p| norte_frontend::path_display(&p),
                    );
                    crate::dto::SearchRowView {
                        name: clamp_display(displayable),
                        hostile,
                        parent: clamp_display(parent),
                        parent_hostile,
                        is_dir: e.kind == Some(EntryKind::Dir),
                        score: e.score,
                    }
                })
                .collect(),
            // `then` and not `then_some`: `then_some`'s argument is ALWAYS
            // evaluated, and with zero hits `len() - 1` overflowed.
            cursor: (!b.hits.is_empty()).then(|| b.cursor.min(b.hits.len() - 1) as u64),
            status: clamp_display(Self::estado_de_busqueda(b, self.lang)),
            running: b.desenlace == Desenlace::Running,
        })
    }

    /// A search that never got queued: it stops claiming it is searching.
    pub(super) fn busqueda_rota(&mut self, epoch: u64, e: &Error) -> Vec<BridgeEnvelope<UiUpdate>> {
        let category = clamp_display(norte_frontend::error::error_category_in(self.lang, e));
        let Some(b) = self.busqueda.as_mut().filter(|b| b.epoca == epoch) else {
            return Vec::new();
        };
        b.desenlace = Desenlace::Failed(category);
        let change = ViewChange::Search {
            search: self.vista_busqueda(),
        };
        vec![self.parche(vec![change])]
    }

    /// A search's status phrase.
    ///
    /// Reuses the TUI's family (`search-status-*`) instead of inventing
    /// another: it is the same information and there are not two ways to say
    /// it.
    ///
    /// The PRECEDENCE is decided by the shared crate, which is where the
    /// disagreement was: stopping the search right at the cap said
    /// "cancelled" in the terminal and "there is more" here.
    ///
    /// The failure carries no count: what needs to be read there is not how
    /// many were found, but that the answer is incomplete and why.
    pub(super) fn estado_de_busqueda(b: &Busqueda, lang: norte_i18n::Lang) -> String {
        let at_cap = b.hits.len() >= usize::try_from(b.cap).unwrap_or(usize::MAX);
        let key = norte_frontend::search_status::status_key(&b.desenlace, at_cap);
        if let Desenlace::Failed(category) = &b.desenlace {
            return norte_i18n::ta_in(lang, key, &[("error", category)]);
        }
        norte_i18n::ta_in(lang, key, &[("n", &b.hits.len().to_string())])
    }

    /// The keys while the search is open.
    pub(super) fn tecla_en_busqueda(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        /// How many rows a page moves.
        const PAGINA: usize = 10;
        let Some(b) = self.busqueda.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        let last = b.hits.len().saturating_sub(1);
        match k.key.as_str() {
            "Escape" | "esc" => {
                // Closing the search CANCELS the Task: continuing to walk a
                // tree for nobody is spending the daemon on a result that no
                // longer has anywhere to appear.
                b.abandoned.store(true, std::sync::atomic::Ordering::SeqCst);
                let task = b.task;
                self.busqueda = None;
                if task.get() != 0 {
                    self.cancelar(task.get());
                }
                // And if it was a SEMANTIC query, it is aborted: it has no
                // Task to cancel — it is a direct call — and what stops it is
                // dropping it, which makes the SDK send `rpc.cancel`.
                if let Some(flight) = self.semantica_en_vuelo.take() {
                    flight.abort();
                }
            }
            "ArrowDown" | "down" => b.cursor = (b.cursor + 1).min(last),
            "ArrowUp" | "up" => b.cursor = b.cursor.saturating_sub(1),
            "PageDown" | "pgdn" => b.cursor = (b.cursor + PAGINA).min(last),
            "PageUp" | "pgup" => b.cursor = b.cursor.saturating_sub(PAGINA),
            "Home" | "home" => b.cursor = 0,
            "End" | "end" => b.cursor = last,
            "Enter" | "enter" => {
                let row = u32::try_from(b.cursor).unwrap_or(u32::MAX);
                return self.ir_al_resultado(row, backend, mailbox);
            }
            _ => return (self.aplicada(), Vec::new()),
        }
        let change = ViewChange::Search {
            search: self.vista_busqueda(),
        };
        (self.aplicada(), vec![self.parche(vec![change])])
    }

    /// Goes to result `fila`: the panel navigates to its directory and the
    /// cursor ends up ON it.
    ///
    /// Without rebuilding any path: the hit's is the one the daemon sent, and
    /// it is handed whole to the panel so it matches it byte for byte once
    /// the listing lands. A painted name never becomes a path again — that is
    /// exactly how you end up opening a different file.
    pub(super) fn ir_al_resultado(
        &mut self,
        fila: u32,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(b) = self.busqueda.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        // Out of range is not clamped: clamping used to navigate to the LAST
        // hit instead of doing nothing. Hits are only ever appended, so a
        // valid index always names the same one and this list needs no
        // generation; one that overshoots means the list was emptied.
        let Some(hit) = b.hits.get(fila as usize).cloned() else {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        };
        b.cursor = fila as usize;
        // A directory opens from the inside; a file, in its folder with the
        // cursor on it.
        // With no kind — a semantic hit — it is treated as a file: its
        // folder opens with the cursor on it. That is the conservative
        // choice; entering INTO something that turns out not to be a
        // directory leads nowhere.
        let (target, focus) = if hit.kind == Some(EntryKind::Dir) {
            (hit.path.clone(), None)
        } else {
            match hit.path.parent() {
                Some(p) => (p, Some(hit.path.clone())),
                None => (hit.path.clone(), None),
            }
        };
        let task = b.task;
        self.busqueda = None;
        if task.get() != 0 {
            self.cancelar(task.get());
        }
        if let Some(child) = focus {
            self.hueco_mut().pane.set_pending_focus(child);
        }
        let closing = self.parche(vec![ViewChange::Search { search: None }]);
        let mut outgoing = vec![closing];
        outgoing.extend(self.navegar(&target, Trail::Record, backend, mailbox));
        (self.aplicada(), outgoing)
    }

    /// The key, when the incremental search box is open.
    ///
    /// `None` = this key is not its own and goes its normal way (a function
    /// key, a shortcut with a modifier): opening the quick search does NOT
    /// disconnect the rest of the keyboard, it only keeps text, backspace,
    /// and the three keys that govern it.
    pub(super) fn tecla_en_quick(
        &mut self,
        k: &crate::keys::KeyInput,
    ) -> Option<(ActionAck, Vec<BridgeEnvelope<UiUpdate>>)> {
        if k.ctrl || k.alt || k.meta {
            return None;
        }
        let pane = &mut self.hueco_mut().pane;
        match k.key.as_str() {
            "Escape" | "esc" => pane.quick_cancel(),
            "Enter" | "enter" => {
                pane.quick_confirm();
            }
            "Backspace" | "backspace" => pane.quick_backspace(),
            "ArrowDown" | "down" => pane.quick_down(),
            "ArrowUp" | "up" => pane.quick_up(),
            other => {
                let mut chars = other.chars();
                let (Some(c), None) = (chars.next(), chars.next()) else {
                    return None;
                };
                pane.quick_char(c);
            }
        }
        Some((self.aplicada(), vec![self.parche_filas()]))
    }

    /// Opens a SEMANTIC query's prompt.
    // TODO(translation): review — this one-line doc is duplicated verbatim
    /// right below (both lines said the exact same thing in the source); it
    /// looks like a stale leftover from an earlier edit, kept as-is.
    /// Opens a SEMANTIC query's prompt.
    ///
    /// It carries no root, and that is what the dialog says: the index is
    /// built by roots and not by what is currently being looked at, so
    /// scoping the search to the panel's directory would promise a scope the
    /// index might not have. The WHOLE index is asked, same as the TUI.
    pub(super) fn pedir_consulta_semantica(
        &mut self,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let view = DialogView {
            id,
            title_key: "modal-semantic-title".to_owned(),
            destination: None,
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body: vec![crate::dto::DialogLine {
                text: clamp_display(norte_i18n::t_in(self.lang, "modal-semantic-scope")),
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
            input: Some(String::new()),
            input_hostile: false,
            input_secret: false,
            fields: Vec::new(),
            dest_check: crate::dto::DestCheckView::NotAsked,
        };
        self.dialogos.push(Dialogo {
            id,
            vista: view.clone(),
            tecleado: Tecleado::Texto(String::new()),
            reconocido: true,
            al_confirmar: Some(Pendiente::ConsultaSemantica),
        });
        let change = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![change])])
    }

    /// Launches the query against the index. The answer comes back to the
    /// actor.
    ///
    /// A new epoch per query: the answer takes a while — there is an embed in
    /// the middle — and whoever asks twice cannot end up looking at the
    /// first one's results.
    pub(super) fn lanzar_semantica(
        &mut self,
        query: String,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        if query.trim().is_empty() {
            // An empty query does not leave the process: it means nothing,
            // and what leaves goes to an external provider.
            //
            // The message is the SAME as the terminal's (#122). It used to be
            // `err-empty-pattern`, which is the name-search one and says "an
            // empty one matches the whole tree" — false here: an empty
            // semantic query matches nothing, there is nothing to compare
            // against. Two frontends refusing the same thing for two
            // different reasons, and one of them made up.
            self.status.message = Some(clamp_display(norte_i18n::t_in(
                self.lang,
                "modal-semantic-empty-query",
            )));
            // And the field COMES BACK, which is the other half of parity:
            // the terminal leaves the modal open with the error underneath,
            // so a message asking to type a query over a screen with nowhere
            // to type it was not a refusal, it was a dead end. The field was
            // empty, so nothing is lost by rebuilding it.
            let (_, mut outgoing) = self.pedir_consulta_semantica();
            let change = ViewChange::Status(self.status.clone());
            outgoing.push(self.parche(vec![change]));
            return outgoing;
        }
        self.epoca_busqueda += 1;
        let epoch = self.epoca_busqueda;
        self.busqueda = Some(Busqueda {
            semantic: true,
            epoca: epoch,
            task: norte_proto::TaskId::new(0),
            abandoned: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            query: query.clone(),
            // The scope is the whole index: there is no root to show, and
            // the view says so via `semantic`.
            root: self.hueco().pane.dir().clone(),
            hits: Vec::new(),
            cursor: 0,
            desenlace: Desenlace::Running,
            cap: norte_proto::methods::INDEX_SEMANTIC_MAX_K,
        });
        let backend = Arc::clone(backend);
        let mailbox = mailbox.clone();
        let handle = tokio::spawn(async move {
            let hits = backend
                .semantic_search(query, norte_frontend::SEMANTIC_K)
                .await;
            let _ = mailbox
                .send(Mensaje::Fondo(Box::new(Fondo::Semanticos(epoch, hits))))
                .await;
        });
        // Relaunching ABORTS the previous one, and aborting really does
        // cancel it: the SDK sends `rpc.cancel` on dropping the call. Letting
        // it run would be paying for an embed and an index sweep for an
        // answer the epoch already condemns to being discarded.
        if let Some(old_handle) = self.semantica_en_vuelo.replace(handle) {
            old_handle.abort();
        }
        let change = ViewChange::Search {
            search: self.vista_busqueda(),
        };
        vec![self.parche(vec![change])]
    }

    /// The index's answer: it goes in if it is still the current query.
    pub(super) fn aplicar_semanticos(
        &mut self,
        epoch: u64,
        hits: Result<Vec<norte_proto::methods::SemanticHit>, Error>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // The view may have closed or been superseded while the embed was
        // running.
        if self.busqueda.as_ref().is_none_or(|b| b.epoca != epoch) {
            return Vec::new();
        }
        let hits = match hits {
            // The wire sweep is the SHARED one: it clamps `k` and refuses a
            // non-finite score, which serialized as `null` would poison the
            // order.
            Ok(h) => {
                let Some(h) = norte_frontend::validate_semantic_hits(h) else {
                    self.busqueda = None;
                    let mut outgoing = vec![self.parche(vec![ViewChange::Search { search: None }])];
                    outgoing.extend(self.decir("msg-semantic-bad-hits"));
                    return outgoing;
                };
                h
            }
            Err(e) => {
                self.busqueda = None;
                let key = match e {
                    // `NotFound` here is NOT "there are no results": it is
                    // that this root has no rows in the index. Reading it as
                    // an empty search leaves the reader believing there is
                    // nothing like what they asked for.
                    Error::NotFound => "msg-semantic-no-index",
                    Error::Unsupported => "msg-semantic-unsupported",
                    _ => norte_frontend::error::error_key(&e),
                };
                // The view STAYS, saying why it broke, instead of closing and
                // leaving the reason on the bar: the next key takes it away
                // there, and the reader is left with no index and no idea.
                // Same treatment as a normal search that fails
                // (`Desenlace::Failed` is persistent); semantic's own keys —
                // "no index", "not supported" — are the ones that really
                // explain this, so they win over the error's generic
                // category.
                let reason = clamp_display(norte_i18n::t_in(self.lang, key));
                if let Some(b) = self.busqueda.as_mut() {
                    b.desenlace = Desenlace::Failed(reason);
                }
                let mut outgoing = vec![self.parche(vec![ViewChange::Search {
                    search: self.vista_busqueda(),
                }])];
                outgoing.extend(self.decir(key));
                return outgoing;
            }
        };
        self.semantica_en_vuelo = None;
        if let Some(b) = self.busqueda.as_mut() {
            b.hits = hits
                .into_iter()
                .map(|h| Hit {
                    path: h.path,
                    // The index returns paths and similarities, not kinds.
                    kind: None,
                    score: Some(h.score),
                })
                .collect();
            b.desenlace = Desenlace::Done;
        }
        let change = ViewChange::Search {
            search: self.vista_busqueda(),
        };
        vec![self.parche(vec![change])]
    }

    /// Opens the create-directory prompt, with its text field empty.
    // TODO(translation): review — this paragraph describes a create-directory
    /// prompt, but the item right after it is `buscar_rapido`'s own doc,
    /// about starting the quick search; it looks like a stale fragment left
    /// by an earlier edit.
    /// Starts the listing's incremental search.
    ///
    /// Filtering is the DEFAULT mode — the one that does not move the
    /// listing under the cursor while typing —, but `[ui] quick_search`
    /// chooses it, same as in the terminal.
    pub(super) fn buscar_rapido(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // The mode is set by `[ui] quick_search`, like in the terminal. It
        // used to be hardcoded to `Filter`, so `quick_search = "jump"` moved
        // the cursor in `ntc` and clamped the listing in the window: the same
        // key with two behaviors.
        let mode = self.config.quick_search_mode;
        self.hueco_mut().pane.quick_start(mode);
        (self.aplicada(), vec![self.parche_filas()])
    }
}
