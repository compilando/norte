//! Help (F1) as seen from the host: which page is open and how it is
//! projected for whoever paints it.
//!
//! None of this is new. The corpus and its block model belong to
//! `norte-help`, the overlay —sidebar, history, filter, what is
//! executable— is `norte_frontend::help::HelpState`, resolving the live
//! marks is `norte_frontend::help_chords::Chords` and the keyboard sheet is
//! `norte_frontend::keysheet`. What this module contributes is the
//! PROJECTION: turning all of that into the closed vocabulary that crosses
//! the bridge, so the renderer builds DOM nodes one at a time and does not
//! interpret markup.
//!
//! Two things are frozen on OPEN and not recomputed on paint, for the same
//! reason the continuations panel is built at the transition: the facts a
//! row is dimmed by (`Facts`) and the keyboard sheet, which are several
//! strings and a Fluent format per row. Freezing the facts also keeps a
//! page from contradicting itself mid-read —a row dimmed above and live
//! below because the cursor moved underneath—, which is the same decision
//! the TUI made.

use std::collections::{HashMap, HashSet};

use norte_frontend::help::{Action, Focus, HelpState, KEYS_ID, PluginNode, SidebarRow};
use norte_frontend::help_badge::plugin_label;
use norte_frontend::help_chords::Chords;
use norte_frontend::keymap::{Availability, Effective, Screen, short_unavailable_message};
use norte_frontend::keysheet::sheet;
use norte_help::{Block, Span, TopicId, rows_of};
use norte_i18n::Lang;

use crate::bridge::clamp_display;
use crate::dto::{
    HelpActionView, HelpBlockView, HelpFocusView, HelpKeyRowView, HelpSidebarRowView, HelpSpanView,
    HelpView,
};

/// Help, open.
pub(crate) struct Help {
    /// The shared model: which page, which cursor, which filter.
    pub(crate) state: HelpState,
    /// The resolver THIS opening is painted with, with its facts already
    /// frozen.
    chords: Chords,
    /// The facts `chords` carries inside, to know whether re-freezing
    /// changes anything. `Chords` does not return them, and asking "did it
    /// change?" is the difference between a patch when the listing moves
    /// and a patch for every fill batch.
    facts: norte_frontend::availability::Facts,
    /// The keyboard sheet, generated once with the reader's effective map.
    keys: Vec<HelpBlockView>,
    /// Who to attribute each plugin's page to, already masked and clamped.
    ///
    /// Comes from the catalogue's SNAPSHOT and never from the page: a
    /// plugin does not decide who publishes it. A blank publisher does not
    /// go in —the badge would paint "published by " with nothing behind
    /// it, which reads as a painting bug and not as an absence.
    publishers: HashMap<String, String>,
    /// The plugin pages already REQUESTED in this opening.
    ///
    /// "Requested once" belongs here and not to the model: `plugin_needs_fetch`
    /// is a POLLING question, and without this memory a dead daemon would be
    /// asked again on every projection.
    requested: HashSet<String>,
    /// The last request to scroll the body, with its sequence number (bridge
    /// 76). See [`crate::dto::HelpView::scroll`].
    scroll: Option<crate::dto::HelpScrollView>,
}

impl Help {
    /// Asks the renderer to scroll the body. Each request carries a number
    /// higher than the last, so a repaint does not apply it twice.
    pub(crate) fn scroll(&mut self, to: crate::dto::HelpScrollTo) {
        let seq = self.scroll.map_or(1, |d| d.seq + 1);
        self.scroll = Some(crate::dto::HelpScrollView { to, seq });
    }

    /// Opens help on the page for the CONTEXT the reader is in, or on the
    /// index if that context has no page.
    ///
    /// The context is a word from the corpus's closed vocabulary and there
    /// is always one: each page's front matter says who claims it, so a new
    /// page for a new dialog does not touch this code.
    ///
    /// The context's page is opened as ROOT (`open_as_root`): the reader
    /// did not navigate to it, help put them there, so "back" cannot take
    /// them to an index they were never in — and a single `Esc` press has
    /// to leave behind a page nobody asked for.
    pub(crate) fn open(
        lang: Lang,
        context: &str,
        listing: &Effective,
        visor: &Effective,
        facts: norte_frontend::availability::Facts,
    ) -> Self {
        let mut state = HelpState::new(lang, norte_i18n::t_in(lang, "help-topic-keys"));
        if let Some(t) = norte_help::topic_for_context(lang, context) {
            state.open_as_root(&t.id);
        }
        Self {
            state,
            publishers: HashMap::new(),
            requested: HashSet::new(),
            scroll: None,
            // TWO screens and not three: this window's dialogs are answered
            // by the renderer with its own buttons, so there is no
            // `dialog` map to resolve one of its verbs in, and giving it a
            // key would put it on a page nobody is going to press.
            chords: Chords::over(&[listing, visor], lang).with_facts(facts),
            facts,
            keys: keyboard_sheet(listing, visor, lang),
        }
    }

    /// Re-freezes the facts WITHOUT losing anything else.
    ///
    /// The freeze guards against the READER moving, not against the world
    /// moving: two of the facts —`enterable` and `viewable`— describe the
    /// entry under the cursor, and a copy or a delete that ends with help
    /// in front re-lists the pane underneath. Without this, the reason
    /// sentence keeps talking about a selection that no longer exists
    /// (#262). The TUI does the same through its refresh funnel.
    ///
    /// Keeps the plugin SNAPSHOT on purpose (`with_facts` copies it): a
    /// listing does not change that, and asking for it again would leave
    /// the sidebar without extension pages for a flicker.
    /// Returns whether the facts CHANGED. Re-freezing the same thing is not
    /// a screen change, and publishing it as one would be a patch for every
    /// fill batch of a directory the reader is not even looking at.
    pub(crate) fn refreeze(&mut self, facts: norte_frontend::availability::Facts) -> bool {
        if self.facts == facts {
            return false;
        }
        self.facts = facts;
        self.chords = self.chords.with_facts(facts);
        true
    }

    /// The whole projection.
    ///
    /// `effects` and `visor_open` are the two halves of the question
    /// "can THIS window run this row?", which is not the same as "can it
    /// be run right now?" — see [`motivo_de`].
    pub(crate) fn vista(
        &self,
        lang: Lang,
        effects: crate::commands::Effects,
        visor_open: bool,
    ) -> HelpView {
        let topic = self.state.current_topic();
        let in_keys = self.state.current().as_str() == KEYS_ID;
        // An extension page that has not arrived yet has no `Topic`, and
        // that is where the hole was: no title and NO PROVENANCE LINE, that
        // is, with the exact shape of a page from the binary itself. The
        // sidebar node does know its name, and that it is from a third
        // party, so the in-flight page is painted with both.
        let in_flight = topic
            .is_none()
            .then(|| self.state.plugin_needs_fetch())
            .flatten();
        let title = if in_keys {
            norte_i18n::t_in(lang, "help-topic-keys")
        } else if let Some(id) = in_flight {
            self.title_of_node(id)
        } else {
            topic.map_or_else(String::new, |t| t.title.clone())
        };
        let rows = self.rows(lang, effects, visor_open);
        HelpView {
            title: clamp_display(title),
            // Does NOT go through `clamp_display`: it is a KEY, and
            // clamping is not injective. Whole or empty.
            topic_id: identity(self.state.current().as_str()),
            badge: self.insignia(lang, topic, in_flight),
            sidebar: self.lateral(lang),
            cursor: self.state.cursor() as u64,
            focus: match self.state.focus() {
                Focus::Topics => HelpFocusView::Topics,
                Focus::Body => HelpFocusView::Body,
            },
            blocks: if in_keys {
                self.keys.clone()
            } else {
                topic.map_or_else(Vec::new, |t| {
                    t.blocks
                        .iter()
                        .map(|b| block(b, &self.chords, self.state.actions()))
                        .collect()
                })
            },
            actions: rows.into_iter().map(|(_, v, _)| v).collect(),
            action_cursor: (!self.state.actions().is_empty())
                .then_some(self.state.action_cursor() as u64),
            filter: clamp_display(self.state.filter_display()),
            filtering: self.state.filtering(),
            can_back: self.state.can_back(),
            scroll: self.scroll,
        }
    }

    /// The title the sidebar names `id` with.
    ///
    /// Comes from the NODE and not the page, because the page is exactly
    /// what has not arrived. Already masked and clamped at the point of
    /// entry.
    fn title_of_node(&self, id: &str) -> String {
        self.state
            .rows()
            .iter()
            .find_map(|r| match r {
                SidebarRow::Topic { id: this, title } if this.as_str() == id => Some(title.clone()),
                _ => None,
            })
            .unwrap_or_else(|| id.to_owned())
    }

    /// The provenance line, if the page is from a third party.
    ///
    /// An extension page ALWAYS carries it, even while it is still being
    /// requested: a line that only sometimes appears teaches the opposite
    /// of the truth when it is missing, and whoever reads it is deciding
    /// whether to approve the extension. With the page in flight it is not
    /// yet known whether it was truncated or whether some bytes failed to
    /// decode, so the two things that ARE known are said: that it is from
    /// an extension, and who publishes it.
    fn insignia(
        &self,
        lang: Lang,
        topic: Option<&norte_help::Topic>,
        in_flight: Option<&str>,
    ) -> Option<String> {
        if let Some(id) = in_flight {
            return norte_frontend::help_badge::plugin_badge(
                self.publishers.get(id).map(String::as_str),
                false,
                false,
                lang,
            )
            .map(clamp_display);
        }
        match &topic?.origin {
            norte_help::Origin::BuiltIn => None,
            norte_help::Origin::Plugin {
                publisher,
                truncated,
                lossy,
                ..
            } => norte_frontend::help_badge::plugin_badge(
                publisher.as_deref(),
                *truncated,
                *lossy,
                lang,
            )
            .map(clamp_display),
        }
    }

    /// The sidebar, with group headers already translated.
    fn lateral(&self, lang: Lang) -> Vec<HelpSidebarRowView> {
        let actual = self.state.current();
        self.state
            .rows()
            .iter()
            .map(|r| match r {
                SidebarRow::Group { tag } => HelpSidebarRowView::Group {
                    label: clamp_display(group_label(tag, lang)),
                },
                SidebarRow::Topic { id, title } => HelpSidebarRowView::Topic {
                    title: clamp_display(title.clone()),
                    current: id == actual,
                },
            })
            .collect()
    }

    /// The executable rows: the MODEL's action and its projection, together.
    ///
    /// A single pass produces both on purpose. `HelpActivate{index}` indexes
    /// the model with an index that came out of the projection, so two
    /// separate traversals were two lists that could drift apart without
    /// anything saying so — and then a click on "copy" runs something else.
    /// The order is [`HelpState::actions`]'s: first the commands the page
    /// documents, then its "see also" links.
    fn rows(
        &self,
        lang: Lang,
        effects: crate::commands::Effects,
        visor_open: bool,
    ) -> Vec<(Action, HelpActionView, Option<&'static str>)> {
        let Some(topic) = self.state.current_topic() else {
            return Vec::new();
        };
        let mut out: Vec<(Action, HelpActionView, Option<&'static str>)> =
            rows_of(topic, &self.chords)
                .into_iter()
                .map(|r| {
                    let motivo = motivo_de(&r.row, effects, visor_open);
                    let vista = HelpActionView {
                        // A command's name can come from a user keymap layer
                        // or from the project, which carries no trust, and
                        // `label_or_id` falls back to the raw id when the
                        // catalogue does not name it.
                        label: clamp_display(norte_frontend::display_name(r.label.as_bytes()).0),
                        chord: clamp_display(r.chord.unwrap_or_default()),
                        enabled: motivo.is_none(),
                        reason: clamp_display(
                            motivo.map_or_else(String::new, |k| norte_i18n::t_in(lang, k)),
                        ),
                        opens_topic: false,
                    };
                    (Action::Run(r.row.command), vista, motivo)
                })
                .collect();
        // `links()` and not `see_also`: the prose's links are followed too.
        out.extend(topic.links().iter().map(|id| {
            let vista = HelpActionView {
                label: clamp_display(title_of(id, self.state.lang())),
                chord: String::new(),
                // A link can always be followed: all it does is change the
                // page, and if this language's corpus does not have it, the
                // model itself ignores it without panicking.
                enabled: true,
                reason: String::new(),
                opens_topic: true,
            };
            (Action::Open(id.clone()), vista, None)
        }));
        out
    }

    /// What action `i` does, if this window can do it.
    ///
    /// `Err` is the Fluent key for the reason. The HOST checks it, not the
    /// renderer: if the check lived only in whoever paints, the keyboard
    /// path —which does not go through there— would run a dimmed row.
    pub(crate) fn action_executable(
        &self,
        i: usize,
        lang: Lang,
        effects: crate::commands::Effects,
        visor_open: bool,
    ) -> Result<Action, String> {
        let rows = self.rows(lang, effects, visor_open);
        let (action, _, motivo) = rows.get(i).ok_or_else(String::new)?;
        match motivo {
            None => Ok(action.clone()),
            Some(k) => Err((*k).to_owned()),
        }
    }

    /// Feeds the plugin catalogue into the model (H3e).
    ///
    /// The ONLY entry point for third-party text into this window's help,
    /// and it does three things the model does not:
    ///
    /// 1. **Discards** —never rewrites— an id that is not a valid
    ///    reverse-DNS one. An id is a KEY: it goes into a `TopicId`, comes
    ///    out as an argument of `plugin.help` and is what the sidebar
    ///    filter folds on every keystroke. Masking it would not be a
    ///    security measure, because it is not injective: it would map two
    ///    different plugins to the same row.
    /// 2. **Masks and clamps** the name and the publisher, which are
    ///    third-party prose, at the point of entry and not at paint time:
    ///    `PluginNode` documents its title as "already safe" and the model
    ///    masks nothing.
    /// 3. Falls back to the **id** when the name is left blank. `name` is
    ///    mandatory in the manifest but nobody checks that it says
    ///    anything, and a name made of HANGUL filler characters paints an
    ///    empty row under the extensions header: a page that can be opened
    ///    and read, with no name.
    ///
    /// A node is ACTIVE if the plugin is approved AND enabled. That decides
    /// whether its command rows can be run, never whether its page can be
    /// seen: a human reads an extension's documentation precisely to decide
    /// whether to enable it.
    pub(crate) fn set_plugins(&mut self, plugins: &[norte_proto::methods::PluginInfo]) {
        let validos: Vec<&norte_proto::methods::PluginInfo> = plugins
            .iter()
            .filter(|p| norte_proto::methods::is_valid_plugin_id(&p.id))
            .collect();
        self.publishers = validos
            .iter()
            .filter_map(|p| {
                let who = plugin_label(&p.publisher);
                (!norte_help::is_blank_id(&who)).then(|| (p.id.clone(), who))
            })
            .collect();
        self.state.set_plugins(
            validos
                .iter()
                .map(|p| {
                    let name = plugin_label(&p.name);
                    PluginNode {
                        id: p.id.clone(),
                        title: if norte_help::is_blank_id(&name) {
                            plugin_label(&p.id)
                        } else {
                            name
                        },
                        has_help: p.has_help,
                        active: p.approved && p.enabled,
                    }
                })
                .collect(),
        );
    }

    /// The plugin page that must be requested, if there is one and it has
    /// not been requested yet in this opening. Claiming it marks it as
    /// requested.
    pub(crate) fn claim_page(&mut self) -> Option<String> {
        let id = self.state.plugin_needs_fetch()?.to_owned();
        self.requested.insert(id.clone()).then_some(id)
    }

    /// Installs a plugin's page, parsed as what it is: text that is not
    /// controlled.
    ///
    /// `fold_flags` is not optional: the text arrives ALREADY clamped and
    /// ALREADY decoded by the daemon, so this parse comes out clean and the
    /// badge —which is all the visible mitigation against a hostile
    /// `help.md`— would go dark.
    pub(crate) fn install_page(&mut self, id: &str, res: &norte_proto::methods::PluginHelpResult) {
        // The publisher comes from the SNAPSHOT, never from the page: a
        // plugin does not say who publishes it. `parse_untrusted` masks it
        // again anyway, which is harmless.
        let publisher = self.publishers.get(id).cloned();
        let parsed = norte_help::parse_untrusted(res.markdown.as_bytes(), id, publisher)
            .fold_flags(res.truncated, res.lossy);
        self.state.install_plugin_topic(parsed.topic);
    }

    /// Moves the body cursor to row `i` — what a click on it means, the
    /// half that does NOT execute.
    ///
    /// Done in addition to executing, not instead of: after following a
    /// link, the next arrow key has to move from where the reader just
    /// pointed, not from the sidebar.
    pub(crate) fn point_at(&mut self, i: usize) {
        self.state.click_action(i);
    }
}

/// Why this window CANNOT run a row, or `None` if it can.
///
/// There are TWO questions, and either one turns the row off:
///
/// - The **shared catalogue** answers "can it be run RIGHT NOW?" with the
///   facts frozen when help was opened (inside a zip nothing gets copied
///   here).
/// - This **window's list** answers "does this window do it?", and that
///   answer depends on the SCREEN the command belongs to, not on a flat
///   list: a `dialog.*` verb is answered by the dialog itself with its own
///   buttons, and a viewer command only means something with the viewer
///   open. Asking against the flat list got both answers wrong at once — a
///   dialog page came out entirely dimmed "because this window doesn't do
///   it", and the viewer's rows came out enabled with the viewer closed,
///   only to refuse when pressed.
fn motivo_de(
    row: &norte_help::CommandRow,
    effects: crate::commands::Effects,
    visor_open: bool,
) -> Option<&'static str> {
    let cmd = row.command.as_str();
    if cmd.starts_with("dialog.") {
        return Some(norte_frontend::availability::reason_key(
            norte_help::Reason::AnsweredByTheOverlay,
        ));
    }
    if crate::commands::IMPLEMENTED_VISOR.contains(&cmd) {
        return (!visor_open).then_some("reason-viewer-only");
    }
    if !crate::commands::implemented(effects).contains(&cmd) {
        return Some("keymap-short-not-here");
    }
    row.avail
        .reason()
        .map(norte_frontend::availability::reason_key)
}

/// An IDENTITY that crosses the bridge: whole, or empty.
///
/// Never clamped. Clamping is not injective, and this is a key: two ids
/// that matched on their first few thousand bytes would arrive as one and
/// the same, which is the trap ADR 0061 decided never to set again. A key
/// that does not fit becomes one that matches nothing, which is a visible
/// failure, instead of one that matches the wrong thing.
fn identity(id: &str) -> String {
    if id.len() > crate::bridge::MAX_STRING_BYTES {
        return String::new();
    }
    id.to_owned()
}

/// The synthetic keyboard page: every bound key of every screen, with its
/// catalogue label, in the map's REAL precedence order.
///
/// Generated, never a hand-maintained list: a rebind changes it. It is not
/// a corpus table because its rows carry availability and a reason, which a
/// table cell has nowhere to put.
fn keyboard_sheet(listing: &Effective, visor: &Effective, lang: Lang) -> Vec<HelpBlockView> {
    let mut out = vec![HelpBlockView::Paragraph {
        spans: vec![HelpSpanView::Text {
            text: clamp_display(norte_i18n::t_in(lang, "keys-page-note")),
        }],
    }];
    for (title, screen, eff) in [
        ("help-section-browse", Screen::Browse, listing),
        ("help-section-viewer", Screen::Viewer, visor),
    ] {
        let rows: Vec<HelpKeyRowView> = sheet(&[(screen, eff.clone())])
            .into_iter()
            .map(|row| {
                // The label can come from a user's `keymap.toml`: it is
                // masked, and it says that it was masked (#266).
                let label = norte_frontend::whichkey::command_label(&row.command, lang);
                let (paintable, hostile) = norte_frontend::display_name(label.as_bytes());
                HelpKeyRowView {
                    chord: clamp_display(row.chord),
                    label: clamp_display(paintable),
                    label_hostile: hostile,
                    enabled: row.avail == Availability::Here,
                    reason: clamp_display(short_unavailable_message(row.avail, lang)),
                }
            })
            .collect();
        if rows.is_empty() {
            continue;
        }
        out.push(HelpBlockView::Heading {
            level: 2,
            text: clamp_display(norte_i18n::t_in(lang, title)),
        });
        out.push(HelpBlockView::Keys { rows });
    }
    out
}

/// A sidebar group's header, translated.
///
/// `t_in` answers a key it does not have with the key itself, so a tag with
/// no entry would paint `help-group-…` for the reader: that echo is
/// detected —it IS the failure— and it falls back to the tag, which is at
/// least a word.
fn group_label(tag: &str, lang: Lang) -> String {
    let id = format!("help-group-{tag}");
    let text = norte_i18n::t_in(lang, &id);
    if text == id { tag.to_owned() } else { text }
}

/// A page's title, or its id if this language's corpus does not have it.
fn title_of(id: &TopicId, lang: Lang) -> String {
    norte_help::topic(lang, id.as_str()).map_or_else(|| id.as_str().to_owned(), |t| t.title.clone())
}

/// A corpus block, projected.
fn block(b: &Block, chords: &Chords, actions: &[Action]) -> HelpBlockView {
    match b {
        Block::Heading { level, text } => HelpBlockView::Heading {
            level: (*level).clamp(1, 3),
            text: clamp_display(text.clone()),
        },
        Block::Paragraph(spans) => HelpBlockView::Paragraph {
            spans: spans
                .iter()
                .map(|s| fragmento(s, chords, actions))
                .collect(),
        },
        Block::Bullets(items) => HelpBlockView::Bullets {
            items: items
                .iter()
                .map(|spans| {
                    spans
                        .iter()
                        .map(|s| fragmento(s, chords, actions))
                        .collect()
                })
                .collect(),
        },
        Block::Code { lang, text } => HelpBlockView::Code {
            lang: lang.clone().map(clamp_display),
            text: clamp_display(text.clone()),
        },
        Block::Table { header, rows } => HelpBlockView::Table {
            header: header.iter().cloned().map(clamp_display).collect(),
            rows: rows
                .iter()
                .map(|r| r.iter().cloned().map(clamp_display).collect())
                .collect(),
        },
        Block::Callout { kind, spans } => HelpBlockView::Callout {
            kind: match kind {
                norte_help::Callout::Note => crate::dto::HelpCalloutView::Note,
                norte_help::Callout::Warn => crate::dto::HelpCalloutView::Warn,
                norte_help::Callout::Tip => crate::dto::HelpCalloutView::Tip,
            },
            spans: spans
                .iter()
                .map(|s| fragmento(s, chords, actions))
                .collect(),
        },
    }
}

/// A fragment, with both LIVE marks already resolved against this reader's
/// keymap and language.
fn fragmento(s: &Span, chords: &Chords, actions: &[Action]) -> HelpSpanView {
    match s {
        Span::Text(t) => HelpSpanView::Text {
            text: clamp_display(t.clone()),
        },
        Span::Strong(t) => HelpSpanView::Strong {
            text: clamp_display(t.clone()),
        },
        Span::Emph(t) => HelpSpanView::Emph {
            text: clamp_display(t.clone()),
        },
        Span::Code(t) => HelpSpanView::Code {
            text: clamp_display(t.clone()),
        },
        Span::CommandRef(c) => {
            // The last step of `render_command` is the command's RAW ID,
            // and in trusted mode the parser does not check its alphabet:
            // `{{cmd:\u{202E}fs.copy}}` survives intact. What makes it safe
            // not to mask here is that the EMBEDDED corpus goes through a
            // gate (`check_commands` against the shared list, which today
            // runs in `norte-tui/tests/help_gate.rs`) and this host paints
            // that SAME corpus. A plugin page does not count: it comes from
            // `parse_untrusted`, which refuses a key it could not paint.
            let text = norte_help::render_command(c, chords);
            HelpSpanView::Command {
                is_chord: text.is_chord(),
                text: clamp_display(text.into_text()),
            }
        }
        Span::TopicLink(id) => HelpSpanView::Link {
            text: clamp_display(title_of(id, chords_lang(chords))),
            // The row that follows it: `Topic::links()` puts every prose
            // link into the actions, so pressing it activates that row.
            action: actions
                .iter()
                .position(|a| matches!(a, Action::Open(dest) if dest == id))
                .map(|i| i as u64),
        },
    }
}

/// The language the resolver was built with.
///
/// Asked of the resolver and not passed as a parameter because they are the
/// same language by construction, and two sources is where a page ends up
/// with the title in one language and the prose in another.
fn chords_lang(chords: &Chords) -> Lang {
    chords.lang()
}
