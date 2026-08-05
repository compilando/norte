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
//! - A BUILT-IN topic is trusted text, cross-checked by the documentation gate
//!   (`norte-tui/tests/help_gate.rs`): every `{{cmd:…}}` id is compared
//!   byte-exactly against the frontend's command vocabulary, so a spoofed id
//!   fails the build instead of reaching a terminal.
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
//! in. What this module does guarantee is the *geometry*: it never emits a
//! line wider than the width it was given (measured in terminal CELLS, not
//! chars — see [`render_block`]), so no wrapping decision here can push a
//! painted cell off the pane.

use norte_help::{Block, Callout, ChordResolver, CommandText, Span as HSpan, Topic};
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
}

/// Paints a whole topic: title, a rule, the blocks separated by a blank line,
/// the runnable rows and the `see_also` links.
///
/// `width` is the body's width in terminal cells. Everything wraps to it
/// except code blocks (see [`render_block`]).
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
/// let out = render_topic(copying, &Keys, 60, &theme);
///
/// // One action line per command and per link, in `HelpState::actions` order.
/// assert_eq!(
///     out.action_lines.len(),
///     copying.commands.len() + copying.see_also.len()
/// );
/// assert!(out.action_lines.iter().all(|&i| i < out.lines.len()));
/// ```
#[must_use]
pub fn render_topic<'a>(
    topic: &'a Topic,
    r: &(impl ChordResolver + ?Sized),
    width: usize,
    theme: &TuiTheme,
) -> Rendered<'a> {
    let mut lines = vec![
        Line::from(Span::styled(topic.title.clone(), theme.role(Role::Title))),
        Line::from(Span::styled(
            "─".repeat(width),
            theme.role(Role::BorderUnfocused),
        )),
    ];

    for block in &topic.blocks {
        lines.push(Line::default());
        lines.extend(render_block(block, r, width, theme));
    }

    let rows = norte_help::rows_of(topic, r);
    let mut action_lines = Vec::with_capacity(rows.len() + topic.see_also.len());

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
            lines.push(row_line(row, col, width, theme));
        }
    }

    if !topic.see_also.is_empty() {
        lines.push(Line::default());
        for id in &topic.see_also {
            action_lines.push(lines.len());
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    fit(&format!("[[{id}]]"), width.saturating_sub(2)),
                    theme.role(Role::Info),
                ),
            ]));
        }
    }

    Rendered {
        lines,
        action_lines,
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
/// use norte_help::{Availability, Block, ChordResolver, Span};
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
/// for line in render_block(&block, &Bare, 20, &theme) {
///     let cells: usize = line.spans.iter().map(|s| s.content.width()).sum();
///     assert!(cells <= 20, "two cells per char, budgeted as such");
/// }
/// ```
#[must_use]
pub fn render_block<'a>(
    block: &'a Block,
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
        Block::Paragraph(spans) => wrap(&frags(spans, r, theme), width, "", "", Style::default()),
        Block::Bullets(items) => items
            .iter()
            .flat_map(|item| {
                wrap(
                    &frags(item, r, theme),
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
            let (glyph, role) = match kind {
                Callout::Note => ("ℹ", Role::Info),
                Callout::Warn => ("⚠", Role::Warning),
                Callout::Tip => ("💡", Role::Info),
            };
            let head = format!("{glyph} ");
            let cont = " ".repeat(head.width());
            wrap(
                &frags(spans, r, theme),
                width,
                &head,
                &cont,
                theme.role(role),
            )
        }
    }
}

/// One runnable row: the chord in its padded column, then the label.
///
/// Always ONE line — an action that wrapped would break the invariant
/// [`Rendered::action_lines`] rests on — so the label is cut, not wrapped.
fn row_line<'a>(
    row: &norte_help::ResolvedRow,
    col: usize,
    width: usize,
    theme: &TuiTheme,
) -> Line<'a> {
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
    Line::from(vec![
        Span::raw("  "),
        Span::styled(chord.to_owned(), key_style),
        Span::raw(pad),
        Span::styled(fit(&row.label, width.saturating_sub(used)), label_style),
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
            HSpan::TopicLink(id) => (format!("[[{id}]]"), theme.role(Role::Info)),
            HSpan::CommandRef(c) => match norte_help::render_command(c, r) {
                CommandText::Chord(k) => (k, theme.role(Role::Mark)),
                CommandText::Name(n) => (n, theme.role(Role::Info)),
            },
        })
        .collect()
}

/// Wraps styled fragments into lines of at most `width` CELLS, prefixing the
/// first line with `head` and every continuation with `cont`.
///
/// The budget is cells throughout. A word carries the whitespace that follows
/// it, and that whitespace is only painted when another word joins it on the
/// same line — so a wrap never joins two words and a line never ends in
/// trailing blanks. A word wider than the whole line is cut at a cell
/// boundary.
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

    for (text, style) in frags {
        for (word, spaces) in tokens(text) {
            if !word.is_empty() {
                let word_w = word.width();
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

                let mut rest = word;
                while !rest.is_empty() {
                    let avail = inner - cur_w;
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
            if !spaces.is_empty() && cur_w > 0 {
                // Whitespace is normalised to spaces: a tab or a newline
                // inside a span would be painted raw into a cell grid.
                let blanks = " ".repeat(spaces.chars().count());
                pending_w += blanks.width();
                pending.push(Span::styled(blanks, *style));
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
        let out = render_topic(copying, &Fake, 60, &theme());
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
        let lines = render_block(&block, &Fake, 20, &theme());
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
        let lines = render_block(&block, &Fake, 10, &theme());
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
        let out = render_topic(copying, &Fake, 60, &theme());
        assert_eq!(
            out.action_lines.len(),
            copying.commands.len() + copying.see_also.len(),
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
        let link = &out.lines[out.action_lines[copying.commands.len()]];
        assert!(
            flatten(std::slice::from_ref(link)).contains("[[selection]]"),
            "the links follow the commands: {link:?}"
        );
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
        let lines = render_block(&block, &Fake, 40, &theme());
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
        let named = render_block(&unbound, &Fake, 60, &th);
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
        let bound = render_block(&with_key, &Fake, 60, &th);
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
        let out = render_topic(copying, &Fake, 60, &th);
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
            let lines = render_block(&block, &Fake, 40, &th);
            assert!(!lines.is_empty(), "{block:?} painted nothing");
            for line in &lines {
                assert!(cells(line) <= 40, "{block:?} overflowed: {line:?}");
            }
        }
    }

    #[test]
    fn wrapping_never_joins_two_words() {
        let long = Block::Paragraph(vec![
            HSpan::Text("alpha beta".to_owned()),
            HSpan::Strong("gamma".to_owned()),
            HSpan::Text(" delta epsilon".to_owned()),
        ]);
        let lines = render_block(&long, &Fake, 12, &theme());
        let text = flatten(&lines);
        assert!(
            !text.contains("betagamma"),
            "a wrap must not glue two words together: {text}"
        );
        for line in &lines {
            let painted = flatten(std::slice::from_ref(line));
            assert_eq!(
                painted.trim_end(),
                painted,
                "no line ends in the blanks that separated it from the next"
            );
        }
    }

    #[test]
    fn a_topic_with_no_commands_still_paints_and_maps_nothing() {
        let keys = topic(Lang::En, "keyboard").or_else(|| topic(Lang::En, "index"));
        let Some(t) = keys else {
            panic!("the corpus ships an index topic");
        };
        let out = render_topic(t, &Fake, 40, &theme());
        assert!(!out.lines.is_empty());
        assert_eq!(
            out.action_lines.len(),
            t.commands.len() + t.see_also.len(),
            "the map matches the topic even when it has no runnable rows"
        );
    }
}
