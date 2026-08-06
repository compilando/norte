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

use norte_frontend::keymap::{Effective, KeymapFile, Screen, presets};
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
                let known = norte_frontend::keymap::preset_commands(screen);
                let known: Vec<&str> = known.iter().map(String::as_str).collect();
                let Ok(eff) = Effective::build_for_subset(&preset_kf, layers, &known, screen)
                else {
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
/// `is_blank_id` and not `str::trim`: `"\u{3164}"` (HANGUL FILLER) is not
/// whitespace, so a trim-based check calls it a publisher and prints
/// `published by ` with nothing after it.
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
    let mut parts = vec![norte_i18n::t_in(lang, "help-plugin-origin")];
    if let Some(p) = publisher.as_deref().filter(|p| !norte_help::is_blank_id(p)) {
        parts.push(norte_i18n::ta_in(lang, "help-plugin-by", &[("who", p)]));
    }
    if *truncated {
        parts.push(norte_i18n::t_in(lang, "help-plugin-truncated"));
    }
    if *lossy {
        parts.push(norte_i18n::t_in(lang, "help-plugin-lossy"));
    }
    Some(parts.join(" · "))
}

/// The keyboard cheatsheet: every binding of every screen with its catalogue
/// description, in real precedence order (what the key DOES, not what the
/// preset says).
///
/// Generated, never a maintained list — a rebind rewrites this page. It
/// describes the keys of the interactive frontends, which is what a keymap IS:
/// shared config that this command can read without running either of them.
#[must_use]
pub fn keys_page(chords: &CliChords, lang: Lang) -> String {
    let mut out = format!("{}\n\n", norte_i18n::t_in(lang, "help-topic-keys"));
    for (screen, title_id) in SCREENS {
        let Some((_, eff)) = chords.effectives.iter().find(|(s, _)| *s == screen) else {
            continue;
        };
        let _ = writeln!(out, "── {} ──", norte_i18n::t_in(lang, title_id));
        if screen == Screen::Dialog {
            // #113's note, kept: each overlay supports its own SUBSET of these
            // verbs, and a flat list without it reads as a promise.
            let _ = writeln!(out, "  {}", norte_i18n::t_in(lang, "help-dialog-note"));
        }
        for (seq, cmd) in eff.bindings() {
            let seq = norte_frontend::keymap::paint_chord(&seq);
            let _ = writeln!(
                out,
                "  {seq}{} {}",
                pad_to(&seq, CHORD_COLUMN),
                norte_i18n::t_in(lang, &CliChords::label_id(cmd))
            );
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
    text: String,
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
const JSON_VERSION: u32 = 1;

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
            text: render_topic(t, lang, chords),
        })
        .collect();
    // The keyboard page is a page: it is listed, it is reachable by id, and an
    // agent asking for "everything norte documents" must not have to know it is
    // generated. It has no commands — a chord is not something Enter runs.
    if only.is_none_or(|id| id == KEYS_ID) {
        topics.push(JsonTopic {
            id: KEYS_ID.to_owned(),
            title: norte_i18n::t_in(lang, "help-topic-keys"),
            tags: vec![KEYS_ID.to_owned()],
            context: Vec::new(),
            see_also: Vec::new(),
            commands: Vec::new(),
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
            let doc = JsonDoc {
                version: JSON_VERSION,
                lang: json_doc(lang, &chords, None).lang,
                topics: json_doc(lang, &chords, None)
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
