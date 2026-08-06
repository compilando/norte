//! GUI help overlay (H3f, `F1`): the GUI twin of the TUI's `help` +
//! `app::HelpView`.
//!
//! Same split as `palette_view`/`settings_view`: this file is PURE — resolver,
//! view state, keyboard routing — and `main.rs` owns the async `Backend` calls
//! and the painting. The model itself (`norte_frontend::help::HelpState`) is
//! shared with the TUI; what a frontend has to supply is the answer to the
//! three questions `norte-help` asks — the reader's chord, a short label, and
//! whether the command can run right now — which is [`GuiChords`].

use std::collections::{BTreeSet, HashMap};

use norte_frontend::availability::Facts;
use norte_frontend::keymap::{Effective, paint_chord};
use norte_help::{Availability, ChordResolver};

/// The facts of a context with nothing in the way: what [`GuiChords`] answers
/// against until the overlay opens and freezes the real ones.
///
/// Everything permissive on purpose. Not `Default`, because "all false" is what
/// a derive would give and that is the OPPOSITE of permissive here —
/// `enterable: false` alone dims `nav.enter` on every page painted through a
/// resolver nobody had frozen yet.
pub const NO_IMPEDIMENT: Facts = Facts {
    enterable: true,
    viewable: true,
    rename_single: true,
    source_read_only: false,
    dest_read_only: false,
    degraded: false,
};

/// Cap, in CHARS, on a string a plugin chose before it is painted — the GUI
/// twin of the TUI's `app::plugin_label`, and deliberately the SAME number
/// (`PLUGIN_NAME_WIRE_CAP`, itself a mirror of the manifest's
/// `COMMAND_TITLE_MAX_CHARS`). A legitimate 100-char command title complete in
/// one frontend and elided in the other is one product describing one command
/// two ways.
///
/// `mask_terminal_hazards` neutralises hazards and does NOT cap length, and a
/// kilometric name is its own denial of service against a one-line row. Note
/// what this is NOT: a bound on painted WIDTH. Sixty-four full-width glyphs
/// pass it and occupy twice the columns of Latin — see `help_render`'s badge.
const PLUGIN_TEXT_CAP: usize = 120;

/// What a plugin-contributed dispatch key starts with.
///
/// The LOOSE question — "does this claim to be a plugin's, and therefore carry
/// text nobody in this process wrote?" — unlike
/// `norte_frontend::availability::plugin_of_command`, which stays the authority
/// on whether such a key is WELL FORMED.
const PLUGIN_KEY_PREFIX: &str = "plugin:";

/// The GUI's answer to the three questions `norte-help` asks a frontend: the
/// reader's effective chord, a short label, and whether the command can run
/// now.
///
/// **Rebuild it wherever the effectives are rebuilt** (startup and keymap
/// reload): a rebind that does not reach this resolver is a help page teaching
/// the OLD key.
#[derive(Debug, Clone)]
pub struct GuiChords {
    /// Command to its painted chord, filled browse then viewer.
    ///
    /// Precomputed rather than resolved per row: `Effective::bindings`
    /// materialises the WHOLE keymap — every chord formatted into a fresh
    /// `String` — and asking it once per rendered mark, per frame, is the cost
    /// this map pays once.
    ///
    /// First writer wins, so a command bound in both screens keeps its browse
    /// chord (the same precedence a browse-first or-chain would have had), and
    /// within a screen it keeps the binding that actually FIRES.
    chords: HashMap<String, String>,
    /// The language `label` answers in.
    lang: norte_i18n::Lang,
    /// The context `availability` answers against, FROZEN when the overlay
    /// opened.
    ///
    /// Frozen and not read live, the same call the GUI's context menu already
    /// makes: the reader walks a page whose rows were dimmed under one set of
    /// facts, and a row that changed verdict halfway down would make the page
    /// disagree with itself.
    facts: Facts,
    /// Plugin ids that are approved AND enabled, snapshotted when the overlay
    /// opened.
    ///
    /// Empty dims every plugin row, which is the right answer both when there
    /// are no plugins and when the snapshot could not be taken: offering a row
    /// `plugin.run_command` would refuse is the worse mistake.
    ///
    /// Note the asymmetry with [`Self::facts`], which defaults to fail-OPEN. It
    /// is not an inconsistency: the facts table is a list of known IMPEDIMENTS,
    /// so "I have not looked" means "no impediment known", while a plugin set
    /// is an ALLOWLIST and "I have not looked" means "I cannot vouch for any of
    /// them".
    active_plugins: BTreeSet<String>,
    /// Dispatch key (`plugin:{plugin_id}:{command_id}`) to the title the
    /// MANIFEST gives that command, snapshotted with [`Self::active_plugins`]
    /// from the same `plugin.list`.
    ///
    /// The titles come from the SNAPSHOT and never from the `help.md`. A plugin
    /// authors both, so only one of them can be the answer, and it has to be
    /// the one the extension manager shows the human who approves the plugin —
    /// otherwise a page could call a command one thing while the manager, the
    /// palette and the approval prompt call it another.
    ///
    /// Values are already masked and capped ([`plugin_text`]): the resolver
    /// hands strings straight to a painter.
    plugin_labels: HashMap<String, String>,
}

impl GuiChords {
    /// Builds from the GUI's two effective keymaps — it has no `Dialog` screen,
    /// unlike the TUI — and the language `label` answers in.
    #[must_use]
    pub fn new(browse: &Effective, viewer: &Effective, lang: norte_i18n::Lang) -> Self {
        let mut chords: HashMap<String, String> = HashMap::new();
        for eff in [browse, viewer] {
            for (seq, cmd) in eff.bindings() {
                chords
                    .entry(cmd.to_owned())
                    .or_insert_with(|| paint_chord(&seq));
            }
        }
        Self {
            chords,
            lang,
            facts: NO_IMPEDIMENT,
            active_plugins: BTreeSet::new(),
            plugin_labels: HashMap::new(),
        }
    }

    /// The same resolver answering [`ChordResolver::availability`] against
    /// `facts`.
    ///
    /// Returns a NEW value instead of mutating: the open overlay holds the
    /// resolver it was laid out with, and the freeze happens by swapping a
    /// fresh one in. Nothing already painted can change underneath it.
    #[must_use]
    pub fn with_facts(&self, facts: Facts) -> Self {
        Self {
            facts,
            ..self.clone()
        }
    }

    /// The same resolver carrying ONE `plugin.list` photograph: which plugins
    /// are active, and what the manifest calls each of their commands.
    ///
    /// Both halves in one builder because they are one instant — a resolver
    /// holding a fresh active set beside stale titles would dim a row correctly
    /// while naming it wrong. The two builders compose in either order: each
    /// carries the other's fields over, so a re-freeze while the overlay is
    /// open cannot drop the snapshot and dim every plugin row halfway down a
    /// page, nor take their names away.
    /// The facts this resolver was frozen against.
    ///
    /// Exists so a re-freeze can CARRY THEM OVER rather than re-read them: the
    /// plugin catalogue answering is not a reason for the page's verdicts to
    /// change under the reader.
    #[must_use]
    pub fn facts(&self) -> Facts {
        self.facts
    }

    #[must_use]
    pub fn with_plugins(&self, active: BTreeSet<String>, titles: HashMap<String, String>) -> Self {
        Self {
            active_plugins: active,
            plugin_labels: titles,
            ..self.clone()
        }
    }
}

impl ChordResolver for GuiChords {
    /// The command's chord in the screen it belongs to, masked — see the chord
    /// map for why it is filled the way it is, and
    /// `norte_frontend::keymap::paint_chord` for why nothing raw leaves here.
    fn chord(&self, command: &str) -> Option<String> {
        self.chords.get(command).cloned()
    }

    /// The catalogue's short label, or an EMPTY string when it has no entry.
    ///
    /// Blank on a miss is the contract, not an accident: `norte_i18n::t_in`
    /// answers a missing message with the id itself, so returning it
    /// unconditionally would paint `help-cmd-…` at the reader and stop
    /// `norte_help::render_command`'s fallback chain from ever naming the
    /// command. The miss is detected by testing for that echo rather than for a
    /// proxy: the echo IS the failure mode, so this cannot drift out of
    /// agreement with it, and a message id can never be its own translation.
    ///
    /// A `plugin:{id}:{command}` key is answered from the SNAPSHOT instead: the
    /// app's Fluent catalogue cannot possibly hold an entry for a command a
    /// third party declared. The guard is `plugin_of_command` — STRUCTURAL
    /// rather than a promise about the caller, since this map arrives through a
    /// public builder and a built-in command's label must not be overridable by
    /// data that came off the wire even in principle.
    fn label(&self, command: &str) -> String {
        if norte_frontend::availability::plugin_of_command(command).is_some()
            && let Some(title) = self.plugin_labels.get(command)
        {
            return title.clone();
        }
        let id = crate::keymap::help_id(command);
        let text = norte_i18n::t_in(self.lang, &id);
        if text != id {
            return text;
        }
        if command.starts_with(PLUGIN_KEY_PREFIX) {
            // A `plugin:` key the snapshot does not name. Blank would be the
            // generic answer, and `norte_help::label_or_id` would then fall back
            // to the command id — the behaviour we want, except that a
            // `plugin:` id is THIRD-PARTY TEXT and that fallback paints it RAW.
            // Handing back the masked, capped spelling keeps the visible
            // behaviour (the row wears its key) and takes the hazard out of it.
            //
            // The PERMISSIVE prefix on purpose, unlike the strict
            // `plugin_of_command` above: that one asks "may this be answered
            // from the snapshot", an identity question, while this asks "is this
            // third-party text", and anything merely CLAIMING to be a plugin key
            // must be handled as though it were.
            return plugin_text(command);
        }
        String::new()
    }

    /// Whether the command can run in the context the overlay was opened in,
    /// answered by the ONE shared table
    /// (`norte_frontend::availability::verdict_with_plugins`) so a dimmed help
    /// row and a greyed-out context-menu entry can never disagree.
    ///
    /// Routed through `verdict_with_plugins` and never through plain `verdict`:
    /// the latter answers a `plugin:` key through its fail-OPEN wildcard,
    /// lighting every such row unconditionally, and a plugin's own page lists
    /// exactly those rows.
    fn availability(&self, command: &str) -> Availability {
        norte_frontend::availability::verdict_with_plugins(
            command,
            &self.facts,
            &self.active_plugins,
        )
    }
}

/// The open help overlay: the shared model plus what only this frontend knows.
#[derive(Debug)]
pub struct HelpView {
    /// Sidebar, body scroll, filter, history and focus — shared with the TUI.
    pub state: norte_frontend::help::HelpState,
    /// Body of the synthetic keyboard page ([`keys_lines`]).
    pub keys_lines: Vec<String>,
    /// Plugin ids whose page has already been ASKED FOR in this overlay.
    ///
    /// `HelpState::plugin_needs_fetch` is a POLLING question, not an event: it
    /// keeps answering `Some(id)` until the page is installed, so a caller that
    /// visits it after every key would re-issue the request forever against a
    /// daemon that cannot answer. Claiming BEFORE the request, rather than
    /// after it succeeds, is the whole point: the case worth guarding is the
    /// one where the answer never comes.
    ///
    /// Lives on the VIEW, so its scope is the open overlay: closing and
    /// reopening the help asks again, which is the only retry a reader has.
    asked: BTreeSet<String>,
    /// Publisher of each plugin of the snapshot, keyed by id, already masked
    /// and capped.
    ///
    /// `norte_help::parse_untrusted` takes the publisher as the attribution of
    /// the page it is about to build, and the page is parsed when its
    /// `plugin.help` answer arrives — long after the snapshot that knew it.
    publishers: HashMap<String, String>,
    /// Ids of the snapshot that are approved AND enabled.
    active: BTreeSet<String>,
    /// `plugin:{id}:{command}` to the manifest's title, from the same snapshot.
    titles: HashMap<String, String>,
    /// One line the overlay's own footer shows instead of the key hints —
    /// today, why the row under the cursor did nothing.
    ///
    /// It lives HERE and not in the app's window-level flash because the flash
    /// is painted before this overlay's scrim and would be covered by it. It is
    /// cleared by the next key, so it never outlives the question it answers.
    pub status: Option<String>,
}

impl HelpView {
    /// Opens the overlay on the index topic of `lang`.
    ///
    /// The label of the synthetic keyboard row is resolved HERE and handed to
    /// the model: `norte_frontend::help` has no Fluent access on purpose, and
    /// this is the frontend that names the page. Resolving it once, at the
    /// seam, keeps the sidebar and the filter looking at the same string.
    #[must_use]
    pub fn new(lang: norte_i18n::Lang, keys_lines: Vec<String>) -> Self {
        Self {
            state: norte_frontend::help::HelpState::new(lang, norte_i18n::t("help-topic-keys")),
            keys_lines,
            asked: BTreeSet::new(),
            publishers: HashMap::new(),
            active: BTreeSet::new(),
            titles: HashMap::new(),
            status: None,
        }
    }

    /// Installs ONE `plugin.list` photograph: sidebar nodes, publishers, the
    /// active set and the command titles.
    ///
    /// All of it at once because they are one instant (see
    /// [`GuiChords::with_plugins`]). Every third-party string is masked and
    /// capped HERE, at the single point where a `PluginListResult` enters this
    /// view, so everything downstream is already safe to paint.
    pub fn set_plugins(&mut self, plugins: &[norte_proto::methods::PluginInfo]) {
        self.publishers.clear();
        self.active.clear();
        self.titles.clear();
        let mut nodes = Vec::with_capacity(plugins.len());
        for p in plugins {
            // The id is the one field that has to be RIGHT rather than merely
            // paintable: it becomes a `TopicId`, it is folded by the sidebar
            // filter on every keystroke, and it goes back out as the argument
            // of `plugin.help`. An announcement this process had no business
            // receiving is DROPPED, never rewritten — masking an id is not
            // injective, so two of them could collapse onto one row, and
            // `is_valid_plugin_id` is also the only thing bounding its length.
            if !norte_core::is_valid_plugin_id(&p.id) {
                continue;
            }
            let active = p.approved && p.enabled;
            if active {
                self.active.insert(p.id.clone());
            }
            // A blank publisher is NO attribution, not an empty one: the model
            // must not carry a claim the badge would then have to re-filter.
            let publisher = plugin_text(&p.publisher);
            if !norte_help::is_blank_id(&publisher) {
                self.publishers.insert(p.id.clone(), publisher);
            }
            for c in &p.commands {
                // A blank title is DROPPED so `norte_help::label_or_id` reaches
                // its own fallback. Storing it would satisfy the `get` in
                // `GuiChords::label` and return "", which that function treats
                // as "no label" — and the fallback then paints the RAW
                // `plugin:{id}:{command}` dispatch key, which is exactly what
                // the snapshot exists to prevent. The manifest bounds a title's
                // length and never its content, so this is reachable by an
                // honest plugin, not only a hostile one.
                let title = plugin_text(&c.title);
                if norte_help::is_blank_id(&title) {
                    continue;
                }
                self.titles
                    .insert(format!("plugin:{}:{}", p.id, c.id), title);
            }
            // A name that is blank AFTER masking — `""`, or U+3164 HANGUL
            // FILLER, which is neither a hazard nor whitespace — falls back to
            // the id. Without it the Extensions group grows a nameless row the
            // reader can arrow onto and open: a full third-party page with
            // nothing on screen attributing it to anyone, at the moment they
            // are deciding whether to approve that plugin (the H3e fix, which
            // this frontend was missing).
            let named = plugin_text(&p.name);
            nodes.push(norte_frontend::help::PluginNode {
                id: p.id.clone(),
                title: if norte_help::is_blank_id(&named) {
                    plugin_text(&p.id)
                } else {
                    named
                },
                has_help: p.has_help,
                active,
            });
        }
        self.state.set_plugins(nodes);
    }

    /// The plugin id whose page must be fetched NOW, claimed so the next call
    /// does not ask again. `None` when there is nothing to fetch or the open
    /// page has already been asked for.
    pub fn claim_plugin_fetch(&mut self) -> Option<String> {
        let id = self.state.plugin_needs_fetch()?.to_owned();
        self.asked.insert(id.clone()).then_some(id)
    }

    /// Parses a `plugin.help` answer in hostile mode and installs it as the
    /// page of `id`.
    ///
    /// The publisher comes from the SNAPSHOT, never from the page: a plugin
    /// does not get to say who published it. `fold_flags` is not optional
    /// either — the text arrives already short and already decoded, so this
    /// parse comes out clean and the badge, the whole user-facing mitigation
    /// for a hostile `help.md`, would go dark.
    pub fn install_plugin_page(&mut self, id: &str, res: &norte_proto::methods::PluginHelpResult) {
        let publisher = self.publishers.get(id).cloned();
        let parsed = norte_help::parse_untrusted(res.markdown.as_bytes(), id, publisher)
            .fold_flags(res.truncated, res.lossy);
        self.state.install_plugin_topic(parsed.topic);
    }

    /// The resolver this overlay paints through: `base` carrying the frozen
    /// `facts` and this view's plugin snapshot.
    #[must_use]
    pub fn freeze(&self, base: &GuiChords, facts: Facts) -> GuiChords {
        base.with_facts(facts)
            .with_plugins(self.active.clone(), self.titles.clone())
    }
}

/// Width in CHARS of the chord column of the keyboard cheatsheet.
///
/// The GUI paints that page as text lines (one element each), so unlike the
/// TUI's cell-correct column this is a plain char count: GPUI measures and
/// wraps the line itself, and a wide codepoint bound by a project keymap layer
/// shifts the label instead of pushing it off the pane.
const CHORD_COLUMN: usize = 14;

/// The body of the synthetic keyboard page: every binding of both screens with
/// its catalogue description, in real precedence order (what the key DOES, not
/// what the preset says).
///
/// GENERATED, never a maintained list — rebinding changes the sheet. The GUI
/// has no `Dialog` screen, so unlike the TUI's `help::build` there is no third
/// section.
#[must_use]
pub fn keys_lines(browse: &Effective, viewer: &Effective) -> Vec<String> {
    let mut out = Vec::new();
    for (title, eff) in [
        (norte_i18n::t("help-section-browse"), browse),
        (norte_i18n::t("help-section-viewer"), viewer),
    ] {
        out.push(String::new());
        out.push(format!("── {title} ──"));
        for (seq, cmd) in eff.bindings() {
            let seq = paint_chord(&seq);
            let pad = " ".repeat(CHORD_COLUMN.saturating_sub(seq.chars().count()));
            out.push(format!(
                "  {seq}{pad} {}",
                norte_i18n::t(&crate::keymap::help_id(cmd))
            ));
        }
    }
    out
}

/// What the caller (`main.rs`) must do after a key.
#[derive(Debug)]
pub enum HelpOutcome {
    /// Nothing beyond a repaint.
    None,
    /// Close the overlay.
    Close,
    /// Enter over a RUNNABLE row: dispatch this key — a built-in command id or
    /// a `plugin:{id}:{command}`, the same vocabulary the palette dispatches,
    /// down the same path. No second route, and therefore no bypass of policy
    /// or of plugin approval.
    Run(String),
    /// Enter over a DIMMED row: say why, run nothing. The help never offers
    /// what the app would refuse, and a row that failed silently on Enter
    /// would make the dimming decorative.
    Blocked(norte_help::Reason),
    /// `ctrl+p`: hand the current filter to the command palette.
    Palette(String),
}

/// Rows a page key moves in the sidebar, and lines it scrolls in the body.
///
/// A constant rather than the painted height: this module never sees a window,
/// and `main.rs` clamps the scroll against the real body length after the key.
const PAGE: usize = 10;

/// Keyboard routing (GPUI key names, hardcoded — the convention
/// `settings_view::on_key` and `palette_view::on_key` established for a GUI
/// overlay; unlike the TUI there is no `dialog` keymap to resolve through).
///
/// # `f1` closes even when `app.help` no longer opens with it
///
/// A known and unfixed asymmetry. H3f put `app.help` into the GUI's command
/// table, so the key that OPENS the help is now whatever the user bound it to,
/// while the key that closes it is spelled here. Rebind `app.help` to `f2` and
/// you get `f2` to open and both `f2` (nothing) and `f1` (close) afterwards.
/// The TUI resolves its close through the keymap — "what is hardcoded is the
/// meaning, not the key" — and this overlay should too; doing it needs the
/// resolver in here, which is the whole reason this module is pure. `Esc`
/// closes in every configuration, which is what the footer promises.
///
/// `chords` is the FROZEN resolver the open page was painted through
/// ([`HelpView::freeze`]): Enter has to be answered by the verdict the reader
/// can SEE, never by a fresher one that would run what the page shows dimmed.
#[must_use]
pub fn on_key(
    view: &mut HelpView,
    key: &str,
    key_char: Option<&str>,
    chords: &GuiChords,
) -> HelpOutcome {
    use norte_frontend::help::{Action, Focus};

    // The filter is a REGIME, not a flag. While it is open the keys that leave
    // it must leave it, and nothing else may be routed as if it were closed:
    // without this, a reader who typed `/cop`, opened the page and pressed `⇥`
    // was still filtering, so every letter meant for the body silently edited
    // the sidebar — which rebuilds the rows and can move the page out from
    // under them. `Esc` here closes the BOX and keeps the text (the search is
    // not undone); `Esc` outside it closes the overlay, which is what the
    // footer advertises.
    if view.state.filtering() {
        match key {
            "escape" | "enter" => {
                view.state.end_filter();
                return HelpOutcome::None;
            }
            "backspace" => {
                view.state.backspace();
                return HelpOutcome::None;
            }
            "up" => {
                view.state.up();
                return HelpOutcome::None;
            }
            "down" => {
                view.state.down();
                return HelpOutcome::None;
            }
            _ => {
                if let Some(c) = crate::keys::typed_char(key, key_char) {
                    view.state.push_char(c);
                }
                return HelpOutcome::None;
            }
        }
    }

    match key {
        "escape" | "f1" => return HelpOutcome::Close,
        "tab" => view.state.toggle_focus(),
        "up" => view.state.up(),
        "down" => view.state.down(),
        "pageup" => view.state.page_up(PAGE),
        "pagedown" => view.state.page_down(PAGE),
        "backspace" => {
            // The filter regime above already claimed this key while the box is
            // open, so here it always means the trail. `false` = already at the
            // root: nothing to do. Backspace on the first page is not a close
            // (that is `Esc`), and a reader put on a page by `F1` never had a
            // trail to walk back.
            let _ = view.state.back();
        }
        "enter" => {
            if view.state.focus() == Focus::Topics {
                view.state.open_selected();
                return HelpOutcome::None;
            }
            return match view.state.action().cloned() {
                Some(Action::Open(id)) => {
                    view.state.open(&id);
                    HelpOutcome::None
                }
                Some(Action::Run(cmd)) => match chords.availability(&cmd) {
                    Availability::Available => HelpOutcome::Run(cmd),
                    Availability::Unavailable { reason } => HelpOutcome::Blocked(reason),
                },
                None => HelpOutcome::None,
            };
        }
        _ => {
            // Outside the filter regime a typed char means one thing only:
            // `/` opens the box. Everything else is inert rather than swallowed
            // into a filter nobody opened.
            if crate::keys::typed_char(key, key_char) == Some('/') {
                view.state.start_filter();
            }
        }
    }
    HelpOutcome::None
}

/// `ctrl+p` from the help: the palette opens carrying the filter the reader
/// already typed.
///
/// Two views of one model at two densities, so crossing between them must not
/// cost a re-type. The RAW filter and not the display one: the palette masks
/// what it paints on its own, and handing it a masked query would make it
/// search for the replacement characters instead of for what the reader meant.
///
/// A free function rather than an arm of [`on_key`] because `main.rs` strips
/// `ctrl` before the pure router ever sees a key (the same modifier gate the
/// palette and the settings view sit behind).
#[must_use]
pub fn handoff(view: &HelpView) -> HelpOutcome {
    HelpOutcome::Palette(view.state.filter_raw().to_owned())
}

/// Masks and caps a string a plugin chose, so a painter can take it verbatim.
///
/// The cap is counted in CHARS before masking and the ellipsis is added after,
/// so the marker survives whatever the mask did to the text it replaced.
#[must_use]
pub fn plugin_text(raw: &str) -> String {
    let mut chars = raw.chars();
    let head: String = chars.by_ref().take(PLUGIN_TEXT_CAP).collect();
    let overflowed = chars.next().is_some();
    let mut out = norte_encoding::mask_terminal_hazards(&head);
    if overflowed {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn effectives() -> (Effective, Effective) {
        crate::keymap::build_effectives_preset_only("orthodox")
    }

    #[test]
    fn chord_resuelve_en_la_pantalla_del_comando() {
        let (browse, viewer) = effectives();
        let r = GuiChords::new(&browse, &viewer, norte_i18n::Lang::En);
        // A browse command wears its browse chord…
        assert_eq!(r.chord("pane.copy").as_deref(), Some("F5"));
        // …and a command nobody bound names nothing rather than inventing a key.
        assert_eq!(r.chord("no.such.command"), None);
    }

    #[test]
    fn label_cae_a_vacio_y_nunca_al_id_fluent() {
        let (browse, viewer) = effectives();
        let r = GuiChords::new(&browse, &viewer, norte_i18n::Lang::En);
        assert_eq!(
            r.label("app.quit"),
            norte_i18n::t_in(norte_i18n::Lang::En, "help-cmd-app-quit")
        );
        assert_eq!(
            r.label("no.such.command"),
            "",
            "a miss is BLANK, never the echoed id"
        );
    }

    #[test]
    fn availability_sin_congelar_no_atenua_nada() {
        let (browse, viewer) = effectives();
        let r = GuiChords::new(&browse, &viewer, norte_i18n::Lang::En);
        assert!(r.availability("pane.copy").is_available());
    }

    #[test]
    fn un_plugin_apagado_atenua_su_fila_y_conserva_su_titulo() {
        let (browse, viewer) = effectives();
        let key = "plugin:org.norte.demo:greet";
        let mut titles = HashMap::new();
        titles.insert(key.to_owned(), "Greet the world".to_owned());
        let r = GuiChords::new(&browse, &viewer, norte_i18n::Lang::En)
            .with_plugins(BTreeSet::new(), titles);
        assert_eq!(r.label(key), "Greet the world");
        assert!(
            !r.availability(key).is_available(),
            "an empty active set is an allowlist miss: fail-closed"
        );
        let r = r.with_plugins(
            BTreeSet::from(["org.norte.demo".to_owned()]),
            HashMap::new(),
        );
        assert!(r.availability(key).is_available());
    }

    #[test]
    fn una_clave_plugin_desconocida_se_pinta_enmascarada_y_acotada() {
        let (browse, viewer) = effectives();
        let r = GuiChords::new(&browse, &viewer, norte_i18n::Lang::En);
        let hostile = format!("plugin:evil\u{202e}id:{}", "x".repeat(200));
        let painted = r.label(&hostile);
        assert!(
            !painted.contains('\u{202e}'),
            "bidi override survived: {painted:?}"
        );
        assert!(
            painted.chars().count() <= PLUGIN_TEXT_CAP + 1,
            "no cap: {} chars",
            painted.chars().count()
        );
    }

    fn info(
        id: &str,
        has_help: bool,
        approved: bool,
        enabled: bool,
    ) -> norte_proto::methods::PluginInfo {
        norte_proto::methods::PluginInfo {
            id: id.into(),
            name: format!("Name of {id}"),
            publisher: "ACME".into(),
            version: "0.1.0".into(),
            category: "command".into(),
            capabilities: Vec::new(),
            approved,
            enabled,
            description: None,
            commands: vec![norte_proto::methods::PluginCommandInfo {
                id: "greet".into(),
                title: "Greet".into(),
            }],
            columns: Vec::new(),
            has_help,
        }
    }

    fn topic_ids(view: &HelpView) -> Vec<&str> {
        view.state
            .rows()
            .iter()
            .filter_map(|r| match r {
                norte_frontend::help::SidebarRow::Topic { id, .. } => Some(id.as_str()),
                norte_frontend::help::SidebarRow::Group { .. } => None,
            })
            .collect()
    }

    #[test]
    fn set_plugins_solo_da_fila_a_quien_trae_pagina() {
        let mut view = HelpView::new(norte_i18n::Lang::En, Vec::new());
        view.set_plugins(&[
            info("acme.ftp", true, true, true),
            info("acme.mute", false, true, true),
        ]);
        let ids = topic_ids(&view);
        assert!(
            ids.contains(&"acme.ftp"),
            "a plugin with help has a row: {ids:?}"
        );
        assert!(
            !ids.contains(&"acme.mute"),
            "a node that opens nothing is a dead end the reader pays a keystroke for"
        );
    }

    /// H3f review (encoding H1 / rust MAJOR 3): the H3e fix the GUI was
    /// missing. A name that is blank AFTER masking falls back to the id, so the
    /// Extensions group cannot grow a row with no attribution on it — the row a
    /// reader opens to decide whether to approve that very plugin.
    ///
    /// The blank comes from the shared corpus (`invisible_filler_blank`,
    /// U+3164 HANGUL FILLER ×3), whose own `why` names this exact surface: it
    /// is not a terminal hazard, so masking passes it through, and it is not
    /// whitespace, so a `trim` check would too.
    #[test]
    fn un_nombre_en_blanco_cae_al_id_del_plugin() {
        let filler = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == "invisible_filler_blank")
            .expect("la fixture del corpus existe");
        let blank = String::from_utf8(filler.bytes).expect("la fixture es UTF-8");
        for name in [blank.as_str(), "", "   "] {
            let mut p = info("acme.ftp", true, true, true);
            p.name = name.to_owned();
            let mut view = HelpView::new(norte_i18n::Lang::En, Vec::new());
            view.set_plugins(&[p]);
            let titles: Vec<String> = view
                .state
                .rows()
                .iter()
                .filter_map(|r| match r {
                    norte_frontend::help::SidebarRow::Topic { id, title }
                        if id.as_str() == "acme.ftp" =>
                    {
                        Some(title.clone())
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(
                titles,
                vec!["acme.ftp".to_owned()],
                "a nameless row for name={name:?}"
            );
        }
    }

    /// H3f review (encoding M1 / rust MAJOR 2): an id this process had no
    /// business receiving is DROPPED, never rewritten — it becomes a `TopicId`,
    /// is folded by the filter on every keystroke, and goes back out as the
    /// argument of `plugin.help`.
    #[test]
    fn un_id_invalido_no_entra_en_el_modelo() {
        for bad in [
            String::new(),
            "has:colon".to_owned(),
            "acme.\u{202e}ftp".to_owned(),
            "a".repeat(4096),
        ] {
            let mut p = info("placeholder", true, true, true);
            p.id = bad.clone();
            let mut view = HelpView::new(norte_i18n::Lang::En, Vec::new());
            view.set_plugins(&[p]);
            assert!(
                topic_ids(&view).iter().all(|id| *id != bad),
                "id {bad:?} reached the sidebar"
            );
        }
    }

    /// H3f review (encoding M2): a blank command title must not satisfy the
    /// snapshot lookup, because `norte_help::label_or_id` reads a blank label
    /// as "no label" and falls back to painting the RAW dispatch key — which is
    /// what the snapshot exists to prevent.
    #[test]
    fn un_titulo_de_comando_en_blanco_no_secuestra_la_etiqueta() {
        let (browse, viewer) = effectives();
        let mut p = info("acme.ftp", true, true, true);
        p.commands[0].title = "\u{3164}".to_owned();
        let mut view = HelpView::new(norte_i18n::Lang::En, Vec::new());
        view.set_plugins(&[p]);
        let r = view.freeze(
            &GuiChords::new(&browse, &viewer, norte_i18n::Lang::En),
            NO_IMPEDIMENT,
        );
        let key = "plugin:acme.ftp:greet";
        // The masked, capped spelling of the key — the `label` fallback — and
        // never the blank the manifest offered.
        assert_eq!(r.label(key), plugin_text(key));
    }

    #[test]
    fn claim_plugin_fetch_pregunta_una_sola_vez() {
        let mut view = HelpView::new(norte_i18n::Lang::En, Vec::new());
        view.set_plugins(&[info("acme.ftp", true, true, true)]);
        view.state.open(&norte_help::TopicId::new("acme.ftp"));
        assert_eq!(view.claim_plugin_fetch().as_deref(), Some("acme.ftp"));
        assert_eq!(
            view.claim_plugin_fetch(),
            None,
            "a page that never arrives is not asked for again"
        );
    }

    #[test]
    fn una_pagina_que_no_llega_deja_la_pagina_vacia() {
        let mut view = HelpView::new(norte_i18n::Lang::En, Vec::new());
        view.set_plugins(&[info("acme.ftp", true, true, true)]);
        view.state.open(&norte_help::TopicId::new("acme.ftp"));
        let _ = view.claim_plugin_fetch();
        assert!(
            view.state.current_topic().is_none(),
            "an empty page with the plugin's name beats an error toast over the help"
        );
    }

    #[test]
    fn install_enmascara_la_pagina_y_conserva_el_editor_del_snapshot() {
        let mut view = HelpView::new(norte_i18n::Lang::En, Vec::new());
        view.set_plugins(&[info("acme.ftp", true, true, true)]);
        view.state.open(&norte_help::TopicId::new("acme.ftp"));
        let _ = view.claim_plugin_fetch();
        view.install_plugin_page(
            "acme.ftp",
            &norte_proto::methods::PluginHelpResult {
                // `id` is REQUIRED in the front matter — a header without it
                // degrades to "there is no header" and the page wears the
                // host-assigned fallback title instead.
                markdown: "---\nid = \"acme.ftp\"\ntitle = \"T\u{202e}itle\"\n---\n\nbody".into(),
                truncated: false,
                lossy: false,
            },
        );
        let topic = view.state.current_topic().expect("the page is installed");
        assert!(
            !topic.title.contains('\u{202e}'),
            "bidi override survived: {:?}",
            topic.title
        );
        assert!(
            matches!(&topic.origin, norte_help::Origin::Plugin { publisher, .. }
                if publisher.as_deref() == Some("ACME")),
            "the publisher comes from the SNAPSHOT, never from the page: {:?}",
            topic.origin
        );
    }

    #[test]
    fn el_resolver_congelado_conoce_los_plugins_activos_y_sus_titulos() {
        let (browse, viewer) = effectives();
        let mut view = HelpView::new(norte_i18n::Lang::En, Vec::new());
        // Approved but DISABLED: it has a page, and its rows do not run.
        view.set_plugins(&[info("acme.ftp", true, true, false)]);
        let r = view.freeze(
            &GuiChords::new(&browse, &viewer, norte_i18n::Lang::En),
            NO_IMPEDIMENT,
        );
        let key = "plugin:acme.ftp:greet";
        assert_eq!(r.label(key), "Greet");
        assert!(
            !r.availability(key).is_available(),
            "a disabled plugin dims its own rows instead of promising a refused dispatch"
        );
    }

    #[test]
    fn keys_lines_cubre_ambas_pantallas_y_traduce_cada_binding() {
        let (browse, viewer) = effectives();
        let lines = keys_lines(&browse, &viewer);
        let joined = lines.join("\n");
        assert!(joined.contains(&norte_i18n::t("help-section-browse")));
        assert!(joined.contains(&norte_i18n::t("help-section-viewer")));
        // A real binding, spelled the way the documentation spells it.
        assert!(joined.contains("F5"), "no browse chord: {joined}");
        // Nothing paints a bare Fluent id at the reader.
        assert!(
            !joined.contains("help-cmd-"),
            "untranslated id in the sheet: {joined}"
        );
    }

    fn chords() -> GuiChords {
        let (browse, viewer) = effectives();
        GuiChords::new(&browse, &viewer, norte_i18n::Lang::En)
    }

    /// Focuses the body of a topic that HAS runnable rows, the way a reader
    /// does: open the page, then `⇥`.
    fn body_of(topic: &str) -> HelpView {
        let mut view = HelpView::new(norte_i18n::Lang::En, Vec::new());
        view.state.open(&norte_help::TopicId::new(topic));
        let _ = on_key(&mut view, "tab", None, &chords());
        assert_eq!(
            view.state.focus(),
            norte_frontend::help::Focus::Body,
            "{topic} has no runnable rows: pick another topic for this test"
        );
        view
    }

    #[test]
    fn escape_cierra_y_barra_abre_el_filtro() {
        let c = chords();
        let mut view = HelpView::new(norte_i18n::Lang::En, Vec::new());
        assert!(matches!(
            on_key(&mut view, "escape", None, &c),
            HelpOutcome::Close
        ));
        assert!(matches!(
            on_key(&mut view, "/", Some("/"), &c),
            HelpOutcome::None
        ));
        assert!(view.state.filtering());
        let _ = on_key(&mut view, "c", Some("c"), &c);
        assert_eq!(view.state.filter_raw(), "c");
        // Backspace erases WHILE filtering instead of walking history back.
        let _ = on_key(&mut view, "backspace", None, &c);
        assert_eq!(view.state.filter_raw(), "");
        // And a `/` typed INTO the filter is text, not a second open.
        let _ = on_key(&mut view, "/", Some("/"), &c);
        assert_eq!(view.state.filter_raw(), "/");
    }

    #[test]
    fn enter_sobre_una_fila_ejecutable_despacha_el_mismo_id_que_la_paleta() {
        let topic = norte_help::topic(norte_help::Lang::En, "copying").expect("corpus topic");
        let first = topic.commands.first().expect("copying has command rows");
        let mut view = body_of("copying");
        match on_key(&mut view, "enter", None, &chords()) {
            HelpOutcome::Run(cmd) => {
                // The FOCUSED row, not merely something that looks like an id:
                // the body cursor starts on the first action.
                assert_eq!(&cmd, first);
                // …and it is a key this frontend can actually dispatch, which
                // is what "the same id as the palette" means.
                assert!(
                    crate::keymap::COMMANDS.contains(&cmd.as_str()),
                    "{cmd} is not in the GUI's command table"
                );
            }
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn enter_sobre_una_fila_atenuada_no_despacha_nada() {
        let (browse, viewer) = effectives();
        // Both panes read-only: the copy/move/delete rows of `copying` dim.
        let c = GuiChords::new(&browse, &viewer, norte_i18n::Lang::En).with_facts(Facts {
            source_read_only: true,
            dest_read_only: true,
            ..NO_IMPEDIMENT
        });
        let mut view = body_of("copying");
        match on_key(&mut view, "enter", None, &c) {
            HelpOutcome::Blocked(_) => {}
            other => panic!("a dimmed row must not dispatch: {other:?}"),
        }
    }

    #[test]
    fn tab_alterna_el_foco_y_backspace_vuelve_atras() {
        let c = chords();
        let mut view = HelpView::new(norte_i18n::Lang::En, Vec::new());
        let start = view.state.current().clone();
        view.state.open(&norte_help::TopicId::new("copying"));
        assert_ne!(view.state.current(), &start);
        let _ = on_key(&mut view, "backspace", None, &c);
        assert_eq!(view.state.current(), &start, "history back");
        assert_eq!(view.state.focus(), norte_frontend::help::Focus::Topics);
        let _ = on_key(&mut view, "tab", None, &c);
        assert_eq!(view.state.focus(), norte_frontend::help::Focus::Body);
    }

    /// H3f review (rust MAJOR 6): the filter is a regime. `Esc` inside it
    /// closes the BOX and keeps the text; outside it closes the overlay. And
    /// once it is closed, a typed letter must not go on editing a search the
    /// reader already left — that silently rebuilt the sidebar under the page
    /// they were reading.
    #[test]
    fn el_filtro_se_abandona_sin_cerrar_la_ayuda() {
        let c = chords();
        let mut view = HelpView::new(norte_i18n::Lang::En, Vec::new());
        let _ = on_key(&mut view, "/", Some("/"), &c);
        for ch in "cop".chars() {
            let s = ch.to_string();
            let _ = on_key(&mut view, &s, Some(&s), &c);
        }
        assert!(matches!(
            on_key(&mut view, "escape", None, &c),
            HelpOutcome::None
        ));
        assert!(!view.state.filtering(), "Esc left the box");
        assert_eq!(view.state.filter_raw(), "cop", "…and kept the search");
        // A letter now is inert, not more filter text.
        let _ = on_key(&mut view, "x", Some("x"), &c);
        assert_eq!(view.state.filter_raw(), "cop");
        // And Esc again closes the overlay, as the footer advertises.
        assert!(matches!(
            on_key(&mut view, "escape", None, &c),
            HelpOutcome::Close
        ));
    }

    /// Enter inside the box also leaves it (and does NOT open a topic behind
    /// the reader's back).
    #[test]
    fn enter_en_el_filtro_solo_cierra_la_caja() {
        let c = chords();
        let mut view = HelpView::new(norte_i18n::Lang::En, Vec::new());
        let before = view.state.current().clone();
        let _ = on_key(&mut view, "/", Some("/"), &c);
        assert!(matches!(
            on_key(&mut view, "enter", None, &c),
            HelpOutcome::None
        ));
        assert!(!view.state.filtering());
        assert_eq!(view.state.current(), &before);
    }

    #[test]
    fn ctrl_p_entrega_el_filtro_crudo_a_la_paleta() {
        let c = chords();
        let mut view = HelpView::new(norte_i18n::Lang::En, Vec::new());
        let _ = on_key(&mut view, "/", Some("/"), &c);
        for ch in "copy".chars() {
            let s = ch.to_string();
            let _ = on_key(&mut view, &s, Some(&s), &c);
        }
        match handoff(&view) {
            HelpOutcome::Palette(q) => assert_eq!(q, "copy"),
            other => panic!("expected Palette, got {other:?}"),
        }
    }

    #[test]
    fn congelar_facts_no_pierde_el_snapshot_de_plugins() {
        let (browse, viewer) = effectives();
        let key = "plugin:org.norte.demo:greet";
        let r = GuiChords::new(&browse, &viewer, norte_i18n::Lang::En)
            .with_plugins(
                BTreeSet::from(["org.norte.demo".to_owned()]),
                HashMap::new(),
            )
            .with_facts(Facts {
                source_read_only: true,
                ..NO_IMPEDIMENT
            });
        assert!(
            r.availability(key).is_available(),
            "the re-freeze must not dim every plugin row"
        );
    }
}
