//! The help screen's model: which topic is open, its scroll and the
//! navigation history within the help itself.

use super::plugins::plugin_label;
use norte_i18n::t;

/// The resolver [`App::new`] starts with: the ORTHODOX preset with no user
/// and no project layer, in the language of the environment.
///
/// `main.rs` OVERWRITES [`App::help_chords`] at startup and on every hot
/// reload with the effectives actually in force, exactly as it does with
/// `help_lines`, `palette_rows` and `dialog_hints` — a rebind that does not
/// reach the resolver is a help page that teaches the OLD key. This default
/// only exists so `App::new` stays infallible for the render tests and the
/// paths that never load a keymap at all.
///
/// Built ONCE per process: `Effective::build_for` materialises the whole
/// merged keymap for three screens, and `App::new` runs in every render test.
///
/// Which makes this dead weight in the BINARY, and deliberately so: `App::new`
/// forces it and `main.rs` throws it away on the very next lines. It earns its
/// place in the render tests, which build an `App` and never load a keymap —
/// and nowhere else.
///
/// One consequence a test author has to know: the LABEL of every row is frozen
/// here at `Lang::from_env()`, because `TuiChords` resolves labels once, when
/// it is built. The prose and the chords do not depend on it, but a test that
/// asserts on a help label through this default reads whatever locale the
/// machine running it happens to have — so such a test must build its own
/// `TuiChords` with the language it means (`snapshots_ui.rs`'s `open_help`
/// does exactly that).
pub(crate) fn default_help_chords() -> std::sync::Arc<crate::help::TuiChords> {
    static DEFAULT: std::sync::LazyLock<std::sync::Arc<crate::help::TuiChords>> =
        std::sync::LazyLock::new(|| {
            // Both `expect`s rest on the same invariant: the preset and the
            // vocabulary are BINARY CONSTANTS, and `tests/keymap.rs` merges
            // this exact combination for all three screens — an invalid one
            // fails the suite, never a user's session. `presets()` itself
            // panics on the same grounds.
            let (_, preset) = crate::keymap::presets()
                .into_iter()
                .find(|(n, _)| *n == "orthodox")
                .expect("`presets()` always ships the orthodox preset");
            // The `dialog` effective merges `[global]` too, so the vocabulary
            // is the UNION — `DIALOG_COMMANDS` alone would reject the preset.
            let known: Vec<&str> = crate::keymap::COMMANDS
                .iter()
                .copied()
                .chain(crate::keymap::DIALOG_COMMANDS.iter().copied())
                .collect();
            let eff = |screen| {
                crate::keymap::Effective::build_for(&preset, &[], &known, screen)
                    .expect("the embedded orthodox preset merges for every screen")
            };
            std::sync::Arc::new(crate::help::TuiChords::new(
                &eff(crate::keymap::Screen::Browse),
                &eff(crate::keymap::Screen::Viewer),
                &eff(crate::keymap::Screen::Dialog),
                norte_i18n::Lang::from_env(),
            ))
        });
    std::sync::Arc::clone(&DEFAULT)
}

/// State of the help overlay (H3b): the shared navigation model, the
/// generated keyboard page, and the body as last laid out.
///
/// The body is PRE-RENDERED into the state rather than laid out by the
/// painter, which is this repo's existing idiom (`help_lines`,
/// [`crate::app::App::palette_rows`], [`crate::hints::DialogHints`] are all precomputed
/// and rebuilt on hot reload). The reason is concrete:
/// `HelpState::reveal`/`clamp_scroll` need the laid-out LINE COUNT, `draw_*`
/// only ever gets a `&App`, and a renderer that cannot tell the model what it
/// laid out leaves `body_scroll` unbounded — a reader who pages past the end
/// gets a permanently blank body.
#[derive(Debug, Clone)]
pub struct HelpView {
    /// Sidebar, body scroll, filter, history and focus.
    pub state: norte_frontend::help::HelpState,
    /// The effective-keymap cheatsheet ([`crate::help::build`]), the body of
    /// the synthetic `keys` entry — already styled (K3b: an unavailable row
    /// is dimmed there, not here). Rebuilt on hot reload with everything else
    /// derived from the keymap.
    pub keys_lines: Vec<ratatui::text::Line<'static>>,
    /// Body lines and the action→line map of whatever `state.current()` is,
    /// laid out for `width`. See [`HelpView::refresh`].
    ///
    /// `'static` because a rendering that borrowed from [`Self::state`] would
    /// make this a self-referential struct. That is free for the corpus —
    /// `HelpState::topic` hands back a `&'static Topic`, so `render_topic`
    /// produces a `Rendered<'static>` outright — and costs one clone of the
    /// VISIBLE page for a plugin's, whose topic `HelpState` owns
    /// (`crate::help_render::into_static`, H3e).
    body: crate::help_render::Rendered<'static>,
    /// Plugin ids whose page has already been ASKED FOR in this overlay (H3e).
    ///
    /// `HelpState::plugin_needs_fetch` is a POLLING question, not an event: it
    /// keeps answering `Some(id)` until the page is installed, so the run loop
    /// would re-issue the request on every frame — and forever against a daemon
    /// that cannot answer. This set is what turns it into an event, and it
    /// covers BOTH halves at once: the request in flight, and the ones that
    /// already answered. A success stops answering by itself
    /// (`install_plugin_topic`); a failure is what needs remembering.
    ///
    /// Lives on the VIEW, so its scope is the open overlay: closing and
    /// reopening the help asks again, which is the only retry a reader has and
    /// the only one they can ask for.
    asked: std::collections::BTreeSet<String>,
    /// Publisher of each plugin of the snapshot, keyed by id — already masked
    /// and capped ([`plugin_label`]).
    ///
    /// Kept here and not in `norte_frontend::help::PluginNode` because only one
    /// caller needs it and only once: `norte_help::parse_untrusted` takes the
    /// publisher as the attribution of the page it is about to build, and the
    /// page is parsed when its `plugin.help` answer arrives — long after the
    /// snapshot that knew the publisher was taken.
    publishers: std::collections::BTreeMap<String, String>,
    /// `true` when the overlay was opened while a modal was ALREADY on screen
    /// (H3c).
    ///
    /// It decides who owns the keys, and the two directions are different
    /// events:
    ///
    /// * opened FROM a modal (this flag `true`) the help owns them. The reader
    ///   asked to read about the question in front of them, so `Esc` has to put
    ///   them back in front of it rather than answer it, and the modal's own
    ///   verbs stay unreachable meanwhile — an agent operation is approved by
    ///   looking at it, never by a key pressed blind through a page.
    /// * a modal ARRIVING over an already-open help (this flag `false`) closes
    ///   the help instead, exactly as it closes the palette and the settings
    ///   overlay: the next key must land where the pixels point.
    ///
    /// Two consequences, both deliberate. The modal keeps being painted LAST
    /// ([`crate::ui::draw`]), so a help opened over it does not hide the
    /// question — the box stays on top of the page, and its verbs simply do
    /// nothing until the help closes. And a help page left open over an agent
    /// approval lets its TTL expire, which DENIES the agent: fail-closed, which
    /// is the direction to fail in.
    pub over_modal: bool,
    /// Height of the body window at the last [`refresh`](Self::refresh), in
    /// lines. What a page means ([`page`](Self::page)) and where an arrow key
    /// stops walking actions and starts scrolling ([`line_down`](Self::line_down))
    /// both depend on it, and only the layout knows it.
    height: usize,
}

impl HelpView {
    /// Opens the overlay on the index topic of `lang`, with `keys_lines` as
    /// the body of the synthetic keyboard entry.
    ///
    /// The label of that entry is resolved HERE and handed to the model:
    /// `norte_frontend::help` has no Fluent access on purpose, and this is
    /// the frontend that names the page. Resolving it once, at the seam,
    /// keeps the sidebar and the filter looking at the same string — a
    /// painter-side special case would only make the row unfindable by the
    /// name it wears.
    ///
    /// The body starts EMPTY: nothing has been laid out yet because nothing
    /// knows how wide the terminal is. [`refresh`](Self::refresh) is what
    /// fills it, and the run loop calls it before every paint.
    #[must_use]
    pub fn new(lang: norte_help::Lang, keys_lines: Vec<ratatui::text::Line<'static>>) -> Self {
        Self {
            state: norte_frontend::help::HelpState::new(lang, t("help-topic-keys")),
            keys_lines,
            body: crate::help_render::Rendered {
                lines: Vec::new(),
                action_lines: Vec::new(),
                heading_lines: Vec::new(),
            },
            asked: std::collections::BTreeSet::new(),
            publishers: std::collections::BTreeMap::new(),
            over_modal: false,
            height: 0,
        }
    }

    /// Lines a page moves in the body: the window less two lines of overlap,
    /// so the reader keeps their place across the jump. Never zero.
    #[must_use]
    pub fn page(&self) -> usize {
        self.height.saturating_sub(2).max(1)
    }

    /// Where each heading of the open page landed, in body lines.
    #[must_use]
    pub fn heading_lines(&self) -> &[usize] {
        &self.body.heading_lines
    }

    /// `]`: the next section at the top of the window. Past the last heading,
    /// the end of the page — a key that does nothing reads as broken.
    ///
    /// Whatever half has the focus: the sections are the BODY's, and jumping
    /// to one is reading, so the view leads (`scroll_body_to`).
    pub fn section_next(&mut self) {
        let scroll = self.state.body_scroll();
        match self.body.heading_lines.iter().find(|&&l| l > scroll) {
            Some(&linea) => self.state.scroll_body_to(linea),
            None => self.state.scroll_body_to(usize::MAX),
        }
    }

    /// `[`: the previous section at the top of the window; before the first
    /// heading, the start of the page.
    pub fn section_prev(&mut self) {
        let scroll = self.state.body_scroll();
        let linea = self
            .body
            .heading_lines
            .iter()
            .rev()
            .find(|&&l| l < scroll)
            .copied()
            .unwrap_or(0);
        self.state.scroll_body_to(linea);
    }

    /// Down arrow: one topic in the sidebar; in the body, the next action if
    /// it is on screen, and otherwise one line of prose.
    pub fn line_down(&mut self) {
        self.step(true);
    }

    /// Up arrow: see [`line_down`](Self::line_down).
    pub fn line_up(&mut self) {
        self.step(false);
    }

    /// The body half of the arrows. The actions sit AFTER all the prose, so
    /// an arrow that always walked them jumped a long page to its end before
    /// the reader had read it; one that always scrolled could never reach
    /// them. So: walk the actions while they are in view, scroll otherwise.
    fn step(&mut self, down: bool) {
        if self.state.focus() != norte_frontend::help::Focus::Body {
            if down {
                self.state.down();
            } else {
                self.state.up();
            }
            return;
        }
        let height = self.height.max(1);
        let scroll = self.state.body_scroll();
        let window = scroll..scroll.saturating_add(height);
        let lines = &self.body.action_lines;
        let visible = |i: usize| lines.get(i).is_some_and(|l| window.contains(l));
        let cur = self.state.action_cursor();
        if visible(cur) {
            let next = if down {
                cur.checked_add(1)
            } else {
                cur.checked_sub(1)
            };
            if next.is_some_and(visible) {
                if down {
                    self.state.down();
                } else {
                    self.state.up();
                }
                return;
            }
        } else {
            // The cursor is off screen: the arrow lands on the nearest action
            // that is on it, in the direction of travel, before any scroll.
            let mut in_view = (0..lines.len()).filter(|&i| visible(i));
            let hit = if down {
                in_view.next()
            } else {
                in_view.next_back()
            };
            if let Some(i) = hit {
                self.state.settle_action_cursor(i);
                return;
            }
        }
        let max = self.body.lines.len().saturating_sub(height);
        if down && scroll < max {
            self.state.scroll_body(1);
        } else if !down && scroll > 0 {
            self.state.scroll_body(-1);
        } else {
            // Nothing left to scroll that way (or nothing laid out yet): the
            // arrow walks the actions, as it always did. An arrow that does
            // NOTHING at the end of a page is a key the reader reads as broken.
            if down {
                self.state.down();
            } else {
                self.state.up();
            }
            return;
        }
        // Scrolling hands the lead to the view (`scroll_body`); an action the
        // cursor was on that is STILL on screen keeps it, instead of the
        // cursor jumping to the first action in the window.
        let scroll = self.state.body_scroll();
        let window = scroll..scroll.saturating_add(height);
        if self
            .body
            .action_lines
            .get(cur)
            .is_some_and(|l| window.contains(l))
        {
            self.state.settle_action_cursor(cur);
        }
    }

    /// Opens the overlay on the page for `context`, falling back to the index
    /// when no page claims it.
    ///
    /// The fallback is not a papering-over: `norte_help::check_contexts` fails
    /// the documentation gate for a context with no page, so the pages that are
    /// still missing are on a shrinking allowlist and nothing else can reach
    /// here. The index is the least surprising place to land.
    ///
    /// The contextual page arrives as the ROOT of the trail
    /// (`HelpState::open_as_root`): `F1` putting the reader on a page is not
    /// navigation the reader did, so `Esc` must close the overlay instead of
    /// walking back to an index they never asked for.
    ///
    /// `over_modal` is the caller's answer to "was a modal already on screen?"
    /// — see the field for what it decides.
    #[must_use]
    pub fn new_at(
        lang: norte_help::Lang,
        keys_lines: Vec<ratatui::text::Line<'static>>,
        context: &str,
        over_modal: bool,
    ) -> Self {
        if let Some(topic) = norte_help::topic_for_context(lang, context) {
            return Self::new_at_topic(lang, keys_lines, &topic.id, over_modal);
        }
        let mut view = Self::new(lang, keys_lines);
        view.over_modal = over_modal;
        view
    }

    /// Opens the help on a page the caller already picked, instead of on a
    /// context the corpus resolves (H3c).
    ///
    /// The sibling of [`new_at`](Self::new_at) for the other bridge into the
    /// corpus: `F1` on a command palette row opens the page that DOCUMENTS that
    /// command ([`norte_help::topic_for_command`]), which is a topic id in hand
    /// and not a place the reader is standing in.
    ///
    /// Same trail treatment for the same reason — the page arrives as the ROOT
    /// (`HelpState::open_as_root`), because being PUT on a page is not
    /// navigation the reader did and `Esc` has to close the overlay rather than
    /// walk back to an index they never saw.
    #[must_use]
    pub fn new_at_topic(
        lang: norte_help::Lang,
        keys_lines: Vec<ratatui::text::Line<'static>>,
        topic: &norte_help::TopicId,
        over_modal: bool,
    ) -> Self {
        let mut view = Self::new(lang, keys_lines);
        view.over_modal = over_modal;
        view.state.open_as_root(topic);
        view
    }

    /// Lays the open page out for `width` and re-establishes the scroll
    /// invariants: clamps `body_scroll` to what exists, and reveals the
    /// focused action when the body has the focus.
    ///
    /// Call after ANY change to what is shown — opening a topic, going back,
    /// moving either cursor, editing the filter, a resize, a hot reload —
    /// and before painting. Cheap: the corpus is static and eight topics.
    pub fn refresh(
        &mut self,
        chords: &crate::help::TuiChords,
        width: usize,
        height: usize,
        theme: &crate::theme::TuiTheme,
    ) {
        let lang = self.state.lang();
        self.body = if let Some(topic) = self.state.topic() {
            // A corpus page. Asked for FIRST and through `topic()` rather than
            // `current_topic()` for the lifetime alone: this one is `'static`,
            // so the common case keeps rendering straight into the field with
            // nothing cloned. `current_topic()` resolves the corpus first too,
            // so the two can never pick different pages.
            crate::help_render::render_topic(topic, lang, chords, width, theme)
        } else if let Some(topic) = self.state.current_topic() {
            // A plugin page (H3e): owned by the model, so the rendering that
            // borrows it has to be detached before it can be stored.
            crate::help_render::into_static(crate::help_render::render_topic(
                topic, lang, chords, width, theme,
            ))
        } else if self.state.current().as_str() == norte_frontend::help::KEYS_ID {
            // The synthetic `keys` page: its body is the effective keymap,
            // generated text with no runnable rows and therefore no action
            // map — a chord is not something Enter runs.
            //
            // Keyed on the ID and not on "no topic resolved", which is the same
            // branch written the safe way round. Three different states answer
            // `None` to `current_topic()` — the keyboard page, a plugin page in
            // flight, and a `current` naming a page that no longer exists — and
            // only the first is this one. `HelpState` can reach the third:
            // `rebuild_rows` moves the body onto a surviving row, but with the
            // sidebar left EMPTY by a filter there is nowhere to move to and it
            // deliberately keeps showing what was being read. Unreachable in
            // this binary (the catalogue is only ever installed on the open
            // path, before any filter), but "unreachable" is a claim about
            // callers and this is a claim about the id.
            // Already styled (K3b: an unavailable row is dimmed by
            // `crate::help::build`, not here) — no `Line::raw` mapping left
            // to do.
            crate::help_render::Rendered {
                lines: self.keys_lines.clone(),
                action_lines: Vec::new(),
                heading_lines: Vec::new(),
            }
        } else {
            // A plugin page still in flight — and any other page that resolves
            // to nothing. EMPTY, never the keyboard page: nothing else on
            // screen tells the two apart, and the whole cheatsheet appearing
            // under an extension's name would read as that extension's own
            // documentation.
            crate::help_render::Rendered {
                lines: Vec::new(),
                action_lines: Vec::new(),
                heading_lines: Vec::new(),
            }
        };
        self.height = height;
        // The LAST screen is a full one: clamping to the last line let a
        // reader page on until one line sat over a box of blank rows.
        self.state
            .clamp_scroll_window(self.body.lines.len(), height.max(1));
        if self.state.focus() != norte_frontend::help::Focus::Body {
            return;
        }
        // Focus JUST arrived at the body (or the reader just paged):
        // then the VIEW leads. The cursor lands on the first action that
        // falls inside the window, and if there's none it stays wherever it
        // is without dragging anything. Before this, `Tab` took you to the
        // first runnable line — behind all the prose on a long page — so it
        // didn't look like switching columns: it looked like jumping to the
        // end.
        if self.state.action_follows_view() {
            let window = self.state.body_scroll()..self.state.body_scroll().saturating_add(height);
            if let Some(i) = self
                .body
                .action_lines
                .iter()
                .position(|line| window.contains(line))
            {
                self.state.settle_action_cursor(i);
            }
            return;
        }
        // And if the cursor moved, IT leads: the view follows it.
        //
        // The guard is not defensive noise: a topic with neither commands nor
        // `see_also` has no line to reveal, and `HelpState` only refuses the
        // FOCUS on an empty action list — the cursor itself can be stale for
        // one frame after a filter rebuilt the page under it.
        if let Some(&line) = self.body.action_lines.get(self.state.action_cursor()) {
            self.state.reveal(line, height);
        }
    }

    /// Body lines to paint and the line each action landed on.
    #[must_use]
    pub fn body(&self) -> (&[ratatui::text::Line<'static>], &[usize]) {
        (&self.body.lines, &self.body.action_lines)
    }

    /// The plugin id whose page must be fetched NOW, claiming it so the next
    /// call does not ask again (H3e). `None` when there is nothing to fetch or
    /// the open page has already been asked for.
    ///
    /// The claim is what makes `HelpState::plugin_needs_fetch` — which polls,
    /// see the view's own `asked` set — usable from a run loop that visits it
    /// every frame.
    /// Claiming BEFORE the request, rather than after it succeeds, is the whole
    /// point: the case worth guarding is the one where the answer never comes.
    ///
    /// ```
    /// use norte_frontend::help::PluginNode;
    /// use norte_help::{Lang, TopicId};
    /// use norte_tui::app::HelpView;
    ///
    /// let mut view = HelpView::new(Lang::En, Vec::new());
    /// view.state.set_plugins(vec![PluginNode {
    ///     id: "acme.ftp".to_owned(),
    ///     title: "FTP".to_owned(),
    ///     has_help: true,
    ///     active: true,
    /// }]);
    /// view.state.open(&TopicId::new("acme.ftp"));
    /// assert_eq!(view.claim_plugin_fetch().as_deref(), Some("acme.ftp"));
    /// // Asked once. A page that never arrives is not asked for again.
    /// assert_eq!(view.claim_plugin_fetch(), None);
    /// ```
    pub fn claim_plugin_fetch(&mut self) -> Option<String> {
        let id = self.state.plugin_needs_fetch()?.to_owned();
        self.asked.insert(id.clone()).then_some(id)
    }

    /// Installs the plugin catalogue this overlay was opened with (H3e).
    ///
    /// The ONE ingest point for third-party text into the help model: `name`
    /// and `publisher` are masked and capped HERE ([`plugin_label`]), exactly as
    /// the palette does with a plugin's `description`, because
    /// `norte_frontend::help::PluginNode` documents its `title` as already safe
    /// and the model masks nothing.
    ///
    /// A node is ACTIVE when the plugin is approved AND enabled. That decides
    /// whether its command rows are runnable, never whether its page shows: a
    /// human reads a plugin's documentation precisely in order to decide
    /// whether to enable it.
    ///
    /// A BLANK `name` falls back to the plugin's id. `name` is required in the
    /// manifest but never checked for content, so `name = "\u{3164}\u{3164}"`
    /// — HANGUL FILLERs, which are not whitespace and survive masking — is a
    /// legal manifest whose sidebar row paints as an empty line under the
    /// `Extensions` header: a page the reader can move onto, open, and read,
    /// attached to a name that says nothing. The id is the one identifier the
    /// host assigns, so it is what the row falls back to; it goes through
    /// [`plugin_label`] like everything else, because until the id itself is
    /// validated at this seam it is no more trustworthy than the name.
    ///
    /// `norte_help::is_blank_id` and not `str::trim().is_empty()`: the filler
    /// characters this exists to catch are not whitespace, so a trim-based
    /// check answers "not blank" about a string that paints nothing. Asked
    /// AFTER masking, so a name of zero-width spaces — hazards rather than
    /// invisibles — has already become `U+FFFD` and counts as blank too.
    ///
    /// The extension MANAGER has the same gap and is deliberately left alone:
    /// its row carries the version and the approval badges beside the name, so
    /// a blank name there is an odd-looking row rather than an unattributed
    /// one. Seen and judged, not missed.
    ///
    /// # The id is validated HERE, and a bad one is DROPPED
    ///
    /// `PluginInfo.id` arrives over the wire. Our own host will only ever send
    /// a reverse-DNS id it validated, but this frontend does not get to assume
    /// the peer enforced what our host enforces — the same reasoning
    /// `norte_frontend::help::HelpState::set_plugins` gives for its own
    /// duplicate and corpus-collision guards. An id is a LOOKUP KEY that flows
    /// straight into `TopicId`, into `plugin_needs_fetch`, and back out as the
    /// argument to `plugin.help`, so it is the one field that must be right
    /// rather than merely paintable.
    ///
    /// DROPPED, never rewritten. Masking an id is not a safety measure — it is
    /// not injective, so it silently maps two distinct plugins onto one row —
    /// and a repaired id would be a key that resolves to nothing or, worse, to
    /// something else. Refusing the node is the only answer that cannot lie:
    /// the reader loses a help page for a plugin the host should not have
    /// announced, and `norte doctor` is where that gets diagnosed. Same
    /// discipline as `norte_help::parse_untrusted`'s command keys, which are
    /// refused rather than rewritten for exactly this reason.
    ///
    /// It also bounds the work: `is_valid_plugin_id` caps the length at 128, so
    /// a megabyte of `id` costs one rejected comparison instead of a masked,
    /// capped copy per plugin and a `TopicId` the sidebar filter folds on every
    /// keystroke.
    pub fn set_plugins(&mut self, plugins: &[norte_proto::methods::PluginInfo]) {
        let plugins: Vec<&norte_proto::methods::PluginInfo> = plugins
            .iter()
            .filter(|p| norte_core::is_valid_plugin_id(&p.id))
            .collect();
        self.publishers = plugins
            .iter()
            .map(|p| (p.id.clone(), plugin_label(&p.publisher)))
            .collect();
        self.state.set_plugins(
            plugins
                .iter()
                .map(|p| {
                    let named = plugin_label(&p.name);
                    norte_frontend::help::PluginNode {
                        id: p.id.clone(),
                        title: if norte_help::is_blank_id(&named) {
                            plugin_label(&p.id)
                        } else {
                            named
                        },
                        has_help: p.has_help,
                        active: p.approved && p.enabled,
                    }
                })
                .collect(),
        );
    }

    /// Who to attribute `id`'s page to, ready to hand to
    /// `norte_help::parse_untrusted`. `None` for a plugin outside the snapshot
    /// or one that declares no publisher — a blank attribution is worse than
    /// none, because the badge would print `published by ` with nothing after
    /// it, which reads as a rendering fault rather than as an absence.
    ///
    /// Blankness is `norte_help::is_blank_id`, not `str::trim().is_empty()`:
    /// `publisher` is a required TOML field that the manifest never checks for
    /// content, and `"\u{3164}"` (HANGUL FILLER) is not whitespace, so a
    /// trim-based check would call it a publisher. Asked AFTER
    /// [`plugin_label`] has masked, so a publisher of zero-width spaces — a
    /// hazard rather than an invisible — is already `U+FFFD` by the time this
    /// looks, and counts as blank too.
    #[must_use]
    pub fn publisher_of(&self, id: &str) -> Option<String> {
        self.publishers
            .get(id)
            .filter(|p| !norte_help::is_blank_id(p))
            .cloned()
    }

    /// `true` while the body shows the generated keyboard page.
    ///
    /// The same predicate [`refresh`](Self::refresh) branches on, so the two
    /// cannot disagree about which body is on screen. Since H3e "no corpus
    /// page" is no longer enough — a plugin page, fetched or in flight, is not
    /// a corpus page either — so both ask the ID.
    #[must_use]
    pub fn on_keys_page(&self) -> bool {
        self.state.current().as_str() == norte_frontend::help::KEYS_ID
    }
}

#[cfg(test)]
mod help_view_tests {
    use crate::app::{ALLOW_HELP, HelpOutcome, HelpView, help_action};
    use crate::keymap::DIALOG_COMMANDS;
    use norte_frontend::help::{Focus, KEYS_ID};
    use norte_help::TopicId;
    use norte_i18n::Lang;

    /// The default resolver plus the shipped theme: deterministic, and the
    /// same pair `App` starts with.
    fn refresh(view: &mut HelpView, width: usize, height: usize) {
        let chords = crate::app::default_help_chords();
        let theme = crate::theme::TuiTheme::new(
            norte_theme::Theme::preset_default(),
            norte_theme::ColorDepth::Truecolor,
        );
        view.refresh(&chords, width, height, &theme);
    }

    fn flatten(lines: &[ratatui::text::Line<'static>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_new_view_opens_on_the_index_with_an_empty_body() {
        let view = HelpView::new(Lang::En, Vec::new());
        assert_eq!(view.state.current().as_str(), "index");
        assert!(!view.on_keys_page(), "the index IS a corpus topic");
        let (lines, actions) = view.body();
        assert!(
            lines.is_empty() && actions.is_empty(),
            "nothing is laid out until `refresh` knows how wide the terminal is"
        );
    }

    /// The allowlist and the dispatcher are ONE list: a verb the footer hint
    /// advertises (`DialogHints::help`, generated from `ALLOW_HELP`) and that
    /// dispatch drops is a hint that lies. Swept over the WHOLE `dialog`
    /// vocabulary, so a verb added to `ALLOW_HELP` without an arm — or an arm
    /// added without the allowlist — fails here.
    #[test]
    fn help_action_accepts_exactly_the_allowlist() {
        for cmd in ALLOW_HELP {
            assert!(
                help_action(cmd).is_some(),
                "{cmd} is allowed but dispatches to nothing"
            );
        }
        let mut outside = 0_usize;
        for cmd in DIALOG_COMMANDS {
            if ALLOW_HELP.contains(cmd) {
                continue;
            }
            outside += 1;
            assert_eq!(
                help_action(cmd),
                None,
                "{cmd} is outside `ALLOW_HELP`: the key must be INERT"
            );
        }
        assert!(
            outside > 5,
            "the sweep must actually cover verbs the overlay refuses: {outside}"
        );
        assert_eq!(help_action("pane.copy"), None, "not even a `dialog.*` verb");
        // Named samples, so a regression says WHICH arm was transposed.
        assert_eq!(help_action("dialog.confirm"), Some(HelpOutcome::Activate));
        assert_eq!(help_action("dialog.cancel"), Some(HelpOutcome::Close));
        assert_eq!(help_action("dialog.pane"), Some(HelpOutcome::TogglePane));
        assert_eq!(help_action("dialog.back"), Some(HelpOutcome::Back));
        assert_eq!(help_action("dialog.filter"), Some(HelpOutcome::StartFilter));
    }

    /// Why the body is pre-rendered at all: `page_down` deliberately does not
    /// bound itself (the model cannot know how many lines the prose wrapped
    /// into), so without this clamp a reader who pages past the end gets a
    /// PERMANENTLY blank body — no key scrolls back into a body that is not
    /// there.
    #[test]
    fn refresh_clamps_a_scroll_paged_past_the_end() {
        let mut view = HelpView::new(Lang::En, Vec::new());
        view.state.open(&TopicId::new("copying"));
        view.state.toggle_focus();
        assert_eq!(view.state.focus(), Focus::Body, "`copying` has actions");
        view.state.page_down(1_000_000);
        refresh(&mut view, 60, 10);
        let (lines, _) = view.body();
        assert!(!lines.is_empty(), "the topic laid out");
        assert!(
            view.state.body_scroll() < lines.len(),
            "scroll {} outside a body of {} lines: the page is blank",
            view.state.body_scroll(),
            lines.len()
        );
    }

    /// The other half of the same contract: with the focus on the body, the
    /// action the cursor is on has to be ON SCREEN — the cursor walks
    /// ACTIONS and the body scrolls in LINES, and only the renderer can
    /// translate one into the other.
    #[test]
    fn refresh_reveals_the_focused_action() {
        let mut view = HelpView::new(Lang::En, Vec::new());
        view.state.open(&TopicId::new("copying"));
        view.state.toggle_focus();
        assert_eq!(view.state.focus(), Focus::Body);
        for _ in 0..50 {
            view.state.down();
        }
        let height = 6;
        refresh(&mut view, 60, height);
        let (_, action_lines) = view.body();
        let line = action_lines[view.state.action_cursor()];
        let first = view.state.body_scroll();
        assert!(
            (first..first + height).contains(&line),
            "action line {line} outside the window [{first}, {}): the reader \
             cannot see what Enter would run",
            first + height
        );
    }

    /// Down in the body READS: with no action in view it scrolls the prose one
    /// line, instead of jumping to the first action behind all of it — which
    /// on a long page is the end of the page.
    #[test]
    fn down_in_the_body_scrolls_the_prose_when_no_action_is_in_view() {
        let mut view = HelpView::new(Lang::En, Vec::new());
        view.state.open(&TopicId::new("copying"));
        view.state.toggle_focus();
        refresh(&mut view, 60, 6);
        let (_, action_lines) = view.body();
        assert!(
            action_lines[0] >= 6,
            "copying opens on prose, not on an action"
        );
        view.line_down();
        refresh(&mut view, 60, 6);
        assert_eq!(view.state.body_scroll(), 1, "one line, not a jump");
        view.line_up();
        refresh(&mut view, 60, 6);
        assert_eq!(view.state.body_scroll(), 0);
    }

    /// Once the actions are on screen, Down walks them — and walking off the
    /// last one in view scrolls instead of dragging the cursor out of sight.
    #[test]
    fn down_walks_the_actions_that_are_in_view() {
        let mut view = HelpView::new(Lang::En, Vec::new());
        view.state.open(&TopicId::new("copying"));
        view.state.toggle_focus();
        view.state.bottom();
        refresh(&mut view, 60, 6);
        let first = view.state.action_cursor();
        view.line_down();
        refresh(&mut view, 60, 6);
        assert_eq!(view.state.action_cursor(), first + 1, "the next action");
    }

    /// `]` puts the NEXT section at the top and `[` the previous one; past
    /// the last heading, `]` goes to the end, and before the first one `[`
    /// goes to the start — a key that does nothing reads as broken.
    #[test]
    fn section_jumps_go_from_heading_to_heading() {
        let mut view = HelpView::new(Lang::Es, Vec::new());
        view.state.open(&TopicId::new("panes"));
        refresh(&mut view, 60, 8);
        let headings = view.heading_lines().to_vec();
        assert!(headings.len() >= 3, "panes has sections: {headings:?}");

        view.section_next();
        refresh(&mut view, 60, 8);
        assert_eq!(view.state.body_scroll(), headings[0]);
        view.section_next();
        refresh(&mut view, 60, 8);
        assert_eq!(view.state.body_scroll(), headings[1]);
        view.section_prev();
        refresh(&mut view, 60, 8);
        assert_eq!(view.state.body_scroll(), headings[0]);
        view.section_prev();
        refresh(&mut view, 60, 8);
        assert_eq!(
            view.state.body_scroll(),
            0,
            "before the first one, to the start"
        );

        for _ in 0..headings.len() + 2 {
            view.section_next();
            refresh(&mut view, 60, 8);
        }
        let total = view.body().0.len();
        assert_eq!(
            view.state.body_scroll(),
            total - 8,
            "after the last one, to the end"
        );
    }

    /// A page is the WINDOW, less two lines of overlap, not a fixed ten.
    #[test]
    fn a_page_is_the_window_less_its_overlap() {
        let mut view = HelpView::new(Lang::En, Vec::new());
        view.state.open(&TopicId::new("copying"));
        refresh(&mut view, 60, 20);
        assert_eq!(view.page(), 18);
        refresh(&mut view, 60, 2);
        assert_eq!(view.page(), 1, "never zero: a key that does nothing");
    }

    /// `End` lands on the last FULL screen, not on a lone last line.
    #[test]
    fn bottom_fills_the_last_screen() {
        let mut view = HelpView::new(Lang::En, Vec::new());
        view.state.open(&TopicId::new("copying"));
        view.state.toggle_focus();
        view.state.bottom();
        refresh(&mut view, 60, 6);
        let total = view.body().0.len();
        assert_eq!(view.state.body_scroll(), total - 6);
    }

    /// The keyboard page, the longest, can now be read: the focus reaches its
    /// body and the arrows scroll it.
    #[test]
    fn the_keys_page_scrolls_with_the_arrows() {
        let keys: Vec<ratatui::text::Line<'static>> = (0..40)
            .map(|i| ratatui::text::Line::raw(format!("fila {i}")))
            .collect();
        let mut view = HelpView::new(Lang::En, keys);
        view.state.open(&TopicId::new(KEYS_ID));
        view.state.toggle_focus();
        assert_eq!(view.state.focus(), Focus::Body);
        refresh(&mut view, 60, 10);
        view.line_down();
        refresh(&mut view, 60, 10);
        assert_eq!(view.state.body_scroll(), 1);
    }

    #[test]
    fn the_keys_page_paints_keys_lines_and_maps_no_action() {
        let mut view = HelpView::new(
            Lang::En,
            vec![
                ratatui::text::Line::raw("── Browsing ──"),
                ratatui::text::Line::raw("  f5   copy"),
            ],
        );
        view.state.open(&TopicId::new(KEYS_ID));
        assert!(view.on_keys_page());
        refresh(&mut view, 60, 10);
        let (lines, action_lines) = view.body();
        assert_eq!(lines.len(), 2, "one painted line per generated line");
        assert!(flatten(lines).contains("f5   copy"), "{:?}", flatten(lines));
        assert!(
            action_lines.is_empty(),
            "its rows are chords, and a chord is not something Enter runs"
        );

        // And going back to a corpus topic restores a real action map: the
        // empty one above is the KEYS page, not a renderer that lost it.
        view.state.open(&TopicId::new("copying"));
        refresh(&mut view, 60, 10);
        assert!(!view.on_keys_page());
        assert!(!view.body().1.is_empty());
    }

    /// A plugin from the catalogue, in the shape that arrives over the wire.
    fn plugin(id: &str, name: &str) -> norte_proto::methods::PluginInfo {
        norte_proto::methods::PluginInfo {
            id: id.to_owned(),
            name: name.to_owned(),
            publisher: "ACME".to_owned(),
            version: "1.0.0".to_owned(),
            category: "command".to_owned(),
            capabilities: Vec::new(),
            approved: true,
            enabled: true,
            description: None,
            commands: Vec::new(),
            columns: Vec::new(),
            panels: Vec::new(),
            has_help: true,
            manifest_digest: None,
        }
    }

    /// H3e: a plugin's page that hasn't arrived YET paints EMPTY, never like
    /// the keyboard page. Both are "no corpus topic" for `HelpState::topic`,
    /// and without the distinction the whole cheatsheet would show up under
    /// an extension's name, reading as its documentation.
    #[test]
    fn an_in_flight_plugin_page_comes_out_empty_and_is_not_the_keyboard() {
        let mut view = HelpView::new(Lang::En, vec![ratatui::text::Line::raw("  f5   copy")]);
        view.set_plugins(&[plugin("acme.ftp", "FTP")]);
        view.state.open(&TopicId::new("acme.ftp"));
        assert!(!view.on_keys_page(), "a plugin page isn't the keyboard one");
        refresh(&mut view, 60, 10);
        let (lines, action_lines) = view.body();
        assert!(lines.is_empty(), "empty body while it arrives: {lines:?}");
        assert!(action_lines.is_empty());

        // And once it arrives, it paints — with its origin badge.
        let parsed = norte_help::parse_untrusted(
            b"+++\nid = \"acme.ftp\"\ntitle = \"FTP\"\n+++\nthe plugin's body",
            "acme.ftp",
            view.publisher_of("acme.ftp"),
        )
        .fold_flags(true, false);
        view.state.install_plugin_topic(parsed.topic);
        refresh(&mut view, 60, 10);
        let painted = flatten(view.body().0);
        assert!(painted.contains("the plugin's body"), "{painted}");
        assert!(
            painted.contains("ACME"),
            "the publisher comes along: {painted}"
        );
        assert!(
            painted.contains(&norte_i18n::t_in(Lang::En, "help-plugin-truncated")),
            "and the truncation badge: {painted}"
        );
    }

    /// Third-party text is masked and bounded at the ENTRY POINT:
    /// `PluginNode::title` promises to arrive safe and the model masks
    /// nothing — it just filters whatever it's given.
    #[test]
    fn plugin_name_arrives_masked_and_bounded() {
        let mut view = HelpView::new(Lang::En, Vec::new());
        let mut p = plugin("acme.ftp", &format!("a\u{202E}{}", "x".repeat(5_000)));
        p.publisher = "AC\u{202E}ME".to_owned();
        view.set_plugins(&[p]);
        let row = view
            .state
            .rows()
            .iter()
            .find_map(|r| match r {
                norte_frontend::help::SidebarRow::Topic { id, title }
                    if id.as_str() == "acme.ftp" =>
                {
                    Some(title.clone())
                }
                _ => None,
            })
            .expect("the node is on the sidebar");
        assert!(!row.contains('\u{202E}'), "no raw bidi: {row:?}");
        assert!(
            row.chars().count() <= crate::app::PLUGIN_NAME_WIRE_CAP + 1,
            "capped: {} chars",
            row.chars().count()
        );
        assert!(
            row.ends_with('…'),
            "and the truncation gets MARKED, as the neighbors that do this \
             same thing mark it: presenting a cut name as complete is the lie \
             this phase went to hunt down: {row:?}"
        );
        let pub_ = view.publisher_of("acme.ftp").expect("there is a publisher");
        assert!(!pub_.contains('\u{202E}'), "clean publisher: {pub_:?}");
    }

    /// H3e: a BLANK `name` falls back to the plugin's id.
    ///
    /// `name` is required in the manifest but nobody checks it has content,
    /// and U+3164 (HANGUL FILLER) isn't whitespace: it survives `trim` and
    /// masking. Without the fallback, the sidebar paints an EMPTY row under
    /// the "Extensions" header — a page the reader can move onto, open and
    /// read, attached to a name that says nothing.
    #[test]
    fn a_blank_name_falls_back_to_the_plugin_id() {
        let mut view = HelpView::new(Lang::En, Vec::new());
        let mut p = plugin("acme.ftp", "\u{3164}\u{3164}");
        p.publisher = "\u{3164}".to_owned();
        view.set_plugins(&[p]);
        let row = view
            .state
            .rows()
            .iter()
            .find_map(|r| match r {
                norte_frontend::help::SidebarRow::Topic { id, title }
                    if id.as_str() == "acme.ftp" =>
                {
                    Some(title.clone())
                }
                _ => None,
            })
            .expect("the node is on the sidebar");
        assert_eq!(row, "acme.ftp", "the row is named with the id: {row:?}");

        // And a blank publisher isn't attributed: the badge would paint
        // "published by " with nothing behind it, which reads as a
        // rendering fault and not as an absence.
        assert_eq!(view.publisher_of("acme.ftp"), None);

        // Anti-emptiness: a REAL name isn't touched.
        let mut view = HelpView::new(Lang::En, Vec::new());
        view.set_plugins(&[plugin("acme.ftp", "FTP")]);
        assert!(view.state.rows().iter().any(|r| matches!(
            r,
            norte_frontend::help::SidebarRow::Topic { title, .. } if title == "FTP"
        )));
        assert_eq!(view.publisher_of("acme.ftp").as_deref(), Some("ACME"));
    }

    /// H3e: an id that is NOT a valid plugin id gets DISCARDED at the entry
    /// point — never repaired.
    ///
    /// The id arrives over the wire and is a KEY: it travels to `TopicId`,
    /// to `plugin_needs_fetch` and back out as `plugin.help`'s argument.
    /// Masking it wouldn't be a security measure (masking isn't injective:
    /// two different plugins would land on the same row) and a "repaired"
    /// id would be a key that resolves to nothing, or worse, to something
    /// else. Refusing it is the only answer that can't lie. Same criterion
    /// as `parse_untrusted`'s command keys, which get rejected instead of
    /// rewritten.
    #[test]
    fn an_id_that_is_not_a_plugins_is_discarded_at_entry() {
        let mut view = HelpView::new(Lang::En, Vec::new());
        let mut bidi = plugin("acme.\u{202E}ftp", "Bidi");
        bidi.publisher = "ACME".to_owned();
        view.set_plugins(&[
            plugin("acme.ftp", "Bueno"),
            bidi,
            plugin("sinpunto", "Sin punto"),
            plugin(&"a.".repeat(500), "Mile-long"),
        ]);
        let ids: Vec<String> = view
            .state
            .rows()
            .iter()
            .filter_map(|r| match r {
                norte_frontend::help::SidebarRow::Topic { id, .. } => Some(id.as_str().to_owned()),
                norte_frontend::help::SidebarRow::Group { .. } => None,
            })
            // The sidebar carries the WHOLE corpus besides the extensions:
            // what's looked at here are the plugin rows, which are the ones
            // this filter decides.
            .filter(|id| {
                id != norte_frontend::help::KEYS_ID && norte_help::topic(Lang::En, id).is_none()
            })
            .collect();
        assert_eq!(
            ids,
            vec!["acme.ftp".to_owned()],
            "only the valid id survives: {ids:?}"
        );
        // And no attribution is left hanging off the one that got dropped.
        assert_eq!(view.publisher_of("acme.\u{202E}ftp"), None);
    }

    /// A name of invisibles that are HAZARDS (not `INVISIBLE`) also counts
    /// as blank — because it's asked AFTER masking, when they're already
    /// `U+FFFD`. It's the order that lets one question cover both families.
    #[test]
    fn a_name_of_zero_width_spaces_also_falls_back_to_the_id() {
        let mut view = HelpView::new(Lang::En, Vec::new());
        view.set_plugins(&[plugin("acme.ftp", "\u{200B}\u{200B}")]);
        assert!(view.state.rows().iter().any(|r| matches!(
            r,
            norte_frontend::help::SidebarRow::Topic { title, .. } if title == "acme.ftp"
        )));
    }

    /// `plugin_needs_fetch` ASKS, it doesn't notify: it keeps answering
    /// `Some` until the page is installed, and the run loop visits it every
    /// turn. Without the claim, a daemon that doesn't answer would get
    /// retried at frame rate.
    #[test]
    fn the_page_is_requested_only_once_per_overlay() {
        let mut view = HelpView::new(Lang::En, Vec::new());
        view.set_plugins(&[plugin("acme.ftp", "FTP")]);
        view.state.open(&TopicId::new("acme.ftp"));
        assert_eq!(view.claim_plugin_fetch().as_deref(), Some("acme.ftp"));
        for _ in 0..100 {
            assert_eq!(
                view.claim_plugin_fetch(),
                None,
                "a failure isn't retried within the same overlay"
            );
            assert_eq!(
                view.state.plugin_needs_fetch(),
                Some("acme.ftp"),
                "and the model keeps saying it's missing: it's the claim that \
                 cuts the loop, not the model"
            );
        }
        // Closing and reopening the help DOES ask again: it's the only
        // retry the reader has, and the only one they can ask for.
        let mut other = HelpView::new(Lang::En, Vec::new());
        other.set_plugins(&[plugin("acme.ftp", "FTP")]);
        other.state.open(&TopicId::new("acme.ftp"));
        assert_eq!(other.claim_plugin_fetch().as_deref(), Some("acme.ftp"));
    }

    /// A corpus page requests nothing, and neither does the keyboard one:
    /// requesting for them would be a call to the daemon every frame for
    /// the whole reading session.
    #[test]
    fn a_corpus_page_asks_for_nothing() {
        let mut view = HelpView::new(Lang::En, Vec::new());
        view.set_plugins(&[plugin("acme.ftp", "FTP")]);
        assert_eq!(
            view.claim_plugin_fetch(),
            None,
            "the index requests nothing"
        );
        view.state.open(&TopicId::new(KEYS_ID));
        assert_eq!(view.claim_plugin_fetch(), None, "neither does the keyboard");
    }
}

#[cfg(test)]
mod help_plugin_snapshot_tests {
    use crate::app::App;
    use norte_help::ChordResolver;
    use norte_vfs::VPath;

    fn app() -> App {
        let d = VPath::parse("file:///x").expect("test wire");
        App::new(
            crate::app::Pane::new(d.clone(), Vec::new()),
            crate::app::Pane::new(d, Vec::new()),
        )
    }

    fn plugin(id: &str, approved: bool, enabled: bool) -> norte_proto::methods::PluginInfo {
        norte_proto::methods::PluginInfo {
            id: id.to_owned(),
            name: id.to_owned(),
            publisher: "ACME".to_owned(),
            version: "1.0.0".to_owned(),
            category: "command".to_owned(),
            capabilities: Vec::new(),
            approved,
            enabled,
            description: None,
            commands: Vec::new(),
            columns: Vec::new(),
            panels: Vec::new(),
            has_help: true,
            manifest_digest: None,
        }
    }

    /// The SAME `help.md` in the test's two cases below: it declares the
    /// command in its front matter (the row) and cites it in the prose (the
    /// inline mark). Notice what it does NOT carry: a title. A topic's
    /// header has nowhere to put it — `parse_untrusted` only keeps dispatch
    /// keys — so the name can only come from the manifest.
    const PAGE: &[u8] = b"+++\nid = \"org.norte.demo\"\ntitle = \"Demo\"\n\
                            commands = [\"plugin:org.norte.demo:greet\"]\n+++\n\
                            Its own mark: {{cmd:plugin:org.norte.demo:greet}}";

    /// A command's name comes from the SNAPSHOT (the manifest), never from
    /// `help.md`. The plugin writes both, so only one can win, and it has
    /// to be the one the human approving the plugin sees: the extensions
    /// manager, the palette and the approval request show the manifest's,
    /// and a page calling `greet` something else would leave the reader not
    /// knowing what they're approving.
    ///
    /// Demonstrated by changing the manifest with the SAME page bytes: if
    /// the painted text follows the manifest, the page isn't the source.
    #[test]
    fn a_commands_name_comes_from_the_snapshot_not_the_page() {
        let painted_with = |title: &str| -> String {
            let mut app = app();
            app.help = Some(crate::app::HelpView::new(norte_help::Lang::En, Vec::new()));
            let mut p = plugin("org.norte.demo", true, true);
            p.commands = vec![norte_proto::methods::PluginCommandInfo {
                id: "greet".to_owned(),
                title: title.to_owned(),
                kind: norte_proto::methods::PluginCommandKind::Command,
            }];
            app.freeze_help_plugins(&[p]);
            let help = app.help.as_mut().expect("open");
            help.state.open(&norte_help::TopicId::new("org.norte.demo"));
            let parsed = norte_help::parse_untrusted(PAGE, "org.norte.demo", None);
            help.state.install_plugin_topic(parsed.topic);
            app.refresh_help(70, 20);
            let (lines, _) = app.help.as_ref().expect("open").body();
            lines
                .iter()
                .map(|l| {
                    l.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        let text = painted_with("Greet the world");
        assert!(
            text.contains("Greet the world"),
            "the row and the mark carry the manifest's name: {text}"
        );
        assert!(
            !text.contains("plugin:org.norte.demo:greet"),
            "and NOT its dispatch key, neither in the prose nor in the row: {text}"
        );
        // Twice: once in the prose (the inline mark) and once in the
        // runnable-rows table. `render_command` and `rows_of` share
        // `label_or_id` exactly so they can't disagree.
        assert_eq!(text.matches("Greet the world").count(), 2, "{text}");

        // Same page bytes, different manifest: the manifest wins.
        let other = painted_with("Saludar al mundo");
        assert!(other.contains("Saludar al mundo"), "{other}");
        assert!(!other.contains("Greet the world"), "{other}");
    }

    /// H3e: the snapshot freezes BOTH halves at once — the sidebar offers
    /// every plugin's page with `help.md`, and the resolver dims the
    /// commands of the ones that aren't approved-and-active. If only one
    /// took, the reader would read a page whose rows promise what the app
    /// is going to refuse.
    #[test]
    fn the_snapshot_reaches_the_bar_and_the_resolver() {
        let mut app = app();
        app.help = Some(crate::app::HelpView::new(norte_help::Lang::En, Vec::new()));
        app.freeze_help_plugins(&[
            plugin("acme.ftp", true, true),
            plugin("otro.off", true, false),
        ]);

        let help = app.help.as_ref().expect("the help is open");
        let ids: Vec<String> = help
            .state
            .rows()
            .iter()
            .filter_map(|r| match r {
                norte_frontend::help::SidebarRow::Topic { id, .. } => Some(id.as_str().to_owned()),
                norte_frontend::help::SidebarRow::Group { .. } => None,
            })
            .collect();
        assert!(ids.iter().any(|i| i == "acme.ftp"), "{ids:?}");
        assert!(
            ids.iter().any(|i| i == "otro.off"),
            "a disabled plugin KEEPS its page — reading it is how you decide \
             whether to enable it: {ids:?}"
        );

        assert!(
            app.help_chords
                .availability("plugin:acme.ftp:sync")
                .is_available()
        );
        assert_eq!(
            app.help_chords
                .availability("plugin:otro.off:sync")
                .reason(),
            Some(norte_help::Reason::PluginInactive),
            "but its rows aren't offered"
        );
    }

    /// The same gate, in the RESOLVER's half: neither the active set nor the
    /// title map can hold an id the host shouldn't have announced.
    ///
    /// The id comes from the canonical corpus (`plugin_id_bidi_segment`) and
    /// not from a literal: the GUI tests its half of this same gate against
    /// the same fixture, and two frontends with their own spelling of the
    /// adversary is exactly the drift the corpus exists to not have.
    #[test]
    fn an_invalid_id_does_not_enter_the_resolvers_snapshot() {
        let fixture = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == "plugin_id_bidi_segment")
            .expect("the fixture lives in the canonical corpus");
        let id = String::from_utf8(fixture.bytes).expect("the fixture is UTF-8");
        let key = format!("plugin:{id}:sync");
        let mut app = app();
        app.help = Some(crate::app::HelpView::new(norte_help::Lang::En, Vec::new()));
        let mut bad = plugin(&id, true, true);
        bad.commands = vec![norte_proto::methods::PluginCommandInfo {
            id: "sync".to_owned(),
            title: "Sync".to_owned(),
            kind: norte_proto::methods::PluginCommandKind::Command,
        }];
        app.freeze_help_plugins(&[bad]);
        assert_eq!(
            norte_help::ChordResolver::availability(&*app.help_chords, &key).reason(),
            Some(norte_help::Reason::PluginInactive),
            "it isn't active: its id never entered the set"
        );
        assert_eq!(
            norte_help::ChordResolver::label(&*app.help_chords, &key)
                .chars()
                .filter(|c| norte_encoding::is_terminal_hazard(*c))
                .count(),
            0,
            "and its title never reached the map: the label falls back to the \
             safe default"
        );
    }

    /// The re-freeze of facts the refresh funnel does with the help open
    /// (`main::after_panes_refresh`) must NOT turn off plugin rows mid-read,
    /// nor the other way around.
    #[test]
    fn refreezing_the_facts_does_not_lose_the_plugins_snapshot() {
        let mut app = app();
        app.help = Some(crate::app::HelpView::new(norte_help::Lang::En, Vec::new()));
        app.freeze_help_plugins(&[plugin("acme.ftp", true, true)]);
        app.freeze_help_facts();
        assert!(
            app.help_chords
                .availability("plugin:acme.ftp:sync")
                .is_available()
        );
    }
}

#[cfg(test)]
mod which_key_tests {
    use crate::app::{App, Pane};
    use norte_frontend::keymap::{
        Availability, Effective, Resolution, Resolver, Screen, parse_chord, parse_keymap,
    };
    use norte_i18n::Lang;
    use norte_proto::Scheme;
    use norte_proto::VPath;

    /// Counts ON, one `g` prefix with an available branch, an unavailable one
    /// and a deeper branch.
    ///
    /// The unavailable one used to be `pane.pack`, `Planned` under #132. That
    /// issue is built, and with it the catalogue ran out of `Planned` entries
    /// altogether — every command a preset names is now a command norte has.
    /// So the dimmed row this test needs is the OTHER unavailable: a live
    /// command this build does not implement, which is what a GUI-only
    /// binding looks like from the terminal.
    fn resolver() -> Resolver {
        let src = r#"
counts = true

[pane]
keymap = [
    { on = ["g", "g"], run = "cursor.top" },
    { on = ["g", "p"], run = "pane.pack" },
    { on = ["j"], run = "cursor.down" },
]
"#;
        let preset = parse_keymap(src).expect("fixture parses");
        let known = ["cursor.top", "cursor.down"];
        let eff =
            Effective::build_for(&preset, &[], &known, Screen::Browse).expect("fixture builds");
        Resolver::new(eff)
    }

    fn app() -> App {
        let root = VPath::root(Scheme::new("mem").expect("scheme"), None);
        App::new(
            Pane::new(root.clone(), Vec::new()),
            Pane::new(root, Vec::new()),
        )
    }

    fn push(app: &mut App, r: &mut Resolver, key: &str) -> Resolution {
        let res = r.push(parse_chord(key).expect("chord"));
        match res {
            Resolution::Pending(_) | Resolution::Counting(_) => app.show_pending(r, Lang::En),
            _ => app.clear_pending(),
        }
        res
    }

    /// The pending arm OPENS it — on the keystroke itself, with no delay of
    /// any kind (ADR 0006) — and the `Run` that ends the sequence closes it.
    #[test]
    fn a_pending_prefix_opens_the_panel_and_a_run_closes_it() {
        let (mut app, mut r) = (app(), resolver());
        assert!(matches!(
            push(&mut app, &mut r, "g"),
            Resolution::Pending(1)
        ));
        let wk = app.which_key.as_ref().expect("the panel is open");
        assert_eq!(wk.title, "g");
        let chords: Vec<&str> = wk.rows.iter().map(|row| row.chord.as_str()).collect();
        assert_eq!(chords, vec!["g", "p"], "{chords:?}");

        assert!(matches!(
            push(&mut app, &mut r, "g"),
            Resolution::Run { .. }
        ));
        assert!(
            app.which_key.is_none(),
            "the sequence ended: so does the panel"
        );
        assert!(app.pending.is_empty(), "and the bar segment goes with it");
    }

    /// A BARE count does not open it: the continuation of a count is any key
    /// at all, so the panel would be the whole keymap. The bar still paints
    /// the number (K2a) — a count that cannot be seen cannot be cancelled.
    #[test]
    fn a_bare_count_does_not_open_the_panel() {
        let (mut app, mut r) = (app(), resolver());
        assert!(matches!(
            push(&mut app, &mut r, "1"),
            Resolution::Counting(1)
        ));
        assert!(matches!(
            push(&mut app, &mut r, "2"),
            Resolution::Counting(12)
        ));
        assert!(app.which_key.is_none(), "a bare count has no panel");
        assert_eq!(app.pending, "12", "but the bar shows it");
    }

    /// A count BEHIND a prefix is the state a reader most often cannot
    /// explain, so the panel that opens says the number is still in flight.
    #[test]
    fn a_count_behind_a_prefix_shows_in_the_title() {
        let (mut app, mut r) = (app(), resolver());
        push(&mut app, &mut r, "1");
        push(&mut app, &mut r, "2");
        push(&mut app, &mut r, "g");
        let wk = app.which_key.as_ref().expect("the panel is open");
        assert_eq!(wk.title, "12 g");
    }

    /// An unavailable continuation is a ROW, dimmed and explained — hiding it
    /// would recreate the silence K1 removed.
    #[test]
    fn the_rows_include_an_unavailable_binding_with_its_reason() {
        let (mut app, mut r) = (app(), resolver());
        push(&mut app, &mut r, "g");
        let wk = app.which_key.as_ref().expect("the panel is open");
        let p = wk
            .rows
            .iter()
            .find(|row| row.chord == "p")
            .expect("the row of the command this build does not run");
        assert!(matches!(p.avail, Availability::NotHere), "{:?}", p.avail);
        assert!(!p.reason.is_empty(), "and it says why: {:?}", p.reason);
    }

    /// `Esc` cancels a sequence, and every other end of the pending state
    /// closes the panel with the bar segment: the two are written by the same
    /// pair of methods precisely so they cannot disagree.
    #[test]
    fn esc_and_a_miss_close_it_too() {
        let (mut app, mut r) = (app(), resolver());
        push(&mut app, &mut r, "g");
        assert!(matches!(push(&mut app, &mut r, "esc"), Resolution::Reset));
        assert!(app.which_key.is_none(), "Esc cancelled the sequence");

        push(&mut app, &mut r, "g");
        // A chord that continues nothing is a miss: same treatment.
        assert!(matches!(push(&mut app, &mut r, "z"), Resolution::Reset));
        assert!(app.which_key.is_none());

        // And a key the frontend does not model at all reaches neither arm:
        // the run loop resets the resolver and clears both by hand.
        push(&mut app, &mut r, "g");
        r.reset();
        app.clear_pending();
        assert!(app.which_key.is_none());
        assert!(app.pending.is_empty());
    }
}
