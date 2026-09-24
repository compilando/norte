//! Painting a help topic in the TUI: [`norte_help::Topic`] → ratatui lines,
//! plus the map from each runnable row and link to the LINE it landed on.
//!
//! The map is the reason this returns a [`Rendered`] rather than a bare
//! `Vec<Line>`: `norte_frontend::help::HelpState` walks *actions* (commands
//! then `see_also` links) while the body scrolls in *lines*, so
//! `HelpState::reveal` needs someone to translate one into the other. Only the
//! painter knows, because only the painter knows how many lines a paragraph
//! wrapped into.
//!
//! # This module masks NOTHING
//!
//! That is a claim about its INPUTS, not an oversight, and it is only true of
//! a corpus that has been through the gate:
//!
//! - A BUILT-IN topic is trusted text, gated in two halves, and the halves are
//!   worth separating because they were once confused for one. IDS —
//!   `{{cmd:…}}` marks and `[[links]]` — are cross-checked by the documentation
//!   gate (`norte-tui/tests/help_gate.rs`) byte-exactly against the frontend's
//!   command vocabulary, so a spoofed id fails the build. PROSE — titles,
//!   headings, table cells, code blocks — is checked by nothing the parser
//!   does; what covers it is a sweep of the shipped corpus for terminal hazards
//!   (`norte-help/tests/corpus.rs`). Corpus files are edited by translators, so
//!   that sweep is the whole guarantee: without it a raw `ESC` in a code fence
//!   is an ANSI injection into the reader's terminal, and this module would
//!   paint it faithfully.
//! - A PLUGIN topic was already masked and bounded by
//!   [`norte_help::parse_untrusted`], which refuses what it could not paint
//!   rather than rewriting it.
//! - A CHORD was masked by the resolver — `crate::help::TuiChords::chord` goes
//!   through `crate::palette::first_chord`, which is where
//!   `norte_encoding::mask_terminal_hazards` runs (a project keymap layer can
//!   bind any codepoint and the chord is painted).
//!
//! A caller feeding this renderer a corpus that has NOT been through that gate
//! — a third-party topic set loaded at runtime, say — must mask before calling
//! in.
//!
//! # The truncation badge (H3e)
//!
//! [`norte_help::Origin::Plugin`] carries `truncated` and `lossy`, and
//! `norte_help::Parsed`'s rustdoc calls them "the UI badge: a reader must never
//! mistake a cut topic for a complete one". `plugin_badge` paints them, right
//! under the title — masking and bounding, which `parse_untrusted` does, is not
//! the same as SAYING the page was cut.
//!
//! A built-in topic has no badge and EVERY plugin topic has one, unconditionally
//! — see `plugin_badge` for the attack that made "unconditionally" load-bearing.
//! The line is the only thing on screen that tells third-party prose from ours,
//! so a plugin must not be able to make it disappear by declaring nothing.
//!
//! What this module does guarantee is the *geometry*: it never emits a
//! line wider than the width it was given (measured in terminal CELLS, not
//! chars — see [`render_block`]), so no wrapping decision here can push a
//! painted cell off the pane.

use norte_help::{Block, Callout, ChordResolver, CommandText, Lang, Span as HSpan, Topic};
use norte_theme::Role;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::theme::TuiTheme;

/// Placeholder in the chord column of a row nothing is bound to. The same `—`
/// the palette uses, so "no key" reads the same in both surfaces.
const NO_CHORD: &str = "—";

/// A rendered topic: the lines to paint, and where each action landed.
#[derive(Clone, Debug)]
pub struct Rendered<'a> {
    /// The body, one entry per painted line.
    pub lines: Vec<Line<'a>>,
    /// Line index of each action of `HelpState::actions`, in the same order
    /// (commands first, then `see_also` links).
    ///
    /// Strictly increasing, and every entry is a valid index into
    /// [`lines`](Self::lines): an action occupies EXACTLY one line, which is
    /// why rows and links are truncated rather than wrapped.
    pub action_lines: Vec<usize>,
    /// Line index of each heading of the page, in order: where `[` and `]`
    /// jump to. Strictly increasing, every entry a valid index into
    /// [`lines`](Self::lines).
    pub heading_lines: Vec<usize>,
}

/// Paints a whole topic: title, a rule, the blocks separated by a blank line,
/// the runnable rows and the `see_also` links.
///
/// `width` is the body's width in terminal cells. Everything wraps to it
/// except code blocks (see [`render_block`]).
///
/// `lang` is the corpus locale the topic came from, and is here for the
/// `see_also` rows alone: a link is painted with the TITLE of the page it
/// opens — the very string the sidebar shows for that row — and resolving an
/// id back to its topic needs the locale. A link the corpus cannot resolve
/// keeps its id, which is at least addressable; an empty row would not be.
///
/// ```
/// use norte_help::{Availability, ChordResolver, Lang, topic};
/// use norte_theme::{ColorDepth, Theme};
/// use norte_tui::help_render::render_topic;
/// use norte_tui::theme::TuiTheme;
///
/// struct Keys;
/// impl ChordResolver for Keys {
///     fn chord(&self, command: &str) -> Option<String> {
///         (command == "pane.copy").then(|| "f5".to_owned())
///     }
///     fn label(&self, command: &str) -> String {
///         command.to_owned()
///     }
///     fn availability(&self, _command: &str) -> Availability {
///         Availability::Available
///     }
/// }
///
/// let theme = TuiTheme::new(Theme::preset_default(), ColorDepth::Truecolor);
/// let copying = topic(Lang::En, "copying").expect("the `copying` topic");
/// let out = render_topic(copying, Lang::En, &Keys, 60, &theme);
///
/// // One action line per command and per link, in `HelpState::actions` order.
/// assert_eq!(
///     out.action_lines.len(),
///     copying.commands.len() + copying.links().len()
/// );
/// assert!(out.action_lines.iter().all(|&i| i < out.lines.len()));
///
/// // A `see_also` row is painted with the linked page's TITLE, not its id.
/// let linked = topic(Lang::En, copying.see_also[0].as_str()).expect("a live link");
/// let row = &out.lines[out.action_lines[copying.commands.len()]];
/// let text: String = row.spans.iter().map(|s| s.content.as_ref()).collect();
/// assert!(text.contains(&linked.title), "{text:?} names {:?}", linked.title);
/// ```
#[must_use]
pub fn render_topic<'a>(
    topic: &'a Topic,
    lang: Lang,
    r: &(impl ChordResolver + ?Sized),
    width: usize,
    theme: &TuiTheme,
) -> Rendered<'a> {
    let mut lines = vec![
        // `fit`ted like every other line this module emits. It was the one
        // exception, and it was safe only by accident: `crate::ui::draw_help`
        // renders the body with no `.wrap()`, so ratatui truncated an
        // over-wide title on its own. Adding `.wrap()` there — a plausible
        // change, the prose is already wrapped by hand — would have made a
        // 280-char plugin title occupy several lines, shifting every index in
        // `action_lines` below it so that the focused-row highlight and
        // `HelpState::reveal` both point at the wrong row. The module
        // guarantees its geometry; the guarantee has to hold for the title too.
        Line::from(Span::styled(
            fit(&topic.title, width),
            theme.role(Role::Title),
        )),
        Line::from(Span::styled(
            "─".repeat(width),
            theme.role(Role::BorderUnfocused),
        )),
    ];
    if let Some(badge) = plugin_badge(topic, lang) {
        // WRAPPED, not `fit`ted. The badge is the only thing on screen that
        // tells third-party prose from ours, and with a publisher and both
        // flags it runs past 60 cells — so cutting it drops the very segments
        // that carry the warning («…some bytes did not d…»), and drops them
        // exactly on the narrow terminals where a reader is least able to
        // guess what was there. Prose wraps here like all the other prose in
        // this module; the action map is built afterwards from `lines.len()`,
        // so the extra rows cost nothing but a line of scroll.
        lines.extend(wrap(
            &[(badge, theme.role(Role::Info))],
            width,
            "",
            "",
            theme.role(Role::Info),
        ));
    }

    let mut heading_lines = Vec::new();
    for (i, block) in topic.blocks.iter().enumerate() {
        lines.push(Line::default());
        // A heading opens a SECTION, and one blank line — the same one that
        // separates two paragraphs — says nothing about that. A second one
        // does. Never at the top of the body (the first block already sits
        // under the title's rule, which is separation enough) and never after
        // the last block: trailing blanks are scroll the reader has to pay
        // for.
        if i > 0 && matches!(block, Block::Heading { .. }) {
            lines.push(Line::default());
        }
        if matches!(block, Block::Heading { .. }) {
            heading_lines.push(lines.len());
        }
        lines.extend(render_block(block, lang, r, width, theme));
    }

    let rows = norte_help::rows_of(topic, r);
    // `links()`: the `see_also` and the prose's `[[links]]`, in the SAME
    // order as the model's actions — otherwise the cursor would point at one
    // row and Enter would follow another.
    let see_also = topic.links();
    let mut action_lines = Vec::with_capacity(rows.len() + see_also.len());

    if !rows.is_empty() {
        lines.push(Line::default());
        // One padded column for every chord, so the labels line up and the
        // reader's eye finds the keys without reading the prose again.
        let col = rows
            .iter()
            .map(|row| row.chord.as_deref().unwrap_or(NO_CHORD).width())
            .max()
            .unwrap_or(0);
        for row in &rows {
            action_lines.push(lines.len());
            lines.push(row_line(row, col, width, lang, theme));
        }
    }

    if !see_also.is_empty() {
        lines.push(Line::default());
        for id in &see_also {
            action_lines.push(lines.len());
            // The TITLE of the page the link opens, not its id: the sidebar
            // row for that same page says exactly this, and a reader who
            // follows the link must land somewhere they can recognise as the
            // place the row named. The `Role::Info` style is what says «this
            // is a link» — the wiki brackets were saying it a second time, in
            // a spelling nothing else in the UI uses.
            //
            // An id the corpus cannot resolve keeps the id: a dangling link
            // is a corpus defect (`norte_help::check_corpus` catches it) and
            // a blank row would hide it from whoever is reading the page.
            let label = norte_help::topic(lang, id.as_str())
                .map_or_else(|| id.to_string(), |t| t.title.clone());
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(fit(&label, width.saturating_sub(2)), theme.role(Role::Info)),
            ]));
        }
    }

    Rendered {
        lines,
        action_lines,
        heading_lines,
    }
}

/// The provenance line of a PLUGIN page, `None` only for something the host
/// wrote (H3e).
///
/// # It is never `None` for a plugin topic, and that is the whole point
///
/// It used to be. The line was assembled from three OPTIONAL facts — publisher,
/// cut short, decoded lossily — and joined only if at least one turned up, so a
/// plugin that declared `publisher = ""` and shipped a clean `help.md` under
/// the host's ceiling got NO line at all. `render_topic` then emitted title,
/// rule, body: byte-for-byte the shape of a built-in corpus page.
///
/// That is not a cosmetic gap, because every input to it is under the
/// attacker's control. `publisher` is a required TOML field but
/// `norte_plugin_host::manifest` does not check it for emptiness (unlike
/// `description`, `command.id` and `command.title`, which are all validated),
/// and staying under 64 KiB of valid UTF-8 is not a constraint on anybody. The
/// exploitation is one keystroke deep: title the page «Approving extensions»,
/// write that catalogue extensions are audited and approving is safe, and the
/// human meets it via `main::extensions_help` — from the extension manager, as
/// the ROOT of the trail, at the exact moment they are deciding whether to
/// approve. The one remaining tell was a sidebar cursor sitting under a group
/// header, which is not a tell anybody reads.
///
/// So the first part is UNCONDITIONAL and constant. A line that is always there
/// is a line the reader can learn to trust; one that shows up only sometimes
/// teaches nothing, and its absence teaches the opposite of the truth.
///
/// # Why the plugin's id is not in it
///
/// [`norte_help::Origin::Plugin`] carries `id`, which is host-assigned and
/// never empty, so it is tempting as the identity half. It is deliberately NOT
/// painted: an id is a LOOKUP KEY and has therefore never been through a mask —
/// `norte_frontend::help::PluginNode`'s `id` says so in as many words, and
/// `parse_untrusted` copies the host's id verbatim. Ours is validated
/// reverse-DNS, but it reaches this process over the wire, and a module whose
/// header promises it masks nothing must not start painting the one string
/// nobody masked. The reader already knows WHICH extension they opened — they
/// arrived on its row, under its name; what they could not know is that the page
/// was written by one.
///
/// The rest is appended when true: the publisher (masked and capped by the
/// parser, like the title — prose, never a claim this renderer vouches for),
/// then whether the page was CUT SHORT, then whether some bytes did not decode.
/// The last two do not survive a re-parse — the text arrives already short and
/// already decoded — so only the sender can report them
/// (`norte_help::Parsed::fold_flags`).
///
/// The assembly itself — the `·` joiner and why the publisher is wrapped in a
/// LABELLED segment, and the display clamp that keeps a wide publisher from
/// pushing the host's flags out of the box — lives in
/// [`norte_frontend::help_badge::plugin_badge`], shared with the other two
/// frontends since H3h.
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

/// Detaches a rendering from the topic it borrowed.
///
/// [`render_topic`] borrows the topic's strings, which is free for the corpus
/// (`'static`) and impossible for a plugin page: that one is OWNED by
/// `norte_frontend::help::HelpState`, and a `crate::app::HelpView` holding both
/// the state and a rendering borrowed from it would be a self-referential
/// struct. Cloning the visible page's spans is the cost of not building one —
/// it happens once per layout, and only for plugin pages.
///
/// Every field of ratatui's `Line` and `Span` is carried across EXPLICITLY,
/// with no `..` rest pattern: dropping styling silently is this function's
/// failure mode, and a field added by a future ratatui must break the build
/// here rather than quietly stop being copied.
///
/// ```
/// use norte_help::{Availability, ChordResolver, Lang, parse_untrusted};
/// use norte_theme::{ColorDepth, Theme};
/// use norte_tui::help_render::{into_static, render_topic};
/// use norte_tui::theme::TuiTheme;
///
/// struct Bare;
/// impl ChordResolver for Bare {
///     fn chord(&self, _command: &str) -> Option<String> {
///         None
///     }
///     fn label(&self, command: &str) -> String {
///         command.to_owned()
///     }
///     fn availability(&self, _command: &str) -> Availability {
///         Availability::Available
///     }
/// }
///
/// let theme = TuiTheme::new(Theme::preset_default(), ColorDepth::Truecolor);
/// let parsed = parse_untrusted(b"a body", "acme.ftp", Some("ACME".to_owned()));
/// // The topic is OWNED here, so the rendering borrows from a local.
/// let owned = into_static(render_topic(&parsed.topic, Lang::En, &Bare, 40, &theme));
/// drop(parsed);
/// // …and outlives it.
/// assert!(!owned.lines.is_empty());
/// ```
#[must_use]
pub fn into_static(r: Rendered<'_>) -> Rendered<'static> {
    Rendered {
        lines: r
            .lines
            .into_iter()
            .map(|line| Line {
                style: line.style,
                alignment: line.alignment,
                spans: line
                    .spans
                    .into_iter()
                    .map(|span| Span {
                        style: span.style,
                        content: std::borrow::Cow::Owned(span.content.into_owned()),
                    })
                    .collect(),
            })
            .collect(),
        action_lines: r.action_lines,
        heading_lines: r.heading_lines,
    }
}

/// Paints one block of the closed vocabulary of [`Block`].
///
/// Wrapping is by terminal CELLS: CJK and emoji take two columns each, so a
/// budget counted in `chars()` overflows the pane and the widget then trims
/// the tail (the defect #79 fixed for the middle ellipsis). Words keep the
/// spaces that follow them, so a wrap never joins two words, and a single word
/// longer than the whole width is cut at a cell boundary rather than allowed
/// to overflow.
///
/// The one thing that does NOT wrap is a [`Block::Code`]: a wrapped code line
/// is a lie about what to type. It is indented and left long; clipping it is
/// the pane's decision, not this function's.
///
/// ```
/// use norte_help::{Availability, Block, ChordResolver, Lang, Span};
/// use norte_theme::{ColorDepth, Theme};
/// use norte_tui::help_render::render_block;
/// use norte_tui::theme::TuiTheme;
/// use unicode_width::UnicodeWidthStr;
///
/// struct Bare;
/// impl ChordResolver for Bare {
///     fn chord(&self, _command: &str) -> Option<String> {
///         None
///     }
///     fn label(&self, command: &str) -> String {
///         command.to_owned()
///     }
///     fn availability(&self, _command: &str) -> Availability {
///         Availability::Available
///     }
/// }
///
/// let theme = TuiTheme::new(Theme::preset_default(), ColorDepth::Truecolor);
/// let block = Block::Paragraph(vec![Span::Text("漢字".repeat(40))]);
/// for line in render_block(&block, Lang::En, &Bare, 20, &theme) {
///     let cells: usize = line.spans.iter().map(|s| s.content.width()).sum();
///     assert!(cells <= 20, "two cells per char, budgeted as such");
/// }
/// ```
#[must_use]
pub fn render_block<'a>(
    block: &'a Block,
    lang: Lang,
    r: &(impl ChordResolver + ?Sized),
    width: usize,
    theme: &TuiTheme,
) -> Vec<Line<'a>> {
    match block {
        // Levels are not indented and carry no `#`: the corpus nests at most
        // three deep and the overlay is narrow, so the heading style is the
        // only signal that survives an 40-cell body intact.
        Block::Heading { text, .. } => wrap(
            &[(text.clone(), theme.role(Role::Title))],
            width,
            "",
            "",
            theme.role(Role::Title),
        ),
        Block::Paragraph(spans) => wrap(
            &frags(spans, lang, r, theme),
            width,
            "",
            "",
            Style::default(),
        ),
        Block::Bullets(items) => items
            .iter()
            .flat_map(|item| {
                wrap(
                    &frags(item, lang, r, theme),
                    width,
                    "• ",
                    "  ",
                    theme.role(Role::Info),
                )
            })
            .collect(),
        Block::Code { text, .. } => text
            .lines()
            .map(|l| {
                Line::from(vec![
                    Span::raw("  "),
                    Span::styled(l.to_owned(), theme.role(Role::Mark)),
                ])
            })
            .collect(),
        Block::Table { header, rows } => table(header, rows, width, theme),
        Block::Callout { kind, spans } => {
            // KNOWN HAZARD (H3b encoding audit, FIX 4b): the three glyphs do
            // not agree on width in practice. `ℹ` (U+2139) and `⚠` (U+26A0)
            // are East Asian AMBIGUOUS — `unicode-width` budgets them at 1,
            // which is what a text-presentation terminal draws, but many
            // terminals give them emoji presentation at 2 cells — while `💡`
            // (U+1F4A1) is 2 everywhere. So the continuation indent computed
            // here matches the first line on some terminals and is one cell
            // short on others, and the note/warn arms drift while the tip arm
            // does not. Deliberately NOT chased: per-terminal presentation
            // cannot be detected from here, and the failure is a one-cell
            // ragged indent, never lost or fabricated text. The ASCII
            // `HOSTILE_BADGE` (`ui.rs`) exists for the case where that is not
            // acceptable.
            let (glyph, role) = match kind {
                Callout::Note => ("ℹ", Role::Info),
                Callout::Warn => ("⚠", Role::Warning),
                Callout::Tip => ("💡", Role::Info),
            };
            let head = format!("{glyph} ");
            let cont = " ".repeat(head.width());
            wrap(
                &frags(spans, lang, r, theme),
                width,
                &head,
                &cont,
                theme.role(role),
            )
        }
    }
}

/// One runnable row: the chord in its padded column, then the label — and, on
/// a row that cannot run, WHY.
///
/// Always ONE line — an action that wrapped would break the invariant
/// [`Rendered::action_lines`] rests on — so the text is cut, not wrapped.
///
/// The reason is painted because dimming alone leaves the reader guessing
/// whether the row is inapplicable or the app is broken. Its Fluent id comes
/// from [`norte_frontend::availability::reason_key`], the same ids the GUI's
/// context menu paints, so the two surfaces explain a veto in one wording.
///
/// # Which text takes the ellipsis
///
/// On an UNAVAILABLE row the reason is budgeted FIRST and the LABEL is cut. The
/// obvious way round — compose `label — reason` and truncate the whole thing —
/// paints `copy the selection to the other pane — read-only backe…` in any
/// realistic pane, which answers the question the reader did not ask and drops
/// the one the dimming raised. The command's own name is already in the prose
/// above the table and its chord is in the column to the left, so the label is
/// the recoverable half.
///
/// An available row is untouched: it has no reason, gets the whole budget for
/// its label, and nothing about it changed.
///
/// If the reason ALONE does not fit, it takes the ellipsis and the label is
/// dropped entirely — there is nothing else to give up, and a row that says
/// `read-onl…` still points at the right kind of answer. The same happens a
/// couple of cells EARLIER, while the label technically still fits: one or two
/// cells of a label is a lone `…`, and `f5    … — read-only backend` reads as a
/// bug in the renderer rather than as a name that was shortened.
fn row_line<'a>(
    row: &norte_help::ResolvedRow,
    col: usize,
    width: usize,
    lang: Lang,
    theme: &TuiTheme,
) -> Line<'a> {
    /// Between the label and the reason. Spaced em dash, as the status bar
    /// spells the same join.
    const SEP: &str = " — ";
    /// Fewest cells worth spending on a truncated label. Below this the label
    /// is dropped entirely — see the `for_label` arm.
    const MIN_LABEL: usize = 4;

    let chord = row.chord.as_deref().unwrap_or(NO_CHORD);
    let available = row.row.avail.is_available();
    // An unavailable row is prose, not a key: it must not wear the key style
    // while it cannot be pressed.
    let key_style = if available {
        theme.role(Role::Mark)
    } else {
        theme.role(Role::Info)
    };
    let label_style = if available {
        theme.role(Role::Regular)
    } else {
        theme.role(Role::Info)
    };
    // The padding stays OUTSIDE the key style, so a themed `mark` background
    // hugs the chord instead of stretching across an empty column.
    let pad = " ".repeat(col.saturating_sub(chord.width()) + 2);
    let used = 2 + chord.width() + pad.width();
    let budget = width.saturating_sub(used);
    let text = match row.row.avail.reason() {
        Some(reason) => {
            let reason = norte_i18n::t_in(lang, norte_frontend::availability::reason_key(reason));
            // What is left once the reason and its separator are paid for. Zero
            // means the reason is the whole line.
            // What is left once the reason and its separator are paid for. A
            // budget under MIN_LABEL is folded into the same case as zero: it
            // is WITHIN the width, but one or two cells of a label is `…` or
            // `d…`, which reads as a rendering fault rather than as a
            // shortened name. Dropping it gives those cells to the reason,
            // which is the half worth keeping.
            let for_label = budget.saturating_sub(SEP.width() + reason.width());
            if for_label < MIN_LABEL {
                fit(&reason, budget)
            } else {
                format!("{}{SEP}{reason}", fit(&row.label, for_label))
            }
        }
        None => fit(&row.label, budget),
    };
    debug_assert!(
        text.width() <= budget,
        "row of {} cells in a {budget}-cell budget: {text:?}",
        text.width()
    );
    Line::from(vec![
        Span::raw("  "),
        Span::styled(chord.to_owned(), key_style),
        Span::raw(pad),
        Span::styled(text, label_style),
    ])
}

/// A simple table: header, a rule, then the rows.
///
/// Cells are read by ZIPPING each row against the column widths rather than
/// indexing by header position. The parser normalises rows to `header.len()`
/// (see [`Block::Table`]) and this renderer relies on that instead of
/// re-normalising — but relying on a contract must not mean panicking when it
/// changes, and the rows come from a `split` over text a plugin wrote.
fn table<'a>(
    header: &[String],
    rows: &[Vec<String>],
    width: usize,
    theme: &TuiTheme,
) -> Vec<Line<'a>> {
    if header.is_empty() {
        return Vec::new();
    }
    let mut widths: Vec<usize> = header.iter().map(|h| h.width()).collect();
    for row in rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.width());
        }
    }
    shrink_to_fit(&mut widths, width);

    let paint = |cells: &[String], style: Style| -> Line<'a> {
        let mut spans = Vec::with_capacity(widths.len() * 2);
        for (i, (w, cell)) in widths.iter().zip(cells).enumerate() {
            if i > 0 {
                spans.push(Span::raw("  "));
            }
            let text = fit(cell, *w);
            let pad = w.saturating_sub(text.width());
            spans.push(Span::styled(text, style));
            spans.push(Span::raw(" ".repeat(pad)));
        }
        Line::from(spans)
    };

    let mut out = vec![paint(header, theme.role(Role::Title))];
    out.push(Line::from(Span::styled(
        widths
            .iter()
            .map(|w| "─".repeat(*w))
            .collect::<Vec<_>>()
            .join("  "),
        theme.role(Role::BorderUnfocused),
    )));
    out.extend(rows.iter().map(|r| paint(r, theme.role(Role::Regular))));
    out
}

/// Shrinks the widest column, one cell at a time, until the table fits.
///
/// Widest-first rather than proportional: it is the prose column that has
/// slack, and taking a cell off a two-character `skip` column to spare one on
/// a sentence helps nobody.
fn shrink_to_fit(widths: &mut [usize], width: usize) {
    let gaps = 2 * widths.len().saturating_sub(1);
    let budget = width.saturating_sub(gaps);
    while widths.iter().sum::<usize>() > budget {
        let Some(w) = widths.iter_mut().max() else {
            return;
        };
        if *w <= 1 {
            return;
        }
        *w -= 1;
    }
}

/// Cuts `text` to `cells` terminal cells, marking the cut with `…` when there
/// is room for it. Never widens: the result measures `cells` at most.
fn fit(text: &str, cells: usize) -> String {
    if text.width() <= cells {
        return text.to_owned();
    }
    if cells == 0 {
        return String::new();
    }
    if cells == 1 {
        return "…".to_owned();
    }
    let cut = cell_prefix(text, cells - 1);
    format!("{}…", &text[..cut])
}

/// The styled fragments of a run of spans, before wrapping.
///
/// The `{{cmd:…}}` arm is the one that matters: it matches on
/// [`CommandText`] rather than flattening to text, because a chord is painted
/// as a key and a name as prose — collapsing them makes every mark look like
/// something the reader can press, including the ones that are not.
fn frags(
    spans: &[HSpan],
    lang: Lang,
    r: &(impl ChordResolver + ?Sized),
    theme: &TuiTheme,
) -> Vec<(String, Style)> {
    spans
        .iter()
        .map(|span| match span {
            HSpan::Text(t) => (t.clone(), theme.role(Role::Regular)),
            HSpan::Strong(t) => (t.clone(), theme.role(Role::Title)),
            HSpan::Emph(t) => (t.clone(), theme.role(Role::Info)),
            HSpan::Code(t) => (t.clone(), theme.role(Role::Mark)),
            // The page's TITLE, exactly as the `see_also` rows and the sidebar
            // paint it. A link mid-sentence used to read `[[selection]]` while
            // the row for that same page read «Marcar sobre qué se actúa» and
            // the sidebar read it a third way — three spellings of one thing,
            // and the wiki brackets were a fourth signal for what `Role::Info`
            // already says. An unresolvable id keeps the id, for the reason
            // the `see_also` arm gives.
            HSpan::TopicLink(id) => (
                norte_help::topic(lang, id.as_str())
                    .map_or_else(|| id.to_string(), |t| t.title.clone()),
                theme.role(Role::Info),
            ),
            HSpan::CommandRef(c) => match norte_help::render_command(c, r) {
                CommandText::Chord(k) => (k, theme.role(Role::Mark)),
                CommandText::Name(n) => (n, theme.role(Role::Info)),
            },
        })
        .collect()
}

/// One WORD of the wrap, and the whitespace that follows it.
///
/// A word can span several styled fragments, which is the whole point: a
/// fragment boundary is not a break opportunity. See [`units`].
struct Unit<'f> {
    /// The word, in pieces — one per fragment it crosses, each with its own
    /// style. Never contains whitespace.
    word: Vec<(&'f str, Style)>,
    /// The blanks that separate it from the next word, already normalised to
    /// spaces, in the style of the fragment each run came from.
    spaces: Vec<(String, Style)>,
}

/// Groups styled fragments into wrap [`Unit`]s: ONLY whitespace opens a break
/// opportunity.
///
/// The defect this exists to kill: `wrap` used to walk each fragment
/// independently, so the boundary BETWEEN two fragments was a break
/// opportunity even with no space at it. A sentence ending in an inline code
/// span — `…inside de un `.zip`.` — is a `Code(".zip")` fragment followed by a
/// `Text(".")` one, and at any width where the boundary lands near the margin
/// the full stop was pushed onto a line of its own. Two fragments with nothing
/// between them are one word and wrap as one unit.
///
/// Whitespace is normalised to spaces here: a tab or a newline inside a span
/// would otherwise be painted raw into a cell grid.
fn units<'f>(frags: &'f [(String, Style)]) -> Vec<Unit<'f>> {
    let mut out: Vec<Unit<'f>> = Vec::new();
    // Whether the last unit's word can still take another piece — i.e. no
    // whitespace has been seen since it started.
    let mut open = false;
    for (text, style) in frags {
        for (word, spaces) in tokens(text) {
            if !word.is_empty() {
                match out.last_mut() {
                    Some(last) if open => last.word.push((word, *style)),
                    _ => out.push(Unit {
                        word: vec![(word, *style)],
                        spaces: Vec::new(),
                    }),
                }
                open = true;
            }
            if !spaces.is_empty() {
                if let Some(last) = out.last_mut() {
                    last.spaces
                        .push((" ".repeat(spaces.chars().count()), *style));
                }
                // Blanks BEFORE the first word are dropped: a body never
                // opens on the indentation of its source.
                open = false;
            }
        }
    }
    out
}

/// Wraps styled fragments into lines of at most `width` CELLS, prefixing the
/// first line with `head` and every continuation with `cont`.
///
/// The budget is cells throughout. A word carries the whitespace that follows
/// it, and that whitespace is only painted when another word joins it on the
/// same line — so a wrap never joins two words and a line never ends in
/// trailing blanks. A word wider than the whole line is cut at a cell
/// boundary.
///
/// The unit of wrapping is a [`Unit`], not a fragment: only whitespace is a
/// break opportunity, so a word that crosses a style change (a code span
/// closing a sentence, say) moves to the next line WHOLE. See [`units`].
fn wrap<'a>(
    frags: &[(String, Style)],
    width: usize,
    head: &str,
    cont: &str,
    prefix_style: Style,
) -> Vec<Line<'a>> {
    let indent = head.width().max(cont.width());
    let inner = width.saturating_sub(indent).max(1);

    let mut lines: Vec<Vec<Span<'a>>> = Vec::new();
    let mut cur: Vec<Span<'a>> = Vec::new();
    let mut cur_w = 0usize;
    let mut pending: Vec<Span<'a>> = Vec::new();
    let mut pending_w = 0usize;

    for unit in units(frags) {
        // The whole unit is measured before the break decision: the word is
        // indivisible at a fragment boundary.
        let word_w: usize = unit.word.iter().map(|(w, _)| w.width()).sum();
        if word_w > 0 {
            if cur_w == 0 {
                // A line never starts with the blanks that ended the
                // previous one.
                pending.clear();
                pending_w = 0;
            } else if cur_w + pending_w + word_w > inner {
                lines.push(std::mem::take(&mut cur));
                cur_w = 0;
                pending.clear();
                pending_w = 0;
            } else {
                cur.append(&mut pending);
                cur_w += pending_w;
                pending_w = 0;
            }

            for (piece, style) in &unit.word {
                let mut rest = *piece;
                while !rest.is_empty() {
                    let avail = inner.saturating_sub(cur_w);
                    if rest.width() <= avail {
                        cur.push(Span::styled(rest.to_owned(), *style));
                        cur_w += rest.width();
                        break;
                    }
                    let cut = cell_prefix(rest, avail);
                    if cut == 0 {
                        if cur_w > 0 {
                            lines.push(std::mem::take(&mut cur));
                            cur_w = 0;
                            continue;
                        }
                        // The line is narrower than a single character (a
                        // two-cell glyph in a one-cell body). Emitting it
                        // overflows by one cell; looping forever would be
                        // worse, and there is no third answer.
                        let n = rest
                            .chars()
                            .next()
                            .map_or(rest.len(), char::len_utf8)
                            .min(rest.len());
                        cur.push(Span::styled(rest[..n].to_owned(), *style));
                        lines.push(std::mem::take(&mut cur));
                        rest = &rest[n..];
                        continue;
                    }
                    cur.push(Span::styled(rest[..cut].to_owned(), *style));
                    lines.push(std::mem::take(&mut cur));
                    cur_w = 0;
                    rest = &rest[cut..];
                }
            }
        }
        if cur_w > 0 {
            for (blanks, style) in unit.spaces {
                pending_w += blanks.width();
                pending.push(Span::styled(blanks, style));
            }
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.is_empty() {
        lines.push(Vec::new());
    }

    lines
        .into_iter()
        .enumerate()
        .map(|(i, mut spans)| {
            let p = if i == 0 { head } else { cont };
            if !p.is_empty() {
                spans.insert(0, Span::styled(p.to_owned(), prefix_style));
            }
            Line::from(spans)
        })
        .collect()
}

/// Splits `text` into `(word, following blanks)` pairs, in order. Both halves
/// can be empty (a fragment that starts with a space yields `("", " ")`).
fn tokens(text: &str) -> Vec<(&str, &str)> {
    let mut out = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let (word, tail) = rest.split_at(end);
        let blanks = tail
            .find(|c: char| !c.is_whitespace())
            .unwrap_or(tail.len());
        let (spaces, next) = tail.split_at(blanks);
        out.push((word, spaces));
        rest = next;
    }
    out
}

/// Byte length of the longest prefix of `text` that measures at most `cells`
/// terminal cells. `0` when not even the first character fits.
fn cell_prefix(text: &str, cells: usize) -> usize {
    let mut used = 0usize;
    for (i, ch) in text.char_indices() {
        let w = ch.width().unwrap_or(0);
        if used + w > cells {
            return i;
        }
        used += w;
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    use norte_help::{Availability, Lang, Reason, TopicId, topic};
    use norte_theme::{ColorDepth, Theme};

    /// A deterministic theme: the shipped preset at a fixed depth, so a style
    /// assertion does not depend on the terminal the suite runs in.
    fn theme() -> TuiTheme {
        TuiTheme::new(Theme::preset_default(), ColorDepth::Truecolor)
    }

    /// A frontend, faked: one command bound, one unavailable, and a label
    /// built from the id so a wrong lookup is visible in the assertion.
    struct Fake;

    impl ChordResolver for Fake {
        fn chord(&self, command: &str) -> Option<String> {
            (command == "pane.copy").then(|| "f5".to_owned())
        }

        fn label(&self, command: &str) -> String {
            format!("do {command}")
        }

        fn availability(&self, command: &str) -> Availability {
            if command == "pane.delete-permanent" {
                Availability::Unavailable {
                    reason: Reason::ReadOnlyBackend,
                }
            } else {
                Availability::Available
            }
        }
    }

    /// Everything vetoed for the same reason: what is checked is that the
    /// reason ACCOMPANIES the row, not which command carries it.
    struct Vetoed;

    impl ChordResolver for Vetoed {
        fn chord(&self, _command: &str) -> Option<String> {
            Some("f5".to_owned())
        }

        fn label(&self, command: &str) -> String {
            format!("do {command}")
        }

        fn availability(&self, _command: &str) -> Availability {
            Availability::Unavailable {
                reason: Reason::ReadOnlyBackend,
            }
        }
    }

    /// Nothing vetoed: [`Vetoed`]'s counterpart.
    struct Free;

    impl ChordResolver for Free {
        fn chord(&self, _command: &str) -> Option<String> {
            Some("f5".to_owned())
        }

        fn label(&self, command: &str) -> String {
            format!("do {command}")
        }

        fn availability(&self, _command: &str) -> Availability {
            Availability::Available
        }
    }

    #[test]
    fn a_vetoed_row_paints_its_reason() {
        // Dimming without saying why leaves the reader guessing whether it is
        // a bug.
        let out = render_topic(
            topic(Lang::En, "copying").expect("copying"),
            Lang::En,
            &Vetoed,
            80,
            &theme(),
        );
        let text = flatten(&out.lines);
        assert!(
            text.contains(&norte_i18n::t_in(Lang::En, "reason-read-only")),
            "the reason accompanies the dimmed row: {text}"
        );
        // And a row that CAN run carries no reason at all: if it painted one,
        // the reader could not tell what they can press.
        let free = render_topic(
            topic(Lang::En, "copying").expect("copying"),
            Lang::En,
            &Free,
            80,
            &theme(),
        );
        let free = flatten(&free.lines);
        assert!(
            !free.contains(&norte_i18n::t_in(Lang::En, "reason-read-only")),
            "reason painted on a page with nothing vetoed: {free}"
        );
    }

    /// Narrow: the REASON survives and the label takes the ellipsis.
    ///
    /// The other way around — composing `label — reason` and clipping the
    /// whole — the reader is left with the command's name (already in the
    /// prose above and in the chord column) and loses the one datum the
    /// dimming was making. And the row stays ONE line no matter what: the
    /// `action_lines` map counts on that.
    #[test]
    fn in_a_narrow_row_the_reason_survives_and_the_label_is_clipped() {
        let reason = norte_i18n::t_in(Lang::En, "reason-read-only");
        let rows_at = |width: usize| -> Vec<String> {
            let out = render_topic(
                topic(Lang::En, "copying").expect("copying"),
                Lang::En,
                &Vetoed,
                width,
                &theme(),
            );
            // No ROW exceeds the width, with or without a reason. (Only the
            // rows: a `Block::Code` is deliberately left long — see
            // `render_block` — and clipping it is the pane's job.)
            for &y in &out.action_lines {
                let line = &out.lines[y];
                assert!(
                    cells(line) <= width,
                    "row of {} cells in a {width}-cell body: {:?}",
                    cells(line),
                    flatten(std::slice::from_ref(line))
                );
            }
            out.action_lines
                .iter()
                .map(|&y| flatten(std::slice::from_ref(&out.lines[y])))
                .collect()
        };

        // The whole reason survives at every width where it FITS, even when
        // the label does not.
        for width in [30, 40, 60, 80] {
            let rows = rows_at(width);
            assert!(
                rows.iter().any(|f| f.contains(&reason)),
                "at {width} cells the whole reason is still there: {rows:?}"
            );
        }

        // At 40 cells the page's longest label no longer fits: it is THAT one
        // that gets clipped, with the reason intact behind it.
        let rows = rows_at(40);
        assert!(
            rows.iter().any(|f| f.contains('…') && f.contains(&reason)),
            "the label yields and the reason stays: {rows:?}"
        );

        // And when not even the reason fits, it takes the ellipsis and the
        // label disappears: there is nothing else left to give up.
        let narrow = rows_at(20);
        let row = narrow.first().expect("there are rows");
        assert!(row.contains('…'), "the reason is clipped: {row:?}");
        assert!(
            !row.contains("do pane"),
            "with no room, the label is not painted half-done: {row:?}"
        );

        // MINOR-8: the widths where the label was left one or two cells.
        // They fit within the budget, so the "zero" case did not catch them
        // and the row came out as `f5    … — read-only backend`: a lone
        // ellipsis is not a shortened name, it looks like a painter bug.
        // They fold into the same case as zero.
        for width in 27..=29 {
            let rows = rows_at(width);
            let row = rows.first().expect("there are rows");
            assert!(
                row.contains(&reason),
                "at {width} cells the reason is what is kept: {row:?}"
            );
            assert!(
                !row.contains(" … — ") && !row.contains("… — "),
                "lone ellipsis where the label went ({width}): {row:?}"
            );
        }
    }

    fn flatten(lines: &[Line<'_>]) -> String {
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

    fn cells(line: &Line<'_>) -> usize {
        line.spans.iter().map(|s| s.content.width()).sum()
    }

    #[test]
    fn a_command_mark_becomes_the_users_chord_in_the_prose() {
        let copying = topic(Lang::En, "copying").expect("the `copying` topic");
        let out = render_topic(copying, Lang::En, &Fake, 60, &theme());
        let text = flatten(&out.lines);
        assert!(
            text.contains("f5"),
            "the prose must show the key THIS user has: {text}"
        );
        assert!(
            !text.contains("{{cmd:"),
            "an unresolved mark on screen is the corpus leaking: {text}"
        );
    }

    #[test]
    fn wrapping_respects_terminal_cells_not_chars() {
        // CJK is two cells per char: budgeting by `chars()` overflows the
        // width and ratatui then trims the tail — the defect #79 fixed in a
        // different place.
        let block = Block::Paragraph(vec![HSpan::Text("漢字".repeat(40))]);
        let lines = render_block(&block, Lang::En, &Fake, 20, &theme());
        assert!(
            lines.len() > 1,
            "160 cells of CJK do not fit in one 20-cell line"
        );
        for line in &lines {
            assert!(
                cells(line) <= 20,
                "line of {} cells in a 20-cell body: {:?}",
                cells(line),
                flatten(std::slice::from_ref(line))
            );
        }
    }

    #[test]
    fn a_word_longer_than_the_width_is_split_at_a_cell_boundary() {
        let block = Block::Paragraph(vec![HSpan::Text("x".repeat(50))]);
        let lines = render_block(&block, Lang::En, &Fake, 10, &theme());
        for line in &lines {
            assert!(
                cells(line) <= 10,
                "an unbreakable word must be cut, not left to overflow"
            );
        }
        assert_eq!(
            flatten(&lines).replace('\n', ""),
            "x".repeat(50),
            "cutting must not lose or invent characters"
        );
    }

    #[test]
    fn command_rows_map_to_the_lines_they_are_painted_on() {
        // Without this map the body cannot scroll to follow the action
        // cursor: `HelpState::reveal` takes a LINE and the cursor walks
        // ACTIONS.
        let copying = topic(Lang::En, "copying").expect("the `copying` topic");
        let out = render_topic(copying, Lang::En, &Fake, 60, &theme());
        assert_eq!(
            out.action_lines.len(),
            copying.commands.len() + copying.links().len(),
            "one line per action of `HelpState::actions`, commands then links"
        );
        assert!(
            out.action_lines.windows(2).all(|w| w[0] < w[1]),
            "actions are painted in order: {:?}",
            out.action_lines
        );
        assert!(
            out.action_lines.iter().all(|&i| i < out.lines.len()),
            "an action line outside the body would scroll into nothing"
        );
        // …and each one really is the row it claims to be.
        let first = &out.lines[out.action_lines[0]];
        assert!(
            flatten(std::slice::from_ref(first)).contains("do pane.copy"),
            "the first action line is the first command: {first:?}"
        );
        // A link row carries the linked page's TITLE — the same string the
        // sidebar paints for it — and not its id in wiki brackets, a spelling
        // nothing else in the UI uses.
        let linked =
            topic(Lang::En, copying.see_also[0].as_str()).expect("`copying` links live pages");
        let link = &out.lines[out.action_lines[copying.commands.len()]];
        let painted = flatten(std::slice::from_ref(link));
        assert!(
            painted.contains(&linked.title),
            "the links follow the commands, named as the sidebar names them: \
             {painted:?} should carry {:?}",
            linked.title
        );
        assert!(
            !painted.contains("[["),
            "…and without the wiki brackets: {painted:?}"
        );
    }

    /// The same map, over the WHOLE corpus and both locales.
    ///
    /// The test above pins `copying`, which is the topic the focus tests use;
    /// this one is the cheap structural sweep behind it. The invariant the
    /// overlay's focus machinery rests on is cross-crate — action *i* of
    /// `HelpState::actions` is painted on `action_lines[i]` — and the two
    /// halves are built in different crates from different data, so a topic
    /// shape nobody thought about (no commands but three links, a table right
    /// before the rows, a `see_also` pointing at a page that does not resolve)
    /// is exactly where the two would fall out of step. Narrow widths are
    /// swept too: wrapping changes how many lines the PROSE takes, and the map
    /// is a set of absolute line indices into it.
    #[test]
    fn the_action_map_matches_every_topic_of_every_locale() {
        let th = theme();
        for lang in [Lang::En, Lang::Es] {
            for t in norte_help::topics(lang) {
                for width in [20, 40, 60] {
                    let out = render_topic(t, lang, &Fake, width, &th);
                    let ctx = format!("{lang:?}/{} at {width} cells", t.id);
                    assert_eq!(
                        out.action_lines.len(),
                        t.commands.len() + t.links().len(),
                        "[{ctx}] one action line per command and per link, in \
                         `HelpState::actions` order"
                    );
                    assert!(
                        out.action_lines.windows(2).all(|w| w[0] < w[1]),
                        "[{ctx}] the indices must be strictly increasing — a \
                         repeat means two actions share a line and the focus \
                         cannot tell them apart: {:?}",
                        out.action_lines
                    );
                    assert!(
                        out.action_lines.iter().all(|&i| i < out.lines.len()),
                        "[{ctx}] an action line outside the body scrolls into \
                         nothing: {:?} of {} lines",
                        out.action_lines,
                        out.lines.len()
                    );
                }
            }
        }
    }

    #[test]
    fn a_table_row_shorter_than_its_header_does_not_panic() {
        // The parser normalises rows to `header.len()` and this renderer
        // RELIES on that rather than re-checking — so this test is what
        // catches it if the contract ever changes.
        let block = Block::Table {
            header: vec!["Answer".to_owned(), "What happens".to_owned()],
            rows: vec![
                vec!["overwrite".to_owned(), "replaced".to_owned()],
                vec!["skip".to_owned()],
                vec![],
            ],
        };
        let lines = render_block(&block, Lang::En, &Fake, 40, &theme());
        let text = flatten(&lines);
        assert!(text.contains("Answer"), "header painted: {text}");
        assert!(text.contains("skip"), "the short row still paints: {text}");
    }

    #[test]
    fn a_command_with_no_chord_is_not_painted_as_a_pressable_key() {
        // The whole point of `CommandText`'s two variants: a name must not
        // wear the key style, or every mark looks like something to press.
        let th = theme();
        let unbound = Block::Paragraph(vec![HSpan::CommandRef("pane.rename".to_owned())]);
        let named = render_block(&unbound, Lang::En, &Fake, 60, &th);
        assert_eq!(
            flatten(&named),
            "do pane.rename",
            "with no key bound the mark names the command"
        );
        assert!(
            named
                .iter()
                .flat_map(|l| l.spans.iter())
                .all(|s| s.style != th.role(Role::Mark)),
            "a name painted in the key style claims a key that does not \
             exist: {named:?}"
        );
        assert!(
            named
                .iter()
                .flat_map(|l| l.spans.iter())
                .any(|s| s.content.contains("pane.rename") && s.style == th.role(Role::Info)),
            "it is prose: {named:?}"
        );

        // Guard against a vacuous pass: a command that DOES have a chord is
        // painted in the key style, so the assertion above is a distinction
        // and not just "nothing is ever a key".
        let with_key = Block::Paragraph(vec![HSpan::CommandRef("pane.copy".to_owned())]);
        let bound = render_block(&with_key, Lang::En, &Fake, 60, &th);
        assert!(
            bound
                .iter()
                .flat_map(|l| l.spans.iter())
                .any(|s| s.content == "f5" && s.style == th.role(Role::Mark)),
            "a real chord is painted as a key: {bound:?}"
        );
    }

    #[test]
    fn an_unavailable_row_is_not_painted_as_a_pressable_key_either() {
        let th = theme();
        let copying = topic(Lang::En, "copying").expect("the `copying` topic");
        let out = render_topic(copying, Lang::En, &Fake, 60, &th);
        let idx = copying
            .commands
            .iter()
            .position(|c| c == "pane.delete-permanent")
            .expect("`copying` documents it");
        let line = &out.lines[out.action_lines[idx]];
        assert!(
            line.spans.iter().all(|s| s.style != th.role(Role::Mark)),
            "a row that cannot run must not wear the key style: {line:?}"
        );
    }

    #[test]
    fn every_block_of_the_closed_vocabulary_paints_something() {
        let th = theme();
        for block in [
            Block::Heading {
                level: 2,
                text: "Heading".to_owned(),
            },
            Block::Paragraph(vec![
                HSpan::Strong("strong".to_owned()),
                HSpan::Text(" and ".to_owned()),
                HSpan::Emph("emph".to_owned()),
                HSpan::Code("code".to_owned()),
                HSpan::TopicLink(TopicId::new("selection")),
            ]),
            Block::Bullets(vec![vec![HSpan::Text("one".to_owned())]]),
            Block::Code {
                lang: Some("sh".to_owned()),
                text: "norte cp --resume a b".to_owned(),
            },
            Block::Callout {
                kind: Callout::Warn,
                spans: vec![HSpan::Text("careful".to_owned())],
            },
        ] {
            let lines = render_block(&block, Lang::En, &Fake, 40, &th);
            assert!(!lines.is_empty(), "{block:?} painted nothing");
            for line in &lines {
                assert!(cells(line) <= 40, "{block:?} overflowed: {line:?}");
            }
        }
    }

    /// A wrap must never swallow the whitespace it broke at: two words the
    /// source SEPARATED must never come out adjacent.
    ///
    /// The invariant is untouched; the fixture is not. It used to read
    /// `Text("alpha beta")` + `Strong("gamma")`, with no whitespace between
    /// `beta` and `gamma` — which makes `betagamma` a single word of the
    /// source, so painting it as one is the correct answer, not the failure
    /// the assertion was reaching for. (Splitting it across two lines is the
    /// defect `a_code_span_and_the_punctuation_after_it_are_one_word` pins.)
    /// The space is now explicit, so every pair here really is two words.
    #[test]
    fn wrapping_never_joins_two_words() {
        let long = Block::Paragraph(vec![
            HSpan::Text("alpha beta ".to_owned()),
            HSpan::Strong("gamma".to_owned()),
            HSpan::Text(" delta epsilon".to_owned()),
        ]);
        let lines = render_block(&long, Lang::En, &Fake, 12, &theme());
        let text = flatten(&lines);
        for glued in ["alphabeta", "betagamma", "gammadelta", "deltaepsilon"] {
            assert!(
                !text.contains(glued),
                "a wrap must not glue two words together ({glued}): {text}"
            );
        }
        for line in &lines {
            let painted = flatten(std::slice::from_ref(line));
            assert_eq!(
                painted.trim_end(),
                painted,
                "no line ends in the blanks that separated it from the next"
            );
        }
    }

    /// Only WHITESPACE is a break opportunity: a fragment boundary with no
    /// space at it is inside a word, and the two halves wrap together.
    ///
    /// The shape that shipped: a sentence closing on an inline code span
    /// broke as `…inside de un .zip` / `.`, leaving the full stop alone on
    /// the next line, because `wrap` walked one fragment at a time. Pinned at
    /// EVERY width where the boundary can land near the margin, not at one
    /// hand-picked one — the defect only shows at the widths that put the
    /// break there.
    #[test]
    fn a_code_span_and_the_punctuation_after_it_are_one_word() {
        let sentence = Block::Paragraph(vec![
            HSpan::Text("Un fichero dentro de un ".to_owned()),
            HSpan::Code(".zip".to_owned()),
            HSpan::Text(". Y sigue.".to_owned()),
        ]);
        for width in 10..=40 {
            let lines = render_block(&sentence, Lang::En, &Fake, width, &theme());
            for line in &lines {
                let painted = flatten(std::slice::from_ref(line));
                assert_ne!(
                    painted.trim(),
                    ".",
                    "the full stop belongs to `.zip`, not to a line of its own \
                     (width {width}): {:?}",
                    flatten(&lines)
                );
                assert!(
                    cells(line) <= width,
                    "keeping the word whole must not overflow the body \
                     (width {width}): {painted:?}"
                );
            }
            // Nothing is lost or invented by the regrouping.
            assert_eq!(
                flatten(&lines).replace('\n', " "),
                "Un fichero dentro de un .zip. Y sigue.",
                "width {width}"
            );
        }
    }

    /// The other half of the same rule: a word that crosses a style change
    /// moves to the next line WHOLE, rather than being split at the boundary
    /// to fill the current one.
    #[test]
    fn a_word_that_crosses_a_style_change_wraps_as_one() {
        // `123456789` + `abc` is one 12-cell word: on a 12-cell body it fits
        // alone on a line, and `xxxx ` before it must push it down entire.
        let block = Block::Paragraph(vec![
            HSpan::Text("xxxx 123456789".to_owned()),
            HSpan::Strong("abc".to_owned()),
        ]);
        let lines = render_block(&block, Lang::En, &Fake, 12, &theme());
        assert_eq!(
            flatten(&lines),
            "xxxx\n123456789abc",
            "the styled tail must not be left behind on the previous line"
        );
    }

    /// A heading opens a section, and the single blank line that separates two
    /// paragraphs does not say so. It gets two — but never at the top of the
    /// body, where the title's rule is separation enough, and never a trailing
    /// one after the last block.
    #[test]
    fn a_heading_is_given_more_air_than_a_paragraph_break() {
        let for_ = |s: &str| Block::Paragraph(vec![HSpan::Text(s.to_owned())]);
        let heading = |s: &str| Block::Heading {
            level: 1,
            text: s.to_owned(),
        };
        let topic = Topic {
            id: TopicId::new("spacing"),
            title: "Spacing".to_owned(),
            tags: Vec::new(),
            see_also: Vec::new(),
            commands: Vec::new(),
            context: Vec::new(),
            blocks: vec![for_("one"), heading("Section"), for_("two")],
            origin: norte_help::Origin::BuiltIn,
        };
        let out = render_topic(&topic, Lang::En, &Fake, 40, &theme());
        let text = flatten(&out.lines);
        let rows: Vec<&str> = text.lines().collect();
        assert_eq!(
            rows,
            vec![
                "Spacing",
                &"─".repeat(40)[..],
                "",
                "one",
                "",
                "",
                "Section",
                "",
                "two",
            ],
            "two blanks before the heading, one between paragraphs, none \
             trailing: {rows:?}"
        );

        // A topic that OPENS on a heading gets no extra blank: the rule above
        // it already separates it from the title.
        let first = Topic {
            blocks: vec![heading("Section"), for_("one")],
            ..topic
        };
        let out = render_topic(&first, Lang::En, &Fake, 40, &theme());
        assert_eq!(
            flatten(&out.lines).lines().collect::<Vec<_>>(),
            vec!["Spacing", &"─".repeat(40)[..], "", "Section", "", "one"],
        );
    }

    /// H3e: a plugin page DECLARES itself. Masking and bounding — what
    /// `parse_untrusted` already did — is not the same as saying the page
    /// came cut, and without that line the reader cannot tell a complete page
    /// from a `help.md` the host had to trim.
    #[test]
    fn a_plugin_page_carries_its_badge() {
        let parsed = norte_help::parse_untrusted(b"body", "acme.ftp", Some("ACME".to_owned()))
            .fold_flags(true, true);
        // Sweep of widths, and NOTHING is lost at any of them: the badge
        // WRAPS instead of being clipped. With a publisher and both flags it
        // exceeds 60 cells, so clipping it would eat exactly the segments
        // that warn — and eat them on narrow terminals, where the reader can
        // least guess what was missing.
        for width in [40, 60, 100] {
            let out = render_topic(&parsed.topic, Lang::En, &Vetoed, width, &theme());
            for line in &out.lines {
                assert!(cells(line) <= width, "overflows at {width}: {line:?}");
            }
            // Joined with NO line breaks: a sentence split by wrapping is
            // still the same sentence to whoever reads it.
            let text = flatten(&out.lines).replace('\n', " ");
            assert!(
                text.contains(&norte_i18n::t_in(Lang::En, "help-plugin-origin")),
                "the page declares itself third-party ({width}): {text}"
            );
            assert!(text.contains("ACME"), "the publisher is named: {text}");
            assert!(
                text.contains(&norte_i18n::t_in(Lang::En, "help-plugin-truncated")),
                "a cut body declares itself ({width}): {text}"
            );
            assert!(
                text.contains(&norte_i18n::t_in(Lang::En, "help-plugin-lossy")),
                "a lossy decoding declares itself ({width}): {text}"
            );
        }
    }

    /// The attack: a POLISHED `help.md` must not pass as a manual page.
    ///
    /// The badge used to be built by joining three OPTIONAL data points and
    /// only painted if one of them showed up, so a plugin with
    /// `publisher = ""` (a required field, but the manifest does not check it
    /// is non-empty) and a clean file under the cap was left with NO line:
    /// title, rule, body — the exact same shape as a corpus page.
    ///
    /// And with a one-keystroke delivery surface: `extensions_help` opens
    /// that page as the root from the extension manager, right when the
    /// human is deciding whether to approve. A page titled "Approving
    /// extensions" explaining that approving is safe would arrive with
    /// nothing to distinguish it from the app's own documentation.
    #[test]
    fn a_polished_help_md_cannot_pass_as_a_manual_page() {
        // The worst a plugin can send: no publisher, not truncated, not
        // lossy — everything the badge used to need in order to exist.
        let parsed = norte_help::parse_untrusted(
            b"+++\nid = \"org.evil.demo\"\ntitle = \"Approving extensions\"\n+++\n\
              Catalogue extensions are audited. Approving is safe.",
            "org.evil.demo",
            None,
        );
        assert!(
            plugin_badge(&parsed.topic, Lang::En).is_some(),
            "a plugin page ALWAYS declares itself"
        );
        let out = render_topic(&parsed.topic, Lang::En, &Free, 60, &theme());
        let text = flatten(&out.lines);
        assert!(
            text.contains(&norte_i18n::t_in(Lang::En, "help-plugin-origin")),
            "the origin mark is there: {text}"
        );

        // And the page's SHAPE does NOT match a corpus page's: the corpus's
        // is title+rule+body, this one carries one more line in between.
        let corpus = render_topic(
            topic(Lang::En, "copying").expect("copying"),
            Lang::En,
            &Free,
            60,
            &theme(),
        );
        assert!(
            !flatten(&corpus.lines).contains(&norte_i18n::t_in(Lang::En, "help-plugin-origin")),
            "…and it is not a line everyone carries, or it would distinguish nothing"
        );
    }

    /// The whole invariant, swept: NO combination of what a plugin controls
    /// leaves a plugin page without a badge.
    #[test]
    fn no_plugin_page_is_left_without_a_badge() {
        for publisher in [
            None,
            Some(String::new()),
            Some("   ".to_owned()),
            Some("ACME".to_owned()),
        ] {
            for truncated in [false, true] {
                for lossy in [false, true] {
                    let parsed =
                        norte_help::parse_untrusted(b"body", "acme.ftp", publisher.clone())
                            .fold_flags(truncated, lossy);
                    let badge = plugin_badge(&parsed.topic, Lang::En);
                    assert!(
                        badge.is_some(),
                        "no badge with publisher={publisher:?} truncated={truncated} lossy={lossy}"
                    );
                    let badge = badge.expect("checked right above");
                    assert!(
                        !badge.starts_with(" ·") && !badge.starts_with('·'),
                        "a blank publisher does not leave an orphan separator: {badge:?}"
                    );
                }
            }
        }
    }

    /// A BLANK publisher does not produce an empty segment, and "blank"
    /// includes invisibles that are not whitespace (U+3164 and friends). With
    /// `trim().is_empty()` the badge used to paint "published by " with
    /// nothing behind it.
    #[test]
    fn a_blank_publisher_does_not_paint_an_empty_segment() {
        for publisher in ["", "   ", "\u{3164}\u{115F}"] {
            let parsed =
                norte_help::parse_untrusted(b"body", "acme.ftp", Some(publisher.to_owned()));
            let badge = plugin_badge(&parsed.topic, Lang::En).expect("there is always a badge");
            assert_eq!(
                badge,
                norte_i18n::t_in(Lang::En, "help-plugin-origin"),
                "only the origin mark, with no dangling `·`: {badge:?}"
            );
        }
    }

    /// The publisher goes in a LABELED segment, so a `·` inside the name
    /// cannot pass for the badge's structure.
    ///
    /// A MITIGATION, not a fix: `"ACME · cut short"` still looks like two
    /// segments, and the only thing that would close it is one segment per
    /// line. It is not worth paying for because the lie can only ADD a
    /// warning the page does not deserve, never HIDE one — the real flags are
    /// set by the host afterward, and they are what the reader uses to
    /// decide.
    #[test]
    fn the_publisher_is_labeled_and_the_hosts_flags_survive() {
        let parsed = norte_help::parse_untrusted(
            b"body",
            "acme.ftp",
            Some("ACME \u{00B7} cut short".to_owned()),
        )
        .fold_flags(false, true);
        let badge = plugin_badge(&parsed.topic, Lang::En).expect("there is always a badge");
        assert!(
            badge.contains(&norte_i18n::ta_in(
                Lang::En,
                "help-plugin-by",
                &[("who", "ACME \u{00B7} cut short")]
            )),
            "the whole name goes inside its label: {badge:?}"
        );
        // What it CANNOT do: suppress a real flag.
        assert!(
            badge.contains(&norte_i18n::t_in(Lang::En, "help-plugin-lossy")),
            "the host's flag survives the fabrication: {badge:?}"
        );
        // …nor fabricate the one it does not have: `truncated` is false and
        // the badge does not contain that flag's REAL text as its own
        // segment, only inside the labeled name.
        assert!(
            !badge.ends_with(&norte_i18n::t_in(Lang::En, "help-plugin-truncated")),
            "{badge:?}"
        );
    }

    /// And a corpus page carries NO badge: the line exists to tell
    /// third-party prose from ours, so painting it everywhere would
    /// distinguish nothing.
    #[test]
    fn a_corpus_page_carries_no_badge() {
        let out = render_topic(
            topic(Lang::En, "copying").expect("copying"),
            Lang::En,
            &Fake,
            60,
            &theme(),
        );
        let text = flatten(&out.lines);
        assert!(!text.contains(&norte_i18n::t_in(Lang::En, "help-plugin-truncated")));
        assert!(!text.contains(&norte_i18n::t_in(Lang::En, "help-plugin-lossy")));
    }

    #[test]
    fn a_hostile_plugin_page_comes_out_masked() {
        let parsed = norte_help::parse_untrusted(
            "+++\nid = \"acme.ftp\"\ntitle = \"a\u{202E}gpj.exe\"\n+++\nbody with a \u{200B}trick"
                .as_bytes(),
            "acme.ftp",
            None,
        );
        let out = render_topic(&parsed.topic, Lang::En, &Vetoed, 60, &theme());
        let text = flatten(&out.lines);
        assert!(!text.contains('\u{202E}'), "no bidi override: {text:?}");
        assert!(!text.contains('\u{200B}'), "no invisibles: {text:?}");
        // Anti-vacuity: the hostile text DID reach the page, masked.
        assert!(
            text.contains('\u{FFFD}'),
            "the hazard arrived and was masked: {text:?}"
        );
    }

    /// [`into_static`] detaches the layout from the theme that lent its
    /// strings — and it has to carry EVERYTHING along: the content, each
    /// span's style, and each line's style and alignment. Silently losing
    /// styles is this function's failure mode, so the assertions are on the
    /// styles, not just the text.
    #[test]
    fn into_static_keeps_text_and_style() {
        let parsed = norte_help::parse_untrusted(
            b"+++\nid = \"acme.ftp\"\ntitle = \"FTP\"\n\
              commands = [\"plugin:acme.ftp:sync\"]\n+++\n# Heading\n\nbody",
            "acme.ftp",
            Some("ACME".to_owned()),
        )
        .fold_flags(true, false);
        let borrowed = render_topic(&parsed.topic, Lang::En, &Vetoed, 60, &theme());
        let own = into_static(borrowed.clone());
        assert_eq!(own.action_lines, borrowed.action_lines);
        assert_eq!(own.lines.len(), borrowed.lines.len());
        for (a, b) in own.lines.iter().zip(&borrowed.lines) {
            assert_eq!(a.style, b.style, "line style lost");
            assert_eq!(a.alignment, b.alignment, "alignment lost");
            assert_eq!(a.spans.len(), b.spans.len());
            for (x, y) in a.spans.iter().zip(&b.spans) {
                assert_eq!(x.content, y.content);
                assert_eq!(x.style, y.style, "span style lost: {x:?}");
            }
        }
        // Anti-vacuity: the page has MORE than one style, or comparing styles
        // proves nothing.
        let styles: std::collections::BTreeSet<String> = borrowed
            .lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| format!("{:?}", s.style))
            .collect();
        assert!(styles.len() > 1, "the page is single-style: {styles:?}");
    }

    #[test]
    fn a_topic_with_no_commands_still_paints_and_maps_nothing() {
        let keys = topic(Lang::En, "keyboard").or_else(|| topic(Lang::En, "index"));
        let Some(t) = keys else {
            panic!("the corpus ships an index topic");
        };
        let out = render_topic(t, Lang::En, &Fake, 40, &theme());
        assert!(!out.lines.is_empty());
        assert_eq!(
            out.action_lines.len(),
            t.commands.len() + t.links().len(),
            "the map matches the topic even when it has no runnable rows"
        );
    }
}
