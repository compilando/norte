//! "Go to anywhere" in the window (#357, phase 6 of the WOW programme).
//!
//! The model — sections, order, filtering, cursor —, how each row class is
//! built and what confirming it means belong to `norte_frontend::goto`, the
//! same ones the TUI uses. What lives here is what only this window knows:
//! which of its own lists the rows come from, how connections reach it (the
//! daemon provides them), and how the index is queried without freezing the
//! screen.
//!
//! Part of `controller`: these are methods of `State`. The only writer is
//! still the actor.

#[allow(clippy::wildcard_imports)]
use super::*;

use norte_frontend::goto::{
    Action, BROUGHT_BY_LIST, FixedSource, Goto, GotoLine, GotoRow, GotoSource, INDEX_CAP, Mode,
    PathSource, SECTION_COMMANDS, SECTION_CONNECTIONS, SECTION_FAVORITES, SECTION_HELP,
    SECTION_HISTORY, SECTION_INDEX, SECTION_POPULAR,
};

impl State {
    /// Opens the search box on `query` (`>` for commands, `""` for places)
    /// and sends it.
    pub(super) fn open_go_to(
        &mut self,
        query: &str,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.start_go_to(query, backend, mailbox);
        let change = ViewChange::Goto {
            goto: self.vista_ir_a(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// Opens the search box on `query`: the state and its late questions,
    /// no patch (the caller sends it, alone or alongside another change).
    ///
    /// The rows are taken as a SNAPSHOT on open: a list that changes under
    /// the cursor while it is being read is how an Enter ends up somewhere
    /// else. The exceptions arrive late, through the mailbox: CONNECTIONS
    /// (only the daemon knows them, not this process), the PLUGIN commands
    /// (the daemon's catalogue) and whatever the index finds (one question
    /// per query).
    pub(super) fn start_go_to(
        &mut self,
        query: &str,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) {
        // Opening it while open switches the mode in place (spec).
        if let Some(g) = self.ir_a.as_mut() {
            g.set_query(query);
            self.request_goto_from_index(backend, mailbox);
            return;
        }
        let sources = self.go_to_sources();
        // The generation bump: a late answer meant for an earlier box has
        // nowhere to land.
        self.close_go_to();
        let mut g = Goto::new(sources).with_recent(&self.palette_recent);
        g.set_query(query);
        self.ir_a = Some(g);
        self.request_plugin_rows(backend, mailbox);
        let generation = self.gen_ir_a;
        let backend = Arc::clone(backend);
        let mailbox = mailbox.clone();
        tokio::spawn(async move {
            let res = match tokio::time::timeout(DEADLINE_PLUGINS, backend.connections()).await {
                Ok(r) => r,
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            // "Go to anywhere" is a list of DESTINATIONS, and an entry that
            // does not parse is not one: only the good ones stay here. Where
            // it says what is wrong with the other one is the connection
            // selector (#365), which is where it will get fixed.
            let res = res.map(|r| r.connections);
            let _ = mailbox
                .send(Message::Background(Box::new(Background::GoToConnections(
                    generation, res,
                ))))
                .await;
        });
    }

    /// The SYNCHRONOUS sources: what this window already has in memory.
    /// Connections start empty and fill in once the daemon answers.
    fn go_to_sources(&self) -> Vec<Box<dyn GotoSource + Send>> {
        let slot = self.slot();
        let encoding = slot.pane.name_encoding();
        let current = slot.pane.dir().clone();
        let mut out: Vec<Box<dyn GotoSource + Send>> = Vec::new();
        out.push(Box::new(PathSource::new(norte_i18n::t_in(
            self.lang,
            "goto-path-desc",
        ))));
        // History is the ONLY section that carries the panel's
        // reinterpretation, because it is the only one whose paths belong to
        // that panel.
        let history: Vec<GotoRow> =
            norte_frontend::history::history_rows(&slot.history, &current, "", encoding)
                .into_iter()
                .filter(|r| r.mark != norte_frontend::history::HistoryMark::Current)
                .take(BROUGHT_BY_LIST)
                .map(|r| {
                    norte_frontend::goto::row_path(SECTION_HISTORY.id, None, &r.path, encoding)
                })
                .collect();
        out.push(Box::new(FixedSource::new(SECTION_HISTORY, history)));
        let popular: Vec<GotoRow> =
            norte_frontend::history::popular_rows(&self.popular, &current, "")
                .into_iter()
                .take(BROUGHT_BY_LIST)
                .map(|r| norte_frontend::goto::row_path(SECTION_POPULAR.id, None, &r.path, None))
                .collect();
        out.push(Box::new(FixedSource::new(SECTION_POPULAR, popular)));
        // A favorite whose destination does not parse is NOT offered: the
        // places list already shows it with its error.
        let favorites: Vec<GotoRow> = self
            .config
            .common
            .hotlist
            .iter()
            .filter_map(|h| {
                h.target.as_ref().ok().map(|p| {
                    norte_frontend::goto::row_path(SECTION_FAVORITES.id, Some(&h.name), p, None)
                })
            })
            .collect();
        out.push(Box::new(FixedSource::new(SECTION_FAVORITES, favorites)));
        out.push(Box::new(FixedSource::new(SECTION_CONNECTIONS, Vec::new())));
        // The commands: `palette_rows` already resolves which ones this host
        // implements and with what effects.
        let commands =
            norte_frontend::goto::command_rows(self.palette_rows(), Some(&self.facts()), self.lang);
        out.push(Box::new(FixedSource::new(SECTION_COMMANDS, commands)));
        out.push(Box::new(FixedSource::new(
            SECTION_HELP,
            norte_frontend::goto::help_rows(self.lang),
        )));
        out
    }

    /// The connections arrived: they go into their section, WITHOUT moving
    /// the cursor (`replace_section` guarantees it). A failure leaves the
    /// section empty and is not announced: it is one section fewer, not a
    /// screen that fails to open.
    pub(super) fn goto_connections(
        &mut self,
        generation: u64,
        res: Result<Vec<norte_proto::methods::ConnectionEntry>, Error>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        if generation != self.gen_ir_a {
            return None;
        }
        let goto = self.ir_a.as_mut()?;
        let rows: Vec<GotoRow> = res
            .ok()?
            .iter()
            .take(crate::bridge::MAX_ROWS_PER_BATCH)
            .map(|c| norte_frontend::goto::row_connection(&c.name, &c.url))
            .collect();
        // The section opened empty: no connections change nothing, and a
        // patch that repaints the same box only races the reader's keys.
        if rows.is_empty() {
            return None;
        }
        goto.replace_section(SECTION_CONNECTIONS, rows, false);
        let change = ViewChange::Goto {
            goto: self.vista_ir_a(),
        };
        Some(self.parche(vec![change]))
    }

    /// Asks the index about what is typed, if it is worth it.
    ///
    /// Relaunching ABORTS the previous question. Below
    /// [`norte_frontend::goto::MINIMUM_FOR_THE_INDEX`] it does not ask and
    /// EMPTIES the section: leaving there what answered a longer query is
    /// showing an answer to a question that is no longer being asked.
    fn request_goto_from_index(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) {
        if let Some(old) = self.go_to_index.take() {
            old.abort();
        }
        let Some(goto) = self.ir_a.as_mut() else {
            return;
        };
        // Three cases where it does not ask, and all three EMPTY the section:
        // - a short query cannot be good, and a `>`/`?` query is not a place;
        // - a typed PATH is not a semantic query, and sending it to an
        //   embeddings provider — maybe remote — is sending it the name of a
        //   directory of the reader's;
        // - a read-only window does not let queries leave the process, same
        //   as its explicit semantic search (`QuerySemantic`).
        let read_only = self.effects == crate::commands::Effects::SoloRead;
        let query = goto.index_query().map(str::to_owned);
        let Some(query) = query.filter(|_| !read_only) else {
            goto.replace_section(SECTION_INDEX, Vec::new(), true);
            return;
        };
        let generation = self.gen_ir_a;
        let backend = Arc::clone(backend);
        let mailbox = mailbox.clone();
        self.go_to_index = Some(tokio::spawn(async move {
            let res = match tokio::time::timeout(
                DEADLINE_PLUGINS,
                backend.semantic_search(query.clone(), INDEX_CAP),
            )
            .await
            {
                Ok(r) => r,
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            let _ = mailbox
                .send(Message::Background(Box::new(Background::GoToIndex(
                    generation, query, res,
                ))))
                .await;
        }));
    }

    /// Puts the index's answer into its section.
    ///
    /// It opens nothing and does not write to the bar (an index that is off
    /// answers `Unsupported`, and that is normal for whoever does not have
    /// one); it does not trust the size of the answer (the SAME
    /// `validate_semantic_hits` as semantic search); and it touches nothing
    /// if the screen already closed or what was typed changed while the
    /// index was thinking.
    pub(super) fn goto_index(
        &mut self,
        generation: u64,
        query: &str,
        res: Result<Vec<norte_proto::methods::SemanticHit>, Error>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        if generation != self.gen_ir_a {
            return None;
        }
        let goto = self.ir_a.as_mut()?;
        if goto.index_query() != Some(query) {
            return None;
        }
        let hits = norte_frontend::validate_semantic_hits(res.ok()?)?;
        goto.replace_section(SECTION_INDEX, norte_frontend::goto::index_rows(&hits), true);
        let change = ViewChange::Goto {
            goto: self.vista_ir_a(),
        };
        Some(self.parche(vec![change]))
    }

    /// Closes "go to" and abandons the question to the index: whatever comes
    /// after rules, and a late answer no longer has anywhere to land.
    fn close_go_to(&mut self) {
        self.ir_a = None;
        self.gen_ir_a += 1;
        if let Some(old) = self.go_to_index.take() {
            old.abort();
        }
    }

    /// The keys while "go to" is open.
    ///
    /// FIXED: there are no `dialog.*` verbs for typing a character or moving
    /// the selection. `Escape` closes, `Enter` goes, the arrows, pages,
    /// `Home` and `End` move, `F1` explains the row and everything else
    /// types — except the chord of `app.palette` or `app.goto`, which
    /// switches the mode in place. Only a NON-plain chord can: vim's `:` is
    /// `app.palette`, and inside the box it must type a colon.
    pub(super) fn key_in_goto(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let page = self.goto_page();
        let Some(g) = self.ir_a.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        match k.key.as_str() {
            "Escape" | "esc" => self.close_go_to(),
            "Enter" | "enter" => return self.enter_in_goto(backend, mailbox),
            "ArrowDown" | "down" => g.down(),
            "ArrowUp" | "up" => g.up(),
            "PageDown" | "pgdn" => g.page_down(page),
            "PageUp" | "pgup" => g.page_up(page),
            "Home" | "home" => g.home(),
            "End" | "end" => g.end(),
            "F1" | "f1" => {
                let topic = g
                    .selected()
                    .and_then(|r| norte_frontend::goto::help_topic(&r.key, self.lang))
                    .map(|t| t.id.as_str().to_owned());
                let Some(id) = topic else {
                    return (self.applied(), self.say("msg-palette-no-help"));
                };
                // The close in its own patch, before help's, like a confirm.
                self.close_go_to();
                let closing = self.parche(vec![ViewChange::Goto { goto: None }]);
                let (ack, mut rest) = self.open_help_on(Some(&id), backend, mailbox);
                let mut out = vec![closing];
                out.append(&mut rest);
                return (ack, out);
            }
            "Backspace" | "backspace" => {
                g.backspace();
                self.request_goto_from_index(backend, mailbox);
            }
            other => {
                // A TEXT key is a code point, not a UTF-16 unit nor a key
                // name: `ArrowLeft` is not typed.
                let mut chars = other.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) if !k.ctrl && !k.alt && !k.meta => {
                        g.push_char(c);
                        self.request_goto_from_index(backend, mailbox);
                    }
                    _ => {
                        let Some(query) = self.mode_switch(k) else {
                            return (self.applied(), Vec::new());
                        };
                        if let Some(g) = self.ir_a.as_mut() {
                            g.set_query(query);
                        }
                        self.request_goto_from_index(backend, mailbox);
                    }
                }
            }
        }
        let change = ViewChange::Goto {
            goto: self.vista_ir_a(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// The query a chord switches the open box to: `>` for the chord of
    /// `app.palette`, `""` for `app.goto`'s, `None` for any other.
    fn mode_switch(&self, k: &crate::keys::KeyInput) -> Option<&'static str> {
        let chord = k.to_chord().ok()?;
        if self.effective.single_chord_runs(chord, "app.palette") {
            Some(norte_frontend::goto::PREFIX_COMMANDS)
        } else if self.effective.single_chord_runs(chord, "app.goto") {
            Some("")
        } else {
            None
        }
    }

    /// How many rows a page moves.
    ///
    /// The box is at most 70vh tall (`style.css` `.palette`) and a row is
    /// about a cell; the renderer does not report the painted height, so
    /// this is the honest approximation.
    pub(super) fn goto_page(&self) -> usize {
        (usize::from(self.viewport.1) * 7 / 10)
            .saturating_sub(2)
            .max(1)
    }

    /// Enter on the chosen row: a prefix row types itself and the box stays
    /// open; anything else is confirmed.
    fn enter_in_goto(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(g) = self.ir_a.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        let act = g.confirm();
        if let Some(Action::SetQuery(q)) = &act {
            g.set_query(q);
            self.request_goto_from_index(backend, mailbox);
            let change = ViewChange::Goto {
                goto: self.vista_ir_a(),
            };
            return (self.applied(), vec![self.parche(vec![change])]);
        }
        self.confirm_go_to(act, backend, mailbox)
    }

    /// A hover: moves the cursor to a row. A header or a stale index moves
    /// nothing, and says nothing.
    pub(super) fn point_in_goto(&mut self, row: u32) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(g) = self.ir_a.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        if !g.point(usize::try_from(row).unwrap_or(usize::MAX)) {
            return (self.applied(), Vec::new());
        }
        let change = ViewChange::Goto {
            goto: self.vista_ir_a(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// A click: runs the line the host has open, the same as Enter on it.
    pub(super) fn activate_in_goto(
        &mut self,
        row: u32,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(g) = self.ir_a.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        if !g.point(usize::try_from(row).unwrap_or(usize::MAX)) {
            return (self.applied(), Vec::new());
        }
        self.enter_in_goto(backend, mailbox)
    }

    /// A paste into the query: its first line only (a newline must never
    /// confirm), capped (a clipboard is untrusted and unbounded).
    pub(super) fn paste_in_goto(
        &mut self,
        text: &str,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        const MAX_PASTE_CHARS: usize = 1024;
        let Some(g) = self.ir_a.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        let line: String = text
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(MAX_PASTE_CHARS)
            .collect();
        g.push_str(&line);
        self.request_goto_from_index(backend, mailbox);
        let change = ViewChange::Goto {
            goto: self.vista_ir_a(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// Confirms the chosen row: closes the screen BEFORE acting — the close
    /// in its own patch, so that whatever the effect opens does not end up
    /// underneath it — and does what `norte_frontend::goto::Goto::confirm`
    /// decided.
    fn confirm_go_to(
        &mut self,
        act: Option<Action>,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.close_go_to();
        let closing = self.parche(vec![ViewChange::Goto { goto: None }]);
        let Some(act) = act else {
            return (self.applied(), vec![closing]);
        };
        let (ack, mut rest) = match act {
            Action::Ir(dir) => (
                self.applied(),
                self.navigate(&dir, Trail::Record, backend, mailbox),
            ),
            Action::Command(cmd) => self.run_command_key(&cmd, backend, mailbox),
            Action::Nothing(reason) => (self.applied(), self.say(reason)),
            Action::Help(id) => self.open_help_on(Some(&id), backend, mailbox),
            // Handled by the caller, before closing.
            Action::SetQuery(_) => (self.applied(), Vec::new()),
            Action::Unavailable(why) => {
                self.status.message = Some(clamp_display(why));
                (
                    self.applied(),
                    vec![self.parche(vec![ViewChange::Status(self.status.clone())])],
                )
            }
        };
        let mut outgoing = vec![closing];
        outgoing.append(&mut rest);
        (ack, outgoing)
    }

    /// "Go to"'s projection.
    pub(super) fn vista_ir_a(&self) -> Option<crate::dto::GotoView> {
        let g = self.ir_a.as_ref()?;
        let rows = g.rows();
        // ONE view line per model line, without skipping any: the cursor is
        // an index into `lines`, and a dropped line would throw it off
        // silently. The invariant — `GotoLine::Row(i)` always names a row
        // that exists — is kept by `Goto::refresh`, which pushes both at
        // once; if it ever broke, an empty row comes out and the cursor keeps
        // pointing at the same thing as the model.
        let lines: Vec<crate::dto::GotoLineView> = g
            .lines()
            .iter()
            .map(|l| match l {
                GotoLine::Header(s) => crate::dto::GotoLineView::Header {
                    title: norte_i18n::t_in(self.lang, s.title_key),
                },
                GotoLine::Row(i) => {
                    let r = rows.get(*i);
                    let shown = |f: fn(&GotoRow) -> Option<&String>| {
                        clamp_display(r.and_then(f).cloned().unwrap_or_default())
                    };
                    crate::dto::GotoLineView::Row {
                        text: clamp_display(r.map(|r| r.text.clone()).unwrap_or_default()),
                        desc: clamp_display(r.map(|r| r.desc.clone()).unwrap_or_default()),
                        hostile: r.is_some_and(|r| r.hostile),
                        chord: shown(|r| r.chord.as_ref()),
                        category: shown(|r| r.category.as_ref()),
                        unavailable: shown(|r| r.unavailable.as_ref()),
                        recent: r.is_some_and(|r| r.recent),
                        positions: r.map(|r| r.positions.clone()).unwrap_or_default(),
                    }
                }
            })
            .take(crate::bridge::MAX_ROWS_PER_BATCH)
            .collect();
        Some(crate::dto::GotoView {
            // Masked, as the TUI paints it: a pasted bidi control must not
            // reorder the line.
            query: clamp_display(g.query_display()),
            cursor: (!lines.is_empty() && !g.is_empty()).then_some(g.cursor() as u64),
            lines,
            empty: norte_i18n::t_in(self.lang, "goto-empty"),
            mode: match g.mode() {
                Mode::Places => crate::dto::GotoModeView::Places,
                Mode::Commands => crate::dto::GotoModeView::Commands,
                Mode::Help => crate::dto::GotoModeView::Help,
            },
            hint: g
                .hint()
                .map(|k| norte_i18n::t_in(self.lang, k))
                .unwrap_or_default(),
        })
    }
}
