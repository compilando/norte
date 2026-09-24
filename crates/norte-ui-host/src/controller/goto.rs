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
    Action, BROUGHT_BY_LIST, FixedSource, Goto, GotoLine, GotoRow, GotoSource, INDEX_CAP,
    MINIMUM_FOR_THE_INDEX, PathSource, SECTION_COMMANDS, SECTION_CONNECTIONS, SECTION_FAVORITES,
    SECTION_HISTORY, SECTION_INDEX, SECTION_POPULAR,
};

impl State {
    /// Opens "go to".
    ///
    /// The rows are taken as a SNAPSHOT on open, just like the palette: a
    /// list that changes under the cursor while it is being read is how an
    /// Enter ends up somewhere else. The two exceptions arrive late, through
    /// the mailbox: CONNECTIONS (only the daemon knows them, not this
    /// process) and whatever the index finds (one question per query).
    pub(super) fn open_go_to(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let sources = self.fuentes_de_ir_a();
        // Reopening while the screen is already open (from a menu, say)
        // closes the previous one properly: its question to the index is
        // aborted instead of continuing to spend a provider on a query that
        // no longer exists.
        self.close_go_to();
        self.ir_a = Some(Goto::new(sources));
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
        let change = ViewChange::Goto {
            goto: self.vista_ir_a(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// The SYNCHRONOUS sources: what this window already has in memory.
    /// Connections start empty and fill in once the daemon answers.
    fn fuentes_de_ir_a(&self) -> Vec<Box<dyn GotoSource + Send>> {
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
        // The commands, the SAME ones this window's palette offers: the
        // palette already resolves which ones this host implements and with
        // what effects.
        let commands = norte_frontend::goto::command_rows(self.palette_rows());
        out.push(Box::new(
            FixedSource::new(SECTION_COMMANDS, commands).only_with_query(),
        ));
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
        goto.replace_section(SECTION_CONNECTIONS, rows, false);
        let change = ViewChange::Goto {
            goto: self.vista_ir_a(),
        };
        Some(self.parche(vec![change]))
    }

    /// Asks the index about what is typed, if it is worth it.
    ///
    /// Relaunching ABORTS the previous question. Below
    /// [`MINIMUM_FOR_THE_INDEX`] it does not ask and EMPTIES the section:
    /// leaving there what answered a longer query is showing an answer to a
    /// question that is no longer being asked.
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
        let query = goto.query().to_owned();
        // Three cases where it does not ask, and all three EMPTY the section:
        // - a short query cannot be good;
        // - a typed PATH is not a semantic query, and sending it to an
        //   embeddings provider — maybe remote — is sending it the name of a
        //   directory of the reader's;
        // - a read-only window does not let queries leave the process, same
        //   as its explicit semantic search (`QuerySemantic`).
        let read_only = self.effects == crate::commands::Effects::SoloRead;
        if query.chars().count() < MINIMUM_FOR_THE_INDEX
            || norte_frontend::goto::looks_path(&query).is_some()
            || read_only
        {
            goto.replace_section(SECTION_INDEX, Vec::new(), true);
            return;
        }
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
        if goto.query() != query {
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
    /// FIXED, like the palette's and for the same reason: there are no
    /// `dialog.*` verbs for typing a character or moving the selection.
    /// `Escape` closes, `Enter` goes, the arrows move and everything else
    /// types.
    pub(super) fn key_in_goto(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(g) = self.ir_a.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        match k.key.as_str() {
            "Escape" | "esc" => self.close_go_to(),
            "Enter" | "enter" => {
                let key = g.selected().map(|r| r.key.clone());
                return self.confirm_go_to(key, backend, mailbox);
            }
            "ArrowDown" | "down" => g.down(),
            "ArrowUp" | "up" => g.up(),
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
                    _ => return (self.applied(), Vec::new()),
                }
            }
        }
        let change = ViewChange::Goto {
            goto: self.vista_ir_a(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// Confirms the chosen row: closes the screen BEFORE acting — the close
    /// in its own patch, like the palette, so that whatever the effect opens
    /// does not end up underneath it — and does what
    /// `norte_frontend::goto::action` decides.
    fn confirm_go_to(
        &mut self,
        key: Option<String>,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.close_go_to();
        let closing = self.parche(vec![ViewChange::Goto { goto: None }]);
        let Some(key) = key else {
            return (self.applied(), vec![closing]);
        };
        let (ack, mut rest) = match norte_frontend::goto::action(&key) {
            Action::Ir(dir) => (
                self.applied(),
                self.navigate(&dir, Trail::Record, backend, mailbox),
            ),
            // Through the SAME path as a key: "go to" is another door into
            // the catalogue, not a second dispatcher.
            Action::Command(cmd) => match effect_of(&cmd, 1) {
                Some(effect) => self.apply_effect(effect, backend, mailbox),
                None => self.no_implementado(&cmd),
            },
            Action::Nothing(reason) => (self.applied(), self.say(reason)),
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
                    crate::dto::GotoLineView::Row {
                        text: clamp_display(r.map(|r| r.text.clone()).unwrap_or_default()),
                        desc: clamp_display(r.map(|r| r.desc.clone()).unwrap_or_default()),
                        hostile: r.is_some_and(|r| r.hostile),
                    }
                }
            })
            .collect();
        Some(crate::dto::GotoView {
            query: clamp_display(g.query().to_owned()),
            cursor: (!lines.is_empty() && !g.is_empty()).then_some(g.cursor() as u64),
            lines,
            empty: norte_i18n::t_in(self.lang, "goto-empty"),
        })
    }
}
