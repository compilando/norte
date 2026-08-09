//! `norte help` (H3g): the corpus as plain text, embedded, no daemon.
//!
//! Two pieces, the same two every frontend needs. [`CliChords`] is this
//! command's answer to the three questions `norte-help` asks — the reader's
//! chord, a short label, and whether the command can run — and [`render_topic`]
//! turns a page into lines instead of widgets.
//!
//! It shares no rendering code with the TUI's or the GUI's on purpose. Those
//! two paint into a pane they must not overflow and style by theme role; this
//! one writes to a stream that may be a pipe, so it wraps nothing, styles
//! nothing, and spells its callouts as ASCII words.
//!
//! # What it does NOT mask, and why that is a claim about its inputs
//!
//! Corpus prose is trusted text, swept for terminal hazards by
//! `norte-help`'s own corpus test — a raw `ESC` in a code fence would be an
//! ANSI injection into the reader's terminal, and this module would print it
//! faithfully. Chords are masked by `norte_frontend::keymap::paint_chord`
//! before they enter [`CliChords`]. A plugin page (not reachable from this
//! command today) was masked by `norte_help::parse_untrusted`. What this module
//! must mask itself is the only text that arrives from neither: the arguments
//! the user typed, echoed back in an error — see `main`'s `Help` arm.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::process::ExitCode;

use norte_frontend::keymap::{
    Availability as KeyAvailability, Effective, KeymapFile, Screen, presets,
};
use norte_frontend::keysheet;
use norte_help::{Availability, Block, Callout, ChordResolver, CommandText, Lang, Span, Topic};

/// Id of the synthetic keyboard page, the one entry of this command that is
/// GENERATED rather than written: its body is the effective keymap, so
/// rebinding a key rewrites the page.
pub const KEYS_ID: &str = "keys";

/// Screens the cheatsheet covers, with the Fluent id of each section title.
///
/// A fourth `Screen` must be added here as well; the arrays are separate from
/// `norte_frontend::keymap::Screen` because the CLI names its sections in the
/// order a reader learns them, not in the enum's order.
const SCREENS: [(Screen, &str); 3] = [
    (Screen::Browse, "help-section-browse"),
    (Screen::Viewer, "help-section-viewer"),
    (Screen::Dialog, "help-section-dialog"),
];

/// Width in CHARS of the chord column of the cheatsheet and of the command
/// rows.
const CHORD_COLUMN: usize = 14;

/// What a command with no key bound to it shows in the chord column.
const NO_CHORD: &str = "—";

/// The CLI's [`ChordResolver`].
///
/// Availability is the one answer that differs from the other two frontends,
/// and it is a fact about this process rather than a shortcut: there is no
/// pane, no selection, no connection and no plugin catalogue here, so every
/// impediment the shared table knows how to report is unknowable. Dimming a
/// row would be a claim; answering `Available` is the absence of one, and it is
/// also what `norte_frontend::availability::verdict` does when it has no fact
/// to go on.
///
/// # Two questions, and this type answers only one of them
///
/// [`norte_help::Availability`] — what [`Self::availability`] returns — is
/// about RIGHT NOW: is there a selection, is the pane read-only, is the
/// connection up. This process has no such state, hence `Available`.
///
/// The keyboard page answers the OTHER question, added by K3b: has norte
/// BUILT the command this key is bound to — a fact about the shared catalogue
/// (`Status::Live` vs `Status::Planned`), identical in every process. That one
/// arrives through [`norte_frontend::keysheet::SheetRow`] and never through
/// this trait; the two enums are unrelated types with unrelated meanings, and
/// merging them would make `norte help keys` claim to know things about a pane
/// it does not have.
///
/// It answers only the catalogue's half of that question. Which of the TUI and
/// the GUI implements a built command is a third question, and needs a
/// frontend's own command set — which this process does not have and must not
/// grow (rule 7). See [`JsonAvailability`].
pub struct CliChords {
    /// The effective keymap of each screen, kept per screen because the
    /// cheatsheet has to say WHICH screen a binding belongs to — a single
    /// merged map cannot, and merging is exactly what [`Self::chord`] wants.
    effectives: Vec<(Screen, Effective)>,
    /// Command to its painted chord, filled browse → viewer → dialog. First
    /// writer wins, so a command bound in several screens keeps the binding
    /// that fires first — the same precedence the TUI's resolver uses.
    chords: HashMap<String, String>,
    /// The language `label` answers in.
    lang: Lang,
}

impl CliChords {
    /// Builds from the user's REAL keymap: the preset named in `norte.toml`
    /// plus every `keymap.toml` layer, read exactly as `norte doctor` reads
    /// them.
    ///
    /// A layer that fails to parse is SKIPPED rather than fatal. This command
    /// prints documentation, and refusing to explain the app because a config
    /// file is broken is the least useful moment to be strict; `norte doctor`
    /// is the command that reports it, and the page then describes the keys the
    /// surviving layers give — which are the keys the app itself would use.
    #[must_use]
    pub fn from_config(lang: Lang) -> Self {
        let layers = norte_config::standard_layers();
        let preset = norte_config::load(&layers).map_or_else(
            |_| norte_config::DEFAULT_PRESET.to_owned(),
            |cfg| cfg.preset,
        );
        let mut sources = Vec::new();
        let mut layer_kfs = Vec::new();
        for (dir, kind) in &layers.dirs {
            if let Ok(Some(kf)) =
                norte_frontend::config::load_keymap_layer(dir, *kind, &mut sources)
            {
                layer_kfs.push(kf);
            }
        }
        Self::build(&preset, &layer_kfs, lang)
    }

    /// The same thing with no layers: the shape the tests pin.
    ///
    /// A test seam, and only that — `from_config` is what the command uses, and
    /// it already degrades to the default preset on its own.
    #[cfg(test)]
    #[must_use]
    pub fn from_preset(preset: &str, lang: Lang) -> Self {
        Self::build(preset, &[], lang)
    }

    fn build(preset: &str, layers: &[KeymapFile], lang: Lang) -> Self {
        // An unknown preset falls back to the default instead of failing: a
        // typo in `norte.toml` is `norte doctor`'s finding, and a page that
        // refuses to print is no way to learn that.
        let src = presets::source(preset)
            .or_else(|| presets::source(norte_config::DEFAULT_PRESET))
            .unwrap_or_default();
        let mut effectives = Vec::new();
        let mut chords: HashMap<String, String> = HashMap::new();
        if let Ok(preset_kf) = norte_frontend::keymap::parse_keymap(src) {
            for (screen, _) in SCREENS {
                // The command vocabulary comes from the PRESETS themselves: the
                // CLI has no `COMMANDS` table of its own, and inventing one
                // would be a third list to keep in step with two frontends.
                //
                // rust-reviewer MAJOR-4: but `preset_commands` returns every
                // name any bundled preset BINDS, which is by construction a
                // superset that includes the `Planned` ones. `check_binding`
                // never consults the catalogue for a name already in `known`,
                // so a `Planned` command came out `Here` and `ntc keys`
                // printed it as a working shortcut. The moment K2 ships
                // `alt+f1 → pane.select-drive`, the one artefact a Total
                // Commander migrant reads to learn the keys would be the one
                // that lies about them — the exact trap ADR 0043 exists to
                // close. Filter to what the catalogue calls Live.
                //
                // K3b keeps that intent and moves the mechanism. This filter
                // is what makes the availability TRUE — with the `Planned`
                // names out of `known`, the catalogue is consulted and answers
                // `NotBuilt` — and it is no longer what hides the row: since
                // K2b's four imported presets bind ~30 such commands
                // (#131..#140), `keys_page` renders them from
                // `keysheet::sheet`, marked and with their issue, instead of
                // walking the filtered `bindings()`. Never presenting an
                // unbuilt command as a working shortcut is still the rule; a
                // row that says "not built yet" does not break it, and a
                // missing row taught the migrant nothing at all.
                //
                // `chords` below still comes from `bindings()`, and must: it
                // answers "which key runs this command" for the prose pages
                // ({{cmd:…}} marks), where there is no room to explain, and a
                // chord printed there IS the claim that pressing it works.
                //
                // What this `known` set is NOT is a frontend's command list.
                // It is every name the bundled presets bind, minus the planned
                // ones, so `Availability::Here` here means "the catalogue
                // calls it Live" and `NotHere` means "no bundled preset binds
                // it" — neither is a statement about the TUI or the GUI. Both
                // the page and `JsonAvailability` are careful to claim only
                // the first; a fourth `COMMANDS` table in the CLI would break
                // rule 7 and would go stale the first time a frontend grew a
                // command.
                let known = norte_frontend::keymap::preset_commands(screen);
                let known: Vec<&str> = known
                    .iter()
                    .map(String::as_str)
                    .filter(|n| {
                        norte_frontend::keymap::catalogue::lookup(n)
                            .is_none_or(|d| d.status == norte_frontend::keymap::Status::Live)
                    })
                    .collect();
                let Ok(eff) = Effective::build_for(&preset_kf, layers, &known, screen) else {
                    continue;
                };
                for (seq, cmd) in eff.bindings() {
                    chords
                        .entry(cmd.to_owned())
                        .or_insert_with(|| norte_frontend::keymap::paint_chord(&seq));
                }
                effectives.push((screen, eff));
            }
        }
        Self {
            effectives,
            chords,
            lang,
        }
    }

    /// Fluent id of a command's short label: `dialog.*` verbs live in
    /// `dialog-cmd-*` and everything else in `help-cmd-*`.
    ///
    /// Routed by PREFIX and not by which list the caller is walking, because
    /// the two do not agree: the `dialog` effective merges the preset's
    /// `[global]` section, so `app.quit` turns up while rendering the dialog
    /// section, and asking `dialog-cmd-*` for it finds nothing.
    /// The `dialog.` PREFIX is stripped before dashing, which the `help-cmd-*`
    /// side is not: the catalogue spells them `dialog-cmd-confirm`, not
    /// `dialog-cmd-dialog-confirm` (`norte_tui::keymap::dialog_hint_id` is the
    /// shape this mirrors). Getting it wrong is silent — `t_in` answers a
    /// missing message with the id — so the cheatsheet printed a column of raw
    /// `dialog-cmd-dialog-…` ids at the reader.
    fn label_id(command: &str) -> String {
        match command.strip_prefix("dialog.") {
            Some(verb) => format!("dialog-cmd-{}", verb.replace('.', "-")),
            None => format!("help-cmd-{}", command.replace('.', "-")),
        }
    }
}

impl ChordResolver for CliChords {
    fn chord(&self, command: &str) -> Option<String> {
        self.chords.get(command).cloned()
    }

    /// The catalogue's short label, or an EMPTY string on a miss.
    ///
    /// Blank is the contract: `norte_i18n::t_in` answers a missing message with
    /// the id itself, so returning it would print `help-cmd-…` at the reader
    /// and stop `norte_help::render_command`'s fallback chain from ever naming
    /// the command. The miss is detected by testing for that echo, which IS the
    /// failure mode, so the check cannot drift out of agreement with it.
    fn label(&self, command: &str) -> String {
        let id = Self::label_id(command);
        let text = norte_i18n::t_in(self.lang, &id);
        if text == id { String::new() } else { text }
    }

    fn availability(&self, _command: &str) -> Availability {
        Availability::Available
    }
}

/// Pads `text` to `col` chars, or nothing when it is already wider.
fn pad_to(text: &str, col: usize) -> String {
    " ".repeat(col.saturating_sub(text.chars().count()))
}

/// One page as plain text: title, rule, provenance (for a plugin page), blocks,
/// runnable rows, `see also`.
///
/// The ORDER is the TUI's and the GUI's, so one page reads the same on three
/// surfaces. Nothing is wrapped: the terminal wraps on its own, and a hard wrap
/// is a lie once the output is a pipe into something that is not one.
#[must_use]
pub fn render_topic(topic: &Topic, lang: Lang, r: &(impl ChordResolver + ?Sized)) -> String {
    let mut out = String::new();
    out.push_str(&topic.title);
    out.push('\n');
    out.push_str(&"─".repeat(topic.title.chars().count()));
    out.push('\n');

    if let Some(badge) = plugin_badge(topic, lang) {
        out.push_str(&badge);
        out.push('\n');
    }

    for block in &topic.blocks {
        out.push('\n');
        out.push_str(&render_block(block, lang, r));
    }

    let rows = norte_help::rows_of(topic, r);
    if !rows.is_empty() {
        out.push('\n');
        for row in &rows {
            let chord = row.chord.as_deref().unwrap_or(NO_CHORD);
            let _ = writeln!(
                out,
                "  {chord}{} {}",
                pad_to(chord, CHORD_COLUMN),
                row.label
            );
        }
    }

    if !topic.see_also.is_empty() {
        // The TITLE of each linked page, not its id: the index and the sidebar
        // of the other frontends name it that way, and a reader who follows the
        // link has to land somewhere they recognise. An id the corpus cannot
        // resolve keeps the id — a dangling link is a corpus defect
        // (`norte_help::check_corpus` catches it) and blanking it would hide
        // that from whoever is reading the page.
        let names: Vec<String> = topic
            .see_also
            .iter()
            .map(|id| {
                norte_help::topic(lang, id.as_str())
                    .map_or_else(|| id.to_string(), |t| t.title.clone())
            })
            .collect();
        let _ = writeln!(
            out,
            "\n{}: {}",
            norte_i18n::t_in(lang, "help-see-also"),
            names.join(", ")
        );
    }

    out
}

/// One block of the closed vocabulary of [`Block`].
fn render_block(block: &Block, lang: Lang, r: &(impl ChordResolver + ?Sized)) -> String {
    match block {
        // No `#` and no indent: the corpus nests three deep at most, and a
        // heading in a stream is a line of its own with nothing else on it.
        Block::Heading { text, .. } => format!("{text}\n"),
        Block::Paragraph(spans) => format!("{}\n", inline(spans, lang, r)),
        Block::Bullets(items) => items.iter().fold(String::new(), |mut acc, item| {
            let _ = writeln!(acc, "  • {}", inline(item, lang, r));
            acc
        }),
        // NEVER wrapped and never re-indented beyond the two spaces: a wrapped
        // code line is a lie about what to type.
        Block::Code { text, .. } => text.lines().fold(String::new(), |mut acc, l| {
            let _ = writeln!(acc, "  {l}");
            acc
        }),
        Block::Table { header, rows } => table(header, rows),
        Block::Callout { kind, spans } => {
            // ASCII words rather than the TUI's `ℹ`/`⚠`/`💡`: this stream may be
            // read by something that is not a terminal, and the glyphs are the
            // part of that renderer that depends on one.
            let key = match kind {
                Callout::Note => "help-callout-note",
                Callout::Warn => "help-callout-warn",
                Callout::Tip => "help-callout-tip",
            };
            format!(
                "  {}: {}\n",
                norte_i18n::t_in(lang, key),
                inline(spans, lang, r)
            )
        }
    }
}

/// A run of spans as one string.
///
/// The `{{cmd:…}}` arm goes through `render_command` rather than flattening,
/// because a chord and a name are different things to a reader: the first is
/// something to press. Without a style to tell them apart, the CLI keeps the
/// distinction where it can — a chord prints as the key, a name as prose.
fn inline(spans: &[Span], lang: Lang, r: &(impl ChordResolver + ?Sized)) -> String {
    spans
        .iter()
        .map(|span| match span {
            Span::CommandRef(c) => match norte_help::render_command(c, r) {
                CommandText::Chord(k) | CommandText::Name(k) => k,
            },
            // The linked page's TITLE, for the reason the `see_also` arm gives.
            Span::TopicLink(id) => norte_help::topic(lang, id.as_str())
                .map_or_else(|| id.to_string(), |t| t.title.clone()),
            other => norte_help::render_span(other, r),
        })
        .collect()
}

/// Header, a rule, then the rows, in columns as wide as their widest cell.
///
/// Cells are read by ZIPPING each row against the header rather than indexing
/// by position: the parser normalises rows to `header.len()`, and relying on a
/// contract must not mean panicking when it changes — these rows come from a
/// `split` over text that, for a plugin page, somebody else wrote.
fn table(header: &[String], rows: &[Vec<String>]) -> String {
    if header.is_empty() {
        return String::new();
    }
    let mut widths: Vec<usize> = header.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if let Some(w) = widths.get_mut(i) {
                *w = (*w).max(cell.chars().count());
            }
        }
    }
    let line = |cells: &[String]| -> String {
        let mut s = String::from("  ");
        for (cell, w) in cells.iter().zip(&widths) {
            s.push_str(cell);
            s.push_str(&pad_to(cell, *w + 2));
        }
        format!("{}\n", s.trim_end())
    };
    let mut out = line(header);
    let _ = writeln!(
        out,
        "  {}",
        widths
            .iter()
            .map(|w| "─".repeat(*w))
            .collect::<Vec<_>>()
            .join("  ")
    );
    for row in rows {
        out.push_str(&line(row));
    }
    out
}

/// The provenance line of a PLUGIN page, `None` for something norte wrote.
///
/// UNCONDITIONAL for every plugin page, publisher or no publisher, flags or no
/// flags. A page that can pass for norte's own prose is a page that can tell
/// the reader approving an extension is safe, and the first segment is constant
/// so its absence never teaches the opposite of the truth. (Plugin pages are
/// not reachable from this command today; the renderer takes a `Topic`, and
/// this is not the place to forget.)
///
/// The assembly is [`norte_frontend::help_badge::plugin_badge`], shared with the
/// two windowed frontends since H3h. The clamp it applies to the publisher is
/// not about a terminal — it is about a third-party string never deciding
/// whether the host's own flags are visible — so it belongs to all three, and
/// this command is where an unbounded line would be piped somewhere else.
fn plugin_badge(topic: &Topic, lang: Lang) -> Option<String> {
    let norte_help::Origin::Plugin {
        publisher,
        truncated,
        lossy,
        ..
    } = &topic.origin
    else {
        return None;
    };
    norte_frontend::help_badge::plugin_badge(publisher.as_deref(), *truncated, *lossy, lang)
}

/// The keyboard cheatsheet: every binding of every screen with its catalogue
/// description, in real precedence order (what the key DOES, not what the
/// preset says).
///
/// Generated, never a maintained list — a rebind rewrites this page. It
/// describes the keys of the interactive frontends, which is what a keymap IS:
/// shared config that this command can read without running either of them.
///
/// Since K3b it prints the keys norte has NOT BUILT as well, in their place in
/// key order, each with the reason and the issue tracking it — see
/// [`norte_frontend::keysheet`], whose rows the three surfaces now share. This
/// stream cannot dim anything, so the row carries the whole answer as words.
///
/// "Not built" is the only unavailability this command can honestly report,
/// and the page says so once under its title: whether a BUILT command is
/// implemented by the TUI or by the GUI is a question about a frontend, and
/// this process is neither (see [`JsonAvailability`], and
/// [`CliChords`]'s "two questions").
#[must_use]
pub fn keys_page(chords: &CliChords, lang: Lang) -> String {
    let mut out = format!("{}\n\n", norte_i18n::t_in(lang, "help-topic-keys"));
    let _ = writeln!(out, "{}\n", norte_i18n::t_in(lang, "keys-page-note"));
    let rows = keysheet::sheet(&chords.effectives);
    for (screen, title_id) in SCREENS {
        if !chords.effectives.iter().any(|(s, _)| *s == screen) {
            continue;
        }
        let _ = writeln!(out, "── {} ──", norte_i18n::t_in(lang, title_id));
        if screen == Screen::Dialog {
            // #113's note, kept: each overlay supports its own SUBSET of these
            // verbs, and a flat list without it reads as a promise.
            let _ = writeln!(out, "  {}", norte_i18n::t_in(lang, "help-dialog-note"));
        }
        for row in rows.iter().filter(|r| r.screen == screen) {
            // The shared label router, with its fallback to the command NAME:
            // an unbuilt command has no help text — nobody writes help for
            // something that does not exist — and `t_in` answers a missing
            // message with the id, which would print `help-cmd-pane-pack` at
            // the reader.
            let label = norte_frontend::whichkey::command_label(&row.command, lang);
            // ONLY `NotBuilt` earns a suffix. `NotHere` means "absent from the
            // command set this `Effective` was built with", and this command's
            // set is the bundled presets' vocabulary, not a frontend's — so
            // here it means "no bundled preset binds it", which says nothing
            // about availability and must not be printed as if it did. See
            // `JsonAvailability`, which collapses the same two states for the
            // same reason.
            let why = match row.avail {
                KeyAvailability::NotBuilt { .. } => {
                    norte_frontend::keymap::short_unavailable_message(row.avail, lang)
                }
                KeyAvailability::Here | KeyAvailability::NotHere => String::new(),
            };
            let _ = if why.is_empty() {
                writeln!(
                    out,
                    "  {}{} {label}",
                    row.chord,
                    pad_to(&row.chord, CHORD_COLUMN)
                )
            } else {
                writeln!(
                    out,
                    "  {}{} {label} — {why}",
                    row.chord,
                    pad_to(&row.chord, CHORD_COLUMN)
                )
            };
        }
        out.push('\n');
    }
    out
}

/// Every page: id and title, one per line.
///
/// The synthetic [`KEYS_ID`] page is listed with the others because it is
/// reachable the same way (`norte help keys`), and a list that omits a
/// reachable page is a list the reader cannot use to find things.
#[must_use]
pub fn list_page(lang: Lang) -> String {
    let mut out = String::new();
    for topic in norte_help::topics(lang) {
        let id = topic.id.as_str();
        let _ = writeln!(out, "{id}{} {}", pad_to(id, 20), topic.title);
    }
    let _ = writeln!(
        out,
        "{KEYS_ID}{} {}",
        pad_to(KEYS_ID, 20),
        norte_i18n::t_in(lang, "help-topic-keys")
    );
    out
}

/// One search hit.
#[derive(Debug)]
pub struct Hit {
    /// Page id, to pass back to `norte help <id>`.
    pub id: String,
    /// The first line of the page that matched — the answer to "why is this
    /// here", which a list of bare ids does not give.
    pub line: String,
}

/// Pages whose title, tags, command ids or body match `query`.
///
/// Case- and accent-insensitive through `norte_frontend::nav::fold`, the same
/// fold the palette and the quick search use, so "what counts as a match" has
/// one definition in the app. A substring match and nothing cleverer: ranking
/// is explicitly out of scope in the design.
#[must_use]
pub fn search_pages(lang: Lang, query: &str, r: &(impl ChordResolver + ?Sized)) -> Vec<Hit> {
    let needle = norte_frontend::nav::fold(query.as_bytes());
    if needle.is_empty() {
        return Vec::new();
    }
    let mut hits = Vec::new();
    for topic in norte_help::topics(lang) {
        let body = render_topic(topic, lang, r);
        let haystack = format!(
            "{} {} {} {}",
            topic.title,
            topic.tags.join(" "),
            topic.commands.join(" "),
            body
        );
        if !norte_frontend::nav::fold(haystack.as_bytes()).contains(&needle) {
            continue;
        }
        // The line that matched, or the title when the hit came from a tag or a
        // command id the prose never spells.
        let line = body
            .lines()
            .find(|l| norte_frontend::nav::fold(l.as_bytes()).contains(&needle))
            .unwrap_or(topic.title.as_str())
            .trim()
            .to_owned();
        hits.push(Hit {
            id: topic.id.as_str().to_owned(),
            line,
        });
    }
    hits
}

/// The JSON shape of `norte help --json`.
///
/// A PROJECTION and not the model: `norte_help::Block` is an internal
/// vocabulary that will grow, and a consumer wants the prose. Each topic
/// therefore carries the ids and the RENDERED text of its page — which is also
/// what makes the golden readable in a diff.
///
/// `version` is first and manual: this is a consumed contract, and the moment
/// to have a discriminator is before somebody needs one.
#[derive(serde::Serialize)]
struct JsonDoc {
    version: u32,
    lang: &'static str,
    topics: Vec<JsonTopic>,
}

/// One page in [`JsonDoc`].
#[derive(serde::Serialize)]
struct JsonTopic {
    id: String,
    title: String,
    tags: Vec<String>,
    context: Vec<String>,
    see_also: Vec<String>,
    commands: Vec<JsonRow>,
    /// The keyboard rows, on the synthetic [`KEYS_ID`] page and nowhere else —
    /// hence absent, rather than an empty array, on every prose page (which
    /// keeps them byte-identical to v1). `Some(vec![])` and `None` are
    /// different answers on purpose: a keyboard page whose every layer failed
    /// to build has no keys, and must not be mistaken for a page that never
    /// had any. Structured, because the alternative is a consumer parsing
    /// [`Self::text`] with a regex to find out whether a key works.
    #[serde(skip_serializing_if = "Option::is_none")]
    keys: Option<Vec<JsonKey>>,
    text: String,
}

/// One key of the keyboard page (v2).
#[derive(serde::Serialize)]
struct JsonKey {
    /// `browse` | `viewer` | `dialog`: the flat list stays groupable.
    screen: &'static str,
    /// The sequence as a reader presses it, painted and masked.
    chord: String,
    command: String,
    /// Translated, falling back to [`Self::command`] — an unbuilt command has
    /// no help text.
    label: String,
    availability: JsonAvailability,
}

/// Whether norte has BUILT the command a key is bound to — the catalogue's
/// question, not `norte_help::Availability` (which is about the state of a
/// running app and is unknowable here; see [`CliChords`]).
///
/// **Two states, not three, and that is a limit of this process rather than a
/// simplification.** `norte_frontend::keymap::Availability` also distinguishes
/// `NotHere` — built, but not by THIS frontend — and that distinction needs a
/// frontend's command set to make. This command has none: it feeds
/// `Effective::build_for` the bundled presets' own vocabulary, so `Here` there
/// means "the catalogue calls it Live" and `NotHere` means "no bundled preset
/// binds it", neither of which is a claim about the TUI or the GUI. Reporting
/// them as `here`/`not-here` would tell an agent that `Alt+Left` goes back in
/// the GUI, which it does not — the exact class of lie the `MAJOR-4` filter in
/// [`CliChords::build`] exists to close. So both collapse to `built`, and the
/// page says once that which frontend implements a built key is not knowable
/// from here.
///
/// Tagged rather than a bare string so the two fields that exist for only one
/// variant travel with it: an agent reading `not-built` gets the issue to
/// point a human at. Every wire word is spelled out with `rename`, for the
/// reason [`screen_key`] is a hand-written table: v2 freezes these words, and
/// a Rust identifier renamed while tidying must not silently rename a field of
/// a consumed contract.
#[derive(serde::Serialize)]
#[serde(tag = "state")]
enum JsonAvailability {
    /// `Status::Live`: norte has built it. Which frontend implements it is a
    /// question this process cannot answer.
    #[serde(rename = "built")]
    Built,
    /// `Status::Planned`: norte has not built it, and the key does nothing in
    /// any frontend.
    #[serde(rename = "not-built")]
    NotBuilt {
        /// Translated short reason (the catalogue holds a Fluent id).
        reason: String,
        /// The issue tracking it. Never invented, never zero.
        issue: u32,
    },
}

impl JsonAvailability {
    fn of(avail: KeyAvailability, lang: Lang) -> Self {
        match avail {
            // `NotHere` collapses into `built` deliberately — see the type's
            // doc. It is a fact about the command set this `Effective` was
            // built with, and this command's set is not a frontend's.
            KeyAvailability::Here | KeyAvailability::NotHere => Self::Built,
            KeyAvailability::NotBuilt { reason, issue } => Self::NotBuilt {
                reason: norte_i18n::t_in(lang, reason),
                issue,
            },
        }
    }
}

/// The `screen` string of [`JsonKey`]. A stable wire word, spelled here rather
/// than derived from `Debug`: a rename of the enum variant must not silently
/// rename a field of a consumed contract.
const fn screen_key(screen: Screen) -> &'static str {
    match screen {
        Screen::Browse => "browse",
        Screen::Viewer => "viewer",
        Screen::Dialog => "dialog",
    }
}

/// One runnable row in [`JsonTopic`].
#[derive(serde::Serialize)]
struct JsonRow {
    command: String,
    label: String,
    /// `null` when this reader has no key bound to it — never a made-up chord.
    chord: Option<String>,
}

/// Version of the `--json` shape. Bump it when a field's MEANING changes;
/// adding a field is additive and does not.
///
/// - **1** — the original H3g shape.
/// - **2** (K3b) — the `keys` page changed what it CONTAINS, which no added
///   field can describe. Until now every key it listed was a key that worked,
///   because the ones this build cannot run were filtered out of the page
///   entirely; now they are listed too, in their place in key order, and a
///   consumer that read "every listed key works" from v1 would silently start
///   reading keys that do nothing. The `keys` array (structured rows, each
///   with its [`JsonAvailability`]) is what makes the distinction machine-
///   readable instead of a phrase inside `text`.
const JSON_VERSION: u32 = 2;

/// The pages `--json` emits, filtered to `only` when the caller named a page.
fn json_doc(lang: Lang, chords: &CliChords, only: Option<&str>) -> JsonDoc {
    let mut topics: Vec<JsonTopic> = norte_help::topics(lang)
        .iter()
        .filter(|t| only.is_none_or(|id| t.id.as_str() == id))
        .map(|t| JsonTopic {
            id: t.id.as_str().to_owned(),
            title: t.title.clone(),
            tags: t.tags.clone(),
            context: t.context.clone(),
            see_also: t.see_also.iter().map(ToString::to_string).collect(),
            commands: norte_help::rows_of(t, chords)
                .into_iter()
                .map(|row| JsonRow {
                    command: row.row.command,
                    label: row.label,
                    chord: row.chord,
                })
                .collect(),
            keys: None,
            text: render_topic(t, lang, chords),
        })
        .collect();
    // The keyboard page is a page: it is listed, it is reachable by id, and an
    // agent asking for "everything norte documents" must not have to know it is
    // generated. It has no commands — a chord is not something Enter runs —
    // and since v2 it has `keys` instead, which is what it always was.
    if only.is_none_or(|id| id == KEYS_ID) {
        topics.push(JsonTopic {
            id: KEYS_ID.to_owned(),
            title: norte_i18n::t_in(lang, "help-topic-keys"),
            tags: vec![KEYS_ID.to_owned()],
            context: Vec::new(),
            see_also: Vec::new(),
            commands: Vec::new(),
            keys: Some(
                keysheet::sheet(&chords.effectives)
                    .into_iter()
                    .map(|row| JsonKey {
                        screen: screen_key(row.screen),
                        chord: row.chord,
                        label: norte_frontend::whichkey::command_label(&row.command, lang),
                        command: row.command,
                        availability: JsonAvailability::of(row.avail, lang),
                    })
                    .collect(),
            ),
            text: keys_page(chords, lang),
        });
    }
    JsonDoc {
        version: JSON_VERSION,
        lang: match lang {
            Lang::Es => "es",
            Lang::En => "en",
        },
        topics,
    }
}

/// Writes `text` to stdout, treating a closed pipe as a normal end.
///
/// `println!` PANICS on `BrokenPipe`, so `norte help | head -5` printed a Rust
/// backtrace at the reader — a pitfall this repo has already paid for once. A
/// reader who walked away is not an error: nothing more is printed and the exit
/// code stays 0.
fn write_out(text: &str) -> ExitCode {
    let mut out = std::io::stdout().lock();
    match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

/// Echoes a caller-supplied string back into an error line, masked and capped.
///
/// The argument is not third-party in the plugin sense, but an error line is
/// exactly where a pasted string carrying an `ESC` ends up, and this one goes
/// straight to a terminal.
fn echo(arg: &str) -> String {
    let mut chars = arg.chars();
    let head: String = chars.by_ref().take(64).collect();
    let overflowed = chars.next().is_some();
    let mut out = norte_encoding::mask_terminal_hazards(&head);
    if overflowed {
        out.push('…');
    }
    out
}

/// `norte help [topic] [--list] [--search Q] [--json]`.
///
/// Exit codes follow `grep` where the question is the same: 1 means "nothing
/// matched", on stderr, with stdout left empty so a pipeline sees no page.
pub fn run(topic: Option<&str>, list: bool, search: Option<&str>, json: bool) -> ExitCode {
    let lang = norte_i18n::active();
    let chords = CliChords::from_config(lang);

    if let Some(query) = search {
        let hits = search_pages(lang, query, &chords);
        if hits.is_empty() {
            eprintln!(
                "{}",
                norte_i18n::ta_in(lang, "cli-help-no-matches", &[("query", &echo(query))])
            );
            return ExitCode::FAILURE;
        }
        if json {
            let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
            // ONE document, then filtered. It used to be built twice — once
            // for `lang`, once for `topics` — which since K3b means rendering
            // every page and walking the whole keyboard sheet a second time.
            let all = json_doc(lang, &chords, None);
            let doc = JsonDoc {
                version: JSON_VERSION,
                lang: all.lang,
                topics: all
                    .topics
                    .into_iter()
                    .filter(|t| ids.contains(&t.id.as_str()))
                    .collect(),
            };
            return write_json(&doc);
        }
        let text = hits.iter().fold(String::new(), |mut acc, h| {
            let _ = writeln!(acc, "{}{} {}", h.id, pad_to(&h.id, 20), h.line);
            acc
        });
        return write_out(&text);
    }

    if list {
        return write_out(&list_page(lang));
    }

    if json {
        // A named page filters the dump; no name dumps everything. An unknown
        // name is still an error rather than an empty array — a consumer asking
        // for a page that does not exist has a bug, and silence hides it.
        if let Some(id) = topic
            && json_doc(lang, &chords, Some(id)).topics.is_empty()
        {
            return unknown(lang, id);
        }
        return write_json(&json_doc(lang, &chords, topic));
    }

    match topic {
        None => {
            let index = norte_help::topic(lang, "index");
            index.map_or_else(
                // A corpus with no index is a build defect, not a user error;
                // the list is still something to read.
                || write_out(&list_page(lang)),
                |t| write_out(&render_topic(t, lang, &chords)),
            )
        }
        Some(id) if id == KEYS_ID => write_out(&keys_page(&chords, lang)),
        Some(id) => match norte_help::topic(lang, id) {
            Some(t) => write_out(&render_topic(t, lang, &chords)),
            None => unknown(lang, id),
        },
    }
}

/// One line on stderr naming the way out, and exit 1.
fn unknown(lang: Lang, id: &str) -> ExitCode {
    eprintln!(
        "{}",
        norte_i18n::ta_in(lang, "cli-help-unknown-topic", &[("id", &echo(id))])
    );
    ExitCode::FAILURE
}

/// Serializes `doc` pretty-printed, with the same pipe treatment as the text
/// output.
fn write_json(doc: &JsonDoc) -> ExitCode {
    match serde_json::to_string_pretty(doc) {
        Ok(s) => write_out(&format!("{s}\n")),
        // Unreachable with these types (no maps with non-string keys, no
        // non-finite floats), and not an `expect`: a panic in a documentation
        // command is worse than an exit code.
        Err(_) => ExitCode::FAILURE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed;
    impl ChordResolver for Fixed {
        fn chord(&self, c: &str) -> Option<String> {
            (c == "pane.copy").then(|| "F5".to_owned())
        }
        fn label(&self, c: &str) -> String {
            format!("label of {c}")
        }
        fn availability(&self, _c: &str) -> Availability {
            Availability::Available
        }
    }

    #[test]
    fn el_acorde_es_el_del_usuario_y_un_comando_sin_binding_no_inventa_tecla() {
        let chords = CliChords::from_preset("orthodox", Lang::En);
        assert_eq!(chords.chord("pane.copy").as_deref(), Some("F5"));
        assert_eq!(chords.chord("no.such.command"), None);
    }

    #[test]
    fn la_etiqueta_sale_del_catalogo_y_falla_a_vacio() {
        let chords = CliChords::from_preset("orthodox", Lang::En);
        assert_eq!(
            chords.label("app.quit"),
            norte_i18n::t_in(Lang::En, "help-cmd-app-quit")
        );
        assert_eq!(
            chords.label("no.such.command"),
            "",
            "a miss is BLANK: returning the id would print `help-cmd-…` at the reader"
        );
    }

    #[test]
    fn en_la_cli_todo_esta_disponible() {
        let chords = CliChords::from_preset("orthodox", Lang::En);
        assert!(chords.availability("pane.copy").is_available());
        assert!(
            chords.availability("plugin:acme.ftp:greet").is_available(),
            "no pane and no catalogue here: dimming would be a claim about a context \
             this process does not have"
        );
    }

    #[test]
    fn un_preset_desconocido_cae_al_default_en_vez_de_fallar() {
        let chords = CliChords::from_preset("no-such-preset", Lang::En);
        assert!(
            chords.chord("pane.copy").is_some(),
            "printing a page is not the moment to refuse over a config typo — \
             `norte doctor` is what reports that"
        );
    }

    #[test]
    fn una_pagina_se_imprime_con_titulo_cuerpo_y_filas() {
        let topic = norte_help::topic(Lang::En, "copying").expect("corpus topic");
        let out = render_topic(topic, Lang::En, &Fixed);
        assert!(out.starts_with(&topic.title), "the title leads: {out:.60}");
        assert!(out.contains("F5"), "a {{cmd:}} mark became a chord");
        for command in &topic.commands {
            assert!(
                out.contains(&format!("label of {command}")),
                "row for {command} is missing"
            );
        }
    }

    #[test]
    fn los_enlaces_se_nombran_por_su_titulo_no_por_su_id() {
        let topic = norte_help::topic(Lang::En, "copying").expect("corpus topic");
        assert!(
            !topic.see_also.is_empty(),
            "this test needs a page with links"
        );
        let out = render_topic(topic, Lang::En, &Fixed);
        for id in &topic.see_also {
            let linked = norte_help::topic(Lang::En, id.as_str()).expect("live link");
            assert!(
                out.contains(&linked.title),
                "link named by id instead of title"
            );
        }
    }

    #[test]
    fn no_se_emite_ningun_peligro_de_terminal() {
        // The whole shipped corpus, both locales: this command writes straight
        // to a terminal, so an ESC in a code fence would be an ANSI injection.
        for lang in [Lang::Es, Lang::En] {
            for topic in norte_help::topics(lang) {
                let out = render_topic(topic, lang, &Fixed);
                // Line by line, because `is_terminal_hazard` counts `\n` — it
                // is a C0 control, and a renderer that emits lines emits it by
                // definition. Everything else it flags (ESC, the other
                // controls, bidi overrides, invisible format characters) is
                // still caught, which is the point of the sweep.
                for line in out.lines() {
                    assert!(
                        !line.chars().any(norte_encoding::is_terminal_hazard),
                        "{}/{lang:?} emits a terminal hazard: {line:?}",
                        topic.id.as_str()
                    );
                }
            }
        }
    }

    #[test]
    fn una_pagina_de_plugin_declara_su_procedencia() {
        // Not reachable from this command today, but the renderer takes a
        // `Topic` and must not be the place that forgets.
        let parsed = norte_help::parse_untrusted(b"body", "acme.ftp", None);
        let out = render_topic(&parsed.topic, Lang::En, &Fixed);
        assert!(out.contains(&norte_i18n::t_in(Lang::En, "help-plugin-origin")));
    }

    #[test]
    fn la_hoja_de_teclado_cubre_las_tres_pantallas() {
        let chords = CliChords::from_preset("orthodox", Lang::En);
        let out = keys_page(&chords, Lang::En);
        for section in [
            "help-section-browse",
            "help-section-viewer",
            "help-section-dialog",
        ] {
            assert!(
                out.contains(&norte_i18n::t_in(Lang::En, section)),
                "missing section {section}"
            );
        }
        assert!(out.contains("F5"));
        assert!(
            !out.contains("help-cmd-") && !out.contains("dialog-cmd-"),
            "untranslated id in the sheet"
        );
    }

    /// K3b: the rows K2b's imported presets made necessary. Total Commander's
    /// `Alt+F5` packs; norte does not pack yet. Before K3b the page simply had
    /// no such line, so the one artefact a migrant reads to learn the keys was
    /// silent about a third of the preset.
    #[test]
    fn una_tecla_no_construida_es_una_fila_que_dice_por_que() {
        let chords = CliChords::from_preset("total-commander", Lang::En);
        let out = keys_page(&chords, Lang::En);
        let row = out
            .lines()
            .find(|l| l.contains("Alt+F5"))
            .expect("the key is where a Total Commander migrant looks for it");
        assert!(
            row.contains("pane.pack"),
            "no help text exists for an unbuilt command, so the label is its NAME: {row}"
        );
        assert!(row.contains("132"), "the issue is the way out: {row}");
        assert!(
            row.contains(&norte_i18n::t_in(Lang::En, "keymap-reason-archive-write")),
            "the reason is TRANSLATED, not the catalogue's Fluent id: {row}"
        );
    }

    /// The v2 wire words, pinned as LITERALS. Nothing else pins them: the
    /// golden is generated from the default preset, which binds no planned
    /// command, so it contains `built` and nothing else. A rename of a Rust
    /// variant (`NotBuilt` → `Planned` is the natural tidy-up, since that is
    /// what the catalogue calls it) would otherwise silently retire a
    /// consumer's `state === "not-built"` branch with the whole suite green.
    #[test]
    fn las_palabras_del_contrato_v2_son_literales() {
        let json = |a: &JsonAvailability| serde_json::to_string(a).expect("serializa");
        assert_eq!(json(&JsonAvailability::Built), r#"{"state":"built"}"#);
        assert_eq!(
            json(&JsonAvailability::NotBuilt {
                reason: "writing archives".to_owned(),
                issue: 132,
            }),
            r#"{"state":"not-built","reason":"writing archives","issue":132}"#,
            "the issue is a NUMBER: an agent points a human at it"
        );
        assert_eq!(
            [Screen::Browse, Screen::Viewer, Screen::Dialog].map(screen_key),
            ["browse", "viewer", "dialog"]
        );
    }

    /// And a real `not-built` row reaches the wire end to end — the golden
    /// cannot show one, because the default preset binds no planned command.
    #[test]
    fn una_fila_no_construida_llega_al_json_con_su_issue() {
        let chords = CliChords::from_preset("total-commander", Lang::En);
        let doc = json_doc(Lang::En, &chords, Some(KEYS_ID));
        let keys = doc.topics[0].keys.as_ref().expect("the keyboard page");
        let pack = keys
            .iter()
            .find(|k| k.command == "pane.pack")
            .expect("total-commander binds pane.pack");
        let json = serde_json::to_value(&pack.availability).expect("serializa");
        assert_eq!(json["state"], "not-built");
        assert_eq!(json["issue"], 132);
        assert_eq!(
            json["reason"],
            norte_i18n::t_in(Lang::En, "keymap-reason-archive-write"),
            "translated prose, not the catalogue's Fluent id"
        );
        assert!(
            keys.iter().any(
                |k| serde_json::to_value(&k.availability).expect("serializa")["state"] == "built"
            ),
            "and the built ones are still built"
        );
    }

    /// Interleaved in key order, never a section of leftovers at the bottom:
    /// the sheet answers "what does this key do", and a reader scanning the
    /// F-keys must find the unavailable one between its neighbours.
    #[test]
    fn las_filas_no_disponibles_no_se_agrupan_al_final() {
        let chords = CliChords::from_preset("total-commander", Lang::En);
        let out = keys_page(&chords, Lang::En);
        let lines: Vec<&str> = out.lines().collect();
        let pack = lines
            .iter()
            .position(|l| l.contains("Alt+F5"))
            .expect("the pane.pack row");
        let viewer = lines
            .iter()
            .position(|l| l.contains(&norte_i18n::t_in(Lang::En, "help-section-viewer")))
            .expect("the viewer section");
        assert!(
            pack < viewer,
            "it belongs to the browse section it is bound in"
        );
        // `Ctrl+u` (pane.swap) is bound AFTER Alt+F5 in the preset and this
        // build runs it: an available row after an unavailable one is exactly
        // what a segregated sheet could not produce. (Painted lowercase — a
        // character token is the key as typed; only named keys are prettified.)
        let swap = lines
            .iter()
            .position(|l| l.contains("Ctrl+u"))
            .expect("the pane.swap row");
        assert!(
            pack < swap && swap < viewer,
            "the sheet is one list in key order, not two: pack={pack} swap={swap}"
        );
    }

    #[test]
    fn list_da_id_y_titulo_de_cada_pagina() {
        let out = list_page(Lang::En);
        for topic in norte_help::topics(Lang::En) {
            assert!(
                out.contains(topic.id.as_str()),
                "{} missing",
                topic.id.as_str()
            );
            assert!(out.contains(&topic.title));
        }
        assert!(out.contains(KEYS_ID), "the keyboard page is reachable too");
    }

    #[test]
    fn search_encuentra_por_titulo_cuerpo_comando_y_etiqueta() {
        let chords = CliChords::from_preset("orthodox", Lang::En);
        let hits = search_pages(Lang::En, "copy", &chords);
        assert!(
            hits.iter().any(|h| h.id == "copying"),
            "the copying page must match its own subject: {hits:?}"
        );
        // A command id matches even where the prose never spells it.
        let by_command = search_pages(Lang::En, "pane.copy", &chords);
        assert!(by_command.iter().any(|h| h.id == "copying"));
        // Case-insensitive, like every other filter in the app.
        assert_eq!(
            search_pages(Lang::En, "COPY", &chords).len(),
            search_pages(Lang::En, "copy", &chords).len()
        );
        assert!(search_pages(Lang::En, "zzzzz-no-such-word", &chords).is_empty());
        assert!(
            search_pages(Lang::En, "", &chords).is_empty(),
            "empty matches nothing, not everything"
        );
    }

    #[test]
    fn cada_hit_dice_por_que_coincidio() {
        let chords = CliChords::from_preset("orthodox", Lang::En);
        for hit in search_pages(Lang::En, "copy", &chords) {
            assert!(!hit.line.trim().is_empty(), "{} has a blank reason", hit.id);
        }
    }
}
