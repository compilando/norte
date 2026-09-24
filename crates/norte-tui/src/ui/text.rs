//! Truncating and measuring text for the cell it has to fit.
//!
//! Nothing here knows about `App` or ratatui except `Span`: these are the
//! functions that decide where the ellipsis lands, how many cells a glyph
//! occupies, and what badge goes in front of a hostile name.

use super::HOSTILE_BADGE;
use ratatui::text::Span;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Prefix of `s` that fits in `max` CELLS (review MN2/MN3): width-aware
/// truncation — a double-cell char never overruns the budget (truncating by
/// `chars()` did).
pub(crate) fn take_width(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut used = 0usize;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if used + cw > max {
            break;
        }
        used += cw;
        out.push(c);
    }
    out
}

/// Cell width of `s`, the same budget [`take_width`] spends.
pub(crate) fn cells(s: &str) -> usize {
    s.chars().map(|c| c.width().unwrap_or(0)).sum()
}

/// Right-truncation to `max` CELLS, marking the cut with a single `…`.
///
/// The shape for a LABEL, where `middle_ellipsis` is the wrong tool: head
/// plus tail collides any two labels that agree on both ends (`Foo…bar` and
/// `Foo…bar` for two different plugin-supplied titles), while a right cut
/// keeps a distinct prefix distinct. Cell-aware, never char counts: a
/// double-width glyph that does not fit is dropped whole rather than
/// overflowing the column by one cell.
pub(crate) fn right_ellipsis(s: &str, max: usize) -> String {
    if cells(s) <= max {
        return s.to_owned();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = take_width(s, max - 1);
    out.push('…');
    out
}

/// Splits a generated dialog hint into its whole `[chord] label` groups.
///
/// [`crate::hints::dialog_hints`] joins the groups with a single space and
/// every group starts with `[`, so the boundary is the ` [` join and NOT any
/// space: a label is prose and carries spaces of its own (`the other pane`).
pub(crate) fn hint_groups(hint: &str) -> Vec<&str> {
    let mut groups = Vec::new();
    let mut start = 0usize;
    for (i, _) in hint.match_indices(" [") {
        groups.push(&hint[start..i]);
        start = i + 1;
    }
    if start < hint.len() {
        groups.push(&hint[start..]);
    }
    groups
}

/// Fits a generated dialog hint into `max` CELLS by dropping whole
/// `[chord] label` groups, marking the loss with a trailing `…`.
///
/// Every other overlay measures its hint and grows its popup to fit
/// (`draw_nav_popup`, `draw_extensions`, …). The help overlay is full-screen
/// and cannot grow, so its footer has to be CUT — and a `middle_ellipsis`
/// there was actively lying twice over. At 80 columns it produced
/// `[enter] confirmar [esc]…kspace] atrás [/] filtrar`: the cut fell inside a
/// group and left the brackets balanced, so `[esc]…kspace]` reads as a chord
/// for a key called *kspace* that the app invented; and middle truncation
/// eats the MIDDLE of the list, which is exactly where `[tab] otro panel`
/// sat — the verb the whole two-pane design rests on, gone without a trace.
///
/// A group is therefore emitted WHOLE or not at all, and the `…` says that
/// something was dropped. Groups are kept in order, stopping at the first
/// that does not fit: the footer is then a true prefix of the real hint.
#[must_use]
pub fn fit_hint_groups(hint: &str, max: usize) -> String {
    if cells(hint) <= max {
        return hint.to_owned();
    }
    // Two cells held back: the `…` and the space that separates it from the
    // last group kept.
    let budget = max.saturating_sub(2);
    let mut out = String::new();
    for g in hint_groups(hint) {
        let sep = usize::from(!out.is_empty());
        if cells(&out) + sep + cells(g) > budget {
            break;
        }
        if sep == 1 {
            out.push(' ');
        }
        out.push_str(g);
    }
    if out.is_empty() {
        // Not even one group fits: say so rather than paint half a chord.
        return take_width("…", max);
    }
    out.push(' ');
    out.push('…');
    out
}

/// The text with its hostile-name badge in front, if it carries one.
pub(crate) fn with_badge(text: &str, hostile: bool) -> String {
    if hostile {
        format!("{HOSTILE_BADGE} {text}")
    } else {
        text.to_owned()
    }
}

/// A two-field row in `width` cells: `left` on the left, `right` flush to
/// the right.
///
/// The right field is NEVER truncated, and that is the rule that matters: it
/// is a SIZE, and a `38.2 GiB` truncated at the head paints `8.2 GiB`, which
/// is not a broken label but a FALSE number. If it does not fit whole, the
/// right field is dropped and only the name is left.
pub(crate) fn two_fields(
    left: &str,
    right: &str,
    width: usize,
    truncate: fn(&str, usize) -> String,
) -> String {
    /// Cells below which the name stops identifying anything.
    const FLOOR: usize = 6;
    let d = norte_frontend::cells(right);
    // Room on both sides of the pair, PLUS one separator cell between the
    // two fields: without it a name that fills its spot leaves the `…`
    // glued to the number (`/home/os…1P`), which reads as data and not as a
    // truncation.
    if d + 3 + FLOOR >= width {
        return truncate(&format!(" {left}"), width);
    }
    let room = width - d - 3;
    let i = truncate(&format!(" {left}"), room);
    let slot = room.saturating_sub(norte_frontend::cells(&i)) + 1;
    format!("{i}{}{right} ", " ".repeat(slot))
}

/// Truncation through the MIDDLE, for what is identified by its tail: a
/// path.
pub(crate) fn middle(text: &str, width: usize) -> String {
    norte_frontend::middle_ellipsis(text, width)
}

/// Truncates by the TAIL to `width` cells, marking with `…`.
///
/// By the tail and not the middle ([`norte_frontend::middle_ellipsis`])
/// because here what identifies the row is at the start: a favorite's name,
/// a section's. Middle ellipsis exists for paths, where what identifies is
/// the end.
pub(crate) fn head(text: &str, width: usize) -> String {
    if norte_frontend::cells(text) <= width {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for c in text.chars() {
        let w = UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        used += w;
        out.push(c);
    }
    out.push('…');
    out
}

/// Modal width by CONTENT (H1 T3 follow-up): GENERATED footers can exceed
/// the historic 60 cols — e.g. collision: `[esc] … [w] newer` — and
/// truncating them would hide real keys. Ceiling = frame width minus margin;
/// floor = the historic 60. MINOR-1 (H1 close): measured in
/// Truncates a row of spans to `max` CELLS, cutting on the right and
/// respecting character boundaries.
///
/// The last span that does not fit whole is cut by characters (never by
/// bytes): splitting a wide character in half paints half a garbage cell,
/// and splitting by bytes is not even UTF-8.
pub(crate) fn clamp_spans(spans: Vec<Span<'static>>, max: usize) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut left = max;
    for sp in spans {
        if left == 0 {
            break;
        }
        let w = sp.content.width();
        if w <= left {
            left -= w;
            out.push(sp);
            continue;
        }
        let mut text = String::new();
        let mut acc = 0_usize;
        for c in sp.content.chars() {
            let cw = UnicodeWidthChar::width(c).unwrap_or(0);
            if acc + cw > left {
                break;
            }
            acc += cw;
            text.push(c);
        }
        // Cutting by character is not enough: a ZWJ or a variation selector
        // measures ZERO, so it always fits and the chunk can end on a
        // joiner that composes with whatever is painted right after it —
        // `emoji_zwj_family` truncated to three cells left the family
        // joined to the next character (#246 m1). `middle_ellipsis` fixed
        // the mirror of this by draining from the front; here it drains
        // from the back.
        while text
            .chars()
            .next_back()
            .is_some_and(|c| UnicodeWidthChar::width(c).unwrap_or(0) == 0 && !c.is_ascii())
        {
            text.pop();
        }
        if !text.is_empty() {
            out.push(Span::styled(text, sp.style));
        }
        break;
    }
    out
}

/// The TAIL of `s`, with `…` in front when something was left out.
///
/// In terminal CELLS, not chars. It used to count chars, and for what this
/// budget protects — that the field fits in its box — that is the wrong
/// measure: fifty CJK chars are ONE HUNDRED cells, so a Japanese name
/// overflowed just the same and dragged the end's cursor along with it.
/// Uncovered by the transfer modal's screenshot, which did not exist until
/// today.
///
/// The cut is delegated to [`norte_frontend::skip_cells`], which does not
/// split a wide character in half and leaves its slot blank: without that,
/// the tail could start with half a cell and shift the whole line.
pub(crate) fn tail_window(s: &str, max: usize) -> String {
    let total = norte_frontend::cells(s);
    if total <= max {
        return s.to_owned();
    }
    // One cell goes to the `…`.
    let fits = max.saturating_sub(1);
    let tail = norte_frontend::skip_cells(s, total.saturating_sub(fits));
    format!("…{tail}")
}

/// Prefixes the hostile badge OUTSIDE the translation (audit MINOR-5: the
/// badge mechanism cannot depend on every locale keeping a `{ $badge }` —
/// Rust-side concatenation, translation-proof).
pub(crate) fn badge_prefixed(hostile: bool, line: String) -> String {
    if hostile {
        format!("{HOSTILE_BADGE}{line}")
    } else {
        line
    }
}

pub(crate) fn clamp_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// How many rows `text` occupies wrapped to `width` columns.
///
/// Counts CELLS, not bytes or `char`s: measuring in bytes would reserve too
/// much and in `char`s too little — and too little is what truncates the
/// sentence that says this cannot be undone.
pub(crate) fn wrapped_rows(text: &str, width: u16) -> u16 {
    if width == 0 {
        return 1;
    }
    let cells = u16::try_from(text.width()).unwrap_or(u16::MAX);
    let exact = cells.div_ceil(width).max(1);
    // One row of slack as soon as the sentence wraps: `Wrap` splits by
    // WORDS, so `ceil(cells / width)` is a LOWER bound and stopping there
    // truncates the last line — which is the one that says this cannot be
    // undone. `sync_layout`'s cap bounds what the slack can cost.
    if cells > width {
        exact.saturating_add(1)
    } else {
        exact
    }
}

#[cfg(test)]
mod ellipsis_tests {
    use norte_frontend::middle_ellipsis;
    use unicode_width::UnicodeWidthStr;

    /// A string that already fits in `max` cells comes back untouched.
    #[test]
    fn fits_intact() {
        assert_eq!(middle_ellipsis("file:///d/a.txt", 46), "file:///d/a.txt");
    }

    /// ASCII that overflows: identical behavior to the above (cells==chars),
    /// head + `…` + tail, without exceeding `max`.
    #[test]
    fn ascii_keeps_head_and_tail() {
        let s = "file:///muy/larga/ruta/hacia/un/archivo/final.txt";
        let out = middle_ellipsis(s, 20);
        assert!(out.contains('…'));
        assert!(out.starts_with("file:"), "keeps the scheme (head)");
        let tail = out.rsplit_once('…').expect("there is an ellipsis").1;
        assert!(
            !tail.is_empty() && s.ends_with(tail),
            "the tail is a REAL suffix of the original: {out:?}"
        );
        assert!(out.width() <= 20, "does not exceed the width: {out:?}");
    }

    /// CJK (each char = 2 cells): NEVER exceeds `max` cells and KEEPS the
    /// tail — bug #79 lost it because it budgeted by chars.
    #[test]
    fn cjk_stays_bounded_and_keeps_tail() {
        let s = "日本語".repeat(20); // 60 chars, 120 cells
        let out = middle_ellipsis(&s, 21);
        assert!(out.width() <= 21, "width {} > 21 in {out:?}", out.width());
        assert!(out.contains('…'));
        assert!(out.ends_with('語'), "the tail survives: {out:?}");
        assert!(out.starts_with('日'), "the head survives: {out:?}");
    }

    /// Wide emoji (2 cells): does not overflow either.
    #[test]
    fn emoji_does_not_overflow() {
        let s = "a😀b😀c😀d😀e😀f😀g";
        let out = middle_ellipsis(s, 9);
        assert!(out.width() <= 9, "width {} in {out:?}", out.width());
        assert!(out.contains('…'));
    }

    /// `max` smaller than a single wide char: the cell is not split → just
    /// `…`.
    #[test]
    fn max_smaller_than_a_wide_char() {
        let out = middle_ellipsis("日本", 1);
        assert_eq!(out, "…");
        assert!(out.width() <= 1);
    }

    /// P1 encoding audit F2 (LOW): a flood of combining marks (ZERO width
    /// each) overflows the cell-based walker WITHOUT ever touching its
    /// budget — the width early-return, or the walker itself, could
    /// return/process the ENTIRE string unbounded, with `max` cells
    /// satisfied but the real size uncapped. The corpus's `nfd_e_acute`
    /// (`e` + combining acute) is the canonical base+combining pair — here
    /// it is flooded 100,000x to exercise the backstop by char COUNT, not
    /// just by width.
    #[test]
    fn flood_of_combining_marks_does_not_overflow() {
        let fixture = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == "nfd_e_acute")
            .expect("corpus fixture");
        let text = String::from_utf8(fixture.bytes).expect("nfd_e_acute is valid UTF-8");
        let (base, combining) = text.split_at(1); // "e" + "\u{0301}"
        let flood: String = std::iter::once(base)
            .chain(std::iter::repeat_n(combining, 100_000))
            .collect();
        assert_eq!(flood.width(), 1, "control: the whole flood weighs 1 cell");
        let out = middle_ellipsis(&flood, 10);
        // Bound: the backstop pre-truncates to `char_cap = 4*max` chars, but
        // both the head AND tail walkers each operate over the WHOLE
        // pre-cut (not over separate halves) — with zero width neither one
        // stops on budget, so each can consume it whole. Bounded
        // (2*char_cap + 1), not exact — what F2 (LOW) asks for is to stop
        // being UNBOUNDED, not a tight bound.
        let bound = 2 * (10 * 4) + 1;
        assert!(
            out.chars().count() <= bound,
            "the char-count backstop did not bound the output: {} chars (bound {bound})",
            out.chars().count()
        );
    }
}

#[cfg(test)]
mod clamp_spans_tests {
    use super::clamp_spans;
    use ratatui::text::Span;
    use unicode_width::UnicodeWidthStr as _;

    fn truncate(text: &str, max: usize) -> String {
        clamp_spans(vec![Span::raw(text.to_owned())], max)
            .into_iter()
            .map(|s| s.content.into_owned())
            .collect()
    }

    /// What fits whole passes whole, and what does not is cut by CELLS.
    #[test]
    fn truncates_by_cells_not_bytes() {
        assert_eq!(truncate("abcdef", 10), "abcdef");
        assert_eq!(truncate("abcdef", 3), "abc");
        // CJK: two cells per character, so one fits in three cells.
        assert_eq!(truncate("日本語", 3), "日");
        assert!(truncate("日本語", 3).width() <= 3);
    }

    /// A joiner measures ZERO, so it always fit and the chunk used to end
    /// on it: whatever was painted after would compose with the truncated
    /// family (#246 m1). `middle_ellipsis` drains from the front; this
    /// drains from the back.
    #[test]
    fn does_not_end_on_a_joiner() {
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        for max in 0..=8 {
            let output = truncate(family, max);
            assert!(
                !output.ends_with('\u{200D}'),
                "at {max} cells a trailing ZWJ was left: {output:?}"
            );
        }
    }

    /// A `max` of zero paints nothing, and never panics.
    #[test]
    fn zero_cells_paints_nothing() {
        assert_eq!(truncate("hello", 0), "");
        assert_eq!(truncate("", 5), "");
    }

    /// Spans that fit are kept as SPANS, with their style: truncation
    /// cannot merge into one what the hostile badge keeps separate.
    #[test]
    fn keeps_the_spans_that_fit() {
        let spans = vec![
            Span::raw("ab".to_owned()),
            Span::raw("cd".to_owned()),
            Span::raw("ef".to_owned()),
        ];
        let output = clamp_spans(spans, 5);
        assert_eq!(output.len(), 3);
        assert_eq!(output[2].content.as_ref(), "e");
    }
}
